//! Offline scenario schema for lockstep replay.
//!
//! Scenario JSON uses integer raw milli-units for simulation-space positions,
//! radii, and speeds. For example, `x: 1500` means 1.5 simulation units.

use serde::{Deserialize, Serialize};
use sim_core::{
    CombatConfig, CombatState, EntityId, EntityKind, FaceCommand, Fp, FrameId, MoveCommand,
    MovementConfig, MovementState, QuantizedDir, SIM_CORE_SCHEMA_VERSION, SceneBounds, SimCommand,
    SimConfig, SimEntity, SimHash, SimInput, SimInputSource, SimRngState, SimTransform, SimWorld,
    StaticObstacle, TeamId, Vec2Fp,
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
            combat: CombatConfig::default(),
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
}

impl ScenarioConfig {
    fn validate(&self) -> Result<(), ScenarioError> {
        self.movement.validate()
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
            combat: CombatState {
                hp: self.hp,
                max_hp: self.hp,
                ..CombatState::default()
            },
            alive: self.hp > 0,
        }
    }
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

    fn assert_error_contains(error: &ScenarioError, expected: &str) {
        let actual = error.to_string();
        assert!(
            actual.contains(expected),
            "expected error to contain `{expected}`, got `{actual}`"
        );
    }
}
