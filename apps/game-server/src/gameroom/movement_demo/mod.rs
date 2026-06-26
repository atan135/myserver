use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::{info, warn};

use crate::core::config_table::ConfigTableRuntime;
use crate::core::logic::{
    ROOM_TRANSFER_SCHEMA_VERSION, RoomLogic, RoomLogicBroadcast, RoomLogicTransfer,
    RoomLogicTransferState,
};
use crate::core::room::PlayerInputRecord;
use crate::core::system::movement::{
    RoomMovementState, decide_corrections, full_sync_broadcast, reject_broadcast,
    snapshot_broadcasts, tick_movement,
};
use crate::core::system::scene::SceneQuery;
use crate::pb::{MovementCorrectionReason, MovementRecoveryState};

const DEFAULT_MOVE_SPEED: f32 = 4.0;
const MOVEMENT_DEMO_TRANSFER_SCHEMA: &str = "movement-demo.logic.v1";

#[derive(Default)]
pub struct MovementDemoLogic {
    pub room_id: String,
    pub tick_count: u64,
    pub default_scene_id: i32,
    pub config_tables: Option<ConfigTableRuntime>,
    pub movement_state: Option<RoomMovementState>,
    pub pending_broadcasts: Vec<RoomLogicBroadcast>,
    pub recipients: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MovementDemoTransferLogicState {
    schema: String,
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    room_id: String,
    tick_count: u64,
    default_scene_id: i32,
    recipients: Vec<String>,
}

impl MovementDemoLogic {
    pub fn new(
        config_tables: ConfigTableRuntime,
        default_scene_id: i32,
        correction_interval_frames: u32,
        correction_threshold: f32,
        aoi_radius: f32,
        aoi_enabled: bool,
        movement_control_stop_frames: u32,
    ) -> Self {
        let mut movement_state =
            RoomMovementState::new(default_scene_id, correction_interval_frames);
        movement_state.set_correction_config(
            correction_interval_frames,
            correction_threshold,
            aoi_radius,
            aoi_enabled,
        );
        movement_state.set_movement_control_stop_frames(movement_control_stop_frames);
        Self {
            room_id: String::new(),
            tick_count: 0,
            default_scene_id,
            config_tables: Some(config_tables),
            movement_state: Some(movement_state),
            pending_broadcasts: Vec::new(),
            recipients: Vec::new(),
        }
    }

    fn spawn_character_if_needed(&mut self, character_id: &str) {
        let Some(config_tables) = self.config_tables.as_ref() else {
            return;
        };
        let config = config_tables.current_snapshot();
        let scene_catalog = config.scene_catalog.as_ref();
        let Some(movement_state) = self.movement_state.as_mut() else {
            return;
        };

        if movement_state.entity(character_id).is_some() {
            return;
        }

        let Some(scene) = scene_catalog.scene(self.default_scene_id) else {
            warn!(
                scene_id = self.default_scene_id,
                "movement demo scene missing"
            );
            return;
        };
        let Some(spawn) = scene_catalog.spawn_point(scene.default_spawn_id) else {
            warn!(
                scene_id = scene.id,
                spawn_id = scene.default_spawn_id,
                "movement demo default spawn missing"
            );
            return;
        };

        movement_state.spawn_character(character_id, spawn, DEFAULT_MOVE_SPEED);
        info!(
            room_id = self.room_id,
            character_id,
            scene_id = scene.id,
            spawn_id = spawn.id,
            "movement demo player spawned"
        );
    }
}

impl RoomLogic for MovementDemoLogic {
    fn on_room_created(&mut self, room_id: &str) {
        self.room_id = room_id.to_string();
        info!(room_id, "[RoomLogic/movement_demo] room created");
    }

    fn on_character_join(&mut self, character_id: &str) {
        if !self
            .recipients
            .iter()
            .any(|existing| existing == character_id)
        {
            self.recipients.push(character_id.to_string());
        }
        self.spawn_character_if_needed(character_id);
    }

    fn on_character_leave(&mut self, character_id: &str) {
        if let Some(movement_state) = self.movement_state.as_mut() {
            movement_state.remove_character(character_id);
        }
        self.recipients.retain(|existing| existing != character_id);
    }

    fn on_character_offline(&mut self, _room_id: &str, character_id: &str) {
        let Some(movement_state) = self.movement_state.as_mut() else {
            return;
        };

        let frame_id = movement_state
            .last_snapshot_frame
            .max(self.tick_count as u32);
        let Some(corrected) = movement_state.stop_character(character_id, frame_id) else {
            return;
        };

        let correction = movement_state.incremental_correction(
            frame_id,
            MovementCorrectionReason::PlayerOffline,
            Vec::new(),
            vec![corrected],
        );
        self.pending_broadcasts
            .extend(snapshot_broadcasts(&self.room_id, vec![correction]));
        info!(
            room_id = self.room_id,
            character_id, frame_id, "movement demo player stopped after offline"
        );
    }

    fn on_game_started(&mut self, _room_id: &str) {
        let Some(movement_state) = self.movement_state.as_mut() else {
            return;
        };
        self.pending_broadcasts.push(full_sync_broadcast(
            &self.room_id,
            movement_state,
            0,
            MovementCorrectionReason::GameStarted,
        ));
    }

    fn on_tick(&mut self, frame_id: u32, fps: u16, inputs: &[PlayerInputRecord]) {
        self.tick_count += 1;
        let Some(config_tables) = self.config_tables.as_ref() else {
            return;
        };
        let config = config_tables.current_snapshot();
        let scene_catalog = config.scene_catalog.as_ref();
        let Some(movement_state) = self.movement_state.as_mut() else {
            return;
        };

        let result = tick_movement(movement_state, frame_id, fps, inputs, scene_catalog);
        for reject in &result.rejects {
            info!(
                room_id = self.room_id,
                frame_id,
                character_id = reject.character_id,
                error_code = reject.error_code,
                "movement input rejected"
            );
            self.pending_broadcasts
                .push(reject_broadcast(&self.room_id, frame_id, reject));
        }

        let corrections = decide_corrections(movement_state, frame_id, &self.recipients, &result);
        if !corrections.is_empty() {
            info!(
                room_id = self.room_id,
                frame_id,
                correction_count = corrections.len(),
                entity_count = movement_state.entity_count(),
                "movement corrections queued"
            );
            self.pending_broadcasts
                .extend(snapshot_broadcasts(&self.room_id, corrections));
        }
    }

    fn get_serialized_state(&self) -> String {
        #[derive(Serialize)]
        struct DemoEntityState {
            entity_id: u64,
            character_id: String,
            scene_id: i32,
            x: f32,
            y: f32,
            dir_x: f32,
            dir_y: f32,
            moving: bool,
            last_input_frame: u32,
        }

        #[derive(Serialize)]
        struct DemoRoomState<'a> {
            room_id: &'a str,
            tick_count: u64,
            scene_id: i32,
            entity_count: usize,
            entities: Vec<DemoEntityState>,
        }

        let Some(movement_state) = self.movement_state.as_ref() else {
            return String::new();
        };

        serde_json::to_string(&DemoRoomState {
            room_id: &self.room_id,
            tick_count: self.tick_count,
            scene_id: movement_state.scene_id,
            entity_count: movement_state.entity_count(),
            entities: movement_state
                .all_transforms()
                .into_iter()
                .map(|entity| DemoEntityState {
                    entity_id: entity.entity_id,
                    character_id: entity.character_id,
                    scene_id: entity.scene_id,
                    x: entity.x,
                    y: entity.y,
                    dir_x: entity.dir_x,
                    dir_y: entity.dir_y,
                    moving: entity.moving,
                    last_input_frame: entity.last_input_frame,
                })
                .collect(),
        })
        .unwrap_or_default()
    }

    fn movement_recovery_state(
        &self,
        requester_character_id: Option<&str>,
        reason: MovementCorrectionReason,
    ) -> Option<MovementRecoveryState> {
        let movement_state = self.movement_state.as_ref()?;
        Some(
            movement_state.recovery_state_for_character(
                requester_character_id,
                movement_state
                    .last_snapshot_frame
                    .max(self.tick_count as u32),
                reason,
            ),
        )
    }

    fn take_pending_broadcasts(&mut self) -> Vec<RoomLogicBroadcast> {
        std::mem::take(&mut self.pending_broadcasts)
    }
}

impl RoomLogicTransfer for MovementDemoLogic {
    fn export_transfer_state(&self) -> Result<RoomLogicTransferState, &'static str> {
        let movement_state = self
            .movement_state
            .as_ref()
            .ok_or("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?;
        let logic_state = MovementDemoTransferLogicState {
            schema: MOVEMENT_DEMO_TRANSFER_SCHEMA.to_string(),
            schema_version: ROOM_TRANSFER_SCHEMA_VERSION,
            room_id: self.room_id.clone(),
            tick_count: self.tick_count,
            default_scene_id: self.default_scene_id,
            recipients: self.recipients.clone(),
        };

        Ok(RoomLogicTransferState {
            schema_version: ROOM_TRANSFER_SCHEMA_VERSION,
            logic_state_json: serde_json::to_string(&logic_state)
                .map_err(|_| "ROOM_TRANSFER_INVALID_LOGIC_STATE")?,
            movement_state_json: movement_state.export_transfer_state_json()?,
            combat_state_json: String::new(),
            npc_state_json: String::new(),
            timer_state_json: String::new(),
        })
    }

    fn import_transfer_state(
        &mut self,
        state: &RoomLogicTransferState,
    ) -> Result<(), &'static str> {
        if state.schema_version != ROOM_TRANSFER_SCHEMA_VERSION {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }

        let logic_state = serde_json::from_str::<serde_json::Value>(&state.logic_state_json)
            .map_err(|_| "ROOM_TRANSFER_INVALID_LOGIC_STATE")?;
        if logic_state
            .get("schema")
            .and_then(serde_json::Value::as_str)
            != Some(MOVEMENT_DEMO_TRANSFER_SCHEMA)
        {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }
        if logic_state
            .get("schemaVersion")
            .and_then(serde_json::Value::as_u64)
            != Some(ROOM_TRANSFER_SCHEMA_VERSION as u64)
        {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }
        let logic_state = serde_json::from_value::<MovementDemoTransferLogicState>(logic_state)
            .map_err(|_| "ROOM_TRANSFER_INVALID_LOGIC_STATE")?;
        if !self.room_id.is_empty() && logic_state.room_id != self.room_id {
            return Err("ROOM_TRANSFER_INVALID_LOGIC_STATE");
        }
        if logic_state.room_id.trim().is_empty() {
            return Err("ROOM_TRANSFER_INVALID_LOGIC_STATE");
        }
        validate_transfer_recipients(&logic_state.recipients)?;
        let movement_state =
            RoomMovementState::import_transfer_state_json(&state.movement_state_json)?;

        self.room_id = logic_state.room_id;
        self.tick_count = logic_state.tick_count;
        self.default_scene_id = logic_state.default_scene_id;
        self.recipients = logic_state.recipients;
        self.movement_state = Some(movement_state);
        self.pending_broadcasts.clear();

        Ok(())
    }
}

fn validate_transfer_recipients(recipients: &[String]) -> Result<(), &'static str> {
    let mut seen = HashSet::new();
    for recipient in recipients {
        if recipient.trim().is_empty() || !seen.insert(recipient.as_str()) {
            return Err("ROOM_TRANSFER_INVALID_LOGIC_STATE");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn transfer_state_with_logic(logic_state_json: String) -> RoomLogicTransferState {
        RoomLogicTransferState {
            schema_version: ROOM_TRANSFER_SCHEMA_VERSION,
            logic_state_json,
            movement_state_json: String::new(),
            combat_state_json: String::new(),
            npc_state_json: String::new(),
            timer_state_json: String::new(),
        }
    }

    #[test]
    fn import_transfer_state_rejects_invalid_logic_identity() {
        let mut logic = MovementDemoLogic::default();
        let duplicate_recipient_state = transfer_state_with_logic(
            json!({
                "schema": MOVEMENT_DEMO_TRANSFER_SCHEMA,
                "schemaVersion": ROOM_TRANSFER_SCHEMA_VERSION,
                "room_id": "room-a",
                "tick_count": 1,
                "default_scene_id": 1,
                "recipients": ["player-a", "player-a"]
            })
            .to_string(),
        );
        assert_eq!(
            logic.import_transfer_state(&duplicate_recipient_state),
            Err("ROOM_TRANSFER_INVALID_LOGIC_STATE")
        );

        let empty_room_state = transfer_state_with_logic(
            json!({
                "schema": MOVEMENT_DEMO_TRANSFER_SCHEMA,
                "schemaVersion": ROOM_TRANSFER_SCHEMA_VERSION,
                "room_id": "",
                "tick_count": 1,
                "default_scene_id": 1,
                "recipients": ["player-a"]
            })
            .to_string(),
        );
        assert_eq!(
            logic.import_transfer_state(&empty_room_state),
            Err("ROOM_TRANSFER_INVALID_LOGIC_STATE")
        );
    }
}
