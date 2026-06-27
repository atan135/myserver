use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::buffs::BuffEffectType;
use super::catalog::CombatCatalog;
use super::components::{
    BuffSlot, DamageFormula, EntityMeta, EntityType, Health, MoveState, MoveStateType, Position,
    SkillSlot, Stats,
};
use super::skills::{SkillDefinition, SkillEffect, SkillEffectType, SkillTargetType};

pub const MAX_ENTITIES: usize = 2048;
pub const MAX_SKILLS_PER_ENTITY: usize = 8;
pub const MAX_BUFFS_PER_ENTITY: usize = 6;
const ROOM_COMBAT_TRANSFER_SCHEMA: &str = "room-combat-ecs.v1";
const ROOM_COMBAT_TRANSFER_SCHEMA_VERSION: u32 = 1;
const ROOM_TRANSFER_INVALID_COMBAT_STATE: &str = "ROOM_TRANSFER_INVALID_COMBAT_STATE";
const ROOM_TRANSFER_UNSUPPORTED_SCHEMA: &str = "ROOM_TRANSFER_UNSUPPORTED_SCHEMA";
const COMBAT_SKILL_CODE_NOT_FOUND: &str = "COMBAT_SKILL_CODE_NOT_FOUND";

pub type EntityId = u32;
type DenseIndex = usize;

#[derive(Debug, Clone)]
pub struct CombatEntityBlueprint {
    pub entity_type: EntityType,
    pub character_id: Option<String>,
    pub team_id: u16,
    pub position: Position,
    pub facing: Position,
    pub health: Health,
    pub stats: Stats,
    pub skill_loadout: Vec<u16>,
}

impl CombatEntityBlueprint {
    pub fn player(character_id: &str, team_id: u16, position: Position) -> Self {
        Self {
            entity_type: EntityType::Player,
            character_id: Some(character_id.to_string()),
            team_id,
            position,
            facing: Position { x: 1.0, y: 0.0 },
            health: Health::new(120),
            stats: Stats {
                attack: 20,
                defense: 10,
                speed: 120,
                crit_rate_bps: 500,
                crit_damage_bps: 5_000,
            },
            skill_loadout: Vec::new(),
        }
    }

    pub fn monster(team_id: u16, position: Position) -> Self {
        Self {
            entity_type: EntityType::Monster,
            character_id: None,
            team_id,
            position,
            facing: Position { x: -1.0, y: 0.0 },
            health: Health::new(150),
            stats: Stats {
                attack: 12,
                defense: 8,
                speed: 90,
                crit_rate_bps: 0,
                crit_damage_bps: 5_000,
            },
            skill_loadout: vec![1, 5],
        }
    }

    pub fn with_health(mut self, health: Health) -> Self {
        self.health = health;
        self
    }

    pub fn with_stats(mut self, stats: Stats) -> Self {
        self.stats = stats;
        self
    }

    pub fn with_facing(mut self, facing: Position) -> Self {
        self.facing = facing;
        self
    }

    pub fn with_skills(mut self, skill_ids: &[u16]) -> Self {
        self.skill_loadout = skill_ids.to_vec();
        self
    }

    pub fn with_skill_codes(
        mut self,
        skill_codes: &[String],
        catalog: &dyn CombatCatalog,
    ) -> Result<Self, &'static str> {
        self.skill_loadout = resolve_skill_loadout_from_codes(skill_codes, catalog)?;
        Ok(self)
    }

    pub fn with_active_discipline_skill_pool(
        self,
        skill_codes: &[String],
        catalog: &dyn CombatCatalog,
    ) -> Result<Self, &'static str> {
        self.with_skill_codes(skill_codes, catalog)
    }
}

pub fn resolve_skill_loadout_from_codes(
    skill_codes: &[String],
    catalog: &dyn CombatCatalog,
) -> Result<Vec<u16>, &'static str> {
    let mut skill_ids = Vec::new();
    for code in skill_codes {
        let code = code.trim();
        if code.is_empty() {
            continue;
        }
        let Some(skill_id) = catalog.skill_id_by_code(code) else {
            return Err(COMBAT_SKILL_CODE_NOT_FOUND);
        };
        if !skill_ids.contains(&skill_id) {
            skill_ids.push(skill_id);
        }
        if skill_ids.len() >= MAX_SKILLS_PER_ENTITY {
            break;
        }
    }
    Ok(skill_ids)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCastRequest {
    pub frame_id: u32,
    pub source_entity: EntityId,
    pub skill_id: u16,
    pub target_entity: Option<EntityId>,
    pub target_point: Option<Position>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CombatCommand {
    CastSkill(SkillCastRequest),
    ApplyBuff {
        frame_id: u32,
        source_entity: Option<EntityId>,
        target_entity: EntityId,
        buff_id: u16,
        duration_frames: Option<u16>,
    },
    RemoveBuff {
        frame_id: u32,
        target_entity: EntityId,
        buff_id: u16,
    },
    SetPosition {
        frame_id: u32,
        entity_id: EntityId,
        position: Position,
    },
    Custom {
        frame_id: u32,
        source_entity: Option<EntityId>,
        name: String,
        payload_json: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CombatCommandResult {
    Accepted,
    Ignored,
    Rejected { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CombatEventKind {
    Spawned,
    Removed,
    SkillCast,
    Damage,
    Heal,
    BuffApplied,
    BuffExpired,
    Rejected,
    Defeated,
}

impl CombatEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spawned => "spawned",
            Self::Removed => "removed",
            Self::SkillCast => "skill_cast",
            Self::Damage => "damage",
            Self::Heal => "heal",
            Self::BuffApplied => "buff_applied",
            Self::BuffExpired => "buff_expired",
            Self::Rejected => "rejected",
            Self::Defeated => "defeated",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CombatEvent {
    pub frame_id: u32,
    pub kind: CombatEventKind,
    pub source_entity: Option<EntityId>,
    pub target_entity: Option<EntityId>,
    pub skill_id: Option<u16>,
    pub buff_id: Option<u16>,
    pub value: i32,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DamageContext {
    pub frame_id: u32,
    pub source_entity: Option<EntityId>,
    pub target_entity: EntityId,
    pub skill_id: Option<u16>,
    pub buff_id: Option<u16>,
    pub amount: i32,
    pub is_true_damage: bool,
    pub is_periodic: bool,
    pub was_critical: bool,
}

pub trait CombatHooks: Send {
    fn allow_cast(
        &mut self,
        _ecs: &RoomCombatEcs,
        _request: &SkillCastRequest,
        _skill: &SkillDefinition,
    ) -> Result<(), String> {
        Ok(())
    }

    fn modify_damage(&mut self, _context: &mut DamageContext) {}

    fn on_event(&mut self, _ecs: &RoomCombatEcs, _event: &CombatEvent) {}

    fn handle_custom_command(
        &mut self,
        _ecs: &mut RoomCombatEcs,
        _command: &CombatCommand,
        _catalog: &dyn CombatCatalog,
    ) -> CombatCommandResult {
        CombatCommandResult::Ignored
    }
}

#[derive(Debug, Default)]
pub struct NoopCombatHooks;

impl CombatHooks for NoopCombatHooks {}

#[derive(Debug, Clone, Serialize)]
pub struct CombatSkillSnapshot {
    pub skill_id: u16,
    pub cooldown_remaining: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct CombatBuffSnapshot {
    pub buff_id: u16,
    pub duration_remaining: u16,
    pub interval_remaining: u16,
    pub stacks: u8,
    pub source_entity: EntityId,
}

#[derive(Debug, Clone, Serialize)]
pub struct CombatEntitySnapshot {
    pub entity_id: EntityId,
    pub entity_type: EntityType,
    pub character_id: Option<String>,
    pub team_id: u16,
    pub alive: bool,
    pub x: f32,
    pub y: f32,
    pub hp: i32,
    pub max_hp: i32,
    pub base_stats: Stats,
    pub effective_stats: Stats,
    pub skills: Vec<CombatSkillSnapshot>,
    pub buffs: Vec<CombatBuffSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CombatSnapshot {
    pub frame_id: u32,
    pub entity_count: usize,
    pub entities: Vec<CombatEntitySnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoomCombatTransferSnapshot {
    schema: String,
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    next_entity_id: EntityId,
    last_tick_frame: u32,
    // pending_events are deliberately not transferred; they represent already
    // emitted side effects and replaying them after import would duplicate pushes.
    pending_events_replayed: bool,
    entities: Vec<EntityMeta>,
    positions_x: Vec<f32>,
    positions_y: Vec<f32>,
    directions_x: Vec<f32>,
    directions_y: Vec<f32>,
    healths: Vec<Health>,
    base_stats: Vec<Stats>,
    move_states: Vec<MoveState>,
    skill_slots: Vec<[SkillSlot; MAX_SKILLS_PER_ENTITY]>,
    buff_slots: Vec<[BuffSlot; MAX_BUFFS_PER_ENTITY]>,
    character_entity_map: Vec<RoomCombatTransferCharacterEntity>,
    pending_skill_requests: Vec<SkillCastRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RoomCombatTransferCharacterEntity {
    character_id: String,
    entity_id: EntityId,
}

#[derive(Debug, Default)]
pub struct RoomCombatEcs {
    next_entity_id: EntityId,
    entities: Vec<EntityMeta>,
    positions_x: Vec<f32>,
    positions_y: Vec<f32>,
    directions_x: Vec<f32>,
    directions_y: Vec<f32>,
    healths: Vec<Health>,
    base_stats: Vec<Stats>,
    move_states: Vec<MoveState>,
    skill_slots: Vec<[SkillSlot; MAX_SKILLS_PER_ENTITY]>,
    buff_slots: Vec<[BuffSlot; MAX_BUFFS_PER_ENTITY]>,
    character_entity_map: HashMap<String, EntityId>,
    entity_index_map: HashMap<EntityId, DenseIndex>,
    index_entity_map: Vec<EntityId>,
    pending_events: Vec<CombatEvent>,
    pending_skill_requests: Vec<SkillCastRequest>,
    last_tick_frame: u32,
}

impl RoomCombatEcs {
    pub fn new() -> Self {
        Self {
            next_entity_id: 1,
            entities: Vec::with_capacity(MAX_ENTITIES),
            positions_x: Vec::with_capacity(MAX_ENTITIES),
            positions_y: Vec::with_capacity(MAX_ENTITIES),
            directions_x: Vec::with_capacity(MAX_ENTITIES),
            directions_y: Vec::with_capacity(MAX_ENTITIES),
            healths: Vec::with_capacity(MAX_ENTITIES),
            base_stats: Vec::with_capacity(MAX_ENTITIES),
            move_states: Vec::with_capacity(MAX_ENTITIES),
            skill_slots: Vec::with_capacity(MAX_ENTITIES),
            buff_slots: Vec::with_capacity(MAX_ENTITIES),
            character_entity_map: HashMap::new(),
            entity_index_map: HashMap::new(),
            index_entity_map: Vec::with_capacity(MAX_ENTITIES),
            pending_events: Vec::new(),
            pending_skill_requests: Vec::new(),
            last_tick_frame: 0,
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    pub fn last_tick_frame(&self) -> u32 {
        self.last_tick_frame
    }

    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    pub fn entity_id_by_character(&self, character_id: &str) -> Option<EntityId> {
        self.character_entity_map.get(character_id).copied()
    }

    pub fn entity_character_id(&self, entity_id: EntityId) -> Option<&str> {
        let dense_index = self.dense_index(entity_id)?;
        self.entities.get(dense_index)?.character_id.as_deref()
    }

    pub fn entity_position(&self, entity_id: EntityId) -> Option<Position> {
        let dense_index = self.dense_index(entity_id)?;
        self.position_at_dense(dense_index)
    }

    pub fn spawn_entity(
        &mut self,
        blueprint: CombatEntityBlueprint,
    ) -> Result<EntityId, &'static str> {
        if self.entities.len() >= MAX_ENTITIES {
            return Err("COMBAT_ENTITY_LIMIT_REACHED");
        }

        if let Some(character_id) = blueprint.character_id.as_ref() {
            if self.character_entity_map.contains_key(character_id) {
                return Err("COMBAT_CHARACTER_ALREADY_SPAWNED");
            }
        }

        let entity_id = self.next_entity_id;
        self.next_entity_id = self.next_entity_id.saturating_add(1);

        let dense_index = self.entities.len();
        self.entities.push(EntityMeta {
            entity_id,
            entity_type: blueprint.entity_type,
            character_id: blueprint.character_id.clone(),
            team_id: blueprint.team_id,
            alive: blueprint.health.is_alive(),
        });
        self.positions_x.push(blueprint.position.x);
        self.positions_y.push(blueprint.position.y);
        self.directions_x.push(blueprint.facing.x);
        self.directions_y.push(blueprint.facing.y);
        self.healths.push(blueprint.health);
        self.base_stats.push(blueprint.stats);
        self.move_states.push(MoveState::idle());

        let mut skill_slots = [SkillSlot::empty(); MAX_SKILLS_PER_ENTITY];
        for (slot_index, skill_id) in blueprint
            .skill_loadout
            .iter()
            .copied()
            .take(MAX_SKILLS_PER_ENTITY)
            .enumerate()
        {
            skill_slots[slot_index] = SkillSlot {
                skill_id,
                cooldown_remaining: 0,
            };
        }
        self.skill_slots.push(skill_slots);
        self.buff_slots
            .push([BuffSlot::empty(); MAX_BUFFS_PER_ENTITY]);

        if let Some(character_id) = blueprint.character_id {
            self.character_entity_map.insert(character_id, entity_id);
        }
        self.entity_index_map.insert(entity_id, dense_index);
        self.index_entity_map.push(entity_id);

        self.pending_events.push(CombatEvent {
            frame_id: self.last_tick_frame,
            kind: CombatEventKind::Spawned,
            source_entity: Some(entity_id),
            target_entity: None,
            skill_id: None,
            buff_id: None,
            value: 0,
            x: Some(blueprint.position.x),
            y: Some(blueprint.position.y),
            detail: "spawn_entity".to_string(),
        });

        Ok(entity_id)
    }

    pub fn remove_entity(&mut self, entity_id: EntityId) -> bool {
        let Some(dense_index) = self.entity_index(entity_id) else {
            return false;
        };

        if let Some(character_id) = self.entities[dense_index].character_id.as_ref() {
            self.character_entity_map.remove(character_id);
        }

        self.entities.swap_remove(dense_index);
        self.positions_x.swap_remove(dense_index);
        self.positions_y.swap_remove(dense_index);
        self.directions_x.swap_remove(dense_index);
        self.directions_y.swap_remove(dense_index);
        self.healths.swap_remove(dense_index);
        self.base_stats.swap_remove(dense_index);
        self.move_states.swap_remove(dense_index);
        self.skill_slots.swap_remove(dense_index);
        self.buff_slots.swap_remove(dense_index);

        self.entity_index_map.remove(&entity_id);
        self.index_entity_map.swap_remove(dense_index);
        if dense_index < self.index_entity_map.len() {
            let swapped_entity_id = self.index_entity_map[dense_index];
            self.entity_index_map.insert(swapped_entity_id, dense_index);
        }

        self.pending_events.push(CombatEvent {
            frame_id: self.last_tick_frame,
            kind: CombatEventKind::Removed,
            source_entity: Some(entity_id),
            target_entity: None,
            skill_id: None,
            buff_id: None,
            value: 0,
            x: None,
            y: None,
            detail: "remove_entity".to_string(),
        });

        true
    }

    pub fn request_skill(&mut self, request: SkillCastRequest) {
        self.pending_skill_requests.push(request);
    }

    pub fn execute_command(
        &mut self,
        command: CombatCommand,
        catalog: &dyn CombatCatalog,
        hooks: &mut dyn CombatHooks,
    ) -> CombatCommandResult {
        match command {
            CombatCommand::CastSkill(request) => {
                if self.entity_index(request.source_entity).is_none() {
                    return CombatCommandResult::Rejected {
                        reason: "COMBAT_SOURCE_NOT_FOUND".to_string(),
                    };
                }
                self.request_skill(request);
                CombatCommandResult::Accepted
            }
            CombatCommand::ApplyBuff {
                frame_id,
                source_entity,
                target_entity,
                buff_id,
                duration_frames,
            } => match self.apply_buff(
                frame_id,
                source_entity,
                target_entity,
                buff_id,
                duration_frames,
                catalog,
                hooks,
            ) {
                Ok(()) => CombatCommandResult::Accepted,
                Err(reason) => CombatCommandResult::Rejected { reason },
            },
            CombatCommand::RemoveBuff {
                frame_id,
                target_entity,
                buff_id,
            } => {
                if self.remove_buff(frame_id, target_entity, buff_id, hooks) {
                    CombatCommandResult::Accepted
                } else {
                    CombatCommandResult::Rejected {
                        reason: "COMBAT_BUFF_NOT_FOUND".to_string(),
                    }
                }
            }
            CombatCommand::SetPosition {
                frame_id,
                entity_id,
                position,
            } => {
                if self.set_position(frame_id, entity_id, position) {
                    CombatCommandResult::Accepted
                } else {
                    CombatCommandResult::Rejected {
                        reason: "COMBAT_ENTITY_NOT_FOUND".to_string(),
                    }
                }
            }
            CombatCommand::Custom { .. } => hooks.handle_custom_command(self, &command, catalog),
        }
    }

    pub fn tick(
        &mut self,
        frame_id: u32,
        fps: u16,
        catalog: &dyn CombatCatalog,
        hooks: &mut dyn CombatHooks,
    ) {
        self.last_tick_frame = frame_id;
        self.tick_cooldowns();
        self.process_skill_requests(frame_id, fps, catalog, hooks);
        self.tick_movements(fps);
        self.tick_buffs(frame_id, catalog, hooks);
    }

    pub fn drain_events(&mut self) -> Vec<CombatEvent> {
        std::mem::take(&mut self.pending_events)
    }

    pub fn snapshot(&self, frame_id: u32, catalog: &dyn CombatCatalog) -> CombatSnapshot {
        let mut entities = Vec::with_capacity(self.entities.len());
        for dense_index in 0..self.entities.len() {
            let meta = &self.entities[dense_index];
            let position = self.position_at_dense(dense_index).unwrap_or_default();
            let health = self.healths[dense_index];
            let effective_stats = self.effective_stats_at_dense(dense_index, catalog);
            let skills = self.skill_slots[dense_index]
                .iter()
                .filter(|slot| slot.skill_id != 0)
                .map(|slot| CombatSkillSnapshot {
                    skill_id: slot.skill_id,
                    cooldown_remaining: slot.cooldown_remaining,
                })
                .collect();
            let buffs = self.buff_slots[dense_index]
                .iter()
                .filter(|slot| !slot.is_empty())
                .map(|slot| CombatBuffSnapshot {
                    buff_id: slot.buff_id,
                    duration_remaining: slot.duration_remaining,
                    interval_remaining: slot.interval_remaining,
                    stacks: slot.stacks,
                    source_entity: slot.source_entity,
                })
                .collect();

            entities.push(CombatEntitySnapshot {
                entity_id: meta.entity_id,
                entity_type: meta.entity_type,
                character_id: meta.character_id.clone(),
                team_id: meta.team_id,
                alive: meta.alive,
                x: position.x,
                y: position.y,
                hp: health.current,
                max_hp: health.max,
                base_stats: self.base_stats[dense_index],
                effective_stats,
                skills,
                buffs,
            });
        }

        CombatSnapshot {
            frame_id,
            entity_count: entities.len(),
            entities,
        }
    }

    pub fn export_transfer_state_json(&self) -> Result<String, &'static str> {
        let snapshot = RoomCombatTransferSnapshot::from_state(self)?;
        serde_json::to_string(&snapshot).map_err(|_| ROOM_TRANSFER_INVALID_COMBAT_STATE)
    }

    pub fn import_transfer_state_json(state_json: &str) -> Result<Self, &'static str> {
        let value = serde_json::from_str::<Value>(state_json)
            .map_err(|_| ROOM_TRANSFER_INVALID_COMBAT_STATE)?;
        if value.get("schema").and_then(Value::as_str) != Some(ROOM_COMBAT_TRANSFER_SCHEMA) {
            return Err(ROOM_TRANSFER_UNSUPPORTED_SCHEMA);
        }
        if value.get("schemaVersion").and_then(Value::as_u64)
            != Some(ROOM_COMBAT_TRANSFER_SCHEMA_VERSION as u64)
        {
            return Err(ROOM_TRANSFER_UNSUPPORTED_SCHEMA);
        }

        let snapshot = serde_json::from_value::<RoomCombatTransferSnapshot>(value)
            .map_err(|_| ROOM_TRANSFER_INVALID_COMBAT_STATE)?;
        snapshot.into_state()
    }

    fn entity_index(&self, entity_id: EntityId) -> Option<DenseIndex> {
        self.entity_index_map.get(&entity_id).copied()
    }

    fn dense_index(&self, entity_id: EntityId) -> Option<DenseIndex> {
        self.entity_index(entity_id)
    }

    fn position_at_dense(&self, dense_index: DenseIndex) -> Option<Position> {
        Some(Position {
            x: *self.positions_x.get(dense_index)?,
            y: *self.positions_y.get(dense_index)?,
        })
    }

    fn team_id(&self, entity_id: EntityId) -> Option<u16> {
        let dense_index = self.dense_index(entity_id)?;
        self.entities.get(dense_index).map(|meta| meta.team_id)
    }

    fn is_alive(&self, entity_id: EntityId) -> bool {
        let Some(dense_index) = self.dense_index(entity_id) else {
            return false;
        };
        self.entities
            .get(dense_index)
            .map(|meta| meta.alive)
            .unwrap_or(false)
    }

    fn set_position(&mut self, frame_id: u32, entity_id: EntityId, position: Position) -> bool {
        let Some(dense_index) = self.dense_index(entity_id) else {
            return false;
        };

        self.positions_x[dense_index] = position.x;
        self.positions_y[dense_index] = position.y;
        self.move_states[dense_index] = MoveState::idle();

        self.pending_events.push(CombatEvent {
            frame_id,
            kind: CombatEventKind::Spawned,
            source_entity: Some(entity_id),
            target_entity: None,
            skill_id: None,
            buff_id: None,
            value: 0,
            x: Some(position.x),
            y: Some(position.y),
            detail: "set_position".to_string(),
        });
        true
    }

    fn tick_cooldowns(&mut self) {
        for slots in &mut self.skill_slots {
            for slot in slots.iter_mut() {
                slot.tick();
            }
        }
    }
}

impl RoomCombatTransferSnapshot {
    fn from_state(state: &RoomCombatEcs) -> Result<Self, &'static str> {
        validate_parallel_lengths(state)?;
        if state.next_entity_id == 0 {
            return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
        }

        for dense_index in 0..state.entities.len() {
            validate_entity_meta(&state.entities[dense_index], state.next_entity_id)?;
            validate_finite(state.positions_x[dense_index])?;
            validate_finite(state.positions_y[dense_index])?;
            validate_finite(state.directions_x[dense_index])?;
            validate_finite(state.directions_y[dense_index])?;
            validate_health(state.healths[dense_index])?;
            validate_move_state(state.move_states[dense_index])?;
            validate_skill_slots(&state.skill_slots[dense_index])?;
            validate_buff_slots(&state.buff_slots[dense_index], state.next_entity_id)?;
        }

        let mut character_entity_map = state
            .character_entity_map
            .iter()
            .map(|(character_id, entity_id)| {
                validate_character_id(character_id)?;
                validate_entity_id(*entity_id, state.next_entity_id)?;
                Ok(RoomCombatTransferCharacterEntity {
                    character_id: character_id.clone(),
                    entity_id: *entity_id,
                })
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        character_entity_map.sort_by(|left, right| left.character_id.cmp(&right.character_id));

        for request in &state.pending_skill_requests {
            validate_skill_request(request, state.next_entity_id)?;
        }

        Ok(Self {
            schema: ROOM_COMBAT_TRANSFER_SCHEMA.to_string(),
            schema_version: ROOM_COMBAT_TRANSFER_SCHEMA_VERSION,
            next_entity_id: state.next_entity_id,
            last_tick_frame: state.last_tick_frame,
            pending_events_replayed: false,
            entities: state.entities.clone(),
            positions_x: state.positions_x.clone(),
            positions_y: state.positions_y.clone(),
            directions_x: state.directions_x.clone(),
            directions_y: state.directions_y.clone(),
            healths: state.healths.clone(),
            base_stats: state.base_stats.clone(),
            move_states: state.move_states.clone(),
            skill_slots: state.skill_slots.clone(),
            buff_slots: state.buff_slots.clone(),
            character_entity_map,
            pending_skill_requests: state.pending_skill_requests.clone(),
        })
    }

    fn into_state(self) -> Result<RoomCombatEcs, &'static str> {
        if self.schema != ROOM_COMBAT_TRANSFER_SCHEMA
            || self.schema_version != ROOM_COMBAT_TRANSFER_SCHEMA_VERSION
        {
            return Err(ROOM_TRANSFER_UNSUPPORTED_SCHEMA);
        }
        if self.next_entity_id == 0 || self.pending_events_replayed {
            return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
        }

        let entity_count = self.entities.len();
        validate_transfer_parallel_lengths(
            entity_count,
            &[
                self.positions_x.len(),
                self.positions_y.len(),
                self.directions_x.len(),
                self.directions_y.len(),
                self.healths.len(),
                self.base_stats.len(),
                self.move_states.len(),
                self.skill_slots.len(),
                self.buff_slots.len(),
            ],
        )?;
        if entity_count > MAX_ENTITIES {
            return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
        }

        let mut entity_index_map = HashMap::with_capacity(entity_count);
        let mut index_entity_map = Vec::with_capacity(entity_count);
        let mut seen_entity_ids = HashSet::with_capacity(entity_count);
        let mut derived_character_entity_map: HashMap<String, EntityId> = HashMap::new();

        for (dense_index, meta) in self.entities.iter().enumerate() {
            validate_entity_meta(meta, self.next_entity_id)?;
            if !seen_entity_ids.insert(meta.entity_id) {
                return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
            }
            entity_index_map.insert(meta.entity_id, dense_index);
            index_entity_map.push(meta.entity_id);

            if let Some(character_id) = meta.character_id.as_ref() {
                validate_character_id(character_id)?;
                if derived_character_entity_map
                    .insert(character_id.clone(), meta.entity_id)
                    .is_some()
                {
                    return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
                }
            }
        }

        let mut character_entity_map = HashMap::new();
        for entry in self.character_entity_map {
            validate_character_id(&entry.character_id)?;
            validate_entity_id(entry.entity_id, self.next_entity_id)?;
            if character_entity_map
                .insert(entry.character_id.clone(), entry.entity_id)
                .is_some()
            {
                return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
            }
            if derived_character_entity_map.get(&entry.character_id) != Some(&entry.entity_id) {
                return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
            }
        }
        if character_entity_map.len() != derived_character_entity_map.len() {
            return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
        }

        for dense_index in 0..entity_count {
            validate_finite(self.positions_x[dense_index])?;
            validate_finite(self.positions_y[dense_index])?;
            validate_finite(self.directions_x[dense_index])?;
            validate_finite(self.directions_y[dense_index])?;
            validate_health(self.healths[dense_index])?;
            validate_move_state(self.move_states[dense_index])?;
            validate_skill_slots(&self.skill_slots[dense_index])?;
            validate_buff_slots(&self.buff_slots[dense_index], self.next_entity_id)?;
        }

        for request in &self.pending_skill_requests {
            validate_skill_request(request, self.next_entity_id)?;
            if !seen_entity_ids.contains(&request.source_entity)
                || request
                    .target_entity
                    .is_some_and(|entity_id| !seen_entity_ids.contains(&entity_id))
            {
                return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
            }
        }

        Ok(RoomCombatEcs {
            next_entity_id: self.next_entity_id,
            entities: self.entities,
            positions_x: self.positions_x,
            positions_y: self.positions_y,
            directions_x: self.directions_x,
            directions_y: self.directions_y,
            healths: self.healths,
            base_stats: self.base_stats,
            move_states: self.move_states,
            skill_slots: self.skill_slots,
            buff_slots: self.buff_slots,
            character_entity_map,
            entity_index_map,
            index_entity_map,
            pending_events: Vec::new(),
            pending_skill_requests: self.pending_skill_requests,
            last_tick_frame: self.last_tick_frame,
        })
    }
}

fn validate_parallel_lengths(state: &RoomCombatEcs) -> Result<(), &'static str> {
    validate_transfer_parallel_lengths(
        state.entities.len(),
        &[
            state.positions_x.len(),
            state.positions_y.len(),
            state.directions_x.len(),
            state.directions_y.len(),
            state.healths.len(),
            state.base_stats.len(),
            state.move_states.len(),
            state.skill_slots.len(),
            state.buff_slots.len(),
            state.index_entity_map.len(),
        ],
    )
}

fn validate_transfer_parallel_lengths(
    expected: usize,
    lengths: &[usize],
) -> Result<(), &'static str> {
    if lengths.iter().any(|length| *length != expected) {
        return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
    }
    Ok(())
}

fn validate_entity_meta(meta: &EntityMeta, next_entity_id: EntityId) -> Result<(), &'static str> {
    validate_entity_id(meta.entity_id, next_entity_id)?;
    if let Some(character_id) = meta.character_id.as_ref() {
        validate_character_id(character_id)?;
    }
    Ok(())
}

fn validate_entity_id(entity_id: EntityId, next_entity_id: EntityId) -> Result<(), &'static str> {
    if entity_id == 0 || entity_id >= next_entity_id {
        return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
    }
    Ok(())
}

fn validate_character_id(character_id: &str) -> Result<(), &'static str> {
    if character_id.trim().is_empty() {
        return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
    }
    Ok(())
}

fn validate_health(health: Health) -> Result<(), &'static str> {
    if health.max < 0
        || health.base_max < 0
        || health.current < 0
        || health.current > health.max
        || health.max > health.base_max
    {
        return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
    }
    Ok(())
}

fn validate_move_state(move_state: MoveState) -> Result<(), &'static str> {
    validate_finite(move_state.start_x)?;
    validate_finite(move_state.start_y)?;
    validate_finite(move_state.target_x)?;
    validate_finite(move_state.target_y)?;
    validate_finite(move_state.progress)?;
    validate_non_negative_finite(move_state.speed)?;
    if !(0.0..=1.0).contains(&move_state.progress) {
        return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
    }
    Ok(())
}

fn validate_skill_slots(
    skill_slots: &[SkillSlot; MAX_SKILLS_PER_ENTITY],
) -> Result<(), &'static str> {
    let mut seen_skill_ids = HashSet::new();
    for slot in skill_slots {
        if slot.skill_id == 0 {
            if slot.cooldown_remaining != 0 {
                return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
            }
            continue;
        }
        if !seen_skill_ids.insert(slot.skill_id) {
            return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
        }
    }
    Ok(())
}

fn validate_buff_slots(
    buff_slots: &[BuffSlot; MAX_BUFFS_PER_ENTITY],
    next_entity_id: EntityId,
) -> Result<(), &'static str> {
    let mut seen_buff_ids = HashSet::new();
    for slot in buff_slots {
        if slot.is_empty() {
            if *slot != BuffSlot::empty() {
                return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
            }
            continue;
        }
        if slot.buff_id == 0
            || slot.duration_remaining == 0
            || slot.stacks == 0
            || !is_valid_buff_source(slot.source_entity, next_entity_id)
            || !seen_buff_ids.insert(slot.buff_id)
        {
            return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
        }
    }
    Ok(())
}

fn is_valid_buff_source(source_entity: EntityId, next_entity_id: EntityId) -> bool {
    source_entity == 0 || validate_entity_id(source_entity, next_entity_id).is_ok()
}

fn validate_skill_request(
    request: &SkillCastRequest,
    next_entity_id: EntityId,
) -> Result<(), &'static str> {
    validate_entity_id(request.source_entity, next_entity_id)?;
    if request.skill_id == 0 {
        return Err(ROOM_TRANSFER_INVALID_COMBAT_STATE);
    }
    if let Some(target_entity) = request.target_entity {
        validate_entity_id(target_entity, next_entity_id)?;
    }
    if let Some(target_point) = request.target_point {
        validate_finite(target_point.x)?;
        validate_finite(target_point.y)?;
    }
    Ok(())
}

fn validate_finite(value: f32) -> Result<(), &'static str> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(ROOM_TRANSFER_INVALID_COMBAT_STATE)
    }
}

fn validate_non_negative_finite(value: f32) -> Result<(), &'static str> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(ROOM_TRANSFER_INVALID_COMBAT_STATE)
    }
}

impl RoomCombatEcs {
    fn process_skill_requests(
        &mut self,
        frame_id: u32,
        fps: u16,
        catalog: &dyn CombatCatalog,
        hooks: &mut dyn CombatHooks,
    ) {
        let requests = std::mem::take(&mut self.pending_skill_requests);
        for request in requests {
            let Some(skill) = catalog.skill_definition(request.skill_id) else {
                self.emit_reject(
                    frame_id,
                    Some(request.source_entity),
                    request.target_entity,
                    "COMBAT_SKILL_UNKNOWN",
                    hooks,
                );
                continue;
            };

            if !self.is_alive(request.source_entity) {
                self.emit_reject(
                    frame_id,
                    Some(request.source_entity),
                    request.target_entity,
                    "COMBAT_SOURCE_DEAD",
                    hooks,
                );
                continue;
            }

            let Some(source_dense_index) = self.dense_index(request.source_entity) else {
                self.emit_reject(
                    frame_id,
                    Some(request.source_entity),
                    request.target_entity,
                    "COMBAT_SOURCE_NOT_FOUND",
                    hooks,
                );
                continue;
            };
            let Some(slot_index) = self.skill_slots[source_dense_index]
                .iter()
                .position(|slot| slot.skill_id == request.skill_id)
            else {
                self.emit_reject(
                    frame_id,
                    Some(request.source_entity),
                    request.target_entity,
                    "COMBAT_SKILL_NOT_EQUIPPED",
                    hooks,
                );
                continue;
            };

            if self.skill_slots[source_dense_index][slot_index].cooldown_remaining > 0 {
                self.emit_reject(
                    frame_id,
                    Some(request.source_entity),
                    request.target_entity,
                    "COMBAT_SKILL_ON_COOLDOWN",
                    hooks,
                );
                continue;
            }

            if let Err(reason) = hooks.allow_cast(self, &request, skill) {
                self.emit_reject(
                    frame_id,
                    Some(request.source_entity),
                    request.target_entity,
                    &reason,
                    hooks,
                );
                continue;
            }

            let Some(source_position) = self.entity_position(request.source_entity) else {
                self.emit_reject(
                    frame_id,
                    Some(request.source_entity),
                    request.target_entity,
                    "COMBAT_SOURCE_POSITION_MISSING",
                    hooks,
                );
                continue;
            };

            let Some(anchor) = self.resolve_anchor(
                skill.target_type,
                request.source_entity,
                request.target_entity,
                request.target_point,
            ) else {
                self.emit_reject(
                    frame_id,
                    Some(request.source_entity),
                    request.target_entity,
                    "COMBAT_TARGET_INVALID",
                    hooks,
                );
                continue;
            };

            if source_position.distance(anchor) > skill.range {
                self.emit_reject(
                    frame_id,
                    Some(request.source_entity),
                    request.target_entity,
                    "COMBAT_TARGET_OUT_OF_RANGE",
                    hooks,
                );
                continue;
            }

            self.skill_slots[source_dense_index][slot_index].cooldown_remaining =
                skill.cooldown_frames;
            self.emit_event(
                CombatEvent {
                    frame_id,
                    kind: CombatEventKind::SkillCast,
                    source_entity: Some(request.source_entity),
                    target_entity: request.target_entity,
                    skill_id: Some(skill.id),
                    buff_id: None,
                    value: 0,
                    x: Some(anchor.x),
                    y: Some(anchor.y),
                    detail: skill.name.to_string(),
                },
                hooks,
            );

            for effect in skill.effects.iter() {
                let targets = self.collect_effect_targets(
                    request.source_entity,
                    request.target_entity,
                    anchor,
                    skill.target_type,
                    effect,
                );

                match effect.effect_type {
                    SkillEffectType::Damage => {
                        for target_entity in targets {
                            self.apply_damage_from_formula(
                                frame_id,
                                Some(request.source_entity),
                                target_entity,
                                Some(skill.id),
                                None,
                                effect.formula,
                                false,
                                hooks,
                                catalog,
                            );
                        }
                    }
                    SkillEffectType::Heal => {
                        for target_entity in targets {
                            self.apply_heal(
                                frame_id,
                                Some(request.source_entity),
                                target_entity,
                                Some(skill.id),
                                None,
                                effect.value,
                                hooks,
                            );
                        }
                    }
                    SkillEffectType::ApplyBuff => {
                        for target_entity in targets {
                            let _ = self.apply_buff(
                                frame_id,
                                Some(request.source_entity),
                                target_entity,
                                effect.buff_id,
                                Some(effect.buff_duration),
                                catalog,
                                hooks,
                            );
                        }
                    }
                    SkillEffectType::Knockback => {
                        for target_entity in targets {
                            self.apply_knockback(
                                frame_id,
                                fps,
                                request.source_entity,
                                target_entity,
                                effect.displacement_distance,
                            );
                        }
                    }
                    SkillEffectType::Custom => {
                        self.emit_event(
                            CombatEvent {
                                frame_id,
                                kind: CombatEventKind::Rejected,
                                source_entity: Some(request.source_entity),
                                target_entity: request.target_entity,
                                skill_id: Some(skill.id),
                                buff_id: None,
                                value: 0,
                                x: Some(anchor.x),
                                y: Some(anchor.y),
                                detail: "custom_skill_effect_not_bound".to_string(),
                            },
                            hooks,
                        );
                    }
                }
            }
        }
    }

    fn resolve_anchor(
        &self,
        target_type: SkillTargetType,
        source_entity: EntityId,
        target_entity: Option<EntityId>,
        target_point: Option<Position>,
    ) -> Option<Position> {
        if matches!(target_type, SkillTargetType::SelfOnly) {
            return self.entity_position(source_entity);
        }

        if let Some(target_entity) = target_entity {
            return self.entity_position(target_entity);
        }

        target_point.or_else(|| self.entity_position(source_entity))
    }

    fn collect_effect_targets(
        &self,
        source_entity: EntityId,
        requested_target: Option<EntityId>,
        anchor: Position,
        target_type: SkillTargetType,
        effect: &SkillEffect,
    ) -> Vec<EntityId> {
        if matches!(target_type, SkillTargetType::SelfOnly) {
            return vec![source_entity];
        }

        if effect.aoe_radius <= 0.0 {
            return requested_target
                .filter(|target_entity| {
                    self.is_valid_target(source_entity, *target_entity, target_type)
                })
                .into_iter()
                .collect();
        }

        self.entities
            .iter()
            .filter(|meta| meta.alive)
            .filter(|meta| self.is_valid_target(source_entity, meta.entity_id, target_type))
            .filter_map(|meta| {
                let position = self.entity_position(meta.entity_id)?;
                (position.distance(anchor) <= effect.aoe_radius).then_some(meta.entity_id)
            })
            .collect()
    }

    fn is_valid_target(
        &self,
        source_entity: EntityId,
        candidate_entity: EntityId,
        target_type: SkillTargetType,
    ) -> bool {
        if source_entity == candidate_entity {
            return matches!(
                target_type,
                SkillTargetType::SelfOnly | SkillTargetType::Ally
            );
        }

        let Some(source_team_id) = self.team_id(source_entity) else {
            return false;
        };
        let Some(candidate_team_id) = self.team_id(candidate_entity) else {
            return false;
        };

        match target_type {
            SkillTargetType::Enemy | SkillTargetType::Ground => source_team_id != candidate_team_id,
            SkillTargetType::Ally => source_team_id == candidate_team_id,
            SkillTargetType::SelfOnly => false,
        }
    }

    fn tick_movements(&mut self, fps: u16) {
        let fps = fps.max(1) as f32;
        for dense_index in 0..self.move_states.len() {
            if !self.move_states[dense_index].is_active() {
                continue;
            }

            let state = &mut self.move_states[dense_index];
            let total_dist = Position {
                x: state.start_x,
                y: state.start_y,
            }
            .distance(Position {
                x: state.target_x,
                y: state.target_y,
            });
            if total_dist <= f32::EPSILON {
                *state = MoveState::idle();
                continue;
            }

            let progress_delta = (state.speed / fps / total_dist).max(0.01);
            state.progress = (state.progress + progress_delta).min(1.0);
            let position = state.current_position();
            self.positions_x[dense_index] = position.x;
            self.positions_y[dense_index] = position.y;

            if state.progress >= 1.0 {
                *state = MoveState::idle();
            }
        }
    }

    fn tick_buffs(
        &mut self,
        frame_id: u32,
        catalog: &dyn CombatCatalog,
        hooks: &mut dyn CombatHooks,
    ) {
        for dense_index in 0..self.entities.len() {
            if !self.entities[dense_index].alive {
                continue;
            }

            let entity_id = self.entities[dense_index].entity_id;
            for slot_index in 0..MAX_BUFFS_PER_ENTITY {
                let mut periodic_trigger = None;
                let mut expired = None;

                {
                    let slot = &mut self.buff_slots[dense_index][slot_index];
                    if slot.is_empty() {
                        continue;
                    }

                    let buff_id = slot.buff_id;
                    let source_entity = slot.source_entity;
                    let stacks = slot.stacks;

                    if slot.duration_remaining > 0 {
                        slot.duration_remaining -= 1;
                    }

                    if let Some(buff) = catalog.buff_definition(buff_id) {
                        if buff.interval_frames > 0 {
                            if slot.interval_remaining > 0 {
                                slot.interval_remaining -= 1;
                            }
                            if slot.interval_remaining == 0 {
                                slot.interval_remaining = buff.interval_frames;
                                periodic_trigger = Some((buff_id, source_entity, stacks));
                            }
                        }
                    }

                    if slot.duration_remaining == 0 {
                        slot.clear();
                        expired = Some(buff_id);
                    }
                }

                if let Some((buff_id, source_entity, stacks)) = periodic_trigger {
                    self.apply_periodic_buff(
                        frame_id,
                        source_entity,
                        entity_id,
                        buff_id,
                        stacks,
                        catalog,
                        hooks,
                    );
                }

                if let Some(buff_id) = expired {
                    self.emit_event(
                        CombatEvent {
                            frame_id,
                            kind: CombatEventKind::BuffExpired,
                            source_entity: Some(entity_id),
                            target_entity: Some(entity_id),
                            skill_id: None,
                            buff_id: Some(buff_id),
                            value: 0,
                            x: None,
                            y: None,
                            detail: "buff_expired".to_string(),
                        },
                        hooks,
                    );
                }
            }
        }
    }
}

impl RoomCombatEcs {
    fn apply_periodic_buff(
        &mut self,
        frame_id: u32,
        source_entity: EntityId,
        target_entity: EntityId,
        buff_id: u16,
        stacks: u8,
        catalog: &dyn CombatCatalog,
        hooks: &mut dyn CombatHooks,
    ) {
        let Some(buff_definition) = catalog.buff_definition(buff_id) else {
            return;
        };

        for effect in buff_definition.effects.iter() {
            match effect.effect_type {
                BuffEffectType::DamagePeriodic => {
                    let formula = match effect.formula {
                        DamageFormula::Fixed(value) => {
                            DamageFormula::Fixed(value.saturating_mul(i32::from(stacks)))
                        }
                        other => other,
                    };
                    self.apply_damage_from_formula(
                        frame_id,
                        Some(source_entity),
                        target_entity,
                        None,
                        Some(buff_id),
                        formula,
                        true,
                        hooks,
                        catalog,
                    );
                }
                BuffEffectType::HealPeriodic => {
                    self.apply_heal(
                        frame_id,
                        Some(source_entity),
                        target_entity,
                        None,
                        Some(buff_id),
                        effect.value.saturating_mul(i32::from(stacks)),
                        hooks,
                    );
                }
                BuffEffectType::ModifyAttack
                | BuffEffectType::ModifyDefense
                | BuffEffectType::ModifySpeed
                | BuffEffectType::Custom => {}
            }
        }
    }

    fn apply_damage_from_formula(
        &mut self,
        frame_id: u32,
        source_entity: Option<EntityId>,
        target_entity: EntityId,
        skill_id: Option<u16>,
        buff_id: Option<u16>,
        formula: DamageFormula,
        is_periodic: bool,
        hooks: &mut dyn CombatHooks,
        catalog: &dyn CombatCatalog,
    ) {
        let Some(target_dense_index) = self.dense_index(target_entity) else {
            return;
        };
        if !self.entities[target_dense_index].alive {
            return;
        }

        let source_stats = source_entity
            .and_then(|entity_id| self.dense_index(entity_id))
            .map(|dense_index| self.effective_stats_at_dense(dense_index, catalog))
            .unwrap_or_default();

        let mut amount = match formula {
            DamageFormula::Fixed(value) => value.max(0),
            DamageFormula::Scaling {
                base,
                attack_scale_bps,
            } => base.saturating_add(
                source_stats
                    .attack
                    .saturating_mul(i32::from(attack_scale_bps))
                    / 10_000,
            ),
            DamageFormula::TrueDamage(value) => value.max(0),
        };
        let mut is_true_damage = matches!(formula, DamageFormula::TrueDamage(_));
        let mut was_critical = false;

        if !is_periodic && !is_true_damage {
            let roll_seed = u32::from(source_stats.crit_rate_bps)
                ^ frame_id
                ^ source_entity.unwrap_or_default()
                ^ target_entity
                ^ u32::from(skill_id.unwrap_or_default());
            if roll_seed % 10_000 < u32::from(source_stats.crit_rate_bps) {
                amount = amount.saturating_mul(10_000 + i32::from(source_stats.crit_damage_bps))
                    / 10_000;
                was_critical = true;
            }
        }

        let mut context = DamageContext {
            frame_id,
            source_entity,
            target_entity,
            skill_id,
            buff_id,
            amount,
            is_true_damage,
            is_periodic,
            was_critical,
        };
        hooks.modify_damage(&mut context);
        amount = context.amount.max(0);
        is_true_damage = context.is_true_damage;

        if !is_true_damage {
            let effective_stats = self.effective_stats_at_dense(target_dense_index, catalog);
            let defense = effective_stats.defense.max(0) as f32;
            let reduction = defense / (defense + 200.0);
            amount = ((amount as f32) * (1.0 - reduction)).round() as i32;
        }

        let applied = self.healths[target_dense_index].take_damage(amount);
        if applied <= 0 {
            return;
        }

        self.emit_event(
            CombatEvent {
                frame_id,
                kind: CombatEventKind::Damage,
                source_entity,
                target_entity: Some(target_entity),
                skill_id,
                buff_id,
                value: applied,
                x: None,
                y: None,
                detail: if context.was_critical {
                    "critical".to_string()
                } else if is_periodic {
                    "periodic".to_string()
                } else {
                    "damage".to_string()
                },
            },
            hooks,
        );

        if !self.healths[target_dense_index].is_alive() {
            self.entities[target_dense_index].alive = false;
            self.emit_event(
                CombatEvent {
                    frame_id,
                    kind: CombatEventKind::Defeated,
                    source_entity,
                    target_entity: Some(target_entity),
                    skill_id,
                    buff_id,
                    value: 0,
                    x: None,
                    y: None,
                    detail: "defeated".to_string(),
                },
                hooks,
            );
        }
    }

    fn apply_heal(
        &mut self,
        frame_id: u32,
        source_entity: Option<EntityId>,
        target_entity: EntityId,
        skill_id: Option<u16>,
        buff_id: Option<u16>,
        amount: i32,
        hooks: &mut dyn CombatHooks,
    ) {
        let Some(target_dense_index) = self.dense_index(target_entity) else {
            return;
        };
        if !self.entities[target_dense_index].alive {
            return;
        }

        let applied = self.healths[target_dense_index].heal(amount);
        if applied <= 0 {
            return;
        }

        self.emit_event(
            CombatEvent {
                frame_id,
                kind: CombatEventKind::Heal,
                source_entity,
                target_entity: Some(target_entity),
                skill_id,
                buff_id,
                value: applied,
                x: None,
                y: None,
                detail: "heal".to_string(),
            },
            hooks,
        );
    }

    fn apply_buff(
        &mut self,
        frame_id: u32,
        source_entity: Option<EntityId>,
        target_entity: EntityId,
        buff_id: u16,
        duration_frames: Option<u16>,
        catalog: &dyn CombatCatalog,
        hooks: &mut dyn CombatHooks,
    ) -> Result<(), String> {
        let Some(buff_definition) = catalog.buff_definition(buff_id) else {
            return Err("COMBAT_BUFF_UNKNOWN".to_string());
        };
        let Some(target_dense_index) = self.dense_index(target_entity) else {
            return Err("COMBAT_TARGET_NOT_FOUND".to_string());
        };
        if !self.entities[target_dense_index].alive {
            return Err("COMBAT_TARGET_DEAD".to_string());
        }

        let duration = duration_frames
            .unwrap_or(buff_definition.duration_frames)
            .max(1);
        let source_entity = source_entity.unwrap_or_default();

        if let Some(slot_index) = self.buff_slots[target_dense_index]
            .iter()
            .position(|slot| slot.buff_id == buff_id && !slot.is_empty())
        {
            let stacks = {
                let existing_slot = &mut self.buff_slots[target_dense_index][slot_index];
                existing_slot.duration_remaining = duration;
                existing_slot.interval_remaining = buff_definition.interval_frames;
                existing_slot.stacks = existing_slot
                    .stacks
                    .saturating_add(1)
                    .min(buff_definition.max_stacks.max(1));
                existing_slot.source_entity = source_entity;
                existing_slot.stacks
            };

            self.emit_event(
                CombatEvent {
                    frame_id,
                    kind: CombatEventKind::BuffApplied,
                    source_entity: Some(source_entity),
                    target_entity: Some(target_entity),
                    skill_id: None,
                    buff_id: Some(buff_id),
                    value: i32::from(stacks),
                    x: None,
                    y: None,
                    detail: buff_definition.name.to_string(),
                },
                hooks,
            );
            return Ok(());
        }

        let Some(empty_slot_index) = self.buff_slots[target_dense_index]
            .iter()
            .position(|slot| slot.is_empty())
        else {
            return Err("COMBAT_BUFF_SLOT_FULL".to_string());
        };

        self.buff_slots[target_dense_index][empty_slot_index] = BuffSlot {
            buff_id,
            duration_remaining: duration,
            interval_remaining: buff_definition.interval_frames,
            stacks: 1,
            source_entity,
        };

        self.emit_event(
            CombatEvent {
                frame_id,
                kind: CombatEventKind::BuffApplied,
                source_entity: Some(source_entity),
                target_entity: Some(target_entity),
                skill_id: None,
                buff_id: Some(buff_id),
                value: 1,
                x: None,
                y: None,
                detail: buff_definition.name.to_string(),
            },
            hooks,
        );

        Ok(())
    }

    fn remove_buff(
        &mut self,
        frame_id: u32,
        target_entity: EntityId,
        buff_id: u16,
        hooks: &mut dyn CombatHooks,
    ) -> bool {
        let Some(target_dense_index) = self.dense_index(target_entity) else {
            return false;
        };

        let Some(slot) = self.buff_slots[target_dense_index]
            .iter_mut()
            .find(|slot| slot.buff_id == buff_id && !slot.is_empty())
        else {
            return false;
        };

        slot.clear();
        self.emit_event(
            CombatEvent {
                frame_id,
                kind: CombatEventKind::BuffExpired,
                source_entity: Some(target_entity),
                target_entity: Some(target_entity),
                skill_id: None,
                buff_id: Some(buff_id),
                value: 0,
                x: None,
                y: None,
                detail: "remove_buff".to_string(),
            },
            hooks,
        );
        true
    }

    fn apply_knockback(
        &mut self,
        frame_id: u32,
        fps: u16,
        source_entity: EntityId,
        target_entity: EntityId,
        distance: f32,
    ) {
        let Some(target_dense_index) = self.dense_index(target_entity) else {
            return;
        };
        let Some(source_position) = self.entity_position(source_entity) else {
            return;
        };
        let Some(target_position) = self.entity_position(target_entity) else {
            return;
        };

        let direction = source_position.direction_to(target_position);
        let target = Position {
            x: target_position.x + direction.x * distance,
            y: target_position.y + direction.y * distance,
        };
        let speed = distance.max(1.0) * f32::from(fps.max(1)) / 4.0;
        self.move_states[target_dense_index] = MoveState {
            state_type: MoveStateType::Knockback,
            start_x: target_position.x,
            start_y: target_position.y,
            target_x: target.x,
            target_y: target.y,
            progress: 0.0,
            speed,
        };

        self.pending_events.push(CombatEvent {
            frame_id,
            kind: CombatEventKind::Rejected,
            source_entity: Some(source_entity),
            target_entity: Some(target_entity),
            skill_id: None,
            buff_id: None,
            value: 0,
            x: Some(target.x),
            y: Some(target.y),
            detail: "knockback_applied".to_string(),
        });
    }

    fn effective_stats_at_dense(
        &self,
        dense_index: DenseIndex,
        catalog: &dyn CombatCatalog,
    ) -> Stats {
        let mut effective = self.base_stats[dense_index];
        for slot in self.buff_slots[dense_index]
            .iter()
            .filter(|slot| !slot.is_empty())
        {
            let Some(definition) = catalog.buff_definition(slot.buff_id) else {
                continue;
            };
            let stack_count = i32::from(slot.stacks.max(1));
            for effect in definition.effects.iter() {
                match effect.effect_type {
                    BuffEffectType::ModifyAttack => {
                        effective.attack = effective
                            .attack
                            .saturating_add(effect.value.saturating_mul(stack_count));
                    }
                    BuffEffectType::ModifyDefense => {
                        effective.defense = effective
                            .defense
                            .saturating_add(effect.value.saturating_mul(stack_count));
                    }
                    BuffEffectType::ModifySpeed => {
                        effective.speed = effective
                            .speed
                            .saturating_add(effect.value.saturating_mul(stack_count));
                    }
                    BuffEffectType::DamagePeriodic
                    | BuffEffectType::HealPeriodic
                    | BuffEffectType::Custom => {}
                }
            }
        }
        effective
    }

    fn emit_reject(
        &mut self,
        frame_id: u32,
        source_entity: Option<EntityId>,
        target_entity: Option<EntityId>,
        reason: &str,
        hooks: &mut dyn CombatHooks,
    ) {
        self.emit_event(
            CombatEvent {
                frame_id,
                kind: CombatEventKind::Rejected,
                source_entity,
                target_entity,
                skill_id: None,
                buff_id: None,
                value: 0,
                x: None,
                y: None,
                detail: reason.to_string(),
            },
            hooks,
        );
    }

    fn emit_event(&mut self, event: CombatEvent, hooks: &mut dyn CombatHooks) {
        self.pending_events.push(event.clone());
        hooks.on_event(self, &event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::system::combat::BuiltinCombatCatalog;
    use serde_json::json;

    fn transfer_sample_ecs() -> RoomCombatEcs {
        let catalog = BuiltinCombatCatalog::default();
        let mut ecs = RoomCombatEcs::new();
        let player_a = ecs
            .spawn_entity(
                CombatEntityBlueprint::player("player-a", 1, Position { x: 0.0, y: 0.0 })
                    .with_skills(&[2, 4]),
            )
            .unwrap();
        let player_b = ecs
            .spawn_entity(
                CombatEntityBlueprint::player("player-b", 2, Position { x: 20.0, y: 0.0 })
                    .with_skills(&[1]),
            )
            .unwrap();
        let mut hooks = NoopCombatHooks;

        ecs.execute_command(
            CombatCommand::CastSkill(SkillCastRequest {
                frame_id: 1,
                source_entity: player_a,
                skill_id: 4,
                target_entity: Some(player_b),
                target_point: None,
            }),
            &catalog,
            &mut hooks,
        );
        ecs.tick(1, 20, &catalog, &mut hooks);
        ecs.drain_events();

        assert_eq!(
            ecs.execute_command(
                CombatCommand::ApplyBuff {
                    frame_id: 2,
                    source_entity: Some(player_a),
                    target_entity: player_b,
                    buff_id: 2,
                    duration_frames: Some(77),
                },
                &catalog,
                &mut hooks,
            ),
            CombatCommandResult::Accepted
        );
        ecs.tick(2, 20, &catalog, &mut hooks);
        ecs.request_skill(SkillCastRequest {
            frame_id: 3,
            source_entity: player_a,
            skill_id: 2,
            target_entity: Some(player_b),
            target_point: None,
        });

        ecs
    }

    fn transfer_sample_value() -> serde_json::Value {
        serde_json::from_str(
            &transfer_sample_ecs()
                .export_transfer_state_json()
                .expect("sample combat transfer state should export"),
        )
        .expect("sample combat transfer state should be json")
    }

    #[test]
    fn skill_cast_applies_damage_and_starts_cooldown() {
        let catalog = BuiltinCombatCatalog::default();
        let mut ecs = RoomCombatEcs::new();
        let player_a = ecs
            .spawn_entity(
                CombatEntityBlueprint::player("player-a", 1, Position { x: 0.0, y: 0.0 })
                    .with_skills(&[1]),
            )
            .unwrap();
        let player_b = ecs
            .spawn_entity(CombatEntityBlueprint::player(
                "player-b",
                2,
                Position { x: 10.0, y: 0.0 },
            ))
            .unwrap();

        ecs.request_skill(SkillCastRequest {
            frame_id: 1,
            source_entity: player_a,
            skill_id: 1,
            target_entity: Some(player_b),
            target_point: None,
        });

        let mut hooks = NoopCombatHooks;
        ecs.tick(1, 30, &catalog, &mut hooks);

        let snapshot = ecs.snapshot(1, &catalog);
        let target = snapshot
            .entities
            .into_iter()
            .find(|entity| entity.entity_id == player_b)
            .unwrap();
        assert!(target.hp < target.max_hp);
        let caster = ecs.dense_index(player_a).unwrap();
        assert_eq!(ecs.skill_slots[caster][0].cooldown_remaining, 30);
    }

    #[test]
    fn blueprint_skill_codes_resolve_to_combat_skill_loadout() {
        let catalog = BuiltinCombatCatalog::default();
        let blueprint = CombatEntityBlueprint::player("player-a", 1, Position { x: 0.0, y: 0.0 })
            .with_active_discipline_skill_pool(
                &["basic_attack".to_string(), "charge".to_string()],
                &catalog,
            )
            .unwrap();

        assert_eq!(blueprint.skill_loadout, vec![1, 4]);
        assert_eq!(
            resolve_skill_loadout_from_codes(
                &["fireball".to_string(), "burn".to_string()],
                &catalog
            )
            .unwrap(),
            vec![2, 5]
        );
    }

    #[test]
    fn burn_buff_ticks_periodic_damage() {
        let catalog = BuiltinCombatCatalog::default();
        let mut ecs = RoomCombatEcs::new();
        let player_a = ecs
            .spawn_entity(CombatEntityBlueprint::player(
                "player-a",
                1,
                Position { x: 0.0, y: 0.0 },
            ))
            .unwrap();
        let player_b = ecs
            .spawn_entity(CombatEntityBlueprint::player(
                "player-b",
                2,
                Position { x: 1.0, y: 0.0 },
            ))
            .unwrap();
        let mut hooks = NoopCombatHooks;

        ecs.execute_command(
            CombatCommand::ApplyBuff {
                frame_id: 1,
                source_entity: Some(player_a),
                target_entity: player_b,
                buff_id: 1,
                duration_frames: Some(31),
            },
            &catalog,
            &mut hooks,
        );

        let before = ecs
            .snapshot(1, &catalog)
            .entities
            .into_iter()
            .find(|entity| entity.entity_id == player_b)
            .unwrap()
            .hp;

        for frame_id in 1..=30 {
            ecs.tick(frame_id, 30, &catalog, &mut hooks);
        }

        let after = ecs
            .snapshot(30, &catalog)
            .entities
            .into_iter()
            .find(|entity| entity.entity_id == player_b)
            .unwrap()
            .hp;

        assert!(after < before);
    }

    #[test]
    fn combat_transfer_roundtrip_restores_runtime_state() {
        let catalog = BuiltinCombatCatalog::default();
        let ecs = transfer_sample_ecs();
        let player_a = ecs.entity_id_by_character("player-a").unwrap();
        let player_b = ecs.entity_id_by_character("player-b").unwrap();
        let original_snapshot = ecs.snapshot(ecs.last_tick_frame(), &catalog);

        let transfer_json = ecs.export_transfer_state_json().unwrap();
        let transfer_value: serde_json::Value = serde_json::from_str(&transfer_json).unwrap();
        assert_eq!(transfer_value["schema"], ROOM_COMBAT_TRANSFER_SCHEMA);
        assert_eq!(transfer_value["last_tick_frame"], 2);
        assert_eq!(transfer_value["pending_events_replayed"], false);
        assert!(transfer_value.get("pending_events").is_none());
        assert!(transfer_value.get("player_entity_map").is_none());
        assert_eq!(
            transfer_value["character_entity_map"],
            serde_json::json!([
                {"character_id": "player-a", "entity_id": player_a},
                {"character_id": "player-b", "entity_id": player_b}
            ])
        );

        let mut restored = RoomCombatEcs::import_transfer_state_json(&transfer_json).unwrap();
        assert_eq!(restored.last_tick_frame(), 2);
        assert_eq!(restored.entity_count(), 2);
        assert_eq!(restored.entity_id_by_character("player-a"), Some(player_a));
        assert_eq!(restored.entity_id_by_character("player-b"), Some(player_b));
        assert!(restored.pending_events.is_empty());
        assert_eq!(restored.pending_skill_requests.len(), 1);
        assert_eq!(
            serde_json::to_value(&restored.snapshot(2, &catalog)).unwrap(),
            serde_json::to_value(&original_snapshot).unwrap()
        );

        let caster_index = restored.dense_index(player_a).unwrap();
        let charge_slot = restored.skill_slots[caster_index]
            .iter()
            .find(|slot| slot.skill_id == 4)
            .unwrap();
        assert_eq!(charge_slot.cooldown_remaining, 59);

        let target_index = restored.dense_index(player_b).unwrap();
        assert!(restored.move_states[target_index].is_active());
        let shield = restored.buff_slots[target_index]
            .iter()
            .find(|slot| slot.buff_id == 2)
            .unwrap();
        assert_eq!(shield.duration_remaining, 76);
        assert_eq!(shield.source_entity, player_a);

        restored.tick(3, 20, &catalog, &mut NoopCombatHooks);
        assert!(restored.pending_skill_requests.is_empty());
        assert!(
            restored
                .drain_events()
                .iter()
                .any(|event| event.kind == CombatEventKind::SkillCast && event.skill_id == Some(2))
        );
        let fireball_slot = restored.skill_slots[caster_index]
            .iter()
            .find(|slot| slot.skill_id == 2)
            .unwrap();
        assert_eq!(fireball_slot.cooldown_remaining, 90);
        assert!(restored.positions_x[target_index] > ecs.positions_x[target_index]);
    }

    #[test]
    fn combat_transfer_import_rejects_invalid_schema_and_payloads() {
        assert_eq!(
            RoomCombatEcs::import_transfer_state_json("{bad").unwrap_err(),
            ROOM_TRANSFER_INVALID_COMBAT_STATE
        );

        let mut unsupported_schema = transfer_sample_value();
        unsupported_schema["schemaVersion"] = json!(ROOM_COMBAT_TRANSFER_SCHEMA_VERSION + 1);
        assert_eq!(
            RoomCombatEcs::import_transfer_state_json(&unsupported_schema.to_string()).unwrap_err(),
            ROOM_TRANSFER_UNSUPPORTED_SCHEMA
        );

        let mut duplicate_entity = transfer_sample_value();
        let first_entity_id = duplicate_entity["entities"][0]["entity_id"].clone();
        duplicate_entity["entities"][1]["entity_id"] = first_entity_id;
        assert_eq!(
            RoomCombatEcs::import_transfer_state_json(&duplicate_entity.to_string()).unwrap_err(),
            ROOM_TRANSFER_INVALID_COMBAT_STATE
        );

        let mut duplicate_player = transfer_sample_value();
        let first_character_id = duplicate_player["entities"][0]["character_id"].clone();
        duplicate_player["entities"][1]["character_id"] = first_character_id;
        assert_eq!(
            RoomCombatEcs::import_transfer_state_json(&duplicate_player.to_string()).unwrap_err(),
            ROOM_TRANSFER_INVALID_COMBAT_STATE
        );

        let mut duplicate_player_map = transfer_sample_value();
        let first_map_entry = duplicate_player_map["character_entity_map"][0].clone();
        duplicate_player_map["character_entity_map"]
            .as_array_mut()
            .unwrap()
            .push(first_map_entry);
        assert_eq!(
            RoomCombatEcs::import_transfer_state_json(&duplicate_player_map.to_string())
                .unwrap_err(),
            ROOM_TRANSFER_INVALID_COMBAT_STATE
        );

        let mut zero_entity_id = transfer_sample_value();
        zero_entity_id["entities"][0]["entity_id"] = json!(0);
        assert_eq!(
            RoomCombatEcs::import_transfer_state_json(&zero_entity_id.to_string()).unwrap_err(),
            ROOM_TRANSFER_INVALID_COMBAT_STATE
        );

        let mut length_mismatch = transfer_sample_value();
        length_mismatch["positions_x"].as_array_mut().unwrap().pop();
        assert_eq!(
            RoomCombatEcs::import_transfer_state_json(&length_mismatch.to_string()).unwrap_err(),
            ROOM_TRANSFER_INVALID_COMBAT_STATE
        );

        let mut invalid_move_state = transfer_sample_value();
        invalid_move_state["move_states"][0]["progress"] = json!(1.5);
        assert_eq!(
            RoomCombatEcs::import_transfer_state_json(&invalid_move_state.to_string()).unwrap_err(),
            ROOM_TRANSFER_INVALID_COMBAT_STATE
        );

        let mut invalid_skill_request = transfer_sample_value();
        invalid_skill_request["pending_skill_requests"][0]["source_entity"] = json!(0);
        assert_eq!(
            RoomCombatEcs::import_transfer_state_json(&invalid_skill_request.to_string())
                .unwrap_err(),
            ROOM_TRANSFER_INVALID_COMBAT_STATE
        );

        let mut system_buff_source = transfer_sample_value();
        system_buff_source["buff_slots"][1][0]["source_entity"] = json!(0);
        assert!(RoomCombatEcs::import_transfer_state_json(&system_buff_source.to_string()).is_ok());

        let mut invalid_buff_source = transfer_sample_value();
        invalid_buff_source["buff_slots"][1][0]["source_entity"] =
            invalid_buff_source["next_entity_id"].clone();
        assert_eq!(
            RoomCombatEcs::import_transfer_state_json(&invalid_buff_source.to_string())
                .unwrap_err(),
            ROOM_TRANSFER_INVALID_COMBAT_STATE
        );

        let mut non_finite_runtime = transfer_sample_ecs();
        non_finite_runtime.positions_x[0] = f32::INFINITY;
        assert_eq!(
            non_finite_runtime.export_transfer_state_json(),
            Err(ROOM_TRANSFER_INVALID_COMBAT_STATE)
        );
    }
}
