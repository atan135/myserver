use crate::core::room::PlayerInputRecord;
use crate::protocol::MessageType;

#[derive(Debug, Clone)]
pub struct RoomLogicBroadcast {
    pub message_type: MessageType,
    pub body: Vec<u8>,
    pub target_player_ids: Vec<String>,
}

impl RoomLogicBroadcast {
    pub fn broadcast_to_room(message_type: MessageType, body: Vec<u8>) -> Self {
        Self {
            message_type,
            body,
            target_player_ids: Vec::new(),
        }
    }

    pub fn broadcast_to_players(
        message_type: MessageType,
        body: Vec<u8>,
        target_player_ids: Vec<String>,
    ) -> Self {
        Self {
            message_type,
            body,
            target_player_ids,
        }
    }
}

pub trait RoomLogic: Send {
    fn on_room_created(&mut self, _room_id: &str) {}

    fn on_player_join(&mut self, _player_id: &str) {}

    fn on_player_leave(&mut self, _player_id: &str) {}

    // Disconnection hook for AI takeover or offline state handling.
    fn on_player_offline(&mut self, _room_id: &str, _player_id: &str) {}

    fn on_player_online(&mut self, _room_id: &str, _player_id: &str) {}

    fn on_game_started(&mut self, _room_id: &str) {}

    fn on_game_ended(&mut self, _room_id: &str) {}

    fn on_player_input(&mut self, _player_id: &str, _action: &str, _payload_json: &str) {}

    fn on_tick(&mut self, _frame_id: u32, _fps: u16, _inputs: &[PlayerInputRecord]) {}

    fn should_destroy(&self) -> bool {
        false
    }

    fn get_serialized_state(&self) -> String {
        String::new()
    }

    fn restore_from_serialized_state(&mut self, _state: &str) {}

    fn movement_recovery_state(
        &self,
        _requester_player_id: Option<&str>,
        _reason: crate::pb::MovementCorrectionReason,
    ) -> Option<crate::pb::MovementRecoveryState> {
        None
    }

    fn take_pending_broadcasts(&mut self) -> Vec<RoomLogicBroadcast> {
        Vec::new()
    }
}
