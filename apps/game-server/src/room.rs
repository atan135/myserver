use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::pb::{RoomMember, RoomSnapshot};

#[derive(Clone)]
pub struct OutboundMessage {
    pub message_type: crate::protocol::MessageType,
    pub seq: u32,
    pub body: Vec<u8>,
}

#[derive(Clone)]
pub struct RoomMemberState {
    pub player_id: String,
    pub ready: bool,
    pub sender: mpsc::UnboundedSender<OutboundMessage>,
}

#[derive(Clone)]
pub struct Room {
    pub room_id: String,
    pub owner_player_id: String,
    pub members: HashMap<String, RoomMemberState>,
}

impl Room {
    pub fn snapshot(&self) -> RoomSnapshot {
        let members = self
            .members
            .values()
            .map(|member| RoomMember {
                player_id: member.player_id.clone(),
                ready: member.ready,
                is_owner: member.player_id == self.owner_player_id,
            })
            .collect();

        RoomSnapshot {
            room_id: self.room_id.clone(),
            owner_player_id: self.owner_player_id.clone(),
            state: self.state_name(),
            members,
        }
    }

    fn state_name(&self) -> String {
        if self.members.is_empty() {
            return "empty".to_string();
        }

        if self.members.values().all(|member| member.ready) {
            return "ready".to_string();
        }

        "waiting".to_string()
    }
}
