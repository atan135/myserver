use std::collections::HashMap;
use std::time::Instant;

use tokio::sync::mpsc;

use crate::core::logic::RoomLogic;
use crate::pb::{RoomMember, RoomSnapshot};

#[derive(Clone)]
pub struct OutboundMessage {
    pub message_type: crate::protocol::MessageType,
    pub seq: u32,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomPhase {
    Waiting,
    InGame,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberRole {
    Player,
    Observer,
}

#[derive(Clone)]
pub struct RoomMemberState {
    pub player_id: String,
    pub ready: bool,
    pub sender: mpsc::UnboundedSender<OutboundMessage>,
    pub offline: bool,
    pub offline_since: Option<Instant>,
    pub role: MemberRole,
}

#[derive(Clone)]
pub struct PlayerInputRecord {
    pub frame_id: u32,
    pub player_id: String,
    pub action: String,
    pub payload_json: String,
    pub received_at: Instant,
}

pub struct Room {
    pub room_id: String,
    pub match_id: Option<String>,
    pub owner_player_id: String,
    pub phase: RoomPhase,
    pub policy_id: String,
    pub current_frame: u32,
    pub pending_inputs: Vec<PlayerInputRecord>,
    pub members: HashMap<String, RoomMemberState>,
    pub logic: Box<dyn RoomLogic>,
    pub created_at: Instant,
    pub last_active_at: Instant,
    pub empty_since: Option<Instant>,
    pub marked_for_destruction: bool,
    pub input_history: Vec<PlayerInputRecord>,
    pub last_snapshot_frame: u32,
}

impl Room {
    pub fn new(
        room_id: String,
        owner_player_id: String,
        policy_id: String,
        logic: Box<dyn RoomLogic>,
    ) -> Self {
        let now = Instant::now();
        Self {
            room_id,
            match_id: None,
            owner_player_id,
            phase: RoomPhase::Waiting,
            policy_id,
            current_frame: 0,
            pending_inputs: Vec::new(),
            members: HashMap::new(),
            logic,
            created_at: now,
            last_active_at: now,
            empty_since: None,
            marked_for_destruction: false,
            input_history: Vec::new(),
            last_snapshot_frame: 0,
        }
    }

    pub fn snapshot(&self) -> RoomSnapshot {
        use crate::pb::MemberRole as PbMemberRole;

        let members = self
            .members
            .values()
            .map(|member| {
                let role: i32 = match member.role {
                    MemberRole::Player => PbMemberRole::Player as i32,
                    MemberRole::Observer => PbMemberRole::Observer as i32,
                };
                RoomMember {
                    player_id: member.player_id.clone(),
                    ready: member.ready,
                    is_owner: member.player_id == self.owner_player_id,
                    offline: member.offline,
                    role,
                }
            })
            .collect();

        RoomSnapshot {
            room_id: self.room_id.clone(),
            owner_player_id: self.owner_player_id.clone(),
            state: self.state_name(),
            members,
            current_frame_id: self.current_frame,
            game_state: self.logic.get_serialized_state(),
        }
    }

    pub fn state_name(&self) -> String {
        match self.phase {
            RoomPhase::InGame => "in_game".to_string(),
            RoomPhase::Waiting => {
                if self.members.is_empty() {
                    return "empty".to_string();
                }

                if self.members.values().all(|member| member.ready) {
                    return "ready".to_string();
                }

                "waiting".to_string()
            }
        }
    }

    pub fn can_start_game(&self, player_id: &str, min_players: usize) -> Result<(), &'static str> {
        if self.phase == RoomPhase::InGame {
            return Err("ROOM_ALREADY_IN_GAME");
        }

        if self.owner_player_id != player_id {
            return Err("ONLY_OWNER_CAN_START");
        }

        if self.members.len() < min_players {
            return Err("ROOM_NOT_ENOUGH_PLAYERS");
        }

        if self.members.values().any(|member| !member.ready) {
            return Err("ROOM_NOT_READY");
        }

        Ok(())
    }

    pub fn can_send_input(&self, player_id: &str) -> Result<(), &'static str> {
        if self.phase != RoomPhase::InGame {
            return Err("ROOM_NOT_IN_GAME");
        }

        if !self.members.contains_key(player_id) {
            return Err("ROOM_MEMBER_NOT_FOUND");
        }

        if let Some(member) = self.members.get(player_id) {
            if member.role == MemberRole::Observer {
                return Err("OBSERVER_CANNOT_SEND_INPUT");
            }
        }

        Ok(())
    }

    pub fn can_end_game(&self, player_id: &str) -> Result<(), &'static str> {
        if self.phase != RoomPhase::InGame {
            return Err("ROOM_NOT_IN_GAME");
        }

        if self.owner_player_id != player_id {
            return Err("ONLY_OWNER_CAN_END_GAME");
        }

        Ok(())
    }

    pub fn reset_to_waiting(&mut self) {
        self.phase = RoomPhase::Waiting;
        self.pending_inputs.clear();
        for member in self.members.values_mut() {
            member.ready = false;
        }
    }

    pub fn update_activity(&mut self) {
        self.last_active_at = Instant::now();
    }

    pub fn mark_empty(&mut self) {
        if self.empty_since.is_none() {
            self.empty_since = Some(Instant::now());
        }
    }

    pub fn clear_empty(&mut self) {
        self.empty_since = None;
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    pub fn should_destroy(&self) -> bool {
        self.marked_for_destruction
    }

    pub fn mark_for_destruction(&mut self) {
        self.marked_for_destruction = true;
    }

    pub fn has_online_members(&self) -> bool {
        self.members.values().any(|m| !m.offline)
    }

    pub fn mark_offline(&mut self, player_id: &str) {
        if let Some(member) = self.members.get_mut(player_id) {
            member.offline = true;
            member.offline_since = Some(Instant::now());
        }
    }

    pub fn mark_online(&mut self, player_id: &str, sender: mpsc::UnboundedSender<OutboundMessage>) -> bool {
        if let Some(member) = self.members.get_mut(player_id) {
            member.offline = false;
            member.offline_since = None;
            member.sender = sender;
            return true;
        }
        false
    }

    pub fn update_sender(&mut self, player_id: &str, sender: mpsc::UnboundedSender<OutboundMessage>) {
        if let Some(member) = self.members.get_mut(player_id) {
            member.sender = sender;
        }
    }

    pub fn collect_expired_offline_players(&self, ttl_secs: u64) -> Vec<String> {
        self.members
            .values()
            .filter(|m| {
                if !m.offline {
                    return false;
                }
                if let Some(since) = m.offline_since {
                    since.elapsed().as_secs() >= ttl_secs
                } else {
                    false
                }
            })
            .map(|m| m.player_id.clone())
            .collect()
    }

    pub fn remove_members(&mut self, player_ids: &[String]) {
        for id in player_ids {
            self.members.remove(id);
        }
        if self.owner_player_id != *"" && !self.members.contains_key(&self.owner_player_id) {
            if let Some(next) = self.members.keys().next() {
                self.owner_player_id = next.clone();
            }
        }
    }

    pub fn push_input_history(&mut self, input: PlayerInputRecord) {
        const MAX_HISTORY: usize = 300;
        self.input_history.push(input);
        if self.input_history.len() > MAX_HISTORY {
            self.input_history.remove(0);
        }
    }

    pub fn get_inputs_in_range(&self, from_frame: u32, to_frame: u32) -> Vec<&PlayerInputRecord> {
        self.input_history
            .iter()
            .filter(|i| i.frame_id >= from_frame && i.frame_id <= to_frame)
            .collect()
    }

    pub fn online_members(&self) -> Vec<&RoomMemberState> {
        self.members.values().filter(|m| !m.offline).collect()
    }

    pub fn set_match_id(&mut self, match_id: String) {
        self.match_id = Some(match_id);
    }

    pub fn is_matched_room(&self) -> bool {
        self.match_id.is_some()
    }
}
