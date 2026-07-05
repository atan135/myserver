use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sim_core::{
    BuffDefinition, BuffId, CastSkillCommand, CombatConfig, CombatEffect, CombatState,
    DamageFormula, EntityId, EntityKind, FaceCommand, Fp, FrameId, MoveCommand, MovementConfig,
    MovementMode, MovementState, QuantizedDir, SceneBounds, SimCommand, SimConfig, SimEntity,
    SimHash, SimInput, SimInputSource, SimSnapshot, SimStepResult, SimTransform, SimWorld,
    SkillDefinition, SkillId, SkillSlot, SkillTarget, SkillTargetType, SnapshotError, StepError,
    TeamId, Vec2Fp, restore as restore_sim_snapshot, snapshot as capture_sim_snapshot,
};

use crate::core::room::PlayerInputRecord;

pub const SIM_INPUT_ACTION: &str = "sim_input";
pub const SIM_INPUT_VERSION: u32 = 1;
pub const PLAYER_ENTITY_ID_BASE: u32 = 1000;
pub const TRAINING_TARGET_ENTITY_ID: u32 = 9000;
pub const DEFAULT_PLAYER_SKILL_ID: u32 = 1;
pub const DEFAULT_DEMO_BUFF_ID: u32 = 1;
pub const DEFAULT_LOCKSTEP_SIM_TICK_RATE: u16 = 20;
pub const LOCKSTEP_SIM_DEMO_FIXED_CONFIG_VERSION: u64 = 1;
pub const LOCKSTEP_SIM_DEMO_CONFIG_SOURCE: &str = "lockstep_sim_demo.fixed_v1";
pub const LOCKSTEP_SIM_DEMO_CONFIG_MIGRATION_BOUNDARY: &str =
    "skills_and_buffs_use_fixed_demo_definitions_until_sim_core_csv_mapping_is_complete";
pub const SIM_INITIAL_SNAPSHOT_SCHEMA: &str = "myserver.lockstep-sim.initial-snapshot.v1";
pub const SIM_FRAME_ENVELOPE_SCHEMA: &str = "myserver.lockstep-sim.frame-envelope.v1";
pub const SIM_DOWNLINK_SCHEMA_VERSION: u32 = 1;
pub const SIM_EVENT_SUMMARY_SCHEMA_VERSION: u32 = 1;
pub const SIM_DEBUG_STATE_SCHEMA_VERSION: u32 = 1;
pub const SIM_FRAME_DEBUG_STATE_ENTITY_LIMIT: usize = 32;
const SIM_INPUT_PAYLOAD_MAX_BYTES: usize = 2048;
const SIM_INPUT_MAX_COMMANDS: usize = 8;
const SIM_INPUT_MAX_SPEED_MILLI: i64 = 12_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimHashEnvelope {
    pub frame: u32,
    pub value: u64,
    pub hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimControlBinding {
    pub character_id: String,
    pub entity_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundSimConfig {
    pub config_version: u64,
    pub config_hash: String,
    pub sim_schema_version: u16,
    pub config: SimConfig,
}

impl BoundSimConfig {
    pub fn tick_rate(&self) -> u16 {
        self.config.movement.tick_rate
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimInitialSnapshot {
    pub schema: String,
    pub schema_version: u32,
    pub room_id: String,
    pub start_frame: u32,
    pub tick_rate: u16,
    #[serde(default = "default_sim_config_version")]
    pub config_version: u64,
    pub config_hash: String,
    #[serde(default = "default_sim_schema_version")]
    pub sim_schema_version: u16,
    pub rng_seed: u64,
    pub state_hash: SimHashEnvelope,
    pub snapshot: SimSnapshot,
    pub entities: Vec<SimEntity>,
    pub control_bindings: Vec<SimControlBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimFrameInputSourceSummary {
    pub frame: u32,
    pub character_id: String,
    pub source: SimFrameInputSource,
    pub action: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SimFrameInputSource {
    Real,
    SynthesizedEmpty,
    SynthesizedRepeatLast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

impl SimFrameEventKind {
    fn sort_code(self) -> u8 {
        match self {
            Self::SkillCast => 10,
            Self::BuffApplied => 20,
            Self::BuffTick => 30,
            Self::Damage => 40,
            Self::Heal => 50,
            Self::BuffExpired => 60,
            Self::Death => 70,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimFrameEventSummary {
    #[serde(default = "default_sim_event_summary_schema_version")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimFrameDebugState {
    #[serde(default = "default_sim_debug_state_schema_version")]
    pub schema_version: u32,
    pub entities: Vec<SimFrameEntityDebugState>,
}

impl Default for SimFrameDebugState {
    fn default() -> Self {
        Self {
            schema_version: SIM_DEBUG_STATE_SCHEMA_VERSION,
            entities: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimFrameEntityDebugState {
    pub entity_id: u32,
    pub x_raw: i64,
    pub y_raw: i64,
    pub hp: i32,
    pub max_hp: i32,
    pub alive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimFrameEnvelope {
    pub schema: String,
    pub schema_version: u32,
    pub room_id: String,
    pub frame: u32,
    pub tick_rate: u16,
    #[serde(default = "default_sim_config_version")]
    pub config_version: u64,
    pub config_hash: String,
    #[serde(default = "default_sim_schema_version")]
    pub sim_schema_version: u16,
    pub state_hash: SimHashEnvelope,
    #[serde(default)]
    pub event_count: usize,
    pub events: Vec<sim_core::SimEvent>,
    #[serde(default)]
    pub event_summaries: Vec<SimFrameEventSummary>,
    #[serde(default)]
    pub input_sources: Vec<SimFrameInputSourceSummary>,
    pub debug_summary: SimFrameDebugSummary,
    #[serde(default)]
    pub debug_state: SimFrameDebugState,
}

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
    let tick_rate = tick_rate.max(1);
    SimConfig {
        movement: MovementConfig {
            tick_rate,
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
                cooldown_frames: tick_rate as u32,
                cast_range: Fp::from_i32(12),
                target_type: SkillTargetType::Enemy,
                effects: vec![CombatEffect::Damage {
                    formula: DamageFormula::Fixed { amount: 15 },
                }],
            }],
            vec![BuffDefinition {
                id: BuffId::new(DEFAULT_DEMO_BUFF_ID),
                duration_frames: tick_rate as u32 * 3,
                interval_frames: tick_rate as u32,
                max_stacks: 1,
                effects: vec![CombatEffect::Heal {
                    formula: DamageFormula::Fixed { amount: 1 },
                }],
            }],
        )
        .expect("lockstep_sim_demo default combat config should be valid"),
    }
}

pub fn default_bound_sim_config(tick_rate: u16) -> BoundSimConfig {
    room_sim_config(LOCKSTEP_SIM_DEMO_FIXED_CONFIG_VERSION, tick_rate)
}

pub fn room_sim_config(config_version: u64, tick_rate: u16) -> BoundSimConfig {
    let config = default_sim_config(tick_rate);
    BoundSimConfig {
        config_version,
        config_hash: sim_config_hash_hex(&config),
        sim_schema_version: sim_core::SIM_CORE_SCHEMA_VERSION,
        config,
    }
}

pub fn sim_hash_envelope(hash: SimHash) -> SimHashEnvelope {
    SimHashEnvelope {
        frame: hash.frame.raw(),
        value: hash.value,
        hex: sim_hash_hex(hash),
    }
}

pub fn sim_hash_hex(hash: SimHash) -> String {
    format!("{:016x}", hash.value)
}

pub fn sim_config_hash_hex(config: &SimConfig) -> String {
    let encoded = serde_json::to_vec(config).expect("sim config should serialize to JSON");
    let digest = Sha256::digest(encoded);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").expect("writing sha256 hex to String should not fail");
    }
    hex
}

pub fn create_initial_snapshot(
    room_id: &str,
    tick_rate: u16,
    world: &SimWorld,
    bindings: &HashMap<String, EntityId>,
) -> SimInitialSnapshot {
    let config = default_bound_sim_config(tick_rate);
    create_initial_snapshot_with_config(room_id, &config, world, bindings)
}

pub fn create_initial_snapshot_with_config(
    room_id: &str,
    config: &BoundSimConfig,
    world: &SimWorld,
    bindings: &HashMap<String, EntityId>,
) -> SimInitialSnapshot {
    let snapshot = capture_sim_snapshot(world, &config.config);

    SimInitialSnapshot {
        schema: SIM_INITIAL_SNAPSHOT_SCHEMA.to_string(),
        schema_version: SIM_DOWNLINK_SCHEMA_VERSION,
        room_id: room_id.to_string(),
        start_frame: world.frame.raw(),
        tick_rate: config.tick_rate(),
        config_version: config.config_version,
        config_hash: config.config_hash.clone(),
        sim_schema_version: config.sim_schema_version,
        rng_seed: world.rng.seed,
        state_hash: sim_hash_envelope(snapshot.hash),
        snapshot,
        entities: world.entities_sorted_by_id().to_vec(),
        control_bindings: control_bindings_snapshot(bindings),
    }
}

pub fn create_frame_envelope(
    room_id: &str,
    tick_rate: u16,
    world: &SimWorld,
    inputs: &[PlayerInputRecord],
    result: &SimStepResult,
) -> SimFrameEnvelope {
    let config = default_bound_sim_config(tick_rate);
    create_frame_envelope_with_config(room_id, &config, world, inputs, result)
}

pub fn create_frame_envelope_with_config(
    room_id: &str,
    config: &BoundSimConfig,
    world: &SimWorld,
    inputs: &[PlayerInputRecord],
    result: &SimStepResult,
) -> SimFrameEnvelope {
    SimFrameEnvelope {
        schema: SIM_FRAME_ENVELOPE_SCHEMA.to_string(),
        schema_version: SIM_DOWNLINK_SCHEMA_VERSION,
        room_id: room_id.to_string(),
        frame: result.frame.raw(),
        tick_rate: config.tick_rate(),
        config_version: config.config_version,
        config_hash: config.config_hash.clone(),
        sim_schema_version: config.sim_schema_version,
        state_hash: sim_hash_envelope(result.state_hash),
        event_count: result.events.len(),
        events: result.events.clone(),
        event_summaries: sim_event_summaries(&result.events),
        input_sources: frame_input_source_summary(inputs),
        debug_summary: frame_debug_summary(world, inputs, &result.events),
        debug_state: frame_debug_state(world),
    }
}

fn default_sim_config_version() -> u64 {
    LOCKSTEP_SIM_DEMO_FIXED_CONFIG_VERSION
}

fn default_sim_schema_version() -> u16 {
    sim_core::SIM_CORE_SCHEMA_VERSION
}

fn default_sim_event_summary_schema_version() -> u32 {
    SIM_EVENT_SUMMARY_SCHEMA_VERSION
}

fn default_sim_debug_state_schema_version() -> u32 {
    SIM_DEBUG_STATE_SCHEMA_VERSION
}

pub fn restore_initial_snapshot(
    snapshot: &SimInitialSnapshot,
) -> Result<(SimWorld, HashMap<String, EntityId>), LockstepSimSnapshotError> {
    if snapshot.schema != SIM_INITIAL_SNAPSHOT_SCHEMA {
        return Err(LockstepSimSnapshotError::UnsupportedSchema);
    }
    if snapshot.schema_version != SIM_DOWNLINK_SCHEMA_VERSION {
        return Err(LockstepSimSnapshotError::UnsupportedSchema);
    }
    if snapshot.room_id.trim().is_empty() {
        return Err(LockstepSimSnapshotError::InvalidRoomId);
    }
    if snapshot.tick_rate == 0 {
        return Err(LockstepSimSnapshotError::InvalidTickRate);
    }
    if snapshot.config_version == 0 {
        return Err(LockstepSimSnapshotError::InvalidConfigVersion);
    }
    if snapshot.sim_schema_version != sim_core::SIM_CORE_SCHEMA_VERSION {
        return Err(LockstepSimSnapshotError::UnsupportedSchema);
    }

    let config = room_sim_config(snapshot.config_version, snapshot.tick_rate);
    if snapshot.config_hash != config.config_hash {
        return Err(LockstepSimSnapshotError::ConfigHashMismatch);
    }

    let world =
        restore_sim_snapshot(&snapshot.snapshot).map_err(LockstepSimSnapshotError::Snapshot)?;
    if snapshot.start_frame != world.frame.raw() {
        return Err(LockstepSimSnapshotError::FrameMismatch);
    }
    if snapshot.rng_seed != world.rng.seed {
        return Err(LockstepSimSnapshotError::RngSeedMismatch);
    }
    if snapshot.state_hash != sim_hash_envelope(snapshot.snapshot.hash) {
        return Err(LockstepSimSnapshotError::HashEnvelopeMismatch);
    }

    let mut entities = snapshot.entities.clone();
    entities.sort_by_key(|entity| entity.id);
    if entities != world.entities_sorted_by_id() {
        return Err(LockstepSimSnapshotError::EntitiesMismatch);
    }

    let bindings = restore_control_bindings(&snapshot.control_bindings, &world)?;
    Ok((world, bindings))
}

fn control_bindings_snapshot(bindings: &HashMap<String, EntityId>) -> Vec<SimControlBinding> {
    let mut snapshot = bindings
        .iter()
        .map(|(character_id, entity_id)| SimControlBinding {
            character_id: character_id.clone(),
            entity_id: entity_id.raw(),
        })
        .collect::<Vec<_>>();
    snapshot.sort_by(|left, right| {
        left.character_id
            .cmp(&right.character_id)
            .then_with(|| left.entity_id.cmp(&right.entity_id))
    });
    snapshot
}

fn restore_control_bindings(
    bindings: &[SimControlBinding],
    world: &SimWorld,
) -> Result<HashMap<String, EntityId>, LockstepSimSnapshotError> {
    let entities = world
        .entities_sorted_by_id()
        .iter()
        .map(|entity| (entity.id, entity))
        .collect::<HashMap<_, _>>();
    let mut restored = HashMap::new();
    let mut character_ids = HashSet::new();
    let mut bound_entity_ids = HashSet::new();

    for binding in bindings {
        if binding.character_id.trim().is_empty()
            || !character_ids.insert(binding.character_id.clone())
        {
            return Err(LockstepSimSnapshotError::InvalidControlBinding);
        }

        let entity_id = EntityId::new(binding.entity_id);
        let Some(entity) = entities.get(&entity_id) else {
            return Err(LockstepSimSnapshotError::InvalidControlBinding);
        };
        if !bound_entity_ids.insert(entity_id)
            || entity.owner_character_id.as_deref() != Some(binding.character_id.as_str())
        {
            return Err(LockstepSimSnapshotError::InvalidControlBinding);
        }
        restored.insert(binding.character_id.clone(), entity_id);
    }

    Ok(restored)
}

fn frame_debug_summary(
    world: &SimWorld,
    inputs: &[PlayerInputRecord],
    events: &[sim_core::SimEvent],
) -> SimFrameDebugSummary {
    SimFrameDebugSummary {
        input_count: inputs.len(),
        real_input_count: inputs.iter().filter(|input| !input.is_synthetic).count(),
        synthetic_input_count: inputs.iter().filter(|input| input.is_synthetic).count(),
        synthesized_empty_input_count: inputs
            .iter()
            .filter(|input| matches!(sim_input_source(input), SimInputSource::SynthesizedEmpty))
            .count(),
        synthesized_repeat_last_input_count: inputs
            .iter()
            .filter(|input| {
                matches!(
                    sim_input_source(input),
                    SimInputSource::SynthesizedRepeatLast
                )
            })
            .count(),
        event_count: events.len(),
        entity_count: world.entities_sorted_by_id().len(),
        alive_entity_count: world
            .entities_sorted_by_id()
            .iter()
            .filter(|entity| entity.alive)
            .count(),
        player_entity_count: world
            .entities_sorted_by_id()
            .iter()
            .filter(|entity| entity.kind == EntityKind::Player)
            .count(),
    }
}

pub fn sim_event_summaries(events: &[sim_core::SimEvent]) -> Vec<SimFrameEventSummary> {
    let mut summaries = events.iter().map(sim_event_summary).collect::<Vec<_>>();
    summaries.sort_by_key(sim_event_summary_sort_key);
    summaries
}

fn sim_event_summary(event: &sim_core::SimEvent) -> SimFrameEventSummary {
    match event {
        sim_core::SimEvent::SkillCast {
            frame,
            source_entity,
            target_entity,
            skill_id,
            value,
            sequence,
        } => SimFrameEventSummary {
            schema_version: SIM_EVENT_SUMMARY_SCHEMA_VERSION,
            kind: SimFrameEventKind::SkillCast,
            frame: frame.raw(),
            source_entity_id: source_entity.raw(),
            target_entity_id: target_entity.map(EntityId::raw),
            skill_id: Some(skill_id.raw()),
            buff_id: None,
            amount: *value,
            sequence: *sequence,
        },
        sim_core::SimEvent::DamageApplied {
            frame,
            source_entity,
            target_entity,
            skill_id,
            buff_id,
            value,
            sequence,
        } => SimFrameEventSummary {
            schema_version: SIM_EVENT_SUMMARY_SCHEMA_VERSION,
            kind: SimFrameEventKind::Damage,
            frame: frame.raw(),
            source_entity_id: source_entity.raw(),
            target_entity_id: Some(target_entity.raw()),
            skill_id: skill_id.map(SkillId::raw),
            buff_id: buff_id.map(BuffId::raw),
            amount: *value,
            sequence: *sequence,
        },
        sim_core::SimEvent::HealApplied {
            frame,
            source_entity,
            target_entity,
            skill_id,
            buff_id,
            value,
            sequence,
        } => SimFrameEventSummary {
            schema_version: SIM_EVENT_SUMMARY_SCHEMA_VERSION,
            kind: SimFrameEventKind::Heal,
            frame: frame.raw(),
            source_entity_id: source_entity.raw(),
            target_entity_id: Some(target_entity.raw()),
            skill_id: skill_id.map(SkillId::raw),
            buff_id: buff_id.map(BuffId::raw),
            amount: *value,
            sequence: *sequence,
        },
        sim_core::SimEvent::BuffApplied {
            frame,
            source_entity,
            target_entity,
            buff_id,
            value,
            sequence,
        } => SimFrameEventSummary {
            schema_version: SIM_EVENT_SUMMARY_SCHEMA_VERSION,
            kind: SimFrameEventKind::BuffApplied,
            frame: frame.raw(),
            source_entity_id: source_entity.raw(),
            target_entity_id: Some(target_entity.raw()),
            skill_id: None,
            buff_id: Some(buff_id.raw()),
            amount: *value,
            sequence: *sequence,
        },
        sim_core::SimEvent::BuffExpired {
            frame,
            source_entity,
            target_entity,
            buff_id,
            value,
            sequence,
        } => SimFrameEventSummary {
            schema_version: SIM_EVENT_SUMMARY_SCHEMA_VERSION,
            kind: SimFrameEventKind::BuffExpired,
            frame: frame.raw(),
            source_entity_id: source_entity.raw(),
            target_entity_id: Some(target_entity.raw()),
            skill_id: None,
            buff_id: Some(buff_id.raw()),
            amount: *value,
            sequence: *sequence,
        },
        sim_core::SimEvent::EntityDied {
            frame,
            source_entity,
            target_entity,
            skill_id,
            buff_id,
            value,
            sequence,
        } => SimFrameEventSummary {
            schema_version: SIM_EVENT_SUMMARY_SCHEMA_VERSION,
            kind: SimFrameEventKind::Death,
            frame: frame.raw(),
            source_entity_id: source_entity.raw(),
            target_entity_id: Some(target_entity.raw()),
            skill_id: skill_id.map(SkillId::raw),
            buff_id: buff_id.map(BuffId::raw),
            amount: *value,
            sequence: *sequence,
        },
        sim_core::SimEvent::BuffTick {
            frame,
            source_entity,
            target_entity,
            buff_id,
            value,
            sequence,
        } => SimFrameEventSummary {
            schema_version: SIM_EVENT_SUMMARY_SCHEMA_VERSION,
            kind: SimFrameEventKind::BuffTick,
            frame: frame.raw(),
            source_entity_id: source_entity.raw(),
            target_entity_id: Some(target_entity.raw()),
            skill_id: None,
            buff_id: Some(buff_id.raw()),
            amount: *value,
            sequence: *sequence,
        },
    }
}

fn sim_event_summary_sort_key(summary: &SimFrameEventSummary) -> (u32, u8, u32, Option<u32>, u32) {
    (
        summary.frame,
        summary.kind.sort_code(),
        summary.source_entity_id,
        summary.target_entity_id,
        summary.sequence,
    )
}

fn frame_debug_state(world: &SimWorld) -> SimFrameDebugState {
    SimFrameDebugState {
        schema_version: SIM_DEBUG_STATE_SCHEMA_VERSION,
        entities: world
            .entities_sorted_by_id()
            .iter()
            .take(SIM_FRAME_DEBUG_STATE_ENTITY_LIMIT)
            .map(entity_debug_state)
            .collect(),
    }
}

fn entity_debug_state(entity: &SimEntity) -> SimFrameEntityDebugState {
    SimFrameEntityDebugState {
        entity_id: entity.id.raw(),
        x_raw: entity.transform.pos.x.raw(),
        y_raw: entity.transform.pos.y.raw(),
        hp: entity.combat.hp,
        max_hp: entity.combat.max_hp,
        alive: entity.alive,
    }
}

fn frame_input_source_summary(inputs: &[PlayerInputRecord]) -> Vec<SimFrameInputSourceSummary> {
    inputs
        .iter()
        .map(|input| SimFrameInputSourceSummary {
            frame: input.frame_id,
            character_id: input.character_id.clone(),
            source: frame_input_source(input),
            action: input.action.clone(),
        })
        .collect()
}

fn frame_input_source(input: &PlayerInputRecord) -> SimFrameInputSource {
    match sim_input_source(input) {
        SimInputSource::Real => SimFrameInputSource::Real,
        SimInputSource::SynthesizedEmpty => SimFrameInputSource::SynthesizedEmpty,
        SimInputSource::SynthesizedRepeatLast => SimFrameInputSource::SynthesizedRepeatLast,
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
    let config = default_bound_sim_config(fps);
    step_world_with_config(world, frame_id, &config, inputs, bindings)
}

pub fn step_world_with_config(
    world: &mut SimWorld,
    frame_id: u32,
    config: &BoundSimConfig,
    inputs: &[PlayerInputRecord],
    bindings: &HashMap<String, EntityId>,
) -> Result<SimStepResult, LockstepSimStepError> {
    let sim_inputs = sim_inputs_from_records(inputs, bindings)?;
    let mut next_world = world.clone();

    let result = sim_core::step(
        &mut next_world,
        FrameId::new(frame_id),
        &sim_inputs,
        &config.config,
    )
    .map_err(LockstepSimStepError::from)?;
    *world = next_world;
    Ok(result)
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
pub enum LockstepSimSnapshotError {
    UnsupportedSchema,
    InvalidRoomId,
    InvalidTickRate,
    InvalidConfigVersion,
    ConfigHashMismatch,
    Snapshot(SnapshotError),
    FrameMismatch,
    RngSeedMismatch,
    HashEnvelopeMismatch,
    EntitiesMismatch,
    InvalidControlBinding,
}

impl std::fmt::Display for LockstepSimSnapshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchema => formatter.write_str("UNSUPPORTED_SIM_SNAPSHOT_SCHEMA"),
            Self::InvalidRoomId => formatter.write_str("INVALID_SIM_SNAPSHOT_ROOM_ID"),
            Self::InvalidTickRate => formatter.write_str("INVALID_SIM_SNAPSHOT_TICK_RATE"),
            Self::InvalidConfigVersion => {
                formatter.write_str("INVALID_SIM_SNAPSHOT_CONFIG_VERSION")
            }
            Self::ConfigHashMismatch => formatter.write_str("SIM_SNAPSHOT_CONFIG_HASH_MISMATCH"),
            Self::Snapshot(error) => write!(formatter, "{error}"),
            Self::FrameMismatch => formatter.write_str("SIM_SNAPSHOT_FRAME_MISMATCH"),
            Self::RngSeedMismatch => formatter.write_str("SIM_SNAPSHOT_RNG_SEED_MISMATCH"),
            Self::HashEnvelopeMismatch => {
                formatter.write_str("SIM_SNAPSHOT_HASH_ENVELOPE_MISMATCH")
            }
            Self::EntitiesMismatch => formatter.write_str("SIM_SNAPSHOT_ENTITIES_MISMATCH"),
            Self::InvalidControlBinding => {
                formatter.write_str("INVALID_SIM_SNAPSHOT_CONTROL_BINDING")
            }
        }
    }
}

impl std::error::Error for LockstepSimSnapshotError {}

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
    use std::time::Instant;

    fn input(frame_id: u32, character_id: &str, payload_json: String) -> PlayerInputRecord {
        PlayerInputRecord {
            frame_id,
            character_id: character_id.to_string(),
            action: SIM_INPUT_ACTION.to_string(),
            payload_json,
            received_at: Instant::now(),
            is_synthetic: false,
        }
    }

    fn synthetic_empty_input(frame_id: u32, character_id: &str) -> PlayerInputRecord {
        PlayerInputRecord {
            frame_id,
            character_id: character_id.to_string(),
            action: String::new(),
            payload_json: String::new(),
            received_at: Instant::now(),
            is_synthetic: true,
        }
    }

    fn synthetic_repeat_last_input(
        frame_id: u32,
        character_id: &str,
        payload_json: String,
    ) -> PlayerInputRecord {
        PlayerInputRecord {
            frame_id,
            character_id: character_id.to_string(),
            action: SIM_INPUT_ACTION.to_string(),
            payload_json,
            received_at: Instant::now(),
            is_synthetic: true,
        }
    }

    fn move_right_payload(seq: u32) -> String {
        serde_json::json!({
            "version": SIM_INPUT_VERSION,
            "seq": seq,
            "commands": [
                { "type": "move", "dirX": 1000, "dirY": 0 }
            ]
        })
        .to_string()
    }

    fn stop_payload(seq: u32) -> String {
        serde_json::json!({
            "version": SIM_INPUT_VERSION,
            "seq": seq,
            "commands": [
                { "type": "stop" }
            ]
        })
        .to_string()
    }

    fn unknown_skill_payload(seq: u32) -> String {
        serde_json::json!({
            "version": SIM_INPUT_VERSION,
            "seq": seq,
            "commands": [
                { "type": "castSkill", "skillId": 999, "targetEntityId": TRAINING_TARGET_ENTITY_ID }
            ]
        })
        .to_string()
    }

    fn cast_training_target_payload(seq: u32) -> String {
        serde_json::json!({
            "version": SIM_INPUT_VERSION,
            "seq": seq,
            "commands": [
                {
                    "type": "castSkill",
                    "skillId": DEFAULT_PLAYER_SKILL_ID,
                    "targetEntityId": TRAINING_TARGET_ENTITY_ID
                }
            ]
        })
        .to_string()
    }

    #[test]
    fn sim_input_payload_accepts_supported_commands_and_preserves_seq() {
        let payload = serde_json::json!({
            "version": SIM_INPUT_VERSION,
            "seq": 42,
            "commands": [
                { "type": "move", "dirX": 1000, "dirY": 0, "speed": 6000 },
                { "type": "stop" },
                { "type": "face", "dirX": 0, "dirY": -1000 },
                {
                    "type": "castSkill",
                    "skillId": DEFAULT_PLAYER_SKILL_ID,
                    "targetEntityId": TRAINING_TARGET_ENTITY_ID
                }
            ]
        })
        .to_string();

        let parsed = parse_sim_input_payload(&payload).unwrap();

        assert_eq!(parsed.seq, 42);
        assert_eq!(parsed.commands.len(), 4);
        assert_eq!(
            parsed.commands[0],
            ParsedSimCommand::Move {
                dir: QuantizedDir::RIGHT,
                speed_per_second: Some(Fp::from_milli(6000))
            }
        );
        assert_eq!(parsed.commands[1], ParsedSimCommand::Stop);
        assert_eq!(
            parsed.commands[2],
            ParsedSimCommand::Face {
                dir: QuantizedDir::UP
            }
        );
        assert_eq!(
            parsed.commands[3],
            ParsedSimCommand::CastSkill {
                skill_id: SkillId::new(DEFAULT_PLAYER_SKILL_ID),
                target: SkillTarget::Entity(EntityId::new(TRAINING_TARGET_ENTITY_ID))
            }
        );
    }

    #[test]
    fn sim_inputs_use_room_identity_and_server_control_binding() {
        let players = vec!["player-a".to_string()];
        let (_, bindings) = create_minimal_world(&players);
        let record = input(7, "player-a", move_right_payload(99));

        let sim_inputs = sim_inputs_from_records(&[record], &bindings).unwrap();

        assert_eq!(sim_inputs.len(), 1);
        assert_eq!(sim_inputs[0].frame, FrameId::new(7));
        assert_eq!(sim_inputs[0].character_id, "player-a");
        assert_eq!(
            sim_inputs[0].entity_id,
            EntityId::new(PLAYER_ENTITY_ID_BASE)
        );
        assert_eq!(sim_inputs[0].seq, 99);
        assert!(matches!(
            sim_inputs[0].command,
            SimCommand::Move(MoveCommand {
                dir: QuantizedDir::RIGHT,
                ..
            })
        ));
    }

    #[test]
    fn sim_input_payload_rejects_invalid_protocol_and_field_types() {
        let cases = [
            (
                r#"{"version":2,"seq":1,"commands":[]}"#,
                "UNSUPPORTED_SIM_INPUT_VERSION",
            ),
            (
                r#"{"version":1,"seq":1,"commands":[{"type":"move","dirX":1001,"dirY":0}]}"#,
                "SIM_INPUT_DIR_OUT_OF_RANGE",
            ),
            (
                r#"{"version":1,"seq":1,"commands":[{"type":"warp","dirX":1000,"dirY":0}]}"#,
                "INVALID_SIM_INPUT_JSON",
            ),
            (
                r#"{"version":1,"seq":1,"commands":[{"type":"castSkill","skillId":1,"targetEntityId":{"entityId":9000}}]}"#,
                "INVALID_SIM_INPUT_JSON",
            ),
            (
                r#"{"version":1,"seq":"1","commands":[]}"#,
                "INVALID_SIM_INPUT_JSON",
            ),
            (
                r#"{"version":1,"seq":1,"commands":{"type":"stop"}}"#,
                "INVALID_SIM_INPUT_JSON",
            ),
            (
                r#"{"version":1,"seq":1,"commands":[{"type":"face","dirX":"0","dirY":-1000}]}"#,
                "INVALID_SIM_INPUT_JSON",
            ),
            (
                r#"{"version":1,"seq":1,"commands":[{"type":"castSkill","skillId":0}]}"#,
                "SIM_INPUT_SKILL_ID_OUT_OF_RANGE",
            ),
            (
                r#"{"version":1,"seq":1,"commands":[{"type":"castSkill","skillId":1,"targetEntityId":0}]}"#,
                "SIM_INPUT_TARGET_ENTITY_ID_OUT_OF_RANGE",
            ),
        ];

        for (payload, expected) in cases {
            assert_eq!(
                validate_player_input(SIM_INPUT_ACTION, payload),
                Err(expected)
            );
        }
    }

    #[test]
    fn sim_input_payload_rejects_client_authoritative_state_fields() {
        let cases = [
            r#"{"version":1,"seq":1,"entityId":1000,"commands":[{"type":"move","dirX":1000,"dirY":0}]}"#,
            r#"{"version":1,"seq":1,"commands":[{"type":"castSkill","skillId":1,"targetEntityId":9000,"hit":true}]}"#,
            r#"{"version":1,"seq":1,"commands":[{"type":"castSkill","skillId":1,"targetEntityId":9000,"damage":9999}]}"#,
            r#"{"version":1,"seq":1,"commands":[{"type":"castSkill","skillId":1,"targetEntityId":9000,"buffs":[{"id":1}]}]}"#,
            r#"{"version":1,"seq":1,"commands":[{"type":"castSkill","skillId":1,"targetEntityId":9000,"finalState":{"hp":0}}]}"#,
            r#"{"version":1,"seq":1,"commands":[{"type":"stop","stateHash":"0000000000000000"}]}"#,
        ];

        for payload in cases {
            assert_eq!(
                validate_player_input(SIM_INPUT_ACTION, payload),
                Err("INVALID_SIM_INPUT_JSON")
            );
        }
    }

    #[test]
    fn invalid_sim_input_rejects_step_without_advancing_frame() {
        let players = vec!["player-a".to_string()];
        let (mut world, bindings) = create_minimal_world(&players);
        let before_hash = world_hash(&world);
        let record = input(
            1,
            "player-a",
            r#"{"version":2,"seq":1,"commands":[]}"#.to_string(),
        );

        let result = step_world(&mut world, 1, 20, &[record], &bindings);

        assert!(matches!(
            result,
            Err(LockstepSimStepError::Input("UNSUPPORTED_SIM_INPUT_VERSION"))
        ));
        assert_eq!(world.frame, FrameId::new(0));
        assert_eq!(world_hash(&world), before_hash);
    }

    #[test]
    fn sim_step_error_does_not_commit_partial_world_updates() {
        let players = vec!["player-a".to_string()];
        let (mut world, bindings) = create_minimal_world(&players);
        step_world(
            &mut world,
            1,
            20,
            &[input(1, "player-a", move_right_payload(1))],
            &bindings,
        )
        .unwrap();
        let before_world = world.clone();
        let before_hash = world_hash(&world);

        let result = step_world(
            &mut world,
            2,
            20,
            &[input(2, "player-a", unknown_skill_payload(2))],
            &bindings,
        );

        assert!(matches!(result, Err(LockstepSimStepError::Step(_))));
        assert_eq!(world, before_world);
        assert_eq!(world.frame, FrameId::new(1));
        assert_eq!(world_hash(&world), before_hash);
    }

    #[test]
    fn game_server_can_reference_sim_core_minimal_step_api() {
        use sim_core::{FrameId, SimConfig, SimInput, SimWorld, step};

        let players = vec!["player-a".to_string()];
        let (world, _) = create_minimal_world(&players);
        let mut world: SimWorld = world;
        let inputs: Vec<SimInput> = Vec::new();
        let config: SimConfig = default_sim_config(DEFAULT_LOCKSTEP_SIM_TICK_RATE);

        let result = step(&mut world, FrameId::new(1), &inputs, &config).unwrap();

        assert_eq!(world.frame, FrameId::new(1));
        assert_eq!(result.frame, FrameId::new(1));
    }

    #[test]
    fn default_sim_config_contains_movement_skill_and_buff_definitions() {
        let config = default_sim_config(20);

        assert_eq!(config.movement.tick_rate, 20);
        assert_eq!(config.movement.default_speed_per_second, Fp::from_i32(6));
        assert_eq!(config.movement.max_speed_per_second, Fp::from_i32(12));
        assert_eq!(
            config.movement.bounds.min,
            Vec2Fp::new(Fp::from_i32(-100), Fp::from_i32(-100))
        );
        assert_eq!(
            config.movement.bounds.max,
            Vec2Fp::new(Fp::from_i32(100), Fp::from_i32(100))
        );
        assert_eq!(config.combat.skills.iter().count(), 1);
        assert_eq!(
            config
                .combat
                .skills
                .get(SkillId::new(DEFAULT_PLAYER_SKILL_ID))
                .expect("default player skill should exist")
                .cooldown_frames,
            20
        );
        assert_eq!(config.combat.buffs.iter().count(), 1);
        assert_eq!(
            config
                .combat
                .buffs
                .get(BuffId::new(DEFAULT_DEMO_BUFF_ID))
                .expect("default demo buff should exist")
                .duration_frames,
            60
        );
    }

    #[test]
    fn bound_sim_config_hash_is_stable_and_changes_when_config_changes() {
        let first = room_sim_config(7, 20);
        let second = room_sim_config(7, 20);
        let different_tick_rate = room_sim_config(7, 30);
        let mut different_skill = default_sim_config(20);
        different_skill
            .combat
            .skills
            .skills
            .first_mut()
            .expect("default skill should exist")
            .cooldown_frames += 1;

        assert_eq!(first, second);
        assert_eq!(first.config_version, 7);
        assert_eq!(first.sim_schema_version, sim_core::SIM_CORE_SCHEMA_VERSION);
        assert_eq!(first.config_hash.len(), 64);
        assert_eq!(first.config_hash, second.config_hash);
        assert_ne!(first.config_hash, different_tick_rate.config_hash);
        assert_ne!(first.config_hash, sim_config_hash_hex(&different_skill));
    }

    #[test]
    fn snapshot_and_frame_envelope_bind_config_metadata() {
        let players = vec!["player-a".to_string()];
        let (mut world, bindings) = create_minimal_world(&players);
        let config = room_sim_config(42, 20);

        let snapshot =
            create_initial_snapshot_with_config("room-lockstep", &config, &world, &bindings);
        assert_eq!(snapshot.config_version, 42);
        assert_eq!(snapshot.config_hash, config.config_hash);
        assert_eq!(
            snapshot.sim_schema_version,
            sim_core::SIM_CORE_SCHEMA_VERSION
        );

        let input = input(1, "player-a", cast_training_target_payload(1));
        let result = step_world_with_config(
            &mut world,
            1,
            &config,
            std::slice::from_ref(&input),
            &bindings,
        )
        .unwrap();
        let envelope =
            create_frame_envelope_with_config("room-lockstep", &config, &world, &[input], &result);

        assert_eq!(envelope.config_version, snapshot.config_version);
        assert_eq!(envelope.config_hash, snapshot.config_hash);
        assert_eq!(envelope.sim_schema_version, snapshot.sim_schema_version);
    }

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
        assert!(
            world
                .entity(EntityId::new(TRAINING_TARGET_ENTITY_ID))
                .is_some()
        );

        let result = step_world(&mut world, 1, 20, &[], &bindings).unwrap();
        assert_eq!(world.frame, FrameId::new(1));
        assert_eq!(result.frame, FrameId::new(1));
    }

    #[test]
    fn minimal_world_builds_single_player_entity_with_initial_state() {
        let players = vec!["player-a".to_string()];
        let (world, bindings) = create_minimal_world(&players);

        assert_eq!(world.schema_version, sim_core::SIM_CORE_SCHEMA_VERSION);
        assert_eq!(world.frame, FrameId::new(0));
        assert_eq!(world.rng.seed, 0);
        assert_eq!(world.entities.len(), 2);
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings["player-a"], EntityId::new(PLAYER_ENTITY_ID_BASE));

        let player = world
            .entity(EntityId::new(PLAYER_ENTITY_ID_BASE))
            .expect("single player entity should exist");
        assert_eq!(player.kind, EntityKind::Player);
        assert_eq!(player.owner_character_id.as_deref(), Some("player-a"));
        assert_eq!(player.team_id, TeamId::new(1));
        assert_eq!(player.transform.pos, Vec2Fp::new(Fp::ZERO, Fp::ZERO));
        assert_eq!(player.transform.facing, QuantizedDir::RIGHT);
        assert_eq!(player.transform.radius, Fp::from_milli(500));
        assert_eq!(player.movement.mode, MovementMode::Idle);
        assert_eq!(player.movement.move_dir, QuantizedDir::ZERO);
        assert_eq!(player.movement.speed_per_second, Fp::ZERO);
        assert_eq!(player.combat.hp, 100);
        assert_eq!(player.combat.max_hp, 100);
        assert_eq!(player.combat.skill_slots.len(), 1);
        assert_eq!(
            player.combat.skill_slots[0].skill_id,
            SkillId::new(DEFAULT_PLAYER_SKILL_ID)
        );
        assert!(player.alive);
    }

    #[test]
    fn minimal_world_builds_training_target_for_movement_and_combat_scenarios() {
        let players = vec!["player-a".to_string()];
        let (world, _) = create_minimal_world(&players);

        let target = world
            .entity(EntityId::new(TRAINING_TARGET_ENTITY_ID))
            .expect("training target should exist");
        assert_eq!(target.kind, EntityKind::Monster);
        assert_eq!(target.owner_character_id, None);
        assert_eq!(target.team_id, TeamId::new(90));
        assert_eq!(target.transform.pos, Vec2Fp::new(Fp::from_i32(8), Fp::ZERO));
        assert_eq!(target.transform.facing, QuantizedDir::LEFT);
        assert_eq!(target.movement, MovementState::default());
        assert_eq!(target.combat.hp, 150);
        assert_eq!(target.combat.max_hp, 150);
        assert_eq!(target.combat.defense, 1);
        assert!(target.alive);
    }

    #[test]
    fn minimal_world_entity_ids_and_initial_hash_are_deterministic_for_same_players() {
        let players = vec!["player-b".to_string(), "player-a".to_string()];
        let (world_a, bindings_a) = create_minimal_world(&players);
        let (world_b, bindings_b) = create_minimal_world(&players);

        assert_eq!(world_a, world_b);
        assert_eq!(world_hash(&world_a), world_hash(&world_b));
        assert_eq!(bindings_a, bindings_b);
        assert_eq!(bindings_a["player-b"], EntityId::new(PLAYER_ENTITY_ID_BASE));
        assert_eq!(
            bindings_a["player-a"],
            EntityId::new(PLAYER_ENTITY_ID_BASE + 1)
        );
        assert_eq!(
            world_a
                .entities_sorted_by_id()
                .iter()
                .map(|entity| entity.id.raw())
                .collect::<Vec<_>>(),
            vec![
                PLAYER_ENTITY_ID_BASE,
                PLAYER_ENTITY_ID_BASE + 1,
                TRAINING_TARGET_ENTITY_ID
            ]
        );

        let snapshot_a = create_initial_snapshot("room-lockstep", 20, &world_a, &bindings_a);
        let snapshot_b = create_initial_snapshot("room-lockstep", 20, &world_b, &bindings_b);
        assert_eq!(snapshot_a, snapshot_b);
        assert_eq!(snapshot_a.start_frame, 0);
        assert_eq!(snapshot_a.rng_seed, 0);
        assert_eq!(
            snapshot_a.state_hash,
            sim_hash_envelope(world_hash(&world_a))
        );
    }

    #[test]
    fn initial_snapshot_restores_and_continues_with_same_hash() {
        let players = vec!["player-a".to_string()];
        let (mut continuous_world, continuous_bindings) = create_minimal_world(&players);
        let (mut restored_source_world, restored_source_bindings) = create_minimal_world(&players);

        let first_input = input(1, "player-a", move_right_payload(1));
        step_world(
            &mut continuous_world,
            1,
            20,
            std::slice::from_ref(&first_input),
            &continuous_bindings,
        )
        .unwrap();
        step_world(
            &mut restored_source_world,
            1,
            20,
            &[first_input],
            &restored_source_bindings,
        )
        .unwrap();

        let snapshot = create_initial_snapshot(
            "room-lockstep",
            20,
            &restored_source_world,
            &restored_source_bindings,
        );
        assert_eq!(snapshot.schema, SIM_INITIAL_SNAPSHOT_SCHEMA);
        assert_eq!(snapshot.schema_version, SIM_DOWNLINK_SCHEMA_VERSION);
        assert_eq!(snapshot.room_id, "room-lockstep");
        assert_eq!(snapshot.start_frame, 1);
        assert_eq!(snapshot.tick_rate, 20);
        assert_eq!(snapshot.rng_seed, restored_source_world.rng.seed);
        assert_eq!(snapshot.entities.len(), 2);
        assert_eq!(snapshot.control_bindings[0].character_id, "player-a");
        assert_eq!(
            snapshot.control_bindings[0].entity_id,
            PLAYER_ENTITY_ID_BASE
        );
        assert_eq!(snapshot.state_hash.hex.len(), 16);

        let (mut restored_world, restored_bindings) = restore_initial_snapshot(&snapshot).unwrap();
        assert_eq!(world_hash(&restored_world), world_hash(&continuous_world));

        let second_input = input(2, "player-a", cast_training_target_payload(2));
        let continuous_result = step_world(
            &mut continuous_world,
            2,
            20,
            std::slice::from_ref(&second_input),
            &continuous_bindings,
        )
        .unwrap();
        let restored_result = step_world(
            &mut restored_world,
            2,
            20,
            &[second_input],
            &restored_bindings,
        )
        .unwrap();

        assert_eq!(restored_result.state_hash, continuous_result.state_hash);
        assert_eq!(world_hash(&restored_world), world_hash(&continuous_world));
    }

    #[test]
    fn frame_envelope_contains_hash_events_and_debug_summary() {
        let players = vec!["player-a".to_string()];
        let (mut world, bindings) = create_minimal_world(&players);
        let input = input(1, "player-a", cast_training_target_payload(1));
        let result =
            step_world(&mut world, 1, 20, std::slice::from_ref(&input), &bindings).unwrap();

        let envelope = create_frame_envelope("room-lockstep", 20, &world, &[input], &result);

        assert_eq!(envelope.schema, SIM_FRAME_ENVELOPE_SCHEMA);
        assert_eq!(envelope.schema_version, SIM_DOWNLINK_SCHEMA_VERSION);
        assert_eq!(envelope.room_id, "room-lockstep");
        assert_eq!(envelope.frame, 1);
        assert_eq!(envelope.state_hash, sim_hash_envelope(result.state_hash));
        assert_eq!(envelope.state_hash.hex.len(), 16);
        assert_eq!(envelope.event_count, 2);
        assert_eq!(envelope.events.len(), 2);
        assert_eq!(envelope.event_summaries.len(), 2);
        assert_eq!(envelope.event_summaries[0].schema_version, 1);
        assert_eq!(
            envelope.event_summaries[0].kind,
            SimFrameEventKind::SkillCast
        );
        assert_eq!(
            envelope.event_summaries[0].source_entity_id,
            PLAYER_ENTITY_ID_BASE
        );
        assert_eq!(
            envelope.event_summaries[0].target_entity_id,
            Some(TRAINING_TARGET_ENTITY_ID)
        );
        assert_eq!(
            envelope.event_summaries[0].skill_id,
            Some(DEFAULT_PLAYER_SKILL_ID)
        );
        assert_eq!(envelope.event_summaries[0].buff_id, None);
        assert_eq!(envelope.event_summaries[0].amount, 1);
        assert_eq!(envelope.event_summaries[1].kind, SimFrameEventKind::Damage);
        assert_eq!(envelope.event_summaries[1].amount, 14);
        assert_eq!(envelope.input_sources.len(), 1);
        assert_eq!(envelope.input_sources[0].frame, 1);
        assert_eq!(envelope.input_sources[0].character_id, "player-a");
        assert_eq!(envelope.input_sources[0].source, SimFrameInputSource::Real);
        assert_eq!(envelope.input_sources[0].action, SIM_INPUT_ACTION);
        assert_eq!(envelope.debug_summary.input_count, 1);
        assert_eq!(envelope.debug_summary.real_input_count, 1);
        assert_eq!(envelope.debug_summary.synthetic_input_count, 0);
        assert_eq!(envelope.debug_summary.synthesized_empty_input_count, 0);
        assert_eq!(
            envelope.debug_summary.synthesized_repeat_last_input_count,
            0
        );
        assert_eq!(envelope.debug_summary.event_count, 2);
        assert_eq!(envelope.debug_summary.entity_count, 2);
        assert_eq!(envelope.debug_summary.player_entity_count, 1);
        assert_eq!(envelope.debug_state.schema_version, 1);
        assert_eq!(envelope.debug_state.entities.len(), 2);
        assert_eq!(
            envelope.debug_state.entities[0].entity_id,
            PLAYER_ENTITY_ID_BASE
        );
        assert_eq!(envelope.debug_state.entities[0].x_raw, 0);
        assert_eq!(envelope.debug_state.entities[0].hp, 100);
        assert!(envelope.debug_state.entities[0].alive);
        assert_eq!(
            envelope.debug_state.entities[1].entity_id,
            TRAINING_TARGET_ENTITY_ID
        );
        assert_eq!(envelope.debug_state.entities[1].x_raw, 8000);
        assert_eq!(envelope.debug_state.entities[1].hp, 136);

        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["eventCount"], 2);
        assert_eq!(json["eventSummaries"][0]["kind"], "skillCast");
        assert_eq!(json["eventSummaries"][1]["kind"], "damage");
        assert_eq!(json["debugState"]["entities"][0]["xRaw"], 0);
        assert!(json.get("snapshot").is_none());
        assert!(json["debugState"]["entities"][0].get("combat").is_none());
    }

    #[test]
    fn event_summaries_preserve_stable_fields_and_sort_order() {
        let frame = FrameId::new(7);
        let source = EntityId::new(1001);
        let target = EntityId::new(9001);
        let skill = SkillId::new(11);
        let buff = BuffId::new(22);
        let events = vec![
            sim_core::SimEvent::EntityDied {
                frame,
                source_entity: source,
                target_entity: target,
                skill_id: Some(skill),
                buff_id: None,
                value: 0,
                sequence: 7,
            },
            sim_core::SimEvent::HealApplied {
                frame,
                source_entity: source,
                target_entity: target,
                skill_id: None,
                buff_id: Some(buff),
                value: 5,
                sequence: 5,
            },
            sim_core::SimEvent::DamageApplied {
                frame,
                source_entity: source,
                target_entity: target,
                skill_id: Some(skill),
                buff_id: None,
                value: 17,
                sequence: 4,
            },
            sim_core::SimEvent::BuffTick {
                frame,
                source_entity: source,
                target_entity: target,
                buff_id: buff,
                value: 2,
                sequence: 3,
            },
            sim_core::SimEvent::BuffExpired {
                frame,
                source_entity: source,
                target_entity: target,
                buff_id: buff,
                value: 2,
                sequence: 6,
            },
            sim_core::SimEvent::BuffApplied {
                frame,
                source_entity: source,
                target_entity: target,
                buff_id: buff,
                value: 2,
                sequence: 2,
            },
            sim_core::SimEvent::SkillCast {
                frame,
                source_entity: source,
                target_entity: Some(target),
                skill_id: skill,
                value: 1,
                sequence: 1,
            },
        ];

        let summaries = sim_event_summaries(&events);

        assert_eq!(
            summaries
                .iter()
                .map(|summary| summary.kind)
                .collect::<Vec<_>>(),
            vec![
                SimFrameEventKind::SkillCast,
                SimFrameEventKind::BuffApplied,
                SimFrameEventKind::BuffTick,
                SimFrameEventKind::Damage,
                SimFrameEventKind::Heal,
                SimFrameEventKind::BuffExpired,
                SimFrameEventKind::Death,
            ]
        );
        assert!(summaries.iter().all(|summary| summary.schema_version == 1));
        assert!(summaries.iter().all(|summary| summary.frame == 7));
        assert!(
            summaries
                .iter()
                .all(|summary| summary.source_entity_id == source.raw())
        );
        assert!(
            summaries
                .iter()
                .all(|summary| summary.target_entity_id == Some(target.raw()))
        );
        assert_eq!(summaries[0].skill_id, Some(skill.raw()));
        assert_eq!(summaries[0].buff_id, None);
        assert_eq!(summaries[0].amount, 1);
        assert_eq!(summaries[1].skill_id, None);
        assert_eq!(summaries[1].buff_id, Some(buff.raw()));
        assert_eq!(summaries[3].skill_id, Some(skill.raw()));
        assert_eq!(summaries[3].amount, 17);
        assert_eq!(summaries[4].skill_id, None);
        assert_eq!(summaries[4].buff_id, Some(buff.raw()));
        assert_eq!(summaries[4].amount, 5);
        assert_eq!(summaries[6].kind, SimFrameEventKind::Death);
        assert_eq!(summaries[6].skill_id, Some(skill.raw()));

        let json = serde_json::to_value(&summaries).unwrap();
        assert_eq!(json[0]["schemaVersion"], 1);
        assert_eq!(json[0]["kind"], "skillCast");
        assert_eq!(json[3]["kind"], "damage");
        assert_eq!(json[4]["kind"], "heal");
        assert_eq!(json[6]["kind"], "death");
        assert_eq!(json[3]["sourceEntityId"], source.raw());
        assert_eq!(json[3]["targetEntityId"], target.raw());
        assert_eq!(json[3]["skillId"], skill.raw());
        assert_eq!(json[3]["buffId"], serde_json::Value::Null);
        assert_eq!(json[3]["amount"], 17);
    }

    #[test]
    fn frame_envelope_debug_state_is_lightweight_and_bounded() {
        let players = (0..40)
            .map(|index| format!("player-{index}"))
            .collect::<Vec<_>>();
        let (mut world, bindings) = create_minimal_world(&players);
        let result = step_world(&mut world, 1, 20, &[], &bindings).unwrap();

        let envelope = create_frame_envelope("room-lockstep", 20, &world, &[], &result);

        assert_eq!(envelope.debug_summary.entity_count, 41);
        assert_eq!(
            envelope.debug_state.entities.len(),
            SIM_FRAME_DEBUG_STATE_ENTITY_LIMIT
        );
        assert_eq!(envelope.debug_state.entities[0].entity_id, 1000);
        assert_eq!(envelope.debug_state.entities[31].entity_id, 1031);

        let json = serde_json::to_value(&envelope).unwrap();
        let first_entity = &json["debugState"]["entities"][0];
        assert!(first_entity.get("id").is_none());
        assert!(first_entity.get("kind").is_none());
        assert!(first_entity.get("transform").is_none());
        assert!(first_entity.get("movement").is_none());
        assert!(first_entity.get("combat").is_none());
        assert!(first_entity.get("buffs").is_none());
        assert!(json.get("snapshot").is_none());
    }

    #[test]
    fn frame_envelope_distinguishes_real_empty_and_repeat_last_inputs() {
        let players = vec!["player-a".to_string(), "player-b".to_string()];
        let (mut world, bindings) = create_minimal_world(&players);
        let inputs = vec![
            input(1, "player-a", move_right_payload(1)),
            synthetic_empty_input(1, "player-b"),
            synthetic_repeat_last_input(1, "player-c", move_right_payload(1)),
        ];
        let result = step_world(&mut world, 1, 20, &inputs, &bindings).unwrap();

        let envelope = create_frame_envelope("room-lockstep", 20, &world, &inputs, &result);

        assert_eq!(envelope.input_sources.len(), 3);
        assert_eq!(envelope.input_sources[0].source, SimFrameInputSource::Real);
        assert_eq!(
            envelope.input_sources[1].source,
            SimFrameInputSource::SynthesizedEmpty
        );
        assert_eq!(
            envelope.input_sources[2].source,
            SimFrameInputSource::SynthesizedRepeatLast
        );
        assert_eq!(envelope.debug_summary.input_count, 3);
        assert_eq!(envelope.debug_summary.real_input_count, 1);
        assert_eq!(envelope.debug_summary.synthetic_input_count, 2);
        assert_eq!(envelope.debug_summary.synthesized_empty_input_count, 1);
        assert_eq!(
            envelope.debug_summary.synthesized_repeat_last_input_count,
            1
        );
    }

    #[test]
    fn synthesized_empty_advances_movement_without_recasting_skill() {
        let players = vec!["player-a".to_string()];
        let (mut world, bindings) = create_minimal_world(&players);

        let move_result = step_world(
            &mut world,
            1,
            20,
            &[input(1, "player-a", move_right_payload(1))],
            &bindings,
        )
        .unwrap();
        assert!(move_result.events.is_empty());
        assert_eq!(
            world
                .entity(EntityId::new(PLAYER_ENTITY_ID_BASE))
                .unwrap()
                .transform
                .pos
                .x
                .raw(),
            300
        );

        let cast_result = step_world(
            &mut world,
            2,
            20,
            &[input(2, "player-a", cast_training_target_payload(2))],
            &bindings,
        )
        .unwrap();
        assert_eq!(cast_result.events.len(), 2);
        assert_eq!(
            world
                .entity(EntityId::new(TRAINING_TARGET_ENTITY_ID))
                .unwrap()
                .combat
                .hp,
            136
        );

        let empty_input = synthetic_empty_input(3, "player-a");
        let empty_result = step_world(
            &mut world,
            3,
            20,
            std::slice::from_ref(&empty_input),
            &bindings,
        )
        .unwrap();
        let envelope =
            create_frame_envelope("room-lockstep", 20, &world, &[empty_input], &empty_result);

        assert!(empty_result.events.is_empty());
        assert_eq!(
            world
                .entity(EntityId::new(TRAINING_TARGET_ENTITY_ID))
                .unwrap()
                .combat
                .hp,
            136
        );
        assert_eq!(
            world
                .entity(EntityId::new(PLAYER_ENTITY_ID_BASE))
                .unwrap()
                .transform
                .pos
                .x
                .raw(),
            900
        );
        assert_eq!(envelope.debug_summary.synthesized_empty_input_count, 1);
        assert_eq!(
            envelope.input_sources[0].source,
            SimFrameInputSource::SynthesizedEmpty
        );
        assert_eq!(empty_result.state_hash, world_hash(&world));
    }

    #[test]
    fn stop_input_updates_authoritative_world_and_frame_envelope_hash() {
        let players = vec!["player-a".to_string()];
        let (mut world, bindings) = create_minimal_world(&players);

        step_world(
            &mut world,
            1,
            20,
            &[input(1, "player-a", move_right_payload(1))],
            &bindings,
        )
        .unwrap();
        step_world(&mut world, 2, 20, &[], &bindings).unwrap();
        let moving_x = world
            .entity(EntityId::new(PLAYER_ENTITY_ID_BASE))
            .unwrap()
            .transform
            .pos
            .x
            .raw();
        assert_eq!(moving_x, 600);

        let stop_input = input(3, "player-a", stop_payload(3));
        let result = step_world(
            &mut world,
            3,
            20,
            std::slice::from_ref(&stop_input),
            &bindings,
        )
        .unwrap();
        let envelope = create_frame_envelope("room-lockstep", 20, &world, &[stop_input], &result);

        let player = world.entity(EntityId::new(PLAYER_ENTITY_ID_BASE)).unwrap();
        assert_eq!(world.frame, FrameId::new(3));
        assert_eq!(player.transform.pos.x.raw(), moving_x);
        assert_eq!(player.movement.mode, MovementMode::Idle);
        assert_eq!(player.movement.move_dir, QuantizedDir::ZERO);
        assert_eq!(player.movement.speed_per_second, Fp::ZERO);
        assert_eq!(result.frame, FrameId::new(3));
        assert_eq!(result.state_hash, world_hash(&world));
        assert_eq!(envelope.frame, 3);
        assert_eq!(envelope.state_hash, sim_hash_envelope(result.state_hash));
        assert_eq!(envelope.debug_summary.real_input_count, 1);
        assert_eq!(envelope.events.len(), result.events.len());
    }

    #[test]
    fn same_input_sequence_replays_to_identical_hashes() {
        let players = vec!["player-a".to_string()];
        let (mut first_world, first_bindings) = create_minimal_world(&players);
        let (mut second_world, second_bindings) = create_minimal_world(&players);
        let frames = vec![
            vec![input(1, "player-a", move_right_payload(1))],
            vec![synthetic_repeat_last_input(
                2,
                "player-a",
                move_right_payload(1),
            )],
            vec![input(3, "player-a", stop_payload(3))],
            vec![input(4, "player-a", cast_training_target_payload(4))],
            vec![synthetic_empty_input(5, "player-a")],
        ];

        let mut first_hashes = Vec::new();
        let mut second_hashes = Vec::new();
        for (index, frame_inputs) in frames.iter().enumerate() {
            let frame_id = index as u32 + 1;
            let first_result = step_world(
                &mut first_world,
                frame_id,
                20,
                frame_inputs,
                &first_bindings,
            )
            .unwrap();
            let second_result = step_world(
                &mut second_world,
                frame_id,
                20,
                frame_inputs,
                &second_bindings,
            )
            .unwrap();

            first_hashes.push(first_result.state_hash);
            second_hashes.push(second_result.state_hash);
            assert_eq!(first_result.frame, FrameId::new(frame_id));
            assert_eq!(first_result.events, second_result.events);
        }

        assert_eq!(first_hashes, second_hashes);
        assert_eq!(world_hash(&first_world), world_hash(&second_world));
    }

    #[test]
    fn restore_then_continue_matches_continuous_world_and_snapshot_is_read_only() {
        let players = vec!["player-a".to_string()];
        let (mut continuous_world, continuous_bindings) = create_minimal_world(&players);
        let (mut snapshot_world, snapshot_bindings) = create_minimal_world(&players);

        for frame in 1..=2 {
            let input = input(frame, "player-a", move_right_payload(frame));
            step_world(
                &mut continuous_world,
                frame,
                20,
                std::slice::from_ref(&input),
                &continuous_bindings,
            )
            .unwrap();
            step_world(&mut snapshot_world, frame, 20, &[input], &snapshot_bindings).unwrap();
        }

        let snapshot =
            create_initial_snapshot("room-lockstep", 20, &snapshot_world, &snapshot_bindings);
        let snapshot_hash = world_hash(&snapshot_world);
        let observer_snapshot = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(world_hash(&snapshot_world), snapshot_hash);
        assert_eq!(
            observer_snapshot["stateHash"]["hex"].as_str(),
            Some(snapshot.state_hash.hex.as_str())
        );

        let (mut restored_world, restored_bindings) = restore_initial_snapshot(&snapshot).unwrap();
        assert_eq!(world_hash(&restored_world), world_hash(&continuous_world));

        let frame_3 = synthetic_empty_input(3, "player-a");
        let continuous_result = step_world(
            &mut continuous_world,
            3,
            20,
            std::slice::from_ref(&frame_3),
            &continuous_bindings,
        )
        .unwrap();
        let restored_result =
            step_world(&mut restored_world, 3, 20, &[frame_3], &restored_bindings).unwrap();

        assert_eq!(restored_result.state_hash, continuous_result.state_hash);
        assert_eq!(world_hash(&restored_world), world_hash(&continuous_world));
        assert_eq!(restored_result.frame.raw(), snapshot.start_frame + 1);
    }
}
