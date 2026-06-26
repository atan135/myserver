use super::*;

use prost::Message;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::core::logic::{
    ROOM_TRANSFER_SCHEMA_VERSION, RoomLogicTransferState, RoomRuntimeTimerTransferState,
    UNSUPPORTED_ROOM_TRANSFER,
};
use crate::core::runtime::room_policy::{MissingInputStrategy, RoomRuntimePolicy};
use crate::pb::RoomTransferPayload;

pub(super) fn frame_input_from_record(input: &PlayerInputRecord) -> FrameInput {
    FrameInput {
        character_id: input.character_id.clone(),
        action: input.action.clone(),
        payload_json: input.payload_json.clone(),
        frame_id: input.frame_id,
    }
}

pub(super) fn room_frame_inputs_from_history(
    room: &Room,
    current_frame_id: u32,
) -> Vec<FrameInput> {
    room.get_inputs_in_range(current_frame_id.saturating_sub(300), current_frame_id)
        .into_iter()
        .map(frame_input_from_record)
        .collect()
}

pub(super) fn room_frame_inputs_from_pending(room: &Room, frame_id: u32) -> Vec<FrameInput> {
    room.pending_inputs_for_frame(frame_id)
        .into_iter()
        .map(frame_input_from_record)
        .collect()
}

pub(super) fn character_input_record_from_frame_input(
    input: FrameInput,
    is_synthetic: bool,
) -> PlayerInputRecord {
    PlayerInputRecord {
        frame_id: input.frame_id,
        character_id: input.character_id,
        action: input.action,
        payload_json: input.payload_json,
        received_at: Instant::now(),
        is_synthetic,
    }
}

pub(super) fn room_phase_name(phase: RoomPhase) -> &'static str {
    match phase {
        RoomPhase::Waiting => "waiting",
        RoomPhase::InGame => "in_game",
    }
}

pub(super) fn parse_room_phase(value: &str) -> Result<RoomPhase, &'static str> {
    match value {
        "waiting" | "empty" | "ready" => Ok(RoomPhase::Waiting),
        "in_game" => Ok(RoomPhase::InGame),
        _ => Err("ROOM_TRANSFER_INVALID_PHASE"),
    }
}

pub(super) fn room_transfer_checksum(payload: &RoomTransferPayload) -> String {
    let mut canonical = payload.clone();
    canonical.checksum.clear();
    let mut encoded = Vec::new();
    canonical
        .encode(&mut encoded)
        .expect("room transfer payload encode failed");
    format!("{:x}", Sha256::digest(&encoded))
}

pub(super) fn room_transfer_logic_state_json(state: &RoomLogicTransferState) -> String {
    json!({
        "schema": "room-transfer.logic.v1",
        "schemaVersion": state.schema_version,
        "logicStateJson": state.logic_state_json,
        "combatStateJson": state.combat_state_json,
        "npcStateJson": state.npc_state_json,
    })
    .to_string()
}

pub(super) fn room_transfer_movement_state_json(state: &RoomLogicTransferState) -> String {
    json!({
        "schema": "room-transfer.movement.v1",
        "schemaVersion": state.schema_version,
        "movementStateJson": state.movement_state_json,
    })
    .to_string()
}

pub(super) fn room_transfer_timer_state_json(
    state: &RoomLogicTransferState,
    runtime_summary: serde_json::Value,
) -> Result<String, &'static str> {
    let timer_state_json = state
        .timer_transfer_state()?
        .map(|timer_state| timer_state.to_json())
        .transpose()?
        .unwrap_or_default();

    Ok(json!({
        "schema": "room-transfer.runtime-timers.v1",
        "schemaVersion": state.schema_version,
        "timerStateJson": timer_state_json,
        "runtimeSummary": runtime_summary,
    })
    .to_string())
}

fn validate_room_transfer_runtime_summary(summary: &serde_json::Value) -> Result<(), &'static str> {
    let summary = summary
        .as_object()
        .ok_or("ROOM_TRANSFER_INVALID_TIMER_STATE")?;

    if !summary
        .get("hasEmptySince")
        .is_some_and(serde_json::Value::is_boolean)
    {
        return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
    }
    if !summary
        .get("hasWaitStarted")
        .is_some_and(serde_json::Value::is_boolean)
    {
        return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
    }
    if summary
        .get("inputDelayFrames")
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value <= u64::from(u32::MAX))
        .is_none()
    {
        return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
    }
    if summary
        .get("snapshotIntervalFrames")
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value > 0 && *value <= u64::from(u32::MAX))
        .is_none()
    {
        return Err("ROOM_TRANSFER_INVALID_TIMER_STATE");
    }

    Ok(())
}

fn validate_room_transfer_timer_wrapper<'a>(
    timers: &'a serde_json::Value,
) -> Result<&'a str, &'static str> {
    if timers.get("schema").and_then(|value| value.as_str())
        != Some("room-transfer.runtime-timers.v1")
    {
        return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
    }
    if timers.get("schemaVersion").and_then(|value| value.as_u64())
        != Some(u64::from(ROOM_TRANSFER_SCHEMA_VERSION))
    {
        return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
    }

    let timer_state_json = timers
        .get("timerStateJson")
        .and_then(|value| value.as_str())
        .ok_or("ROOM_TRANSFER_INVALID_TIMER_STATE")?;
    RoomRuntimeTimerTransferState::from_optional_json(timer_state_json)?;
    let runtime_summary = timers
        .get("runtimeSummary")
        .ok_or("ROOM_TRANSFER_INVALID_TIMER_STATE")?;
    validate_room_transfer_runtime_summary(runtime_summary)?;

    Ok(timer_state_json)
}

pub(super) fn room_transfer_state_from_payload(
    payload: &RoomTransferPayload,
) -> Result<RoomLogicTransferState, &'static str> {
    let logic =
        serde_json::from_str::<serde_json::Value>(&payload.logic_state_json).map_err(|_| {
            if payload.logic_state_json.trim().is_empty() {
                UNSUPPORTED_ROOM_TRANSFER
            } else {
                "ROOM_TRANSFER_INVALID_LOGIC_STATE"
            }
        })?;
    let movement = serde_json::from_str::<serde_json::Value>(&payload.movement_state_json)
        .map_err(|_| "ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?;
    let timers = serde_json::from_str::<serde_json::Value>(&payload.runtime_timers_json)
        .map_err(|_| "ROOM_TRANSFER_INVALID_TIMER_STATE")?;

    if logic.get("schema").and_then(|value| value.as_str()) != Some("room-transfer.logic.v1") {
        return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
    }
    if movement.get("schema").and_then(|value| value.as_str()) != Some("room-transfer.movement.v1")
    {
        return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
    }
    let timer_state_json = validate_room_transfer_timer_wrapper(&timers)?;

    let schema_version = logic
        .get("schemaVersion")
        .and_then(|value| value.as_u64())
        .ok_or("ROOM_TRANSFER_UNSUPPORTED_SCHEMA")?;
    if schema_version != ROOM_TRANSFER_SCHEMA_VERSION as u64
        || movement
            .get("schemaVersion")
            .and_then(|value| value.as_u64())
            != Some(schema_version)
    {
        return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
    }

    Ok(RoomLogicTransferState {
        schema_version: schema_version as u32,
        logic_state_json: logic
            .get("logicStateJson")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        movement_state_json: movement
            .get("movementStateJson")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        combat_state_json: logic
            .get("combatStateJson")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        npc_state_json: logic
            .get("npcStateJson")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        timer_state_json: timer_state_json.to_string(),
    })
}

pub(super) fn validate_room_transfer_payload(
    payload: &RoomTransferPayload,
) -> Result<(), &'static str> {
    if payload.rollout_epoch.trim().is_empty() {
        return Err("INVALID_ROLLOUT_EPOCH");
    }
    if payload.room_id.trim().is_empty() {
        return Err("ROOM_TRANSFER_INVALID_ROOM_ID");
    }
    if payload.policy_id.trim().is_empty() {
        return Err("ROOM_TRANSFER_INVALID_POLICY");
    }
    if payload.owner_character_id.trim().is_empty() {
        return Err("ROOM_TRANSFER_INVALID_OWNER");
    }
    if payload.snapshot.is_none() {
        return Err("ROOM_TRANSFER_MISSING_SNAPSHOT");
    }
    if payload.checksum.trim().is_empty() {
        return Err("ROOM_TRANSFER_CHECKSUM_MISMATCH");
    }
    let expected = room_transfer_checksum(payload);
    if expected != payload.checksum {
        return Err("ROOM_TRANSFER_CHECKSUM_MISMATCH");
    }
    parse_room_phase(&payload.room_phase)?;
    Ok(())
}

fn synthetic_empty_input(frame_id: u32, character_id: &str) -> PlayerInputRecord {
    PlayerInputRecord {
        frame_id,
        character_id: character_id.to_string(),
        action: String::new(),
        payload_json: String::new(),
        received_at: Instant::now(),
        is_synthetic: true,
    }
}

fn clone_input_for_frame(frame_id: u32, input: &PlayerInputRecord) -> PlayerInputRecord {
    PlayerInputRecord {
        frame_id,
        character_id: input.character_id.clone(),
        action: input.action.clone(),
        payload_json: input.payload_json.clone(),
        received_at: Instant::now(),
        is_synthetic: true,
    }
}

pub(super) fn resolve_tick_inputs(
    room: &mut Room,
    participants: &[String],
    frame_id: u32,
    policy: &RoomRuntimePolicy,
) -> (Vec<PlayerInputRecord>, Vec<String>) {
    let mut frame_inputs = room.take_pending_inputs_for_frame(frame_id);
    let mut resolved_inputs = Vec::with_capacity(participants.len());
    let mut newly_offline_characters = Vec::new();

    for character_id in participants {
        if let Some(input) = frame_inputs.remove(character_id) {
            room.reset_missing_input_streak(character_id);
            room.set_last_applied_input(character_id, input.clone());
            resolved_inputs.push(input);
            continue;
        }

        let resolved = match policy.missing_input_strategy {
            MissingInputStrategy::Empty => synthetic_empty_input(frame_id, character_id),
            MissingInputStrategy::RepeatLast => room
                .last_applied_input_for_character(character_id)
                .map(|input| clone_input_for_frame(frame_id, input))
                .unwrap_or_else(|| synthetic_empty_input(frame_id, character_id)),
            MissingInputStrategy::DropAfterMisses => {
                let streak = room.increment_missing_input_streak(character_id);
                if streak >= MAX_MISSING_INPUT_STREAK_BEFORE_OFFLINE {
                    let should_mark_offline = room
                        .members
                        .get(character_id)
                        .map(|member| !member.offline)
                        .unwrap_or(false);
                    if should_mark_offline {
                        if let Some(member) = room.members.get_mut(character_id) {
                            member.offline = true;
                            member.offline_since = Some(Instant::now());
                        }
                        room.logic.on_character_offline(&room.room_id, character_id);
                        newly_offline_characters.push(character_id.clone());
                    }
                }
                synthetic_empty_input(frame_id, character_id)
            }
        };

        room.set_last_applied_input(character_id, resolved.clone());
        resolved_inputs.push(resolved);
    }

    (resolved_inputs, newly_offline_characters)
}
