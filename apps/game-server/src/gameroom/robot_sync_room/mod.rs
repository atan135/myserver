use std::collections::VecDeque;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::core::logic::{RoomLogic, RoomLogicTransfer};
use crate::core::room::PlayerInputRecord;

pub const ROBOT_MOVE_ACTION: &str = "robot_move";

const ROBOT_MOVE_PAYLOAD_MAX_BYTES: usize = 256;
const RECENT_INPUT_LIMIT: usize = 16;

#[derive(Default)]
pub struct RobotSyncRoomLogic {
    room_id: String,
    tick_count: u64,
    last_frame: u32,
    recent_inputs: VecDeque<RobotMoveInputSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct RobotMoveInputSummary {
    frame_id: u32,
    player_id: String,
    seq: u32,
    bot_tick: u32,
    dir_x: i32,
    dir_y: i32,
    speed: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RobotMovePayload {
    seq: u32,
    bot_tick: u32,
    dir_x: i32,
    dir_y: i32,
    speed: u32,
}

impl RoomLogic for RobotSyncRoomLogic {
    fn on_room_created(&mut self, room_id: &str) {
        self.room_id = room_id.to_string();
    }

    fn validate_player_input(
        &self,
        _player_id: &str,
        action: &str,
        payload_json: &str,
    ) -> Result<(), &'static str> {
        if action != ROBOT_MOVE_ACTION {
            return Err("INVALID_ROBOT_MOVE_ACTION");
        }

        validate_robot_move_payload(payload_json).map(|_| ())
    }

    fn on_tick(&mut self, frame_id: u32, _fps: u16, inputs: &[PlayerInputRecord]) {
        self.tick_count = self.tick_count.saturating_add(1);
        self.last_frame = frame_id;

        for input in inputs {
            if input.action != ROBOT_MOVE_ACTION || input.is_synthetic {
                continue;
            }

            let Ok(payload) = validate_robot_move_payload(&input.payload_json) else {
                continue;
            };

            self.recent_inputs.push_back(RobotMoveInputSummary {
                frame_id: input.frame_id,
                player_id: input.player_id.clone(),
                seq: payload.seq,
                bot_tick: payload.bot_tick,
                dir_x: payload.dir_x,
                dir_y: payload.dir_y,
                speed: payload.speed,
            });
            while self.recent_inputs.len() > RECENT_INPUT_LIMIT {
                self.recent_inputs.pop_front();
            }
        }
    }

    fn get_serialized_state(&self) -> String {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct RobotSyncRoomState<'a> {
            logic_type: &'static str,
            room_id: &'a str,
            tick_count: u64,
            last_frame: u32,
            recent_inputs: Vec<&'a RobotMoveInputSummary>,
        }

        serde_json::to_string(&RobotSyncRoomState {
            logic_type: "robot_sync_room",
            room_id: &self.room_id,
            tick_count: self.tick_count,
            last_frame: self.last_frame,
            recent_inputs: self.recent_inputs.iter().collect(),
        })
        .unwrap_or_default()
    }
}

impl RoomLogicTransfer for RobotSyncRoomLogic {}

fn validate_robot_move_payload(payload_json: &str) -> Result<RobotMovePayload, &'static str> {
    if payload_json.len() > ROBOT_MOVE_PAYLOAD_MAX_BYTES {
        return Err("ROBOT_MOVE_PAYLOAD_TOO_LARGE");
    }

    let value =
        serde_json::from_str::<Value>(payload_json).map_err(|_| "INVALID_ROBOT_MOVE_JSON")?;
    let object = value.as_object().ok_or("ROBOT_MOVE_PAYLOAD_NOT_OBJECT")?;
    validate_robot_move_fields(object)?;

    let version = required_u32(object, "version", "INVALID_ROBOT_MOVE_VERSION")?;
    if version != 1 {
        return Err("UNSUPPORTED_ROBOT_MOVE_VERSION");
    }

    Ok(RobotMovePayload {
        seq: required_u32(object, "seq", "ROBOT_MOVE_SEQ_OUT_OF_RANGE")?,
        bot_tick: required_u32(object, "botTick", "ROBOT_MOVE_BOT_TICK_OUT_OF_RANGE")?,
        dir_x: required_i32_range(object, "dirX", -1000, 1000, "ROBOT_MOVE_DIR_OUT_OF_RANGE")?,
        dir_y: required_i32_range(object, "dirY", -1000, 1000, "ROBOT_MOVE_DIR_OUT_OF_RANGE")?,
        speed: required_u32_range(object, "speed", 0, 10000, "ROBOT_MOVE_SPEED_OUT_OF_RANGE")?,
    })
}

fn validate_robot_move_fields(object: &Map<String, Value>) -> Result<(), &'static str> {
    const REQUIRED_FIELDS: [&str; 6] = ["version", "seq", "botTick", "dirX", "dirY", "speed"];

    if object.len() != REQUIRED_FIELDS.len() {
        return Err("ROBOT_MOVE_FIELDS_MISMATCH");
    }

    for field in REQUIRED_FIELDS {
        if !object.contains_key(field) {
            return Err("ROBOT_MOVE_FIELDS_MISMATCH");
        }
    }

    Ok(())
}

fn required_i64(object: &Map<String, Value>, field: &str) -> Result<i64, &'static str> {
    object
        .get(field)
        .and_then(Value::as_i64)
        .ok_or("ROBOT_MOVE_FIELD_NOT_INTEGER")
}

fn required_u32(
    object: &Map<String, Value>,
    field: &str,
    range_error: &'static str,
) -> Result<u32, &'static str> {
    required_u32_range(object, field, 0, u32::MAX, range_error)
}

fn required_u32_range(
    object: &Map<String, Value>,
    field: &str,
    min: u32,
    max: u32,
    range_error: &'static str,
) -> Result<u32, &'static str> {
    let value = required_i64(object, field)?;
    if value < i64::from(min) || value > i64::from(max) {
        return Err(range_error);
    }
    Ok(value as u32)
}

fn required_i32_range(
    object: &Map<String, Value>,
    field: &str,
    min: i32,
    max: i32,
    range_error: &'static str,
) -> Result<i32, &'static str> {
    let value = required_i64(object, field)?;
    if value < i64::from(min) || value > i64::from(max) {
        return Err(range_error);
    }
    Ok(value as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config_table::ConfigTableRuntime;
    use crate::core::logic::RoomLogicFactory;
    use crate::gameroom::GameRoomLogicFactory;
    use std::path::Path;
    use std::time::Instant;

    fn valid_payload() -> &'static str {
        r#"{"version":1,"seq":42,"botTick":100,"dirX":-1000,"dirY":1000,"speed":10000}"#
    }

    fn config_tables() -> ConfigTableRuntime {
        ConfigTableRuntime::load_with_scene_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("csv"),
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("scene"),
        )
        .expect("game-server csv fixture should load")
    }

    #[test]
    fn validates_legal_robot_move_payload() {
        let logic = RobotSyncRoomLogic::default();

        assert_eq!(
            logic.validate_player_input("player-a", ROBOT_MOVE_ACTION, valid_payload()),
            Ok(())
        );
    }

    #[test]
    fn rejects_invalid_robot_move_payloads() {
        let logic = RobotSyncRoomLogic::default();

        assert_eq!(
            logic.validate_player_input("player-a", "move", valid_payload()),
            Err("INVALID_ROBOT_MOVE_ACTION")
        );
        assert_eq!(
            logic.validate_player_input("player-a", ROBOT_MOVE_ACTION, "{"),
            Err("INVALID_ROBOT_MOVE_JSON")
        );

        let too_large = format!(
            "{{\"payload\":\"{}\"}}",
            "x".repeat(ROBOT_MOVE_PAYLOAD_MAX_BYTES)
        );
        assert_eq!(
            logic.validate_player_input("player-a", ROBOT_MOVE_ACTION, &too_large),
            Err("ROBOT_MOVE_PAYLOAD_TOO_LARGE")
        );

        assert_eq!(
            logic.validate_player_input(
                "player-a",
                ROBOT_MOVE_ACTION,
                r#"{"version":1,"seq":42,"botTick":100,"dirX":-1,"dirY":1}"#
            ),
            Err("ROBOT_MOVE_FIELDS_MISMATCH")
        );
        assert_eq!(
            logic.validate_player_input(
                "player-a",
                ROBOT_MOVE_ACTION,
                r#"{"version":1,"seq":42,"botTick":100,"dirX":-1,"dirY":1,"speed":5,"extra":0}"#
            ),
            Err("ROBOT_MOVE_FIELDS_MISMATCH")
        );
        assert_eq!(
            logic.validate_player_input(
                "player-a",
                ROBOT_MOVE_ACTION,
                r#"{"version":1,"seq":42.0,"botTick":100,"dirX":-1,"dirY":1,"speed":5}"#
            ),
            Err("ROBOT_MOVE_FIELD_NOT_INTEGER")
        );
        assert_eq!(
            logic.validate_player_input(
                "player-a",
                ROBOT_MOVE_ACTION,
                r#"{"version":1,"seq":"42","botTick":100,"dirX":-1,"dirY":1,"speed":5}"#
            ),
            Err("ROBOT_MOVE_FIELD_NOT_INTEGER")
        );
        assert_eq!(
            logic.validate_player_input(
                "player-a",
                ROBOT_MOVE_ACTION,
                r#"{"version":1,"seq":42,"botTick":100,"dirX":-1001,"dirY":1,"speed":5}"#
            ),
            Err("ROBOT_MOVE_DIR_OUT_OF_RANGE")
        );
        assert_eq!(
            logic.validate_player_input(
                "player-a",
                ROBOT_MOVE_ACTION,
                r#"{"version":1,"seq":4294967296,"botTick":100,"dirX":-1,"dirY":1,"speed":5}"#
            ),
            Err("ROBOT_MOVE_SEQ_OUT_OF_RANGE")
        );
        assert_eq!(
            logic.validate_player_input(
                "player-a",
                ROBOT_MOVE_ACTION,
                r#"{"version":1,"seq":42,"botTick":100,"dirX":-1,"dirY":1,"speed":10001}"#
            ),
            Err("ROBOT_MOVE_SPEED_OUT_OF_RANGE")
        );
    }

    #[test]
    fn factory_creates_robot_sync_room_logic() {
        let factory = GameRoomLogicFactory::new(config_tables());

        let logic = factory.create("robot_sync_room");
        assert_eq!(
            logic.validate_player_input("player-a", ROBOT_MOVE_ACTION, valid_payload()),
            Ok(())
        );
        assert_eq!(
            logic.validate_player_input("player-a", "test_room_action", valid_payload()),
            Err("INVALID_ROBOT_MOVE_ACTION")
        );
        assert!(
            logic.get_serialized_state().contains("robot_sync_room"),
            "robot_sync_room should not fall back to TestRoomLogic"
        );
    }

    #[test]
    fn on_tick_state_contains_recent_input_summary() {
        let mut logic = RobotSyncRoomLogic::default();
        logic.on_room_created("room-robot");
        let input = PlayerInputRecord {
            frame_id: 7,
            player_id: "player-a".to_string(),
            action: ROBOT_MOVE_ACTION.to_string(),
            payload_json: valid_payload().to_string(),
            received_at: Instant::now(),
            is_synthetic: false,
        };

        logic.on_tick(7, 20, &[input]);

        let state = serde_json::from_str::<Value>(&logic.get_serialized_state()).unwrap();
        assert_eq!(state["logicType"], "robot_sync_room");
        assert_eq!(state["roomId"], "room-robot");
        assert_eq!(state["tickCount"], 1);
        assert_eq!(state["lastFrame"], 7);
        assert_eq!(state["recentInputs"][0]["frameId"], 7);
        assert_eq!(state["recentInputs"][0]["playerId"], "player-a");
        assert_eq!(state["recentInputs"][0]["seq"], 42);
        assert_eq!(state["recentInputs"][0]["botTick"], 100);
        assert_eq!(state["recentInputs"][0]["dirX"], -1000);
        assert_eq!(state["recentInputs"][0]["dirY"], 1000);
        assert_eq!(state["recentInputs"][0]["speed"], 10000);
        assert!(state.get("snapshot").is_none());
        assert!(state.get("movementState").is_none());
    }
}
