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
                        member.character_id.clone(),
                        member.sender.clone(),
                        member.close_state.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };

        for (character_id, sender, close_state) in senders {
            if let Err(error) = try_send_outbound(
                &sender,
                &close_state,
                OutboundMessage {
                    message_type,
                    seq: 0,
                    body: body.clone(),
                },
                OutboundQueueLogContext {
                    subject_id: Some(&character_id),
                    room_id: Some(room_id),
                    operation: "room_broadcast",
                    ..OutboundQueueLogContext::default()
                },
            ) {
                warn!(
                    room_id = room_id,
                    character_id = %character_id,
                    message_type = ?message_type,
                    error = %error,
                    "failed to queue room broadcast"
                );
            }
        }

        Ok(())
    }

    pub(super) async fn broadcast_to_characters(
        &self,
        room_id: &str,
        target_character_ids: &[String],
        message_type: MessageType,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        let senders = {
            let Some(room_entry) = self.get_room_entry(room_id).await else {
                info!(room_id = room_id, "broadcast_to_characters: room not found");
                return Ok(());
            };
            let room = room_entry.lock().await;
            if room.marked_for_destruction {
                info!(
                    room_id = room_id,
                    "broadcast_to_characters: room is being destroyed"
                );
                return Ok(());
            }

            let targets = target_character_ids
                .iter()
                .filter_map(|character_id| room.members.get(character_id))
                .filter(|member| !member.offline && !member.syncing)
                .map(|member| {
                    (
                        member.character_id.clone(),
                        member.sender.clone(),
                        member.close_state.clone(),
                    )
                })
                .collect::<Vec<_>>();

            info!(
                room_id = room_id,
                message_type = ?message_type,
                target_count = targets.len(),
                "broadcast_to_characters"
            );

            targets
        };

        for (character_id, sender, close_state) in senders {
            if let Err(error) = try_send_outbound(
                &sender,
                &close_state,
                OutboundMessage {
                    message_type,
                    seq: 0,
                    body: body.clone(),
                },
                OutboundQueueLogContext {
                    subject_id: Some(&character_id),
                    room_id: Some(room_id),
                    operation: "targeted_room_broadcast",
                    ..OutboundQueueLogContext::default()
                },
            ) {
                warn!(
                    room_id = room_id,
                    character_id = %character_id,
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
        target_character_ids: &[String],
        message_type: MessageType,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        if target_character_ids.is_empty() {
            self.broadcast_to_room(room_id, message_type, body).await
        } else {
            self.broadcast_to_characters(room_id, target_character_ids, message_type, body)
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
            target_character_ids,
        } in broadcasts
        {
            let _ = self
                .broadcast_message(room_id, &target_character_ids, message_type, body)
                .await;
        }
    }

    pub async fn send_to_character(
        &self,
        character_id: &str,
        message_type: MessageType,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        let Some(room_id) = self.character_rooms.read().await.get(character_id).cloned() else {
            return Ok(());
        };
        let Some(room_entry) = self.get_room_entry(&room_id).await else {
            self.remove_character_indexes_for_room(character_id, &room_id)
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
                match room.members.get(character_id) {
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
                    subject_id: Some(character_id),
                    operation: "send_to_character",
                    ..OutboundQueueLogContext::default()
                },
            ) {
                warn!(
                    character_id = character_id,
                    message_type = ?message_type,
                    error = %error,
                    "failed to queue character message"
                );
                return Err(std::io::Error::other(error.to_string()));
            }
        } else if stale_index {
            self.remove_character_indexes_for_room(character_id, &room_id)
                .await;
        }

        Ok(())
    }
}
