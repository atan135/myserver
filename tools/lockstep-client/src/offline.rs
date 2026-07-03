use crate::scenario::{Scenario, ScenarioError};
use sim_core::{
    EntityId, FrameId, MovementMode, SimCommand, SimEntity, SimHash, SimInput, SimWorld,
    SkillTarget, StepError, hash_world, step,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: lockstep-client --mode offline --scenario <path-or-name>";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CliMode {
    Offline,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliOptions {
    pub mode: CliMode,
    pub scenario: String,
}

impl CliOptions {
    pub fn parse<I, S>(args: I) -> Result<Self, OfflineError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut mode = None;
        let mut scenario = None;
        let mut args = args.into_iter().map(Into::into);

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--mode" => {
                    if mode.is_some() {
                        return Err(OfflineError::invalid_args("duplicate --mode"));
                    }

                    let Some(value) = args.next() else {
                        return Err(OfflineError::invalid_args("missing value for --mode"));
                    };

                    mode = Some(match value.as_str() {
                        "offline" => CliMode::Offline,
                        _ => {
                            return Err(OfflineError::invalid_args(format!(
                                "unsupported --mode `{value}`; expected `offline`"
                            )));
                        }
                    });
                }
                "--scenario" => {
                    if scenario.is_some() {
                        return Err(OfflineError::invalid_args("duplicate --scenario"));
                    }

                    let Some(value) = args.next() else {
                        return Err(OfflineError::invalid_args("missing value for --scenario"));
                    };

                    if value.trim().is_empty() {
                        return Err(OfflineError::invalid_args("--scenario must not be empty"));
                    }

                    scenario = Some(value);
                }
                "--help" | "-h" => {
                    return Err(OfflineError::invalid_args(USAGE));
                }
                _ => {
                    return Err(OfflineError::invalid_args(format!(
                        "unexpected argument `{arg}`"
                    )));
                }
            }
        }

        Ok(Self {
            mode: mode.ok_or_else(|| OfflineError::invalid_args("missing --mode offline"))?,
            scenario: scenario
                .ok_or_else(|| OfflineError::invalid_args("missing --scenario <path-or-name>"))?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OfflineReport {
    pub scenario_path: PathBuf,
    pub final_frame: u32,
    pub final_hash: SimHash,
}

impl OfflineReport {
    pub fn final_hash_hex(&self) -> String {
        format!("{:016x}", self.final_hash.value)
    }
}

impl fmt::Display for OfflineReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "scenario: {}", self.scenario_path.display())?;
        writeln!(f, "final frame: {}", self.final_frame)?;
        write!(f, "final hash: {}", self.final_hash_hex())
    }
}

pub fn run_cli<I, S>(args: I) -> Result<OfflineReport, OfflineError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let options = CliOptions::parse(args)?;
    match options.mode {
        CliMode::Offline => run_offline_by_name_or_path(&options.scenario),
    }
}

pub fn run_offline_by_name_or_path(scenario: &str) -> Result<OfflineReport, OfflineError> {
    let scenario_path = resolve_scenario_path(scenario, default_scenario_dir())?;
    run_offline_file(scenario_path)
}

pub fn run_offline_file(scenario_path: PathBuf) -> Result<OfflineReport, OfflineError> {
    let json = fs::read_to_string(&scenario_path).map_err(|source| OfflineError::ReadScenario {
        path: scenario_path.clone(),
        source,
    })?;
    let scenario = Scenario::from_json_str(&json).map_err(OfflineError::Scenario)?;
    let replay = replay_scenario(&scenario)?;

    Ok(OfflineReport {
        scenario_path,
        final_frame: replay.final_frame,
        final_hash: replay.final_hash,
    })
}

pub fn resolve_scenario_path(
    scenario: impl AsRef<Path>,
    scenario_dir: impl AsRef<Path>,
) -> Result<PathBuf, OfflineError> {
    let scenario = scenario.as_ref();
    if scenario.exists() {
        return Ok(scenario.to_path_buf());
    }

    let scenario_dir = scenario_dir.as_ref();
    let mut candidates = Vec::new();
    candidates.push(scenario_dir.join(scenario));

    if scenario.extension().is_none() {
        candidates.push(scenario_dir.join(scenario).with_extension("json"));
    }

    for candidate in &candidates {
        if candidate.exists() {
            return Ok(candidate.clone());
        }
    }

    Err(OfflineError::ScenarioNotFound {
        input: scenario.display().to_string(),
        tried: candidates,
    })
}

fn default_scenario_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayResult {
    pub final_frame: u32,
    pub final_hash: SimHash,
}

pub fn replay_scenario(scenario: &Scenario) -> Result<ReplayResult, OfflineError> {
    let config = scenario.to_sim_config();
    let mut server_sim = scenario
        .to_initial_world()
        .map_err(OfflineError::Scenario)?;
    let mut client_sim = server_sim.clone();
    let inputs = scenario.to_sim_inputs().map_err(OfflineError::Scenario)?;
    let initial_frame = scenario.initial.frame;
    let final_frame = scenario.assertions.final_frame;

    if final_frame > initial_frame {
        for frame in (initial_frame + 1)..=final_frame {
            let frame_id = FrameId::new(frame);
            let frame_inputs = collect_frame_inputs(&inputs, frame_id);

            let server_result =
                step(&mut server_sim, frame_id, &frame_inputs, &config).map_err(|source| {
                    OfflineError::Step {
                        side: SimSide::Server,
                        frame,
                        source,
                    }
                })?;
            let client_result =
                step(&mut client_sim, frame_id, &frame_inputs, &config).map_err(|source| {
                    OfflineError::Step {
                        side: SimSide::Client,
                        frame,
                        source,
                    }
                })?;

            if server_result.state_hash != client_result.state_hash {
                let diff = MismatchDiff::new(
                    frame,
                    server_result.state_hash,
                    client_result.state_hash,
                    &server_sim,
                    &client_sim,
                    &frame_inputs,
                );
                return Err(OfflineError::FrameHashMismatch { diff });
            }
        }
    }

    let final_hash = hash_world(&server_sim);
    let client_final_hash = hash_world(&client_sim);
    if final_hash != client_final_hash {
        let final_frame_inputs = collect_frame_inputs(&inputs, FrameId::new(final_frame));
        let diff = MismatchDiff::new(
            final_frame,
            final_hash,
            client_final_hash,
            &server_sim,
            &client_sim,
            &final_frame_inputs,
        );
        return Err(OfflineError::FrameHashMismatch { diff });
    }

    let expected_final_hash = scenario
        .expected_final_hash()
        .map_err(OfflineError::Scenario)?;

    // A zero hash is the scenario bless-time placeholder used before stage 7.
    if expected_final_hash.value != 0 && expected_final_hash != final_hash {
        return Err(OfflineError::FinalHashMismatch {
            expected: expected_final_hash,
            actual: final_hash,
        });
    }

    Ok(ReplayResult {
        final_frame,
        final_hash,
    })
}

fn collect_frame_inputs(inputs: &[SimInput], frame_id: FrameId) -> Vec<SimInput> {
    inputs
        .iter()
        .filter(|input| input.frame == frame_id)
        .cloned()
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MismatchDiff {
    pub frame: u32,
    pub server_hash: SimHash,
    pub client_hash: SimHash,
    pub server_entity_count: usize,
    pub client_entity_count: usize,
    pub entity_diffs: Vec<EntityDiff>,
    pub inputs: Vec<InputSummary>,
}

impl MismatchDiff {
    pub fn new(
        frame: u32,
        server_hash: SimHash,
        client_hash: SimHash,
        server_world: &SimWorld,
        client_world: &SimWorld,
        frame_inputs: &[SimInput],
    ) -> Self {
        Self {
            frame,
            server_hash,
            client_hash,
            server_entity_count: server_world.entities_sorted_by_id().len(),
            client_entity_count: client_world.entities_sorted_by_id().len(),
            entity_diffs: build_entity_diffs(server_world, client_world),
            inputs: frame_inputs.iter().map(InputSummary::from_input).collect(),
        }
    }

    pub fn entity_count_delta(&self) -> i128 {
        self.client_entity_count as i128 - self.server_entity_count as i128
    }
}

impl fmt::Display for MismatchDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "hash mismatch: first mismatch frame {}", self.frame)?;
        write!(f, "\nserver_hash: {:016x}", self.server_hash.value)?;
        write!(f, "\nclient_hash: {:016x}", self.client_hash.value)?;
        write!(
            f,
            "\nentity count: server={} client={} diff={:+}",
            self.server_entity_count,
            self.client_entity_count,
            self.entity_count_delta()
        )?;

        if self.entity_diffs.is_empty() {
            write!(f, "\nentity diffs: none in tracked fields")?;
        } else {
            write!(f, "\nentity diffs:")?;
            for diff in &self.entity_diffs {
                write!(f, "\n  - entity {}:", diff.entity_id)?;
                if diff.has_position_diff() {
                    write!(
                        f,
                        "\n    pos: server_pos={} client_pos={}",
                        format_optional(diff.server_pos),
                        format_optional(diff.client_pos)
                    )?;
                }
                if diff.has_hp_diff() {
                    write!(
                        f,
                        "\n    hp: server_hp={} client_hp={}",
                        format_optional(diff.server_hp),
                        format_optional(diff.client_hp)
                    )?;
                }
                if diff.has_alive_diff() {
                    write!(
                        f,
                        "\n    alive: server_alive={} client_alive={}",
                        format_optional(diff.server_alive),
                        format_optional(diff.client_alive)
                    )?;
                }
                if diff.has_movement_diff() {
                    write!(
                        f,
                        "\n    movement: server_movement={} client_movement={}",
                        format_optional(diff.server_movement),
                        format_optional(diff.client_movement)
                    )?;
                }
            }
        }

        if self.inputs.is_empty() {
            write!(f, "\ninputs: none")?;
        } else {
            write!(f, "\ninputs:")?;
            for input in &self.inputs {
                write!(f, "\n  - {input}")?;
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntityDiff {
    pub entity_id: u32,
    pub server_pos: Option<PositionSummary>,
    pub client_pos: Option<PositionSummary>,
    pub server_hp: Option<i32>,
    pub client_hp: Option<i32>,
    pub server_alive: Option<bool>,
    pub client_alive: Option<bool>,
    pub server_movement: Option<MovementSummary>,
    pub client_movement: Option<MovementSummary>,
}

impl EntityDiff {
    fn new(
        entity_id: EntityId,
        server_entity: Option<&SimEntity>,
        client_entity: Option<&SimEntity>,
    ) -> Option<Self> {
        let diff = Self {
            entity_id: entity_id.raw(),
            server_pos: server_entity.map(PositionSummary::from_entity),
            client_pos: client_entity.map(PositionSummary::from_entity),
            server_hp: server_entity.map(|entity| entity.combat.hp),
            client_hp: client_entity.map(|entity| entity.combat.hp),
            server_alive: server_entity.map(|entity| entity.alive),
            client_alive: client_entity.map(|entity| entity.alive),
            server_movement: server_entity.map(MovementSummary::from_entity),
            client_movement: client_entity.map(MovementSummary::from_entity),
        };

        if diff.has_any_diff() {
            Some(diff)
        } else {
            None
        }
    }

    fn has_any_diff(&self) -> bool {
        self.has_position_diff()
            || self.has_hp_diff()
            || self.has_alive_diff()
            || self.has_movement_diff()
    }

    fn has_position_diff(&self) -> bool {
        self.server_pos != self.client_pos
    }

    fn has_hp_diff(&self) -> bool {
        self.server_hp != self.client_hp
    }

    fn has_alive_diff(&self) -> bool {
        self.server_alive != self.client_alive
    }

    fn has_movement_diff(&self) -> bool {
        self.server_movement != self.client_movement
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionSummary {
    pub x: i64,
    pub y: i64,
}

impl PositionSummary {
    fn from_entity(entity: &SimEntity) -> Self {
        let (x, y) = entity.transform.pos.raw_tuple();
        Self { x, y }
    }
}

impl fmt::Display for PositionSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MovementSummary {
    pub mode: MovementMode,
    pub dir_x: i16,
    pub dir_y: i16,
    pub speed_per_second_milli: i64,
}

impl MovementSummary {
    fn from_entity(entity: &SimEntity) -> Self {
        let (dir_x, dir_y) = entity.movement.move_dir.raw_tuple();
        Self {
            mode: entity.movement.mode,
            dir_x,
            dir_y,
            speed_per_second_milli: entity.movement.speed_per_second.raw(),
        }
    }
}

impl fmt::Display for MovementSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "mode={:?} dir=({}, {}) speed_per_second_milli={}",
            self.mode, self.dir_x, self.dir_y, self.speed_per_second_milli
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputSummary {
    pub frame: u32,
    pub character_id: String,
    pub entity_id: u32,
    pub seq: u32,
    pub command: String,
}

impl InputSummary {
    fn from_input(input: &SimInput) -> Self {
        Self {
            frame: input.frame.raw(),
            character_id: input.character_id.clone(),
            entity_id: input.entity_id.raw(),
            seq: input.seq,
            command: format_command(&input.command),
        }
    }
}

impl fmt::Display for InputSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "frame={} character_id={} entity_id={} seq={} command={}",
            self.frame, self.character_id, self.entity_id, self.seq, self.command
        )
    }
}

fn build_entity_diffs(server_world: &SimWorld, client_world: &SimWorld) -> Vec<EntityDiff> {
    let server_entities = server_world
        .entities_sorted_by_id()
        .iter()
        .map(|entity| (entity.id, entity))
        .collect::<BTreeMap<_, _>>();
    let client_entities = client_world
        .entities_sorted_by_id()
        .iter()
        .map(|entity| (entity.id, entity))
        .collect::<BTreeMap<_, _>>();
    let mut entity_ids = server_entities.keys().copied().collect::<BTreeSet<_>>();
    entity_ids.extend(client_entities.keys().copied());

    entity_ids
        .into_iter()
        .filter_map(|entity_id| {
            EntityDiff::new(
                entity_id,
                server_entities.get(&entity_id).copied(),
                client_entities.get(&entity_id).copied(),
            )
        })
        .collect()
}

fn format_command(command: &SimCommand) -> String {
    match command {
        SimCommand::Move(command) => {
            let (dir_x, dir_y) = command.dir.raw_tuple();
            match command.speed_per_second {
                Some(speed) => format!(
                    "Move(dir=({}, {}), speed_per_second_milli={})",
                    dir_x,
                    dir_y,
                    speed.raw()
                ),
                None => format!(
                    "Move(dir=({}, {}), speed_per_second_milli=<default>)",
                    dir_x, dir_y
                ),
            }
        }
        SimCommand::Stop => "Stop".to_owned(),
        SimCommand::Face(command) => {
            let (dir_x, dir_y) = command.dir.raw_tuple();
            format!("Face(dir=({}, {}))", dir_x, dir_y)
        }
        SimCommand::CastSkill(command) => format!(
            "CastSkill(skill_id={}, target={})",
            command.skill_id.raw(),
            format_skill_target(command.target)
        ),
        SimCommand::Noop => "Noop".to_owned(),
    }
}

fn format_skill_target(target: SkillTarget) -> String {
    match target {
        SkillTarget::None => "None".to_owned(),
        SkillTarget::Entity(entity_id) => format!("Entity({})", entity_id.raw()),
        SkillTarget::Position(pos) => {
            let (x, y) = pos.raw_tuple();
            format!("Position({}, {})", x, y)
        }
        SkillTarget::Direction(dir) => {
            let (x, y) = dir.raw_tuple();
            format!("Direction({}, {})", x, y)
        }
    }
}

fn format_optional<T: fmt::Display>(value: Option<T>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "<missing>".to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimSide {
    Server,
    Client,
}

impl fmt::Display for SimSide {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Server => write!(f, "server_sim"),
            Self::Client => write!(f, "client_sim"),
        }
    }
}

#[derive(Debug)]
pub enum OfflineError {
    InvalidArgs {
        message: String,
    },
    ScenarioNotFound {
        input: String,
        tried: Vec<PathBuf>,
    },
    ReadScenario {
        path: PathBuf,
        source: io::Error,
    },
    Scenario(ScenarioError),
    Step {
        side: SimSide,
        frame: u32,
        source: StepError,
    },
    FrameHashMismatch {
        diff: MismatchDiff,
    },
    FinalHashMismatch {
        expected: SimHash,
        actual: SimHash,
    },
}

impl OfflineError {
    fn invalid_args(message: impl Into<String>) -> Self {
        Self::InvalidArgs {
            message: message.into(),
        }
    }
}

impl fmt::Display for OfflineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgs { message } => write!(f, "{message}\n{USAGE}"),
            Self::ScenarioNotFound { input, tried } => {
                write!(f, "scenario `{input}` not found")?;
                if !tried.is_empty() {
                    write!(f, "; tried")?;
                    for candidate in tried {
                        write!(f, " {}", candidate.display())?;
                    }
                }
                Ok(())
            }
            Self::ReadScenario { path, source } => {
                write!(f, "failed to read scenario `{}`: {source}", path.display())
            }
            Self::Scenario(error) => write!(f, "{error}"),
            Self::Step {
                side,
                frame,
                source,
            } => write!(f, "{side} failed at frame {frame}: {source}"),
            Self::FrameHashMismatch { diff } => write!(f, "{diff}"),
            Self::FinalHashMismatch { expected, actual } => write!(
                f,
                "final hash mismatch at frame {}: expected {:016x}, got {:016x}",
                expected.frame.raw(),
                expected.value,
                actual.value
            ),
        }
    }
}

impl std::error::Error for OfflineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadScenario { source, .. } => Some(source),
            Self::Scenario(error) => Some(error),
            Self::Step { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::{
        CombatState, EntityKind, FaceCommand, Fp, MoveCommand, MovementState, QuantizedDir,
        SimInputSource, SimRngState, SimTransform, TeamId, Vec2Fp,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

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
                "frame": 3,
                "characterId": "chr_a",
                "entityId": 1001,
                "seq": 2,
                "command": { "type": "Stop" }
            }
        ],
        "assertions": {
            "finalFrame": 5,
            "finalHash": "0000000000000000"
        }
    }"#;

    #[test]
    fn cli_parse_accepts_offline_scenario_args() {
        let options = CliOptions::parse(["--mode", "offline", "--scenario", "smoke"]).unwrap();

        assert_eq!(options.mode, CliMode::Offline);
        assert_eq!(options.scenario, "smoke");
    }

    #[test]
    fn resolve_scenario_path_uses_existing_path_or_default_json_name() {
        let temp_dir = unique_temp_dir();
        fs::create_dir_all(&temp_dir).unwrap();
        let direct_path = temp_dir.join("direct.scenario");
        let named_path = temp_dir.join("named.json");
        fs::write(&direct_path, "{}").unwrap();
        fs::write(&named_path, "{}").unwrap();

        assert_eq!(
            resolve_scenario_path(&direct_path, &temp_dir).unwrap(),
            direct_path
        );
        assert_eq!(
            resolve_scenario_path("named", &temp_dir).unwrap(),
            named_path
        );

        fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[test]
    fn replay_succeeds_when_expected_hash_is_placeholder_zero() {
        let scenario = Scenario::from_json_str(VALID_SCENARIO).unwrap();

        let result = replay_scenario(&scenario).unwrap();

        assert_eq!(result.final_frame, 5);
        assert_eq!(result.final_hash.frame, FrameId::new(5));
    }

    #[test]
    fn replay_rejects_non_zero_expected_final_hash_mismatch() {
        let json = VALID_SCENARIO.replace(
            r#""finalHash": "0000000000000000""#,
            r#""finalHash": "ffffffffffffffff""#,
        );
        let scenario = Scenario::from_json_str(&json).unwrap();

        let error = replay_scenario(&scenario).unwrap_err();

        assert!(matches!(error, OfflineError::FinalHashMismatch { .. }));
    }

    #[test]
    fn invalid_input_fixture_is_rejected_with_readable_error() {
        let error = run_offline_by_name_or_path("move_invalid_input").unwrap_err();
        let message = error.to_string();

        assert!(matches!(
            &error,
            OfflineError::Scenario(ScenarioError::InvalidInput { .. })
        ));
        assert!(message.contains("invalid scenario input"));
        assert!(message.contains("speedPerSecondMilli exceeds"));
    }

    #[test]
    fn frame_hash_mismatch_display_includes_readable_diff() {
        let server_world = SimWorld::with_rng(
            FrameId::new(7),
            SimRngState {
                seed: 11,
                counter: 0,
            },
            vec![
                test_entity(
                    1001,
                    "chr_a",
                    Vec2Fp::new(Fp::from_milli(1_000), Fp::from_milli(2_000)),
                    100,
                    true,
                    MovementState {
                        mode: MovementMode::Controlled,
                        move_dir: QuantizedDir::RIGHT,
                        speed_per_second: Fp::from_milli(6_000),
                    },
                ),
                test_entity(
                    2002,
                    "chr_b",
                    Vec2Fp::new(Fp::from_milli(4_000), Fp::from_milli(5_000)),
                    80,
                    true,
                    MovementState::default(),
                ),
            ],
        )
        .unwrap();
        let client_world = SimWorld::with_rng(
            FrameId::new(7),
            SimRngState {
                seed: 11,
                counter: 0,
            },
            vec![test_entity(
                1001,
                "chr_a",
                Vec2Fp::new(Fp::from_milli(3_000), Fp::from_milli(2_000)),
                90,
                false,
                MovementState {
                    mode: MovementMode::Idle,
                    move_dir: QuantizedDir::ZERO,
                    speed_per_second: Fp::ZERO,
                },
            )],
        )
        .unwrap();
        let inputs = vec![
            SimInput {
                frame: FrameId::new(7),
                character_id: "chr_a".to_owned(),
                entity_id: EntityId::new(1001),
                seq: 42,
                source: SimInputSource::Real,
                command: SimCommand::Move(MoveCommand {
                    dir: QuantizedDir::RIGHT,
                    speed_per_second: Some(Fp::from_milli(6_000)),
                }),
            },
            SimInput {
                frame: FrameId::new(7),
                character_id: "chr_b".to_owned(),
                entity_id: EntityId::new(2002),
                seq: 9,
                source: SimInputSource::Real,
                command: SimCommand::Face(FaceCommand {
                    dir: QuantizedDir::LEFT,
                }),
            },
        ];

        let error = OfflineError::FrameHashMismatch {
            diff: MismatchDiff::new(
                7,
                hash_world(&server_world),
                hash_world(&client_world),
                &server_world,
                &client_world,
                &inputs,
            ),
        };
        let display = error.to_string();

        assert!(display.contains("first mismatch frame 7"));
        assert!(display.contains("server_hash:"));
        assert!(display.contains("client_hash:"));
        assert!(display.contains("entity count: server=2 client=1 diff=-1"));
        assert!(display.contains("entity 1001"));
        assert!(display.contains("server_pos=(1000, 2000)"));
        assert!(display.contains("client_pos=(3000, 2000)"));
        assert!(display.contains("server_hp=100"));
        assert!(display.contains("client_hp=90"));
        assert!(display.contains("server_alive=true"));
        assert!(display.contains("client_alive=false"));
        assert!(display.contains("server_movement=mode=Controlled"));
        assert!(display.contains("client_movement=mode=Idle"));
        assert!(display.contains("entity 2002"));
        assert!(display.contains("client_pos=<missing>"));
        assert!(display.contains("frame=7"));
        assert!(display.contains("character_id=chr_a"));
        assert!(display.contains("entity_id=1001"));
        assert!(display.contains("seq=42"));
        assert!(display.contains("command=Move"));
    }

    fn test_entity(
        id: u32,
        character_id: &str,
        pos: Vec2Fp,
        hp: i32,
        alive: bool,
        movement: MovementState,
    ) -> SimEntity {
        SimEntity {
            id: EntityId::new(id),
            kind: EntityKind::Player,
            owner_character_id: Some(character_id.to_owned()),
            team_id: TeamId::new(1),
            transform: SimTransform {
                pos,
                facing: QuantizedDir::RIGHT,
                radius: Fp::from_milli(500),
            },
            movement,
            combat: CombatState {
                hp,
                max_hp: 100,
                attack: 10,
                defense: 3,
                speed: 6,
                crit_rate_bps: 500,
                crit_damage_bps: 15_000,
                skill_slots: Vec::new(),
                buffs: Vec::new(),
            },
            alive,
        }
    }

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("lockstep-client-test-{nanos}"))
    }
}
