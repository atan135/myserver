use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::core::room::PlayerInputRecord;
use crate::protocol::MessageType;

pub const ROOM_TRANSFER_SCHEMA_VERSION: u32 = 1;
pub const ROOM_NPC_TRANSFER_SCHEMA: &str = "room-transfer.npc-state.v1";
pub const ROOM_RUNTIME_TIMER_TRANSFER_SCHEMA: &str = "room-transfer.runtime-timer-state.v1";
pub const UNSUPPORTED_ROOM_TRANSFER: &str = "UNSUPPORTED_ROOM_TRANSFER";
const ROOM_TRANSFER_INVALID_NPC_STATE: &str = "ROOM_TRANSFER_INVALID_NPC_STATE";

#[derive(Debug, Clone)]
pub struct RoomLogicBroadcast {
    pub message_type: MessageType,
    pub body: Vec<u8>,
    pub target_character_ids: Vec<String>,
}

impl RoomLogicBroadcast {
    pub fn broadcast_to_room(message_type: MessageType, body: Vec<u8>) -> Self {
        Self {
            message_type,
            body,
            target_character_ids: Vec::new(),
        }
    }

    pub fn broadcast_to_characters(
        message_type: MessageType,
        body: Vec<u8>,
        target_character_ids: Vec<String>,
    ) -> Self {
        Self {
            message_type,
            body,
            target_character_ids,
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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RoomNpcTransferPosition {
    pub x: f32,
    pub y: f32,
}

impl Default for RoomNpcTransferPosition {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0 }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomNpcTransferThreatEntry {
    #[serde(rename = "targetEntityId", default)]
    pub target_entity_id: Option<u32>,
    #[serde(rename = "targetCharacterId", default)]
    pub target_character_id: Option<String>,
    pub threat: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomNpcTransferSkillState {
    #[serde(rename = "skillId")]
    pub skill_id: u16,
    #[serde(rename = "cooldownRemaining")]
    pub cooldown_remaining: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomNpcTransferPathState {
    #[serde(rename = "pathId", default)]
    pub path_id: Option<String>,
    #[serde(default)]
    pub waypoints: Vec<RoomNpcTransferPosition>,
    #[serde(rename = "nextWaypointIndex", default)]
    pub next_waypoint_index: u32,
}

impl Default for RoomNpcTransferPathState {
    fn default() -> Self {
        Self {
            path_id: None,
            waypoints: Vec::new(),
            next_waypoint_index: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomNpcTransferWaitTimerState {
    #[serde(rename = "timerKind")]
    pub timer_kind: String,
    #[serde(rename = "remainingFrames")]
    pub remaining_frames: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomNpcTransferEntity {
    #[serde(rename = "entityId")]
    pub entity_id: u32,
    #[serde(rename = "entityKind")]
    pub entity_kind: String,
    pub position: RoomNpcTransferPosition,
    pub hp: i32,
    #[serde(rename = "maxHp")]
    pub max_hp: i32,
    #[serde(rename = "targetEntityId", default)]
    pub target_entity_id: Option<u32>,
    #[serde(rename = "targetCharacterId", default)]
    pub target_character_id: Option<String>,
    #[serde(rename = "threatEntries", default)]
    pub threat_entries: Vec<RoomNpcTransferThreatEntry>,
    #[serde(rename = "behaviorNode")]
    pub behavior_node: String,
    #[serde(default)]
    pub blackboard: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub context: BTreeMap<String, serde_json::Value>,
    #[serde(rename = "rngState", default)]
    pub rng_state: Option<String>,
    #[serde(default)]
    pub path: RoomNpcTransferPathState,
    #[serde(rename = "waitTimer", default)]
    pub wait_timer: Option<RoomNpcTransferWaitTimerState>,
    #[serde(rename = "skillCooldowns", default)]
    pub skill_cooldowns: Vec<RoomNpcTransferSkillState>,
}

impl RoomNpcTransferEntity {
    pub fn new(
        entity_id: u32,
        entity_kind: impl Into<String>,
        position: RoomNpcTransferPosition,
        hp: i32,
        max_hp: i32,
        behavior_node: impl Into<String>,
    ) -> Self {
        Self {
            entity_id,
            entity_kind: entity_kind.into(),
            position,
            hp,
            max_hp,
            target_entity_id: None,
            target_character_id: None,
            threat_entries: Vec::new(),
            behavior_node: behavior_node.into(),
            blackboard: BTreeMap::new(),
            context: BTreeMap::new(),
            rng_state: None,
            path: RoomNpcTransferPathState::default(),
            wait_timer: None,
            skill_cooldowns: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomNpcTransferState {
    pub schema: String,
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(default)]
    pub entities: Vec<RoomNpcTransferEntity>,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl RoomNpcTransferState {
    pub fn new() -> Self {
        Self {
            schema: ROOM_NPC_TRANSFER_SCHEMA.to_string(),
            schema_version: ROOM_TRANSFER_SCHEMA_VERSION,
            entities: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    pub fn to_json(&self) -> Result<String, &'static str> {
        self.validate()?;
        serde_json::to_string(self).map_err(|_| ROOM_TRANSFER_INVALID_NPC_STATE)
    }

    pub fn from_json(state_json: &str) -> Result<Self, &'static str> {
        let value = serde_json::from_str::<serde_json::Value>(state_json)
            .map_err(|_| ROOM_TRANSFER_INVALID_NPC_STATE)?;
        if value.get("schema").and_then(serde_json::Value::as_str) != Some(ROOM_NPC_TRANSFER_SCHEMA)
        {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }
        if value
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(ROOM_TRANSFER_SCHEMA_VERSION))
        {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }
        if contains_json_key(&value, "targetPlayerId") {
            return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
        }

        let state =
            serde_json::from_value::<Self>(value).map_err(|_| ROOM_TRANSFER_INVALID_NPC_STATE)?;
        state.validate()?;
        Ok(state)
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema != ROOM_NPC_TRANSFER_SCHEMA {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }
        if self.schema_version != ROOM_TRANSFER_SCHEMA_VERSION {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }

        const MAX_NPC_TRANSFER_ENTITIES: usize = 2048;
        if self.entities.len() > MAX_NPC_TRANSFER_ENTITIES {
            return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
        }

        let mut seen_entity_ids = HashSet::new();
        for entity in &self.entities {
            validate_npc_transfer_entity(entity, &mut seen_entity_ids)?;
        }
        validate_json_map(&self.metadata)?;

        Ok(())
    }
}

impl Default for RoomNpcTransferState {
    fn default() -> Self {
        Self::new()
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

fn validate_npc_transfer_entity(
    entity: &RoomNpcTransferEntity,
    seen_entity_ids: &mut HashSet<u32>,
) -> Result<(), &'static str> {
    validate_npc_entity_id(entity.entity_id)?;
    if !seen_entity_ids.insert(entity.entity_id) {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }
    if entity.entity_kind.trim().is_empty() {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }
    validate_npc_position(entity.position)?;
    validate_npc_health(entity.hp, entity.max_hp)?;
    if let Some(entity_id) = entity.target_entity_id {
        validate_npc_entity_id(entity_id)?;
    }
    if entity
        .target_character_id
        .as_deref()
        .is_some_and(|character_id| character_id.trim().is_empty())
    {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }
    if entity.behavior_node.trim().is_empty() {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }

    validate_npc_threat_entries(&entity.threat_entries)?;
    validate_json_map(&entity.blackboard)?;
    validate_json_map(&entity.context)?;
    if entity
        .rng_state
        .as_deref()
        .is_some_and(|rng_state| rng_state.trim().is_empty())
    {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }
    validate_npc_path_state(&entity.path)?;
    if let Some(wait_timer) = entity.wait_timer.as_ref() {
        validate_npc_wait_timer(wait_timer)?;
    }
    validate_npc_skill_cooldowns(&entity.skill_cooldowns)?;

    Ok(())
}

fn validate_npc_entity_id(entity_id: u32) -> Result<(), &'static str> {
    if entity_id == 0 {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }
    Ok(())
}

fn validate_npc_position(position: RoomNpcTransferPosition) -> Result<(), &'static str> {
    validate_npc_finite(position.x)?;
    validate_npc_finite(position.y)
}

fn validate_npc_finite(value: f32) -> Result<(), &'static str> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(ROOM_TRANSFER_INVALID_NPC_STATE)
    }
}

fn validate_npc_health(hp: i32, max_hp: i32) -> Result<(), &'static str> {
    if hp < 0 || max_hp < 0 || hp > max_hp {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }
    Ok(())
}

fn validate_npc_threat_entries(
    threat_entries: &[RoomNpcTransferThreatEntry],
) -> Result<(), &'static str> {
    const MAX_THREAT_ENTRIES: usize = 1024;
    if threat_entries.len() > MAX_THREAT_ENTRIES {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }

    let mut seen_targets = HashSet::new();
    for entry in threat_entries {
        if entry.threat < 0 {
            return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
        }
        if let Some(entity_id) = entry.target_entity_id {
            validate_npc_entity_id(entity_id)?;
        }
        if entry
            .target_character_id
            .as_deref()
            .is_some_and(|character_id| character_id.trim().is_empty())
        {
            return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
        }
        if entry.target_entity_id.is_none() && entry.target_character_id.is_none() {
            return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
        }
        let target_key = (
            entry.target_entity_id,
            entry.target_character_id.as_deref().unwrap_or_default(),
        );
        if !seen_targets.insert(target_key) {
            return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
        }
    }

    Ok(())
}

fn validate_npc_path_state(path: &RoomNpcTransferPathState) -> Result<(), &'static str> {
    const MAX_PATH_WAYPOINTS: usize = 1024;
    if path
        .path_id
        .as_deref()
        .is_some_and(|path_id| path_id.trim().is_empty())
        || path.waypoints.len() > MAX_PATH_WAYPOINTS
    {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }
    if path.waypoints.is_empty() {
        if path.next_waypoint_index != 0 {
            return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
        }
    } else if path.next_waypoint_index as usize > path.waypoints.len() {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }
    for waypoint in &path.waypoints {
        validate_npc_position(*waypoint)?;
    }
    Ok(())
}

fn validate_npc_wait_timer(wait_timer: &RoomNpcTransferWaitTimerState) -> Result<(), &'static str> {
    if wait_timer.timer_kind.trim().is_empty() {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }
    Ok(())
}

fn validate_npc_skill_cooldowns(
    skill_cooldowns: &[RoomNpcTransferSkillState],
) -> Result<(), &'static str> {
    let mut seen_skill_ids = HashSet::new();
    for skill in skill_cooldowns {
        if skill.skill_id == 0 || !seen_skill_ids.insert(skill.skill_id) {
            return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
        }
    }
    Ok(())
}

fn validate_json_map(map: &BTreeMap<String, serde_json::Value>) -> Result<(), &'static str> {
    const MAX_JSON_MAP_ENTRIES: usize = 256;
    if map.len() > MAX_JSON_MAP_ENTRIES {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }
    for (key, value) in map {
        if key.trim().is_empty() {
            return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
        }
        validate_json_value(value, 0)?;
    }
    Ok(())
}

fn validate_json_value(value: &serde_json::Value, depth: usize) -> Result<(), &'static str> {
    const MAX_JSON_DEPTH: usize = 8;
    const MAX_JSON_ARRAY_LEN: usize = 256;
    const MAX_JSON_OBJECT_LEN: usize = 256;
    if depth > MAX_JSON_DEPTH {
        return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
    }

    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::String(_) => {
            Ok(())
        }
        serde_json::Value::Number(_) => Ok(()),
        serde_json::Value::Array(values) => {
            if values.len() > MAX_JSON_ARRAY_LEN {
                return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
            }
            for value in values {
                validate_json_value(value, depth + 1)?;
            }
            Ok(())
        }
        serde_json::Value::Object(values) => {
            if values.len() > MAX_JSON_OBJECT_LEN {
                return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
            }
            for (key, value) in values {
                if key.trim().is_empty() {
                    return Err(ROOM_TRANSFER_INVALID_NPC_STATE);
                }
                validate_json_value(value, depth + 1)?;
            }
            Ok(())
        }
    }
}

fn contains_json_key(value: &serde_json::Value, needle: &str) -> bool {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| contains_json_key(value, needle)),
        serde_json::Value::Object(values) => values
            .iter()
            .any(|(key, value)| key == needle || contains_json_key(value, needle)),
        _ => false,
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

    fn on_character_join(&mut self, _character_id: &str) {}

    fn on_character_leave(&mut self, _character_id: &str) {}

    // Disconnection hook for AI takeover or offline state handling.
    fn on_character_offline(&mut self, _room_id: &str, _character_id: &str) {}

    fn on_character_online(&mut self, _room_id: &str, _character_id: &str) {}

    fn on_game_started(&mut self, _room_id: &str) {}

    fn on_game_ended(&mut self, _room_id: &str) {}

    // Called only after framework validation and pending-input upsert.
    // Use this for telemetry or non-authoritative collection only. Authoritative
    // gameplay state changes must be applied in on_tick with resolved frame inputs.
    fn on_character_input(&mut self, _character_id: &str, _action: &str, _payload_json: &str) {}

    fn validate_character_input(
        &self,
        _character_id: &str,
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
        _requester_character_id: Option<&str>,
        _reason: crate::pb::MovementCorrectionReason,
    ) -> Option<crate::pb::MovementRecoveryState> {
        None
    }

    fn take_pending_broadcasts(&mut self) -> Vec<RoomLogicBroadcast> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npc_transfer_json_uses_target_character_id_fields() {
        let mut state = RoomNpcTransferState::new();
        let mut entity = RoomNpcTransferEntity::new(
            1,
            "monster",
            RoomNpcTransferPosition { x: 12.0, y: 34.0 },
            80,
            100,
            "attack",
        );
        entity.target_character_id = Some("chr_0000000000001".to_string());
        entity.threat_entries.push(RoomNpcTransferThreatEntry {
            target_entity_id: None,
            target_character_id: Some("chr_0000000000002".to_string()),
            threat: 42,
        });
        state.entities.push(entity);

        let json = state.to_json().expect("npc transfer state should serialize");
        assert!(json.contains("targetCharacterId"));
        assert!(!json.contains("targetPlayerId"));

        let value: serde_json::Value =
            serde_json::from_str(&json).expect("npc transfer json should be valid");
        assert_eq!(
            value["entities"][0]["targetCharacterId"],
            "chr_0000000000001"
        );
        assert_eq!(
            value["entities"][0]["threatEntries"][0]["targetCharacterId"],
            "chr_0000000000002"
        );

        let restored =
            RoomNpcTransferState::from_json(&json).expect("npc transfer state should restore");
        assert_eq!(
            restored.entities[0].target_character_id.as_deref(),
            Some("chr_0000000000001")
        );
        assert_eq!(
            restored.entities[0].threat_entries[0]
                .target_character_id
                .as_deref(),
            Some("chr_0000000000002")
        );
    }

    #[test]
    fn npc_transfer_json_rejects_legacy_target_player_id_only_threat_entry() {
        let legacy_json = serde_json::json!({
            "schema": ROOM_NPC_TRANSFER_SCHEMA,
            "schemaVersion": ROOM_TRANSFER_SCHEMA_VERSION,
            "entities": [
                {
                    "entityId": 1,
                    "entityKind": "monster",
                    "position": { "x": 0.0, "y": 0.0 },
                    "hp": 10,
                    "maxHp": 10,
                    "targetPlayerId": "chr_0000000000001",
                    "threatEntries": [
                        {
                            "targetPlayerId": "chr_0000000000001",
                            "threat": 10
                        }
                    ],
                    "behaviorNode": "attack"
                }
            ]
        });

        assert_eq!(
            RoomNpcTransferState::from_json(&legacy_json.to_string()),
            Err(ROOM_TRANSFER_INVALID_NPC_STATE)
        );
    }

    #[test]
    fn npc_transfer_json_rejects_legacy_entity_target_player_id_even_with_valid_targets() {
        let legacy_json = serde_json::json!({
            "schema": ROOM_NPC_TRANSFER_SCHEMA,
            "schemaVersion": ROOM_TRANSFER_SCHEMA_VERSION,
            "entities": [
                {
                    "entityId": 1,
                    "entityKind": "monster",
                    "position": { "x": 0.0, "y": 0.0 },
                    "hp": 10,
                    "maxHp": 10,
                    "targetPlayerId": "chr_0000000000001",
                    "targetCharacterId": "chr_0000000000001",
                    "threatEntries": [
                        {
                            "targetCharacterId": "chr_0000000000002",
                            "threat": 10
                        }
                    ],
                    "behaviorNode": "attack"
                }
            ]
        });

        assert_eq!(
            RoomNpcTransferState::from_json(&legacy_json.to_string()),
            Err(ROOM_TRANSFER_INVALID_NPC_STATE)
        );
    }
}
