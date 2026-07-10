use crate::offline::MismatchDiff;
use crate::scenario::{Scenario, ScenarioError};
use sim_core::{
    CastSkillCommand, CombatConfig, CombatEffect, EntityId, FaceCommand, Fp, FrameId, MoveCommand,
    MovementConfig, QuantizedDir, SceneBounds, SimCommand, SimConfig, SimEntity, SimEvent, SimHash,
    SimInput, SimInputSource, SimSnapshot, SimWorld, SkillDefinition, SkillId, SkillTarget,
    SkillTargetType, StaticObstacle, StepError, Vec2Fp, hash_world,
    restore as restore_sim_snapshot, step,
};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[allow(dead_code)]
mod pb {
    include!("../../../apps/game-server/src/proto/myserver.game.rs");
}

const USAGE: &str = "\
usage: lockstep-client --mode online --scenario <path-or-name> [options]

options:
  --server <host:port>        TCP game-server or local game-proxy endpoint, default 127.0.0.1:7000
  --ticket <ticket>          ticket issued by auth-http or a local test ticket
  --test-ticket <ticket>     alias for --ticket
  --probe-observer-recovery  verify RoomJoinAsObserverRes.snapshot.game_state after replay
  --observer-ticket <ticket> ticket for the observer recovery probe
  --room <room-id>           room id, default lockstep-online-demo
  --policy <policy-id>       room policy id, default lockstep_sim_demo
  --character-id <id>        expected ticket-bound character id for reporting
  --timeout-ms <ms>          socket/read timeout, default 5000
  --dry-run                  parse scenario and print the packets that would be sent";

const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:7000";
const DEFAULT_ROOM_ID: &str = "lockstep-online-demo";
const LOCKSTEP_SIM_DEMO_POLICY_ID: &str = "lockstep_sim_demo";
const SIM_INPUT_ACTION: &str = "sim_input";
const SIM_INPUT_VERSION: u32 = 1;
const SIM_INITIAL_SNAPSHOT_SCHEMA: &str = "myserver.lockstep-sim.initial-snapshot.v1";
const SIM_FRAME_ENVELOPE_SCHEMA: &str = "myserver.lockstep-sim.frame-envelope.v1";
const SIM_DOWNLINK_SCHEMA_VERSION: u32 = 1;
const DEFAULT_PLAYER_SKILL_ID: u32 = 1;
const SIM_INPUT_MAX_SPEED_MILLI: i64 = 12_000;
const DEFAULT_ONLINE_INPUT_DELAY_FRAMES: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnlineCliOptions {
    pub scenario: String,
    pub server_addr: String,
    pub ticket: Option<String>,
    pub observer_ticket: Option<String>,
    pub probe_observer_recovery: bool,
    pub room_id: String,
    pub policy_id: String,
    pub character_id: Option<String>,
    pub timeout_ms: u64,
    pub dry_run: bool,
}

impl OnlineCliOptions {
    pub fn parse<I, S>(args: I) -> Result<Self, OnlineError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut mode = None;
        let mut scenario = None;
        let mut server_addr = None;
        let mut ticket = None;
        let mut observer_ticket = None;
        let mut probe_observer_recovery = false;
        let mut room_id = None;
        let mut policy_id = None;
        let mut character_id = None;
        let mut timeout_ms = None;
        let mut dry_run = false;
        let mut args = args.into_iter().map(Into::into);

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--mode" => {
                    if mode.is_some() {
                        return Err(OnlineError::invalid_args("duplicate --mode"));
                    }
                    let value = next_arg(&mut args, "--mode")?;
                    if value != "online" {
                        return Err(OnlineError::invalid_args(format!(
                            "unsupported --mode `{value}` for online runner; expected `online`"
                        )));
                    }
                    mode = Some(value);
                }
                "--scenario" => {
                    if scenario.is_some() {
                        return Err(OnlineError::invalid_args("duplicate --scenario"));
                    }
                    let value = next_non_empty_arg(&mut args, "--scenario")?;
                    scenario = Some(value);
                }
                "--server" => {
                    if server_addr.is_some() {
                        return Err(OnlineError::invalid_args("duplicate --server"));
                    }
                    server_addr = Some(next_non_empty_arg(&mut args, "--server")?);
                }
                "--ticket" | "--test-ticket" => {
                    if ticket.is_some() {
                        return Err(OnlineError::invalid_args("duplicate --ticket"));
                    }
                    ticket = Some(next_non_empty_arg(&mut args, arg.as_str())?);
                }
                "--observer-ticket" => {
                    if observer_ticket.is_some() {
                        return Err(OnlineError::invalid_args("duplicate --observer-ticket"));
                    }
                    observer_ticket = Some(next_non_empty_arg(&mut args, "--observer-ticket")?);
                }
                "--probe-observer-recovery" => {
                    probe_observer_recovery = true;
                }
                "--room" | "--room-id" => {
                    if room_id.is_some() {
                        return Err(OnlineError::invalid_args("duplicate --room"));
                    }
                    room_id = Some(next_non_empty_arg(&mut args, arg.as_str())?);
                }
                "--policy" | "--policy-id" => {
                    if policy_id.is_some() {
                        return Err(OnlineError::invalid_args("duplicate --policy"));
                    }
                    policy_id = Some(next_non_empty_arg(&mut args, arg.as_str())?);
                }
                "--character-id" => {
                    if character_id.is_some() {
                        return Err(OnlineError::invalid_args("duplicate --character-id"));
                    }
                    character_id = Some(next_non_empty_arg(&mut args, "--character-id")?);
                }
                "--timeout-ms" => {
                    if timeout_ms.is_some() {
                        return Err(OnlineError::invalid_args("duplicate --timeout-ms"));
                    }
                    let value = next_non_empty_arg(&mut args, "--timeout-ms")?;
                    let parsed = value.parse::<u64>().map_err(|_| {
                        OnlineError::invalid_args(format!(
                            "--timeout-ms must be a positive integer, got `{value}`"
                        ))
                    })?;
                    if parsed == 0 {
                        return Err(OnlineError::invalid_args(
                            "--timeout-ms must be greater than zero",
                        ));
                    }
                    timeout_ms = Some(parsed);
                }
                "--dry-run" => {
                    dry_run = true;
                }
                "--help" | "-h" => {
                    return Err(OnlineError::invalid_args(USAGE));
                }
                _ => {
                    return Err(OnlineError::invalid_args(format!(
                        "unexpected argument `{arg}`"
                    )));
                }
            }
        }

        if mode.as_deref() != Some("online") {
            return Err(OnlineError::invalid_args("missing --mode online"));
        }

        Ok(Self {
            scenario: scenario
                .ok_or_else(|| OnlineError::invalid_args("missing --scenario <path-or-name>"))?,
            server_addr: server_addr.unwrap_or_else(|| DEFAULT_SERVER_ADDR.to_owned()),
            ticket,
            observer_ticket,
            probe_observer_recovery,
            room_id: room_id.unwrap_or_else(|| DEFAULT_ROOM_ID.to_owned()),
            policy_id: policy_id.unwrap_or_else(|| LOCKSTEP_SIM_DEMO_POLICY_ID.to_owned()),
            character_id,
            timeout_ms: timeout_ms.unwrap_or(5_000),
            dry_run,
        })
    }

    fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

fn next_arg(args: &mut impl Iterator<Item = String>, name: &str) -> Result<String, OnlineError> {
    args.next()
        .ok_or_else(|| OnlineError::invalid_args(format!("missing value for {name}")))
}

fn next_non_empty_arg(
    args: &mut impl Iterator<Item = String>,
    name: &str,
) -> Result<String, OnlineError> {
    let value = next_arg(args, name)?;
    if value.trim().is_empty() {
        return Err(OnlineError::invalid_args(format!(
            "{name} must not be empty"
        )));
    }
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnlineReport {
    pub scenario_path: PathBuf,
    pub server_addr: String,
    pub room_id: String,
    pub policy_id: String,
    pub dry_run: bool,
    pub dry_run_packets: Vec<OnlineDryRunPacket>,
    pub input_plan_count: usize,
    pub frames_checked: usize,
    pub final_frame: Option<u32>,
    pub final_hash: Option<SimHash>,
    pub observer_recovery: Option<ObserverRecoveryProbeReport>,
}

impl fmt::Display for OnlineReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "scenario: {}", self.scenario_path.display())?;
        writeln!(
            f,
            "mode: online{}",
            if self.dry_run { " (dry-run)" } else { "" }
        )?;
        writeln!(f, "server: {}", self.server_addr)?;
        writeln!(f, "room: {}", self.room_id)?;
        writeln!(f, "policy: {}", self.policy_id)?;
        writeln!(f, "sim_input packets: {}", self.input_plan_count)?;
        if self.dry_run && !self.dry_run_packets.is_empty() {
            writeln!(f, "dry-run packet plan:")?;
            for packet in &self.dry_run_packets {
                writeln!(f, "  - {packet}")?;
            }
        }
        writeln!(f, "frames checked: {}", self.frames_checked)?;
        if let Some(frame) = self.final_frame {
            writeln!(f, "final frame: {frame}")?;
        }
        if let Some(hash) = self.final_hash {
            writeln!(f, "final hash: {:016x}", hash.value)?;
        } else if self.dry_run {
            writeln!(f, "network: not started; dry-run only")?;
        } else {
            writeln!(f, "final hash: <none>")?;
        }
        if let Some(recovery) = &self.observer_recovery {
            writeln!(f, "observer recovery: ok")?;
            writeln!(f, "observer current frame: {}", recovery.current_frame_id)?;
            writeln!(f, "observer snapshot frame: {}", recovery.snapshot_frame_id)?;
            writeln!(
                f,
                "observer initial snapshot frame: {}",
                recovery.initial_snapshot_frame
            )?;
            writeln!(f, "observer last frame: {}", recovery.last_frame)?;
            writeln!(
                f,
                "observer observerFrame.lastFrame: {}",
                recovery.observer_last_frame
            )?;
            write!(f, "observer hash: {:016x}", recovery.observer_hash.value)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnlineDryRunPacket {
    pub direction: &'static str,
    pub name: &'static str,
    pub msg_type: Option<u16>,
    pub summary: String,
}

impl fmt::Display for OnlineDryRunPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.msg_type {
            Some(msg_type) => write!(
                f,
                "{} {}({msg_type}): {}",
                self.direction, self.name, self.summary
            ),
            None => write!(f, "{} {}: {}", self.direction, self.name, self.summary),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObserverRecoveryProbeReport {
    pub room_id: String,
    pub current_frame_id: u32,
    pub snapshot_frame_id: u32,
    pub initial_snapshot_frame: u32,
    pub last_frame: u32,
    pub observer_last_frame: u32,
    pub observer_hash: SimHash,
}

pub fn run_cli<I, S>(args: I) -> Result<OnlineReport, OnlineError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let options = OnlineCliOptions::parse(args)?;
    run_online(options)
}

pub fn run_online(options: OnlineCliOptions) -> Result<OnlineReport, OnlineError> {
    let scenario_path =
        crate::offline::resolve_scenario_path(&options.scenario, default_scenario_dir())?;
    let json = fs::read_to_string(&scenario_path).map_err(|source| OnlineError::ReadScenario {
        path: scenario_path.clone(),
        source,
    })?;
    let scenario = Scenario::from_json_str(&json).map_err(OnlineError::Scenario)?;
    let sim_inputs = scenario.to_sim_inputs().map_err(OnlineError::Scenario)?;
    let input_plan = build_player_input_plan(&sim_inputs)?;

    if options.dry_run {
        let dry_run_packets = build_dry_run_packet_plan(&options, &input_plan);
        return Ok(OnlineReport {
            scenario_path,
            server_addr: options.server_addr,
            room_id: options.room_id,
            policy_id: options.policy_id,
            dry_run: true,
            dry_run_packets,
            input_plan_count: input_plan.len(),
            frames_checked: 0,
            final_frame: None,
            final_hash: None,
            observer_recovery: None,
        });
    }

    let ticket = options.ticket.clone().ok_or_else(|| {
        OnlineError::invalid_args(
            "online mode requires --ticket or --test-ticket unless --dry-run is used",
        )
    })?;

    let mut transport = TcpGameTransport::connect(&options.server_addr, options.timeout())?;
    let outcome = drive_online_session(&mut transport, &options, &ticket, &input_plan)?;
    let observer_recovery = if options.probe_observer_recovery {
        let observer_ticket = options.observer_ticket.as_deref().ok_or_else(|| {
            OnlineError::invalid_args(
                "--probe-observer-recovery requires --observer-ticket for a separate observer character",
            )
        })?;
        Some(run_observer_recovery_probe(&options, observer_ticket)?)
    } else {
        None
    };

    Ok(OnlineReport {
        scenario_path,
        server_addr: options.server_addr,
        room_id: options.room_id,
        policy_id: options.policy_id,
        dry_run: false,
        dry_run_packets: Vec::new(),
        input_plan_count: input_plan.len(),
        frames_checked: outcome.frames_checked,
        final_frame: outcome.final_hash.map(|hash| hash.frame.raw()),
        final_hash: outcome.final_hash,
        observer_recovery,
    })
}

fn default_scenario_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerInputPlan {
    pub frame_id: u32,
    pub action: String,
    pub payload_json: String,
}

pub fn build_player_input_plan(inputs: &[SimInput]) -> Result<Vec<PlayerInputPlan>, OnlineError> {
    inputs
        .iter()
        .map(|input| {
            Ok(PlayerInputPlan {
                frame_id: input.frame.raw(),
                action: SIM_INPUT_ACTION.to_owned(),
                payload_json: build_sim_input_payload(
                    input.seq,
                    std::slice::from_ref(&input.command),
                )?,
            })
        })
        .collect()
}

fn build_dry_run_packet_plan(
    options: &OnlineCliOptions,
    input_plan: &[PlayerInputPlan],
) -> Vec<OnlineDryRunPacket> {
    let mut packets = vec![
        OnlineDryRunPacket {
            direction: "send",
            name: "AuthReq",
            msg_type: Some(MessageType::AuthReq as u16),
            summary: format!(
                "ticket={}",
                options
                    .ticket
                    .as_deref()
                    .map(|_| "<provided>")
                    .unwrap_or("<not required for dry-run>")
            ),
        },
        OnlineDryRunPacket {
            direction: "send",
            name: "RoomJoinReq",
            msg_type: Some(MessageType::RoomJoinReq as u16),
            summary: format!(
                "create-or-join room={} policy={}",
                options.room_id, options.policy_id
            ),
        },
        OnlineDryRunPacket {
            direction: "expect",
            name: "RoomStatePush",
            msg_type: Some(MessageType::RoomStatePush as u16),
            summary: "RoomSnapshot.game_state.initialSnapshot".to_owned(),
        },
        OnlineDryRunPacket {
            direction: "send",
            name: "RoomReadyReq",
            msg_type: Some(MessageType::RoomReadyReq as u16),
            summary: "ready=true".to_owned(),
        },
        OnlineDryRunPacket {
            direction: "send",
            name: "RoomStartReq",
            msg_type: Some(MessageType::RoomStartReq as u16),
            summary: "start lockstep room".to_owned(),
        },
        OnlineDryRunPacket {
            direction: "expect",
            name: "FrameBundlePush",
            msg_type: Some(MessageType::FrameBundlePush as u16),
            summary: "snapshot.game_state.lastFrame or observerFrame.lastFrame with hash/events/eventSummaries/inputSources"
                .to_owned(),
        },
    ];

    packets.extend(input_plan.iter().map(|input| OnlineDryRunPacket {
        direction: "send",
        name: "PlayerInputReq",
        msg_type: Some(MessageType::PlayerInputReq as u16),
        summary: format!(
            "frame={} action={} payload_json={}",
            input.frame_id, input.action, input.payload_json
        ),
    }));

    packets
}

pub fn build_sim_input_payload(seq: u32, commands: &[SimCommand]) -> Result<String, OnlineError> {
    let commands = commands
        .iter()
        .map(WireSimCommand::try_from_sim_command)
        .collect::<Result<Vec<_>, _>>()?;
    serde_json::to_string(&WireSimInputPayload {
        version: SIM_INPUT_VERSION,
        seq,
        commands,
    })
    .map_err(OnlineError::Json)
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct WireSimInputPayload {
    version: u32,
    seq: u32,
    commands: Vec<WireSimCommand>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
enum WireSimCommand {
    Move {
        #[serde(rename = "dirX")]
        dir_x: i16,
        #[serde(rename = "dirY")]
        dir_y: i16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
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
        #[serde(
            rename = "targetEntityId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        target_entity_id: Option<u32>,
    },
}

impl WireSimCommand {
    fn try_from_sim_command(command: &SimCommand) -> Result<Self, OnlineError> {
        match *command {
            SimCommand::Move(MoveCommand {
                dir,
                speed_per_second,
            }) => {
                let (dir_x, dir_y) = dir.raw_tuple();
                Ok(Self::Move {
                    dir_x,
                    dir_y,
                    speed: speed_per_second.map(|speed| speed.raw()),
                })
            }
            SimCommand::Stop => Ok(Self::Stop {}),
            SimCommand::Face(FaceCommand { dir }) => {
                let (dir_x, dir_y) = dir.raw_tuple();
                Ok(Self::Face { dir_x, dir_y })
            }
            SimCommand::CastSkill(CastSkillCommand { skill_id, target }) => {
                let target_entity_id = match target {
                    SkillTarget::None => None,
                    SkillTarget::Entity(entity_id) => Some(entity_id.raw()),
                    SkillTarget::Position(_) | SkillTarget::Direction(_) => {
                        return Err(OnlineError::UnsupportedOnlineCommand(
                            "lockstep_sim_demo online payload only supports CastSkill target None or Entity",
                        ));
                    }
                };
                Ok(Self::CastSkill {
                    skill_id: skill_id.raw(),
                    target_entity_id,
                })
            }
            SimCommand::Noop => Ok(Self::Stop {}),
        }
    }

    fn validate(self) -> Result<ParsedWireSimCommand, OnlineError> {
        match self {
            Self::Move {
                dir_x,
                dir_y,
                speed,
            } => {
                let dir = QuantizedDir::new(dir_x, dir_y)
                    .map_err(|_| OnlineError::InvalidSimInput("SIM_INPUT_DIR_OUT_OF_RANGE"))?;
                if dir == QuantizedDir::ZERO {
                    return Err(OnlineError::InvalidSimInput("SIM_INPUT_MOVE_DIR_ZERO"));
                }
                let speed_per_second = speed
                    .map(|speed| {
                        if speed <= 0 || speed > SIM_INPUT_MAX_SPEED_MILLI {
                            return Err(OnlineError::InvalidSimInput(
                                "SIM_INPUT_SPEED_OUT_OF_RANGE",
                            ));
                        }
                        Ok(Fp::from_milli(speed))
                    })
                    .transpose()?;
                Ok(ParsedWireSimCommand::Move {
                    dir,
                    speed_per_second,
                })
            }
            Self::Stop {} => Ok(ParsedWireSimCommand::Stop),
            Self::Face { dir_x, dir_y } => {
                let dir = QuantizedDir::new(dir_x, dir_y)
                    .map_err(|_| OnlineError::InvalidSimInput("SIM_INPUT_DIR_OUT_OF_RANGE"))?;
                Ok(ParsedWireSimCommand::Face { dir })
            }
            Self::CastSkill {
                skill_id,
                target_entity_id,
            } => {
                if skill_id == 0 {
                    return Err(OnlineError::InvalidSimInput(
                        "SIM_INPUT_SKILL_ID_OUT_OF_RANGE",
                    ));
                }
                let target = match target_entity_id {
                    Some(0) => {
                        return Err(OnlineError::InvalidSimInput(
                            "SIM_INPUT_TARGET_ENTITY_ID_OUT_OF_RANGE",
                        ));
                    }
                    Some(target_entity_id) => SkillTarget::Entity(EntityId::new(target_entity_id)),
                    None => SkillTarget::None,
                };
                Ok(ParsedWireSimCommand::CastSkill {
                    skill_id: SkillId::new(skill_id),
                    target,
                })
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParsedWireSimCommand {
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

impl ParsedWireSimCommand {
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

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimHashEnvelope {
    pub frame: u32,
    pub value: u64,
    pub hex: String,
}

impl SimHashEnvelope {
    fn to_sim_hash(&self) -> SimHash {
        SimHash {
            frame: FrameId::new(self.frame),
            value: self.value,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimControlBinding {
    pub character_id: String,
    pub entity_id: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimInitialSnapshot {
    pub schema: String,
    pub schema_version: u32,
    pub room_id: String,
    pub start_frame: u32,
    pub tick_rate: u16,
    pub config_version: u64,
    pub config_hash: String,
    pub sim_schema_version: u16,
    pub rng_seed: u64,
    pub state_hash: SimHashEnvelope,
    pub snapshot: SimSnapshot,
    #[serde(default)]
    pub entities: Vec<SimEntity>,
    #[serde(default)]
    pub control_bindings: Vec<SimControlBinding>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimFrameDebugSummary {
    pub input_count: usize,
    pub real_input_count: usize,
    pub synthetic_input_count: usize,
    #[serde(default)]
    pub synthesized_empty_input_count: usize,
    #[serde(default)]
    pub synthesized_repeat_last_input_count: usize,
    pub event_count: usize,
    pub entity_count: usize,
    pub alive_entity_count: usize,
    pub player_entity_count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimFrameInputSourceSummary {
    pub frame: u32,
    pub character_id: String,
    pub source: SimFrameInputSource,
    pub action: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SimFrameInputSource {
    Real,
    SynthesizedEmpty,
    SynthesizedRepeatLast,
}

impl From<SimFrameInputSource> for SimInputSource {
    fn from(value: SimFrameInputSource) -> Self {
        match value {
            SimFrameInputSource::Real => Self::Real,
            SimFrameInputSource::SynthesizedEmpty => Self::SynthesizedEmpty,
            SimFrameInputSource::SynthesizedRepeatLast => Self::SynthesizedRepeatLast,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SimFrameEventKind {
    SkillCast,
    Damage,
    Heal,
    BuffApplied,
    BuffExpired,
    BuffTick,
    Death,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimFrameEventSummary {
    pub schema_version: u32,
    pub kind: SimFrameEventKind,
    pub frame: u32,
    pub source_entity_id: u32,
    pub target_entity_id: Option<u32>,
    pub skill_id: Option<u32>,
    pub buff_id: Option<u32>,
    pub amount: i32,
    pub sequence: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimFrameDebugState {
    pub schema_version: u32,
    pub entities: Vec<SimFrameEntityDebugState>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimFrameEntityDebugState {
    pub entity_id: u32,
    pub x_raw: i64,
    pub y_raw: i64,
    pub hp: i32,
    pub max_hp: i32,
    pub alive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimFrameEnvelope {
    pub schema: String,
    pub schema_version: u32,
    pub room_id: String,
    pub frame: u32,
    pub tick_rate: u16,
    pub config_version: u64,
    pub config_hash: String,
    pub sim_schema_version: u16,
    pub state_hash: SimHashEnvelope,
    pub event_count: usize,
    pub events: Vec<SimEvent>,
    pub event_summaries: Vec<SimFrameEventSummary>,
    pub input_sources: Vec<SimFrameInputSourceSummary>,
    pub debug_summary: SimFrameDebugSummary,
    pub debug_state: SimFrameDebugState,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockstepSimDemoState {
    pub logic_type: Option<String>,
    pub room_id: Option<String>,
    #[serde(default)]
    pub world_frame: u32,
    #[serde(default)]
    pub tick_rate: u16,
    #[serde(default)]
    pub training_target_entity_id: u32,
    #[serde(default)]
    pub player_entities: Vec<LockstepSimPlayerDebugState>,
    #[serde(default)]
    pub training_target: Option<LockstepSimEntityDebugState>,
    #[serde(default)]
    pub initial_snapshot: Option<SimInitialSnapshot>,
    #[serde(default)]
    pub last_frame: Option<SimFrameEnvelope>,
    #[serde(default)]
    pub observer_frame: Option<LockstepSimObserverFrame>,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl LockstepSimDemoState {
    fn observed_frame(&self) -> Option<&SimFrameEnvelope> {
        self.observer_frame
            .as_ref()
            .and_then(|observer| observer.last_frame.as_ref())
            .or(self.last_frame.as_ref())
    }

    fn project_server_world(&self, local_world: &SimWorld) -> SimWorld {
        let mut server_world = local_world.clone();
        for debug in self
            .player_entities
            .iter()
            .map(|player| &player.entity)
            .chain(self.training_target.iter())
        {
            if let Some(entity) = server_world.entity_mut(EntityId::new(debug.entity_id)) {
                entity.transform.pos =
                    Vec2Fp::new(Fp::from_milli(debug.x), Fp::from_milli(debug.y));
                entity.combat.hp = debug.hp;
                entity.combat.max_hp = debug.max_hp;
                entity.alive = debug.alive;
            }
        }
        server_world
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockstepSimObserverFrame {
    pub world_frame: u32,
    pub state_hash: SimHashEnvelope,
    pub last_event_count: usize,
    pub last_event_summaries: Vec<SimFrameEventSummary>,
    #[serde(default)]
    pub last_frame: Option<SimFrameEnvelope>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockstepSimPlayerDebugState {
    pub character_id: String,
    #[serde(flatten)]
    pub entity: LockstepSimEntityDebugState,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockstepSimEntityDebugState {
    pub entity_id: u32,
    pub x: i64,
    pub y: i64,
    pub hp: i32,
    pub max_hp: i32,
    pub alive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameInputRecord {
    pub character_id: String,
    pub action: String,
    pub payload_json: String,
    pub frame_id: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerFrameObservation {
    pub envelope: SimFrameEnvelope,
    pub inputs: Vec<FrameInputRecord>,
    pub game_state: Option<LockstepSimDemoState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SimContractMetadata {
    config_version: u64,
    config_hash: String,
    sim_schema_version: u16,
}

impl From<&SimInitialSnapshot> for SimContractMetadata {
    fn from(snapshot: &SimInitialSnapshot) -> Self {
        Self {
            config_version: snapshot.config_version,
            config_hash: snapshot.config_hash.clone(),
            sim_schema_version: snapshot.sim_schema_version,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OnlineReplay {
    room_id: String,
    contract: SimContractMetadata,
    config: SimConfig,
    world: SimWorld,
    bindings: HashMap<String, EntityId>,
    frames_checked: usize,
    final_hash: SimHash,
}

impl OnlineReplay {
    pub fn from_initial_snapshot(snapshot: &SimInitialSnapshot) -> Result<Self, OnlineError> {
        validate_initial_snapshot(snapshot)?;
        let world = restore_sim_snapshot(&snapshot.snapshot)
            .map_err(|error| OnlineError::InvalidInitialSnapshot(error.to_string()))?;
        let final_hash = hash_world(&world);
        let bindings = restore_control_bindings(&snapshot.control_bindings, &world)?;

        Ok(Self {
            room_id: snapshot.room_id.clone(),
            contract: SimContractMetadata::from(snapshot),
            config: lockstep_demo_config(snapshot.tick_rate),
            world,
            bindings,
            frames_checked: 0,
            final_hash,
        })
    }

    pub fn current_frame(&self) -> u32 {
        self.world.frame.raw()
    }

    pub fn final_hash(&self) -> SimHash {
        self.final_hash
    }

    pub fn frames_checked(&self) -> usize {
        self.frames_checked
    }

    pub fn apply_server_frame(
        &mut self,
        observation: &ServerFrameObservation,
    ) -> Result<(), OnlineError> {
        let envelope = &observation.envelope;
        validate_frame_envelope(envelope, &self.room_id, &self.contract)?;

        if envelope.frame <= self.world.frame.raw() {
            return Ok(());
        }

        let inputs = sim_inputs_from_frame_records(
            &observation.inputs,
            &envelope.input_sources,
            &self.bindings,
        )?;
        let result = step(
            &mut self.world,
            FrameId::new(envelope.frame),
            &inputs,
            &self.config,
        )
        .map_err(|source| OnlineError::Step {
            frame: envelope.frame,
            source,
        })?;
        self.final_hash = result.state_hash;
        self.frames_checked += 1;

        let server_hash = envelope.state_hash.to_sim_hash();
        let hash_matches = result.state_hash == server_hash;
        let events_match = result.events == envelope.events;
        if !hash_matches || !events_match {
            let server_world = observation
                .game_state
                .as_ref()
                .map(|state| state.project_server_world(&self.world))
                .unwrap_or_else(|| self.world.clone());
            let diff = OnlineMismatchDiff::new(
                envelope.frame,
                server_hash,
                result.state_hash,
                &server_world,
                &self.world,
                &inputs,
                &envelope.events,
                &result.events,
                observation.game_state.is_some(),
            );
            return Err(OnlineError::Mismatch { diff });
        }

        Ok(())
    }
}

pub fn parse_game_state_json(input: &str) -> Result<LockstepSimDemoState, OnlineError> {
    serde_json::from_str::<LockstepSimDemoState>(input).map_err(OnlineError::Json)
}

pub fn observation_from_game_state_and_inputs(
    game_state_json: &str,
    inputs: Vec<FrameInputRecord>,
) -> Result<Option<ServerFrameObservation>, OnlineError> {
    if game_state_json.trim().is_empty() {
        return Ok(None);
    }

    let game_state = parse_game_state_json(game_state_json)?;
    let Some(envelope) = game_state.observed_frame().cloned() else {
        return Ok(None);
    };

    Ok(Some(ServerFrameObservation {
        envelope,
        inputs,
        game_state: Some(game_state),
    }))
}

fn validate_initial_snapshot(snapshot: &SimInitialSnapshot) -> Result<(), OnlineError> {
    if snapshot.schema != SIM_INITIAL_SNAPSHOT_SCHEMA
        || snapshot.schema_version != SIM_DOWNLINK_SCHEMA_VERSION
    {
        return Err(OnlineError::InvalidInitialSnapshot(
            "UNSUPPORTED_SIM_SNAPSHOT_SCHEMA".to_owned(),
        ));
    }
    if snapshot.room_id.trim().is_empty() {
        return Err(OnlineError::InvalidInitialSnapshot(
            "INVALID_SIM_SNAPSHOT_ROOM_ID".to_owned(),
        ));
    }
    if snapshot.tick_rate == 0 {
        return Err(OnlineError::InvalidInitialSnapshot(
            "INVALID_SIM_SNAPSHOT_TICK_RATE".to_owned(),
        ));
    }
    if snapshot.config_version == 0 {
        return Err(OnlineError::InvalidInitialSnapshot(
            "UNSUPPORTED_SIM_CONFIG_VERSION expected >= 1, got 0".to_owned(),
        ));
    }
    if snapshot.config_hash.trim().is_empty() {
        return Err(OnlineError::InvalidInitialSnapshot(
            "INVALID_SIM_CONFIG_HASH".to_owned(),
        ));
    }
    if snapshot.sim_schema_version != sim_core::SIM_CORE_SCHEMA_VERSION {
        return Err(OnlineError::InvalidInitialSnapshot(format!(
            "UNSUPPORTED_SIM_SCHEMA_VERSION expected {}, got {}",
            sim_core::SIM_CORE_SCHEMA_VERSION,
            snapshot.sim_schema_version
        )));
    }
    if snapshot.start_frame != snapshot.snapshot.frame.raw() {
        return Err(OnlineError::InvalidInitialSnapshot(
            "SIM_SNAPSHOT_FRAME_MISMATCH".to_owned(),
        ));
    }
    if snapshot.rng_seed != snapshot.snapshot.world.rng.seed {
        return Err(OnlineError::InvalidInitialSnapshot(
            "SIM_SNAPSHOT_RNG_SEED_MISMATCH".to_owned(),
        ));
    }
    if snapshot.state_hash.to_sim_hash() != snapshot.snapshot.hash {
        return Err(OnlineError::InvalidInitialSnapshot(
            "SIM_SNAPSHOT_HASH_ENVELOPE_MISMATCH".to_owned(),
        ));
    }

    let mut entities = snapshot.entities.clone();
    entities.sort_by_key(|entity| entity.id);
    if !entities.is_empty() && entities != snapshot.snapshot.world.entities_sorted_by_id() {
        return Err(OnlineError::InvalidInitialSnapshot(
            "SIM_SNAPSHOT_ENTITIES_MISMATCH".to_owned(),
        ));
    }

    Ok(())
}

fn restore_control_bindings(
    bindings: &[SimControlBinding],
    world: &SimWorld,
) -> Result<HashMap<String, EntityId>, OnlineError> {
    let entities = world
        .entities_sorted_by_id()
        .iter()
        .map(|entity| (entity.id, entity))
        .collect::<HashMap<_, _>>();
    let mut restored = HashMap::new();

    for binding in bindings {
        if binding.character_id.trim().is_empty() {
            return Err(OnlineError::InvalidInitialSnapshot(
                "INVALID_SIM_SNAPSHOT_CONTROL_BINDING".to_owned(),
            ));
        }
        let entity_id = EntityId::new(binding.entity_id);
        let Some(entity) = entities.get(&entity_id) else {
            return Err(OnlineError::InvalidInitialSnapshot(
                "INVALID_SIM_SNAPSHOT_CONTROL_BINDING".to_owned(),
            ));
        };
        if entity.owner_character_id.as_deref() != Some(binding.character_id.as_str()) {
            return Err(OnlineError::InvalidInitialSnapshot(
                "INVALID_SIM_SNAPSHOT_CONTROL_BINDING".to_owned(),
            ));
        }
        if restored
            .insert(binding.character_id.clone(), entity_id)
            .is_some()
        {
            return Err(OnlineError::InvalidInitialSnapshot(
                "INVALID_SIM_SNAPSHOT_CONTROL_BINDING".to_owned(),
            ));
        }
    }

    Ok(restored)
}

fn validate_frame_envelope(
    envelope: &SimFrameEnvelope,
    room_id: &str,
    expected: &SimContractMetadata,
) -> Result<(), OnlineError> {
    if envelope.schema != SIM_FRAME_ENVELOPE_SCHEMA
        || envelope.schema_version != SIM_DOWNLINK_SCHEMA_VERSION
    {
        return Err(OnlineError::InvalidFrameEnvelope(
            "UNSUPPORTED_SIM_FRAME_ENVELOPE_SCHEMA".to_owned(),
        ));
    }
    if envelope.room_id != room_id {
        return Err(OnlineError::InvalidFrameEnvelope(format!(
            "SIM_FRAME_ROOM_MISMATCH expected {room_id}, got {}",
            envelope.room_id
        )));
    }
    if envelope.tick_rate == 0 {
        return Err(OnlineError::InvalidFrameEnvelope(
            "INVALID_SIM_FRAME_TICK_RATE".to_owned(),
        ));
    }
    if envelope.config_version != expected.config_version {
        return Err(OnlineError::InvalidFrameEnvelope(format!(
            "SIM_FRAME_CONFIG_VERSION_MISMATCH expected {}, got {}",
            expected.config_version, envelope.config_version
        )));
    }
    if envelope.config_hash != expected.config_hash {
        return Err(OnlineError::InvalidFrameEnvelope(format!(
            "SIM_FRAME_CONFIG_HASH_MISMATCH expected {}, got {}",
            expected.config_hash, envelope.config_hash
        )));
    }
    if envelope.sim_schema_version != expected.sim_schema_version {
        return Err(OnlineError::InvalidFrameEnvelope(format!(
            "SIM_FRAME_SIM_SCHEMA_VERSION_MISMATCH expected {}, got {}",
            expected.sim_schema_version, envelope.sim_schema_version
        )));
    }
    if envelope.state_hash.frame != envelope.frame {
        return Err(OnlineError::InvalidFrameEnvelope(
            "SIM_FRAME_HASH_FRAME_MISMATCH".to_owned(),
        ));
    }
    if envelope.event_count != envelope.events.len() {
        return Err(OnlineError::InvalidFrameEnvelope(format!(
            "SIM_FRAME_EVENT_COUNT_MISMATCH declared {}, actual {}",
            envelope.event_count,
            envelope.events.len()
        )));
    }
    Ok(())
}

fn sim_inputs_from_frame_records(
    records: &[FrameInputRecord],
    input_sources: &[SimFrameInputSourceSummary],
    bindings: &HashMap<String, EntityId>,
) -> Result<Vec<SimInput>, OnlineError> {
    let source_by_key = input_sources
        .iter()
        .map(|source| {
            (
                (
                    source.frame,
                    source.character_id.as_str(),
                    source.action.as_str(),
                ),
                source.source,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut sim_inputs = Vec::new();

    for record in records {
        let Some(entity_id) = bindings.get(&record.character_id).copied() else {
            continue;
        };
        let source = source_by_key
            .get(&(
                record.frame_id,
                record.character_id.as_str(),
                record.action.as_str(),
            ))
            .copied()
            .unwrap_or(SimFrameInputSource::Real)
            .into();

        if record.action.is_empty() || record.action != SIM_INPUT_ACTION {
            sim_inputs.push(SimInput {
                frame: FrameId::new(record.frame_id),
                character_id: record.character_id.clone(),
                entity_id,
                seq: 0,
                source,
                command: SimCommand::Noop,
            });
            continue;
        }

        let payload = parse_wire_sim_input_payload(&record.payload_json)?;
        sim_inputs.extend(payload.commands.into_iter().map(|command| SimInput {
            frame: FrameId::new(record.frame_id),
            character_id: record.character_id.clone(),
            entity_id,
            seq: payload.seq,
            source,
            command: command.to_sim_command(),
        }));
    }

    Ok(sim_inputs)
}

fn parse_wire_sim_input_payload(input: &str) -> Result<ParsedWireSimInputPayload, OnlineError> {
    let payload = serde_json::from_str::<WireSimInputPayload>(input).map_err(OnlineError::Json)?;
    if payload.version != SIM_INPUT_VERSION {
        return Err(OnlineError::InvalidSimInput(
            "UNSUPPORTED_SIM_INPUT_VERSION",
        ));
    }
    let commands = payload
        .commands
        .into_iter()
        .map(WireSimCommand::validate)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ParsedWireSimInputPayload {
        seq: payload.seq,
        commands,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ParsedWireSimInputPayload {
    seq: u32,
    commands: Vec<ParsedWireSimCommand>,
}

pub fn lockstep_demo_config(tick_rate: u16) -> SimConfig {
    SimConfig {
        movement: MovementConfig {
            tick_rate: tick_rate.max(1),
            default_speed_per_second: Fp::from_i32(6),
            max_speed_per_second: Fp::from_i32(12),
            bounds: SceneBounds {
                min: Vec2Fp::new(Fp::from_i32(-100), Fp::from_i32(-100)),
                max: Vec2Fp::new(Fp::from_i32(100), Fp::from_i32(100)),
            },
            static_obstacles: Vec::<StaticObstacle>::new(),
        },
        combat: CombatConfig::from_definitions(
            vec![SkillDefinition {
                id: SkillId::new(DEFAULT_PLAYER_SKILL_ID),
                cooldown_frames: tick_rate.max(1) as u32,
                cast_range: Fp::from_i32(12),
                target_type: SkillTargetType::Enemy,
                effects: vec![CombatEffect::Damage {
                    formula: sim_core::DamageFormula::Fixed { amount: 15 },
                }],
            }],
            Vec::new(),
        )
        .expect("lockstep_sim_demo default combat config should be valid"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnlineMismatchDiff {
    pub frame: u32,
    pub server_hash: SimHash,
    pub client_hash: SimHash,
    pub entity_diff: MismatchDiff,
    pub event_diff: EventDiff,
    pub server_debug_available: bool,
}

impl OnlineMismatchDiff {
    fn new(
        frame: u32,
        server_hash: SimHash,
        client_hash: SimHash,
        server_world: &SimWorld,
        client_world: &SimWorld,
        frame_inputs: &[SimInput],
        server_events: &[SimEvent],
        client_events: &[SimEvent],
        server_debug_available: bool,
    ) -> Self {
        Self {
            frame,
            server_hash,
            client_hash,
            entity_diff: MismatchDiff::new(
                frame,
                server_hash,
                client_hash,
                server_world,
                client_world,
                frame_inputs,
            ),
            event_diff: EventDiff {
                server_events: server_events.to_vec(),
                client_events: client_events.to_vec(),
            },
            server_debug_available,
        }
    }
}

impl fmt::Display for OnlineMismatchDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "online mismatch: first mismatch frame {}", self.frame)?;
        writeln!(f, "server_hash: {:016x}", self.server_hash.value)?;
        writeln!(f, "client_hash: {:016x}", self.client_hash.value)?;
        writeln!(
            f,
            "entity count: server={} client={} diff={:+}",
            self.entity_diff.server_entity_count,
            self.entity_diff.client_entity_count,
            self.entity_diff.entity_count_delta()
        )?;

        if !self.server_debug_available {
            writeln!(
                f,
                "entity diffs: server debug state unavailable in frame snapshot"
            )?;
        } else if self.entity_diff.entity_diffs.is_empty() {
            writeln!(f, "entity diffs: none in tracked fields")?;
        } else {
            writeln!(f, "entity diffs:")?;
            for diff in &self.entity_diff.entity_diffs {
                writeln!(f, "  - entity {}:", diff.entity_id)?;
                if diff.server_pos != diff.client_pos {
                    writeln!(
                        f,
                        "    pos: server_pos={} client_pos={}",
                        format_optional(diff.server_pos),
                        format_optional(diff.client_pos)
                    )?;
                }
                if diff.server_hp != diff.client_hp {
                    writeln!(
                        f,
                        "    hp: server_hp={} client_hp={}",
                        format_optional(diff.server_hp),
                        format_optional(diff.client_hp)
                    )?;
                }
                if diff.server_alive != diff.client_alive {
                    writeln!(
                        f,
                        "    alive: server_alive={} client_alive={}",
                        format_optional(diff.server_alive),
                        format_optional(diff.client_alive)
                    )?;
                }
                if diff.server_movement != diff.client_movement {
                    writeln!(
                        f,
                        "    movement: server_movement={} client_movement={}",
                        format_optional(diff.server_movement),
                        format_optional(diff.client_movement)
                    )?;
                }
            }
        }

        if self.event_diff.server_events == self.event_diff.client_events {
            writeln!(f, "event diffs: none")?;
        } else {
            writeln!(f, "event diffs:")?;
            writeln!(f, "  server_events: {:?}", self.event_diff.server_events)?;
            writeln!(f, "  client_events: {:?}", self.event_diff.client_events)?;
        }

        if self.entity_diff.inputs.is_empty() {
            write!(f, "inputs: none")?;
        } else {
            writeln!(f, "inputs:")?;
            for input in &self.entity_diff.inputs {
                writeln!(f, "  - {input}")?;
            }
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventDiff {
    pub server_events: Vec<SimEvent>,
    pub client_events: Vec<SimEvent>,
}

fn format_optional<T: fmt::Display>(value: Option<T>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "<missing>".to_owned())
}

#[derive(Debug)]
pub enum OnlineError {
    InvalidArgs {
        message: String,
    },
    ReadScenario {
        path: PathBuf,
        source: io::Error,
    },
    Scenario(ScenarioError),
    Json(serde_json::Error),
    Io(io::Error),
    Protocol(String),
    ServerRejected {
        stage: &'static str,
        error_code: String,
    },
    InvalidInitialSnapshot(String),
    InvalidFrameEnvelope(String),
    InvalidSimInput(&'static str),
    UnsupportedOnlineCommand(&'static str),
    Step {
        frame: u32,
        source: StepError,
    },
    Mismatch {
        diff: OnlineMismatchDiff,
    },
}

impl OnlineError {
    fn invalid_args(message: impl Into<String>) -> Self {
        Self::InvalidArgs {
            message: message.into(),
        }
    }
}

impl From<crate::offline::OfflineError> for OnlineError {
    fn from(error: crate::offline::OfflineError) -> Self {
        Self::Protocol(error.to_string())
    }
}

impl From<io::Error> for OnlineError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl fmt::Display for OnlineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgs { message } => write!(f, "{message}\n{USAGE}"),
            Self::ReadScenario { path, source } => {
                write!(f, "failed to read scenario `{}`: {source}", path.display())
            }
            Self::Scenario(error) => write!(f, "{error}"),
            Self::Json(error) => write!(f, "JSON error: {error}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Protocol(message) => write!(f, "protocol error: {message}"),
            Self::ServerRejected { stage, error_code } => {
                write!(f, "{stage} rejected by server: {error_code}")
            }
            Self::InvalidInitialSnapshot(message) => {
                write!(f, "invalid lockstep initial snapshot: {message}")
            }
            Self::InvalidFrameEnvelope(message) => {
                write!(f, "invalid lockstep frame envelope: {message}")
            }
            Self::InvalidSimInput(code) => write!(f, "invalid sim_input payload: {code}"),
            Self::UnsupportedOnlineCommand(message) => write!(f, "{message}"),
            Self::Step { frame, source } => {
                write!(f, "client replay failed at frame {frame}: {source}")
            }
            Self::Mismatch { diff } => write!(f, "{diff}"),
        }
    }
}

impl std::error::Error for OnlineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ReadScenario { source, .. } => Some(source),
            Self::Scenario(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Step { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
enum MessageType {
    AuthReq = 1001,
    AuthRes = 1002,
    RoomJoinReq = 1101,
    RoomJoinRes = 1102,
    RoomReadyReq = 1105,
    RoomReadyRes = 1106,
    RoomStartReq = 1107,
    RoomStartRes = 1108,
    PlayerInputReq = 1111,
    PlayerInputRes = 1112,
    RoomJoinAsObserverReq = 1117,
    RoomJoinAsObserverRes = 1118,
    RoomStatePush = 1201,
    FrameBundlePush = 1203,
    ErrorRes = 9000,
}

impl MessageType {
    fn from_u16(value: u16) -> Option<Self> {
        match value {
            1001 => Some(Self::AuthReq),
            1002 => Some(Self::AuthRes),
            1101 => Some(Self::RoomJoinReq),
            1102 => Some(Self::RoomJoinRes),
            1105 => Some(Self::RoomReadyReq),
            1106 => Some(Self::RoomReadyRes),
            1107 => Some(Self::RoomStartReq),
            1108 => Some(Self::RoomStartRes),
            1111 => Some(Self::PlayerInputReq),
            1112 => Some(Self::PlayerInputRes),
            1117 => Some(Self::RoomJoinAsObserverReq),
            1118 => Some(Self::RoomJoinAsObserverRes),
            1201 => Some(Self::RoomStatePush),
            1203 => Some(Self::FrameBundlePush),
            9000 => Some(Self::ErrorRes),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct Packet {
    msg_type: u16,
    seq: u32,
    body: Vec<u8>,
}

struct TcpGameTransport {
    stream: TcpStream,
    next_seq: u32,
}

impl TcpGameTransport {
    fn connect(addr: &str, timeout: Duration) -> Result<Self, OnlineError> {
        let socket_addr = addr
            .to_socket_addrs()
            .map_err(OnlineError::Io)?
            .next()
            .ok_or_else(|| {
                OnlineError::Protocol(format!(
                    "server address `{addr}` resolved to no socket address"
                ))
            })?;
        let stream = TcpStream::connect_timeout(&socket_addr, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;

        Ok(Self {
            stream,
            next_seq: 1,
        })
    }

    fn send<M: prost::Message>(
        &mut self,
        msg_type: MessageType,
        message: &M,
    ) -> Result<u32, OnlineError> {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        let body = message.encode_to_vec();
        let packet = encode_packet(msg_type, seq, &body);
        self.stream.write_all(&packet)?;
        Ok(seq)
    }

    fn read_packet(&mut self) -> Result<Packet, OnlineError> {
        read_packet(&mut self.stream)
    }

    fn read_until(
        &mut self,
        expected: MessageType,
        expected_seq: Option<u32>,
        replay: Option<&mut Option<OnlineReplay>>,
    ) -> Result<Packet, OnlineError> {
        let mut replay = replay;
        loop {
            let packet = self.read_packet()?;
            if MessageType::from_u16(packet.msg_type) == Some(expected)
                && expected_seq.is_none_or(|seq| seq == packet.seq)
            {
                return Ok(packet);
            }

            if let Some(replay_slot) = replay.as_deref_mut() {
                maybe_consume_push_packet(&packet, replay_slot)?;
            }
        }
    }
}

fn drive_online_session(
    transport: &mut TcpGameTransport,
    options: &OnlineCliOptions,
    ticket: &str,
    input_plan: &[PlayerInputPlan],
) -> Result<OnlineSessionOutcome, OnlineError> {
    let auth_seq = transport.send(
        MessageType::AuthReq,
        &pb::AuthReq {
            ticket: ticket.to_owned(),
        },
    )?;
    let auth_packet = transport.read_until(MessageType::AuthRes, Some(auth_seq), None)?;
    let auth = decode_body::<pb::AuthRes>(&auth_packet)?;
    if !auth.ok {
        return Err(OnlineError::ServerRejected {
            stage: "auth",
            error_code: auth.error_code,
        });
    }

    let join_seq = transport.send(
        MessageType::RoomJoinReq,
        &pb::RoomJoinReq {
            room_id: options.room_id.clone(),
            policy_id: options.policy_id.clone(),
        },
    )?;
    let join_packet = transport.read_until(MessageType::RoomJoinRes, Some(join_seq), None)?;
    let join = decode_body::<pb::RoomJoinRes>(&join_packet)?;
    if !join.ok {
        return Err(OnlineError::ServerRejected {
            stage: "room_join",
            error_code: join.error_code,
        });
    }

    let mut replay = None;
    drain_until_initial_snapshot(transport, &mut replay)?;

    let ready_seq = transport.send(MessageType::RoomReadyReq, &pb::RoomReadyReq { ready: true })?;
    let ready_packet = transport.read_until(
        MessageType::RoomReadyRes,
        Some(ready_seq),
        Some(&mut replay),
    )?;
    let ready = decode_body::<pb::RoomReadyRes>(&ready_packet)?;
    if !ready.ok {
        return Err(OnlineError::ServerRejected {
            stage: "room_ready",
            error_code: ready.error_code,
        });
    }

    let start_seq = transport.send(MessageType::RoomStartReq, &pb::RoomStartReq {})?;
    let start_packet = transport.read_until(
        MessageType::RoomStartRes,
        Some(start_seq),
        Some(&mut replay),
    )?;
    let start = decode_body::<pb::RoomStartRes>(&start_packet)?;
    if !start.ok {
        return Err(OnlineError::ServerRejected {
            stage: "room_start",
            error_code: start.error_code,
        });
    }

    drain_until_initial_snapshot(transport, &mut replay)?;

    if replay.is_none() {
        return Err(OnlineError::Protocol(
            "room did not publish lockstep initialSnapshot in RoomSnapshot.game_state".to_owned(),
        ));
    }

    for input in input_plan {
        wait_until_input_frame_is_sendable(
            transport,
            &mut replay,
            input.frame_id,
            DEFAULT_ONLINE_INPUT_DELAY_FRAMES,
        )?;
        let seq = transport.send(
            MessageType::PlayerInputReq,
            &pb::PlayerInputReq {
                frame_id: input.frame_id,
                action: input.action.clone(),
                payload_json: input.payload_json.clone(),
                client_timestamp_ms: now_ms(),
            },
        )?;
        let packet =
            transport.read_until(MessageType::PlayerInputRes, Some(seq), Some(&mut replay))?;
        let response = decode_body::<pb::PlayerInputRes>(&packet)?;
        if !response.ok {
            return Err(OnlineError::ServerRejected {
                stage: "player_input",
                error_code: response.error_code,
            });
        }
    }

    let target_frame = input_plan
        .iter()
        .map(|input| input.frame_id)
        .max()
        .unwrap_or(0);
    while replay
        .as_ref()
        .is_none_or(|replay| replay.current_frame() < target_frame)
    {
        let packet = transport.read_packet()?;
        maybe_consume_push_packet(&packet, &mut replay)?;
    }

    let Some(replay) = replay else {
        return Err(OnlineError::Protocol(
            "room did not publish lockstep initialSnapshot in RoomSnapshot.game_state".to_owned(),
        ));
    };

    Ok(OnlineSessionOutcome {
        frames_checked: replay.frames_checked(),
        final_hash: Some(replay.final_hash()),
    })
}

fn run_observer_recovery_probe(
    options: &OnlineCliOptions,
    observer_ticket: &str,
) -> Result<ObserverRecoveryProbeReport, OnlineError> {
    let mut transport = TcpGameTransport::connect(&options.server_addr, options.timeout())?;
    let auth_seq = transport.send(
        MessageType::AuthReq,
        &pb::AuthReq {
            ticket: observer_ticket.to_owned(),
        },
    )?;
    let auth_packet = transport.read_until(MessageType::AuthRes, Some(auth_seq), None)?;
    let auth = decode_body::<pb::AuthRes>(&auth_packet)?;
    if !auth.ok {
        return Err(OnlineError::ServerRejected {
            stage: "observer_auth",
            error_code: auth.error_code,
        });
    }

    let observer_seq = transport.send(
        MessageType::RoomJoinAsObserverReq,
        &pb::RoomJoinAsObserverReq {
            room_id: options.room_id.clone(),
        },
    )?;
    let observer_packet =
        transport.read_until(MessageType::RoomJoinAsObserverRes, Some(observer_seq), None)?;
    let response = decode_body::<pb::RoomJoinAsObserverRes>(&observer_packet)?;
    if !response.ok {
        return Err(OnlineError::ServerRejected {
            stage: "observer_recovery",
            error_code: response.error_code,
        });
    }

    let Some(snapshot) = response.snapshot else {
        return Err(OnlineError::Protocol(
            "observer recovery response did not include RoomSnapshot".to_owned(),
        ));
    };

    validate_observer_recovery_snapshot(snapshot, response.current_frame_id)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct OnlineSessionOutcome {
    frames_checked: usize,
    final_hash: Option<SimHash>,
}

fn wait_until_input_frame_is_sendable(
    transport: &mut TcpGameTransport,
    replay: &mut Option<OnlineReplay>,
    frame_id: u32,
    input_delay_frames: u32,
) -> Result<(), OnlineError> {
    let input_delay_frames = input_delay_frames.max(1);
    while replay
        .as_ref()
        .map(|replay| replay.current_frame().saturating_add(input_delay_frames) < frame_id)
        .unwrap_or(false)
    {
        let packet = transport.read_packet()?;
        maybe_consume_push_packet(&packet, replay)?;
    }

    Ok(())
}

fn drain_until_initial_snapshot(
    transport: &mut TcpGameTransport,
    replay: &mut Option<OnlineReplay>,
) -> Result<(), OnlineError> {
    if replay.is_some() {
        return Ok(());
    }

    let packet = transport.read_packet()?;
    maybe_consume_push_packet(&packet, replay)?;
    Ok(())
}

fn maybe_consume_push_packet(
    packet: &Packet,
    replay: &mut Option<OnlineReplay>,
) -> Result<(), OnlineError> {
    match MessageType::from_u16(packet.msg_type) {
        Some(MessageType::RoomStatePush) => {
            let push = decode_body::<pb::RoomStatePush>(packet)?;
            if let Some(snapshot) = push.snapshot {
                consume_room_snapshot(snapshot, Vec::new(), replay)?;
            }
        }
        Some(MessageType::FrameBundlePush) => {
            let push = decode_body::<pb::FrameBundlePush>(packet)?;
            if let Some(snapshot) = push.snapshot {
                let inputs = push
                    .inputs
                    .into_iter()
                    .map(|input| FrameInputRecord {
                        character_id: input.character_id,
                        action: input.action,
                        payload_json: input.payload_json,
                        frame_id: input.frame_id,
                    })
                    .collect();
                consume_room_snapshot(snapshot, inputs, replay)?;
            }
        }
        Some(MessageType::ErrorRes) => {
            let error = decode_body::<pb::ErrorRes>(packet)?;
            return Err(OnlineError::Protocol(format!(
                "server error response: {} {}",
                error.error_code, error.message
            )));
        }
        _ => {}
    }

    Ok(())
}

fn consume_room_snapshot(
    snapshot: pb::RoomSnapshot,
    inputs: Vec<FrameInputRecord>,
    replay: &mut Option<OnlineReplay>,
) -> Result<(), OnlineError> {
    if snapshot.game_state.trim().is_empty() {
        return Ok(());
    }

    let game_state = parse_game_state_json(&snapshot.game_state)?;
    if replay.is_none() {
        if let Some(initial) = &game_state.initial_snapshot {
            *replay = Some(OnlineReplay::from_initial_snapshot(initial)?);
        }
    }

    let Some(replay) = replay else {
        return Ok(());
    };
    let Some(envelope) = game_state.observed_frame().cloned() else {
        return Ok(());
    };

    replay.apply_server_frame(&ServerFrameObservation {
        envelope,
        inputs,
        game_state: Some(game_state),
    })
}

fn validate_observer_recovery_snapshot(
    snapshot: pb::RoomSnapshot,
    current_frame_id: u32,
) -> Result<ObserverRecoveryProbeReport, OnlineError> {
    if snapshot.game_state.trim().is_empty() {
        return Err(OnlineError::Protocol(
            "observer recovery RoomSnapshot.game_state is empty".to_owned(),
        ));
    }

    let game_state = parse_game_state_json(&snapshot.game_state)?;
    let initial = game_state.initial_snapshot.as_ref().ok_or_else(|| {
        OnlineError::Protocol(
            "observer recovery RoomSnapshot.game_state missing initialSnapshot".to_owned(),
        )
    })?;
    validate_initial_snapshot(initial)?;
    let contract = SimContractMetadata::from(initial);

    let last_frame = game_state.last_frame.as_ref().ok_or_else(|| {
        OnlineError::Protocol(
            "observer recovery RoomSnapshot.game_state missing lastFrame".to_owned(),
        )
    })?;
    validate_frame_envelope(last_frame, &snapshot.room_id, &contract)?;

    let observer_frame = game_state.observer_frame.as_ref().ok_or_else(|| {
        OnlineError::Protocol(
            "observer recovery RoomSnapshot.game_state missing observerFrame".to_owned(),
        )
    })?;
    let observer_last_frame = observer_frame.last_frame.as_ref().ok_or_else(|| {
        OnlineError::Protocol(
            "observer recovery RoomSnapshot.game_state missing observerFrame.lastFrame".to_owned(),
        )
    })?;
    validate_frame_envelope(observer_last_frame, &snapshot.room_id, &contract)?;

    if observer_frame.world_frame != snapshot.current_frame_id {
        return Err(OnlineError::Protocol(format!(
            "observer recovery worldFrame {} did not match RoomSnapshot.current_frame_id {}",
            observer_frame.world_frame, snapshot.current_frame_id
        )));
    }
    if current_frame_id != snapshot.current_frame_id {
        return Err(OnlineError::Protocol(format!(
            "observer recovery response current_frame_id {} did not match snapshot current_frame_id {}",
            current_frame_id, snapshot.current_frame_id
        )));
    }
    if observer_last_frame.frame != last_frame.frame {
        return Err(OnlineError::Protocol(format!(
            "observer recovery observerFrame.lastFrame {} did not match lastFrame {}",
            observer_last_frame.frame, last_frame.frame
        )));
    }
    if observer_frame.state_hash.to_sim_hash() != observer_last_frame.state_hash.to_sim_hash() {
        return Err(OnlineError::Protocol(
            "observer recovery observerFrame.stateHash did not match observerFrame.lastFrame.stateHash"
                .to_owned(),
        ));
    }

    Ok(ObserverRecoveryProbeReport {
        room_id: snapshot.room_id,
        current_frame_id,
        snapshot_frame_id: snapshot.current_frame_id,
        initial_snapshot_frame: initial.snapshot.frame.raw(),
        last_frame: last_frame.frame,
        observer_last_frame: observer_last_frame.frame,
        observer_hash: observer_frame.state_hash.to_sim_hash(),
    })
}

fn decode_body<M: prost::Message + Default>(packet: &Packet) -> Result<M, OnlineError> {
    M::decode(packet.body.as_slice()).map_err(|error| {
        OnlineError::Protocol(format!(
            "failed to decode message type {} body: {error}",
            packet.msg_type
        ))
    })
}

const MAGIC: u16 = 0xCAFE;
const PROTOCOL_VERSION: u8 = 1;
const HEADER_LEN: usize = 14;
const MAX_BODY_LEN: u32 = 1024 * 1024;

fn encode_packet(msg_type: MessageType, seq: u32, body: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(HEADER_LEN + body.len());
    packet.extend_from_slice(&MAGIC.to_be_bytes());
    packet.push(PROTOCOL_VERSION);
    packet.push(0);
    packet.extend_from_slice(&(msg_type as u16).to_be_bytes());
    packet.extend_from_slice(&seq.to_be_bytes());
    packet.extend_from_slice(&(body.len() as u32).to_be_bytes());
    packet.extend_from_slice(body);
    packet
}

fn read_packet(stream: &mut impl Read) -> Result<Packet, OnlineError> {
    let mut header = [0_u8; HEADER_LEN];
    stream.read_exact(&mut header)?;

    let magic = u16::from_be_bytes([header[0], header[1]]);
    if magic != MAGIC {
        return Err(OnlineError::Protocol(format!(
            "invalid packet magic: {magic:#06x}"
        )));
    }
    if header[2] != PROTOCOL_VERSION {
        return Err(OnlineError::Protocol(format!(
            "invalid packet version: {}",
            header[2]
        )));
    }
    if header[3] != 0 {
        return Err(OnlineError::Protocol(format!(
            "unsupported packet flags: {}",
            header[3]
        )));
    }

    let msg_type = u16::from_be_bytes([header[4], header[5]]);
    let seq = u32::from_be_bytes([header[6], header[7], header[8], header[9]]);
    let body_len = u32::from_be_bytes([header[10], header[11], header[12], header[13]]);
    if body_len > MAX_BODY_LEN {
        return Err(OnlineError::Protocol(format!(
            "packet body too large: {body_len}"
        )));
    }

    let mut body = vec![0_u8; body_len as usize];
    stream.read_exact(&mut body)?;
    Ok(Packet {
        msg_type,
        seq,
        body,
    })
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;
    use sim_core::{
        CombatState, EntityKind, MovementState, SimRngState, SimTransform, TeamId, snapshot,
    };

    const DEFAULT_LOCKSTEP_SIM_TICK_RATE: u16 = 20;
    const TRAINING_TARGET_ENTITY_ID: u32 = 9000;

    fn player_entity(character_id: &str) -> SimEntity {
        SimEntity {
            id: EntityId::new(1000),
            kind: EntityKind::Player,
            owner_character_id: Some(character_id.to_owned()),
            team_id: TeamId::new(1),
            transform: SimTransform {
                pos: Vec2Fp::zero(),
                facing: QuantizedDir::RIGHT,
                radius: Fp::from_milli(500),
            },
            movement: MovementState::default(),
            combat: CombatState {
                hp: 100,
                max_hp: 100,
                attack: 10,
                defense: 3,
                speed: 6,
                crit_rate_bps: 500,
                crit_damage_bps: 15_000,
                skill_slots: vec![sim_core::SkillSlot {
                    skill_id: SkillId::new(DEFAULT_PLAYER_SKILL_ID),
                    cooldown_remaining: 0,
                }],
                buffs: Vec::new(),
            },
            alive: true,
        }
    }

    fn target_entity() -> SimEntity {
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

    fn initial_snapshot() -> SimInitialSnapshot {
        let config = lockstep_demo_config(DEFAULT_LOCKSTEP_SIM_TICK_RATE);
        let world = SimWorld::with_rng(
            FrameId::new(0),
            SimRngState::default(),
            vec![player_entity("player-a"), target_entity()],
        )
        .unwrap();
        let snapshot = snapshot(&world, &config);

        SimInitialSnapshot {
            schema: SIM_INITIAL_SNAPSHOT_SCHEMA.to_owned(),
            schema_version: SIM_DOWNLINK_SCHEMA_VERSION,
            room_id: "room-lockstep".to_owned(),
            start_frame: 0,
            tick_rate: DEFAULT_LOCKSTEP_SIM_TICK_RATE,
            config_version: 1,
            config_hash: "test".to_owned(),
            sim_schema_version: sim_core::SIM_CORE_SCHEMA_VERSION,
            rng_seed: 0,
            state_hash: sim_hash_envelope(snapshot.hash),
            snapshot,
            entities: world.entities_sorted_by_id().to_vec(),
            control_bindings: vec![SimControlBinding {
                character_id: "player-a".to_owned(),
                entity_id: 1000,
            }],
        }
    }

    fn sim_hash_envelope(hash: SimHash) -> SimHashEnvelope {
        SimHashEnvelope {
            frame: hash.frame.raw(),
            value: hash.value,
            hex: format!("{:016x}", hash.value),
        }
    }

    fn debug_state(world: &SimWorld, last_frame: Option<SimFrameEnvelope>) -> LockstepSimDemoState {
        let player = world.entity(EntityId::new(1000)).unwrap();
        let target = world
            .entity(EntityId::new(TRAINING_TARGET_ENTITY_ID))
            .unwrap();

        LockstepSimDemoState {
            logic_type: Some(LOCKSTEP_SIM_DEMO_POLICY_ID.to_owned()),
            room_id: Some("room-lockstep".to_owned()),
            world_frame: world.frame.raw(),
            tick_rate: DEFAULT_LOCKSTEP_SIM_TICK_RATE,
            training_target_entity_id: TRAINING_TARGET_ENTITY_ID,
            player_entities: vec![LockstepSimPlayerDebugState {
                character_id: "player-a".to_owned(),
                entity: LockstepSimEntityDebugState {
                    entity_id: player.id.raw(),
                    x: player.transform.pos.x.raw(),
                    y: player.transform.pos.y.raw(),
                    hp: player.combat.hp,
                    max_hp: player.combat.max_hp,
                    alive: player.alive,
                },
            }],
            training_target: Some(LockstepSimEntityDebugState {
                entity_id: target.id.raw(),
                x: target.transform.pos.x.raw(),
                y: target.transform.pos.y.raw(),
                hp: target.combat.hp,
                max_hp: target.combat.max_hp,
                alive: target.alive,
            }),
            initial_snapshot: None,
            last_frame,
            observer_frame: None,
            last_error: None,
        }
    }

    fn frame_envelope(result: &sim_core::SimStepResult) -> SimFrameEnvelope {
        SimFrameEnvelope {
            schema: SIM_FRAME_ENVELOPE_SCHEMA.to_owned(),
            schema_version: SIM_DOWNLINK_SCHEMA_VERSION,
            room_id: "room-lockstep".to_owned(),
            frame: result.frame.raw(),
            tick_rate: DEFAULT_LOCKSTEP_SIM_TICK_RATE,
            config_version: 1,
            config_hash: "test".to_owned(),
            sim_schema_version: sim_core::SIM_CORE_SCHEMA_VERSION,
            state_hash: sim_hash_envelope(result.state_hash),
            event_count: result.events.len(),
            events: result.events.clone(),
            event_summaries: Vec::new(),
            input_sources: vec![SimFrameInputSourceSummary {
                frame: result.frame.raw(),
                character_id: "player-a".to_owned(),
                source: SimFrameInputSource::Real,
                action: SIM_INPUT_ACTION.to_owned(),
            }],
            debug_summary: SimFrameDebugSummary {
                input_count: 1,
                real_input_count: 1,
                synthetic_input_count: 0,
                synthesized_empty_input_count: 0,
                synthesized_repeat_last_input_count: 0,
                event_count: result.events.len(),
                entity_count: 2,
                alive_entity_count: 2,
                player_entity_count: 1,
            },
            debug_state: SimFrameDebugState {
                schema_version: 1,
                entities: Vec::new(),
            },
        }
    }

    fn move_right_payload(seq: u32) -> String {
        build_sim_input_payload(
            seq,
            &[SimCommand::Move(MoveCommand {
                dir: QuantizedDir::RIGHT,
                speed_per_second: None,
            })],
        )
        .unwrap()
    }

    fn room_snapshot_from_game_state(
        room_id: &str,
        current_frame_id: u32,
        game_state: &LockstepSimDemoState,
    ) -> pb::RoomSnapshot {
        pb::RoomSnapshot {
            room_id: room_id.to_owned(),
            owner_character_id: "player-a".to_owned(),
            state: "in_game".to_owned(),
            members: vec![],
            current_frame_id,
            game_state: serde_json::to_string(game_state).unwrap(),
        }
    }

    fn assert_online_error_contains(error: OnlineError, expected: &str) {
        let actual = error.to_string();
        assert!(
            actual.contains(expected),
            "expected online error to contain `{expected}`, got `{actual}`"
        );
    }

    #[test]
    fn online_cli_parses_dry_run_options() {
        let options = OnlineCliOptions::parse([
            "--mode",
            "online",
            "--scenario",
            "move_straight",
            "--server",
            "127.0.0.1:4000",
            "--test-ticket",
            "ticket-1",
            "--observer-ticket",
            "observer-ticket-1",
            "--probe-observer-recovery",
            "--room",
            "room-1",
            "--character-id",
            "player-a",
            "--dry-run",
        ])
        .unwrap();

        assert_eq!(options.server_addr, "127.0.0.1:4000");
        assert_eq!(options.ticket.as_deref(), Some("ticket-1"));
        assert_eq!(
            options.observer_ticket.as_deref(),
            Some("observer-ticket-1")
        );
        assert!(options.probe_observer_recovery);
        assert_eq!(options.room_id, "room-1");
        assert_eq!(options.character_id.as_deref(), Some("player-a"));
        assert!(options.dry_run);
    }

    #[test]
    fn online_cli_rejects_invalid_args() {
        assert_online_error_contains(
            OnlineCliOptions::parse(["--mode", "online"]).unwrap_err(),
            "missing --scenario",
        );
        assert_online_error_contains(
            OnlineCliOptions::parse([
                "--mode",
                "online",
                "--scenario",
                "move_straight",
                "--scenario",
                "move_stop",
            ])
            .unwrap_err(),
            "duplicate --scenario",
        );
        assert_online_error_contains(
            OnlineCliOptions::parse([
                "--mode",
                "online",
                "--scenario",
                "move_straight",
                "--timeout-ms",
                "0",
            ])
            .unwrap_err(),
            "--timeout-ms must be greater than zero",
        );
        assert_online_error_contains(
            OnlineCliOptions::parse([
                "--mode",
                "offline",
                "--scenario",
                "move_straight",
                "--dry-run",
            ])
            .unwrap_err(),
            "unsupported --mode `offline`",
        );
        assert_online_error_contains(
            OnlineCliOptions::parse(["--mode", "online", "--scenario", "move_straight", "--wat"])
                .unwrap_err(),
            "unexpected argument `--wat`",
        );
    }

    #[test]
    fn non_dry_run_requires_ticket_before_network_connect() {
        let error = run_cli(["--mode", "online", "--scenario", "move_straight"]).unwrap_err();

        assert_online_error_contains(error, "requires --ticket or --test-ticket");
    }

    #[test]
    fn dry_run_report_lists_join_start_input_and_downlink_packets() {
        let report = run_cli([
            "--mode",
            "online",
            "--scenario",
            "move_straight",
            "--room",
            "room-1",
            "--policy",
            LOCKSTEP_SIM_DEMO_POLICY_ID,
            "--dry-run",
        ])
        .unwrap();
        let rendered = report.to_string();

        assert!(report.dry_run);
        assert_eq!(report.room_id, "room-1");
        assert_eq!(report.policy_id, LOCKSTEP_SIM_DEMO_POLICY_ID);
        assert_eq!(report.input_plan_count, 5);
        assert!(rendered.contains("dry-run packet plan:"));
        assert!(rendered.contains(
            "send RoomJoinReq(1101): create-or-join room=room-1 policy=lockstep_sim_demo"
        ));
        assert!(rendered.contains("send RoomStartReq(1107): start lockstep room"));
        assert!(rendered.contains("expect FrameBundlePush(1203):"));
        assert!(rendered.contains("hash/events/eventSummaries/inputSources"));
        assert!(
            rendered.contains("send PlayerInputReq(1111): frame=1 action=sim_input payload_json=")
        );
        assert!(rendered.contains(r#""type":"move""#));
    }

    #[test]
    fn builds_server_sim_input_payload_from_supported_sim_core_commands() {
        let payload = build_sim_input_payload(
            7,
            &[
                SimCommand::Move(MoveCommand {
                    dir: QuantizedDir::RIGHT,
                    speed_per_second: Some(Fp::from_milli(6_000)),
                }),
                SimCommand::Stop,
                SimCommand::Face(FaceCommand {
                    dir: QuantizedDir::LEFT,
                }),
                SimCommand::CastSkill(CastSkillCommand {
                    skill_id: SkillId::new(DEFAULT_PLAYER_SKILL_ID),
                    target: SkillTarget::Entity(EntityId::new(TRAINING_TARGET_ENTITY_ID)),
                }),
            ],
        )
        .unwrap();
        let parsed = parse_wire_sim_input_payload(&payload).unwrap();

        assert_eq!(parsed.seq, 7);
        assert_eq!(parsed.commands.len(), 4);
        assert!(matches!(
            parsed.commands[0].to_sim_command(),
            SimCommand::Move(command)
                if command.dir == QuantizedDir::RIGHT
                    && command.speed_per_second == Some(Fp::from_milli(6_000))
        ));
        assert_eq!(parsed.commands[1].to_sim_command(), SimCommand::Stop);
        assert!(matches!(
            parsed.commands[2].to_sim_command(),
            SimCommand::Face(command) if command.dir == QuantizedDir::LEFT
        ));
        assert!(matches!(
            parsed.commands[3].to_sim_command(),
            SimCommand::CastSkill(command)
                if command.skill_id == SkillId::new(DEFAULT_PLAYER_SKILL_ID)
                    && command.target == SkillTarget::Entity(EntityId::new(TRAINING_TARGET_ENTITY_ID))
        ));
    }

    #[test]
    fn sim_input_payload_reports_validation_errors() {
        assert_online_error_contains(
            parse_wire_sim_input_payload(r#"{"version":2,"seq":1,"commands":[]}"#).unwrap_err(),
            "UNSUPPORTED_SIM_INPUT_VERSION",
        );
        assert_online_error_contains(
            parse_wire_sim_input_payload(
                r#"{"version":1,"seq":1,"commands":[{"type":"move","dirX":0,"dirY":0}]}"#,
            )
            .unwrap_err(),
            "SIM_INPUT_MOVE_DIR_ZERO",
        );
        assert_online_error_contains(
            parse_wire_sim_input_payload(
                r#"{"version":1,"seq":1,"commands":[{"type":"move","dirX":1000,"dirY":0,"speed":12001}]}"#,
            )
            .unwrap_err(),
            "SIM_INPUT_SPEED_OUT_OF_RANGE",
        );
        assert_online_error_contains(
            parse_wire_sim_input_payload(
                r#"{"version":1,"seq":1,"commands":[{"type":"castSkill","skillId":0}]}"#,
            )
            .unwrap_err(),
            "SIM_INPUT_SKILL_ID_OUT_OF_RANGE",
        );
        assert_online_error_contains(
            build_sim_input_payload(
                1,
                &[SimCommand::CastSkill(CastSkillCommand {
                    skill_id: SkillId::new(DEFAULT_PLAYER_SKILL_ID),
                    target: SkillTarget::Direction(QuantizedDir::RIGHT),
                })],
            )
            .unwrap_err(),
            "only supports CastSkill target None or Entity",
        );
    }

    #[test]
    fn lockstep_demo_melee_scenario_builds_target_9000_payload() {
        let scenario =
            Scenario::from_json_str(include_str!("../scenarios/lockstep_demo_melee.json")).unwrap();
        let inputs = scenario.to_sim_inputs().unwrap();
        let plan = build_player_input_plan(&inputs).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].frame_id, 1);
        assert_eq!(plan[0].action, SIM_INPUT_ACTION);

        let payload = serde_json::from_str::<WireSimInputPayload>(&plan[0].payload_json).unwrap();
        assert_eq!(payload.version, SIM_INPUT_VERSION);
        assert_eq!(payload.seq, 1);
        assert_eq!(
            payload.commands,
            vec![WireSimCommand::CastSkill {
                skill_id: DEFAULT_PLAYER_SKILL_ID,
                target_entity_id: Some(TRAINING_TARGET_ENTITY_ID),
            }]
        );
    }

    #[test]
    fn server_contract_metadata_and_diagnostics_are_deserialized_without_loss() {
        let initial = initial_snapshot();
        let mut world = restore_sim_snapshot(&initial.snapshot).unwrap();
        let mut result = step(
            &mut world,
            FrameId::new(1),
            &[],
            &lockstep_demo_config(DEFAULT_LOCKSTEP_SIM_TICK_RATE),
        )
        .unwrap();
        result.events.push(SimEvent::SkillCast {
            frame: FrameId::new(1),
            source_entity: EntityId::new(1000),
            target_entity: Some(EntityId::new(TRAINING_TARGET_ENTITY_ID)),
            skill_id: SkillId::new(DEFAULT_PLAYER_SKILL_ID),
            value: 1,
            sequence: 7,
        });
        let mut envelope = frame_envelope(&result);
        envelope.event_count = envelope.events.len();
        envelope.event_summaries = vec![SimFrameEventSummary {
            schema_version: 1,
            kind: SimFrameEventKind::SkillCast,
            frame: 1,
            source_entity_id: 1000,
            target_entity_id: Some(TRAINING_TARGET_ENTITY_ID),
            skill_id: Some(DEFAULT_PLAYER_SKILL_ID),
            buff_id: None,
            amount: 1,
            sequence: 7,
        }];
        envelope.debug_state = SimFrameDebugState {
            schema_version: 1,
            entities: vec![SimFrameEntityDebugState {
                entity_id: 1000,
                x_raw: 0,
                y_raw: 0,
                hp: 100,
                max_hp: 100,
                alive: true,
            }],
        };

        let mut game_state = debug_state(&world, Some(envelope.clone()));
        game_state.initial_snapshot = Some(initial);
        game_state.observer_frame = Some(LockstepSimObserverFrame {
            world_frame: 1,
            state_hash: envelope.state_hash.clone(),
            last_event_count: envelope.event_count,
            last_event_summaries: envelope.event_summaries.clone(),
            last_frame: Some(envelope),
        });

        let wire = serde_json::to_value(&game_state).unwrap();
        assert_eq!(wire["initialSnapshot"]["configVersion"], 1);
        assert_eq!(
            wire["initialSnapshot"]["simSchemaVersion"],
            sim_core::SIM_CORE_SCHEMA_VERSION
        );
        assert_eq!(wire["lastFrame"]["eventCount"], 1);
        assert_eq!(wire["lastFrame"]["eventSummaries"][0]["schemaVersion"], 1);
        assert_eq!(wire["lastFrame"]["debugState"]["entities"][0]["xRaw"], 0);

        let parsed = parse_game_state_json(&wire.to_string()).unwrap();
        let parsed_initial = parsed.initial_snapshot.as_ref().unwrap();
        assert_eq!(parsed_initial.config_version, 1);
        assert_eq!(
            parsed_initial.sim_schema_version,
            sim_core::SIM_CORE_SCHEMA_VERSION
        );
        let parsed_frame = parsed.last_frame.as_ref().unwrap();
        assert_eq!(parsed_frame.event_count, 1);
        assert_eq!(
            parsed_frame.event_summaries[0].kind,
            SimFrameEventKind::SkillCast
        );
        assert_eq!(parsed_frame.debug_state.entities[0].entity_id, 1000);
        let parsed_observer = parsed.observer_frame.as_ref().unwrap();
        assert_eq!(parsed_observer.last_event_count, 1);
        assert_eq!(parsed_observer.last_event_summaries.len(), 1);
    }

    #[test]
    fn initial_and_frame_contract_metadata_mismatches_are_rejected() {
        let initial = initial_snapshot();

        let mut invalid_initial = initial.clone();
        invalid_initial.config_version = 0;
        assert_online_error_contains(
            OnlineReplay::from_initial_snapshot(&invalid_initial).unwrap_err(),
            "UNSUPPORTED_SIM_CONFIG_VERSION expected >= 1, got 0",
        );

        let mut invalid_initial = initial.clone();
        invalid_initial.sim_schema_version = sim_core::SIM_CORE_SCHEMA_VERSION + 1;
        assert_online_error_contains(
            OnlineReplay::from_initial_snapshot(&invalid_initial).unwrap_err(),
            &format!(
                "UNSUPPORTED_SIM_SCHEMA_VERSION expected {}, got {}",
                sim_core::SIM_CORE_SCHEMA_VERSION,
                sim_core::SIM_CORE_SCHEMA_VERSION + 1
            ),
        );

        let mut invalid_initial = initial.clone();
        invalid_initial.config_hash.clear();
        assert_online_error_contains(
            OnlineReplay::from_initial_snapshot(&invalid_initial).unwrap_err(),
            "INVALID_SIM_CONFIG_HASH",
        );

        let mut world = restore_sim_snapshot(&initial.snapshot).unwrap();
        let result = step(
            &mut world,
            FrameId::new(1),
            &[],
            &lockstep_demo_config(DEFAULT_LOCKSTEP_SIM_TICK_RATE),
        )
        .unwrap();
        let mut valid_envelope = frame_envelope(&result);
        valid_envelope.input_sources.clear();
        let apply = |envelope: SimFrameEnvelope| {
            let mut replay = OnlineReplay::from_initial_snapshot(&initial).unwrap();
            replay.apply_server_frame(&ServerFrameObservation {
                envelope,
                inputs: Vec::new(),
                game_state: None,
            })
        };

        apply(valid_envelope.clone()).unwrap();

        let mut mismatch = valid_envelope.clone();
        mismatch.config_version += 1;
        assert_online_error_contains(
            apply(mismatch).unwrap_err(),
            "SIM_FRAME_CONFIG_VERSION_MISMATCH expected 1, got 2",
        );

        let mut mismatch = valid_envelope.clone();
        mismatch.config_hash = "other-config".to_owned();
        assert_online_error_contains(
            apply(mismatch).unwrap_err(),
            "SIM_FRAME_CONFIG_HASH_MISMATCH expected test, got other-config",
        );

        let mut mismatch = valid_envelope.clone();
        mismatch.sim_schema_version += 1;
        assert_online_error_contains(
            apply(mismatch).unwrap_err(),
            &format!(
                "SIM_FRAME_SIM_SCHEMA_VERSION_MISMATCH expected {}, got {}",
                sim_core::SIM_CORE_SCHEMA_VERSION,
                sim_core::SIM_CORE_SCHEMA_VERSION + 1
            ),
        );

        let mut mismatch = valid_envelope;
        mismatch.event_count += 1;
        assert_online_error_contains(
            apply(mismatch).unwrap_err(),
            "SIM_FRAME_EVENT_COUNT_MISMATCH declared 1, actual 0",
        );
    }

    #[test]
    fn online_replay_matches_server_frame_hash_and_events() {
        let initial = initial_snapshot();
        let mut server_world = restore_sim_snapshot(&initial.snapshot).unwrap();
        let input = FrameInputRecord {
            character_id: "player-a".to_owned(),
            action: SIM_INPUT_ACTION.to_owned(),
            payload_json: move_right_payload(1),
            frame_id: 1,
        };
        let sim_inputs = sim_inputs_from_frame_records(
            std::slice::from_ref(&input),
            &[SimFrameInputSourceSummary {
                frame: 1,
                character_id: "player-a".to_owned(),
                source: SimFrameInputSource::Real,
                action: SIM_INPUT_ACTION.to_owned(),
            }],
            &HashMap::from([("player-a".to_owned(), EntityId::new(1000))]),
        )
        .unwrap();
        let result = step(
            &mut server_world,
            FrameId::new(1),
            &sim_inputs,
            &lockstep_demo_config(DEFAULT_LOCKSTEP_SIM_TICK_RATE),
        )
        .unwrap();
        let envelope = frame_envelope(&result);
        let game_state = debug_state(&server_world, Some(envelope.clone()));

        let mut replay = OnlineReplay::from_initial_snapshot(&initial).unwrap();
        replay
            .apply_server_frame(&ServerFrameObservation {
                envelope,
                inputs: vec![input],
                game_state: Some(game_state),
            })
            .unwrap();

        assert_eq!(replay.current_frame(), 1);
        assert_eq!(replay.final_hash(), result.state_hash);
        assert_eq!(replay.frames_checked(), 1);
    }

    #[test]
    fn room_state_and_frame_bundle_push_restore_and_replay_snapshot() {
        let initial = initial_snapshot();
        let mut replay = None;
        let mut game_state = debug_state(&restore_sim_snapshot(&initial.snapshot).unwrap(), None);
        game_state.initial_snapshot = Some(initial.clone());
        let room_state_push = pb::RoomStatePush {
            event: "state".to_owned(),
            snapshot: Some(room_snapshot_from_game_state(
                "room-lockstep",
                0,
                &game_state,
            )),
        };
        let room_state_packet = Packet {
            msg_type: MessageType::RoomStatePush as u16,
            seq: 1,
            body: room_state_push.encode_to_vec(),
        };

        maybe_consume_push_packet(&room_state_packet, &mut replay).unwrap();

        let replay_ref = replay.as_ref().unwrap();
        assert_eq!(replay_ref.current_frame(), 0);
        assert_eq!(replay_ref.final_hash(), initial.snapshot.hash);

        let input = FrameInputRecord {
            character_id: "player-a".to_owned(),
            action: SIM_INPUT_ACTION.to_owned(),
            payload_json: move_right_payload(1),
            frame_id: 1,
        };
        let sim_inputs = sim_inputs_from_frame_records(
            std::slice::from_ref(&input),
            &[SimFrameInputSourceSummary {
                frame: 1,
                character_id: "player-a".to_owned(),
                source: SimFrameInputSource::Real,
                action: SIM_INPUT_ACTION.to_owned(),
            }],
            &HashMap::from([("player-a".to_owned(), EntityId::new(1000))]),
        )
        .unwrap();
        let mut server_world = restore_sim_snapshot(&initial.snapshot).unwrap();
        let result = step(
            &mut server_world,
            FrameId::new(1),
            &sim_inputs,
            &lockstep_demo_config(DEFAULT_LOCKSTEP_SIM_TICK_RATE),
        )
        .unwrap();
        let envelope = frame_envelope(&result);
        let bundle_game_state = debug_state(&server_world, Some(envelope));
        let bundle_push = pb::FrameBundlePush {
            room_id: "room-lockstep".to_owned(),
            frame_id: 1,
            fps: DEFAULT_LOCKSTEP_SIM_TICK_RATE as u32,
            inputs: vec![pb::FrameInput {
                character_id: input.character_id,
                action: input.action,
                payload_json: input.payload_json,
                frame_id: input.frame_id,
            }],
            is_silent_frame: false,
            snapshot: Some(room_snapshot_from_game_state(
                "room-lockstep",
                1,
                &bundle_game_state,
            )),
        };
        let bundle_packet = Packet {
            msg_type: MessageType::FrameBundlePush as u16,
            seq: 2,
            body: bundle_push.encode_to_vec(),
        };

        maybe_consume_push_packet(&bundle_packet, &mut replay).unwrap();

        let replay_ref = replay.as_ref().unwrap();
        assert_eq!(replay_ref.current_frame(), 1);
        assert_eq!(replay_ref.final_hash(), result.state_hash);
        assert_eq!(replay_ref.frames_checked(), 1);
    }

    #[test]
    fn online_replay_mismatch_reports_hash_entities_events_and_inputs() {
        let initial = initial_snapshot();
        let mut server_world = restore_sim_snapshot(&initial.snapshot).unwrap();
        let input = FrameInputRecord {
            character_id: "player-a".to_owned(),
            action: SIM_INPUT_ACTION.to_owned(),
            payload_json: move_right_payload(1),
            frame_id: 1,
        };
        let sim_inputs = sim_inputs_from_frame_records(
            std::slice::from_ref(&input),
            &[SimFrameInputSourceSummary {
                frame: 1,
                character_id: "player-a".to_owned(),
                source: SimFrameInputSource::Real,
                action: SIM_INPUT_ACTION.to_owned(),
            }],
            &HashMap::from([("player-a".to_owned(), EntityId::new(1000))]),
        )
        .unwrap();
        let result = step(
            &mut server_world,
            FrameId::new(1),
            &sim_inputs,
            &lockstep_demo_config(DEFAULT_LOCKSTEP_SIM_TICK_RATE),
        )
        .unwrap();
        server_world
            .entity_mut(EntityId::new(1000))
            .unwrap()
            .transform
            .pos
            .x = Fp::from_milli(301);

        let mut envelope = frame_envelope(&result);
        envelope.state_hash.value ^= 1;
        envelope.events.push(SimEvent::SkillCast {
            frame: FrameId::new(1),
            source_entity: EntityId::new(1000),
            target_entity: Some(EntityId::new(TRAINING_TARGET_ENTITY_ID)),
            skill_id: SkillId::new(DEFAULT_PLAYER_SKILL_ID),
            value: 1,
            sequence: 99,
        });
        envelope.event_count = envelope.events.len();
        let game_state = debug_state(&server_world, Some(envelope.clone()));

        let mut replay = OnlineReplay::from_initial_snapshot(&initial).unwrap();
        let error = replay
            .apply_server_frame(&ServerFrameObservation {
                envelope,
                inputs: vec![input],
                game_state: Some(game_state),
            })
            .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("first mismatch frame 1"));
        assert!(message.contains("server_hash:"));
        assert!(message.contains("client_hash:"));
        assert!(message.contains("entity diffs:"));
        assert!(message.contains("event diffs:"));
        assert!(message.contains("inputs:"));
    }

    #[test]
    fn observer_recovery_snapshot_reports_restorable_frame_and_hash() {
        let initial = initial_snapshot();
        let mut server_world = restore_sim_snapshot(&initial.snapshot).unwrap();
        let input = FrameInputRecord {
            character_id: "player-a".to_owned(),
            action: SIM_INPUT_ACTION.to_owned(),
            payload_json: move_right_payload(1),
            frame_id: 1,
        };
        let sim_inputs = sim_inputs_from_frame_records(
            std::slice::from_ref(&input),
            &[SimFrameInputSourceSummary {
                frame: 1,
                character_id: "player-a".to_owned(),
                source: SimFrameInputSource::Real,
                action: SIM_INPUT_ACTION.to_owned(),
            }],
            &HashMap::from([("player-a".to_owned(), EntityId::new(1000))]),
        )
        .unwrap();
        let result = step(
            &mut server_world,
            FrameId::new(1),
            &sim_inputs,
            &lockstep_demo_config(DEFAULT_LOCKSTEP_SIM_TICK_RATE),
        )
        .unwrap();
        let envelope = frame_envelope(&result);
        let mut game_state = debug_state(&server_world, Some(envelope.clone()));
        game_state.initial_snapshot = Some(initial);
        game_state.observer_frame = Some(LockstepSimObserverFrame {
            world_frame: 1,
            state_hash: sim_hash_envelope(result.state_hash),
            last_event_count: envelope.event_count,
            last_event_summaries: envelope.event_summaries.clone(),
            last_frame: Some(envelope),
        });
        let snapshot = room_snapshot_from_game_state("room-lockstep", 1, &game_state);

        let report = validate_observer_recovery_snapshot(snapshot, 1).unwrap();

        assert_eq!(report.current_frame_id, 1);
        assert_eq!(report.snapshot_frame_id, 1);
        assert_eq!(report.initial_snapshot_frame, 0);
        assert_eq!(report.last_frame, 1);
        assert_eq!(report.observer_last_frame, 1);
        assert_eq!(report.observer_hash, result.state_hash);
    }

    #[test]
    fn packet_codec_round_trips_header_and_body() {
        let body = vec![1, 2, 3];
        let encoded = encode_packet(MessageType::PlayerInputReq, 42, &body);
        let packet = read_packet(&mut encoded.as_slice()).unwrap();

        assert_eq!(packet.msg_type, MessageType::PlayerInputReq as u16);
        assert_eq!(packet.seq, 42);
        assert_eq!(packet.body, body);
    }
}
