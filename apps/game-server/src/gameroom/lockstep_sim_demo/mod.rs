use std::collections::HashMap;

use serde::Serialize;
use sim_core::{EntityId, SimEntity, SimHash, SimWorld};
use tracing::{info, warn};

use crate::core::logic::{RoomLogic, RoomLogicTransfer};
use crate::core::room::PlayerInputRecord;
use crate::core::system::lockstep_sim::{
    create_minimal_world, step_world, validate_player_input, world_hash, TRAINING_TARGET_ENTITY_ID,
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

#[derive(Default)]
pub struct LockstepSimDemoLogic {
    room_id: String,
    tick_count: u64,
    roster: Vec<String>,
    world: Option<SimWorld>,
    bindings: HashMap<String, EntityId>,
    last_hash: Option<SimHash>,
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
        self.last_event_count = 0;
        self.world = Some(world);
        self.bindings = bindings;
        self.last_error = None;
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

        let Some(world) = self.world.as_mut() else {
            return;
        };

        match step_world(world, frame_id, fps, inputs, &self.bindings) {
            Ok(result) => {
                self.last_hash = Some(result.state_hash);
                self.last_event_count = result.events.len();
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
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct PlayerState<'a> {
            character_id: &'a str,
            #[serde(flatten)]
            entity: LockstepSimEntityDebugState,
        }

        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct State<'a> {
            logic_type: &'static str,
            room_id: &'a str,
            tick_count: u64,
            roster: &'a [String],
            world_frame: u32,
            entity_count: usize,
            binding_count: usize,
            training_target_entity_id: u32,
            player_entities: Vec<PlayerState<'a>>,
            training_target: Option<LockstepSimEntityDebugState>,
            last_hash: Option<u64>,
            last_event_count: usize,
            last_error: Option<&'a str>,
        }

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
                        world.entity(*entity_id).map(|entity| PlayerState {
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

        serde_json::to_string(&State {
            logic_type: LOCKSTEP_SIM_DEMO_POLICY_ID,
            room_id: &self.room_id,
            tick_count: self.tick_count,
            roster: &self.roster,
            world_frame,
            entity_count,
            binding_count: self.bindings.len(),
            training_target_entity_id: TRAINING_TARGET_ENTITY_ID,
            player_entities,
            training_target,
            last_hash: self.last_hash.map(|hash| hash.value),
            last_event_count: self.last_event_count,
            last_error: self.last_error.as_deref(),
        })
        .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config_table::ConfigTableRuntime;
    use crate::core::logic::RoomLogicFactory;
    use crate::core::system::lockstep_sim::{
        DEFAULT_PLAYER_SKILL_ID, SIM_INPUT_ACTION, SIM_INPUT_VERSION,
    };
    use crate::gameroom::robot_sync_room::ROBOT_MOVE_ACTION;
    use crate::gameroom::GameRoomLogicFactory;
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
        assert_eq!(started["playerEntities"][0]["entityId"], 1000);
        assert_eq!(started["trainingTarget"]["hp"], 150);

        logic.on_tick(1, 20, &[]);

        let advanced =
            serde_json::from_str::<serde_json::Value>(&logic.get_serialized_state()).unwrap();
        assert_eq!(advanced["worldFrame"], 1);
        assert_eq!(advanced["tickCount"], 1);
        assert_eq!(advanced["entityCount"], 2);
        assert!(advanced["lastHash"].as_u64().is_some());
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
        assert!(attacked["lastError"].is_null());
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
