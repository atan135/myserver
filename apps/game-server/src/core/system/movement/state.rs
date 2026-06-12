use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::system::movement::input::{ClientMovementState, MovementCommand};
use crate::core::system::scene::query::SceneSpawnPointDefinition;
use crate::pb::{
    EntityTransform, MovementCorrectionKind, MovementCorrectionReason, MovementRecoveryState,
};

pub type EntityId = u64;
pub type DenseIndex = usize;
const ROOM_MOVEMENT_TRANSFER_SCHEMA: &str = "room-movement-state.v1";
const ROOM_MOVEMENT_TRANSFER_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub fn normalized(self) -> Option<Self> {
        let len_sq = self.x * self.x + self.y * self.y;
        if len_sq <= f32::EPSILON {
            return None;
        }

        let inv_len = len_sq.sqrt().recip();
        Some(Self {
            x: self.x * inv_len,
            y: self.y * inv_len,
        })
    }
}

#[derive(Debug, Clone)]
pub struct EntityMeta {
    pub entity_id: EntityId,
    pub scene_id: i32,
    pub player_id: Option<String>,
    pub alive: bool,
}

#[derive(Debug, Clone)]
pub struct MovementEntityState {
    pub entity_id: EntityId,
    pub player_id: String,
    pub scene_id: i32,
    pub position: Vec2,
    pub direction: Vec2,
    pub speed: f32,
    pub moving: bool,
    pub last_input_frame: u32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ClientStateSample {
    pub frame_id: u32,
    pub x: f32,
    pub y: f32,
}

#[derive(Debug, Clone)]
pub struct MovementCorrectionEnvelope {
    pub frame_id: u32,
    pub entities: Vec<EntityTransform>,
    pub correction_kind: i32,
    pub reason_code: i32,
    pub reference_frame_id: u32,
    pub target_player_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RoomMovementState {
    pub scene_id: i32,
    pub next_entity_id: EntityId,
    pub snapshot_interval_frames: u32,
    pub last_snapshot_frame: u32,
    pub last_full_sync_frame: u32,
    pub correction_distance_threshold: f32,
    pub correction_interval_frames: u32,
    pub aoi_radius: f32,
    pub aoi_enabled: bool,
    pub movement_control_stop_frames: u32,
    entities: Vec<EntityMeta>,
    positions_x: Vec<f32>,
    positions_y: Vec<f32>,
    directions_x: Vec<f32>,
    directions_y: Vec<f32>,
    speeds: Vec<f32>,
    moving_flags: Vec<bool>,
    last_input_frames: Vec<u32>,
    player_entity_map: HashMap<String, EntityId>,
    entity_index_map: HashMap<EntityId, DenseIndex>,
    index_entity_map: Vec<EntityId>,
    latest_client_state_by_player: HashMap<String, ClientStateSample>,
    last_sent_frame_by_player: HashMap<String, u32>,
    missing_control_frames_by_player: HashMap<String, u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoomMovementTransferSnapshot {
    schema: String,
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    scene_id: i32,
    next_entity_id: EntityId,
    snapshot_interval_frames: u32,
    last_snapshot_frame: u32,
    last_full_sync_frame: u32,
    correction_distance_threshold: f32,
    correction_interval_frames: u32,
    aoi_radius: f32,
    aoi_enabled: bool,
    movement_control_stop_frames: u32,
    entities: Vec<RoomMovementTransferEntity>,
    latest_client_state_by_player: Vec<RoomMovementTransferClientState>,
    last_sent_frame_by_player: Vec<RoomMovementTransferPlayerFrame>,
    missing_control_frames_by_player: Vec<RoomMovementTransferPlayerFrame>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoomMovementTransferEntity {
    entity_id: EntityId,
    player_id: Option<String>,
    scene_id: i32,
    position: RoomMovementTransferVec2,
    direction: RoomMovementTransferVec2,
    speed: f32,
    moving: bool,
    last_input_frame: u32,
    alive: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct RoomMovementTransferVec2 {
    x: f32,
    y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoomMovementTransferClientState {
    player_id: String,
    frame_id: u32,
    x: f32,
    y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoomMovementTransferPlayerFrame {
    player_id: String,
    frame_id: u32,
}

impl RoomMovementState {
    pub fn new(scene_id: i32, snapshot_interval_frames: u32) -> Self {
        Self {
            scene_id,
            next_entity_id: 1,
            snapshot_interval_frames,
            last_snapshot_frame: 0,
            last_full_sync_frame: 0,
            correction_distance_threshold: 0.5,
            correction_interval_frames: snapshot_interval_frames.max(1),
            aoi_radius: 0.0,
            aoi_enabled: false,
            movement_control_stop_frames: 3,
            entities: Vec::new(),
            positions_x: Vec::new(),
            positions_y: Vec::new(),
            directions_x: Vec::new(),
            directions_y: Vec::new(),
            speeds: Vec::new(),
            moving_flags: Vec::new(),
            last_input_frames: Vec::new(),
            player_entity_map: HashMap::new(),
            entity_index_map: HashMap::new(),
            index_entity_map: Vec::new(),
            latest_client_state_by_player: HashMap::new(),
            last_sent_frame_by_player: HashMap::new(),
            missing_control_frames_by_player: HashMap::new(),
        }
    }

    pub fn spawn_player(
        &mut self,
        player_id: &str,
        spawn: &SceneSpawnPointDefinition,
        speed: f32,
    ) -> MovementEntityState {
        let entity_id = self.next_entity_id;
        self.next_entity_id += 1;

        let dense_index = self.entities.len();
        self.entities.push(EntityMeta {
            entity_id,
            scene_id: spawn.scene_id,
            player_id: Some(player_id.to_string()),
            alive: true,
        });
        self.positions_x.push(spawn.x);
        self.positions_y.push(spawn.y);
        self.directions_x.push(spawn.dir_x);
        self.directions_y.push(spawn.dir_y);
        self.speeds.push(speed);
        self.moving_flags.push(false);
        self.last_input_frames.push(0);
        self.player_entity_map
            .insert(player_id.to_string(), entity_id);
        self.entity_index_map.insert(entity_id, dense_index);
        self.index_entity_map.push(entity_id);
        self.missing_control_frames_by_player
            .insert(player_id.to_string(), 0);

        self.entity_state_at(dense_index)
            .expect("spawned movement entity missing")
    }

    pub fn remove_player(&mut self, player_id: &str) {
        let Some(entity_id) = self.player_entity_map.remove(player_id) else {
            return;
        };
        let Some(dense_index) = self.entity_index_map.remove(&entity_id) else {
            return;
        };

        self.entities.swap_remove(dense_index);
        self.positions_x.swap_remove(dense_index);
        self.positions_y.swap_remove(dense_index);
        self.directions_x.swap_remove(dense_index);
        self.directions_y.swap_remove(dense_index);
        self.speeds.swap_remove(dense_index);
        self.moving_flags.swap_remove(dense_index);
        self.last_input_frames.swap_remove(dense_index);
        self.index_entity_map.swap_remove(dense_index);

        if dense_index < self.entities.len() {
            let swapped_entity_id = self.index_entity_map[dense_index];
            self.entity_index_map.insert(swapped_entity_id, dense_index);
        }

        self.latest_client_state_by_player.remove(player_id);
        self.last_sent_frame_by_player.remove(player_id);
        self.missing_control_frames_by_player.remove(player_id);
    }

    pub fn entity(&self, player_id: &str) -> Option<MovementEntityState> {
        let dense_index = self.dense_index_by_player(player_id)?;
        self.entity_state_at(dense_index)
    }

    pub fn dense_index_by_player(&self, player_id: &str) -> Option<DenseIndex> {
        let entity_id = self.player_entity_map.get(player_id)?;
        self.entity_index_map.get(entity_id).copied()
    }

    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    pub fn dense_indices(&self) -> std::ops::Range<usize> {
        0..self.entities.len()
    }

    pub fn entity_state_at(&self, dense_index: DenseIndex) -> Option<MovementEntityState> {
        let meta = self.entities.get(dense_index)?;
        let player_id = meta.player_id.as_ref()?.clone();
        Some(MovementEntityState {
            entity_id: meta.entity_id,
            player_id,
            scene_id: meta.scene_id,
            position: Vec2 {
                x: *self.positions_x.get(dense_index)?,
                y: *self.positions_y.get(dense_index)?,
            },
            direction: Vec2 {
                x: *self.directions_x.get(dense_index)?,
                y: *self.directions_y.get(dense_index)?,
            },
            speed: *self.speeds.get(dense_index)?,
            moving: *self.moving_flags.get(dense_index)?,
            last_input_frame: *self.last_input_frames.get(dense_index)?,
        })
    }

    pub fn entity_proto_at(&self, dense_index: DenseIndex) -> Option<EntityTransform> {
        self.entity_state_at(dense_index)
            .map(|entity| entity.to_proto())
    }

    pub fn entity_player_id(&self, dense_index: DenseIndex) -> Option<&str> {
        self.entities.get(dense_index)?.player_id.as_deref()
    }

    pub fn entity_scene_id(&self, dense_index: DenseIndex) -> Option<i32> {
        self.entities.get(dense_index).map(|meta| meta.scene_id)
    }

    pub fn position_at(&self, dense_index: DenseIndex) -> Option<Vec2> {
        Some(Vec2 {
            x: *self.positions_x.get(dense_index)?,
            y: *self.positions_y.get(dense_index)?,
        })
    }

    pub fn direction_at(&self, dense_index: DenseIndex) -> Option<Vec2> {
        Some(Vec2 {
            x: *self.directions_x.get(dense_index)?,
            y: *self.directions_y.get(dense_index)?,
        })
    }

    pub fn speed_at(&self, dense_index: DenseIndex) -> Option<f32> {
        self.speeds.get(dense_index).copied()
    }

    pub fn is_moving_at(&self, dense_index: DenseIndex) -> Option<bool> {
        self.moving_flags.get(dense_index).copied()
    }

    pub fn set_position_at(&mut self, dense_index: DenseIndex, position: Vec2) -> bool {
        let (Some(pos_x), Some(pos_y)) = (
            self.positions_x.get_mut(dense_index),
            self.positions_y.get_mut(dense_index),
        ) else {
            return false;
        };
        *pos_x = position.x;
        *pos_y = position.y;
        true
    }

    pub fn set_direction_at(&mut self, dense_index: DenseIndex, direction: Vec2) -> bool {
        let (Some(dir_x), Some(dir_y)) = (
            self.directions_x.get_mut(dense_index),
            self.directions_y.get_mut(dense_index),
        ) else {
            return false;
        };
        *dir_x = direction.x;
        *dir_y = direction.y;
        true
    }

    pub fn set_moving_at(&mut self, dense_index: DenseIndex, moving: bool) -> bool {
        let Some(flag) = self.moving_flags.get_mut(dense_index) else {
            return false;
        };
        *flag = moving;
        true
    }

    pub fn set_last_input_frame_at(&mut self, dense_index: DenseIndex, frame_id: u32) -> bool {
        let Some(last_input_frame) = self.last_input_frames.get_mut(dense_index) else {
            return false;
        };
        *last_input_frame = frame_id;
        true
    }

    pub fn apply_command_at(
        &mut self,
        dense_index: DenseIndex,
        frame_id: u32,
        command: MovementCommand,
    ) -> bool {
        match command {
            MovementCommand::MoveDir(direction) => {
                let Some(normalized) = direction.normalized() else {
                    return false;
                };
                self.set_direction_at(dense_index, normalized)
                    && self.set_moving_at(dense_index, true)
                    && self.set_last_input_frame_at(dense_index, frame_id)
            }
            MovementCommand::MoveStop => {
                self.set_moving_at(dense_index, false)
                    && self.set_last_input_frame_at(dense_index, frame_id)
            }
            MovementCommand::FaceTo(direction) => {
                let Some(normalized) = direction.normalized() else {
                    return false;
                };
                self.set_direction_at(dense_index, normalized)
                    && self.set_last_input_frame_at(dense_index, frame_id)
            }
        }
    }

    pub fn stop_player(&mut self, player_id: &str, frame_id: u32) -> Option<EntityTransform> {
        let dense_index = self.dense_index_by_player(player_id)?;
        let was_moving = self.is_moving_at(dense_index).unwrap_or(false);
        let current_frame = self
            .entity_state_at(dense_index)
            .map(|entity| entity.last_input_frame)
            .unwrap_or_default();

        if !was_moving && current_frame == frame_id {
            return self.entity_proto_at(dense_index);
        }

        let _ = self.set_moving_at(dense_index, false);
        let _ = self.set_last_input_frame_at(dense_index, frame_id);
        self.reset_missing_movement_control_frames(player_id);
        self.entity_proto_at(dense_index)
    }

    pub fn all_transforms(&self) -> Vec<EntityTransform> {
        self.dense_indices()
            .filter_map(|dense_index| self.entity_proto_at(dense_index))
            .collect()
    }

    pub fn set_client_state_for_player(
        &mut self,
        player_id: &str,
        client_state: ClientMovementState,
    ) {
        self.latest_client_state_by_player.insert(
            player_id.to_string(),
            ClientStateSample {
                frame_id: client_state.frame_id,
                x: client_state.position.x,
                y: client_state.position.y,
            },
        );
    }

    pub fn client_state_for_player(&self, player_id: &str) -> Option<ClientStateSample> {
        self.latest_client_state_by_player.get(player_id).copied()
    }

    pub fn drift_distance_for_player(&self, player_id: &str) -> Option<f32> {
        let client = self.client_state_for_player(player_id)?;
        let entity = self.entity(player_id)?;
        Some(distance(
            entity.position,
            Vec2 {
                x: client.x,
                y: client.y,
            },
        ))
    }

    pub fn should_force_correction_for_player(&self, player_id: &str) -> bool {
        self.drift_distance_for_player(player_id)
            .map(|drift| drift >= self.correction_distance_threshold)
            .unwrap_or(false)
    }

    pub fn note_sent_to_player(&mut self, player_id: &str, frame_id: u32) {
        self.last_sent_frame_by_player
            .insert(player_id.to_string(), frame_id);
    }

    pub fn should_periodic_sync(&self, frame_id: u32) -> bool {
        self.last_snapshot_frame == 0
            || frame_id.saturating_sub(self.last_snapshot_frame) >= self.correction_interval_frames
    }

    pub fn set_correction_config(
        &mut self,
        correction_interval_frames: u32,
        correction_distance_threshold: f32,
        aoi_radius: f32,
        aoi_enabled: bool,
    ) {
        self.correction_interval_frames = correction_interval_frames.max(1);
        self.correction_distance_threshold = correction_distance_threshold.max(0.0);
        self.aoi_radius = aoi_radius.max(0.0);
        self.aoi_enabled = aoi_enabled && self.aoi_radius > 0.0;
    }

    pub fn set_movement_control_stop_frames(&mut self, movement_control_stop_frames: u32) {
        self.movement_control_stop_frames = movement_control_stop_frames;
    }

    pub fn reset_missing_movement_control_frames(&mut self, player_id: &str) {
        self.missing_control_frames_by_player
            .insert(player_id.to_string(), 0);
    }

    pub fn increment_missing_movement_control_frames(&mut self, player_id: &str) -> u32 {
        let next = self
            .missing_control_frames_by_player
            .get(player_id)
            .copied()
            .unwrap_or_default()
            .saturating_add(1);
        self.missing_control_frames_by_player
            .insert(player_id.to_string(), next);
        next
    }

    pub fn targets_for_player(&self, requester_player_id: &str) -> Vec<EntityTransform> {
        if !self.aoi_enabled {
            return self.all_transforms();
        }

        let Some(origin) = self
            .entity(requester_player_id)
            .map(|entity| entity.position)
        else {
            return self.all_transforms();
        };

        self.dense_indices()
            .filter_map(|dense_index| self.entity_proto_at(dense_index))
            .filter(|entity| {
                entity.player_id == requester_player_id
                    || distance(
                        origin,
                        Vec2 {
                            x: entity.x,
                            y: entity.y,
                        },
                    ) <= self.aoi_radius
            })
            .collect()
    }

    pub fn full_correction(
        &mut self,
        frame_id: u32,
        reason_code: MovementCorrectionReason,
        target_player_ids: Vec<String>,
        entities: Vec<EntityTransform>,
    ) -> MovementCorrectionEnvelope {
        self.last_snapshot_frame = frame_id;
        self.last_full_sync_frame = frame_id;
        MovementCorrectionEnvelope {
            frame_id,
            entities,
            correction_kind: MovementCorrectionKind::FullSync as i32,
            reason_code: reason_code as i32,
            reference_frame_id: frame_id,
            target_player_ids,
        }
    }

    pub fn strong_correction(
        &mut self,
        frame_id: u32,
        reason_code: MovementCorrectionReason,
        target_player_ids: Vec<String>,
        entities: Vec<EntityTransform>,
    ) -> MovementCorrectionEnvelope {
        self.last_snapshot_frame = frame_id;
        MovementCorrectionEnvelope {
            frame_id,
            entities,
            correction_kind: MovementCorrectionKind::Strong as i32,
            reason_code: reason_code as i32,
            reference_frame_id: frame_id,
            target_player_ids,
        }
    }

    pub fn incremental_correction(
        &mut self,
        frame_id: u32,
        reason_code: MovementCorrectionReason,
        target_player_ids: Vec<String>,
        entities: Vec<EntityTransform>,
    ) -> MovementCorrectionEnvelope {
        self.last_snapshot_frame = frame_id;
        MovementCorrectionEnvelope {
            frame_id,
            entities,
            correction_kind: MovementCorrectionKind::Incremental as i32,
            reason_code: reason_code as i32,
            reference_frame_id: frame_id,
            target_player_ids,
        }
    }

    pub fn recovery_state_for_player(
        &self,
        requester_player_id: Option<&str>,
        frame_id: u32,
        reason_code: MovementCorrectionReason,
    ) -> MovementRecoveryState {
        let entities = requester_player_id
            .map(|player_id| self.targets_for_player(player_id))
            .unwrap_or_else(|| self.all_transforms());

        MovementRecoveryState {
            frame_id,
            entities,
            correction_kind: MovementCorrectionKind::Recovery as i32,
            reason_code: reason_code as i32,
            reference_frame_id: frame_id,
            aoi_enabled: self.aoi_enabled,
            aoi_radius: self.aoi_radius,
        }
    }

    pub fn export_transfer_state_json(&self) -> Result<String, &'static str> {
        let snapshot = RoomMovementTransferSnapshot::from_state(self)?;
        serde_json::to_string(&snapshot).map_err(|_| "ROOM_TRANSFER_INVALID_MOVEMENT_STATE")
    }

    pub fn import_transfer_state_json(state_json: &str) -> Result<Self, &'static str> {
        let value = serde_json::from_str::<Value>(state_json)
            .map_err(|_| "ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?;
        if value.get("schema").and_then(Value::as_str) != Some(ROOM_MOVEMENT_TRANSFER_SCHEMA) {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }
        if value.get("schemaVersion").and_then(Value::as_u64)
            != Some(ROOM_MOVEMENT_TRANSFER_SCHEMA_VERSION as u64)
        {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }

        let snapshot = serde_json::from_value::<RoomMovementTransferSnapshot>(value)
            .map_err(|_| "ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?;
        snapshot.into_state()
    }
}

impl RoomMovementTransferSnapshot {
    fn from_state(state: &RoomMovementState) -> Result<Self, &'static str> {
        validate_non_negative_finite(state.correction_distance_threshold)?;
        validate_non_negative_finite(state.aoi_radius)?;

        let mut latest_client_state_by_player = state
            .latest_client_state_by_player
            .iter()
            .map(|(player_id, sample)| {
                validate_player_id(player_id)?;
                validate_finite(sample.x)?;
                validate_finite(sample.y)?;
                Ok(RoomMovementTransferClientState {
                    player_id: player_id.clone(),
                    frame_id: sample.frame_id,
                    x: sample.x,
                    y: sample.y,
                })
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        latest_client_state_by_player.sort_by(|left, right| left.player_id.cmp(&right.player_id));

        let mut last_sent_frame_by_player =
            transfer_player_frames(&state.last_sent_frame_by_player)?;
        let mut missing_control_frames_by_player =
            transfer_player_frames(&state.missing_control_frames_by_player)?;
        last_sent_frame_by_player.sort_by(|left, right| left.player_id.cmp(&right.player_id));
        missing_control_frames_by_player
            .sort_by(|left, right| left.player_id.cmp(&right.player_id));

        let entities = state
            .entities
            .iter()
            .enumerate()
            .map(|(dense_index, meta)| {
                let position = Vec2 {
                    x: *state
                        .positions_x
                        .get(dense_index)
                        .ok_or("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?,
                    y: *state
                        .positions_y
                        .get(dense_index)
                        .ok_or("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?,
                };
                let direction = Vec2 {
                    x: *state
                        .directions_x
                        .get(dense_index)
                        .ok_or("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?,
                    y: *state
                        .directions_y
                        .get(dense_index)
                        .ok_or("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?,
                };
                let speed = *state
                    .speeds
                    .get(dense_index)
                    .ok_or("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?;
                validate_finite(position.x)?;
                validate_finite(position.y)?;
                validate_finite(direction.x)?;
                validate_finite(direction.y)?;
                validate_non_negative_finite(speed)?;

                Ok(RoomMovementTransferEntity {
                    entity_id: meta.entity_id,
                    player_id: meta.player_id.clone(),
                    scene_id: meta.scene_id,
                    position: RoomMovementTransferVec2 {
                        x: position.x,
                        y: position.y,
                    },
                    direction: RoomMovementTransferVec2 {
                        x: direction.x,
                        y: direction.y,
                    },
                    speed,
                    moving: *state
                        .moving_flags
                        .get(dense_index)
                        .ok_or("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?,
                    last_input_frame: *state
                        .last_input_frames
                        .get(dense_index)
                        .ok_or("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?,
                    alive: meta.alive,
                })
            })
            .collect::<Result<Vec<_>, &'static str>>()?;

        Ok(Self {
            schema: ROOM_MOVEMENT_TRANSFER_SCHEMA.to_string(),
            schema_version: ROOM_MOVEMENT_TRANSFER_SCHEMA_VERSION,
            scene_id: state.scene_id,
            next_entity_id: state.next_entity_id,
            snapshot_interval_frames: state.snapshot_interval_frames,
            last_snapshot_frame: state.last_snapshot_frame,
            last_full_sync_frame: state.last_full_sync_frame,
            correction_distance_threshold: state.correction_distance_threshold,
            correction_interval_frames: state.correction_interval_frames,
            aoi_radius: state.aoi_radius,
            aoi_enabled: state.aoi_enabled,
            movement_control_stop_frames: state.movement_control_stop_frames,
            entities,
            latest_client_state_by_player,
            last_sent_frame_by_player,
            missing_control_frames_by_player,
        })
    }

    fn into_state(self) -> Result<RoomMovementState, &'static str> {
        if self.schema != ROOM_MOVEMENT_TRANSFER_SCHEMA
            || self.schema_version != ROOM_MOVEMENT_TRANSFER_SCHEMA_VERSION
        {
            return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
        }
        if self.next_entity_id == 0
            || self.snapshot_interval_frames == 0
            || self.correction_interval_frames == 0
        {
            return Err("ROOM_TRANSFER_INVALID_MOVEMENT_STATE");
        }
        validate_non_negative_finite(self.correction_distance_threshold)?;
        validate_non_negative_finite(self.aoi_radius)?;

        let mut state = RoomMovementState::new(self.scene_id, self.snapshot_interval_frames);
        state.next_entity_id = self.next_entity_id;
        state.last_snapshot_frame = self.last_snapshot_frame;
        state.last_full_sync_frame = self.last_full_sync_frame;
        state.correction_distance_threshold = self.correction_distance_threshold;
        state.correction_interval_frames = self.correction_interval_frames;
        state.aoi_radius = self.aoi_radius;
        state.aoi_enabled = self.aoi_enabled;
        state.movement_control_stop_frames = self.movement_control_stop_frames;

        for entity in self.entities {
            entity.push_into_state(&mut state)?;
        }

        state.latest_client_state_by_player =
            client_state_map_from_transfer(self.latest_client_state_by_player)?;
        state.last_sent_frame_by_player =
            player_frame_map_from_transfer(self.last_sent_frame_by_player)?;
        state.missing_control_frames_by_player =
            player_frame_map_from_transfer(self.missing_control_frames_by_player)?;

        Ok(state)
    }
}

impl RoomMovementTransferEntity {
    fn push_into_state(self, state: &mut RoomMovementState) -> Result<(), &'static str> {
        if self.entity_id == 0 || self.entity_id >= state.next_entity_id {
            return Err("ROOM_TRANSFER_INVALID_MOVEMENT_STATE");
        }
        validate_finite(self.position.x)?;
        validate_finite(self.position.y)?;
        validate_finite(self.direction.x)?;
        validate_finite(self.direction.y)?;
        validate_non_negative_finite(self.speed)?;
        if let Some(player_id) = self.player_id.as_deref() {
            validate_player_id(player_id)?;
        }
        if state.entity_index_map.contains_key(&self.entity_id) {
            return Err("ROOM_TRANSFER_INVALID_MOVEMENT_STATE");
        }

        let dense_index = state.entities.len();
        state.entities.push(EntityMeta {
            entity_id: self.entity_id,
            scene_id: self.scene_id,
            player_id: self.player_id.clone(),
            alive: self.alive,
        });
        state.positions_x.push(self.position.x);
        state.positions_y.push(self.position.y);
        state.directions_x.push(self.direction.x);
        state.directions_y.push(self.direction.y);
        state.speeds.push(self.speed);
        state.moving_flags.push(self.moving);
        state.last_input_frames.push(self.last_input_frame);
        state.entity_index_map.insert(self.entity_id, dense_index);
        state.index_entity_map.push(self.entity_id);
        if let Some(player_id) = self.player_id {
            if state
                .player_entity_map
                .insert(player_id, self.entity_id)
                .is_some()
            {
                return Err("ROOM_TRANSFER_INVALID_MOVEMENT_STATE");
            }
        }
        Ok(())
    }
}

fn transfer_player_frames(
    frames: &HashMap<String, u32>,
) -> Result<Vec<RoomMovementTransferPlayerFrame>, &'static str> {
    frames
        .iter()
        .map(|(player_id, frame_id)| {
            validate_player_id(player_id)?;
            Ok(RoomMovementTransferPlayerFrame {
                player_id: player_id.clone(),
                frame_id: *frame_id,
            })
        })
        .collect()
}

fn client_state_map_from_transfer(
    states: Vec<RoomMovementTransferClientState>,
) -> Result<HashMap<String, ClientStateSample>, &'static str> {
    let mut result = HashMap::new();
    for state in states {
        validate_player_id(&state.player_id)?;
        validate_finite(state.x)?;
        validate_finite(state.y)?;
        if result
            .insert(
                state.player_id,
                ClientStateSample {
                    frame_id: state.frame_id,
                    x: state.x,
                    y: state.y,
                },
            )
            .is_some()
        {
            return Err("ROOM_TRANSFER_INVALID_MOVEMENT_STATE");
        }
    }
    Ok(result)
}

fn player_frame_map_from_transfer(
    frames: Vec<RoomMovementTransferPlayerFrame>,
) -> Result<HashMap<String, u32>, &'static str> {
    let mut result = HashMap::new();
    for frame in frames {
        validate_player_id(&frame.player_id)?;
        if result.insert(frame.player_id, frame.frame_id).is_some() {
            return Err("ROOM_TRANSFER_INVALID_MOVEMENT_STATE");
        }
    }
    Ok(result)
}

fn validate_player_id(player_id: &str) -> Result<(), &'static str> {
    if player_id.trim().is_empty() {
        return Err("ROOM_TRANSFER_INVALID_MOVEMENT_STATE");
    }
    Ok(())
}

fn validate_finite(value: f32) -> Result<(), &'static str> {
    if value.is_finite() {
        Ok(())
    } else {
        Err("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")
    }
}

fn validate_non_negative_finite(value: f32) -> Result<(), &'static str> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err("ROOM_TRANSFER_INVALID_MOVEMENT_STATE")
    }
}

impl MovementEntityState {
    pub fn to_proto(&self) -> EntityTransform {
        EntityTransform {
            entity_id: self.entity_id,
            player_id: self.player_id.clone(),
            scene_id: self.scene_id,
            x: self.position.x,
            y: self.position.y,
            dir_x: self.direction.x,
            dir_y: self.direction.y,
            moving: self.moving,
            last_input_frame: self.last_input_frame,
        }
    }
}

fn distance(lhs: Vec2, rhs: Vec2) -> f32 {
    let dx = lhs.x - rhs.x;
    let dy = lhs.y - rhs.y;
    (dx * dx + dy * dy).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_spawn() -> SceneSpawnPointDefinition {
        SceneSpawnPointDefinition {
            id: 1001,
            scene_id: 1,
            code: "spawn".to_string(),
            spawn_type: "player".to_string(),
            x: 1.0,
            y: 1.0,
            dir_x: 1.0,
            dir_y: 0.0,
            radius: 0.0,
            tags: Vec::new(),
        }
    }

    #[test]
    fn stop_player_clears_moving_flag_and_updates_frame() {
        let mut state = RoomMovementState::new(1, 3);
        let spawn = build_spawn();
        state.spawn_player("player-a", &spawn, 4.0);

        let dense_index = state.dense_index_by_player("player-a").unwrap();
        state.apply_command_at(
            dense_index,
            5,
            MovementCommand::MoveDir(Vec2 { x: 1.0, y: 0.0 }),
        );

        let corrected = state.stop_player("player-a", 9).unwrap();

        assert!(!corrected.moving);
        assert_eq!(corrected.last_input_frame, 9);
        assert!(!state.entity("player-a").unwrap().moving);
    }

    #[test]
    fn transfer_state_roundtrip_restores_runtime_fields() {
        let mut state = RoomMovementState::new(1, 3);
        state.set_correction_config(4, 0.35, 16.0, true);
        state.set_movement_control_stop_frames(5);
        state.last_snapshot_frame = 7;
        state.last_full_sync_frame = 6;

        let spawn = build_spawn();
        let spawned = state.spawn_player("player-a", &spawn, 4.0);
        let dense_index = state.dense_index_by_player("player-a").unwrap();
        state.apply_command_at(
            dense_index,
            5,
            MovementCommand::MoveDir(Vec2 { x: 0.0, y: 1.0 }),
        );
        state.set_position_at(dense_index, Vec2 { x: 2.5, y: 3.5 });
        state.set_client_state_for_player(
            "player-a",
            ClientMovementState {
                frame_id: 5,
                position: Vec2 { x: 2.0, y: 3.0 },
            },
        );
        state.note_sent_to_player("player-a", 7);
        state.increment_missing_movement_control_frames("player-a");

        let exported = state.export_transfer_state_json().unwrap();
        let imported = RoomMovementState::import_transfer_state_json(&exported).unwrap();

        assert_eq!(imported.scene_id, 1);
        assert_eq!(imported.next_entity_id, spawned.entity_id + 1);
        assert_eq!(imported.last_snapshot_frame, 7);
        assert_eq!(imported.last_full_sync_frame, 6);
        assert_eq!(imported.correction_interval_frames, 4);
        assert_eq!(imported.correction_distance_threshold, 0.35);
        assert!(imported.aoi_enabled);
        assert_eq!(imported.aoi_radius, 16.0);
        assert_eq!(imported.movement_control_stop_frames, 5);

        let entity = imported.entity("player-a").unwrap();
        assert_eq!(entity.entity_id, spawned.entity_id);
        assert_eq!(entity.position.x, 2.5);
        assert_eq!(entity.position.y, 3.5);
        assert_eq!(entity.direction.x, 0.0);
        assert_eq!(entity.direction.y, 1.0);
        assert_eq!(entity.speed, 4.0);
        assert!(entity.moving);
        assert_eq!(entity.last_input_frame, 5);

        let client = imported.client_state_for_player("player-a").unwrap();
        assert_eq!(client.frame_id, 5);
        assert_eq!(client.x, 2.0);
        assert_eq!(client.y, 3.0);
        assert_eq!(imported.last_sent_frame_by_player.get("player-a"), Some(&7));
        assert_eq!(
            imported.missing_control_frames_by_player.get("player-a"),
            Some(&1)
        );
    }

    #[test]
    fn transfer_state_rejects_invalid_json_or_schema() {
        assert_eq!(
            RoomMovementState::import_transfer_state_json("{bad").unwrap_err(),
            "ROOM_TRANSFER_INVALID_MOVEMENT_STATE"
        );
        assert_eq!(
            RoomMovementState::import_transfer_state_json(
                r#"{"schema":"room-movement-state.v1","schemaVersion":2}"#
            )
            .unwrap_err(),
            "ROOM_TRANSFER_UNSUPPORTED_SCHEMA"
        );
    }

    #[test]
    fn transfer_state_rejects_negative_runtime_values() {
        let mut state = RoomMovementState::new(1, 3);
        let spawn = build_spawn();
        state.spawn_player("player-a", &spawn, 4.0);
        let exported = state.export_transfer_state_json().unwrap();

        let mut value = serde_json::from_str::<serde_json::Value>(&exported).unwrap();
        value["aoi_radius"] = serde_json::json!(-1.0);
        assert_eq!(
            RoomMovementState::import_transfer_state_json(&value.to_string()).unwrap_err(),
            "ROOM_TRANSFER_INVALID_MOVEMENT_STATE"
        );

        let mut value = serde_json::from_str::<serde_json::Value>(&exported).unwrap();
        value["entities"][0]["speed"] = serde_json::json!(-1.0);
        assert_eq!(
            RoomMovementState::import_transfer_state_json(&value.to_string()).unwrap_err(),
            "ROOM_TRANSFER_INVALID_MOVEMENT_STATE"
        );
    }
}
