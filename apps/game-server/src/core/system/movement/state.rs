use std::collections::HashMap;

use crate::core::system::movement::input::{ClientMovementState, MovementCommand};
use crate::core::system::scene::query::SceneSpawnPointDefinition;
use crate::pb::{
    EntityTransform, MovementCorrectionKind, MovementCorrectionReason, MovementRecoveryState,
};

pub type EntityId = u64;
pub type DenseIndex = usize;

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
        Some(distance(entity.position, Vec2 { x: client.x, y: client.y }))
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

    pub fn targets_for_player(&self, requester_player_id: &str) -> Vec<EntityTransform> {
        if !self.aoi_enabled {
            return self.all_transforms();
        }

        let Some(origin) = self.entity(requester_player_id).map(|entity| entity.position) else {
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
