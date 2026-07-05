use std::collections::HashMap;

use serde::Serialize;
use sim_core::{EntityId, SimEntity, SimHash, SimWorld};
use tracing::{info, warn};

use crate::core::logic::{RoomLogic, RoomLogicTransfer};
use crate::core::room::PlayerInputRecord;
use crate::core::system::lockstep_sim::{
    BoundSimConfig, DEFAULT_LOCKSTEP_SIM_TICK_RATE, LOCKSTEP_SIM_DEMO_CONFIG_MIGRATION_BOUNDARY,
    LOCKSTEP_SIM_DEMO_CONFIG_SOURCE, SimFrameEnvelope, SimFrameEventSummary, SimInitialSnapshot,
    TRAINING_TARGET_ENTITY_ID, create_frame_envelope_with_config,
    create_initial_snapshot_with_config, create_minimal_world, restore_initial_snapshot,
    room_sim_config, sim_hash_envelope, step_world_with_config, validate_player_input, world_hash,
};

pub const LOCKSTEP_SIM_DEMO_POLICY_ID: &str = "lockstep_sim_demo";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LockstepSimEntityDebugState {
    entity_id: u32,
    x: i64,
    y: i64,
    hp: i32,
    max_hp: i32,
    alive: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LockstepSimPlayerDebugState<'a> {
    character_id: &'a str,
    #[serde(flatten)]
    entity: LockstepSimEntityDebugState,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LockstepSimDemoState<'a> {
    logic_type: &'static str,
    room_id: &'a str,
    tick_count: u64,
    roster: &'a [String],
    world_frame: u32,
    tick_rate: u16,
    config_version: u64,
    config_hash: &'a str,
    sim_schema_version: u16,
    config_source: &'static str,
    config_migration_boundary: &'static str,
    entity_count: usize,
    binding_count: usize,
    training_target_entity_id: u32,
    player_entities: Vec<LockstepSimPlayerDebugState<'a>>,
    training_target: Option<LockstepSimEntityDebugState>,
    initial_snapshot: Option<SimInitialSnapshot>,
    last_frame: Option<SimFrameEnvelope>,
    observer_frame: Option<LockstepSimObserverFrame>,
    last_hash: Option<u64>,
    last_hash_hex: Option<String>,
    last_state_hash: Option<crate::core::system::lockstep_sim::SimHashEnvelope>,
    last_event_count: usize,
    last_event_summaries: Vec<SimFrameEventSummary>,
    last_error: Option<&'a str>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct LockstepSimObserverFrame {
    world_frame: u32,
    state_hash: crate::core::system::lockstep_sim::SimHashEnvelope,
    last_event_count: usize,
    last_event_summaries: Vec<SimFrameEventSummary>,
    last_frame: Option<SimFrameEnvelope>,
}

pub struct LockstepSimDemoLogic {
    room_id: String,
    tick_count: u64,
    roster: Vec<String>,
    world: Option<SimWorld>,
    bindings: HashMap<String, EntityId>,
    tick_rate: u16,
    config: BoundSimConfig,
    last_hash: Option<SimHash>,
    last_frame: Option<SimFrameEnvelope>,
    last_event_count: usize,
    last_error: Option<String>,
}

impl LockstepSimDemoLogic {
    fn ensure_character_in_roster(&mut self, character_id: &str) {
        if !self.roster.iter().any(|existing| existing == character_id) {
            self.roster.push(character_id.to_string());
        }
    }

    fn rebuild_world(&mut self) {
        let (world, bindings) = create_minimal_world(&self.roster);
        self.last_hash = Some(world_hash(&world));
        self.last_frame = None;
        self.last_event_count = 0;
        self.world = Some(world);
        self.bindings = bindings;
        self.tick_rate = self.config.tick_rate();
        self.last_error = None;
    }

    fn serialized_state(&self) -> LockstepSimDemoState<'_> {
        let world_frame = self
            .world
            .as_ref()
            .map(|world| world.frame.raw())
            .unwrap_or(0);
        let entity_count = self
            .world
            .as_ref()
            .map(|world| world.entities_sorted_by_id().len())
            .unwrap_or(0);
        let player_entities = self
            .world
            .as_ref()
            .map(|world| {
                self.roster
                    .iter()
                    .filter_map(|character_id| {
                        let entity_id = self.bindings.get(character_id)?;
                        world
                            .entity(*entity_id)
                            .map(|entity| LockstepSimPlayerDebugState {
                                character_id,
                                entity: entity_debug_state(entity),
                            })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let training_target = self.world.as_ref().and_then(|world| {
            world
                .entity(EntityId::new(TRAINING_TARGET_ENTITY_ID))
                .map(entity_debug_state)
        });
        let initial_snapshot = self.world.as_ref().map(|world| {
            create_initial_snapshot_with_config(&self.room_id, &self.config, world, &self.bindings)
        });
        let observer_frame = self.world.as_ref().map(|world| LockstepSimObserverFrame {
            world_frame,
            state_hash: sim_hash_envelope(world_hash(world)),
            last_event_count: self.last_event_count,
            last_event_summaries: self
                .last_frame
                .as_ref()
                .map(|frame| frame.event_summaries.clone())
                .unwrap_or_default(),
            last_frame: self.last_frame.clone(),
        });
        let last_state_hash = self.last_hash.map(sim_hash_envelope);
        let last_event_summaries = self
            .last_frame
            .as_ref()
            .map(|frame| frame.event_summaries.clone())
            .unwrap_or_default();

        LockstepSimDemoState {
            logic_type: LOCKSTEP_SIM_DEMO_POLICY_ID,
            room_id: &self.room_id,
            tick_count: self.tick_count,
            roster: &self.roster,
            world_frame,
            tick_rate: self.tick_rate,
            config_version: self.config.config_version,
            config_hash: &self.config.config_hash,
            sim_schema_version: self.config.sim_schema_version,
            config_source: LOCKSTEP_SIM_DEMO_CONFIG_SOURCE,
            config_migration_boundary: LOCKSTEP_SIM_DEMO_CONFIG_MIGRATION_BOUNDARY,
            entity_count,
            binding_count: self.bindings.len(),
            training_target_entity_id: TRAINING_TARGET_ENTITY_ID,
            player_entities,
            training_target,
            initial_snapshot,
            last_frame: self.last_frame.clone(),
            observer_frame,
            last_hash: self.last_hash.map(|hash| hash.value),
            last_hash_hex: self.last_hash.map(|hash| sim_hash_envelope(hash).hex),
            last_state_hash,
            last_event_count: self.last_event_count,
            last_event_summaries,
            last_error: self.last_error.as_deref(),
        }
    }
}

impl Default for LockstepSimDemoLogic {
    fn default() -> Self {
        Self::new(1)
    }
}

impl LockstepSimDemoLogic {
    pub fn new(config_version: u64) -> Self {
        let config = room_sim_config(config_version.max(1), DEFAULT_LOCKSTEP_SIM_TICK_RATE);
        Self {
            room_id: String::new(),
            tick_count: 0,
            roster: Vec::new(),
            world: None,
            bindings: HashMap::new(),
            tick_rate: config.tick_rate(),
            config,
            last_hash: None,
            last_frame: None,
            last_event_count: 0,
            last_error: None,
        }
    }
}

impl RoomLogic for LockstepSimDemoLogic {
    fn on_room_created(&mut self, room_id: &str) {
        self.room_id = room_id.to_string();
        info!(room_id, "[RoomLogic/lockstep_sim_demo] room created");
    }

    fn on_character_join(&mut self, character_id: &str) {
        self.ensure_character_in_roster(character_id);
    }

    fn on_character_leave(&mut self, character_id: &str) {
        self.roster.retain(|existing| existing != character_id);
        self.bindings.remove(character_id);
    }

    fn on_game_started(&mut self, _room_id: &str) {
        self.rebuild_world();
    }

    fn on_game_ended(&mut self, _room_id: &str) {
        self.world = None;
        self.bindings.clear();
        self.last_hash = None;
        self.last_frame = None;
        self.last_event_count = 0;
        self.last_error = None;
    }

    fn validate_character_input(
        &self,
        _character_id: &str,
        action: &str,
        payload_json: &str,
    ) -> Result<(), &'static str> {
        validate_player_input(action, payload_json)
    }

    fn on_tick(&mut self, frame_id: u32, fps: u16, inputs: &[PlayerInputRecord]) {
        self.tick_count = self.tick_count.saturating_add(1);
        if self.world.is_none() {
            self.rebuild_world();
        }
        if self.tick_rate != fps.max(1) {
            self.tick_rate = self.config.tick_rate();
        }

        let Some(world) = self.world.as_mut() else {
            return;
        };

        match step_world_with_config(world, frame_id, &self.config, inputs, &self.bindings) {
            Ok(result) => {
                self.last_hash = Some(result.state_hash);
                self.last_event_count = result.events.len();
                self.last_frame = Some(create_frame_envelope_with_config(
                    &self.room_id,
                    &self.config,
                    world,
                    inputs,
                    &result,
                ));
                self.last_error = None;
            }
            Err(error) => {
                let message = error.to_string();
                warn!(
                    room_id = self.room_id,
                    frame_id,
                    error = %message,
                    "lockstep sim demo step failed"
                );
                self.last_error = Some(message);
            }
        }
    }

    fn get_serialized_state(&self) -> String {
        serde_json::to_string(&self.serialized_state()).unwrap_or_default()
    }

    fn restore_from_serialized_state(&mut self, state: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(state) else {
            self.last_error = Some("INVALID_LOCKSTEP_SIM_DEMO_STATE_JSON".to_string());
            return;
        };
        let Some(snapshot_value) = value.get("initialSnapshot") else {
            self.last_error = Some("LOCKSTEP_SIM_DEMO_STATE_MISSING_SNAPSHOT".to_string());
            return;
        };
        let Ok(snapshot) = serde_json::from_value::<SimInitialSnapshot>(snapshot_value.clone())
        else {
            self.last_error = Some("INVALID_LOCKSTEP_SIM_INITIAL_SNAPSHOT_JSON".to_string());
            return;
        };

        match restore_initial_snapshot(&snapshot) {
            Ok((world, bindings)) => {
                self.room_id = snapshot.room_id;
                self.tick_count = value
                    .get("tickCount")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(self.tick_count);
                self.roster = snapshot
                    .control_bindings
                    .iter()
                    .map(|binding| binding.character_id.clone())
                    .collect();
                self.world = Some(world);
                self.bindings = bindings;
                self.tick_rate = snapshot.tick_rate;
                self.config = room_sim_config(snapshot.config_version, snapshot.tick_rate);
                self.last_hash = Some(SimHash {
                    frame: sim_core::FrameId::new(snapshot.state_hash.frame),
                    value: snapshot.state_hash.value,
                });
                self.last_frame = value
                    .get("lastFrame")
                    .cloned()
                    .filter(|value| !value.is_null())
                    .and_then(|value| serde_json::from_value::<SimFrameEnvelope>(value).ok());
                self.last_event_count = self
                    .last_frame
                    .as_ref()
                    .map(frame_event_count)
                    .unwrap_or(0);
                self.last_error = None;
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
            }
        }
    }
}

impl RoomLogicTransfer for LockstepSimDemoLogic {}

fn entity_debug_state(entity: &SimEntity) -> LockstepSimEntityDebugState {
    LockstepSimEntityDebugState {
        entity_id: entity.id.raw(),
        x: entity.transform.pos.x.raw(),
        y: entity.transform.pos.y.raw(),
        hp: entity.combat.hp,
        max_hp: entity.combat.max_hp,
        alive: entity.alive,
    }
}

fn frame_event_count(frame: &SimFrameEnvelope) -> usize {
    frame
        .event_count
        .max(frame.events.len())
        .max(frame.event_summaries.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config_table::ConfigTableRuntime;
    use crate::core::logic::RoomLogicFactory;
    use crate::core::system::lockstep_sim::{
        DEFAULT_PLAYER_SKILL_ID, SIM_INPUT_ACTION, SIM_INPUT_VERSION,
    };
    use crate::gameroom::GameRoomLogicFactory;
    use crate::gameroom::robot_sync_room::ROBOT_MOVE_ACTION;
    use std::path::Path;
    use std::time::Instant;

    fn config_tables() -> ConfigTableRuntime {
        ConfigTableRuntime::load_with_scene_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("csv"),
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("scene"),
        )
        .expect("game-server csv fixture should load")
    }

    fn input(frame_id: u32, character_id: &str, payload_json: String) -> PlayerInputRecord {
        PlayerInputRecord {
            frame_id,
            character_id: character_id.to_string(),
            action: SIM_INPUT_ACTION.to_string(),
            payload_json,
            received_at: Instant::now(),
            is_synthetic: false,
        }
    }

    fn move_right_payload(seq: u32) -> String {
        serde_json::json!({
            "version": SIM_INPUT_VERSION,
            "seq": seq,
            "commands": [
                { "type": "move", "dirX": 1000, "dirY": 0 }
            ]
        })
        .to_string()
    }

    fn stop_payload(seq: u32) -> String {
        serde_json::json!({
            "version": SIM_INPUT_VERSION,
            "seq": seq,
            "commands": [
                { "type": "stop" }
            ]
        })
        .to_string()
    }

    fn unknown_skill_payload(seq: u32) -> String {
        serde_json::json!({
            "version": SIM_INPUT_VERSION,
            "seq": seq,
            "commands": [
                { "type": "castSkill", "skillId": 999, "targetEntityId": TRAINING_TARGET_ENTITY_ID }
            ]
        })
        .to_string()
    }

    fn cast_training_target_payload(seq: u32) -> String {
        serde_json::json!({
            "version": SIM_INPUT_VERSION,
            "seq": seq,
            "commands": [
                {
                    "type": "castSkill",
                    "skillId": DEFAULT_PLAYER_SKILL_ID,
                    "targetEntityId": TRAINING_TARGET_ENTITY_ID
                }
            ]
        })
        .to_string()
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

    fn synthetic_repeat_last_input(
        frame_id: u32,
        character_id: &str,
        payload_json: String,
    ) -> PlayerInputRecord {
        PlayerInputRecord {
            frame_id,
            character_id: character_id.to_string(),
            action: SIM_INPUT_ACTION.to_string(),
            payload_json,
            received_at: Instant::now(),
            is_synthetic: true,
        }
    }

    #[test]
    fn lockstep_sim_demo_starts_and_advances_one_frame() {
        let mut logic = LockstepSimDemoLogic::default();
        logic.on_room_created("room-lockstep");
        logic.on_character_join("player-a");
        logic.on_game_started("room-lockstep");

        let started =
            serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state()).unwrap();
        assert_eq!(started["logicType"], LOCKSTEP_SIM_DEMO_POLICY_ID);
        assert_eq!(started["worldFrame"], 0);
        assert_eq!(started["entityCount"], 2);
        assert_eq!(started["bindingCount"], 1);
        assert!(started["lastHash"].as_u64().is_some());
        assert_eq!(started["observerFrame"]["worldFrame"], 0);
        assert_eq!(started["configVersion"], 1);
        assert_eq!(
            started["simSchemaVersion"],
            sim_core::SIM_CORE_SCHEMA_VERSION
        );
        assert_eq!(started["configSource"], "lockstep_sim_demo.fixed_v1");
        assert_eq!(
            started["configHash"],
            started["initialSnapshot"]["configHash"]
        );
        assert_eq!(
            started["configVersion"],
            started["initialSnapshot"]["configVersion"]
        );
        assert_eq!(
            started["simSchemaVersion"],
            started["initialSnapshot"]["simSchemaVersion"]
        );
        assert_eq!(
            started["observerFrame"]["stateHash"],
            started["initialSnapshot"]["stateHash"]
        );
        assert_eq!(started["observerFrame"]["lastEventCount"], 0);
        assert!(started["observerFrame"]["lastEventSummaries"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(started["observerFrame"]["lastFrame"].is_null());
        assert_eq!(started["tickRate"], DEFAULT_LOCKSTEP_SIM_TICK_RATE);
        assert_eq!(
            started["initialSnapshot"]["schema"],
            crate::core::system::lockstep_sim::SIM_INITIAL_SNAPSHOT_SCHEMA
        );
        assert_eq!(started["initialSnapshot"]["roomId"], "room-lockstep");
        assert_eq!(started["initialSnapshot"]["startFrame"], 0);
        assert_eq!(
            started["initialSnapshot"]["tickRate"],
            DEFAULT_LOCKSTEP_SIM_TICK_RATE
        );
        assert_eq!(started["initialSnapshot"]["rngSeed"], 0);
        assert_eq!(
            started["initialSnapshot"]["entities"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            started["initialSnapshot"]["controlBindings"][0]["characterId"],
            "player-a"
        );
        assert!(
            started["initialSnapshot"]["stateHash"]["hex"]
                .as_str()
                .is_some_and(|hex| hex.len() == 16)
        );
        assert_eq!(started["playerEntities"][0]["entityId"], 1000);
        assert_eq!(started["trainingTarget"]["hp"], 150);

        logic.on_tick(1, 20, &[]);

        let advanced =
            serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state()).unwrap();
        assert_eq!(advanced["worldFrame"], 1);
        assert_eq!(advanced["tickCount"], 1);
        assert_eq!(advanced["entityCount"], 2);
        assert!(advanced["lastHash"].as_u64().is_some());
        assert_eq!(advanced["initialSnapshot"]["startFrame"], 1);
        assert_eq!(advanced["lastFrame"]["frame"], 1);
        assert_eq!(advanced["lastFrame"]["configHash"], advanced["configHash"]);
        assert_eq!(
            advanced["lastFrame"]["configVersion"],
            advanced["configVersion"]
        );
        assert_eq!(
            advanced["lastFrame"]["simSchemaVersion"],
            advanced["simSchemaVersion"]
        );
        assert_eq!(advanced["observerFrame"]["worldFrame"], 1);
        assert_eq!(
            advanced["observerFrame"]["stateHash"],
            advanced["lastFrame"]["stateHash"]
        );
        assert_eq!(advanced["observerFrame"]["lastFrame"]["frame"], 1);
        assert!(
            advanced["lastFrame"]["stateHash"]["hex"]
                .as_str()
                .is_some_and(|hex| hex.len() == 16)
        );
        assert_eq!(advanced["lastStateHash"], advanced["lastFrame"]["stateHash"]);
        assert_eq!(advanced["lastFrame"]["eventCount"], 0);
        assert!(advanced["lastFrame"]["eventSummaries"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(advanced["lastEventSummaries"].as_array().unwrap().is_empty());
        assert_eq!(advanced["observerFrame"]["lastEventCount"], 0);
        assert!(advanced["observerFrame"]["lastEventSummaries"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(advanced["lastFrame"]["debugSummary"]["inputCount"], 0);
        assert_eq!(advanced["lastFrame"]["debugSummary"]["eventCount"], 0);
        assert_eq!(advanced["lastFrame"]["debugState"]["schemaVersion"], 1);
        assert_eq!(
            advanced["lastFrame"]["debugState"]["entities"][0]["entityId"],
            1000
        );
        assert_eq!(
            advanced["lastFrame"]["debugState"]["entities"][0]["xRaw"],
            0
        );
        assert_eq!(
            advanced["lastFrame"]["debugState"]["entities"][0]["hp"],
            100
        );
        assert_eq!(
            advanced["lastFrame"]["debugState"]["entities"][0]["alive"],
            true
        );
        assert!(advanced["lastError"].is_null());
    }

    #[test]
    fn lockstep_sim_demo_accepts_sim_input_and_advances_movement_and_combat() {
        let mut logic = LockstepSimDemoLogic::default();
        logic.on_room_created("room-lockstep");
        logic.on_character_join("player-a");
        logic.on_game_started("room-lockstep");

        let move_payload = move_right_payload(1);
        assert_eq!(
            logic.validate_character_input("player-a", SIM_INPUT_ACTION, &move_payload),
            Ok(())
        );
        logic.on_tick(1, 20, &[input(1, "player-a", move_payload)]);

        let moved = serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state())
            .expect("state should be valid json");
        assert_eq!(moved["worldFrame"], 1);
        assert_eq!(moved["playerEntities"][0]["x"], 300);
        assert_eq!(moved["playerEntities"][0]["y"], 0);
        assert_eq!(moved["trainingTarget"]["hp"], 150);

        let cast_payload = cast_training_target_payload(2);
        assert_eq!(
            logic.validate_character_input("player-a", SIM_INPUT_ACTION, &cast_payload),
            Ok(())
        );
        logic.on_tick(2, 20, &[input(2, "player-a", cast_payload)]);

        let attacked = serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state())
            .expect("state should be valid json");
        assert_eq!(attacked["worldFrame"], 2);
        assert_eq!(attacked["trainingTarget"]["hp"], 136);
        assert_eq!(attacked["lastEventCount"], 2);
        assert_eq!(attacked["lastFrame"]["frame"], 2);
        assert_eq!(attacked["lastStateHash"], attacked["lastFrame"]["stateHash"]);
        assert_eq!(attacked["lastFrame"]["eventCount"], 2);
        assert_eq!(attacked["lastFrame"]["events"].as_array().unwrap().len(), 2);
        assert_eq!(
            attacked["lastFrame"]["eventSummaries"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(attacked["lastFrame"]["eventSummaries"][0]["kind"], "skillCast");
        assert_eq!(attacked["lastFrame"]["eventSummaries"][0]["schemaVersion"], 1);
        assert_eq!(
            attacked["lastFrame"]["eventSummaries"][0]["sourceEntityId"],
            1000
        );
        assert_eq!(
            attacked["lastFrame"]["eventSummaries"][0]["targetEntityId"],
            TRAINING_TARGET_ENTITY_ID
        );
        assert_eq!(
            attacked["lastFrame"]["eventSummaries"][0]["skillId"],
            DEFAULT_PLAYER_SKILL_ID
        );
        assert_eq!(attacked["lastFrame"]["eventSummaries"][0]["amount"], 1);
        assert_eq!(attacked["lastFrame"]["eventSummaries"][1]["kind"], "damage");
        assert_eq!(
            attacked["lastFrame"]["eventSummaries"][1]["targetEntityId"],
            TRAINING_TARGET_ENTITY_ID
        );
        assert_eq!(attacked["lastFrame"]["eventSummaries"][1]["amount"], 14);
        assert_eq!(
            attacked["lastEventSummaries"],
            attacked["lastFrame"]["eventSummaries"]
        );
        assert_eq!(attacked["observerFrame"]["lastEventCount"], 2);
        assert_eq!(
            attacked["observerFrame"]["lastEventSummaries"],
            attacked["lastFrame"]["eventSummaries"]
        );
        assert_eq!(
            attacked["observerFrame"]["lastFrame"]["events"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            attacked["observerFrame"]["lastFrame"]["eventSummaries"],
            attacked["lastFrame"]["eventSummaries"]
        );
        assert_eq!(attacked["lastFrame"]["debugSummary"]["eventCount"], 2);
        assert_eq!(
            attacked["lastFrame"]["debugState"]["entities"][1]["entityId"],
            TRAINING_TARGET_ENTITY_ID
        );
        assert_eq!(attacked["lastFrame"]["debugState"]["entities"][1]["hp"], 136);
        assert!(
            attacked["lastFrame"]["debugState"]["entities"][1]["alive"]
                .as_bool()
                .unwrap()
        );
        assert!(attacked["lastError"].is_null());
    }

    #[test]
    fn lockstep_sim_demo_accepts_stop_and_saves_authoritative_frame_result() {
        let mut logic = LockstepSimDemoLogic::default();
        logic.on_room_created("room-lockstep");
        logic.on_character_join("player-a");
        logic.on_game_started("room-lockstep");

        logic.on_tick(1, 20, &[input(1, "player-a", move_right_payload(1))]);
        logic.on_tick(2, 20, &[]);
        let moving = serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state())
            .expect("state should be valid json");
        assert_eq!(moving["worldFrame"], 2);
        assert_eq!(moving["playerEntities"][0]["x"], 600);

        logic.on_tick(3, 20, &[input(3, "player-a", stop_payload(3))]);

        let stopped = serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state())
            .expect("state should be valid json");
        assert_eq!(stopped["worldFrame"], 3);
        assert_eq!(stopped["playerEntities"][0]["x"], 600);
        assert_eq!(stopped["lastFrame"]["frame"], 3);
        assert_eq!(stopped["lastFrame"]["events"].as_array().unwrap().len(), 0);
        assert_eq!(stopped["lastFrame"]["eventCount"], 0);
        assert!(stopped["lastFrame"]["eventSummaries"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(stopped["lastEventCount"], 0);
        assert_eq!(
            stopped["lastFrame"]["stateHash"],
            stopped["observerFrame"]["stateHash"]
        );
        assert_eq!(
            stopped["lastHashHex"],
            stopped["lastFrame"]["stateHash"]["hex"]
        );
        assert_eq!(stopped["lastFrame"]["debugSummary"]["realInputCount"], 1);
        assert!(stopped["lastError"].is_null());
    }

    #[test]
    fn lockstep_sim_demo_restores_snapshot_and_continues_with_same_hash() {
        let mut continuous = LockstepSimDemoLogic::default();
        continuous.on_room_created("room-lockstep");
        continuous.on_character_join("player-a");
        continuous.on_game_started("room-lockstep");

        let move_payload = move_right_payload(1);
        continuous.on_tick(1, 20, &[input(1, "player-a", move_payload)]);
        let snapshot_json = continuous.get_serialized_state();
        let snapshot_json_value =
            serde_json::from_str::<serde_json::Value>(&snapshot_json).unwrap();

        let mut restored = LockstepSimDemoLogic::default();
        restored.restore_from_serialized_state(&snapshot_json);
        let restored_state =
            serde_json::from_str::<serde_json::Value>(&restored.get_serialized_state()).unwrap();
        assert_eq!(restored_state["roomId"], "room-lockstep");
        assert_eq!(restored_state["worldFrame"], 1);
        assert_eq!(restored_state["tickRate"], 20);
        assert_eq!(
            restored_state["configHash"],
            snapshot_json_value["configHash"]
        );
        assert!(restored_state["lastError"].is_null());
        assert_eq!(
            restored_state["initialSnapshot"]["stateHash"],
            snapshot_json_value["initialSnapshot"]["stateHash"]
        );

        let cast_payload = cast_training_target_payload(2);
        continuous.on_tick(2, 20, &[input(2, "player-a", cast_payload.clone())]);
        restored.on_tick(2, 20, &[input(2, "player-a", cast_payload)]);

        let continuous_state =
            serde_json::from_str::<serde_json::Value>(&continuous.get_serialized_state()).unwrap();
        let restored_state =
            serde_json::from_str::<serde_json::Value>(&restored.get_serialized_state()).unwrap();

        assert_eq!(restored_state["worldFrame"], 2);
        assert_eq!(restored_state["trainingTarget"]["hp"], 136);
        assert_eq!(restored_state["lastHash"], continuous_state["lastHash"]);
        assert_eq!(
            restored_state["lastHashHex"],
            continuous_state["lastHashHex"]
        );
        assert_eq!(
            restored_state["lastFrame"]["stateHash"],
            continuous_state["lastFrame"]["stateHash"]
        );
        assert_eq!(
            restored_state["observerFrame"]["stateHash"],
            continuous_state["observerFrame"]["stateHash"]
        );
    }

    #[test]
    fn lockstep_sim_demo_serialized_state_initial_snapshot_is_recoverable() {
        let mut continuous = LockstepSimDemoLogic::default();
        continuous.on_room_created("room-lockstep");
        continuous.on_character_join("player-a");
        continuous.on_game_started("room-lockstep");
        continuous.on_tick(1, 20, &[input(1, "player-a", move_right_payload(1))]);

        let serialized = continuous.get_serialized_state();
        let state = serde_json::from_str::<serde_json::Value>(&serialized).unwrap();
        let initial_snapshot = &state["initialSnapshot"];

        for field in [
            "schema",
            "schemaVersion",
            "roomId",
            "startFrame",
            "tickRate",
            "configHash",
            "configVersion",
            "simSchemaVersion",
            "rngSeed",
            "entities",
            "controlBindings",
            "stateHash",
            "snapshot",
        ] {
            assert!(
                initial_snapshot.get(field).is_some(),
                "missing initialSnapshot field {field}"
            );
        }
        assert_eq!(state["worldFrame"], initial_snapshot["startFrame"]);
        assert_eq!(state["roomId"], initial_snapshot["roomId"]);
        assert_eq!(state["tickRate"], initial_snapshot["tickRate"]);
        assert_eq!(state["configHash"], initial_snapshot["configHash"]);
        assert_eq!(state["configVersion"], initial_snapshot["configVersion"]);
        assert_eq!(
            state["simSchemaVersion"],
            initial_snapshot["simSchemaVersion"]
        );
        assert_eq!(state["lastStateHash"], initial_snapshot["stateHash"]);
        assert_eq!(
            state["observerFrame"]["stateHash"],
            initial_snapshot["stateHash"]
        );
        assert_eq!(state["lastFrame"]["frame"], 1);

        let snapshot =
            serde_json::from_value::<SimInitialSnapshot>(initial_snapshot.clone()).unwrap();
        let (restored_world, restored_bindings) = restore_initial_snapshot(&snapshot).unwrap();
        assert_eq!(restored_world.frame.raw(), snapshot.start_frame);
        assert_eq!(restored_bindings.len(), 1);
        assert_eq!(
            restored_bindings["player-a"],
            EntityId::new(crate::core::system::lockstep_sim::PLAYER_ENTITY_ID_BASE)
        );

        let mut restored = LockstepSimDemoLogic::default();
        restored.restore_from_serialized_state(&serialized);
        let restored_state =
            serde_json::from_str::<serde_json::Value>(&restored.get_serialized_state()).unwrap();
        assert_eq!(
            restored_state["initialSnapshot"]["stateHash"],
            initial_snapshot["stateHash"]
        );
        assert!(restored_state["lastError"].is_null());

        let next_frame = initial_snapshot["startFrame"].as_u64().unwrap() as u32 + 1;
        let next_payload = cast_training_target_payload(next_frame);
        continuous.on_tick(
            next_frame,
            20,
            &[input(next_frame, "player-a", next_payload.clone())],
        );
        restored.on_tick(
            next_frame,
            20,
            &[input(next_frame, "player-a", next_payload)],
        );

        let continuous_state =
            serde_json::from_str::<serde_json::Value>(&continuous.get_serialized_state()).unwrap();
        let restored_state =
            serde_json::from_str::<serde_json::Value>(&restored.get_serialized_state()).unwrap();
        assert_eq!(
            restored_state["lastFrame"]["frame"],
            initial_snapshot["startFrame"].as_u64().unwrap() + 1
        );
        assert_eq!(
            restored_state["lastFrame"]["stateHash"],
            continuous_state["lastFrame"]["stateHash"]
        );
        assert_eq!(
            restored_state["observerFrame"]["stateHash"],
            continuous_state["observerFrame"]["stateHash"]
        );
    }

    #[test]
    fn lockstep_sim_demo_binds_config_metadata_for_room_lifetime() {
        let mut logic = LockstepSimDemoLogic::new(2);
        logic.on_room_created("room-lockstep");
        logic.on_character_join("player-a");
        logic.on_game_started("room-lockstep");

        let started =
            serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state()).unwrap();
        let config_hash = started["configHash"].clone();
        assert_eq!(started["configVersion"], 2);
        assert_eq!(started["initialSnapshot"]["configVersion"], 2);
        assert_eq!(started["initialSnapshot"]["configHash"], config_hash);

        logic.on_tick(1, 30, &[]);

        let advanced =
            serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state()).unwrap();
        assert_eq!(advanced["tickRate"], DEFAULT_LOCKSTEP_SIM_TICK_RATE);
        assert_eq!(advanced["configVersion"], 2);
        assert_eq!(advanced["configHash"], config_hash);
        assert_eq!(
            advanced["lastFrame"]["tickRate"],
            DEFAULT_LOCKSTEP_SIM_TICK_RATE
        );
        assert_eq!(advanced["lastFrame"]["configVersion"], 2);
        assert_eq!(advanced["lastFrame"]["configHash"], config_hash);
    }

    #[test]
    fn lockstep_sim_demo_empty_missing_input_continues_movement_without_recasting() {
        let mut logic = LockstepSimDemoLogic::default();
        logic.on_room_created("room-lockstep");
        logic.on_character_join("player-a");
        logic.on_game_started("room-lockstep");

        logic.on_tick(1, 20, &[input(1, "player-a", move_right_payload(1))]);
        logic.on_tick(
            2,
            20,
            &[input(2, "player-a", cast_training_target_payload(2))],
        );
        logic.on_tick(
            3,
            20,
            &[PlayerInputRecord {
                frame_id: 3,
                character_id: "player-a".to_string(),
                action: String::new(),
                payload_json: String::new(),
                received_at: Instant::now(),
                is_synthetic: true,
            }],
        );

        let state = serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state())
            .expect("state should be valid json");
        assert_eq!(state["worldFrame"], 3);
        assert_eq!(state["trainingTarget"]["hp"], 136);
        assert_eq!(state["lastEventCount"], 0);
        assert_eq!(state["playerEntities"][0]["x"], 900);
        assert_eq!(state["lastFrame"]["debugSummary"]["inputCount"], 1);
        assert_eq!(state["lastFrame"]["debugSummary"]["realInputCount"], 0);
        assert_eq!(state["lastFrame"]["debugSummary"]["syntheticInputCount"], 1);
        assert_eq!(
            state["lastFrame"]["debugSummary"]["synthesizedEmptyInputCount"],
            1
        );
        assert_eq!(
            state["lastFrame"]["debugSummary"]["synthesizedRepeatLastInputCount"],
            0
        );
        assert_eq!(
            state["lastFrame"]["inputSources"][0]["source"],
            "synthesizedEmpty"
        );
        assert_eq!(
            state["observerFrame"]["stateHash"],
            state["lastFrame"]["stateHash"]
        );
    }

    #[test]
    fn lockstep_sim_demo_distinguishes_real_empty_and_repeat_last_sources() {
        let mut logic = LockstepSimDemoLogic::default();
        logic.on_room_created("room-lockstep");
        logic.on_character_join("player-a");
        logic.on_character_join("player-b");
        logic.on_game_started("room-lockstep");

        logic.on_tick(
            1,
            20,
            &[
                input(1, "player-a", move_right_payload(1)),
                synthetic_empty_input(1, "player-b"),
                synthetic_repeat_last_input(1, "player-c", move_right_payload(1)),
            ],
        );

        let state = serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state())
            .expect("state should be valid json");
        assert_eq!(state["worldFrame"], 1);
        assert_eq!(
            state["lastFrame"]["inputSources"].as_array().unwrap().len(),
            3
        );
        assert_eq!(state["lastFrame"]["inputSources"][0]["source"], "real");
        assert_eq!(
            state["lastFrame"]["inputSources"][1]["source"],
            "synthesizedEmpty"
        );
        assert_eq!(
            state["lastFrame"]["inputSources"][2]["source"],
            "synthesizedRepeatLast"
        );
        assert_eq!(state["lastFrame"]["debugSummary"]["realInputCount"], 1);
        assert_eq!(state["lastFrame"]["debugSummary"]["syntheticInputCount"], 2);
        assert_eq!(
            state["lastFrame"]["debugSummary"]["synthesizedEmptyInputCount"],
            1
        );
        assert_eq!(
            state["lastFrame"]["debugSummary"]["synthesizedRepeatLastInputCount"],
            1
        );
        assert!(state["lastError"].is_null());
    }

    #[test]
    fn lockstep_sim_demo_step_error_keeps_previous_world_hash_and_last_frame() {
        let mut logic = LockstepSimDemoLogic::default();
        logic.on_room_created("room-lockstep");
        logic.on_character_join("player-a");
        logic.on_game_started("room-lockstep");

        logic.on_tick(1, 20, &[input(1, "player-a", move_right_payload(1))]);
        let before = serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state())
            .expect("state should be valid json");

        logic.on_tick(2, 20, &[input(2, "player-a", unknown_skill_payload(2))]);

        let after = serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state())
            .expect("state should be valid json");
        assert_eq!(after["tickCount"], 2);
        assert_eq!(after["worldFrame"], before["worldFrame"]);
        assert_eq!(after["playerEntities"], before["playerEntities"]);
        assert_eq!(after["trainingTarget"], before["trainingTarget"]);
        assert_eq!(after["lastHash"], before["lastHash"]);
        assert_eq!(after["lastHashHex"], before["lastHashHex"]);
        assert_eq!(after["lastEventCount"], before["lastEventCount"]);
        assert_eq!(after["lastFrame"], before["lastFrame"]);
        assert_eq!(
            after["observerFrame"]["stateHash"],
            before["observerFrame"]["stateHash"]
        );
        assert!(
            after["lastError"]
                .as_str()
                .is_some_and(|error| error.contains("unknown skill 999"))
        );
    }

    #[test]
    fn lockstep_sim_demo_observer_read_only_state_does_not_change_hash() {
        let mut logic = LockstepSimDemoLogic::default();
        logic.on_room_created("room-lockstep");
        logic.on_character_join("player-a");
        logic.on_game_started("room-lockstep");
        logic.on_tick(
            1,
            20,
            &[input(1, "player-a", cast_training_target_payload(1))],
        );

        let before = serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state())
            .expect("state should be valid json");
        let observer_hash = before["observerFrame"]["stateHash"].clone();
        assert_eq!(
            before["observerFrame"]["lastFrame"]["events"],
            before["lastFrame"]["events"]
        );

        let after = serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state())
            .expect("state should be valid json");

        assert_eq!(after["lastHash"], before["lastHash"]);
        assert_eq!(after["observerFrame"]["stateHash"], observer_hash);
        assert_eq!(
            after["initialSnapshot"]["stateHash"],
            before["initialSnapshot"]["stateHash"]
        );
    }

    #[test]
    fn lockstep_sim_demo_rejects_invalid_sim_input_payloads() {
        let logic = LockstepSimDemoLogic::default();

        assert_eq!(
            logic.validate_character_input("player-a", "robot_move", "{}"),
            Err("INVALID_SIM_INPUT_ACTION")
        );
        assert_eq!(
            logic.validate_character_input(
                "player-a",
                SIM_INPUT_ACTION,
                r#"{"version":2,"seq":1,"commands":[]}"#
            ),
            Err("UNSUPPORTED_SIM_INPUT_VERSION")
        );
        assert_eq!(
            logic.validate_character_input(
                "player-a",
                SIM_INPUT_ACTION,
                r#"{"version":1,"seq":1,"commands":[{"type":"move","dirX":1000,"dirY":1000}]}"#
            ),
            Err("SIM_INPUT_DIR_OUT_OF_RANGE")
        );
        assert_eq!(
            logic.validate_character_input(
                "player-a",
                SIM_INPUT_ACTION,
                r#"{"version":1,"seq":1,"commands":[{"type":"move","dirX":0,"dirY":0}]}"#
            ),
            Err("SIM_INPUT_MOVE_DIR_ZERO")
        );
        assert_eq!(
            logic.validate_character_input(
                "player-a",
                SIM_INPUT_ACTION,
                r#"{"version":1,"seq":1,"commands":[{"type":"move","dirX":1000,"dirY":0,"speed":12001}]}"#
            ),
            Err("SIM_INPUT_SPEED_OUT_OF_RANGE")
        );
    }

    #[test]
    fn factory_creates_lockstep_sim_demo_without_replacing_old_demos() {
        let factory = GameRoomLogicFactory::new(config_tables());

        let mut logic = factory.create(LOCKSTEP_SIM_DEMO_POLICY_ID);
        logic.on_room_created("room-lockstep");
        logic.on_character_join("player-a");
        logic.on_game_started("room-lockstep");
        logic.on_tick(1, 20, &[]);
        let state =
            serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state()).unwrap();
        assert_eq!(state["logicType"], LOCKSTEP_SIM_DEMO_POLICY_ID);
        assert_eq!(state["worldFrame"], 1);
        assert_eq!(state["configVersion"], 1);

        let robot = factory.create("robot_sync_room");
        assert_eq!(
            robot.validate_character_input(
                "player-a",
                ROBOT_MOVE_ACTION,
                r#"{"version":1,"seq":1,"botTick":1,"dirX":0,"dirY":0,"speed":0}"#
            ),
            Ok(())
        );

        let movement_state = serde_json::from_str::<serde_json::Value>(
            &factory.create("movement_demo").get_serialized_state(),
        )
        .unwrap();
        assert!(movement_state.get("scene_id").is_some());

        let combat_state = serde_json::from_str::<serde_json::Value>(
            &factory.create("combat_demo").get_serialized_state(),
        )
        .unwrap();
        assert!(combat_state.get("snapshot").is_some());
    }
}
