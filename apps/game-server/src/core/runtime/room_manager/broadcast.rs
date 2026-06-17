use super::*;

use crate::pb::RoomStatePush;
use crate::protocol::{MessageType, encode_body};

impl RoomManager {
    pub async fn broadcast_snapshot(
        &self,
        room_id: &str,
        event: &str,
        snapshot: RoomSnapshot,
    ) -> Result<(), std::io::Error> {
        let body = encode_body(&RoomStatePush {
            event: event.to_string(),
            snapshot: Some(snapshot),
        });
        self.broadcast_to_room(room_id, MessageType::RoomStatePush, body)
            .await
    }

    pub(super) async fn broadcast_to_room(
        &self,
        room_id: &str,
        message_type: MessageType,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        let senders = {
            let Some(room_entry) = self.get_room_entry(room_id).await else {
                info!(room_id = room_id, "broadcast_to_room: room not found");
                return Ok(());
            };
            let room = room_entry.lock().await;
            if room.marked_for_destruction {
                info!(
                    room_id = room_id,
                    "broadcast_to_room: room is being destroyed"
                );
                return Ok(());
            }

            let online = room.broadcast_members();
            info!(
                room_id = room_id,
                message_type = ?message_type,
                online_count = online.len(),
                "broadcast_to_room"
            );

            online
                .iter()
                .map(|member| {
                    (
                        member.player_id.clone(),
                        member.sender.clone(),
                        member.close_state.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };

        for (player_id, sender, close_state) in senders {
            if let Err(error) = try_send_outbound(
                &sender,
                &close_state,
                OutboundMessage {
                    message_type,
                    seq: 0,
                    body: body.clone(),
                },
                OutboundQueueLogContext {
                    player_id: Some(&player_id),
                    room_id: Some(room_id),
                    operation: "room_broadcast",
                    ..OutboundQueueLogContext::default()
                },
            ) {
                warn!(
                    room_id = room_id,
                    player_id = %player_id,
                    message_type = ?message_type,
                    error = %error,
                    "failed to queue room broadcast"
                );
            }
        }

        Ok(())
    }

    pub(super) async fn broadcast_to_players(
        &self,
        room_id: &str,
        target_player_ids: &[String],
        message_type: MessageType,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        let senders = {
            let Some(room_entry) = self.get_room_entry(room_id).await else {
                info!(room_id = room_id, "broadcast_to_players: room not found");
                return Ok(());
            };
            let room = room_entry.lock().await;
            if room.marked_for_destruction {
                info!(
                    room_id = room_id,
                    "broadcast_to_players: room is being destroyed"
                );
                return Ok(());
            }

            let targets = target_player_ids
                .iter()
                .filter_map(|player_id| room.members.get(player_id))
                .filter(|member| !member.offline && !member.syncing)
                .map(|member| {
                    (
                        member.player_id.clone(),
                        member.sender.clone(),
                        member.close_state.clone(),
                    )
                })
                .collect::<Vec<_>>();

            info!(
                room_id = room_id,
                message_type = ?message_type,
                target_count = targets.len(),
                "broadcast_to_players"
            );

            targets
        };

        for (player_id, sender, close_state) in senders {
            if let Err(error) = try_send_outbound(
                &sender,
                &close_state,
                OutboundMessage {
                    message_type,
                    seq: 0,
                    body: body.clone(),
                },
                OutboundQueueLogContext {
                    player_id: Some(&player_id),
                    room_id: Some(room_id),
                    operation: "targeted_room_broadcast",
                    ..OutboundQueueLogContext::default()
                },
            ) {
                warn!(
                    room_id = room_id,
                    player_id = %player_id,
                    message_type = ?message_type,
                    error = %error,
                    "failed to queue targeted room broadcast"
                );
            }
        }

        Ok(())
    }

    pub(super) async fn broadcast_message(
        &self,
        room_id: &str,
        target_player_ids: &[String],
        message_type: MessageType,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        if target_player_ids.is_empty() {
            self.broadcast_to_room(room_id, message_type, body).await
        } else {
            self.broadcast_to_players(room_id, target_player_ids, message_type, body)
                .await
        }
    }

    pub(super) async fn broadcast_logic_broadcasts(
        &self,
        room_id: &str,
        broadcasts: Vec<RoomLogicBroadcast>,
    ) {
        for RoomLogicBroadcast {
            message_type,
            body,
            target_player_ids,
        } in broadcasts
        {
            let _ = self
                .broadcast_message(room_id, &target_player_ids, message_type, body)
                .await;
        }
    }

    pub async fn send_to_player(
        &self,
        player_id: &str,
        message_type: MessageType,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        let Some(room_id) = self.player_rooms.read().await.get(player_id).cloned() else {
            return Ok(());
        };
        let Some(room_entry) = self.get_room_entry(&room_id).await else {
            self.remove_player_indexes_for_room(player_id, &room_id)
                .await;
            return Ok(());
        };

        let (outbound, stale_index) = {
            let room = room_entry.lock().await;
            if room.marked_for_destruction
                || room.transfer_state.status == RoomTransferStatus::Retired
            {
                (None, true)
            } else {
                match room.members.get(player_id) {
                    Some(member) if !member.offline => (
                        Some((member.sender.clone(), member.close_state.clone())),
                        false,
                    ),
                    Some(_) => (None, false),
                    None => (None, true),
                }
            }
        };

        if let Some((sender, close_state)) = outbound {
            if let Err(error) = try_send_outbound(
                &sender,
                &close_state,
                OutboundMessage {
                    message_type,
                    seq: 0,
                    body,
                },
                OutboundQueueLogContext {
                    player_id: Some(player_id),
                    operation: "send_to_player",
                    ..OutboundQueueLogContext::default()
                },
            ) {
                warn!(
                    player_id = player_id,
                    message_type = ?message_type,
                    error = %error,
                    "failed to queue player message"
                );
            }
        } else if stale_index {
            self.remove_player_indexes_for_room(player_id, &room_id)
                .await;
        }

        Ok(())
    }
}
