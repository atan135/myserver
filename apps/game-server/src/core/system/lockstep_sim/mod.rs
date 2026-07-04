use std::collections::HashMap;

use serde::Deserialize;
use sim_core::{
    CastSkillCommand, CombatConfig, CombatEffect, CombatState, DamageFormula, EntityId, EntityKind,
    FaceCommand, Fp, FrameId, MoveCommand, MovementConfig, MovementMode, MovementState,
    QuantizedDir, SceneBounds, SimCommand, SimConfig, SimEntity, SimHash, SimInput, SimInputSource,
    SimStepResult, SimTransform, SimWorld, SkillDefinition, SkillId, SkillSlot, SkillTarget,
    SkillTargetType, StepError, TeamId, Vec2Fp,
};

use crate::core::room::PlayerInputRecord;

pub const SIM_INPUT_ACTION: &str = "sim_input";
pub const SIM_INPUT_VERSION: u32 = 1;
pub const PLAYER_ENTITY_ID_BASE: u32 = 1000;
pub const TRAINING_TARGET_ENTITY_ID: u32 = 9000;
pub const DEFAULT_PLAYER_SKILL_ID: u32 = 1;
const SIM_INPUT_PAYLOAD_MAX_BYTES: usize = 2048;
const SIM_INPUT_MAX_COMMANDS: usize = 8;
const SIM_INPUT_MAX_SPEED_MILLI: i64 = 12_000;

pub fn create_minimal_world(character_ids: &[String]) -> (SimWorld, HashMap<String, EntityId>) {
    let mut bindings = HashMap::new();
    let mut entities = Vec::new();

    for (index, character_id) in character_ids.iter().enumerate() {
        let entity_id = EntityId::new(PLAYER_ENTITY_ID_BASE + index as u32);
        bindings.insert(character_id.clone(), entity_id);
        entities.push(player_entity(index, character_id, entity_id));
    }

    entities.push(training_target_entity());

    let world = SimWorld::new(FrameId::new(0), entities)
        .expect("lockstep_sim_demo minimal world uses unique entity ids");
    (world, bindings)
}

pub fn default_sim_config(tick_rate: u16) -> SimConfig {
    SimConfig {
        movement: MovementConfig {
            tick_rate: tick_rate.max(1),
            default_speed_per_second: Fp::from_i32(6),
            max_speed_per_second: Fp::from_i32(12),
            bounds: SceneBounds {
                min: Vec2Fp::new(Fp::from_i32(-100), Fp::from_i32(-100)),
                max: Vec2Fp::new(Fp::from_i32(100), Fp::from_i32(100)),
            },
            static_obstacles: Vec::new(),
        },
        combat: CombatConfig::from_definitions(
            vec![SkillDefinition {
                id: SkillId::new(DEFAULT_PLAYER_SKILL_ID),
                cooldown_frames: tick_rate.max(1) as u32,
                cast_range: Fp::from_i32(12),
                target_type: SkillTargetType::Enemy,
                effects: vec![CombatEffect::Damage {
                    formula: DamageFormula::Fixed { amount: 15 },
                }],
            }],
            Vec::new(),
        )
        .expect("lockstep_sim_demo default combat config should be valid"),
    }
}

pub fn validate_player_input(action: &str, payload_json: &str) -> Result<(), &'static str> {
    if action != SIM_INPUT_ACTION {
        return Err("INVALID_SIM_INPUT_ACTION");
    }

    parse_sim_input_payload(payload_json).map(|_| ())
}

pub fn step_world(
    world: &mut SimWorld,
    frame_id: u32,
    fps: u16,
    inputs: &[PlayerInputRecord],
    bindings: &HashMap<String, EntityId>,
) -> Result<SimStepResult, LockstepSimStepError> {
    let sim_inputs = sim_inputs_from_records(inputs, bindings)?;
    let config = default_sim_config(fps);

    sim_core::step(world, FrameId::new(frame_id), &sim_inputs, &config).map_err(Into::into)
}

pub fn world_hash(world: &SimWorld) -> SimHash {
    sim_core::hash_world(world)
}

fn sim_inputs_from_records(
    inputs: &[PlayerInputRecord],
    bindings: &HashMap<String, EntityId>,
) -> Result<Vec<SimInput>, LockstepSimStepError> {
    let mut sim_inputs = Vec::new();

    for input in inputs {
        sim_inputs.extend(sim_inputs_from_record(input, bindings)?);
    }

    Ok(sim_inputs)
}

fn sim_inputs_from_record(
    input: &PlayerInputRecord,
    bindings: &HashMap<String, EntityId>,
) -> Result<Vec<SimInput>, LockstepSimStepError> {
    let Some(entity_id) = bindings.get(&input.character_id).copied() else {
        return Ok(Vec::new());
    };

    let source = sim_input_source(input);
    if input.action.is_empty() {
        return Ok(vec![sim_input(
            input,
            entity_id,
            source,
            0,
            SimCommand::Noop,
        )]);
    }

    if input.action != SIM_INPUT_ACTION {
        return Ok(vec![sim_input(
            input,
            entity_id,
            source,
            0,
            SimCommand::Noop,
        )]);
    }

    let payload =
        parse_sim_input_payload(&input.payload_json).map_err(LockstepSimStepError::Input)?;
    Ok(payload
        .commands
        .iter()
        .map(|command| {
            let seq = payload.seq;
            let command = command.to_sim_command();
            sim_input(input, entity_id, source, seq, command)
        })
        .collect())
}

fn sim_input_source(input: &PlayerInputRecord) -> SimInputSource {
    if !input.is_synthetic {
        SimInputSource::Real
    } else if input.action.is_empty() {
        SimInputSource::SynthesizedEmpty
    } else {
        SimInputSource::SynthesizedRepeatLast
    }
}

fn sim_input(
    input: &PlayerInputRecord,
    entity_id: EntityId,
    source: SimInputSource,
    seq: u32,
    command: SimCommand,
) -> SimInput {
    SimInput {
        frame: FrameId::new(input.frame_id),
        character_id: input.character_id.clone(),
        entity_id,
        seq,
        source,
        command,
    }
}

fn parse_sim_input_payload(payload_json: &str) -> Result<SimInputPayload, &'static str> {
    if payload_json.len() > SIM_INPUT_PAYLOAD_MAX_BYTES {
        return Err("SIM_INPUT_PAYLOAD_TOO_LARGE");
    }

    let payload = serde_json::from_str::<RawSimInputPayload>(payload_json)
        .map_err(|_| "INVALID_SIM_INPUT_JSON")?;
    if payload.version != SIM_INPUT_VERSION {
        return Err("UNSUPPORTED_SIM_INPUT_VERSION");
    }
    if payload.commands.len() > SIM_INPUT_MAX_COMMANDS {
        return Err("SIM_INPUT_COMMAND_COUNT_TOO_LARGE");
    }

    let mut commands = Vec::with_capacity(payload.commands.len());
    for command in payload.commands {
        commands.push(command.validate()?);
    }

    Ok(SimInputPayload {
        seq: payload.seq,
        commands,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockstepSimStepError {
    Input(&'static str),
    Step(StepError),
}

impl std::fmt::Display for LockstepSimStepError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input(code) => formatter.write_str(code),
            Self::Step(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for LockstepSimStepError {}

impl From<StepError> for LockstepSimStepError {
    fn from(error: StepError) -> Self {
        Self::Step(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SimInputPayload {
    seq: u32,
    commands: Vec<ParsedSimCommand>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSimInputPayload {
    version: u32,
    seq: u32,
    commands: Vec<RawSimCommand>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum RawSimCommand {
    Move {
        #[serde(rename = "dirX")]
        dir_x: i16,
        #[serde(rename = "dirY")]
        dir_y: i16,
        #[serde(default)]
        speed: Option<i64>,
    },
    Stop {},
    Face {
        #[serde(rename = "dirX")]
        dir_x: i16,
        #[serde(rename = "dirY")]
        dir_y: i16,
    },
    CastSkill {
        #[serde(rename = "skillId")]
        skill_id: u32,
        #[serde(rename = "targetEntityId", default)]
        target_entity_id: Option<u32>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedSimCommand {
    Move {
        dir: QuantizedDir,
        speed_per_second: Option<Fp>,
    },
    Stop,
    Face {
        dir: QuantizedDir,
    },
    CastSkill {
        skill_id: SkillId,
        target: SkillTarget,
    },
}

impl RawSimCommand {
    fn validate(self) -> Result<ParsedSimCommand, &'static str> {
        match self {
            Self::Move {
                dir_x,
                dir_y,
                speed,
            } => {
                let dir =
                    QuantizedDir::new(dir_x, dir_y).map_err(|_| "SIM_INPUT_DIR_OUT_OF_RANGE")?;
                if dir == QuantizedDir::ZERO {
                    return Err("SIM_INPUT_MOVE_DIR_ZERO");
                }
                let speed_per_second = speed
                    .map(|speed| {
                        if speed <= 0 || speed > SIM_INPUT_MAX_SPEED_MILLI {
                            return Err("SIM_INPUT_SPEED_OUT_OF_RANGE");
                        }
                        Ok(Fp::from_milli(speed))
                    })
                    .transpose()?;
                Ok(ParsedSimCommand::Move {
                    dir,
                    speed_per_second,
                })
            }
            Self::Stop {} => Ok(ParsedSimCommand::Stop),
            Self::Face { dir_x, dir_y } => {
                let dir =
                    QuantizedDir::new(dir_x, dir_y).map_err(|_| "SIM_INPUT_DIR_OUT_OF_RANGE")?;
                Ok(ParsedSimCommand::Face { dir })
            }
            Self::CastSkill {
                skill_id,
                target_entity_id,
            } => {
                if skill_id == 0 {
                    return Err("SIM_INPUT_SKILL_ID_OUT_OF_RANGE");
                }
                let target = match target_entity_id {
                    Some(0) => return Err("SIM_INPUT_TARGET_ENTITY_ID_OUT_OF_RANGE"),
                    Some(target_entity_id) => SkillTarget::Entity(EntityId::new(target_entity_id)),
                    None => SkillTarget::None,
                };
                Ok(ParsedSimCommand::CastSkill {
                    skill_id: SkillId::new(skill_id),
                    target,
                })
            }
        }
    }
}

impl ParsedSimCommand {
    fn to_sim_command(self) -> SimCommand {
        match self {
            Self::Move {
                dir,
                speed_per_second,
            } => SimCommand::Move(MoveCommand {
                dir,
                speed_per_second,
            }),
            Self::Stop => SimCommand::Stop,
            Self::Face { dir } => SimCommand::Face(FaceCommand { dir }),
            Self::CastSkill { skill_id, target } => {
                SimCommand::CastSkill(CastSkillCommand { skill_id, target })
            }
        }
    }
}

fn player_entity(index: usize, character_id: &str, entity_id: EntityId) -> SimEntity {
    SimEntity {
        id: entity_id,
        kind: EntityKind::Player,
        owner_character_id: Some(character_id.to_string()),
        team_id: TeamId::new(1),
        transform: SimTransform {
            pos: Vec2Fp::new(Fp::from_i32(index as i32 * 2), Fp::ZERO),
            facing: QuantizedDir::RIGHT,
            radius: Fp::from_milli(500),
        },
        movement: MovementState {
            mode: MovementMode::Idle,
            move_dir: QuantizedDir::ZERO,
            speed_per_second: Fp::ZERO,
        },
        combat: CombatState {
            hp: 100,
            max_hp: 100,
            attack: 10,
            defense: 3,
            speed: 6,
            crit_rate_bps: 500,
            crit_damage_bps: 15_000,
            skill_slots: vec![SkillSlot {
                skill_id: sim_core::SkillId::new(DEFAULT_PLAYER_SKILL_ID),
                cooldown_remaining: 0,
            }],
            buffs: Vec::new(),
        },
        alive: true,
    }
}

fn training_target_entity() -> SimEntity {
    SimEntity {
        id: EntityId::new(TRAINING_TARGET_ENTITY_ID),
        kind: EntityKind::Monster,
        owner_character_id: None,
        team_id: TeamId::new(90),
        transform: SimTransform {
            pos: Vec2Fp::new(Fp::from_i32(8), Fp::ZERO),
            facing: QuantizedDir::LEFT,
            radius: Fp::from_milli(500),
        },
        movement: MovementState::default(),
        combat: CombatState {
            hp: 150,
            max_hp: 150,
            attack: 0,
            defense: 1,
            speed: 0,
            crit_rate_bps: 0,
            crit_damage_bps: 10_000,
            skill_slots: Vec::new(),
            buffs: Vec::new(),
        },
        alive: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_world_contains_players_target_and_bindings() {
        let players = vec!["player-a".to_string(), "player-b".to_string()];
        let (mut world, bindings) = create_minimal_world(&players);

        assert_eq!(bindings["player-a"], EntityId::new(PLAYER_ENTITY_ID_BASE));
        assert_eq!(
            bindings["player-b"],
            EntityId::new(PLAYER_ENTITY_ID_BASE + 1)
        );
        assert_eq!(world.entities.len(), 3);
        assert!(world
            .entity(EntityId::new(TRAINING_TARGET_ENTITY_ID))
            .is_some());

        let result = step_world(&mut world, 1, 20, &[], &bindings).unwrap();
        assert_eq!(world.frame, FrameId::new(1));
        assert_eq!(result.frame, FrameId::new(1));
    }
}
