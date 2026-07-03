use crate::scenario::{Scenario, ScenarioError};
use sim_core::{FrameId, SimHash, StepError, hash_world, step};
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
            let frame_inputs = inputs
                .iter()
                .filter(|input| input.frame == frame_id)
                .cloned()
                .collect::<Vec<_>>();

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
                return Err(OfflineError::FrameHashMismatch {
                    frame,
                    server_hash: server_result.state_hash,
                    client_hash: client_result.state_hash,
                });
            }
        }
    }

    let final_hash = hash_world(&server_sim);
    let client_final_hash = hash_world(&client_sim);
    if final_hash != client_final_hash {
        return Err(OfflineError::FrameHashMismatch {
            frame: final_frame,
            server_hash: final_hash,
            client_hash: client_final_hash,
        });
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
        frame: u32,
        server_hash: SimHash,
        client_hash: SimHash,
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
            Self::FrameHashMismatch {
                frame,
                server_hash,
                client_hash,
            } => write!(
                f,
                "hash mismatch at frame {frame}: server {:016x}, client {:016x}",
                server_hash.value, client_hash.value
            ),
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

    fn unique_temp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("lockstep-client-test-{nanos}"))
    }
}
