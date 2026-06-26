use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::core::logic::{RoomLogic, RoomLogicTransfer};
use crate::core::room::PlayerInputRecord;

pub const UI_TOUCH_ACTION: &str = "ui_touch";
const UI_TOUCH_PAYLOAD_MAX_BYTES: usize = 2048;
const UI_TOUCH_MAX_SAMPLES: usize = 64;

#[derive(Default)]
pub struct UITouchRoomLogic {
    room_id: String,
    tick_count: u64,
    player_states: BTreeMap<String, TouchPlayerState>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TouchPlayerState {
    player_id: String,
    frame_id: u32,
    seq: u32,
    pointer_id: u32,
    pressed: bool,
    x: f32,
    y: f32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TouchInputPayload {
    #[serde(default)]
    seq: u32,
    #[serde(default)]
    pointer_id: u32,
    #[serde(default)]
    pressed: bool,
    #[serde(default)]
    samples: Vec<TouchSample>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TouchSample {
    x: f32,
    y: f32,
}

impl RoomLogic for UITouchRoomLogic {
    fn on_room_created(&mut self, room_id: &str) {
        self.room_id = room_id.to_string();
        info!(room_id, "[RoomLogic/ui_touch_room] room created");
    }

    fn on_character_join(&mut self, character_id: &str) {
        info!(
            room_id = self.room_id,
            character_id, "ui touch player joined"
        );
    }

    fn on_character_leave(&mut self, character_id: &str) {
        self.player_states.remove(character_id);
        info!(room_id = self.room_id, character_id, "ui touch player left");
    }

    fn on_character_offline(&mut self, _room_id: &str, character_id: &str) {
        if let Some(state) = self.player_states.get_mut(character_id) {
            state.pressed = false;
        }
    }

    fn validate_character_input(
        &self,
        _character_id: &str,
        action: &str,
        payload_json: &str,
    ) -> Result<(), &'static str> {
        if action != UI_TOUCH_ACTION {
            return Err("INVALID_UI_TOUCH_ACTION");
        }

        validate_touch_payload(payload_json)
    }

    fn on_tick(&mut self, frame_id: u32, _fps: u16, inputs: &[PlayerInputRecord]) {
        self.tick_count = self.tick_count.saturating_add(1);

        for input in inputs {
            if input.action != UI_TOUCH_ACTION || input.is_synthetic {
                continue;
            }

            let Ok(payload) = serde_json::from_str::<TouchInputPayload>(&input.payload_json) else {
                warn!(
                    room_id = self.room_id,
                    player_id = input.character_id,
                    frame_id,
                    "invalid ui touch payload"
                );
                continue;
            };

            let Some(sample) = payload.samples.last() else {
                self.player_states
                    .entry(input.character_id.clone())
                    .and_modify(|state| {
                        state.frame_id = frame_id;
                        state.seq = payload.seq;
                        state.pressed = payload.pressed;
                    });
                continue;
            };

            self.player_states.insert(
                input.character_id.clone(),
                TouchPlayerState {
                    player_id: input.character_id.clone(),
                    frame_id,
                    seq: payload.seq,
                    pointer_id: payload.pointer_id,
                    pressed: payload.pressed,
                    x: sample.x.clamp(0.0, 1.0),
                    y: sample.y.clamp(0.0, 1.0),
                },
            );
        }
    }

    fn get_serialized_state(&self) -> String {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct TouchRoomState<'a> {
            room_id: &'a str,
            tick_count: u64,
            players: Vec<&'a TouchPlayerState>,
        }

        serde_json::to_string(&TouchRoomState {
            room_id: &self.room_id,
            tick_count: self.tick_count,
            players: self.player_states.values().collect(),
        })
        .unwrap_or_default()
    }
}

impl RoomLogicTransfer for UITouchRoomLogic {}

fn validate_touch_payload(payload_json: &str) -> Result<(), &'static str> {
    if payload_json.len() > UI_TOUCH_PAYLOAD_MAX_BYTES {
        return Err("UI_TOUCH_PAYLOAD_TOO_LARGE");
    }

    let payload = serde_json::from_str::<TouchInputPayload>(payload_json)
        .map_err(|_| "INVALID_UI_TOUCH_PAYLOAD")?;
    if payload.samples.len() > UI_TOUCH_MAX_SAMPLES {
        return Err("UI_TOUCH_SAMPLE_COUNT_TOO_LARGE");
    }

    for sample in &payload.samples {
        if !sample.x.is_finite() || !sample.y.is_finite() {
            return Err("UI_TOUCH_SAMPLE_NOT_FINITE");
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_touch_payload_bounds() {
        let payload = r#"{"version":1,"seq":1,"space":"viewport01","pointerId":0,"pressed":true,"samples":[{"phase":"down","x":0.25,"y":0.75}]}"#;
        assert_eq!(validate_touch_payload(payload), Ok(()));

        let logic = UITouchRoomLogic::default();
        assert_eq!(
            logic.validate_character_input("player-a", "move", payload),
            Err("INVALID_UI_TOUCH_ACTION")
        );

        let too_many_samples = format!(
            "{{\"samples\":[{}]}}",
            (0..=UI_TOUCH_MAX_SAMPLES)
                .map(|_| "{\"x\":0.5,\"y\":0.5}")
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(
            validate_touch_payload(&too_many_samples),
            Err("UI_TOUCH_SAMPLE_COUNT_TOO_LARGE")
        );

        assert_eq!(
            validate_touch_payload("{\"samples\":[{\"x\":null,\"y\":0.5}]}"),
            Err("INVALID_UI_TOUCH_PAYLOAD")
        );
    }
}
