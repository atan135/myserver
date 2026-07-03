//! Offline scenario schema for lockstep replay.
//!
//! Scenario JSON uses integer raw milli-units for simulation-space positions,
//! radii, and speeds. For example, `x: 1500` means 1.5 simulation units.

use serde::{Deserialize, Serialize};
use sim_core::{
    BuffDefinition, BuffId, BuffSlot, CastSkillCommand, CombatConfig, CombatConfigError,
    CombatEffect, CombatState, DamageFormula, EntityId, EntityKind, FaceCommand, Fp, FrameId,
    MoveCommand, MovementConfig, MovementState, QuantizedDir, SIM_CORE_SCHEMA_VERSION, SceneBounds,
    SimCommand, SimConfig, SimEntity, SimEvent, SimHash, SimInput, SimInputSource, SimRngState,
    SimTransform, SimWorld, SkillDefinition, SkillId, SkillSlot, SkillTarget, SkillTargetType,
    StaticObstacle, StepError, TeamId, Vec2Fp,
};
use std::collections::BTreeSet;
use std::fmt;

pub const SCENARIO_SCHEMA_VERSION: u16 = SIM_CORE_SCHEMA_VERSION;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Scenario {
    pub version: u16,
    pub tick_rate: u16,
    pub config: ScenarioConfig,
    pub initial: ScenarioInitial,
    pub inputs: Vec<ScenarioInput>,
    pub assertions: ScenarioAssertions,
}

impl Scenario {
    pub fn from_json_str(input: &str) -> Result<Self, ScenarioError> {
        let scenario =
            serde_json::from_str::<Self>(input).map_err(|error| ScenarioError::Deserialize {
                message: error.to_string(),
            })?;
        scenario.validate()?;
        Ok(scenario)
    }

    pub fn validate(&self) -> Result<(), ScenarioError> {
        if self.version != SCENARIO_SCHEMA_VERSION {
            return Err(ScenarioError::UnsupportedVersion {
                version: self.version,
                supported: SCENARIO_SCHEMA_VERSION,
            });
        }

        if self.tick_rate == 0 {
            return Err(ScenarioError::InvalidConfig {
                field: "tickRate",
                message: "must be greater than zero".to_owned(),
            });
        }

        self.config.validate()?;
        self.initial.validate()?;
        self.validate_inputs()?;
        self.assertions.validate(&self.initial)?;

        Ok(())
    }

    pub fn to_sim_config(&self) -> SimConfig {
        SimConfig {
            movement: MovementConfig {
                tick_rate: self.tick_rate,
                default_speed_per_second: Fp::from_milli(
                    self.config.movement.default_speed_per_second_milli,
                ),
                max_speed_per_second: Fp::from_milli(
                    self.config.movement.max_speed_per_second_milli,
                ),
                bounds: self.config.movement.bounds.to_scene_bounds(),
                static_obstacles: Vec::<StaticObstacle>::new(),
            },
            combat: self.config.to_combat_config(),
        }
    }

    pub fn to_initial_world(&self) -> Result<SimWorld, ScenarioError> {
        self.validate()?;

        let entities = self
            .initial
            .entities
            .iter()
            .map(ScenarioInitialEntity::to_sim_entity)
            .collect::<Vec<_>>();

        SimWorld::with_rng(
            FrameId::new(self.initial.frame),
            SimRngState {
                seed: self.initial.seed,
                counter: 0,
            },
            entities,
        )
        .map_err(|error| ScenarioError::WorldBuild {
            message: error.to_string(),
        })
    }

    pub fn to_sim_inputs(&self) -> Result<Vec<SimInput>, ScenarioError> {
        self.validate()?;

        self.inputs
            .iter()
            .enumerate()
            .map(|(index, input)| input.to_sim_input(index))
            .collect()
    }

    pub fn expected_final_hash(&self) -> Result<SimHash, ScenarioError> {
        let value = parse_final_hash(&self.assertions.final_hash)?;
        Ok(SimHash {
            frame: FrameId::new(self.assertions.final_frame),
            value,
        })
    }

    fn validate_inputs(&self) -> Result<(), ScenarioError> {
        for (index, input) in self.inputs.iter().enumerate() {
            if input.frame <= self.initial.frame {
                return Err(ScenarioError::invalid_input(
                    index,
                    input,
                    format!(
                        "frame must be greater than initial frame {}",
                        self.initial.frame
                    ),
                ));
            }

            if input.frame > self.assertions.final_frame {
                return Err(ScenarioError::invalid_input(
                    index,
                    input,
                    format!(
                        "frame must not exceed assertions.finalFrame {}",
                        self.assertions.final_frame
                    ),
                ));
            }

            let Some(entity) = self
                .initial
                .entities
                .iter()
                .find(|entity| entity.id == input.entity_id)
            else {
                return Err(ScenarioError::invalid_input(
                    index,
                    input,
                    "entityId does not exist in initial.entities".to_owned(),
                ));
            };

            if input.character_id.is_empty() {
                return Err(ScenarioError::invalid_input(
                    index,
                    input,
                    "characterId must not be empty".to_owned(),
                ));
            }

            if input.character_id != entity.character_id {
                return Err(ScenarioError::invalid_input(
                    index,
                    input,
                    format!(
                        "characterId '{}' does not match initial entity characterId '{}'",
                        input.character_id, entity.character_id
                    ),
                ));
            }

            input.command.validate(
                index,
                input,
                self.config.movement.max_speed_per_second_milli,
            )?;
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioConfig {
    pub movement: ScenarioMovementConfig,
    #[serde(default)]
    pub combat: Option<ScenarioCombatConfig>,
}

impl ScenarioConfig {
    fn validate(&self) -> Result<(), ScenarioError> {
        self.movement.validate()?;

        if let Some(combat) = &self.combat {
            combat.validate()?;
        }

        Ok(())
    }

    fn to_combat_config(&self) -> CombatConfig {
        self.combat
            .as_ref()
            .map(ScenarioCombatConfig::to_combat_config)
            .unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioCombatConfig {
    #[serde(default)]
    pub skills: Vec<ScenarioSkillDefinition>,
    #[serde(default)]
    pub buffs: Vec<ScenarioBuffDefinition>,
}

impl ScenarioCombatConfig {
    fn validate(&self) -> Result<(), ScenarioError> {
        self.try_to_combat_config()
            .map(|_| ())
            .map_err(|error| ScenarioError::InvalidConfig {
                field: "config.combat",
                message: error.to_string(),
            })
    }

    fn try_to_combat_config(&self) -> Result<CombatConfig, CombatConfigError> {
        let skills = self
            .skills
            .iter()
            .map(ScenarioSkillDefinition::to_skill_definition)
            .collect::<Vec<_>>();
        let buffs = self
            .buffs
            .iter()
            .map(ScenarioBuffDefinition::to_buff_definition)
            .collect::<Vec<_>>();

        CombatConfig::from_definitions(skills, buffs)
    }

    fn to_combat_config(&self) -> CombatConfig {
        self.try_to_combat_config()
            .expect("scenario combat config is validated before conversion")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioSkillDefinition {
    pub id: u32,
    pub cooldown_frames: u32,
    pub cast_range_milli: i64,
    pub target_type: ScenarioSkillTargetType,
    #[serde(default)]
    pub effects: Vec<ScenarioCombatEffect>,
}

impl ScenarioSkillDefinition {
    fn to_skill_definition(&self) -> SkillDefinition {
        SkillDefinition {
            id: SkillId::new(self.id),
            cooldown_frames: self.cooldown_frames,
            cast_range: Fp::from_milli(self.cast_range_milli),
            target_type: self.target_type.into(),
            effects: self
                .effects
                .iter()
                .map(ScenarioCombatEffect::to_combat_effect)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioBuffDefinition {
    pub id: u32,
    pub duration_frames: u32,
    pub interval_frames: u32,
    pub max_stacks: u16,
    #[serde(default)]
    pub effects: Vec<ScenarioCombatEffect>,
}

impl ScenarioBuffDefinition {
    fn to_buff_definition(&self) -> BuffDefinition {
        BuffDefinition {
            id: BuffId::new(self.id),
            duration_frames: self.duration_frames,
            interval_frames: self.interval_frames,
            max_stacks: self.max_stacks,
            effects: self
                .effects
                .iter()
                .map(ScenarioCombatEffect::to_combat_effect)
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScenarioSkillTargetType {
    None,
    SelfOnly,
    Ally,
    Enemy,
    AnyEntity,
    Position,
    Direction,
}

impl From<ScenarioSkillTargetType> for SkillTargetType {
    fn from(value: ScenarioSkillTargetType) -> Self {
        match value {
            ScenarioSkillTargetType::None => Self::None,
            ScenarioSkillTargetType::SelfOnly => Self::SelfOnly,
            ScenarioSkillTargetType::Ally => Self::Ally,
            ScenarioSkillTargetType::Enemy => Self::Enemy,
            ScenarioSkillTargetType::AnyEntity => Self::AnyEntity,
            ScenarioSkillTargetType::Position => Self::Position,
            ScenarioSkillTargetType::Direction => Self::Direction,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum ScenarioCombatEffect {
    Damage {
        formula: ScenarioDamageFormula,
    },
    Heal {
        formula: ScenarioDamageFormula,
    },
    AddBuff {
        #[serde(rename = "buffId")]
        buff_id: u32,
    },
}

impl ScenarioCombatEffect {
    fn to_combat_effect(&self) -> CombatEffect {
        match self {
            Self::Damage { formula } => CombatEffect::Damage {
                formula: formula.to_damage_formula(),
            },
            Self::Heal { formula } => CombatEffect::Heal {
                formula: formula.to_damage_formula(),
            },
            Self::AddBuff { buff_id } => CombatEffect::AddBuff {
                buff_id: BuffId::new(*buff_id),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum ScenarioDamageFormula {
    Fixed { amount: i32 },
    Scaling { base: i32, attack_scale_bps: i32 },
    TrueDamage { amount: i32 },
}

impl ScenarioDamageFormula {
    fn to_damage_formula(&self) -> DamageFormula {
        match *self {
            Self::Fixed { amount } => DamageFormula::Fixed { amount },
            Self::Scaling {
                base,
                attack_scale_bps,
            } => DamageFormula::Scaling {
                base,
                attack_scale_bps,
            },
            Self::TrueDamage { amount } => DamageFormula::TrueDamage { amount },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioMovementConfig {
    pub bounds: ScenarioBounds,
    #[serde(default = "default_speed_per_second_milli")]
    pub default_speed_per_second_milli: i64,
    #[serde(default = "default_max_speed_per_second_milli")]
    pub max_speed_per_second_milli: i64,
}

impl ScenarioMovementConfig {
    fn validate(&self) -> Result<(), ScenarioError> {
        if self.default_speed_per_second_milli <= 0 {
            return Err(ScenarioError::InvalidConfig {
                field: "config.movement.defaultSpeedPerSecondMilli",
                message: "must be greater than zero".to_owned(),
            });
        }

        if self.max_speed_per_second_milli <= 0 {
            return Err(ScenarioError::InvalidConfig {
                field: "config.movement.maxSpeedPerSecondMilli",
                message: "must be greater than zero".to_owned(),
            });
        }

        if self.default_speed_per_second_milli > self.max_speed_per_second_milli {
            return Err(ScenarioError::InvalidConfig {
                field: "config.movement.defaultSpeedPerSecondMilli",
                message: "must not exceed config.movement.maxSpeedPerSecondMilli".to_owned(),
            });
        }

        Ok(())
    }
}

fn default_speed_per_second_milli() -> i64 {
    6_000
}

fn default_max_speed_per_second_milli() -> i64 {
    10_000
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioBounds {
    /// Minimum x position in raw milli-units.
    pub min_x: i64,
    /// Minimum y position in raw milli-units.
    pub min_y: i64,
    /// Maximum x position in raw milli-units.
    pub max_x: i64,
    /// Maximum y position in raw milli-units.
    pub max_y: i64,
}

impl ScenarioBounds {
    fn to_scene_bounds(self) -> SceneBounds {
        SceneBounds {
            min: Vec2Fp::new(Fp::from_milli(self.min_x), Fp::from_milli(self.min_y)),
            max: Vec2Fp::new(Fp::from_milli(self.max_x), Fp::from_milli(self.max_y)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioInitial {
    #[serde(default)]
    pub frame: u32,
    #[serde(default)]
    pub seed: u64,
    pub entities: Vec<ScenarioInitialEntity>,
}

impl ScenarioInitial {
    fn validate(&self) -> Result<(), ScenarioError> {
        let mut entity_ids = BTreeSet::new();

        for (index, entity) in self.entities.iter().enumerate() {
            if !entity_ids.insert(entity.id) {
                return Err(ScenarioError::DuplicateEntityId {
                    entity_id: entity.id,
                });
            }

            entity.validate(index)?;
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioInitialEntity {
    pub id: u32,
    pub kind: ScenarioEntityKind,
    pub character_id: String,
    pub team_id: u16,
    /// Initial x position in raw milli-units.
    pub x: i64,
    /// Initial y position in raw milli-units.
    pub y: i64,
    /// Collision radius in raw milli-units.
    pub radius: i64,
    pub hp: i32,
    #[serde(default)]
    pub combat: Option<ScenarioInitialCombat>,
}

impl ScenarioInitialEntity {
    fn validate(&self, index: usize) -> Result<(), ScenarioError> {
        if self.character_id.is_empty() {
            return Err(ScenarioError::InvalidInitialEntity {
                index,
                entity_id: self.id,
                message: "characterId must not be empty".to_owned(),
            });
        }

        if self.radius < 0 {
            return Err(ScenarioError::InvalidInitialEntity {
                index,
                entity_id: self.id,
                message: "radius must be raw milli-units greater than or equal to zero".to_owned(),
            });
        }

        if self.hp < 0 {
            return Err(ScenarioError::InvalidInitialEntity {
                index,
                entity_id: self.id,
                message: "hp must be greater than or equal to zero".to_owned(),
            });
        }

        Ok(())
    }

    fn to_sim_entity(&self) -> SimEntity {
        let combat = self.to_combat_state();
        SimEntity {
            id: EntityId::new(self.id),
            kind: self.kind.into(),
            owner_character_id: Some(self.character_id.clone()),
            team_id: TeamId::new(self.team_id),
            transform: SimTransform {
                pos: Vec2Fp::new(Fp::from_milli(self.x), Fp::from_milli(self.y)),
                facing: QuantizedDir::ZERO,
                radius: Fp::from_milli(self.radius),
            },
            movement: MovementState::default(),
            combat,
            alive: self.hp > 0,
        }
    }

    fn to_combat_state(&self) -> CombatState {
        let mut state = CombatState {
            hp: self.hp,
            max_hp: self.hp,
            ..CombatState::default()
        };

        if let Some(combat) = &self.combat {
            state.max_hp = combat.max_hp.unwrap_or(self.hp);
            state.attack = combat.attack.unwrap_or_default();
            state.defense = combat.defense.unwrap_or_default();
            state.speed = combat.speed.unwrap_or_default();
            state.crit_rate_bps = combat.crit.unwrap_or_default();
            state.crit_damage_bps = combat.crit_damage.unwrap_or_default();
            state.skill_slots = combat
                .skill_slots
                .iter()
                .map(ScenarioSkillSlot::to_skill_slot)
                .collect();
            state.buffs = combat
                .buff_slots
                .iter()
                .map(ScenarioBuffSlot::to_buff_slot)
                .collect();
        }

        state
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioInitialCombat {
    #[serde(default)]
    pub max_hp: Option<i32>,
    #[serde(default)]
    pub attack: Option<i32>,
    #[serde(default)]
    pub defense: Option<i32>,
    #[serde(default)]
    pub speed: Option<i32>,
    #[serde(default)]
    pub crit: Option<u16>,
    #[serde(default)]
    pub crit_damage: Option<u16>,
    #[serde(default, alias = "skills")]
    pub skill_slots: Vec<ScenarioSkillSlot>,
    #[serde(default, alias = "buffs")]
    pub buff_slots: Vec<ScenarioBuffSlot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioSkillSlot {
    pub skill_id: u32,
    #[serde(default)]
    pub cooldown_remaining: u32,
}

impl ScenarioSkillSlot {
    fn to_skill_slot(&self) -> SkillSlot {
        SkillSlot {
            skill_id: SkillId::new(self.skill_id),
            cooldown_remaining: self.cooldown_remaining,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioBuffSlot {
    pub buff_id: u32,
    pub duration_remaining: u32,
    pub interval_remaining: u32,
    #[serde(default = "default_buff_stacks")]
    pub stacks: u16,
    pub source_entity: u32,
}

impl ScenarioBuffSlot {
    fn to_buff_slot(&self) -> BuffSlot {
        BuffSlot {
            buff_id: BuffId::new(self.buff_id),
            duration_remaining: self.duration_remaining,
            interval_remaining: self.interval_remaining,
            stacks: self.stacks,
            source_entity: EntityId::new(self.source_entity),
        }
    }
}

fn default_buff_stacks() -> u16 {
    1
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScenarioEntityKind {
    Player,
    Npc,
    Monster,
    Projectile,
    Summon,
}

impl From<ScenarioEntityKind> for EntityKind {
    fn from(value: ScenarioEntityKind) -> Self {
        match value {
            ScenarioEntityKind::Player => Self::Player,
            ScenarioEntityKind::Npc => Self::Npc,
            ScenarioEntityKind::Monster => Self::Monster,
            ScenarioEntityKind::Projectile => Self::Projectile,
            ScenarioEntityKind::Summon => Self::Summon,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioInput {
    pub frame: u32,
    pub character_id: String,
    pub entity_id: u32,
    pub seq: u32,
    pub command: ScenarioCommand,
}

impl ScenarioInput {
    fn to_sim_input(&self, index: usize) -> Result<SimInput, ScenarioError> {
        Ok(SimInput {
            frame: FrameId::new(self.frame),
            character_id: self.character_id.clone(),
            entity_id: EntityId::new(self.entity_id),
            seq: self.seq,
            source: SimInputSource::Real,
            command: self.command.to_sim_command(index, self)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ScenarioCommand {
    #[serde(alias = "move")]
    Move {
        #[serde(rename = "dirX")]
        dir_x: i16,
        #[serde(rename = "dirY")]
        dir_y: i16,
        #[serde(rename = "speedPerSecondMilli", default)]
        speed_per_second_milli: Option<i64>,
    },
    #[serde(alias = "stop")]
    Stop,
    #[serde(alias = "face")]
    Face {
        #[serde(rename = "dirX")]
        dir_x: i16,
        #[serde(rename = "dirY")]
        dir_y: i16,
    },
    #[serde(alias = "castSkill")]
    CastSkill {
        #[serde(rename = "skillId")]
        skill_id: u32,
        target: ScenarioSkillTarget,
    },
}

impl ScenarioCommand {
    fn validate(
        &self,
        index: usize,
        input: &ScenarioInput,
        max_speed_per_second_milli: i64,
    ) -> Result<(), ScenarioError> {
        match *self {
            Self::Move {
                dir_x,
                dir_y,
                speed_per_second_milli,
            } => {
                let dir = parse_dir(index, input, dir_x, dir_y)?;
                if dir == QuantizedDir::ZERO {
                    return Err(ScenarioError::invalid_input(
                        index,
                        input,
                        "Move direction must be non-zero".to_owned(),
                    ));
                }

                if let Some(speed) = speed_per_second_milli {
                    validate_speed(index, input, speed, max_speed_per_second_milli)?;
                }
            }
            Self::Stop => {}
            Self::Face { dir_x, dir_y } => {
                let dir = parse_dir(index, input, dir_x, dir_y)?;
                if dir == QuantizedDir::ZERO {
                    return Err(ScenarioError::invalid_input(
                        index,
                        input,
                        "Face direction must be non-zero".to_owned(),
                    ));
                }
            }
            Self::CastSkill { target, .. } => target.validate(index, input)?,
        }

        Ok(())
    }

    fn to_sim_command(
        &self,
        index: usize,
        input: &ScenarioInput,
    ) -> Result<SimCommand, ScenarioError> {
        match *self {
            Self::Move {
                dir_x,
                dir_y,
                speed_per_second_milli,
            } => Ok(SimCommand::Move(MoveCommand {
                dir: parse_dir(index, input, dir_x, dir_y)?,
                speed_per_second: speed_per_second_milli.map(Fp::from_milli),
            })),
            Self::Stop => Ok(SimCommand::Stop),
            Self::Face { dir_x, dir_y } => Ok(SimCommand::Face(FaceCommand {
                dir: parse_dir(index, input, dir_x, dir_y)?,
            })),
            Self::CastSkill { skill_id, target } => Ok(SimCommand::CastSkill(CastSkillCommand {
                skill_id: SkillId::new(skill_id),
                target: target.to_skill_target(index, input)?,
            })),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ScenarioSkillTarget {
    #[serde(alias = "none")]
    None,
    #[serde(alias = "entity")]
    Entity {
        #[serde(rename = "entityId")]
        entity_id: u32,
    },
    #[serde(alias = "position")]
    Position { x: i64, y: i64 },
    #[serde(alias = "direction")]
    Direction {
        #[serde(rename = "dirX")]
        dir_x: i16,
        #[serde(rename = "dirY")]
        dir_y: i16,
    },
}

impl ScenarioSkillTarget {
    fn validate(&self, index: usize, input: &ScenarioInput) -> Result<(), ScenarioError> {
        if let Self::Direction { dir_x, dir_y } = *self {
            let dir = parse_dir(index, input, dir_x, dir_y)?;
            if dir == QuantizedDir::ZERO {
                return Err(ScenarioError::invalid_input(
                    index,
                    input,
                    "CastSkill direction target must be non-zero".to_owned(),
                ));
            }
        }

        Ok(())
    }

    fn to_skill_target(
        self,
        index: usize,
        input: &ScenarioInput,
    ) -> Result<SkillTarget, ScenarioError> {
        match self {
            Self::None => Ok(SkillTarget::None),
            Self::Entity { entity_id } => Ok(SkillTarget::Entity(EntityId::new(entity_id))),
            Self::Position { x, y } => Ok(SkillTarget::Position(Vec2Fp::new(
                Fp::from_milli(x),
                Fp::from_milli(y),
            ))),
            Self::Direction { dir_x, dir_y } => Ok(SkillTarget::Direction(parse_dir(
                index, input, dir_x, dir_y,
            )?)),
        }
    }
}

fn parse_dir(
    index: usize,
    input: &ScenarioInput,
    dir_x: i16,
    dir_y: i16,
) -> Result<QuantizedDir, ScenarioError> {
    QuantizedDir::new(dir_x, dir_y).map_err(|error| {
        ScenarioError::invalid_input(index, input, format!("invalid direction: {error}"))
    })
}

fn validate_speed(
    index: usize,
    input: &ScenarioInput,
    speed_per_second_milli: i64,
    max_speed_per_second_milli: i64,
) -> Result<(), ScenarioError> {
    if speed_per_second_milli <= 0 {
        return Err(ScenarioError::invalid_input(
            index,
            input,
            format!("speedPerSecondMilli must be greater than zero: {speed_per_second_milli}"),
        ));
    }

    if speed_per_second_milli > max_speed_per_second_milli {
        return Err(ScenarioError::invalid_input(
            index,
            input,
            format!(
                "speedPerSecondMilli exceeds config.movement.maxSpeedPerSecondMilli: {} > {}",
                speed_per_second_milli, max_speed_per_second_milli
            ),
        ));
    }

    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioAssertions {
    pub final_frame: u32,
    pub final_hash: String,
    #[serde(default)]
    pub entity_positions: Vec<ScenarioEntityPositionAssertion>,
    #[serde(default)]
    pub events: Vec<ScenarioEventAssertion>,
    #[serde(default)]
    pub expected_error: Option<ScenarioExpectedError>,
}

impl ScenarioAssertions {
    fn validate(&self, initial: &ScenarioInitial) -> Result<(), ScenarioError> {
        if self.final_frame < initial.frame {
            return Err(ScenarioError::InvalidAssertion {
                field: "assertions.finalFrame",
                message: format!(
                    "must be greater than or equal to initial frame {}",
                    initial.frame
                ),
            });
        }

        parse_final_hash(&self.final_hash)?;

        for (index, event) in self.events.iter().enumerate() {
            event.validate(index, initial)?;
        }

        for (index, position) in self.entity_positions.iter().enumerate() {
            if !initial
                .entities
                .iter()
                .any(|entity| entity.id == position.entity_id)
            {
                return Err(ScenarioError::InvalidAssertion {
                    field: "assertions.entityPositions",
                    message: format!(
                        "entityPositions[{index}].entityId {} does not exist in initial.entities",
                        position.entity_id
                    ),
                });
            }

            if let Some(tolerance) = position.tolerance_milli {
                if tolerance < 0 {
                    return Err(ScenarioError::InvalidAssertion {
                        field: "assertions.entityPositions.toleranceMilli",
                        message: "must be greater than or equal to zero".to_owned(),
                    });
                }
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioEventAssertion {
    pub frame: u32,
    #[serde(rename = "type")]
    pub event_type: ScenarioEventType,
    #[serde(default)]
    pub source_entity: Option<u32>,
    #[serde(default)]
    pub target_entity: Option<u32>,
    #[serde(default)]
    pub skill_id: Option<u32>,
    #[serde(default)]
    pub buff_id: Option<u32>,
    #[serde(default)]
    pub value: Option<i32>,
}

impl ScenarioEventAssertion {
    fn validate(&self, index: usize, initial: &ScenarioInitial) -> Result<(), ScenarioError> {
        if let Some(entity_id) = self.source_entity {
            validate_assertion_entity_exists(
                initial,
                "assertions.events.sourceEntity",
                index,
                entity_id,
            )?;
        }

        if let Some(entity_id) = self.target_entity {
            validate_assertion_entity_exists(
                initial,
                "assertions.events.targetEntity",
                index,
                entity_id,
            )?;
        }

        Ok(())
    }

    pub fn matches(&self, event: &SimEvent) -> bool {
        self.event_type == ScenarioEventType::from(event)
            && self.frame == event_frame(event).raw()
            && self
                .source_entity
                .is_none_or(|expected| event_source_entity(event).raw() == expected)
            && self.target_entity.is_none_or(|expected| {
                event_target_entity(event).is_some_and(|actual| actual.raw() == expected)
            })
            && self.skill_id.is_none_or(|expected| {
                event_skill_id(event).is_some_and(|actual| actual.raw() == expected)
            })
            && self.buff_id.is_none_or(|expected| {
                event_buff_id(event).is_some_and(|actual| actual.raw() == expected)
            })
            && self
                .value
                .is_none_or(|expected| event_value(event) == expected)
    }
}

fn validate_assertion_entity_exists(
    initial: &ScenarioInitial,
    field: &'static str,
    index: usize,
    entity_id: u32,
) -> Result<(), ScenarioError> {
    if !initial.entities.iter().any(|entity| entity.id == entity_id) {
        return Err(ScenarioError::InvalidAssertion {
            field,
            message: format!(
                "events[{index}] entity {entity_id} does not exist in initial.entities"
            ),
        });
    }

    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScenarioEventType {
    SkillCast,
    DamageApplied,
    HealApplied,
    BuffApplied,
    BuffExpired,
    EntityDied,
    BuffTick,
}

impl From<&SimEvent> for ScenarioEventType {
    fn from(value: &SimEvent) -> Self {
        match value {
            SimEvent::SkillCast { .. } => Self::SkillCast,
            SimEvent::DamageApplied { .. } => Self::DamageApplied,
            SimEvent::HealApplied { .. } => Self::HealApplied,
            SimEvent::BuffApplied { .. } => Self::BuffApplied,
            SimEvent::BuffExpired { .. } => Self::BuffExpired,
            SimEvent::EntityDied { .. } => Self::EntityDied,
            SimEvent::BuffTick { .. } => Self::BuffTick,
        }
    }
}

fn event_frame(event: &SimEvent) -> FrameId {
    match event {
        SimEvent::SkillCast { frame, .. }
        | SimEvent::DamageApplied { frame, .. }
        | SimEvent::HealApplied { frame, .. }
        | SimEvent::BuffApplied { frame, .. }
        | SimEvent::BuffExpired { frame, .. }
        | SimEvent::EntityDied { frame, .. }
        | SimEvent::BuffTick { frame, .. } => *frame,
    }
}

fn event_source_entity(event: &SimEvent) -> EntityId {
    match event {
        SimEvent::SkillCast { source_entity, .. }
        | SimEvent::DamageApplied { source_entity, .. }
        | SimEvent::HealApplied { source_entity, .. }
        | SimEvent::BuffApplied { source_entity, .. }
        | SimEvent::BuffExpired { source_entity, .. }
        | SimEvent::EntityDied { source_entity, .. }
        | SimEvent::BuffTick { source_entity, .. } => *source_entity,
    }
}

fn event_target_entity(event: &SimEvent) -> Option<EntityId> {
    match event {
        SimEvent::SkillCast { target_entity, .. } => *target_entity,
        SimEvent::DamageApplied { target_entity, .. }
        | SimEvent::HealApplied { target_entity, .. }
        | SimEvent::BuffApplied { target_entity, .. }
        | SimEvent::BuffExpired { target_entity, .. }
        | SimEvent::EntityDied { target_entity, .. }
        | SimEvent::BuffTick { target_entity, .. } => Some(*target_entity),
    }
}

fn event_skill_id(event: &SimEvent) -> Option<SkillId> {
    match event {
        SimEvent::SkillCast { skill_id, .. } => Some(*skill_id),
        SimEvent::DamageApplied { skill_id, .. }
        | SimEvent::HealApplied { skill_id, .. }
        | SimEvent::EntityDied { skill_id, .. } => *skill_id,
        SimEvent::BuffApplied { .. } | SimEvent::BuffExpired { .. } | SimEvent::BuffTick { .. } => {
            None
        }
    }
}

fn event_buff_id(event: &SimEvent) -> Option<BuffId> {
    match event {
        SimEvent::DamageApplied { buff_id, .. }
        | SimEvent::HealApplied { buff_id, .. }
        | SimEvent::EntityDied { buff_id, .. } => *buff_id,
        SimEvent::BuffApplied { buff_id, .. }
        | SimEvent::BuffExpired { buff_id, .. }
        | SimEvent::BuffTick { buff_id, .. } => Some(*buff_id),
        SimEvent::SkillCast { .. } => None,
    }
}

fn event_value(event: &SimEvent) -> i32 {
    match event {
        SimEvent::SkillCast { value, .. }
        | SimEvent::DamageApplied { value, .. }
        | SimEvent::HealApplied { value, .. }
        | SimEvent::BuffApplied { value, .. }
        | SimEvent::BuffExpired { value, .. }
        | SimEvent::EntityDied { value, .. }
        | SimEvent::BuffTick { value, .. } => *value,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioExpectedError {
    #[serde(default)]
    pub frame: Option<u32>,
    #[serde(rename = "type")]
    pub error_type: ScenarioStepErrorType,
    #[serde(default)]
    pub entity_id: Option<u32>,
    #[serde(default)]
    pub skill_id: Option<u32>,
    #[serde(default)]
    pub target_entity_id: Option<u32>,
}

impl ScenarioExpectedError {
    pub fn matches(&self, frame: u32, error: &StepError) -> bool {
        self.frame.is_none_or(|expected| expected == frame)
            && self.error_type == ScenarioStepErrorType::from(error)
            && self.entity_id.is_none_or(|expected| {
                step_error_entity_id(error).is_some_and(|id| id.raw() == expected)
            })
            && self.skill_id.is_none_or(|expected| {
                step_error_skill_id(error).is_some_and(|id| id.raw() == expected)
            })
            && self.target_entity_id.is_none_or(|expected| {
                step_error_target_entity_id(error).is_some_and(|id| id.raw() == expected)
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScenarioStepErrorType {
    NonSequentialFrame,
    FrameOverflow,
    ZeroTickRate,
    InvalidMovementSpeed,
    MovementSpeedTooHigh,
    ZeroDirectionMove,
    EntityNotFound,
    UnknownSkill,
    UnknownBuff,
    SkillNotEquipped,
    SkillOnCooldown,
    SkillTargetTypeMismatch,
    InvalidSkillTarget,
    SkillTargetOutOfRange,
    SkillTargetDistanceOverflow,
    MovementDeltaOverflow,
}

impl From<&StepError> for ScenarioStepErrorType {
    fn from(value: &StepError) -> Self {
        match value {
            StepError::NonSequentialFrame { .. } => Self::NonSequentialFrame,
            StepError::FrameOverflow { .. } => Self::FrameOverflow,
            StepError::ZeroTickRate => Self::ZeroTickRate,
            StepError::InvalidMovementSpeed { .. } => Self::InvalidMovementSpeed,
            StepError::MovementSpeedTooHigh { .. } => Self::MovementSpeedTooHigh,
            StepError::ZeroDirectionMove { .. } => Self::ZeroDirectionMove,
            StepError::EntityNotFound { .. } => Self::EntityNotFound,
            StepError::UnknownSkill { .. } => Self::UnknownSkill,
            StepError::UnknownBuff { .. } => Self::UnknownBuff,
            StepError::SkillNotEquipped { .. } => Self::SkillNotEquipped,
            StepError::SkillOnCooldown { .. } => Self::SkillOnCooldown,
            StepError::SkillTargetTypeMismatch { .. } => Self::SkillTargetTypeMismatch,
            StepError::InvalidSkillTarget { .. } => Self::InvalidSkillTarget,
            StepError::SkillTargetOutOfRange { .. } => Self::SkillTargetOutOfRange,
            StepError::SkillTargetDistanceOverflow { .. } => Self::SkillTargetDistanceOverflow,
            StepError::MovementDeltaOverflow { .. } => Self::MovementDeltaOverflow,
        }
    }
}

fn step_error_entity_id(error: &StepError) -> Option<EntityId> {
    match error {
        StepError::InvalidMovementSpeed { entity_id, .. }
        | StepError::MovementSpeedTooHigh { entity_id, .. }
        | StepError::ZeroDirectionMove { entity_id }
        | StepError::EntityNotFound { entity_id }
        | StepError::UnknownSkill { entity_id, .. }
        | StepError::UnknownBuff { entity_id, .. }
        | StepError::SkillNotEquipped { entity_id, .. }
        | StepError::SkillOnCooldown { entity_id, .. }
        | StepError::SkillTargetTypeMismatch { entity_id, .. }
        | StepError::InvalidSkillTarget { entity_id, .. }
        | StepError::SkillTargetOutOfRange { entity_id, .. }
        | StepError::SkillTargetDistanceOverflow { entity_id, .. }
        | StepError::MovementDeltaOverflow { entity_id } => Some(*entity_id),
        StepError::NonSequentialFrame { .. }
        | StepError::FrameOverflow { .. }
        | StepError::ZeroTickRate => None,
    }
}

fn step_error_skill_id(error: &StepError) -> Option<SkillId> {
    match error {
        StepError::UnknownSkill { skill_id, .. }
        | StepError::SkillNotEquipped { skill_id, .. }
        | StepError::SkillOnCooldown { skill_id, .. }
        | StepError::SkillTargetTypeMismatch { skill_id, .. }
        | StepError::InvalidSkillTarget { skill_id, .. }
        | StepError::SkillTargetOutOfRange { skill_id, .. }
        | StepError::SkillTargetDistanceOverflow { skill_id, .. } => Some(*skill_id),
        _ => None,
    }
}

fn step_error_target_entity_id(error: &StepError) -> Option<EntityId> {
    match error {
        StepError::InvalidSkillTarget {
            target_entity_id, ..
        }
        | StepError::SkillTargetOutOfRange {
            target_entity_id, ..
        } => Some(*target_entity_id),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioEntityPositionAssertion {
    pub entity_id: u32,
    /// Expected x position in raw milli-units.
    pub x: i64,
    /// Expected y position in raw milli-units.
    pub y: i64,
    #[serde(default)]
    pub tolerance_milli: Option<i64>,
}

fn parse_final_hash(value: &str) -> Result<u64, ScenarioError> {
    let hex = value.strip_prefix("0x").unwrap_or(value);
    if hex.len() != 16 || !hex.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(ScenarioError::InvalidAssertion {
            field: "assertions.finalHash",
            message: "must be a 16-character hexadecimal string".to_owned(),
        });
    }

    u64::from_str_radix(hex, 16).map_err(|error| ScenarioError::InvalidAssertion {
        field: "assertions.finalHash",
        message: error.to_string(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScenarioError {
    Deserialize {
        message: String,
    },
    UnsupportedVersion {
        version: u16,
        supported: u16,
    },
    InvalidConfig {
        field: &'static str,
        message: String,
    },
    DuplicateEntityId {
        entity_id: u32,
    },
    InvalidInitialEntity {
        index: usize,
        entity_id: u32,
        message: String,
    },
    InvalidInput {
        index: usize,
        frame: u32,
        entity_id: u32,
        message: String,
    },
    InvalidAssertion {
        field: &'static str,
        message: String,
    },
    WorldBuild {
        message: String,
    },
}

impl ScenarioError {
    fn invalid_input(index: usize, input: &ScenarioInput, message: String) -> Self {
        Self::InvalidInput {
            index,
            frame: input.frame,
            entity_id: input.entity_id,
            message,
        }
    }
}

impl fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deserialize { message } => {
                write!(f, "failed to deserialize scenario JSON: {message}")
            }
            Self::UnsupportedVersion { version, supported } => write!(
                f,
                "scenario version unsupported: {version} (supported: {supported})"
            ),
            Self::InvalidConfig { field, message } => {
                write!(f, "invalid scenario config {field}: {message}")
            }
            Self::DuplicateEntityId { entity_id } => {
                write!(
                    f,
                    "duplicate entity id in scenario initial.entities: {entity_id}"
                )
            }
            Self::InvalidInitialEntity {
                index,
                entity_id,
                message,
            } => write!(
                f,
                "invalid initial entity at initial.entities[{index}] entity {entity_id}: {message}"
            ),
            Self::InvalidInput {
                index,
                frame,
                entity_id,
                message,
            } => write!(
                f,
                "invalid scenario input at inputs[{index}] frame {frame} entity {entity_id}: {message}"
            ),
            Self::InvalidAssertion { field, message } => {
                write!(f, "invalid scenario assertion {field}: {message}")
            }
            Self::WorldBuild { message } => {
                write!(f, "failed to build initial SimWorld: {message}")
            }
        }
    }
}

impl std::error::Error for ScenarioError {}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::{SimCommand, SimInputSource};

    const VALID_SCENARIO: &str = r#"{
        "version": 1,
        "tickRate": 20,
        "config": {
            "movement": {
                "bounds": { "minX": -250000, "minY": -250000, "maxX": 250000, "maxY": 250000 },
                "defaultSpeedPerSecondMilli": 6000,
                "maxSpeedPerSecondMilli": 10000
            }
        },
        "initial": {
            "frame": 0,
            "seed": 12345,
            "entities": [
                {
                    "id": 1001,
                    "kind": "player",
                    "characterId": "chr_a",
                    "teamId": 1,
                    "x": 0,
                    "y": 0,
                    "radius": 500,
                    "hp": 100
                },
                {
                    "id": 1002,
                    "kind": "monster",
                    "characterId": "npc_1002",
                    "teamId": 2,
                    "x": 5000,
                    "y": 0,
                    "radius": 750,
                    "hp": 200
                }
            ]
        },
        "inputs": [
            {
                "frame": 1,
                "characterId": "chr_a",
                "entityId": 1001,
                "seq": 1,
                "command": { "type": "Move", "dirX": 1000, "dirY": 0, "speedPerSecondMilli": 6000 }
            },
            {
                "frame": 2,
                "characterId": "chr_a",
                "entityId": 1001,
                "seq": 2,
                "command": { "type": "Face", "dirX": -1000, "dirY": 0 }
            },
            {
                "frame": 3,
                "characterId": "chr_a",
                "entityId": 1001,
                "seq": 3,
                "command": { "type": "Stop" }
            }
        ],
        "assertions": {
            "finalFrame": 20,
            "finalHash": "0000000000000000",
            "entityPositions": [
                { "entityId": 1001, "x": 300, "y": 0, "toleranceMilli": 0 }
            ]
        }
    }"#;

    #[test]
    fn valid_scenario_deserializes_validates_and_converts_to_sim_types() {
        let scenario = Scenario::from_json_str(VALID_SCENARIO).unwrap();

        assert_eq!(scenario.version, SCENARIO_SCHEMA_VERSION);
        assert_eq!(scenario.tick_rate, 20);

        let config = scenario.to_sim_config();
        assert_eq!(config.movement.tick_rate, 20);
        assert_eq!(config.movement.default_speed_per_second.raw(), 6_000);
        assert_eq!(config.movement.max_speed_per_second.raw(), 10_000);
        assert_eq!(config.movement.bounds.min.raw_tuple(), (-250_000, -250_000));

        let world = scenario.to_initial_world().unwrap();
        assert_eq!(world.frame, FrameId::new(0));
        assert_eq!(world.rng.seed, 12_345);
        assert_eq!(world.entities_sorted_by_id().len(), 2);
        let entity = world.entity(EntityId::new(1001)).unwrap();
        assert_eq!(entity.kind, EntityKind::Player);
        assert_eq!(entity.owner_character_id.as_deref(), Some("chr_a"));
        assert_eq!(entity.transform.pos.raw_tuple(), (0, 0));
        assert_eq!(entity.transform.radius.raw(), 500);
        assert_eq!(entity.combat.hp, 100);

        let inputs = scenario.to_sim_inputs().unwrap();
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs[0].frame, FrameId::new(1));
        assert_eq!(inputs[0].entity_id, EntityId::new(1001));
        assert_eq!(inputs[0].character_id, "chr_a");
        assert_eq!(inputs[0].seq, 1);
        assert_eq!(inputs[0].source, SimInputSource::Real);
        assert!(
            matches!(inputs[0].command, SimCommand::Move(command) if command.dir == QuantizedDir::RIGHT && command.speed_per_second == Some(Fp::from_milli(6_000)))
        );
        assert!(
            matches!(inputs[1].command, SimCommand::Face(command) if command.dir == QuantizedDir::LEFT)
        );
        assert_eq!(inputs[2].command, SimCommand::Stop);

        let expected_hash = scenario.expected_final_hash().unwrap();
        assert_eq!(expected_hash.frame, FrameId::new(20));
        assert_eq!(expected_hash.value, 0);
    }

    #[test]
    fn missing_required_field_reports_missing_field() {
        let json = VALID_SCENARIO.replace(r#""tickRate": 20,"#, "");

        let error = Scenario::from_json_str(&json).unwrap_err();

        assert_error_contains(&error, "missing field `tickRate`");
    }

    #[test]
    fn unsupported_version_is_rejected() {
        let json = VALID_SCENARIO.replace(r#""version": 1"#, r#""version": 99"#);

        let error = Scenario::from_json_str(&json).unwrap_err();

        assert_eq!(
            error,
            ScenarioError::UnsupportedVersion {
                version: 99,
                supported: SCENARIO_SCHEMA_VERSION
            }
        );
        assert_error_contains(&error, "version unsupported");
    }

    #[test]
    fn duplicate_entity_id_is_rejected() {
        let json = VALID_SCENARIO.replace(r#""id": 1002"#, r#""id": 1001"#);

        let error = Scenario::from_json_str(&json).unwrap_err();

        assert_eq!(error, ScenarioError::DuplicateEntityId { entity_id: 1001 });
        assert_error_contains(&error, "duplicate entity id");
    }

    #[test]
    fn invalid_input_is_rejected() {
        let json = VALID_SCENARIO.replace(
            r#""speedPerSecondMilli": 6000"#,
            r#""speedPerSecondMilli": -1"#,
        );

        let error = Scenario::from_json_str(&json).unwrap_err();

        assert_error_contains(&error, "invalid scenario input");
        assert_error_contains(&error, "speedPerSecondMilli must be greater than zero");
    }

    #[test]
    fn invalid_direction_is_rejected() {
        let json = VALID_SCENARIO.replace(
            r#""command": { "type": "Move", "dirX": 1000, "dirY": 0, "speedPerSecondMilli": 6000 }"#,
            r#""command": { "type": "Move", "dirX": 1000, "dirY": 1000, "speedPerSecondMilli": 6000 }"#,
        );

        let error = Scenario::from_json_str(&json).unwrap_err();

        assert_error_contains(&error, "invalid direction");
        assert_error_contains(&error, "length squared is too large");
    }

    #[test]
    fn invalid_combat_config_returns_error_instead_of_panicking() {
        let json = VALID_SCENARIO.replace(
            r#""movement": {
                "bounds": { "minX": -250000, "minY": -250000, "maxX": 250000, "maxY": 250000 },
                "defaultSpeedPerSecondMilli": 6000,
                "maxSpeedPerSecondMilli": 10000
            }"#,
            r#""movement": {
                "bounds": { "minX": -250000, "minY": -250000, "maxX": 250000, "maxY": 250000 },
                "defaultSpeedPerSecondMilli": 6000,
                "maxSpeedPerSecondMilli": 10000
            },
            "combat": {
                "skills": [
                    {
                        "id": 10,
                        "cooldownFrames": 30,
                        "castRangeMilli": 5000,
                        "targetType": "enemy",
                        "effects": [{ "type": "addBuff", "buffId": 999 }]
                    }
                ],
                "buffs": []
            }"#,
        );

        let error = Scenario::from_json_str(&json).unwrap_err();

        assert!(matches!(
            error,
            ScenarioError::InvalidConfig {
                field: "config.combat",
                ..
            }
        ));
        assert_error_contains(&error, "invalid scenario config config.combat");
        assert_error_contains(&error, "references unknown buff id: 999");
    }

    fn assert_error_contains(error: &ScenarioError, expected: &str) {
        let actual = error.to_string();
        assert!(
            actual.contains(expected),
            "expected error to contain `{expected}`, got `{actual}`"
        );
    }
}
