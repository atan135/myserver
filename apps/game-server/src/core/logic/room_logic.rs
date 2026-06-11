use crate::core::room::PlayerInputRecord;
use crate::protocol::MessageType;

pub const ROOM_TRANSFER_SCHEMA_VERSION: u32 = 1;
pub const UNSUPPORTED_ROOM_TRANSFER: &str = "UNSUPPORTED_ROOM_TRANSFER";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomLogicTransferState {
    pub schema_version: u32,
    pub logic_state_json: String,
    pub movement_state_json: String,
    pub combat_state_json: String,
    pub npc_state_json: String,
    pub timer_state_json: String,
}

impl RoomLogicTransferState {
    pub fn new(schema_version: u32) -> Self {
        Self {
            schema_version,
            logic_state_json: String::new(),
            movement_state_json: String::new(),
            combat_state_json: String::new(),
            npc_state_json: String::new(),
            timer_state_json: String::new(),
        }
    }
}

impl Default for RoomLogicTransferState {
    fn default() -> Self {
        Self::new(ROOM_TRANSFER_SCHEMA_VERSION)
    }
}

pub trait RoomLogicTransfer {
    fn export_transfer_state(&self) -> Result<RoomLogicTransferState, &'static str> {
        Err(UNSUPPORTED_ROOM_TRANSFER)
    }

    fn import_transfer_state(
        &mut self,
        _state: &RoomLogicTransferState,
    ) -> Result<(), &'static str> {
        Err(UNSUPPORTED_ROOM_TRANSFER)
    }
}

pub trait RoomLogic: Send + RoomLogicTransfer {
    fn on_room_created(&mut self, _room_id: &str) {}

    fn on_player_join(&mut self, _player_id: &str) {}

    fn on_player_leave(&mut self, _player_id: &str) {}

    // Disconnection hook for AI takeover or offline state handling.
    fn on_player_offline(&mut self, _room_id: &str, _player_id: &str) {}

    fn on_player_online(&mut self, _room_id: &str, _player_id: &str) {}

    fn on_game_started(&mut self, _room_id: &str) {}

    fn on_game_ended(&mut self, _room_id: &str) {}

    // Called only after framework validation and pending-input upsert.
    // Use this for telemetry or non-authoritative collection only. Authoritative
    // gameplay state changes must be applied in on_tick with resolved frame inputs.
    fn on_player_input(&mut self, _player_id: &str, _action: &str, _payload_json: &str) {}

    fn validate_player_input(
        &self,
        _player_id: &str,
        _action: &str,
        _payload_json: &str,
    ) -> Result<(), &'static str> {
        Ok(())
    }

    // Authoritative frame simulation entry point.
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
