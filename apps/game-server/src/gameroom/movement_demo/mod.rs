use std::sync::Arc;

use serde::Serialize;
use tracing::{info, warn};

use crate::core::logic::{RoomLogic, RoomLogicBroadcast};
use crate::core::room::PlayerInputRecord;
use crate::core::system::movement::{RoomMovementState, decide_snapshot, tick_movement};
use crate::core::system::scene::{SceneCatalog, SceneQuery};
use crate::pb::{MovementRejectPush, MovementSnapshotPush};
use crate::protocol::{MessageType, encode_body};

const DEFAULT_MOVE_SPEED: f32 = 4.0;
const SNAPSHOT_INTERVAL_FRAMES: u32 = 3;

#[derive(Default)]
pub struct MovementDemoLogic {
    pub room_id: String,
    pub tick_count: u64,
    pub default_scene_id: i32,
    pub scene_catalog: Option<Arc<SceneCatalog>>,
    pub movement_state: Option<RoomMovementState>,
    pub pending_broadcasts: Vec<RoomLogicBroadcast>,
}

impl MovementDemoLogic {
    pub fn new(scene_catalog: Arc<SceneCatalog>, default_scene_id: i32) -> Self {
        Self {
            room_id: String::new(),
            tick_count: 0,
            default_scene_id,
            scene_catalog: Some(scene_catalog),
            movement_state: Some(RoomMovementState::new(
                default_scene_id,
                SNAPSHOT_INTERVAL_FRAMES,
            )),
            pending_broadcasts: Vec::new(),
        }
    }

    fn spawn_player_if_needed(&mut self, player_id: &str) {
        let (Some(scene_catalog), Some(movement_state)) =
            (self.scene_catalog.as_ref(), self.movement_state.as_mut())
        else {
            return;
        };

        if movement_state.entity(player_id).is_some() {
            return;
        }

        let Some(scene) = scene_catalog.scene(self.default_scene_id) else {
            warn!(scene_id = self.default_scene_id, "movement demo scene missing");
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

        movement_state.spawn_player(player_id, spawn, DEFAULT_MOVE_SPEED);
        info!(
            room_id = self.room_id,
            player_id,
            scene_id = scene.id,
            spawn_id = spawn.id,
            "movement demo player spawned"
        );
    }

    fn queue_snapshot_push(
        &mut self,
        frame_id: u32,
        full_sync: bool,
        reason: &str,
    ) {
        let Some(movement_state) = self.movement_state.as_ref() else {
            return;
        };
        let message = MovementSnapshotPush {
            room_id: self.room_id.clone(),
            frame_id,
            entities: movement_state.all_transforms(),
            full_sync,
            reason: reason.to_string(),
        };
        self.pending_broadcasts.push(RoomLogicBroadcast {
            message_type: MessageType::MovementSnapshotPush,
            body: encode_body(&message),
        });
    }
}

impl RoomLogic for MovementDemoLogic {
    fn on_room_created(&mut self, room_id: &str) {
        self.room_id = room_id.to_string();
        info!(room_id, "[RoomLogic/movement_demo] room created");
    }

    fn on_player_join(&mut self, player_id: &str) {
        self.spawn_player_if_needed(player_id);
    }

    fn on_player_leave(&mut self, player_id: &str) {
        if let Some(movement_state) = self.movement_state.as_mut() {
            movement_state.remove_player(player_id);
        }
    }

    fn on_game_started(&mut self, _room_id: &str) {
        self.queue_snapshot_push(0, true, "game_started");
    }

    fn on_tick(&mut self, frame_id: u32, fps: u16, inputs: &[PlayerInputRecord]) {
        self.tick_count += 1;
        let (Some(scene_catalog), Some(movement_state)) =
            (self.scene_catalog.as_ref(), self.movement_state.as_mut())
        else {
            return;
        };

        let result = tick_movement(movement_state, frame_id, fps, inputs, scene_catalog.as_ref());
        for reject in &result.rejects {
            info!(
                room_id = self.room_id,
                frame_id,
                player_id = reject.player_id,
                error_code = reject.error_code,
                "movement input rejected"
            );
            let message = MovementRejectPush {
                room_id: self.room_id.clone(),
                frame_id,
                player_id: reject.player_id.clone(),
                error_code: reject.error_code.clone(),
                corrected: Some(reject.corrected.clone()),
            };
            self.pending_broadcasts.push(RoomLogicBroadcast {
                message_type: MessageType::MovementRejectPush,
                body: encode_body(&message),
            });
        }

        let decision = decide_snapshot(
            movement_state,
            frame_id,
            result.changed_entities.len(),
            result.rejects.len(),
        );
        if decision.should_emit_snapshot {
            info!(
                room_id = self.room_id,
                frame_id,
                full_sync = decision.full_sync,
                reason = decision.reason,
                entity_count = movement_state.entity_count(),
                "movement snapshot queued"
            );
            self.queue_snapshot_push(frame_id, decision.full_sync, decision.reason);
        }
    }

    fn get_serialized_state(&self) -> String {
        #[derive(Serialize)]
        struct DemoEntityState {
            entity_id: u64,
            player_id: String,
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
                    player_id: entity.player_id,
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

    fn take_pending_broadcasts(&mut self) -> Vec<RoomLogicBroadcast> {
        std::mem::take(&mut self.pending_broadcasts)
    }
}
