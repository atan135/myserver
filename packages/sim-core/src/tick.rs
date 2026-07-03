//! Simulation tick advancement.

use crate::combat::{
    BuffId, CombatConfig, CombatEffect, DamageFormula, SkillDefinition, SkillId, SkillTargetType,
};
use crate::hash::{SimHash, hash_world};
use crate::ids::{EntityId, FrameId};
use crate::input::{
    CastSkillCommand, SimCommand, SimInput, SkillTarget, ordered_inputs,
    select_latest_cast_skill_inputs, select_latest_movement_inputs,
};
use crate::math::{FP_SCALE, Fp, QuantizedDir, Vec2Fp};
use crate::state::{BuffSlot, MovementMode, SimEntity, SimWorld};
use serde::{Deserialize, Serialize};
use std::fmt;

const BASIS_POINTS_DENOMINATOR: i128 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Axis-aligned rectangular scene bounds.
///
/// `min` and `max` are opposite corners. Each axis is normalized independently,
/// so swapped min/max values still form the same rectangle.
pub struct SceneBounds {
    pub min: Vec2Fp,
    pub max: Vec2Fp,
}

impl SceneBounds {
    /// Clamps an entity center so its radius stays inside this rectangle.
    ///
    /// The center is clamped to `[min + radius, max - radius]` per axis. If the
    /// radius is larger than the available span on an axis, that axis
    /// deterministically collapses to the bounds midpoint.
    pub fn clamp_center_with_radius(self, center: Vec2Fp, radius: Fp) -> Vec2Fp {
        self.clamp_raw_center_with_radius(center.x.raw() as i128, center.y.raw() as i128, radius)
    }

    fn clamp_raw_center_with_radius(
        self,
        center_x_raw: i128,
        center_y_raw: i128,
        radius: Fp,
    ) -> Vec2Fp {
        Vec2Fp::new(
            clamp_axis_with_radius(
                center_x_raw,
                self.min.x.raw(),
                self.max.x.raw(),
                radius.raw(),
            ),
            clamp_axis_with_radius(
                center_y_raw,
                self.min.y.raw(),
                self.max.y.raw(),
                radius.raw(),
            ),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimConfig {
    pub movement: MovementConfig,
    pub combat: CombatConfig,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MovementConfig {
    pub tick_rate: u16,
    /// Simulation units per second represented as `Fp` raw milli-units.
    pub default_speed_per_second: Fp,
    /// Simulation units per second represented as `Fp` raw milli-units.
    pub max_speed_per_second: Fp,
    /// Axis-aligned rectangular movement bounds.
    pub bounds: SceneBounds,
    /// Reserved map collision data.
    ///
    /// P1 movement does not apply static obstacles. They are serializable
    /// configuration only, kept out of `SimWorld` and therefore out of
    /// `hash_world`. When obstacle collision is enabled in a later phase,
    /// entities should still be advanced in sorted `EntityId` order and
    /// obstacles should be resolved in this vector order after bounds clamping.
    pub static_obstacles: Vec<StaticObstacle>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Reserved static obstacle for future map collision.
///
/// P1 movement accepts this structure in configuration but intentionally does
/// not resolve collisions against it.
pub struct StaticObstacle {
    pub shape: StaticObstacleShape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Reserved static obstacle shape.
pub enum StaticObstacleShape {
    Circle {
        center: Vec2Fp,
        radius: Fp,
    },
    /// Axis-aligned rectangle using opposite corners with the same semantics as
    /// `SceneBounds`.
    AxisAlignedRect {
        min: Vec2Fp,
        max: Vec2Fp,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResolvedMoveCommand {
    dir: QuantizedDir,
    speed_per_second: Fp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MovementSelection {
    Move(ResolvedMoveCommand),
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResolvedMovementInput {
    entity_id: EntityId,
    selection: MovementSelection,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedStepInputs {
    movement_inputs: Vec<ResolvedMovementInput>,
    cast_skills: Vec<ResolvedCastSkill>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedCastSkill {
    caster_id: EntityId,
    skill_id: SkillId,
    targets: ResolvedSkillTargets,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedSkillTargets {
    targets: Vec<HitTarget>,
}

impl ResolvedSkillTargets {
    fn empty() -> Self {
        Self {
            targets: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HitTarget {
    entity_id: EntityId,
    distance_squared: i128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingBuffTick {
    target_id: EntityId,
    buff_id: BuffId,
    source_entity: EntityId,
    stacks: u16,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct EventSequence {
    next: u32,
}

impl EventSequence {
    fn next(&mut self) -> u32 {
        let sequence = self.next;
        self.next = self.next.saturating_add(1);
        sequence
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EffectSource {
    Skill(SkillId),
    Buff(BuffId),
}

impl EffectSource {
    fn skill_id(self) -> Option<SkillId> {
        match self {
            Self::Skill(skill_id) => Some(skill_id),
            Self::Buff(_) => None,
        }
    }

    fn buff_id(self) -> Option<BuffId> {
        match self {
            Self::Skill(_) => None,
            Self::Buff(buff_id) => Some(buff_id),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct DamageOutcome {
    value: i32,
    killed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct HealOutcome {
    value: i32,
    killed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SkillTargetFilter {
    Ally,
    Enemy,
    AnyEntity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepError {
    NonSequentialFrame {
        expected: FrameId,
        actual: FrameId,
    },
    FrameOverflow {
        current: FrameId,
        actual: FrameId,
    },
    ZeroTickRate,
    InvalidMovementSpeed {
        entity_id: EntityId,
        speed_per_second: Fp,
    },
    MovementSpeedTooHigh {
        entity_id: EntityId,
        speed_per_second: Fp,
        max_speed_per_second: Fp,
    },
    ZeroDirectionMove {
        entity_id: EntityId,
    },
    EntityNotFound {
        entity_id: EntityId,
    },
    UnknownSkill {
        entity_id: EntityId,
        skill_id: SkillId,
    },
    UnknownBuff {
        entity_id: EntityId,
        buff_id: BuffId,
    },
    SkillNotEquipped {
        entity_id: EntityId,
        skill_id: SkillId,
    },
    SkillOnCooldown {
        entity_id: EntityId,
        skill_id: SkillId,
        cooldown_remaining: u32,
    },
    SkillTargetTypeMismatch {
        entity_id: EntityId,
        skill_id: SkillId,
        expected: SkillTargetType,
        actual: SkillTarget,
    },
    InvalidSkillTarget {
        entity_id: EntityId,
        skill_id: SkillId,
        target_entity_id: EntityId,
    },
    SkillTargetOutOfRange {
        entity_id: EntityId,
        skill_id: SkillId,
        target_entity_id: EntityId,
        distance_squared: i128,
        range_squared: i128,
    },
    SkillTargetDistanceOverflow {
        entity_id: EntityId,
        skill_id: SkillId,
    },
    MovementDeltaOverflow {
        entity_id: EntityId,
    },
}

impl fmt::Display for StepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonSequentialFrame { expected, actual } => write!(
                f,
                "non-sequential simulation frame: expected {}, got {}",
                expected.raw(),
                actual.raw()
            ),
            Self::FrameOverflow { current, actual } => write!(
                f,
                "simulation frame overflow after {}, got {}",
                current.raw(),
                actual.raw()
            ),
            Self::ZeroTickRate => write!(f, "simulation tick rate must be greater than zero"),
            Self::InvalidMovementSpeed {
                entity_id,
                speed_per_second,
            } => write!(
                f,
                "movement speed must be greater than zero for simulation entity {}: {}",
                entity_id.raw(),
                speed_per_second.raw()
            ),
            Self::MovementSpeedTooHigh {
                entity_id,
                speed_per_second,
                max_speed_per_second,
            } => write!(
                f,
                "movement speed is above max for simulation entity {}: {} > {}",
                entity_id.raw(),
                speed_per_second.raw(),
                max_speed_per_second.raw()
            ),
            Self::ZeroDirectionMove { entity_id } => write!(
                f,
                "positive movement speed requires a non-zero direction for simulation entity: {}",
                entity_id.raw()
            ),
            Self::EntityNotFound { entity_id } => {
                write!(f, "simulation entity not found: {}", entity_id.raw())
            }
            Self::UnknownSkill {
                entity_id,
                skill_id,
            } => write!(
                f,
                "simulation entity {} tried to cast unknown skill {}",
                entity_id.raw(),
                skill_id.raw()
            ),
            Self::UnknownBuff { entity_id, buff_id } => write!(
                f,
                "simulation entity {} references unknown buff {}",
                entity_id.raw(),
                buff_id.raw()
            ),
            Self::SkillNotEquipped {
                entity_id,
                skill_id,
            } => write!(
                f,
                "simulation entity {} has not equipped skill {}",
                entity_id.raw(),
                skill_id.raw()
            ),
            Self::SkillOnCooldown {
                entity_id,
                skill_id,
                cooldown_remaining,
            } => write!(
                f,
                "simulation entity {} tried to cast skill {} while cooldown remains {} frames",
                entity_id.raw(),
                skill_id.raw(),
                cooldown_remaining
            ),
            Self::SkillTargetTypeMismatch {
                entity_id,
                skill_id,
                expected,
                actual,
            } => write!(
                f,
                "simulation entity {} tried to cast skill {} with target {}, expected {}",
                entity_id.raw(),
                skill_id.raw(),
                skill_target_name(*actual),
                skill_target_type_name(*expected)
            ),
            Self::InvalidSkillTarget {
                entity_id,
                skill_id,
                target_entity_id,
            } => write!(
                f,
                "simulation entity {} tried to cast skill {} on invalid target entity {}",
                entity_id.raw(),
                skill_id.raw(),
                target_entity_id.raw()
            ),
            Self::SkillTargetOutOfRange {
                entity_id,
                skill_id,
                target_entity_id,
                distance_squared,
                range_squared,
            } => write!(
                f,
                "simulation entity {} tried to cast skill {} on target entity {} out of range: distance_squared {} > range_squared {}",
                entity_id.raw(),
                skill_id.raw(),
                target_entity_id.raw(),
                distance_squared,
                range_squared
            ),
            Self::SkillTargetDistanceOverflow {
                entity_id,
                skill_id,
            } => write!(
                f,
                "simulation entity {} tried to cast skill {} but target distance overflowed",
                entity_id.raw(),
                skill_id.raw()
            ),
            Self::MovementDeltaOverflow { entity_id } => write!(
                f,
                "movement delta overflow for simulation entity: {}",
                entity_id.raw()
            ),
        }
    }
}

impl std::error::Error for StepError {}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SimEvent {
    SkillCast {
        frame: FrameId,
        source_entity: EntityId,
        target_entity: Option<EntityId>,
        skill_id: SkillId,
        value: i32,
        sequence: u32,
    },
    DamageApplied {
        frame: FrameId,
        source_entity: EntityId,
        target_entity: EntityId,
        skill_id: Option<SkillId>,
        buff_id: Option<BuffId>,
        value: i32,
        sequence: u32,
    },
    HealApplied {
        frame: FrameId,
        source_entity: EntityId,
        target_entity: EntityId,
        skill_id: Option<SkillId>,
        buff_id: Option<BuffId>,
        value: i32,
        sequence: u32,
    },
    BuffApplied {
        frame: FrameId,
        source_entity: EntityId,
        target_entity: EntityId,
        buff_id: BuffId,
        value: i32,
        sequence: u32,
    },
    BuffExpired {
        frame: FrameId,
        source_entity: EntityId,
        target_entity: EntityId,
        buff_id: BuffId,
        value: i32,
        sequence: u32,
    },
    EntityDied {
        frame: FrameId,
        source_entity: EntityId,
        target_entity: EntityId,
        skill_id: Option<SkillId>,
        buff_id: Option<BuffId>,
        value: i32,
        sequence: u32,
    },
    BuffTick {
        frame: FrameId,
        source_entity: EntityId,
        target_entity: EntityId,
        buff_id: BuffId,
        value: i32,
        sequence: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimStepResult {
    pub frame: FrameId,
    /// Combat events emitted by this frame only.
    ///
    /// Events are not written into `SimWorld` and are not part of
    /// `hash_world`; `state_hash` is computed only from the final world state.
    pub events: Vec<SimEvent>,
    pub state_hash: SimHash,
}

fn sort_sim_events(events: &mut [SimEvent]) {
    events.sort_by_key(SimEvent::sort_key);
}

impl SimEvent {
    fn sort_key(&self) -> (FrameId, u8, EntityId, Option<EntityId>, u32) {
        (
            self.frame(),
            self.event_type_code(),
            self.source_entity(),
            self.target_entity(),
            self.sequence(),
        )
    }

    fn frame(&self) -> FrameId {
        match self {
            Self::SkillCast { frame, .. }
            | Self::DamageApplied { frame, .. }
            | Self::HealApplied { frame, .. }
            | Self::BuffApplied { frame, .. }
            | Self::BuffExpired { frame, .. }
            | Self::EntityDied { frame, .. }
            | Self::BuffTick { frame, .. } => *frame,
        }
    }

    fn event_type_code(&self) -> u8 {
        match self {
            Self::SkillCast { .. } => 10,
            Self::BuffApplied { .. } => 20,
            Self::BuffTick { .. } => 30,
            Self::DamageApplied { .. } => 40,
            Self::HealApplied { .. } => 50,
            Self::BuffExpired { .. } => 60,
            Self::EntityDied { .. } => 70,
        }
    }

    fn source_entity(&self) -> EntityId {
        match self {
            Self::SkillCast { source_entity, .. }
            | Self::DamageApplied { source_entity, .. }
            | Self::HealApplied { source_entity, .. }
            | Self::BuffApplied { source_entity, .. }
            | Self::BuffExpired { source_entity, .. }
            | Self::EntityDied { source_entity, .. }
            | Self::BuffTick { source_entity, .. } => *source_entity,
        }
    }

    fn target_entity(&self) -> Option<EntityId> {
        match self {
            Self::SkillCast { target_entity, .. } => *target_entity,
            Self::DamageApplied { target_entity, .. }
            | Self::HealApplied { target_entity, .. }
            | Self::BuffApplied { target_entity, .. }
            | Self::BuffExpired { target_entity, .. }
            | Self::EntityDied { target_entity, .. }
            | Self::BuffTick { target_entity, .. } => Some(*target_entity),
        }
    }

    fn sequence(&self) -> u32 {
        match self {
            Self::SkillCast { sequence, .. }
            | Self::DamageApplied { sequence, .. }
            | Self::HealApplied { sequence, .. }
            | Self::BuffApplied { sequence, .. }
            | Self::BuffExpired { sequence, .. }
            | Self::EntityDied { sequence, .. }
            | Self::BuffTick { sequence, .. } => *sequence,
        }
    }
}

/// Advances `world` by exactly one sequential frame using deterministic P0 rules.
///
/// Applies deterministic movement, facing, direct combat effects, and the P2
/// minimal buff runtime effects, then returns frame events and a state hash.
/// Combat events are a step-result output only; they do not enter
/// `hash_world`.
pub fn step(
    world: &mut SimWorld,
    frame: FrameId,
    inputs: &[SimInput],
    config: &SimConfig,
) -> Result<SimStepResult, StepError> {
    let resolved_inputs = validate_step(world, frame, inputs, config)?;

    for movement_input in resolved_inputs.movement_inputs {
        let entity =
            world
                .entity_mut(movement_input.entity_id)
                .ok_or(StepError::EntityNotFound {
                    entity_id: movement_input.entity_id,
                })?;

        match movement_input.selection {
            MovementSelection::Move(command) => {
                entity.movement.mode = MovementMode::Controlled;
                entity.movement.move_dir = command.dir;
                entity.movement.speed_per_second = command.speed_per_second;
            }
            MovementSelection::Stop => {
                entity.movement.mode = MovementMode::Idle;
                entity.movement.move_dir = QuantizedDir::ZERO;
                entity.movement.speed_per_second = Fp::ZERO;
            }
        }
    }

    for indexed in ordered_inputs(inputs) {
        let input = indexed.input;
        let SimCommand::Face(command) = input.command else {
            continue;
        };

        let entity = world
            .entity_mut(input.entity_id)
            .ok_or(StepError::EntityNotFound {
                entity_id: input.entity_id,
            })?;
        entity.transform.facing = command.dir;
    }

    for entity in &mut world.entities {
        advance_controlled_entity(entity, config)?;
    }

    let mut events = Vec::new();
    let mut event_sequence = EventSequence::default();

    // Same-frame skill effects are deterministic and stable:
    // selected cast input order, then resolved target order, then effect vector
    // order. Event sequence records this application order before the public
    // output is sorted by the stable event key.
    for cast_skill in &resolved_inputs.cast_skills {
        apply_resolved_cast_skill(
            world,
            cast_skill,
            frame,
            &mut events,
            &mut event_sequence,
            config,
        )?;
    }

    advance_combat_timers(
        world,
        frame,
        &mut events,
        &mut event_sequence,
        &config.combat,
    )?;
    sort_sim_events(&mut events);

    world.frame = frame;

    Ok(SimStepResult {
        frame,
        events,
        state_hash: hash_world(world),
    })
}

fn validate_step(
    world: &SimWorld,
    frame: FrameId,
    inputs: &[SimInput],
    config: &SimConfig,
) -> Result<ResolvedStepInputs, StepError> {
    let expected =
        world
            .frame
            .raw()
            .checked_add(1)
            .map(FrameId::new)
            .ok_or(StepError::FrameOverflow {
                current: world.frame,
                actual: frame,
            })?;

    if frame != expected {
        return Err(StepError::NonSequentialFrame {
            expected,
            actual: frame,
        });
    }

    if config.movement.tick_rate == 0 {
        return Err(StepError::ZeroTickRate);
    }

    let mut movement_inputs = Vec::new();
    for indexed in select_latest_movement_inputs(inputs) {
        let entity_id = indexed.input.entity_id;
        let entity = world
            .entity(entity_id)
            .ok_or(StepError::EntityNotFound { entity_id })?;

        movement_inputs.push(resolve_movement_input(indexed.input, entity, config)?);
    }

    for indexed in ordered_inputs(inputs) {
        let input = indexed.input;
        if matches!(input.command, SimCommand::Face(_)) && world.entity(input.entity_id).is_none() {
            return Err(StepError::EntityNotFound {
                entity_id: input.entity_id,
            });
        }
    }

    let mut cast_skills = Vec::new();
    for indexed in select_latest_cast_skill_inputs(inputs) {
        let input = indexed.input;
        let entity = world
            .entity(input.entity_id)
            .ok_or(StepError::EntityNotFound {
                entity_id: input.entity_id,
            })?;

        let SimCommand::CastSkill(command) = input.command else {
            unreachable!("select_latest_cast_skill_inputs only returns cast skill commands")
        };

        cast_skills.push(resolve_cast_skill_input(
            input.entity_id,
            command,
            entity,
            world,
            config,
        )?);
    }

    Ok(ResolvedStepInputs {
        movement_inputs,
        cast_skills,
    })
}

fn resolve_movement_input(
    input: &SimInput,
    entity: &SimEntity,
    config: &SimConfig,
) -> Result<ResolvedMovementInput, StepError> {
    let selection = match input.command {
        SimCommand::Move(command) => {
            let speed_per_second = command.speed_per_second.unwrap_or_else(|| {
                if entity.movement.speed_per_second > Fp::ZERO {
                    entity.movement.speed_per_second
                } else {
                    config.movement.default_speed_per_second
                }
            });

            validate_move_speed(input.entity_id, command.dir, speed_per_second, config)?;

            MovementSelection::Move(ResolvedMoveCommand {
                dir: command.dir,
                speed_per_second,
            })
        }
        SimCommand::Stop => MovementSelection::Stop,
        SimCommand::Face(_) | SimCommand::CastSkill(_) | SimCommand::Noop => {
            unreachable!("resolve_movement_input is only called for movement selection commands")
        }
    };

    Ok(ResolvedMovementInput {
        entity_id: input.entity_id,
        selection,
    })
}

fn validate_move_speed(
    entity_id: EntityId,
    dir: QuantizedDir,
    speed_per_second: Fp,
    config: &SimConfig,
) -> Result<(), StepError> {
    if speed_per_second <= Fp::ZERO {
        return Err(StepError::InvalidMovementSpeed {
            entity_id,
            speed_per_second,
        });
    }

    if dir == QuantizedDir::ZERO {
        return Err(StepError::ZeroDirectionMove { entity_id });
    }

    if speed_per_second > config.movement.max_speed_per_second {
        return Err(StepError::MovementSpeedTooHigh {
            entity_id,
            speed_per_second,
            max_speed_per_second: config.movement.max_speed_per_second,
        });
    }

    Ok(())
}

fn resolve_cast_skill_input(
    entity_id: EntityId,
    command: CastSkillCommand,
    entity: &SimEntity,
    world: &SimWorld,
    config: &SimConfig,
) -> Result<ResolvedCastSkill, StepError> {
    let skill = config
        .combat
        .skills
        .get(command.skill_id)
        .ok_or(StepError::UnknownSkill {
            entity_id,
            skill_id: command.skill_id,
        })?;

    let slot = entity
        .combat
        .skill_slots
        .iter()
        .find(|slot| slot.skill_id == command.skill_id)
        .ok_or(StepError::SkillNotEquipped {
            entity_id,
            skill_id: command.skill_id,
        })?;

    if slot.cooldown_remaining > 0 {
        return Err(StepError::SkillOnCooldown {
            entity_id,
            skill_id: command.skill_id,
            cooldown_remaining: slot.cooldown_remaining,
        });
    }

    if !skill_target_matches(command.target, skill.target_type) {
        return Err(StepError::SkillTargetTypeMismatch {
            entity_id,
            skill_id: command.skill_id,
            expected: skill.target_type,
            actual: command.target,
        });
    }

    let targets = resolve_skill_targets(entity_id, command.target, entity, skill, world)?;

    Ok(ResolvedCastSkill {
        caster_id: entity_id,
        skill_id: command.skill_id,
        targets,
    })
}

fn resolve_skill_targets(
    caster_id: EntityId,
    target: SkillTarget,
    caster: &SimEntity,
    skill: &SkillDefinition,
    world: &SimWorld,
) -> Result<ResolvedSkillTargets, StepError> {
    match (skill.target_type, target) {
        (SkillTargetType::SelfOnly, SkillTarget::None) => Ok(ResolvedSkillTargets {
            targets: vec![HitTarget {
                entity_id: caster_id,
                distance_squared: 0,
            }],
        }),
        (
            SkillTargetType::Ally | SkillTargetType::Enemy | SkillTargetType::AnyEntity,
            SkillTarget::Entity(target_entity_id),
        ) => {
            let filter = entity_target_filter(skill.target_type)
                .expect("entity target skill type should have a target filter");
            resolve_entity_skill_target(caster_id, caster, target_entity_id, skill, world, filter)
        }
        (SkillTargetType::Position, SkillTarget::Position(center)) => {
            resolve_position_skill_targets(
                caster_id,
                caster,
                center,
                skill,
                world,
                SkillTargetFilter::Enemy,
            )
        }
        _ => Ok(ResolvedSkillTargets::empty()),
    }
}

fn resolve_entity_skill_target(
    caster_id: EntityId,
    caster: &SimEntity,
    target_entity_id: EntityId,
    skill: &SkillDefinition,
    world: &SimWorld,
    filter: SkillTargetFilter,
) -> Result<ResolvedSkillTargets, StepError> {
    let target = world
        .entity(target_entity_id)
        .ok_or(StepError::EntityNotFound {
            entity_id: target_entity_id,
        })?;

    if !is_valid_hit_candidate(caster, target, filter) {
        return Err(StepError::InvalidSkillTarget {
            entity_id: caster_id,
            skill_id: skill.id,
            target_entity_id,
        });
    }

    let distance_squared = skill_target_distance_squared(
        caster_id,
        skill.id,
        caster.transform.pos,
        target.transform.pos,
    )?;
    let range_squared = range_squared_raw(skill.cast_range);

    if distance_squared > range_squared {
        return Err(StepError::SkillTargetOutOfRange {
            entity_id: caster_id,
            skill_id: skill.id,
            target_entity_id,
            distance_squared,
            range_squared,
        });
    }

    Ok(ResolvedSkillTargets {
        targets: vec![HitTarget {
            entity_id: target_entity_id,
            distance_squared,
        }],
    })
}

fn resolve_position_skill_targets(
    caster_id: EntityId,
    caster: &SimEntity,
    center: Vec2Fp,
    skill: &SkillDefinition,
    world: &SimWorld,
    filter: SkillTargetFilter,
) -> Result<ResolvedSkillTargets, StepError> {
    let range_squared = range_squared_raw(skill.cast_range);
    let mut targets = Vec::new();

    for candidate in world.entities_sorted_by_id() {
        if !is_valid_hit_candidate(caster, candidate, filter) {
            continue;
        }

        let distance_squared =
            skill_target_distance_squared(caster_id, skill.id, center, candidate.transform.pos)?;

        if distance_squared <= range_squared {
            targets.push(HitTarget {
                entity_id: candidate.id,
                distance_squared,
            });
        }
    }

    targets.sort_by_key(|target| (target.distance_squared, target.entity_id));

    Ok(ResolvedSkillTargets { targets })
}

fn apply_resolved_cast_skill(
    world: &mut SimWorld,
    cast_skill: &ResolvedCastSkill,
    frame: FrameId,
    events: &mut Vec<SimEvent>,
    event_sequence: &mut EventSequence,
    config: &SimConfig,
) -> Result<(), StepError> {
    let skill = config
        .combat
        .skills
        .get(cast_skill.skill_id)
        .ok_or(StepError::UnknownSkill {
            entity_id: cast_skill.caster_id,
            skill_id: cast_skill.skill_id,
        })?;
    let caster_attack = world
        .entity(cast_skill.caster_id)
        .ok_or(StepError::EntityNotFound {
            entity_id: cast_skill.caster_id,
        })?
        .combat
        .attack;

    let caster = world
        .entity_mut(cast_skill.caster_id)
        .ok_or(StepError::EntityNotFound {
            entity_id: cast_skill.caster_id,
        })?;
    let slot = caster
        .combat
        .skill_slots
        .iter_mut()
        .find(|slot| slot.skill_id == cast_skill.skill_id)
        .ok_or(StepError::SkillNotEquipped {
            entity_id: cast_skill.caster_id,
            skill_id: cast_skill.skill_id,
        })?;
    slot.cooldown_remaining = skill.cooldown_frames;

    events.push(SimEvent::SkillCast {
        frame,
        source_entity: cast_skill.caster_id,
        target_entity: cast_skill
            .targets
            .targets
            .first()
            .map(|target| target.entity_id),
        skill_id: cast_skill.skill_id,
        value: cast_skill.targets.targets.len() as i32,
        sequence: event_sequence.next(),
    });

    for target in &cast_skill.targets.targets {
        for effect in &skill.effects {
            apply_combat_effect(
                world,
                cast_skill.caster_id,
                caster_attack,
                target.entity_id,
                effect,
                EffectSource::Skill(cast_skill.skill_id),
                frame,
                events,
                event_sequence,
                &config.combat,
                1,
            )?;
        }
    }

    Ok(())
}

fn apply_combat_effect(
    world: &mut SimWorld,
    source_entity: EntityId,
    caster_attack: i32,
    target_id: EntityId,
    effect: &CombatEffect,
    effect_source: EffectSource,
    frame: FrameId,
    events: &mut Vec<SimEvent>,
    event_sequence: &mut EventSequence,
    combat_config: &CombatConfig,
    stacks: u16,
) -> Result<(), StepError> {
    let stack_multiplier = stacks as i128;
    match effect {
        CombatEffect::Damage { formula } => {
            let raw_damage = evaluate_formula(formula, caster_attack) * stack_multiplier;
            let ignores_defense = matches!(formula, DamageFormula::TrueDamage { .. });
            let target = world
                .entity_mut(target_id)
                .ok_or(StepError::EntityNotFound {
                    entity_id: target_id,
                })?;
            let outcome = apply_damage(target, raw_damage, ignores_defense);
            if outcome.value > 0 {
                events.push(SimEvent::DamageApplied {
                    frame,
                    source_entity,
                    target_entity: target_id,
                    skill_id: effect_source.skill_id(),
                    buff_id: effect_source.buff_id(),
                    value: outcome.value,
                    sequence: event_sequence.next(),
                });
            }
            if outcome.killed {
                events.push(SimEvent::EntityDied {
                    frame,
                    source_entity,
                    target_entity: target_id,
                    skill_id: effect_source.skill_id(),
                    buff_id: effect_source.buff_id(),
                    value: 0,
                    sequence: event_sequence.next(),
                });
            }
        }
        CombatEffect::Heal { formula } => {
            let raw_heal = evaluate_formula(formula, caster_attack) * stack_multiplier;
            let target = world
                .entity_mut(target_id)
                .ok_or(StepError::EntityNotFound {
                    entity_id: target_id,
                })?;
            let outcome = apply_heal(target, raw_heal);
            if outcome.value > 0 {
                events.push(SimEvent::HealApplied {
                    frame,
                    source_entity,
                    target_entity: target_id,
                    skill_id: effect_source.skill_id(),
                    buff_id: effect_source.buff_id(),
                    value: outcome.value,
                    sequence: event_sequence.next(),
                });
            }
            if outcome.killed {
                events.push(SimEvent::EntityDied {
                    frame,
                    source_entity,
                    target_entity: target_id,
                    skill_id: effect_source.skill_id(),
                    buff_id: effect_source.buff_id(),
                    value: 0,
                    sequence: event_sequence.next(),
                });
            }
        }
        CombatEffect::AddBuff { buff_id } => {
            let stacks =
                add_or_refresh_buff(world, target_id, source_entity, *buff_id, combat_config)?;
            events.push(SimEvent::BuffApplied {
                frame,
                source_entity,
                target_entity: target_id,
                buff_id: *buff_id,
                value: stacks as i32,
                sequence: event_sequence.next(),
            });
        }
    }

    Ok(())
}

fn add_or_refresh_buff(
    world: &mut SimWorld,
    target_id: EntityId,
    source_entity: EntityId,
    buff_id: BuffId,
    combat_config: &CombatConfig,
) -> Result<u16, StepError> {
    let buff_definition = combat_config
        .buffs
        .get(buff_id)
        .ok_or(StepError::UnknownBuff {
            entity_id: target_id,
            buff_id,
        })?;

    let target = world
        .entity_mut(target_id)
        .ok_or(StepError::EntityNotFound {
            entity_id: target_id,
        })?;

    if let Some(slot) = target
        .combat
        .buffs
        .iter_mut()
        .find(|slot| slot.buff_id == buff_id && slot.source_entity == source_entity)
    {
        // Fixed stacking rule for P2: one slot per (target, buff_id, source).
        // Reapplying that key refreshes timers and adds one stack up to max.
        slot.stacks = slot
            .stacks
            .saturating_add(1)
            .min(buff_definition.max_stacks);
        slot.duration_remaining = buff_definition.duration_frames;
        slot.interval_remaining = buff_definition.interval_frames;
        return Ok(slot.stacks);
    }

    target.combat.buffs.push(BuffSlot {
        buff_id,
        duration_remaining: buff_definition.duration_frames,
        interval_remaining: buff_definition.interval_frames,
        stacks: 1,
        source_entity,
    });

    Ok(1)
}

fn evaluate_formula(formula: &DamageFormula, caster_attack: i32) -> i128 {
    match *formula {
        DamageFormula::Fixed { amount } | DamageFormula::TrueDamage { amount } => amount as i128,
        DamageFormula::Scaling {
            base,
            attack_scale_bps,
        } => {
            base as i128
                + caster_attack as i128 * attack_scale_bps as i128 / BASIS_POINTS_DENOMINATOR
        }
    }
}

fn apply_damage(target: &mut SimEntity, raw_damage: i128, ignores_defense: bool) -> DamageOutcome {
    if !target.alive {
        return DamageOutcome::default();
    }

    if target.combat.hp <= 0 {
        let killed = kill_entity(target);
        return DamageOutcome { value: 0, killed };
    }

    let damage = effective_damage(raw_damage, target.combat.defense, ignores_defense);
    if damage == 0 {
        return DamageOutcome::default();
    }

    let applied = damage.min(target.combat.hp as i128) as i32;
    let remaining_hp = target.combat.hp as i128 - damage;
    let killed = if remaining_hp <= 0 {
        kill_entity(target)
    } else {
        target.combat.hp = remaining_hp as i32;
        false
    };

    DamageOutcome {
        value: applied,
        killed,
    }
}

fn effective_damage(raw_damage: i128, defense: i32, ignores_defense: bool) -> i128 {
    let damage = raw_damage.max(0);
    if ignores_defense {
        damage
    } else {
        (damage - (defense as i128).max(0)).max(0)
    }
}

fn apply_heal(target: &mut SimEntity, raw_heal: i128) -> HealOutcome {
    if !target.alive || target.combat.hp <= 0 {
        let mut killed = false;
        if target.combat.hp <= 0 {
            killed = kill_entity(target);
        }
        return HealOutcome { value: 0, killed };
    }

    let heal = raw_heal.max(0);
    if heal == 0 {
        return HealOutcome::default();
    }

    let max_hp = target.combat.max_hp.max(0) as i128;
    if max_hp == 0 {
        let killed = kill_entity(target);
        return HealOutcome { value: 0, killed };
    }

    let current_hp = (target.combat.hp as i128).clamp(0, max_hp);
    let next_hp = (current_hp + heal).min(max_hp);
    target.combat.hp = next_hp as i32;

    HealOutcome {
        value: (next_hp - current_hp) as i32,
        killed: false,
    }
}

fn kill_entity(entity: &mut SimEntity) -> bool {
    let was_alive = entity.alive;
    entity.combat.hp = 0;
    entity.alive = false;
    was_alive
}

fn entity_target_filter(target_type: SkillTargetType) -> Option<SkillTargetFilter> {
    match target_type {
        SkillTargetType::Ally => Some(SkillTargetFilter::Ally),
        SkillTargetType::Enemy => Some(SkillTargetFilter::Enemy),
        SkillTargetType::AnyEntity => Some(SkillTargetFilter::AnyEntity),
        SkillTargetType::None
        | SkillTargetType::SelfOnly
        | SkillTargetType::Position
        | SkillTargetType::Direction => None,
    }
}

fn is_valid_hit_candidate(
    caster: &SimEntity,
    candidate: &SimEntity,
    filter: SkillTargetFilter,
) -> bool {
    if !candidate.alive || candidate.id == caster.id {
        return false;
    }

    match filter {
        SkillTargetFilter::Ally => candidate.team_id == caster.team_id,
        SkillTargetFilter::Enemy => candidate.team_id != caster.team_id,
        SkillTargetFilter::AnyEntity => true,
    }
}

fn skill_target_distance_squared(
    entity_id: EntityId,
    skill_id: SkillId,
    lhs: Vec2Fp,
    rhs: Vec2Fp,
) -> Result<i128, StepError> {
    lhs.distance_squared_raw(rhs)
        .ok_or(StepError::SkillTargetDistanceOverflow {
            entity_id,
            skill_id,
        })
}

fn range_squared_raw(range: Fp) -> i128 {
    let raw = range.raw() as i128;
    raw * raw
}

fn skill_target_matches(target: SkillTarget, target_type: SkillTargetType) -> bool {
    match target_type {
        SkillTargetType::None | SkillTargetType::SelfOnly => matches!(target, SkillTarget::None),
        SkillTargetType::Ally | SkillTargetType::Enemy | SkillTargetType::AnyEntity => {
            matches!(target, SkillTarget::Entity(_))
        }
        SkillTargetType::Position => matches!(target, SkillTarget::Position(_)),
        SkillTargetType::Direction => matches!(target, SkillTarget::Direction(_)),
    }
}

fn skill_target_type_name(target_type: SkillTargetType) -> &'static str {
    match target_type {
        SkillTargetType::None => "none",
        SkillTargetType::SelfOnly => "self_only",
        SkillTargetType::Ally => "ally",
        SkillTargetType::Enemy => "enemy",
        SkillTargetType::AnyEntity => "any_entity",
        SkillTargetType::Position => "position",
        SkillTargetType::Direction => "direction",
    }
}

fn skill_target_name(target: SkillTarget) -> &'static str {
    match target {
        SkillTarget::None => "none",
        SkillTarget::Entity(_) => "entity",
        SkillTarget::Position(_) => "position",
        SkillTarget::Direction(_) => "direction",
    }
}

fn advance_controlled_entity(entity: &mut SimEntity, config: &SimConfig) -> Result<(), StepError> {
    if entity.movement.mode != MovementMode::Controlled {
        return Ok(());
    }

    let delta_x = movement_delta_raw(
        entity.movement.move_dir.x(),
        entity.movement.speed_per_second,
        config.movement.tick_rate,
        entity.id,
    )?;
    let delta_y = movement_delta_raw(
        entity.movement.move_dir.y(),
        entity.movement.speed_per_second,
        config.movement.tick_rate,
        entity.id,
    )?;

    entity.transform.pos = config.movement.bounds.clamp_raw_center_with_radius(
        entity.transform.pos.x.raw() as i128 + delta_x as i128,
        entity.transform.pos.y.raw() as i128 + delta_y as i128,
        entity.transform.radius,
    );

    Ok(())
}

fn advance_combat_timers(
    world: &mut SimWorld,
    frame: FrameId,
    events: &mut Vec<SimEvent>,
    event_sequence: &mut EventSequence,
    combat_config: &CombatConfig,
) -> Result<(), StepError> {
    let mut pending_ticks = Vec::new();

    for entity in &mut world.entities {
        for slot in &mut entity.combat.skill_slots {
            slot.cooldown_remaining = slot.cooldown_remaining.saturating_sub(1);
        }
    }

    for entity in &mut world.entities {
        let entity_id = entity.id;
        let mut buff_index = 0;

        while buff_index < entity.combat.buffs.len() {
            let expired = {
                let buff = &mut entity.combat.buffs[buff_index];
                let buff_definition =
                    combat_config
                        .buffs
                        .get(buff.buff_id)
                        .ok_or(StepError::UnknownBuff {
                            entity_id,
                            buff_id: buff.buff_id,
                        })?;

                buff.duration_remaining = buff.duration_remaining.saturating_sub(1);
                buff.interval_remaining = buff.interval_remaining.saturating_sub(1);

                let tick_due = buff.interval_remaining == 0;
                let expired = buff.duration_remaining == 0;

                if tick_due {
                    events.push(SimEvent::BuffTick {
                        frame,
                        source_entity: buff.source_entity,
                        target_entity: entity_id,
                        buff_id: buff.buff_id,
                        value: buff.stacks as i32,
                        sequence: event_sequence.next(),
                    });
                    pending_ticks.push(PendingBuffTick {
                        target_id: entity_id,
                        buff_id: buff.buff_id,
                        source_entity: buff.source_entity,
                        stacks: buff.stacks,
                    });

                    if !expired {
                        buff.interval_remaining = buff_definition.interval_frames;
                    }
                }

                if expired {
                    events.push(SimEvent::BuffExpired {
                        frame,
                        source_entity: buff.source_entity,
                        target_entity: entity_id,
                        buff_id: buff.buff_id,
                        value: buff.stacks as i32,
                        sequence: event_sequence.next(),
                    });
                }

                expired
            };

            if expired {
                entity.combat.buffs.remove(buff_index);
            } else {
                buff_index += 1;
            }
        }
    }

    // Buff ticks are collected before applying effects. This freezes entity id
    // order and each entity's slot vector order for the whole frame; buffs
    // created by a tick start participating on the next frame.
    for tick in &pending_ticks {
        apply_pending_buff_tick(world, tick, frame, events, event_sequence, combat_config)?;
    }

    Ok(())
}

fn apply_pending_buff_tick(
    world: &mut SimWorld,
    tick: &PendingBuffTick,
    frame: FrameId,
    events: &mut Vec<SimEvent>,
    event_sequence: &mut EventSequence,
    combat_config: &CombatConfig,
) -> Result<(), StepError> {
    let buff_definition = combat_config
        .buffs
        .get(tick.buff_id)
        .ok_or(StepError::UnknownBuff {
            entity_id: tick.target_id,
            buff_id: tick.buff_id,
        })?;
    let mut source_attack = None;

    for effect in &buff_definition.effects {
        let caster_attack = match effect {
            CombatEffect::Damage { .. } | CombatEffect::Heal { .. } => {
                if let Some(attack) = source_attack {
                    attack
                } else {
                    let attack = world
                        .entity(tick.source_entity)
                        .ok_or(StepError::EntityNotFound {
                            entity_id: tick.source_entity,
                        })?
                        .combat
                        .attack;
                    source_attack = Some(attack);
                    attack
                }
            }
            CombatEffect::AddBuff { .. } => 0,
        };

        apply_combat_effect(
            world,
            tick.source_entity,
            caster_attack,
            tick.target_id,
            effect,
            EffectSource::Buff(tick.buff_id),
            frame,
            events,
            event_sequence,
            combat_config,
            tick.stacks,
        )?;
    }

    Ok(())
}

fn movement_delta_raw(
    dir_component: i16,
    speed_per_second: Fp,
    tick_rate: u16,
    entity_id: EntityId,
) -> Result<i64, StepError> {
    // Fixed movement formula:
    // delta = dir * speed / (FP_SCALE * fps)
    //
    // Rust signed integer division truncates toward zero. That truncation is
    // part of the deterministic movement contract and is covered by tests.
    let denominator = FP_SCALE as i128 * tick_rate as i128;
    let delta = dir_component as i128 * speed_per_second.raw() as i128 / denominator;

    i64::try_from(delta).map_err(|_| StepError::MovementDeltaOverflow { entity_id })
}

fn clamp_axis_with_radius(value: i128, min: i64, max: i64, radius: i64) -> Fp {
    let low = min.min(max) as i128;
    let high = min.max(max) as i128;
    let radius = radius.max(0) as i128;
    let center_low = low + radius;
    let center_high = high - radius;

    if center_low > center_high {
        return Fp::from_raw((low + (high - low) / 2) as i64);
    }

    Fp::from_raw(value.clamp(center_low, center_high) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::combat::{
        BuffDefinition, BuffId, CombatEffect, DamageFormula, SkillDefinition, SkillId,
        SkillTargetType,
    };
    use crate::ids::TeamId;
    use crate::input::{FaceCommand, MoveCommand, SimInputSource};
    use crate::state::{BuffSlot, CombatState, EntityKind, MovementState, SimTransform, SkillSlot};

    fn test_config() -> SimConfig {
        SimConfig {
            movement: MovementConfig {
                tick_rate: 60,
                default_speed_per_second: Fp::from_i32(6),
                max_speed_per_second: Fp::from_i32(10),
                bounds: SceneBounds {
                    min: Vec2Fp::new(Fp::from_i32(-10), Fp::from_i32(-10)),
                    max: Vec2Fp::new(Fp::from_i32(10), Fp::from_i32(10)),
                },
                static_obstacles: Vec::new(),
            },
            combat: CombatConfig::default(),
        }
    }

    fn test_entity(id: u32, pos: Vec2Fp) -> SimEntity {
        SimEntity {
            id: EntityId::new(id),
            kind: EntityKind::Player,
            owner_character_id: Some(format!("chr_{id}")),
            team_id: TeamId::new(1),
            transform: SimTransform {
                pos,
                facing: QuantizedDir::RIGHT,
                radius: Fp::from_milli(500),
            },
            movement: MovementState::default(),
            combat: CombatState::default(),
            alive: true,
        }
    }

    fn input(frame: u32, entity_id: u32, seq: u32, command: SimCommand) -> SimInput {
        SimInput {
            frame: FrameId::new(frame),
            character_id: format!("chr_{entity_id}"),
            entity_id: EntityId::new(entity_id),
            seq,
            source: SimInputSource::Real,
            command,
        }
    }

    fn skill(id: u32, target_type: SkillTargetType) -> SkillDefinition {
        skill_with_range(id, target_type, Fp::from_i32(5))
    }

    fn skill_with_range(id: u32, target_type: SkillTargetType, cast_range: Fp) -> SkillDefinition {
        SkillDefinition {
            id: SkillId::new(id),
            cooldown_frames: 30,
            cast_range,
            target_type,
            effects: Vec::new(),
        }
    }

    fn skill_with_effects(
        id: u32,
        target_type: SkillTargetType,
        effects: Vec<CombatEffect>,
    ) -> SkillDefinition {
        SkillDefinition {
            effects,
            ..skill(id, target_type)
        }
    }

    fn damage_effect(formula: DamageFormula) -> CombatEffect {
        CombatEffect::Damage { formula }
    }

    fn heal_effect(formula: DamageFormula) -> CombatEffect {
        CombatEffect::Heal { formula }
    }

    fn add_buff_effect(buff_id: u32) -> CombatEffect {
        CombatEffect::AddBuff {
            buff_id: BuffId::new(buff_id),
        }
    }

    fn buff_with_effects(
        id: u32,
        duration_frames: u32,
        interval_frames: u32,
        max_stacks: u16,
        effects: Vec<CombatEffect>,
    ) -> BuffDefinition {
        BuffDefinition {
            id: BuffId::new(id),
            duration_frames,
            interval_frames,
            max_stacks,
            effects,
        }
    }

    fn test_config_with_skills(skills: Vec<SkillDefinition>) -> SimConfig {
        test_config_with_combat(skills, Vec::new())
    }

    fn test_config_with_combat(
        skills: Vec<SkillDefinition>,
        buffs: Vec<BuffDefinition>,
    ) -> SimConfig {
        SimConfig {
            combat: CombatConfig::from_definitions(skills, buffs).unwrap(),
            ..test_config()
        }
    }

    fn cast_skill(skill_id: u32, target: SkillTarget) -> SimCommand {
        SimCommand::CastSkill(CastSkillCommand {
            skill_id: SkillId::new(skill_id),
            target,
        })
    }

    fn entity_pos(world: &SimWorld, entity_id: u32) -> (i64, i64) {
        world
            .entity(EntityId::new(entity_id))
            .unwrap()
            .transform
            .pos
            .raw_tuple()
    }

    fn entity_combat(world: &SimWorld, entity_id: u32) -> &CombatState {
        &world.entity(EntityId::new(entity_id)).unwrap().combat
    }

    #[test]
    fn movement_delta_raw_uses_fixed_formula_and_truncates_toward_zero() {
        let entity_id = EntityId::new(100);

        assert_eq!(
            movement_delta_raw(QuantizedDir::RIGHT.x(), Fp::from_i32(6), 60, entity_id).unwrap(),
            100
        );
        assert_eq!(
            movement_delta_raw(QuantizedDir::LEFT.x(), Fp::from_i32(6), 60, entity_id).unwrap(),
            -100
        );
        assert_eq!(
            movement_delta_raw(QuantizedDir::DOWN_RIGHT.x(), Fp::from_i32(1), 60, entity_id)
                .unwrap(),
            11
        );
        assert_eq!(
            movement_delta_raw(QuantizedDir::UP_LEFT.x(), Fp::from_i32(1), 60, entity_id).unwrap(),
            -11
        );
    }

    #[test]
    fn step_moves_horizontal_vertical_and_707_diagonal_by_fixed_formula() {
        let cases = [
            (QuantizedDir::RIGHT, (100, 0)),
            (QuantizedDir::DOWN, (0, 100)),
            (QuantizedDir::UP_RIGHT, (70, -70)),
        ];

        for (dir, expected_pos) in cases {
            let mut world =
                SimWorld::new(FrameId::new(0), vec![test_entity(100, Vec2Fp::zero())]).unwrap();
            let inputs = vec![input(
                1,
                100,
                1,
                SimCommand::Move(MoveCommand {
                    dir,
                    speed_per_second: Some(Fp::from_i32(6)),
                }),
            )];

            step(&mut world, FrameId::new(1), &inputs, &test_config()).unwrap();

            assert_eq!(entity_pos(&world, 100), expected_pos);
        }
    }

    #[test]
    fn step_moves_in_a_straight_line_and_reuses_missing_movement_input() {
        let mut world =
            SimWorld::new(FrameId::new(0), vec![test_entity(100, Vec2Fp::zero())]).unwrap();
        let inputs = vec![
            input(
                1,
                100,
                1,
                SimCommand::Move(MoveCommand {
                    dir: QuantizedDir::RIGHT,
                    speed_per_second: Some(Fp::from_i32(6)),
                }),
            ),
            input(
                1,
                100,
                2,
                SimCommand::Face(FaceCommand {
                    dir: QuantizedDir::LEFT,
                }),
            ),
        ];

        let result = step(&mut world, FrameId::new(1), &inputs, &test_config()).unwrap();

        assert_eq!(result.frame, FrameId::new(1));
        assert!(result.events.is_empty());
        assert_eq!(result.state_hash, hash_world(&world));
        assert_eq!(world.frame, FrameId::new(1));
        assert_eq!(entity_pos(&world, 100), (100, 0));
        let entity = world.entity(EntityId::new(100)).unwrap();
        assert_eq!(entity.movement.mode, MovementMode::Controlled);
        assert_eq!(entity.movement.move_dir, QuantizedDir::RIGHT);
        assert_eq!(entity.transform.facing, QuantizedDir::LEFT);

        step(&mut world, FrameId::new(2), &[], &test_config()).unwrap();

        assert_eq!(world.frame, FrameId::new(2));
        assert_eq!(entity_pos(&world, 100), (200, 0));
    }

    #[test]
    fn step_first_missing_movement_input_keeps_entity_stationary() {
        let mut world =
            SimWorld::new(FrameId::new(0), vec![test_entity(100, Vec2Fp::zero())]).unwrap();

        step(&mut world, FrameId::new(1), &[], &test_config()).unwrap();

        assert_eq!(world.frame, FrameId::new(1));
        assert_eq!(entity_pos(&world, 100), (0, 0));
        assert_eq!(
            world.entity(EntityId::new(100)).unwrap().movement,
            MovementState::default()
        );
    }

    #[test]
    fn step_keeps_initial_skill_slots_and_decrements_cooldowns() {
        let mut entity = test_entity(100, Vec2Fp::zero());
        entity.combat.skill_slots = vec![
            SkillSlot {
                skill_id: SkillId::new(10),
                cooldown_remaining: 2,
            },
            SkillSlot {
                skill_id: SkillId::new(20),
                cooldown_remaining: 0,
            },
        ];
        let mut world = SimWorld::new(FrameId::new(0), vec![entity]).unwrap();

        step(&mut world, FrameId::new(1), &[], &test_config()).unwrap();

        let combat = entity_combat(&world, 100);
        assert_eq!(combat.skill_slots.len(), 2);
        assert_eq!(combat.skill_slots[0].skill_id, SkillId::new(10));
        assert_eq!(combat.skill_slots[0].cooldown_remaining, 1);
        assert_eq!(combat.skill_slots[1].skill_id, SkillId::new(20));
        assert_eq!(combat.skill_slots[1].cooldown_remaining, 0);

        step(&mut world, FrameId::new(2), &[], &test_config()).unwrap();

        let combat = entity_combat(&world, 100);
        assert_eq!(combat.skill_slots[0].cooldown_remaining, 0);
        assert_eq!(combat.skill_slots[1].cooldown_remaining, 0);
    }

    #[test]
    fn step_decrements_buff_timers_and_removes_expired_slots() {
        let mut entity = test_entity(100, Vec2Fp::zero());
        entity.combat.buffs = vec![
            BuffSlot {
                buff_id: BuffId::new(10),
                duration_remaining: 2,
                interval_remaining: 3,
                stacks: 1,
                source_entity: EntityId::new(200),
            },
            BuffSlot {
                buff_id: BuffId::new(20),
                duration_remaining: 1,
                interval_remaining: 2,
                stacks: 2,
                source_entity: EntityId::new(201),
            },
        ];
        let mut world = SimWorld::new(FrameId::new(0), vec![entity]).unwrap();
        let config = test_config_with_combat(
            Vec::new(),
            vec![
                buff_with_effects(10, 10, 3, 1, Vec::new()),
                buff_with_effects(20, 10, 2, 2, Vec::new()),
            ],
        );

        let result = step(&mut world, FrameId::new(1), &[], &config).unwrap();

        let combat = entity_combat(&world, 100);
        assert_eq!(combat.buffs.len(), 1);
        assert_eq!(combat.buffs[0].buff_id, BuffId::new(10));
        assert_eq!(combat.buffs[0].duration_remaining, 1);
        assert_eq!(combat.buffs[0].interval_remaining, 2);
        assert_eq!(combat.buffs[0].stacks, 1);
        assert_eq!(combat.buffs[0].source_entity, EntityId::new(200));
        assert_eq!(
            result.events,
            vec![SimEvent::BuffExpired {
                frame: FrameId::new(1),
                source_entity: EntityId::new(201),
                target_entity: EntityId::new(100),
                buff_id: BuffId::new(20),
                value: 2,
                sequence: 0,
            }]
        );

        let result = step(&mut world, FrameId::new(2), &[], &config).unwrap();

        assert!(entity_combat(&world, 100).buffs.is_empty());
        assert_eq!(
            result.events,
            vec![SimEvent::BuffExpired {
                frame: FrameId::new(2),
                source_entity: EntityId::new(200),
                target_entity: EntityId::new(100),
                buff_id: BuffId::new(10),
                value: 1,
                sequence: 0,
            }]
        );
    }

    #[test]
    fn step_adds_buff_from_skill_and_decrements_new_slot_same_frame() {
        let config = test_config_with_combat(
            vec![skill_with_effects(
                10,
                SkillTargetType::SelfOnly,
                vec![add_buff_effect(100)],
            )],
            vec![buff_with_effects(100, 5, 3, 2, Vec::new())],
        );
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut world = SimWorld::new(FrameId::new(0), vec![caster]).unwrap();
        let inputs = vec![input(1, 100, 1, cast_skill(10, SkillTarget::None))];

        let result = step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        let combat = entity_combat(&world, 100);
        assert_eq!(
            result.events,
            vec![
                SimEvent::SkillCast {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: Some(EntityId::new(100)),
                    skill_id: SkillId::new(10),
                    value: 1,
                    sequence: 0,
                },
                SimEvent::BuffApplied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(100),
                    buff_id: BuffId::new(100),
                    value: 1,
                    sequence: 1,
                },
            ]
        );
        assert_eq!(combat.buffs.len(), 1);
        assert_eq!(combat.buffs[0].buff_id, BuffId::new(100));
        assert_eq!(combat.buffs[0].duration_remaining, 4);
        assert_eq!(combat.buffs[0].interval_remaining, 2);
        assert_eq!(combat.buffs[0].stacks, 1);
        assert_eq!(combat.buffs[0].source_entity, EntityId::new(100));
    }

    #[test]
    fn step_can_tick_cast_added_one_frame_interval_buff_after_same_frame_decrement() {
        let config = test_config_with_combat(
            vec![skill_with_effects(
                10,
                SkillTargetType::SelfOnly,
                vec![add_buff_effect(100)],
            )],
            vec![buff_with_effects(100, 3, 1, 1, Vec::new())],
        );
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut world = SimWorld::new(FrameId::new(0), vec![caster]).unwrap();
        let inputs = vec![input(1, 100, 1, cast_skill(10, SkillTarget::None))];

        let result = step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        let combat = entity_combat(&world, 100);
        assert_eq!(
            result.events,
            vec![
                SimEvent::SkillCast {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: Some(EntityId::new(100)),
                    skill_id: SkillId::new(10),
                    value: 1,
                    sequence: 0,
                },
                SimEvent::BuffApplied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(100),
                    buff_id: BuffId::new(100),
                    value: 1,
                    sequence: 1,
                },
                SimEvent::BuffTick {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(100),
                    buff_id: BuffId::new(100),
                    value: 1,
                    sequence: 2,
                },
            ]
        );
        assert_eq!(combat.buffs.len(), 1);
        assert_eq!(combat.buffs[0].duration_remaining, 2);
        assert_eq!(combat.buffs[0].interval_remaining, 1);
    }

    #[test]
    fn step_reapplies_same_source_buff_refreshes_duration_interval_and_caps_stacks() {
        let config = test_config_with_combat(
            vec![skill_with_effects(
                10,
                SkillTargetType::SelfOnly,
                vec![add_buff_effect(100)],
            )],
            vec![buff_with_effects(100, 6, 4, 2, Vec::new())],
        );
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        caster.combat.buffs = vec![BuffSlot {
            buff_id: BuffId::new(100),
            duration_remaining: 1,
            interval_remaining: 1,
            stacks: 1,
            source_entity: EntityId::new(100),
        }];
        let mut world = SimWorld::new(FrameId::new(0), vec![caster]).unwrap();

        let result = step(
            &mut world,
            FrameId::new(1),
            &[input(1, 100, 1, cast_skill(10, SkillTarget::None))],
            &config,
        )
        .unwrap();

        let combat = entity_combat(&world, 100);
        assert_eq!(
            result.events,
            vec![
                SimEvent::SkillCast {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: Some(EntityId::new(100)),
                    skill_id: SkillId::new(10),
                    value: 1,
                    sequence: 0,
                },
                SimEvent::BuffApplied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(100),
                    buff_id: BuffId::new(100),
                    value: 2,
                    sequence: 1,
                },
            ]
        );
        assert_eq!(combat.buffs.len(), 1);
        assert_eq!(combat.buffs[0].stacks, 2);
        assert_eq!(combat.buffs[0].duration_remaining, 5);
        assert_eq!(combat.buffs[0].interval_remaining, 3);

        world
            .entity_mut(EntityId::new(100))
            .unwrap()
            .combat
            .skill_slots[0]
            .cooldown_remaining = 0;

        step(
            &mut world,
            FrameId::new(2),
            &[input(2, 100, 1, cast_skill(10, SkillTarget::None))],
            &config,
        )
        .unwrap();

        let combat = entity_combat(&world, 100);
        assert_eq!(combat.buffs.len(), 1);
        assert_eq!(combat.buffs[0].stacks, 2);
        assert_eq!(combat.buffs[0].duration_remaining, 5);
        assert_eq!(combat.buffs[0].interval_remaining, 3);
    }

    #[test]
    fn step_keeps_same_buff_from_different_sources_in_separate_slots() {
        let config = test_config_with_combat(
            vec![skill_with_effects(
                10,
                SkillTargetType::Enemy,
                vec![add_buff_effect(100)],
            )],
            vec![buff_with_effects(100, 5, 3, 2, Vec::new())],
        );
        let mut previous_source = test_entity(100, Vec2Fp::zero());
        previous_source.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut new_source = test_entity(300, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        new_source.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(2), Fp::ZERO));
        target.team_id = TeamId::new(2);
        target.combat.buffs = vec![BuffSlot {
            buff_id: BuffId::new(100),
            duration_remaining: 5,
            interval_remaining: 3,
            stacks: 1,
            source_entity: EntityId::new(100),
        }];
        let mut world =
            SimWorld::new(FrameId::new(0), vec![target, new_source, previous_source]).unwrap();
        let inputs = vec![input(
            1,
            300,
            1,
            cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
        )];

        step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        let buffs = &entity_combat(&world, 200).buffs;
        assert_eq!(buffs.len(), 2);
        assert_eq!(buffs[0].source_entity, EntityId::new(100));
        assert_eq!(buffs[0].duration_remaining, 4);
        assert_eq!(buffs[0].interval_remaining, 2);
        assert_eq!(buffs[1].source_entity, EntityId::new(300));
        assert_eq!(buffs[1].duration_remaining, 4);
        assert_eq!(buffs[1].interval_remaining, 2);
    }

    #[test]
    fn step_applies_dot_periodic_damage_multiplied_by_stacks() {
        let config = test_config_with_combat(
            Vec::new(),
            vec![buff_with_effects(
                100,
                5,
                1,
                3,
                vec![damage_effect(DamageFormula::Fixed { amount: 5 })],
            )],
        );
        let source = test_entity(100, Vec2Fp::zero());
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        target.team_id = TeamId::new(2);
        target.combat.hp = 100;
        target.combat.max_hp = 100;
        target.combat.buffs = vec![BuffSlot {
            buff_id: BuffId::new(100),
            duration_remaining: 3,
            interval_remaining: 1,
            stacks: 2,
            source_entity: EntityId::new(100),
        }];
        let mut world = SimWorld::new(FrameId::new(0), vec![target, source]).unwrap();

        let result = step(&mut world, FrameId::new(1), &[], &config).unwrap();

        let combat = entity_combat(&world, 200);
        assert_eq!(
            result.events,
            vec![
                SimEvent::BuffTick {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(200),
                    buff_id: BuffId::new(100),
                    value: 2,
                    sequence: 0,
                },
                SimEvent::DamageApplied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(200),
                    skill_id: None,
                    buff_id: Some(BuffId::new(100)),
                    value: 10,
                    sequence: 1,
                },
            ]
        );
        assert_eq!(combat.hp, 90);
        assert_eq!(combat.buffs[0].duration_remaining, 2);
        assert_eq!(combat.buffs[0].interval_remaining, 1);
    }

    #[test]
    fn step_applies_hot_periodic_heal_multiplied_by_stacks_and_clamped_to_max_hp() {
        let config = test_config_with_combat(
            Vec::new(),
            vec![buff_with_effects(
                100,
                5,
                1,
                3,
                vec![heal_effect(DamageFormula::Fixed { amount: 15 })],
            )],
        );
        let source = test_entity(100, Vec2Fp::zero());
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        target.combat.hp = 80;
        target.combat.max_hp = 100;
        target.combat.buffs = vec![BuffSlot {
            buff_id: BuffId::new(100),
            duration_remaining: 3,
            interval_remaining: 1,
            stacks: 2,
            source_entity: EntityId::new(100),
        }];
        let mut world = SimWorld::new(FrameId::new(0), vec![target, source]).unwrap();

        let result = step(&mut world, FrameId::new(1), &[], &config).unwrap();

        let combat = entity_combat(&world, 200);
        assert_eq!(
            result.events,
            vec![
                SimEvent::BuffTick {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(200),
                    buff_id: BuffId::new(100),
                    value: 2,
                    sequence: 0,
                },
                SimEvent::HealApplied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(200),
                    skill_id: None,
                    buff_id: Some(BuffId::new(100)),
                    value: 20,
                    sequence: 1,
                },
            ]
        );
        assert_eq!(combat.hp, 100);
        assert_eq!(combat.buffs[0].duration_remaining, 2);
        assert_eq!(combat.buffs[0].interval_remaining, 1);
    }

    #[test]
    fn step_emits_buff_tick_events_by_type_source_target_and_sequence() {
        let config = test_config_with_combat(
            Vec::new(),
            vec![
                buff_with_effects(10, 5, 1, 1, Vec::new()),
                buff_with_effects(20, 5, 1, 1, Vec::new()),
                buff_with_effects(30, 5, 1, 1, Vec::new()),
            ],
        );
        let mut later_entity = test_entity(200, Vec2Fp::zero());
        later_entity.combat.buffs = vec![
            BuffSlot {
                buff_id: BuffId::new(20),
                duration_remaining: 3,
                interval_remaining: 1,
                stacks: 1,
                source_entity: EntityId::new(900),
            },
            BuffSlot {
                buff_id: BuffId::new(10),
                duration_remaining: 3,
                interval_remaining: 1,
                stacks: 1,
                source_entity: EntityId::new(901),
            },
        ];
        let mut earlier_entity = test_entity(100, Vec2Fp::zero());
        earlier_entity.combat.buffs = vec![BuffSlot {
            buff_id: BuffId::new(30),
            duration_remaining: 3,
            interval_remaining: 1,
            stacks: 1,
            source_entity: EntityId::new(902),
        }];
        let mut world = SimWorld::new(FrameId::new(0), vec![later_entity, earlier_entity]).unwrap();

        let result = step(&mut world, FrameId::new(1), &[], &config).unwrap();

        assert_eq!(
            result.events,
            vec![
                SimEvent::BuffTick {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(900),
                    target_entity: EntityId::new(200),
                    buff_id: BuffId::new(20),
                    value: 1,
                    sequence: 1,
                },
                SimEvent::BuffTick {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(901),
                    target_entity: EntityId::new(200),
                    buff_id: BuffId::new(10),
                    value: 1,
                    sequence: 2,
                },
                SimEvent::BuffTick {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(902),
                    target_entity: EntityId::new(100),
                    buff_id: BuffId::new(30),
                    value: 1,
                    sequence: 0,
                },
            ]
        );
    }

    #[test]
    fn step_accepts_valid_cast_skill_input_and_starts_cooldown() {
        let config = test_config_with_skills(vec![skill(10, SkillTargetType::Enemy)]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        target.team_id = TeamId::new(2);
        let mut world = SimWorld::new(FrameId::new(0), vec![caster.clone(), target]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
        )];

        let result = step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        assert_eq!(
            result.events,
            vec![SimEvent::SkillCast {
                frame: FrameId::new(1),
                source_entity: EntityId::new(100),
                target_entity: Some(EntityId::new(200)),
                skill_id: SkillId::new(10),
                value: 1,
                sequence: 0,
            }]
        );
        assert_eq!(world.frame, FrameId::new(1));
        assert_eq!(
            entity_combat(&world, 100).skill_slots[0].cooldown_remaining,
            29
        );
    }

    #[test]
    fn step_applies_fixed_damage_after_defense_reduction() {
        let config = test_config_with_skills(vec![skill_with_effects(
            10,
            SkillTargetType::Enemy,
            vec![damage_effect(DamageFormula::Fixed { amount: 20 })],
        )]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        target.team_id = TeamId::new(2);
        target.combat.hp = 100;
        target.combat.max_hp = 100;
        target.combat.defense = 3;
        let mut world = SimWorld::new(FrameId::new(0), vec![caster, target]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
        )];

        let result = step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        assert_eq!(
            result.events,
            vec![
                SimEvent::SkillCast {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: Some(EntityId::new(200)),
                    skill_id: SkillId::new(10),
                    value: 1,
                    sequence: 0,
                },
                SimEvent::DamageApplied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(200),
                    skill_id: Some(SkillId::new(10)),
                    buff_id: None,
                    value: 17,
                    sequence: 1,
                },
            ]
        );
        assert_eq!(entity_combat(&world, 200).hp, 83);
    }

    #[test]
    fn step_applies_scaling_damage_with_basis_points_and_integer_truncation() {
        let config = test_config_with_skills(vec![skill_with_effects(
            10,
            SkillTargetType::Enemy,
            vec![damage_effect(DamageFormula::Scaling {
                base: 5,
                attack_scale_bps: 3_333,
            })],
        )]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.attack = 11;
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        target.team_id = TeamId::new(2);
        target.combat.hp = 100;
        target.combat.max_hp = 100;
        let mut world = SimWorld::new(FrameId::new(0), vec![caster, target]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
        )];

        step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        assert_eq!(entity_combat(&world, 200).hp, 92);
    }

    #[test]
    fn step_applies_true_damage_without_defense_reduction() {
        let config = test_config_with_skills(vec![skill_with_effects(
            10,
            SkillTargetType::Enemy,
            vec![damage_effect(DamageFormula::TrueDamage { amount: 20 })],
        )]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        target.team_id = TeamId::new(2);
        target.combat.hp = 100;
        target.combat.max_hp = 100;
        target.combat.defense = 100;
        let mut world = SimWorld::new(FrameId::new(0), vec![caster, target]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
        )];

        step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        assert_eq!(entity_combat(&world, 200).hp, 80);
    }

    #[test]
    fn step_clamps_defense_reduced_damage_to_zero() {
        let config = test_config_with_skills(vec![skill_with_effects(
            10,
            SkillTargetType::Enemy,
            vec![damage_effect(DamageFormula::Fixed { amount: 15 })],
        )]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        target.team_id = TeamId::new(2);
        target.combat.hp = 100;
        target.combat.max_hp = 100;
        target.combat.defense = 40;
        let mut world = SimWorld::new(FrameId::new(0), vec![caster, target]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
        )];

        let result = step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        assert_eq!(
            result.events,
            vec![SimEvent::SkillCast {
                frame: FrameId::new(1),
                source_entity: EntityId::new(100),
                target_entity: Some(EntityId::new(200)),
                skill_id: SkillId::new(10),
                value: 1,
                sequence: 0,
            }]
        );
        assert_eq!(entity_combat(&world, 200).hp, 100);
    }

    #[test]
    fn step_applies_heal_without_exceeding_max_hp() {
        let config = test_config_with_skills(vec![skill_with_effects(
            10,
            SkillTargetType::SelfOnly,
            vec![heal_effect(DamageFormula::Fixed { amount: 20 })],
        )]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.hp = 95;
        caster.combat.max_hp = 100;
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut world = SimWorld::new(FrameId::new(0), vec![caster]).unwrap();
        let inputs = vec![input(1, 100, 1, cast_skill(10, SkillTarget::None))];

        let result = step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        let combat = entity_combat(&world, 100);
        assert_eq!(
            result.events,
            vec![
                SimEvent::SkillCast {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: Some(EntityId::new(100)),
                    skill_id: SkillId::new(10),
                    value: 1,
                    sequence: 0,
                },
                SimEvent::HealApplied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(100),
                    skill_id: Some(SkillId::new(10)),
                    buff_id: None,
                    value: 5,
                    sequence: 1,
                },
            ]
        );
        assert_eq!(combat.hp, 100);
        assert_eq!(combat.skill_slots[0].cooldown_remaining, 29);
    }

    #[test]
    fn step_does_not_revive_dead_entity_with_heal() {
        let config = test_config_with_skills(vec![skill_with_effects(
            10,
            SkillTargetType::SelfOnly,
            vec![heal_effect(DamageFormula::Fixed { amount: 20 })],
        )]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.alive = false;
        caster.combat.hp = 0;
        caster.combat.max_hp = 100;
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut world = SimWorld::new(FrameId::new(0), vec![caster]).unwrap();
        let inputs = vec![input(1, 100, 1, cast_skill(10, SkillTarget::None))];

        step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        let caster = world.entity(EntityId::new(100)).unwrap();
        assert_eq!(caster.combat.hp, 0);
        assert!(!caster.alive);
    }

    #[test]
    fn step_sets_alive_false_and_clamps_hp_to_zero_when_damage_kills() {
        let config = test_config_with_skills(vec![skill_with_effects(
            10,
            SkillTargetType::Enemy,
            vec![damage_effect(DamageFormula::Fixed { amount: 15 })],
        )]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        target.team_id = TeamId::new(2);
        target.combat.hp = 10;
        target.combat.max_hp = 100;
        let mut world = SimWorld::new(FrameId::new(0), vec![caster, target]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
        )];

        let result = step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        let target = world.entity(EntityId::new(200)).unwrap();
        assert_eq!(
            result.events,
            vec![
                SimEvent::SkillCast {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: Some(EntityId::new(200)),
                    skill_id: SkillId::new(10),
                    value: 1,
                    sequence: 0,
                },
                SimEvent::DamageApplied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(200),
                    skill_id: Some(SkillId::new(10)),
                    buff_id: None,
                    value: 10,
                    sequence: 1,
                },
                SimEvent::EntityDied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(200),
                    skill_id: Some(SkillId::new(10)),
                    buff_id: None,
                    value: 0,
                    sequence: 2,
                },
            ]
        );
        assert_eq!(target.combat.hp, 0);
        assert!(!target.alive);
    }

    #[test]
    fn step_applies_same_frame_casts_in_selected_input_order() {
        let config = test_config_with_skills(vec![
            skill_with_effects(
                10,
                SkillTargetType::Enemy,
                vec![heal_effect(DamageFormula::Fixed { amount: 10 })],
            ),
            skill_with_effects(
                20,
                SkillTargetType::Enemy,
                vec![damage_effect(DamageFormula::Fixed { amount: 15 })],
            ),
        ]);
        let mut first_caster = test_entity(100, Vec2Fp::zero());
        first_caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut second_caster = test_entity(300, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        second_caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(20),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(2), Fp::ZERO));
        target.team_id = TeamId::new(2);
        target.combat.hp = 10;
        target.combat.max_hp = 20;
        let mut world =
            SimWorld::new(FrameId::new(0), vec![second_caster, target, first_caster]).unwrap();
        let inputs = vec![
            input(
                1,
                300,
                1,
                cast_skill(20, SkillTarget::Entity(EntityId::new(200))),
            ),
            input(
                1,
                100,
                1,
                cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
            ),
        ];

        step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        let target = world.entity(EntityId::new(200)).unwrap();
        assert_eq!(target.combat.hp, 5);
        assert!(target.alive);
    }

    #[test]
    fn step_sorts_same_frame_events_by_type_source_target_and_sequence() {
        let config = test_config_with_combat(
            vec![
                skill_with_effects(
                    10,
                    SkillTargetType::Enemy,
                    vec![damage_effect(DamageFormula::Fixed { amount: 4 })],
                ),
                skill_with_effects(
                    20,
                    SkillTargetType::Position,
                    vec![damage_effect(DamageFormula::Fixed { amount: 3 })],
                ),
            ],
            vec![buff_with_effects(
                100,
                3,
                1,
                1,
                vec![heal_effect(DamageFormula::Fixed { amount: 1 })],
            )],
        );
        let mut first_caster = test_entity(100, Vec2Fp::zero());
        first_caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut second_caster = test_entity(300, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        second_caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(20),
            cooldown_remaining: 0,
        }];
        let mut nearer_target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        nearer_target.team_id = TeamId::new(2);
        nearer_target.combat.hp = 10;
        nearer_target.combat.max_hp = 20;
        nearer_target.combat.buffs = vec![BuffSlot {
            buff_id: BuffId::new(100),
            duration_remaining: 3,
            interval_remaining: 1,
            stacks: 1,
            source_entity: EntityId::new(400),
        }];
        let mut farther_target = test_entity(250, Vec2Fp::new(Fp::from_i32(2), Fp::ZERO));
        farther_target.team_id = TeamId::new(2);
        farther_target.combat.hp = 10;
        farther_target.combat.max_hp = 20;
        let buff_source = test_entity(400, Vec2Fp::new(Fp::from_i32(3), Fp::ZERO));
        let mut world = SimWorld::new(
            FrameId::new(0),
            vec![
                farther_target,
                buff_source,
                second_caster,
                nearer_target,
                first_caster,
            ],
        )
        .unwrap();
        let inputs = vec![
            input(
                1,
                300,
                1,
                cast_skill(
                    20,
                    SkillTarget::Position(Vec2Fp::new(Fp::from_i32(1), Fp::ZERO)),
                ),
            ),
            input(
                1,
                100,
                1,
                cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
            ),
        ];

        let result = step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        assert_eq!(
            result.events,
            vec![
                SimEvent::SkillCast {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: Some(EntityId::new(200)),
                    skill_id: SkillId::new(10),
                    value: 1,
                    sequence: 0,
                },
                SimEvent::SkillCast {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(300),
                    target_entity: Some(EntityId::new(200)),
                    skill_id: SkillId::new(20),
                    value: 2,
                    sequence: 2,
                },
                SimEvent::BuffTick {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(400),
                    target_entity: EntityId::new(200),
                    buff_id: BuffId::new(100),
                    value: 1,
                    sequence: 5,
                },
                SimEvent::DamageApplied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(100),
                    target_entity: EntityId::new(200),
                    skill_id: Some(SkillId::new(10)),
                    buff_id: None,
                    value: 4,
                    sequence: 1,
                },
                SimEvent::DamageApplied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(300),
                    target_entity: EntityId::new(200),
                    skill_id: Some(SkillId::new(20)),
                    buff_id: None,
                    value: 3,
                    sequence: 3,
                },
                SimEvent::DamageApplied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(300),
                    target_entity: EntityId::new(250),
                    skill_id: Some(SkillId::new(20)),
                    buff_id: None,
                    value: 3,
                    sequence: 4,
                },
                SimEvent::HealApplied {
                    frame: FrameId::new(1),
                    source_entity: EntityId::new(400),
                    target_entity: EntityId::new(200),
                    skill_id: None,
                    buff_id: Some(BuffId::new(100)),
                    value: 1,
                    sequence: 6,
                },
            ]
        );
        assert_eq!(entity_combat(&world, 200).hp, 4);
        assert_eq!(entity_combat(&world, 250).hp, 7);
    }

    #[test]
    fn step_applies_skill_effects_in_vector_order() {
        let config = test_config_with_skills(vec![skill_with_effects(
            10,
            SkillTargetType::Enemy,
            vec![
                damage_effect(DamageFormula::Fixed { amount: 15 }),
                heal_effect(DamageFormula::Fixed { amount: 10 }),
            ],
        )]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        target.team_id = TeamId::new(2);
        target.combat.hp = 10;
        target.combat.max_hp = 20;
        let mut world = SimWorld::new(FrameId::new(0), vec![caster, target]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
        )];

        step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        let target = world.entity(EntityId::new(200)).unwrap();
        assert_eq!(target.combat.hp, 0);
        assert!(!target.alive);
    }

    #[test]
    fn step_accepts_entity_target_at_melee_range_boundary() {
        let config = test_config_with_skills(vec![skill_with_range(
            10,
            SkillTargetType::Enemy,
            Fp::from_i32(2),
        )]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(2), Fp::ZERO));
        target.team_id = TeamId::new(2);
        let mut world = SimWorld::new(FrameId::new(0), vec![caster, target]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
        )];

        step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        assert_eq!(world.frame, FrameId::new(1));
    }

    #[test]
    fn step_rejects_entity_target_out_of_melee_range_without_updating_world() {
        let config = test_config_with_skills(vec![skill_with_range(
            10,
            SkillTargetType::Enemy,
            Fp::from_i32(2),
        )]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_milli(2_001), Fp::ZERO));
        target.team_id = TeamId::new(2);
        let mut world = SimWorld::new(FrameId::new(0), vec![caster, target]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
        )];

        let error = step(&mut world, FrameId::new(1), &inputs, &config).unwrap_err();

        assert_eq!(
            error,
            StepError::SkillTargetOutOfRange {
                entity_id: EntityId::new(100),
                skill_id: SkillId::new(10),
                target_entity_id: EntityId::new(200),
                distance_squared: 4_004_001,
                range_squared: 4_000_000,
            }
        );
        assert_eq!(world.frame, FrameId::new(0));
        assert_eq!(entity_pos(&world, 100), (0, 0));
    }

    #[test]
    fn step_rejects_dead_entity_target_without_updating_world() {
        let config = test_config_with_skills(vec![skill(10, SkillTargetType::Enemy)]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        target.team_id = TeamId::new(2);
        target.alive = false;
        let mut world = SimWorld::new(FrameId::new(0), vec![caster, target]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
        )];

        let error = step(&mut world, FrameId::new(1), &inputs, &config).unwrap_err();

        assert_eq!(
            error,
            StepError::InvalidSkillTarget {
                entity_id: EntityId::new(100),
                skill_id: SkillId::new(10),
                target_entity_id: EntityId::new(200),
            }
        );
        assert_eq!(world.frame, FrameId::new(0));
    }

    #[test]
    fn step_rejects_enemy_skill_against_same_team_target_without_updating_world() {
        let config = test_config_with_skills(vec![skill(10, SkillTargetType::Enemy)]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        let mut world = SimWorld::new(FrameId::new(0), vec![caster, target]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
        )];

        let error = step(&mut world, FrameId::new(1), &inputs, &config).unwrap_err();

        assert_eq!(
            error,
            StepError::InvalidSkillTarget {
                entity_id: EntityId::new(100),
                skill_id: SkillId::new(10),
                target_entity_id: EntityId::new(200),
            }
        );
        assert_eq!(world.frame, FrameId::new(0));
    }

    #[test]
    fn resolve_position_target_aoe_filters_and_sorts_candidates() {
        let skill = skill_with_range(10, SkillTargetType::Position, Fp::from_i32(3));
        let caster = test_entity(100, Vec2Fp::new(Fp::from_i32(9), Fp::ZERO));
        let mut friendly = test_entity(120, Vec2Fp::new(Fp::from_milli(500), Fp::ZERO));
        friendly.team_id = TeamId::new(1);
        let mut dead_enemy = test_entity(130, Vec2Fp::zero());
        dead_enemy.team_id = TeamId::new(2);
        dead_enemy.alive = false;
        let mut out_of_range_enemy = test_entity(140, Vec2Fp::new(Fp::from_i32(4), Fp::ZERO));
        out_of_range_enemy.team_id = TeamId::new(2);
        let mut enemy_a = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        enemy_a.team_id = TeamId::new(2);
        let mut enemy_b = test_entity(150, Vec2Fp::new(Fp::from_i32(-1), Fp::ZERO));
        enemy_b.team_id = TeamId::new(2);
        let mut enemy_c = test_entity(300, Vec2Fp::new(Fp::from_i32(2), Fp::ZERO));
        enemy_c.team_id = TeamId::new(2);
        let mut enemy_d = test_entity(250, Vec2Fp::new(Fp::ZERO, Fp::from_i32(3)));
        enemy_d.team_id = TeamId::new(2);
        let world = SimWorld::new(
            FrameId::new(0),
            vec![
                caster,
                friendly,
                dead_enemy,
                out_of_range_enemy,
                enemy_a,
                enemy_b,
                enemy_c,
                enemy_d,
            ],
        )
        .unwrap();
        let caster = world.entity(EntityId::new(100)).unwrap();

        let resolved = resolve_skill_targets(
            EntityId::new(100),
            SkillTarget::Position(Vec2Fp::zero()),
            caster,
            &skill,
            &world,
        )
        .unwrap();

        assert_eq!(
            resolved
                .targets
                .iter()
                .map(|target| (target.entity_id.raw(), target.distance_squared))
                .collect::<Vec<_>>(),
            vec![
                (150, 1_000_000),
                (200, 1_000_000),
                (300, 4_000_000),
                (250, 9_000_000),
            ]
        );
    }

    #[test]
    fn resolve_entity_targets_apply_ally_any_and_self_filters() {
        let ally_skill = skill(10, SkillTargetType::Ally);
        let any_skill = skill(20, SkillTargetType::AnyEntity);
        let caster = test_entity(100, Vec2Fp::zero());
        let ally = test_entity(120, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        let mut enemy = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        enemy.team_id = TeamId::new(2);
        let world = SimWorld::new(FrameId::new(0), vec![caster, ally, enemy]).unwrap();
        let caster = world.entity(EntityId::new(100)).unwrap();

        let ally_targets = resolve_skill_targets(
            EntityId::new(100),
            SkillTarget::Entity(EntityId::new(120)),
            caster,
            &ally_skill,
            &world,
        )
        .unwrap();
        let any_targets = resolve_skill_targets(
            EntityId::new(100),
            SkillTarget::Entity(EntityId::new(200)),
            caster,
            &any_skill,
            &world,
        )
        .unwrap();
        let self_error = resolve_skill_targets(
            EntityId::new(100),
            SkillTarget::Entity(EntityId::new(100)),
            caster,
            &any_skill,
            &world,
        )
        .unwrap_err();

        assert_eq!(
            ally_targets.targets,
            vec![HitTarget {
                entity_id: EntityId::new(120),
                distance_squared: 1_000_000,
            }]
        );
        assert_eq!(
            any_targets.targets,
            vec![HitTarget {
                entity_id: EntityId::new(200),
                distance_squared: 1_000_000,
            }]
        );
        assert_eq!(
            self_error,
            StepError::InvalidSkillTarget {
                entity_id: EntityId::new(100),
                skill_id: SkillId::new(20),
                target_entity_id: EntityId::new(100),
            }
        );
    }

    #[test]
    fn step_rejects_unknown_skill_without_updating_world() {
        let mut entity = test_entity(100, Vec2Fp::zero());
        entity.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(999),
            cooldown_remaining: 0,
        }];
        let mut world = SimWorld::new(FrameId::new(0), vec![entity]).unwrap();
        let inputs = vec![input(1, 100, 1, cast_skill(999, SkillTarget::None))];

        let error = step(&mut world, FrameId::new(1), &inputs, &test_config()).unwrap_err();

        assert_eq!(
            error,
            StepError::UnknownSkill {
                entity_id: EntityId::new(100),
                skill_id: SkillId::new(999),
            }
        );
        assert_eq!(world.frame, FrameId::new(0));
        assert_eq!(entity_pos(&world, 100), (0, 0));
    }

    #[test]
    fn step_rejects_unequipped_skill_without_updating_world() {
        let config = test_config_with_skills(vec![skill(10, SkillTargetType::None)]);
        let mut entity = test_entity(100, Vec2Fp::zero());
        entity.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(20),
            cooldown_remaining: 0,
        }];
        let mut world = SimWorld::new(FrameId::new(0), vec![entity]).unwrap();
        let inputs = vec![input(1, 100, 1, cast_skill(10, SkillTarget::None))];

        let error = step(&mut world, FrameId::new(1), &inputs, &config).unwrap_err();

        assert_eq!(
            error,
            StepError::SkillNotEquipped {
                entity_id: EntityId::new(100),
                skill_id: SkillId::new(10),
            }
        );
        assert_eq!(world.frame, FrameId::new(0));
        assert_eq!(entity_pos(&world, 100), (0, 0));
    }

    #[test]
    fn step_rejects_skill_on_cooldown_without_updating_world() {
        let config = test_config_with_skills(vec![skill(10, SkillTargetType::None)]);
        let mut entity = test_entity(100, Vec2Fp::zero());
        entity.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 3,
        }];
        let mut world = SimWorld::new(FrameId::new(0), vec![entity]).unwrap();
        let inputs = vec![input(1, 100, 1, cast_skill(10, SkillTarget::None))];

        let error = step(&mut world, FrameId::new(1), &inputs, &config).unwrap_err();

        assert_eq!(
            error,
            StepError::SkillOnCooldown {
                entity_id: EntityId::new(100),
                skill_id: SkillId::new(10),
                cooldown_remaining: 3,
            }
        );
        assert_eq!(world.frame, FrameId::new(0));
        assert_eq!(
            entity_combat(&world, 100).skill_slots[0].cooldown_remaining,
            3
        );
    }

    #[test]
    fn step_rejects_mismatched_skill_target_type_without_updating_world() {
        let config = test_config_with_skills(vec![skill(10, SkillTargetType::Position)]);
        let mut entity = test_entity(100, Vec2Fp::zero());
        entity.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut world = SimWorld::new(FrameId::new(0), vec![entity]).unwrap();
        let actual = SkillTarget::Direction(QuantizedDir::RIGHT);
        let inputs = vec![input(1, 100, 1, cast_skill(10, actual))];

        let error = step(&mut world, FrameId::new(1), &inputs, &config).unwrap_err();

        assert_eq!(
            error,
            StepError::SkillTargetTypeMismatch {
                entity_id: EntityId::new(100),
                skill_id: SkillId::new(10),
                expected: SkillTargetType::Position,
                actual,
            }
        );
        assert_eq!(world.frame, FrameId::new(0));
        assert_eq!(entity_pos(&world, 100), (0, 0));
    }

    #[test]
    fn step_validates_latest_cast_skill_input_per_character() {
        let config = test_config_with_skills(vec![
            skill(10, SkillTargetType::None),
            skill(20, SkillTargetType::Position),
            skill(30, SkillTargetType::Direction),
        ]);
        let mut entity = test_entity(100, Vec2Fp::zero());
        entity.combat.skill_slots = vec![
            SkillSlot {
                skill_id: SkillId::new(10),
                cooldown_remaining: 0,
            },
            SkillSlot {
                skill_id: SkillId::new(20),
                cooldown_remaining: 0,
            },
            SkillSlot {
                skill_id: SkillId::new(30),
                cooldown_remaining: 0,
            },
        ];
        let mut world = SimWorld::new(FrameId::new(0), vec![entity]).unwrap();
        let inputs = vec![
            input(
                1,
                100,
                1,
                cast_skill(20, SkillTarget::Direction(QuantizedDir::RIGHT)),
            ),
            input(1, 100, 2, cast_skill(10, SkillTarget::None)),
            input(
                1,
                100,
                2,
                cast_skill(30, SkillTarget::Direction(QuantizedDir::LEFT)),
            ),
        ];

        step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        assert_eq!(world.frame, FrameId::new(1));
        assert_eq!(entity_pos(&world, 100), (0, 0));
    }

    #[test]
    fn step_reuses_previous_movement_state_across_consecutive_missing_inputs() {
        let mut world =
            SimWorld::new(FrameId::new(0), vec![test_entity(100, Vec2Fp::zero())]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::RIGHT,
                speed_per_second: Some(Fp::from_i32(6)),
            }),
        )];

        step(&mut world, FrameId::new(1), &inputs, &test_config()).unwrap();
        step(&mut world, FrameId::new(2), &[], &test_config()).unwrap();
        step(&mut world, FrameId::new(3), &[], &test_config()).unwrap();
        step(&mut world, FrameId::new(4), &[], &test_config()).unwrap();

        assert_eq!(world.frame, FrameId::new(4));
        assert_eq!(entity_pos(&world, 100), (400, 0));
        let entity = world.entity(EntityId::new(100)).unwrap();
        assert_eq!(entity.movement.mode, MovementMode::Controlled);
        assert_eq!(entity.movement.move_dir, QuantizedDir::RIGHT);
        assert_eq!(entity.movement.speed_per_second, Fp::from_i32(6));
    }

    #[test]
    fn step_move_without_speed_uses_config_default_when_entity_is_stopped() {
        let mut world =
            SimWorld::new(FrameId::new(0), vec![test_entity(100, Vec2Fp::zero())]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::RIGHT,
                speed_per_second: None,
            }),
        )];

        step(&mut world, FrameId::new(1), &inputs, &test_config()).unwrap();

        assert_eq!(entity_pos(&world, 100), (100, 0));
        assert_eq!(
            world
                .entity(EntityId::new(100))
                .unwrap()
                .movement
                .speed_per_second,
            Fp::from_i32(6)
        );
    }

    #[test]
    fn step_move_without_speed_reuses_entity_positive_speed() {
        let mut entity = test_entity(100, Vec2Fp::zero());
        entity.movement = MovementState {
            mode: MovementMode::Controlled,
            move_dir: QuantizedDir::RIGHT,
            speed_per_second: Fp::from_i32(8),
        };
        let mut world = SimWorld::new(FrameId::new(0), vec![entity]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::DOWN,
                speed_per_second: None,
            }),
        )];

        step(&mut world, FrameId::new(1), &inputs, &test_config()).unwrap();

        assert_eq!(entity_pos(&world, 100), (0, 133));
        let entity = world.entity(EntityId::new(100)).unwrap();
        assert_eq!(entity.movement.move_dir, QuantizedDir::DOWN);
        assert_eq!(entity.movement.speed_per_second, Fp::from_i32(8));
    }

    #[test]
    fn step_hash_is_stable_across_matching_worlds_and_frames() {
        let mut world_a =
            SimWorld::new(FrameId::new(0), vec![test_entity(100, Vec2Fp::zero())]).unwrap();
        let mut world_b = world_a.clone();
        let inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::RIGHT,
                speed_per_second: Some(Fp::from_i32(6)),
            }),
        )];

        let first_a = step(&mut world_a, FrameId::new(1), &inputs, &test_config()).unwrap();
        let first_b = step(&mut world_b, FrameId::new(1), &inputs, &test_config()).unwrap();

        assert_eq!(first_a.state_hash, first_b.state_hash);

        let second_a = step(&mut world_a, FrameId::new(2), &[], &test_config()).unwrap();
        let second_b = step(&mut world_b, FrameId::new(2), &[], &test_config()).unwrap();

        assert_eq!(second_a.state_hash, second_b.state_hash);
        assert_ne!(second_a.state_hash, first_a.state_hash);
        assert_eq!(second_a.state_hash, hash_world(&world_a));
    }

    #[test]
    fn step_state_hash_comes_from_world_state_not_events() {
        let config = test_config_with_skills(vec![skill_with_effects(
            10,
            SkillTargetType::Enemy,
            vec![damage_effect(DamageFormula::Fixed { amount: 3 })],
        )]);
        let mut caster = test_entity(100, Vec2Fp::zero());
        caster.combat.skill_slots = vec![SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 0,
        }];
        let mut target = test_entity(200, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO));
        target.team_id = TeamId::new(2);
        target.combat.hp = 10;
        target.combat.max_hp = 10;
        let mut world = SimWorld::new(FrameId::new(0), vec![caster, target]).unwrap();

        let result = step(
            &mut world,
            FrameId::new(1),
            &[input(
                1,
                100,
                1,
                cast_skill(10, SkillTarget::Entity(EntityId::new(200))),
            )],
            &config,
        )
        .unwrap();

        assert!(!result.events.is_empty());
        assert_eq!(result.state_hash, hash_world(&world));
    }

    #[test]
    fn step_stop_keeps_entity_idle_until_a_new_move_input_arrives() {
        let mut entity = test_entity(100, Vec2Fp::zero());
        entity.movement = MovementState {
            mode: MovementMode::Controlled,
            move_dir: QuantizedDir::RIGHT,
            speed_per_second: Fp::from_i32(6),
        };
        let mut world = SimWorld::new(FrameId::new(0), vec![entity]).unwrap();
        let inputs = vec![input(1, 100, 1, SimCommand::Stop)];

        step(&mut world, FrameId::new(1), &inputs, &test_config()).unwrap();

        assert_eq!(entity_pos(&world, 100), (0, 0));
        let entity = world.entity(EntityId::new(100)).unwrap();
        assert_eq!(entity.movement.mode, MovementMode::Idle);
        assert_eq!(entity.movement.move_dir, QuantizedDir::ZERO);
        assert_eq!(entity.movement.speed_per_second, Fp::ZERO);

        step(&mut world, FrameId::new(2), &[], &test_config()).unwrap();

        assert_eq!(entity_pos(&world, 100), (0, 0));
        assert_eq!(
            world.entity(EntityId::new(100)).unwrap().movement.mode,
            MovementMode::Idle
        );
        assert_eq!(
            world
                .entity(EntityId::new(100))
                .unwrap()
                .movement
                .speed_per_second,
            Fp::ZERO
        );
    }

    #[test]
    fn step_stop_after_movement_makes_following_missing_inputs_stationary() {
        let mut world =
            SimWorld::new(FrameId::new(0), vec![test_entity(100, Vec2Fp::zero())]).unwrap();
        let move_inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::RIGHT,
                speed_per_second: Some(Fp::from_i32(6)),
            }),
        )];
        let stop_inputs = vec![input(3, 100, 2, SimCommand::Stop)];

        step(&mut world, FrameId::new(1), &move_inputs, &test_config()).unwrap();
        step(&mut world, FrameId::new(2), &[], &test_config()).unwrap();
        step(&mut world, FrameId::new(3), &stop_inputs, &test_config()).unwrap();
        step(&mut world, FrameId::new(4), &[], &test_config()).unwrap();
        step(&mut world, FrameId::new(5), &[], &test_config()).unwrap();

        assert_eq!(world.frame, FrameId::new(5));
        assert_eq!(entity_pos(&world, 100), (200, 0));
        assert_eq!(
            world.entity(EntityId::new(100)).unwrap().movement,
            MovementState::default()
        );
    }

    #[test]
    fn step_clamps_position_to_scene_bounds() {
        let config = test_config();
        let mut world = SimWorld::new(
            FrameId::new(0),
            vec![test_entity(
                100,
                Vec2Fp::new(Fp::from_milli(9_950), Fp::ZERO),
            )],
        )
        .unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::RIGHT,
                speed_per_second: None,
            }),
        )];

        step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        assert_eq!(entity_pos(&world, 100), (9_500, 0));
        assert_eq!(
            world.entity(EntityId::new(100)).unwrap().transform.pos.x,
            config
                .movement
                .bounds
                .max
                .x
                .checked_sub(Fp::from_milli(500))
                .unwrap()
        );
    }

    #[test]
    fn step_clamps_position_to_scene_min_bounds_with_entity_radius() {
        let config = test_config();
        let mut world = SimWorld::new(
            FrameId::new(0),
            vec![test_entity(
                100,
                Vec2Fp::new(Fp::from_milli(-9_950), Fp::from_milli(-9_950)),
            )],
        )
        .unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::UP_LEFT,
                speed_per_second: Some(Fp::from_i32(6)),
            }),
        )];

        step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        assert_eq!(entity_pos(&world, 100), (-9_500, -9_500));
        assert_eq!(
            world.entity(EntityId::new(100)).unwrap().transform.pos,
            Vec2Fp::new(
                config
                    .movement
                    .bounds
                    .min
                    .x
                    .checked_add(Fp::from_milli(500))
                    .unwrap(),
                config
                    .movement
                    .bounds
                    .min
                    .y
                    .checked_add(Fp::from_milli(500))
                    .unwrap(),
            )
        );
    }

    #[test]
    fn step_clamps_near_boundary_so_entity_radius_stays_inside() {
        let config = test_config();
        let mut entity = test_entity(
            100,
            Vec2Fp::new(Fp::from_milli(9_450), Fp::from_milli(9_450)),
        );
        entity.transform.radius = Fp::from_milli(750);
        let mut world = SimWorld::new(FrameId::new(0), vec![entity]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::DOWN_RIGHT,
                speed_per_second: Some(Fp::from_i32(6)),
            }),
        )];

        step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        let entity = world.entity(EntityId::new(100)).unwrap();
        assert_eq!(entity.transform.pos.raw_tuple(), (9_250, 9_250));
        assert!(entity.transform.pos.x.raw() + entity.transform.radius.raw() <= 10_000);
        assert!(entity.transform.pos.y.raw() + entity.transform.radius.raw() <= 10_000);
    }

    #[test]
    fn step_collapses_oversized_radius_axis_to_bounds_midpoint() {
        let config = SimConfig {
            movement: MovementConfig {
                tick_rate: 60,
                default_speed_per_second: Fp::from_i32(6),
                max_speed_per_second: Fp::from_i32(10),
                bounds: SceneBounds {
                    min: Vec2Fp::new(Fp::from_i32(0), Fp::from_i32(-10)),
                    max: Vec2Fp::new(Fp::from_i32(1), Fp::from_i32(10)),
                },
                static_obstacles: Vec::new(),
            },
            combat: CombatConfig::default(),
        };
        let mut entity = test_entity(100, Vec2Fp::zero());
        entity.transform.radius = Fp::from_i32(1);
        let mut world = SimWorld::new(FrameId::new(0), vec![entity]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::RIGHT,
                speed_per_second: Some(Fp::from_i32(6)),
            }),
        )];

        step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        assert_eq!(entity_pos(&world, 100), (500, 0));
    }

    #[test]
    fn static_obstacles_are_reserved_and_do_not_affect_p1_movement_or_hash() {
        let config_without_obstacles = test_config();
        let mut config_with_obstacles = config_without_obstacles.clone();
        config_with_obstacles.movement.static_obstacles.extend([
            StaticObstacle {
                shape: StaticObstacleShape::Circle {
                    center: Vec2Fp::new(Fp::from_milli(100), Fp::ZERO),
                    radius: Fp::from_i32(2),
                },
            },
            StaticObstacle {
                shape: StaticObstacleShape::AxisAlignedRect {
                    min: Vec2Fp::new(Fp::from_milli(50), Fp::from_milli(-500)),
                    max: Vec2Fp::new(Fp::from_milli(200), Fp::from_milli(500)),
                },
            },
        ]);
        let mut world_without_obstacles =
            SimWorld::new(FrameId::new(0), vec![test_entity(100, Vec2Fp::zero())]).unwrap();
        let mut world_with_obstacles = world_without_obstacles.clone();
        let inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::RIGHT,
                speed_per_second: Some(Fp::from_i32(6)),
            }),
        )];

        let result_without_obstacles = step(
            &mut world_without_obstacles,
            FrameId::new(1),
            &inputs,
            &config_without_obstacles,
        )
        .unwrap();
        let result_with_obstacles = step(
            &mut world_with_obstacles,
            FrameId::new(1),
            &inputs,
            &config_with_obstacles,
        )
        .unwrap();

        assert_eq!(world_without_obstacles, world_with_obstacles);
        assert_eq!(
            result_without_obstacles.state_hash,
            result_with_obstacles.state_hash
        );
        assert_eq!(
            result_with_obstacles.state_hash,
            hash_world(&world_with_obstacles)
        );
    }

    #[test]
    fn step_rejects_non_sequential_frame_without_updating_world() {
        let mut world =
            SimWorld::new(FrameId::new(2), vec![test_entity(100, Vec2Fp::zero())]).unwrap();

        let error = step(&mut world, FrameId::new(4), &[], &test_config()).unwrap_err();

        assert_eq!(
            error,
            StepError::NonSequentialFrame {
                expected: FrameId::new(3),
                actual: FrameId::new(4),
            }
        );
        assert_eq!(world.frame, FrameId::new(2));
        assert_eq!(entity_pos(&world, 100), (0, 0));
    }

    #[test]
    fn step_rejects_zero_or_negative_move_speed_without_updating_world() {
        let cases = [Fp::ZERO, Fp::from_milli(-1)];

        for speed_per_second in cases {
            let mut world =
                SimWorld::new(FrameId::new(0), vec![test_entity(100, Vec2Fp::zero())]).unwrap();
            let inputs = vec![input(
                1,
                100,
                1,
                SimCommand::Move(MoveCommand {
                    dir: QuantizedDir::RIGHT,
                    speed_per_second: Some(speed_per_second),
                }),
            )];

            let error = step(&mut world, FrameId::new(1), &inputs, &test_config()).unwrap_err();

            assert_eq!(
                error,
                StepError::InvalidMovementSpeed {
                    entity_id: EntityId::new(100),
                    speed_per_second,
                }
            );
            assert_eq!(world.frame, FrameId::new(0));
            assert_eq!(entity_pos(&world, 100), (0, 0));
            assert_eq!(
                world.entity(EntityId::new(100)).unwrap().movement,
                MovementState::default()
            );
        }
    }

    #[test]
    fn step_rejects_speed_above_config_max_without_updating_world() {
        let mut world =
            SimWorld::new(FrameId::new(0), vec![test_entity(100, Vec2Fp::zero())]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::RIGHT,
                speed_per_second: Some(Fp::from_i32(11)),
            }),
        )];

        let error = step(&mut world, FrameId::new(1), &inputs, &test_config()).unwrap_err();

        assert_eq!(
            error,
            StepError::MovementSpeedTooHigh {
                entity_id: EntityId::new(100),
                speed_per_second: Fp::from_i32(11),
                max_speed_per_second: Fp::from_i32(10),
            }
        );
        assert_eq!(world.frame, FrameId::new(0));
        assert_eq!(entity_pos(&world, 100), (0, 0));
        assert_eq!(
            world.entity(EntityId::new(100)).unwrap().movement,
            MovementState::default()
        );
    }

    #[test]
    fn step_rejects_resolved_speed_above_config_max_without_updating_world() {
        let mut entity = test_entity(100, Vec2Fp::zero());
        entity.movement = MovementState {
            mode: MovementMode::Controlled,
            move_dir: QuantizedDir::RIGHT,
            speed_per_second: Fp::from_i32(12),
        };
        let mut world = SimWorld::new(FrameId::new(0), vec![entity.clone()]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::DOWN,
                speed_per_second: None,
            }),
        )];

        let error = step(&mut world, FrameId::new(1), &inputs, &test_config()).unwrap_err();

        assert_eq!(
            error,
            StepError::MovementSpeedTooHigh {
                entity_id: EntityId::new(100),
                speed_per_second: Fp::from_i32(12),
                max_speed_per_second: Fp::from_i32(10),
            }
        );
        assert_eq!(world.frame, FrameId::new(0));
        assert_eq!(entity_pos(&world, 100), (0, 0));
        assert_eq!(
            world.entity(EntityId::new(100)).unwrap().movement,
            entity.movement
        );
    }

    #[test]
    fn step_rejects_positive_speed_with_zero_direction_without_updating_world() {
        let mut world =
            SimWorld::new(FrameId::new(0), vec![test_entity(100, Vec2Fp::zero())]).unwrap();
        let inputs = vec![input(
            1,
            100,
            1,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::ZERO,
                speed_per_second: Some(Fp::from_i32(6)),
            }),
        )];

        let error = step(&mut world, FrameId::new(1), &inputs, &test_config()).unwrap_err();

        assert_eq!(
            error,
            StepError::ZeroDirectionMove {
                entity_id: EntityId::new(100),
            }
        );
        assert_eq!(world.frame, FrameId::new(0));
        assert_eq!(entity_pos(&world, 100), (0, 0));
        assert_eq!(
            world.entity(EntityId::new(100)).unwrap().movement,
            MovementState::default()
        );
    }
}
