use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::{info, warn};

use crate::core::config_table::ConfigTableRuntime;
use crate::core::logic::{
    ROOM_TRANSFER_SCHEMA_VERSION, RoomLogic, RoomLogicBroadcast, RoomLogicTransfer,
    RoomLogicTransferState,
};
use crate::core::room::PlayerInputRecord;
use crate::core::runtime::room_policy::MOVEMENT_DEMO_DEFAULT_SPEED_METERS_PER_SECOND;
use crate::core::system::movement::{
    RoomMovementState, decide_corrections, full_sync_broadcast, reject_broadcast,
    snapshot_broadcasts, tick_movement,
};
use crate::core::system::scene::{SceneCatalog, SceneQuery};
use crate::pb::{MovementCorrectionReason, MovementRecoveryState};

const DEFAULT_MOVE_SPEED: f32 = MOVEMENT_DEMO_DEFAULT_SPEED_METERS_PER_SECOND;
const MOVEMENT_DEMO_TRANSFER_SCHEMA: &str = "movement-demo.logic.v1";
const MOVEMENT_TRANSFER_CONFIG_EPSILON: f32 = 0.000_001;

#[derive(Default)]
pub struct MovementDemoLogic {
    pub room_id: String,
    pub tick_count: u64,
    pub current_frame: u32,
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
    #[serde(default)]
    current_frame: u32,
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
            current_frame: 0,
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

        // Public-world players share the configured spawn. Collision and spawn offsets are not
        // part of movement_demo, so no other character can alter this character's spawn state.
        movement_state.spawn_character(character_id, spawn, DEFAULT_MOVE_SPEED);
        info!(
            room_id = self.room_id,
            character_id,
            scene_id = scene.id,
            spawn_id = spawn.id,
            "movement demo player spawned"
        );
    }

    fn validate_imported_movement_state(
        &self,
        logic_state: &MovementDemoTransferLogicState,
        imported: &RoomMovementState,
        scene_catalog: &SceneCatalog,
    ) -> Result<(), &'static str> {
        let expected = self
            .movement_state
            .as_ref()
            .ok_or("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?;
        if logic_state.default_scene_id != self.default_scene_id
            || imported.scene_id != logic_state.default_scene_id
            || imported.scene_id != expected.scene_id
            || imported.correction_interval_frames != expected.correction_interval_frames
            || imported.movement_control_stop_frames != expected.movement_control_stop_frames
            || imported.aoi_enabled != expected.aoi_enabled
            || !transfer_float_matches(
                imported.correction_distance_threshold,
                expected.correction_distance_threshold,
            )
            || !transfer_float_matches(imported.aoi_radius, expected.aoi_radius)
        {
            return Err("ROOM_TRANSFER_INCOMPATIBLE_MOVEMENT_STATE");
        }

        if scene_catalog.scene(imported.scene_id).is_none() {
            return Err("ROOM_TRANSFER_INCOMPATIBLE_MOVEMENT_STATE");
        }
        for dense_index in imported.dense_indices() {
            let entity = imported
                .entity_state_at(dense_index)
                .ok_or("ROOM_TRANSFER_INCOMPATIBLE_MOVEMENT_STATE")?;
            if entity.scene_id != imported.scene_id
                || !transfer_float_matches(entity.speed, DEFAULT_MOVE_SPEED)
                || !scene_catalog.is_walkable(entity.scene_id, entity.position.x, entity.position.y)
            {
                return Err("ROOM_TRANSFER_INCOMPATIBLE_MOVEMENT_STATE");
            }
        }
        Ok(())
    }
}

fn transfer_float_matches(left: f32, right: f32) -> bool {
    (left - right).abs() <= MOVEMENT_TRANSFER_CONFIG_EPSILON
}

impl RoomLogic for MovementDemoLogic {
    fn on_room_created(&mut self, room_id: &str) {
        self.room_id = room_id.to_string();
        info!(room_id, "[RoomLogic/movement_demo] room created");
    }

    fn on_character_join(&mut self, character_id: &str) {
        let existing_recipients = self.recipients.clone();
        if !self
            .recipients
            .iter()
            .any(|existing| existing == character_id)
        {
            self.recipients.push(character_id.to_string());
        }
        self.spawn_character_if_needed(character_id);

        if existing_recipients.is_empty() {
            return;
        }
        let frame_id = self.authority_frame();
        let Some(movement_state) = self.movement_state.as_mut() else {
            return;
        };
        let entities = movement_state.all_transforms();
        let correction = movement_state.incremental_correction(
            frame_id,
            MovementCorrectionReason::Periodic,
            existing_recipients,
            entities,
        );
        self.pending_broadcasts
            .extend(snapshot_broadcasts(&self.room_id, vec![correction]));
    }

    fn on_character_leave(&mut self, character_id: &str) {
        if let Some(movement_state) = self.movement_state.as_mut() {
            movement_state.remove_character(character_id);
        }
        self.recipients.retain(|existing| existing != character_id);
    }

    fn on_character_offline(&mut self, _room_id: &str, character_id: &str) {
        let frame_id = self.authority_frame();
        let Some(movement_state) = self.movement_state.as_mut() else {
            return;
        };

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
        self.tick_count = 0;
        self.current_frame = 0;
        let Some(movement_state) = self.movement_state.as_mut() else {
            return;
        };
        movement_state.reset_authority_epoch(0);
        self.pending_broadcasts.push(full_sync_broadcast(
            &self.room_id,
            movement_state,
            0,
            MovementCorrectionReason::GameStarted,
        ));
    }

    fn on_tick(&mut self, frame_id: u32, fps: u16, inputs: &[PlayerInputRecord]) {
        self.tick_count += 1;
        self.current_frame = frame_id;
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
        Some(movement_state.recovery_state_for_character(
            requester_character_id,
            self.authority_frame(),
            reason,
        ))
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
            current_frame: self.current_frame,
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
        let config_tables = self
            .config_tables
            .as_ref()
            .ok_or("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?;
        let config = config_tables.current_snapshot();
        self.validate_imported_movement_state(
            &logic_state,
            &movement_state,
            config.scene_catalog.as_ref(),
        )?;

        self.room_id = logic_state.room_id;
        self.tick_count = logic_state.tick_count;
        self.current_frame = logic_state
            .current_frame
            .max(movement_state.last_snapshot_frame);
        self.default_scene_id = logic_state.default_scene_id;
        self.recipients = logic_state.recipients;
        self.movement_state = Some(movement_state);
        self.pending_broadcasts.clear();

        Ok(())
    }
}

impl MovementDemoLogic {
    fn authority_frame(&self) -> u32 {
        self.current_frame
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
    use crate::core::config_table::ConfigTableRuntime;
    use crate::core::runtime::room_policy::{
        MOVEMENT_DEMO_AOI_ENABLED, MOVEMENT_DEMO_CONTROL_STOP_FRAMES,
        MOVEMENT_DEMO_CORRECTION_INTERVAL_FRAMES, MOVEMENT_DEMO_CORRECTION_THRESHOLD_METERS,
    };
    use crate::core::system::movement::input::MovementCommand;
    use crate::core::system::movement::state::Vec2;
    use crate::pb::MovementCorrectionKind;
    use prost::Message;
    use serde_json::json;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_CONFIG_COUNTER: AtomicU64 = AtomicU64::new(1);

    struct TempConfigDir {
        root: PathBuf,
        csv_dir: PathBuf,
        scene_dir: PathBuf,
    }

    impl TempConfigDir {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "myserver-movement-reload-{}-{}",
                std::process::id(),
                TEMP_CONFIG_COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let csv_dir = root.join("csv");
            let scene_dir = root.join("scene");
            fs::create_dir_all(&csv_dir).unwrap();
            fs::create_dir_all(&scene_dir).unwrap();
            let manifest_root = Path::new(env!("CARGO_MANIFEST_DIR"));
            copy_dir(&manifest_root.join("csv"), &csv_dir);
            copy_dir(&manifest_root.join("scene"), &scene_dir);
            Self {
                root,
                csv_dir,
                scene_dir,
            }
        }
    }

    impl Drop for TempConfigDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn copy_dir(source: &Path, target: &Path) {
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let source_path = entry.path();
            let target_path = target.join(entry.file_name());
            if source_path.is_dir() {
                fs::create_dir_all(&target_path).unwrap();
                copy_dir(&source_path, &target_path);
            } else {
                fs::copy(source_path, target_path).unwrap();
            }
        }
    }

    fn config_tables() -> ConfigTableRuntime {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        ConfigTableRuntime::load_with_scene_dir(&root.join("csv"), &root.join("scene"))
            .expect("game-server config fixture should load")
    }

    fn movement_demo_logic() -> MovementDemoLogic {
        MovementDemoLogic::new(
            config_tables(),
            1,
            MOVEMENT_DEMO_CORRECTION_INTERVAL_FRAMES,
            MOVEMENT_DEMO_CORRECTION_THRESHOLD_METERS,
            16.0,
            MOVEMENT_DEMO_AOI_ENABLED,
            MOVEMENT_DEMO_CONTROL_STOP_FRAMES,
        )
    }

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

    #[test]
    fn character_states_remain_independent_through_offline_and_leave_paths() {
        let mut logic = movement_demo_logic();
        logic.on_room_created("main-world-public");
        logic.on_character_join("character-a");
        logic.on_character_join("character-b");

        let character_b_before = {
            let state = logic
                .movement_state
                .as_mut()
                .expect("movement state should be initialized");
            assert_eq!(state.entity_count(), 2);
            let a_index = state
                .dense_index_by_character("character-a")
                .expect("character-a should be spawned");
            let b_index = state
                .dense_index_by_character("character-b")
                .expect("character-b should be spawned");
            let character_a_spawned = state.entity("character-a").unwrap();
            let character_b_spawned = state.entity("character-b").unwrap();
            assert_eq!(character_a_spawned.character_id, "character-a");
            assert_eq!(character_b_spawned.character_id, "character-b");
            assert_ne!(character_a_spawned.entity_id, character_b_spawned.entity_id);
            for entity in [&character_a_spawned, &character_b_spawned] {
                assert_eq!(entity.position.x, 2002.0);
                assert_eq!(entity.position.y, 2002.0);
                assert_eq!(entity.direction.x, 1.0);
                assert_eq!(entity.direction.y, 0.0);
                assert_eq!(entity.last_input_frame, 0);
            }
            assert!(state.apply_command_at(
                a_index,
                11,
                MovementCommand::MoveDir(Vec2 { x: 1.0, y: 0.0 })
            ));
            assert!(state.apply_command_at(
                b_index,
                17,
                MovementCommand::MoveDir(Vec2 { x: 0.0, y: 1.0 })
            ));
            let character_a = state.entity("character-a").unwrap();
            let character_b = state.entity("character-b").unwrap();
            assert_eq!(character_a.character_id, "character-a");
            assert_eq!(character_a.direction.x, 1.0);
            assert_eq!(character_a.direction.y, 0.0);
            assert_eq!(character_a.last_input_frame, 11);
            assert_eq!(character_b.character_id, "character-b");
            assert_eq!(character_b.direction.x, 0.0);
            assert_eq!(character_b.direction.y, 1.0);
            assert_eq!(character_b.last_input_frame, 17);
            character_b
        };

        logic.tick_count = 20;
        logic.current_frame = 20;
        logic.on_character_offline("main-world-public", "character-a");
        {
            let state = logic.movement_state.as_ref().unwrap();
            let character_a = state.entity("character-a").unwrap();
            let character_b_after_offline = state.entity("character-b").unwrap();
            assert!(!character_a.moving);
            assert_eq!(character_a.last_input_frame, 20);
            assert_eq!(
                character_b_after_offline.entity_id,
                character_b_before.entity_id
            );
            assert_eq!(
                character_b_after_offline.position.x,
                character_b_before.position.x
            );
            assert_eq!(
                character_b_after_offline.position.y,
                character_b_before.position.y
            );
            assert_eq!(
                character_b_after_offline.direction.x,
                character_b_before.direction.x
            );
            assert_eq!(
                character_b_after_offline.direction.y,
                character_b_before.direction.y
            );
            assert!(character_b_after_offline.moving);
            assert_eq!(character_b_after_offline.last_input_frame, 17);
        }

        // The room manager invokes this hook only after the offline TTL expires.
        logic.on_character_leave("character-a");
        let state = logic.movement_state.as_ref().unwrap();
        assert!(state.entity("character-a").is_none());
        assert_eq!(state.entity_count(), 1);
        let character_b_after_leave = state.entity("character-b").unwrap();
        assert_eq!(
            character_b_after_leave.entity_id,
            character_b_before.entity_id
        );
        assert_eq!(character_b_after_leave.last_input_frame, 17);
        assert!(character_b_after_leave.moving);
    }

    #[test]
    fn joining_character_queues_membership_snapshot_for_existing_recipients() {
        let mut logic = movement_demo_logic();
        logic.on_room_created("main-world-public");
        logic.on_character_join("character-a");
        assert!(logic.take_pending_broadcasts().is_empty());

        logic.tick_count = 999;
        logic.current_frame = 12;
        logic.on_character_join("character-b");

        let broadcasts = logic.take_pending_broadcasts();
        assert_eq!(broadcasts.len(), 1);
        assert_eq!(
            broadcasts[0].target_character_ids,
            vec!["character-a".to_string()]
        );
        let snapshot = crate::pb::MovementSnapshotPush::decode(broadcasts[0].body.as_slice())
            .expect("join membership snapshot should decode");
        assert_eq!(snapshot.frame_id, 12);
        assert!(!snapshot.full_sync);
        assert_eq!(
            snapshot.correction_kind,
            MovementCorrectionKind::Incremental as i32
        );
        assert_eq!(
            snapshot
                .entities
                .iter()
                .map(|entity| entity.character_id.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(["character-a", "character-b"])
        );
    }

    #[test]
    fn offline_recovery_stops_authoritative_movement_without_losing_last_input() {
        let mut logic = movement_demo_logic();
        logic.on_room_created("main-world-public");
        logic.on_character_join("character-a");
        let state = logic.movement_state.as_mut().unwrap();
        let dense_index = state.dense_index_by_character("character-a").unwrap();
        state.apply_command_at(
            dense_index,
            27,
            MovementCommand::MoveDir(Vec2 { x: 0.0, y: 1.0 }),
        );
        state.set_position_at(
            dense_index,
            Vec2 {
                x: 3999.5,
                y: 2002.0,
            },
        );

        logic.tick_count = 999;
        logic.current_frame = 30;
        logic.on_character_offline("main-world-public", "character-a");
        let recovery = logic
            .movement_recovery_state(
                Some("character-a"),
                MovementCorrectionReason::ReconnectRecovery,
            )
            .unwrap();

        assert_eq!(recovery.frame_id, 30);
        assert_eq!(
            recovery.correction_kind,
            MovementCorrectionKind::Recovery as i32
        );
        assert_eq!(
            recovery.reason_code,
            MovementCorrectionReason::ReconnectRecovery as i32
        );
        assert!(!recovery.aoi_enabled);
        assert_eq!(recovery.entities.len(), 1);
        let entity = &recovery.entities[0];
        assert_eq!(entity.scene_id, 1);
        assert_eq!(entity.x, 3999.5);
        assert_eq!(entity.y, 2002.0);
        assert_eq!(entity.dir_x, 0.0);
        assert_eq!(entity.dir_y, 1.0);
        assert!(!entity.moving);
        assert_eq!(entity.last_input_frame, 30);

        let broadcasts = logic.take_pending_broadcasts();
        assert_eq!(broadcasts.len(), 1);
        let snapshot = crate::pb::MovementSnapshotPush::decode(broadcasts[0].body.as_slice())
            .expect("offline correction should decode");
        assert_eq!(
            snapshot.reason_code,
            MovementCorrectionReason::PlayerOffline as i32
        );
        assert!(!snapshot.entities[0].moving);
    }

    #[tokio::test]
    async fn csv_reload_updates_scene_catalog_without_resetting_existing_entities() {
        let fixture = TempConfigDir::new();
        let runtime = ConfigTableRuntime::load_with_scene_dir(&fixture.csv_dir, &fixture.scene_dir)
            .expect("initial config should load");
        let mut logic = MovementDemoLogic::new(
            runtime.clone(),
            1,
            MOVEMENT_DEMO_CORRECTION_INTERVAL_FRAMES,
            MOVEMENT_DEMO_CORRECTION_THRESHOLD_METERS,
            16.0,
            MOVEMENT_DEMO_AOI_ENABLED,
            MOVEMENT_DEMO_CONTROL_STOP_FRAMES,
        );
        logic.on_room_created("main-world-public");
        logic.on_character_join("character-a");
        let before = logic
            .movement_state
            .as_ref()
            .unwrap()
            .entity("character-a")
            .unwrap();

        let spawn_path = fixture.csv_dir.join("SceneSpawnPoint.csv");
        let updated = fs::read_to_string(&spawn_path).unwrap().replace(
            "1001,1,grassland_player_main,player,2002.0,2002.0,1.0,0.0,2.0,default|safe",
            "1001,1,grassland_player_main,player,2003.5,2002.0,1.0,0.0,2.0,default|safe",
        );
        fs::write(&spawn_path, updated).unwrap();
        runtime
            .reload_changed(std::slice::from_ref(&spawn_path))
            .await
            .expect("scene spawn reload should succeed");

        let existing = logic
            .movement_state
            .as_ref()
            .unwrap()
            .entity("character-a")
            .unwrap();
        assert_eq!(existing.entity_id, before.entity_id);
        assert_eq!(existing.position.x, before.position.x);
        assert_eq!(existing.position.y, before.position.y);
        assert_eq!(existing.last_input_frame, before.last_input_frame);

        logic.on_character_join("character-b");
        let spawned_after_reload = logic
            .movement_state
            .as_ref()
            .unwrap()
            .entity("character-b")
            .unwrap();
        assert_eq!(spawned_after_reload.position.x, 2003.5);
        assert_eq!(spawned_after_reload.position.y, 2002.0);
    }
}
