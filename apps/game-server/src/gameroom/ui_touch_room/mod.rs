use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::core::logic::RoomLogic;
use crate::core::room::PlayerInputRecord;

pub const UI_TOUCH_ACTION: &str = "ui_touch";

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

    fn on_player_join(&mut self, player_id: &str) {
        info!(room_id = self.room_id, player_id, "ui touch player joined");
    }

    fn on_player_leave(&mut self, player_id: &str) {
        self.player_states.remove(player_id);
        info!(room_id = self.room_id, player_id, "ui touch player left");
    }

    fn on_player_offline(&mut self, _room_id: &str, player_id: &str) {
        if let Some(state) = self.player_states.get_mut(player_id) {
            state.pressed = false;
        }
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
                    player_id = input.player_id,
                    frame_id,
                    "invalid ui touch payload"
                );
                continue;
            };

            let Some(sample) = payload.samples.last() else {
                self.player_states
                    .entry(input.player_id.clone())
                    .and_modify(|state| {
                        state.frame_id = frame_id;
                        state.seq = payload.seq;
                        state.pressed = payload.pressed;
                    });
                continue;
            };

            self.player_states.insert(
                input.player_id.clone(),
                TouchPlayerState {
                    player_id: input.player_id.clone(),
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
