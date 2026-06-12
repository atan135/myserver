use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::core::room::PlayerInputRecord;
use crate::protocol::MessageType;

pub const ROOM_TRANSFER_SCHEMA_VERSION: u32 = 1;
pub const ROOM_RUNTIME_TIMER_TRANSFER_SCHEMA: &str = "room-transfer.runtime-timer-state.v1";
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

    pub fn timer_transfer_state(
        &self,
    ) -> Result<Option<RoomRuntimeTimerTransferState>, &'static str> {
        RoomRuntimeTimerTransferState::from_optional_json(&self.timer_state_json)
    }
}

impl Default for RoomLogicTransferState {
    fn default() -> Self {
        Self::new(ROOM_TRANSFER_SCHEMA_VERSION)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomRuntimeTimerTransferSummary {
    #[serde(rename = "ownerKind")]
    pub owner_kind: String,
    #[serde(rename = "logicalFrame")]
    pub logical_frame: u32,
    #[serde(rename = "logicalTick")]
    pub logical_tick: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomTimerTransferEntry {
    pub id: String,
    #[serde(rename = "timerKind")]
    pub timer_kind: String,
    #[serde(rename = "remainingFrames")]
    pub remaining_frames: u32,
    #[serde(rename = "repeatIntervalFrames", default)]
    pub repeat_interval_frames: Option<u32>,
    #[serde(rename = "payloadJson", default)]
    pub payload_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSchedulerTransferEntry {
    pub id: String,
    #[serde(rename = "schedulerKind")]
    pub scheduler_kind: String,
    #[serde(rename = "nextFrame")]
    pub next_frame: u32,
    #[serde(rename = "intervalFrames", default)]
    pub interval_frames: Option<u32>,
    #[serde(rename = "payloadJson", default)]
    pub payload_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomRuntimeTimerTransferState {
    pub schema: String,
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "runtimeSummary")]
    pub runtime_summary: RoomRuntimeTimerTransferSummary,
    #[serde(rename = "timerEntries", default)]
    pub timer_entries: Vec<RoomTimerTransferEntry>,
    #[serde(rename = "schedulerEntries", default)]
    pub scheduler_entries: Vec<RoomSchedulerTransferEntry>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl RoomRuntimeTimerTransferState {
    pub fn new(owner_kind: impl Into<String>, logical_frame: u32, logical_tick: u64) -> Self {
        Self {
            schema: ROOM_RUNTIME_TIMER_TRANSFER_SCHEMA.to_string(),
            schema_version: ROOM_TRANSFER_SCHEMA_VERSION,
            runtime_summary: RoomRuntimeTimerTransferSummary {
                owner_kind: owner_kind.into(),
                logical_frame,
                logical_tick,
            },
            timer_entries: Vec::new(),
            scheduler_entries: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    pub fn to_json(&self) -> Result<String, &'static str> {
        self.validate()?;
        serde_json::to_string(self).map_err(|_| "ROOM_TRANSFER_INVALID_TIMER_STATE")
    }

    pub fn from_json(state_json: &str) -> Result<Self, &'static str> {
        let state = serde_json::from_str::<Self>(state_json)
            .map_err(|_| "ROOM_TRANSFER_INVALID_TIMER_STATE")?;
        state.validate()?;
        Ok(state)
    }

    pub fn from_optional_json(state_json: &str) -> Result<Option<Self>, &'static str> {
        if state_json.trim().is_empty() {
            return Ok(None);
        }
        Self::from_json(state_json).map(Some)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema != ROOM_RUNTIME_TIMER_TRANSFER_SCHEMA {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }
        if self.schema_version != ROOM_TRANSFER_SCHEMA_VERSION {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }
        if self.runtime_summary.owner_kind.trim().is_empty() {
            return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
        }

        const MAX_RUNTIME_TIMER_ENTRIES: usize = 1024;
        if self.timer_entries.len() + self.scheduler_entries.len() > MAX_RUNTIME_TIMER_ENTRIES {
            return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
        }

        let mut seen_ids = HashSet::new();
        for entry in &self.timer_entries {
            validate_transfer_entry_id(&entry.id, &mut seen_ids)?;
            if entry.timer_kind.trim().is_empty() {
                return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
            }
            validate_optional_interval(entry.repeat_interval_frames)?;
            validate_payload_json(&entry.payload_json)?;
        }

        for entry in &self.scheduler_entries {
            validate_transfer_entry_id(&entry.id, &mut seen_ids)?;
            if entry.scheduler_kind.trim().is_empty() {
                return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
            }
            validate_optional_interval(entry.interval_frames)?;
            validate_payload_json(&entry.payload_json)?;
        }

        for key in self.metadata.keys() {
            if key.trim().is_empty() {
                return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
            }
        }

        Ok(())
    }
}

fn validate_transfer_entry_id(
    id: &str,
    seen_ids: &mut HashSet<String>,
) -> Result<(), &'static str> {
    if id.trim().is_empty() || !seen_ids.insert(id.to_string()) {
        return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
    }
    Ok(())
}

fn validate_optional_interval(interval_frames: Option<u32>) -> Result<(), &'static str> {
    if interval_frames == Some(0) {
        return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
    }
    Ok(())
}

fn validate_payload_json(payload_json: &str) -> Result<(), &'static str> {
    if payload_json.trim().is_empty() {
        return Ok(());
    }
    serde_json::from_str::<serde_json::Value>(payload_json)
        .map(|_| ())
        .map_err(|_| "ROOM_TRANSFER_INVALID_TIMER_STATE")
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
