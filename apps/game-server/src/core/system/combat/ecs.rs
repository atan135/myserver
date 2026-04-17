use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::catalog::CombatCatalog;
use super::buffs::BuffEffectType;
use super::components::{
    BuffSlot, DamageFormula, EntityMeta, EntityType, Health, MoveState, MoveStateType, Position,
    SkillSlot, Stats,
};
use super::skills::{SkillDefinition, SkillEffect, SkillEffectType, SkillTargetType};

pub const MAX_ENTITIES: usize = 2048;
pub const MAX_SKILLS_PER_ENTITY: usize = 8;
pub const MAX_BUFFS_PER_ENTITY: usize = 6;

pub type EntityId = u32;
type DenseIndex = usize;

#[derive(Debug, Clone)]
pub struct CombatEntityBlueprint {
    pub entity_type: EntityType,
    pub player_id: Option<String>,
    pub team_id: u16,
    pub position: Position,
    pub facing: Position,
    pub health: Health,
    pub stats: Stats,
    pub skill_loadout: Vec<u16>,
}

impl CombatEntityBlueprint {
    pub fn player(player_id: &str, team_id: u16, position: Position) -> Self {
        Self {
            entity_type: EntityType::Player,
            player_id: Some(player_id.to_string()),
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
            player_id: None,
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
    pub player_id: Option<String>,
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
    player_entity_map: HashMap<String, EntityId>,
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
            player_entity_map: HashMap::new(),
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

    pub fn entity_id_by_player(&self, player_id: &str) -> Option<EntityId> {
        self.player_entity_map.get(player_id).copied()
    }

    pub fn entity_player_id(&self, entity_id: EntityId) -> Option<&str> {
        let dense_index = self.dense_index(entity_id)?;
        self.entities.get(dense_index)?.player_id.as_deref()
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

        if let Some(player_id) = blueprint.player_id.as_ref() {
            if self.player_entity_map.contains_key(player_id) {
                return Err("COMBAT_PLAYER_ALREADY_SPAWNED");
            }
        }

        let entity_id = self.next_entity_id;
        self.next_entity_id = self.next_entity_id.saturating_add(1);

        let dense_index = self.entities.len();
        self.entities.push(EntityMeta {
            entity_id,
            entity_type: blueprint.entity_type,
            player_id: blueprint.player_id.clone(),
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
        self.buff_slots.push([BuffSlot::empty(); MAX_BUFFS_PER_ENTITY]);

        if let Some(player_id) = blueprint.player_id {
            self.player_entity_map.insert(player_id, entity_id);
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

        if let Some(player_id) = self.entities[dense_index].player_id.as_ref() {
            self.player_entity_map.remove(player_id);
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
                player_id: meta.player_id.clone(),
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
                .filter(|target_entity| self.is_valid_target(source_entity, *target_entity, target_type))
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
            return matches!(target_type, SkillTargetType::SelfOnly | SkillTargetType::Ally);
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
            } => base
                .saturating_add(source_stats.attack.saturating_mul(i32::from(attack_scale_bps)) / 10_000),
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
                amount = amount
                    .saturating_mul(10_000 + i32::from(source_stats.crit_damage_bps))
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

        let duration = duration_frames.unwrap_or(buff_definition.duration_frames).max(1);
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
            .position(|slot| slot.is_empty()) else {
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
            .find(|slot| slot.buff_id == buff_id && !slot.is_empty()) else {
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

    fn effective_stats_at_dense(&self, dense_index: DenseIndex, catalog: &dyn CombatCatalog) -> Stats {
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
}
