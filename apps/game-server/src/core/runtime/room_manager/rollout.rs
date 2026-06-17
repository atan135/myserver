use super::*;

use serde_json::json;

use crate::pb::{GameMessagePush, ServerRedirectPush};
use crate::protocol::{MessageType, encode_body};

impl RoomManager {
    pub async fn room_count(&self) -> usize {
        self.rooms.read().await.len()
    }

    pub async fn rollout_drain_snapshot(
        &self,
        owner_server_id: &str,
        route_limit: usize,
    ) -> RolloutDrainSnapshot {
        let room_entries = self.room_entries_snapshot().await;

        let mut owned_room_count = 0_u64;
        let mut migrating_room_count = 0_u64;
        let mut routes = Vec::with_capacity(route_limit.min(room_entries.len()));
        let mut transferable_empty_room_count = 0_u64;
        let mut transferable_empty_room_samples =
            Vec::with_capacity(route_limit.min(room_entries.len()));
        let mut retired_room_count = 0_u64;
        let mut rollout_epoch: Option<String> = None;
        let mut mixed_rollout_epoch = false;

        for (_room_id, room_entry) in room_entries {
            let room = room_entry.lock().await;
            if room.marked_for_destruction {
                continue;
            }

            match room.transfer_state.status {
                RoomTransferStatus::Owned => {
                    owned_room_count = owned_room_count.saturating_add(1);
                    if !room.has_online_members() {
                        transferable_empty_room_count =
                            transferable_empty_room_count.saturating_add(1);
                        if transferable_empty_room_samples.len() < route_limit {
                            transferable_empty_room_samples
                                .push(room_rollout_route_status(&room, owner_server_id));
                        }
                    }
                }
                RoomTransferStatus::Frozen
                | RoomTransferStatus::Exported
                | RoomTransferStatus::Importing => {
                    migrating_room_count = migrating_room_count.saturating_add(1);
                }
                RoomTransferStatus::Retired => {
                    retired_room_count = retired_room_count.saturating_add(1);
                }
                RoomTransferStatus::OwnedByNew => {}
            }

            if let Some(epoch) = room.transfer_state.rollout_epoch.as_deref() {
                if !epoch.is_empty() {
                    match rollout_epoch {
                        None => rollout_epoch = Some(epoch.to_string()),
                        Some(ref existing) if existing == epoch => {}
                        Some(_) => mixed_rollout_epoch = true,
                    }
                }
            }

            if routes.len() < route_limit {
                routes.push(room_rollout_route_status(&room, owner_server_id));
            }
        }

        RolloutDrainSnapshot {
            rollout_epoch: if mixed_rollout_epoch {
                String::new()
            } else {
                rollout_epoch.unwrap_or_default()
            },
            owner_server_id: owner_server_id.to_string(),
            owned_room_count,
            migrating_room_count,
            routes,
            transferable_empty_room_count,
            transferable_empty_room_samples,
            retired_room_count,
        }
    }

    pub async fn trigger_server_redirect(
        &self,
        room_id: &str,
        push: ServerRedirectPush,
    ) -> Result<ServerRedirectDelivery, &'static str> {
        if room_id.trim().is_empty() {
            return Err("INVALID_ROOM_ID");
        }
        if push.rollout_epoch.trim().is_empty() {
            return Err("INVALID_ROLLOUT_EPOCH");
        }
        if push.target_host.trim().is_empty() || push.target_port == 0 {
            return Err("INVALID_REDIRECT_TARGET");
        }

        let body = encode_body(&push);
        let targets = {
            let room_entry = self.get_room_entry(room_id).await.ok_or("ROOM_NOT_FOUND")?;
            let room = room_entry.lock().await;
            if room.marked_for_destruction {
                return Err("ROOM_NOT_FOUND");
            }
            room.members
                .values()
                .filter(|member| !member.offline && !member.syncing)
                .map(|member| {
                    (
                        member.player_id.clone(),
                        member.sender.clone(),
                        member.close_state.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };

        let mut delivered_count = 0u64;
        let mut failed_count = 0u64;
        for (player_id, sender, close_state) in &targets {
            match try_send_outbound(
                sender,
                close_state,
                OutboundMessage {
                    message_type: MessageType::ServerRedirectPush,
                    seq: 0,
                    body: body.clone(),
                },
                OutboundQueueLogContext {
                    player_id: Some(player_id),
                    room_id: Some(room_id),
                    operation: "server_redirect_push",
                    ..OutboundQueueLogContext::default()
                },
            ) {
                Ok(()) => {
                    delivered_count = delivered_count.saturating_add(1);
                    let close_requested = close_state.request_close(SERVER_REDIRECT_CLOSE_REASON);
                    info!(
                        room_id = room_id,
                        player_id = %player_id,
                        rollout_epoch = %push.rollout_epoch,
                        target_host = %push.target_host,
                        target_port = push.target_port,
                        target_server_id = %push.target_server_id,
                        close_reason = SERVER_REDIRECT_CLOSE_REASON,
                        close_requested = close_requested,
                        "server redirect push queued and connection close requested"
                    );
                }
                Err(error) => {
                    failed_count = failed_count.saturating_add(1);
                    warn!(
                        room_id = room_id,
                        player_id = %player_id,
                        rollout_epoch = %push.rollout_epoch,
                        target_host = %push.target_host,
                        target_port = push.target_port,
                        target_server_id = %push.target_server_id,
                        error = %error,
                        "failed to queue server redirect push"
                    );
                }
            }
        }

        let online_member_count = targets.len() as u64;
        info!(
            room_id = room_id,
            rollout_epoch = %push.rollout_epoch,
            target_host = %push.target_host,
            target_port = push.target_port,
            target_server_id = %push.target_server_id,
            delivered_count = delivered_count,
            failed_count = failed_count,
            online_member_count = online_member_count,
            "server redirect trigger completed"
        );

        Ok(ServerRedirectDelivery {
            delivered_count,
            failed_count,
            online_member_count,
        })
    }

    pub async fn trigger_rollout_drain_notice(
        &self,
        notice: RolloutDrainNotice,
    ) -> Result<RolloutDrainNoticeDelivery, &'static str> {
        let room_id = notice.room_id.trim();
        if room_id.is_empty() {
            return Err("INVALID_ROOM_ID");
        }
        if notice.rollout_epoch.trim().is_empty() {
            return Err("INVALID_ROLLOUT_EPOCH");
        }
        if notice.message.trim().is_empty() {
            return Err("INVALID_DRAIN_NOTICE_MESSAGE");
        }

        let payload_json = json!({
            "room_id": room_id,
            "rollout_epoch": &notice.rollout_epoch,
            "reason": &notice.reason,
            "message": &notice.message,
            "retry_after_ms": notice.retry_after_ms,
            "deadline_ms": notice.deadline_ms,
        })
        .to_string();
        let push = GameMessagePush {
            event: "rollout_drain_notice".to_string(),
            room_id: room_id.to_string(),
            player_id: String::new(),
            action: "leave_room".to_string(),
            payload_json,
        };
        let body = encode_body(&push);
        let targets = {
            let room_entry = self.get_room_entry(room_id).await.ok_or("ROOM_NOT_FOUND")?;
            let room = room_entry.lock().await;
            if room.marked_for_destruction {
                return Err("ROOM_NOT_FOUND");
            }
            room.members
                .values()
                .filter(|member| !member.offline && !member.syncing)
                .map(|member| {
                    (
                        member.player_id.clone(),
                        member.sender.clone(),
                        member.close_state.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };

        let mut delivered_count = 0u64;
        let mut failed_count = 0u64;
        for (player_id, sender, close_state) in &targets {
            match try_send_outbound(
                sender,
                close_state,
                OutboundMessage {
                    message_type: MessageType::GameMessagePush,
                    seq: 0,
                    body: body.clone(),
                },
                OutboundQueueLogContext {
                    player_id: Some(player_id),
                    room_id: Some(room_id),
                    operation: "rollout_drain_notice",
                    ..OutboundQueueLogContext::default()
                },
            ) {
                Ok(()) => {
                    delivered_count = delivered_count.saturating_add(1);
                }
                Err(error) => {
                    failed_count = failed_count.saturating_add(1);
                    warn!(
                        room_id = room_id,
                        player_id = %player_id,
                        rollout_epoch = %notice.rollout_epoch,
                        error = %error,
                        "failed to queue rollout drain notice"
                    );
                }
            }
        }

        let online_member_count = targets.len() as u64;
        info!(
            room_id = room_id,
            rollout_epoch = %notice.rollout_epoch,
            reason = %notice.reason,
            retry_after_ms = notice.retry_after_ms,
            deadline_ms = notice.deadline_ms,
            delivered_count = delivered_count,
            failed_count = failed_count,
            online_member_count = online_member_count,
            "rollout drain notice trigger completed"
        );

        Ok(RolloutDrainNoticeDelivery {
            delivered_count,
            failed_count,
            online_member_count,
        })
    }
}
