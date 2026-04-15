use std::collections::HashMap;

use crate::core::system::movement::input::MovementCommand;
use crate::core::system::scene::query::SceneSpawnPointDefinition;
use crate::pb::EntityTransform;

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

#[derive(Debug, Clone)]
pub struct RoomMovementState {
    pub scene_id: i32,
    pub next_entity_id: EntityId,
    pub snapshot_interval_frames: u32,
    pub last_snapshot_frame: u32,
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
}

impl RoomMovementState {
    pub fn new(scene_id: i32, snapshot_interval_frames: u32) -> Self {
        Self {
            scene_id,
            next_entity_id: 1,
            snapshot_interval_frames,
            last_snapshot_frame: 0,
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
