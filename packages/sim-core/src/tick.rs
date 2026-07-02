//! Simulation tick advancement.

use crate::hash::{SimHash, hash_world};
use crate::ids::{EntityId, FrameId};
use crate::input::{SimCommand, SimInput, ordered_inputs, select_latest_movement_inputs};
use crate::math::{FP_SCALE, Fp, QuantizedDir, Vec2Fp};
use crate::state::{MovementMode, SimEntity, SimWorld};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SceneBounds {
    pub min: Vec2Fp,
    pub max: Vec2Fp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimConfig {
    pub tick_rate: u16,
    pub default_move_speed_per_second: Fp,
    pub bounds: SceneBounds,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimEvent;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimStepResult {
    pub frame: FrameId,
    pub events: Vec<SimEvent>,
    pub state_hash: SimHash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepError {
    NonSequentialFrame { expected: FrameId, actual: FrameId },
    FrameOverflow { current: FrameId, actual: FrameId },
    ZeroTickRate,
    EntityNotFound { entity_id: EntityId },
    MovementDeltaOverflow { entity_id: EntityId },
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
            Self::EntityNotFound { entity_id } => {
                write!(f, "simulation entity not found: {}", entity_id.raw())
            }
            Self::MovementDeltaOverflow { entity_id } => write!(
                f,
                "movement delta overflow for simulation entity: {}",
                entity_id.raw()
            ),
        }
    }
}

impl std::error::Error for StepError {}

pub fn step(
    world: &mut SimWorld,
    frame: FrameId,
    inputs: &[SimInput],
    config: &SimConfig,
) -> Result<SimStepResult, StepError> {
    validate_step(world, frame, inputs, config)?;

    for indexed in select_latest_movement_inputs(inputs) {
        let input = indexed.input;
        let entity = world
            .entity_mut(input.entity_id)
            .ok_or(StepError::EntityNotFound {
                entity_id: input.entity_id,
            })?;

        match input.command {
            SimCommand::Move(command) => {
                entity.movement.mode = MovementMode::Controlled;
                entity.movement.move_dir = command.dir;
                entity.movement.speed_per_second = command
                    .speed_per_second
                    .unwrap_or(config.default_move_speed_per_second);
            }
            SimCommand::Stop => {
                entity.movement.mode = MovementMode::Idle;
                entity.movement.move_dir = QuantizedDir::ZERO;
                entity.movement.speed_per_second = Fp::ZERO;
            }
            SimCommand::Face(_) | SimCommand::Noop => {}
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

    world.frame = frame;

    Ok(SimStepResult {
        frame,
        events: Vec::new(),
        state_hash: hash_world(world),
    })
}

fn validate_step(
    world: &SimWorld,
    frame: FrameId,
    inputs: &[SimInput],
    config: &SimConfig,
) -> Result<(), StepError> {
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

    if config.tick_rate == 0 {
        return Err(StepError::ZeroTickRate);
    }

    for indexed in select_latest_movement_inputs(inputs) {
        let entity_id = indexed.input.entity_id;
        if world.entity(entity_id).is_none() {
            return Err(StepError::EntityNotFound { entity_id });
        }
    }

    for indexed in ordered_inputs(inputs) {
        let input = indexed.input;
        if matches!(input.command, SimCommand::Face(_)) && world.entity(input.entity_id).is_none() {
            return Err(StepError::EntityNotFound {
                entity_id: input.entity_id,
            });
        }
    }

    Ok(())
}

fn advance_controlled_entity(entity: &mut SimEntity, config: &SimConfig) -> Result<(), StepError> {
    if entity.movement.mode != MovementMode::Controlled {
        return Ok(());
    }

    let delta_x = movement_delta_raw(
        entity.movement.move_dir.x(),
        entity.movement.speed_per_second,
        config.tick_rate,
        entity.id,
    )?;
    let delta_y = movement_delta_raw(
        entity.movement.move_dir.y(),
        entity.movement.speed_per_second,
        config.tick_rate,
        entity.id,
    )?;

    entity.transform.pos = Vec2Fp::new(
        clamp_axis_i128(
            entity.transform.pos.x.raw() as i128 + delta_x as i128,
            config.bounds.min.x.raw(),
            config.bounds.max.x.raw(),
        ),
        clamp_axis_i128(
            entity.transform.pos.y.raw() as i128 + delta_y as i128,
            config.bounds.min.y.raw(),
            config.bounds.max.y.raw(),
        ),
    );

    Ok(())
}

fn movement_delta_raw(
    dir_component: i16,
    speed_per_second: Fp,
    tick_rate: u16,
    entity_id: EntityId,
) -> Result<i64, StepError> {
    let denominator = FP_SCALE as i128 * tick_rate as i128;
    let delta = dir_component as i128 * speed_per_second.raw() as i128 / denominator;

    i64::try_from(delta).map_err(|_| StepError::MovementDeltaOverflow { entity_id })
}

fn clamp_axis_i128(value: i128, min: i64, max: i64) -> Fp {
    let low = min.min(max) as i128;
    let high = min.max(max) as i128;
    Fp::from_raw(value.clamp(low, high) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::TeamId;
    use crate::input::{FaceCommand, MoveCommand, SimInputSource};
    use crate::state::{CombatState, EntityKind, MovementState, SimTransform};

    fn test_config() -> SimConfig {
        SimConfig {
            tick_rate: 60,
            default_move_speed_per_second: Fp::from_i32(6),
            bounds: SceneBounds {
                min: Vec2Fp::new(Fp::from_i32(-10), Fp::from_i32(-10)),
                max: Vec2Fp::new(Fp::from_i32(10), Fp::from_i32(10)),
            },
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

    fn entity_pos(world: &SimWorld, entity_id: u32) -> (i64, i64) {
        world
            .entity(EntityId::new(entity_id))
            .unwrap()
            .transform
            .pos
            .raw_tuple()
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
    }

    #[test]
    fn step_clamps_position_to_scene_bounds() {
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

        step(&mut world, FrameId::new(1), &inputs, &test_config()).unwrap();

        assert_eq!(entity_pos(&world, 100), (10_000, 0));
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
}
