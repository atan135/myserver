use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::SCHEMA_VERSION;
use crate::calibration::CalibrationThresholds;
use crate::game_kcp::ReconnectPolicy;
use crate::gameplay::{GameplayProfilePlan, PlayerProfile};
use crate::step::ScenarioStep;

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not read configuration: {0}")]
    Read(#[from] std::io::Error),
    #[error("configuration JSON is invalid or contains an unknown field: {0}")]
    Json(#[from] serde_json::Error),
    #[error("configuration rejected: {0}")]
    Rejected(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LoadTestConfig {
    pub schema_version: u32,
    pub environment: EnvironmentProfile,
    pub targets: PlayerTargets,
    pub budget: HardBudget,
    pub scenario: Scenario,
    pub reports_root: String,
    pub prepare_reports_root: String,
    #[serde(default)]
    pub stop_file: Option<String>,
    #[serde(default)]
    pub deadline_unix_ms: Option<u64>,
    #[serde(default = "default_graceful_shutdown_ms")]
    pub graceful_shutdown_ms: u64,
    #[serde(default)]
    pub account_prepare: AccountPrepareConfig,
    #[serde(default)]
    pub calibration: CalibrationThresholds,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentProfile {
    pub name: String,
    pub kind: EnvironmentKind,
    #[serde(default)]
    pub approval_reference: Option<String>,
    #[serde(default)]
    pub allowed_hosts: BTreeSet<String>,
    #[serde(default)]
    pub allowed_ips: BTreeSet<IpAddr>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentKind {
    Local,
    Test,
    Staging,
    Production,
}

impl EnvironmentKind {
    pub fn is_remote(self) -> bool {
        !matches!(self, Self::Local)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlayerTargets {
    pub auth_http: String,
    pub game_proxy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HardBudget {
    pub max_virtual_players: u32,
    pub max_login_qps: f64,
    pub max_new_connections_per_second: f64,
    pub max_business_messages_per_second: f64,
    pub max_messages_per_connection_per_second: f64,
    pub max_duration_secs: u64,
    pub max_total_operations: u64,
    pub max_error_rate: f64,
    pub max_connection_failure_rate: f64,
    pub max_p99_ms: u64,
    pub max_data_writes: u64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BudgetOverride {
    #[serde(default)]
    pub max_virtual_players: Option<u32>,
    #[serde(default)]
    pub max_login_qps: Option<f64>,
    #[serde(default)]
    pub max_new_connections_per_second: Option<f64>,
    #[serde(default)]
    pub max_business_messages_per_second: Option<f64>,
    #[serde(default)]
    pub max_messages_per_connection_per_second: Option<f64>,
    #[serde(default)]
    pub max_duration_secs: Option<u64>,
    #[serde(default)]
    pub max_total_operations: Option<u64>,
    #[serde(default)]
    pub max_error_rate: Option<f64>,
    #[serde(default)]
    pub max_connection_failure_rate: Option<f64>,
    #[serde(default)]
    pub max_p99_ms: Option<u64>,
    #[serde(default)]
    pub max_data_writes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    pub name: String,
    pub load: LoadModel,
    #[serde(default)]
    pub steps: Vec<ScenarioStep>,
    #[serde(default)]
    pub writes_data: bool,
    #[serde(default)]
    pub auth: Option<AuthScenario>,
    #[serde(default)]
    pub reconnect_burst: Option<ReconnectBurstScenario>,
    /// Absent by default so the guarded KCP smoke remains auth/heartbeat-only.
    #[serde(default)]
    pub live_gameplay: Option<LiveGameplayScenario>,
}

pub const MAX_LIVE_GAMEPLAY_FRAME_INPUTS: u32 = 8;
pub const MAX_LIVE_GAMEPLAY_SCENARIO_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LiveGameplayScenario {
    /// A pre-provisioned, explicitly approved room. The tool never discovers
    /// or guesses a room identifier.
    pub room_id: String,
    /// A production policy is supplied by the profile; no binary default.
    pub policy_id: String,
    pub profile: PlayerProfile,
    pub lockstep_scenario_json: String,
    pub max_frame_inputs: u32,
    /// One deliberate KCP reconnect plus ticket-bound room reconnect.
    #[serde(default)]
    pub reconnect: Option<LiveGameplayReconnect>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LiveGameplayReconnect {
    /// Cursor from an explicitly approved prior CharacterPushMeta; zero is
    /// valid only when supplied intentionally by this opt-in block.
    pub last_character_push_sequence: u64,
    /// Exactly one KCP reconnect is allowed for the opt-in live profile.
    pub reconnect_policy: ReconnectPolicyConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReconnectBurstScenario {
    pub virtual_players: u32,
    pub reconnect_attempts_per_player: u32,
    pub reconnect_policy: ReconnectPolicyConfig,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReconnectPolicyConfig {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    #[serde(default)]
    pub max_jitter_ms: u64,
}

impl From<ReconnectPolicyConfig> for ReconnectPolicy {
    fn from(value: ReconnectPolicyConfig) -> Self {
        Self {
            max_attempts: value.max_attempts,
            base_delay_ms: value.base_delay_ms,
            max_delay_ms: value.max_delay_ms,
            max_jitter_ms: value.max_jitter_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AccountPrepareConfig {
    #[serde(default = "default_account_batch")]
    pub batch: String,
    #[serde(default)]
    pub account_count: Option<u32>,
    #[serde(default = "default_character_name_prefix")]
    pub character_name_prefix: String,
}

impl Default for AccountPrepareConfig {
    fn default() -> Self {
        Self {
            batch: default_account_batch(),
            account_count: None,
            character_name_prefix: default_character_name_prefix(),
        }
    }
}

fn default_account_batch() -> String {
    "default".into()
}

fn default_character_name_prefix() -> String {
    "loadtest".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AuthScenario {
    pub operations: Vec<AuthOperation>,
    #[serde(default)]
    pub allow_same_account_concurrency: bool,
    #[serde(default)]
    pub same_account_session_effect: Option<SessionEffect>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthOperation {
    Login,
    Me,
    ListCharacters,
    CreateCharacter,
    SelectCharacter,
    IssueTicket,
    Logout,
    DuplicateLogin,
    FailedLogin,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionEffect {
    SessionKick,
    SessionOverwrite,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum LoadModel {
    FixedConcurrency {
        virtual_players: u32,
        duration_secs: u64,
    },
    ArrivalRate {
        arrivals_per_second: f64,
        duration_secs: u64,
    },
    Staged {
        stages: Vec<LoadStage>,
    },
    Burst {
        burst_size: u32,
        every_secs: u64,
        duration_secs: u64,
    },
}

pub const MAX_GRACEFUL_SHUTDOWN_MS: u64 = 60_000;

fn default_graceful_shutdown_ms() -> u64 {
    5_000
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LoadStage {
    pub name: String,
    pub virtual_players: u32,
    pub duration_secs: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunAccess<'a> {
    pub allow_remote: bool,
    pub confirmation: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        let (scheme, authority) = input
            .split_once("://")
            .ok_or_else(|| ConfigError::Rejected(format!("target must include scheme: {input}")))?;
        if scheme.is_empty()
            || authority.is_empty()
            || authority.contains('/')
            || authority.contains('@')
            || authority.contains('?')
            || authority.contains('#')
        {
            return Err(ConfigError::Rejected(format!(
                "target must be a bare host:port endpoint: {input}"
            )));
        }
        let (host, port) = authority.rsplit_once(':').ok_or_else(|| {
            ConfigError::Rejected(format!("target must include an explicit port: {input}"))
        })?;
        if host.is_empty() || host.contains(':') || host.contains(char::is_whitespace) {
            return Err(ConfigError::Rejected(format!(
                "target host is invalid: {input}"
            )));
        }
        let port = port
            .parse::<u16>()
            .map_err(|_| ConfigError::Rejected(format!("target port is invalid: {input}")))?;
        if port == 0 {
            return Err(ConfigError::Rejected("target port must not be zero".into()));
        }
        Ok(Self {
            scheme: scheme.to_ascii_lowercase(),
            host: host.to_ascii_lowercase(),
            port,
        })
    }

    pub fn is_loopback(&self) -> bool {
        self.host == "localhost" || self.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    }

    pub fn safe_summary(&self) -> String {
        let digest = Sha256::digest(format!("{}://{}:{}", self.scheme, self.host, self.port));
        format!("{}://target-{:x}:{}", self.scheme, digest, self.port)
    }
}

pub fn load_config(
    path: &Path,
    private_path: Option<&Path>,
) -> Result<LoadTestConfig, ConfigError> {
    let config: LoadTestConfig = serde_json::from_slice(&fs::read(path)?)?;
    if let Some(path) = private_path {
        validate_private_config(path)?;
    }
    config.validate_structural()?;
    Ok(config)
}

impl LoadTestConfig {
    pub fn validate_structural(&self) -> Result<(), ConfigError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ConfigError::Rejected(format!(
                "schema_version must be {SCHEMA_VERSION}"
            )));
        }
        if self.environment.name.trim().is_empty() {
            return Err(ConfigError::Rejected(
                "environment name must not be empty".into(),
            ));
        }
        if self.reports_root.trim().is_empty() || self.prepare_reports_root.trim().is_empty() {
            return Err(ConfigError::Rejected(
                "run and account-prepare reports roots are required".into(),
            ));
        }
        if self.reports_root == self.prepare_reports_root {
            return Err(ConfigError::Rejected(
                "run and account-prepare results must use separate roots".into(),
            ));
        }
        self.validate_targets()?;
        self.validate_budget()?;
        self.validate_scenario()?;
        self.validate_account_prepare()?;
        self.calibration.validate().map_err(ConfigError::Rejected)?;
        if self.graceful_shutdown_ms == 0 || self.graceful_shutdown_ms > MAX_GRACEFUL_SHUTDOWN_MS {
            return Err(ConfigError::Rejected(format!(
                "graceful_shutdown_ms must be within 1..={MAX_GRACEFUL_SHUTDOWN_MS}"
            )));
        }
        if self.scenario.writes_data && self.budget.max_data_writes == 0 {
            return Err(ConfigError::Rejected(
                "write scenario has zero data-write budget".into(),
            ));
        }
        Ok(())
    }

    pub fn validate_access(&self, access: RunAccess<'_>) -> Result<(), ConfigError> {
        let targets = self.parsed_targets()?;
        if !self.environment.kind.is_remote() {
            if !targets.iter().all(Endpoint::is_loopback) {
                return Err(ConfigError::Rejected(
                    "local profile accepts loopback targets only".into(),
                ));
            }
            return Ok(());
        }
        if !access.allow_remote {
            return Err(ConfigError::Rejected(
                "remote target rejected without --allow-remote".into(),
            ));
        }
        if self
            .environment
            .approval_reference
            .as_deref()
            .is_none_or(|reference| reference.trim().is_empty())
        {
            return Err(ConfigError::Rejected(
                "remote profile requires a non-empty approval_reference".into(),
            ));
        }
        if access.confirmation != Some(self.environment.name.as_str()) {
            return Err(ConfigError::Rejected(
                "remote run requires --confirm <environment name>".into(),
            ));
        }
        if self.environment.allowed_hosts.is_empty() || self.environment.allowed_ips.is_empty() {
            return Err(ConfigError::Rejected(
                "remote profile requires host and IP allowlists".into(),
            ));
        }
        self.revalidate_targets()?;
        Ok(())
    }

    pub fn revalidate_targets(&self) -> Result<(), ConfigError> {
        for endpoint in self.parsed_targets()? {
            if !self.environment.allowed_hosts.contains(&endpoint.host) {
                return Err(ConfigError::Rejected(format!(
                    "target host {} is outside profile allowlist",
                    endpoint.host
                )));
            }
            let resolved: Vec<SocketAddr> = (endpoint.host.as_str(), endpoint.port)
                .to_socket_addrs()
                .map_err(|error| {
                    ConfigError::Rejected(format!("could not resolve {}: {error}", endpoint.host))
                })?
                .collect();
            if resolved.is_empty() {
                return Err(ConfigError::Rejected(format!(
                    "target {} resolved to no addresses",
                    endpoint.host
                )));
            }
            if resolved
                .iter()
                .any(|address| !self.environment.allowed_ips.contains(&address.ip()))
            {
                return Err(ConfigError::Rejected(format!(
                    "target {} resolved outside IP allowlist",
                    endpoint.host
                )));
            }
        }
        Ok(())
    }

    pub fn effective_budget(
        &self,
        override_budget: &BudgetOverride,
    ) -> Result<HardBudget, ConfigError> {
        let mut effective = self.budget.clone();
        macro_rules! restrict {
            ($field:ident) => {
                if let Some(value) = override_budget.$field {
                    if value > self.budget.$field {
                        return Err(ConfigError::Rejected(
                            concat!(stringify!($field), " may only tighten profile budget").into(),
                        ));
                    }
                    effective.$field = value;
                }
            };
        }
        restrict!(max_virtual_players);
        restrict!(max_login_qps);
        restrict!(max_new_connections_per_second);
        restrict!(max_business_messages_per_second);
        restrict!(max_messages_per_connection_per_second);
        restrict!(max_duration_secs);
        restrict!(max_total_operations);
        restrict!(max_error_rate);
        restrict!(max_connection_failure_rate);
        restrict!(max_p99_ms);
        restrict!(max_data_writes);
        effective.validate()?;
        Ok(effective)
    }

    pub fn parsed_targets(&self) -> Result<[Endpoint; 2], ConfigError> {
        Ok([
            Endpoint::parse(&self.targets.auth_http)?,
            Endpoint::parse(&self.targets.game_proxy)?,
        ])
    }

    fn validate_targets(&self) -> Result<(), ConfigError> {
        let [auth, proxy] = self.parsed_targets()?;
        if auth.scheme != "http" && auth.scheme != "https" {
            return Err(ConfigError::Rejected(
                "auth_http must use http or https".into(),
            ));
        }
        if proxy.scheme != "kcp" {
            return Err(ConfigError::Rejected(
                "game_proxy must use kcp; TCP fallback is diagnostic-only".into(),
            ));
        }
        for endpoint in [auth, proxy] {
            if endpoint.port == 7000
                || endpoint.host == "game-server"
                || endpoint.host.ends_with(".game-server")
            {
                return Err(ConfigError::Rejected(
                    "player load must not target game-server directly".into(),
                ));
            }
        }
        Ok(())
    }

    fn validate_budget(&self) -> Result<(), ConfigError> {
        self.budget.validate()
    }

    fn validate_scenario(&self) -> Result<(), ConfigError> {
        if self.scenario.name.trim().is_empty() {
            return Err(ConfigError::Rejected(
                "scenario name must not be empty".into(),
            ));
        }
        match &self.scenario.load {
            LoadModel::FixedConcurrency {
                virtual_players,
                duration_secs,
            } => {
                require_positive(*virtual_players as u64, "virtual_players")?;
                require_positive(*duration_secs, "duration_secs")?;
            }
            LoadModel::ArrivalRate {
                arrivals_per_second,
                duration_secs,
            } => {
                require_positive_float(*arrivals_per_second, "arrivals_per_second")?;
                require_positive(*duration_secs, "duration_secs")?;
            }
            LoadModel::Burst {
                burst_size,
                every_secs,
                duration_secs,
            } => {
                require_positive(*burst_size as u64, "burst_size")?;
                require_positive(*every_secs, "every_secs")?;
                require_positive(*duration_secs, "duration_secs")?;
            }
            LoadModel::Staged { stages } => {
                if stages.is_empty() {
                    return Err(ConfigError::Rejected(
                        "staged load requires at least one stage".into(),
                    ));
                }
                let mut names = BTreeSet::new();
                for stage in stages {
                    require_positive(stage.virtual_players as u64, "stage virtual_players")?;
                    require_positive(stage.duration_secs, "stage duration_secs")?;
                    if stage.name.trim().is_empty() || !names.insert(&stage.name) {
                        return Err(ConfigError::Rejected(
                            "stage names must be non-empty and unique".into(),
                        ));
                    }
                }
            }
        }
        let mut steps = BTreeSet::new();
        for step in &self.scenario.steps {
            if !steps.insert(&step.name) {
                return Err(ConfigError::Rejected(
                    "scenario contains duplicate step names".into(),
                ));
            }
            step.validate()?;
        }
        if let Some(auth) = &self.scenario.auth {
            if auth.operations.is_empty() {
                return Err(ConfigError::Rejected(
                    "auth scenario requires at least one operation".into(),
                ));
            }
            if auth.allow_same_account_concurrency && auth.same_account_session_effect.is_none() {
                return Err(ConfigError::Rejected(
                    "same-account auth scenario requires an explicit session effect".into(),
                ));
            }
            if !auth.allow_same_account_concurrency && auth.same_account_session_effect.is_some() {
                return Err(ConfigError::Rejected(
                    "session effect is only valid for an explicit same-account scenario".into(),
                ));
            }
        }
        if let Some(reconnect) = &self.scenario.reconnect_burst {
            if reconnect.virtual_players == 0
                || reconnect.virtual_players > self.budget.max_virtual_players
            {
                return Err(ConfigError::Rejected(
                    "reconnect burst virtual_players must be within the hard budget".into(),
                ));
            }
            if reconnect.reconnect_attempts_per_player == 0 {
                return Err(ConfigError::Rejected(
                    "reconnect burst requires at least one attempt per player".into(),
                ));
            }
            ReconnectPolicy::from(reconnect.reconnect_policy)
                .validate()
                .map_err(|error| ConfigError::Rejected(error.to_string()))?;
        }
        if let Some(gameplay) = &self.scenario.live_gameplay {
            if gameplay.room_id.trim().is_empty() || gameplay.policy_id.trim().is_empty() {
                return Err(ConfigError::Rejected(
                    "live gameplay requires explicit non-empty room_id and policy_id".into(),
                ));
            }
            if gameplay.profile == PlayerProfile::Idle {
                return Err(ConfigError::Rejected(
                    "live gameplay requires a profile that emits a frame input".into(),
                ));
            }
            if gameplay.lockstep_scenario_json.trim().is_empty()
                || gameplay.lockstep_scenario_json.len() > MAX_LIVE_GAMEPLAY_SCENARIO_BYTES
            {
                return Err(ConfigError::Rejected(format!(
                    "live gameplay lockstep_scenario_json must be 1..={MAX_LIVE_GAMEPLAY_SCENARIO_BYTES} bytes"
                )));
            }
            if gameplay.max_frame_inputs == 0
                || gameplay.max_frame_inputs > MAX_LIVE_GAMEPLAY_FRAME_INPUTS
            {
                return Err(ConfigError::Rejected(format!(
                    "live gameplay max_frame_inputs must be 1..={MAX_LIVE_GAMEPLAY_FRAME_INPUTS}"
                )));
            }
            let plan = GameplayProfilePlan::from_lockstep_scenario_json(
                gameplay.profile,
                &gameplay.lockstep_scenario_json,
            )
            .map_err(|error| {
                ConfigError::Rejected(format!("live gameplay profile rejected: {error}"))
            })?;
            plan.packet_plan_with_input_limit(
                &gameplay.room_id,
                &gameplay.policy_id,
                gameplay.max_frame_inputs,
            )
            .map_err(|error| {
                ConfigError::Rejected(format!("live gameplay packet plan rejected: {error}"))
            })?;
            if !self.scenario.writes_data {
                return Err(ConfigError::Rejected(
                    "live gameplay emits room/input writes and requires writes_data=true".into(),
                ));
            }
            if let Some(reconnect) = gameplay.reconnect {
                if reconnect.reconnect_policy.max_attempts != 1 {
                    return Err(ConfigError::Rejected(
                        "live gameplay reconnect_policy.max_attempts must equal 1".into(),
                    ));
                }
                ReconnectPolicy::from(reconnect.reconnect_policy)
                    .validate()
                    .map_err(|error| ConfigError::Rejected(error.to_string()))?;
            }
        }
        Ok(())
    }

    fn validate_account_prepare(&self) -> Result<(), ConfigError> {
        let batch = self.account_prepare.batch.trim();
        if batch.is_empty()
            || batch.len() > 32
            || !batch
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(ConfigError::Rejected(
                "account_prepare.batch must be 1..=32 lowercase letters, digits, or hyphens".into(),
            ));
        }
        if let Some(count) = self.account_prepare.account_count {
            require_positive(count as u64, "account_prepare.account_count")?;
            if count > self.budget.max_virtual_players {
                return Err(ConfigError::Rejected(
                    "account_prepare.account_count may not exceed max_virtual_players".into(),
                ));
            }
        }
        let prefix = self.account_prepare.character_name_prefix.trim();
        if prefix.is_empty()
            || prefix.len() > 32
            || !prefix.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            })
        {
            return Err(ConfigError::Rejected(
                "account_prepare.character_name_prefix must be 1..=32 lowercase letters, digits, underscores, or hyphens".into(),
            ));
        }
        let auth_login_name =
            format!("loadtest_{}_{}_000001", self.environment.name, batch).replace('-', "_");
        if !(3..=32).contains(&auth_login_name.len())
            || !auth_login_name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(ConfigError::Rejected(
                "environment and account_prepare.batch must produce a supported auth-http loadtest login name".into(),
            ));
        }
        Ok(())
    }
}

impl HardBudget {
    pub fn validate(&self) -> Result<(), ConfigError> {
        require_positive(self.max_virtual_players as u64, "max_virtual_players")?;
        require_positive_float(self.max_login_qps, "max_login_qps")?;
        require_positive_float(
            self.max_new_connections_per_second,
            "max_new_connections_per_second",
        )?;
        require_positive_float(
            self.max_business_messages_per_second,
            "max_business_messages_per_second",
        )?;
        require_positive_float(
            self.max_messages_per_connection_per_second,
            "max_messages_per_connection_per_second",
        )?;
        require_positive(self.max_duration_secs, "max_duration_secs")?;
        require_positive(self.max_total_operations, "max_total_operations")?;
        require_positive(self.max_p99_ms, "max_p99_ms")?;
        for (name, value) in [
            ("max_error_rate", self.max_error_rate),
            (
                "max_connection_failure_rate",
                self.max_connection_failure_rate,
            ),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(ConfigError::Rejected(format!(
                    "{name} must be within 0..=1"
                )));
            }
        }
        Ok(())
    }
}

fn require_positive(value: u64, name: &str) -> Result<(), ConfigError> {
    if value == 0 {
        Err(ConfigError::Rejected(format!("{name} must be positive")))
    } else {
        Ok(())
    }
}
fn require_positive_float(value: f64, name: &str) -> Result<(), ConfigError> {
    if !value.is_finite() || value <= 0.0 {
        Err(ConfigError::Rejected(format!(
            "{name} must be finite and positive"
        )))
    } else {
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrivateConfig {
    pub secret_references: BTreeSet<String>,
    #[serde(default)]
    pub account_credentials: BTreeMap<String, String>,
}

pub fn load_private_config(path: &Path) -> Result<PrivateConfig, ConfigError> {
    let private: PrivateConfig = serde_json::from_slice(&fs::read(path)?)?;
    if private
        .secret_references
        .iter()
        .any(|reference| reference.trim().is_empty())
    {
        return Err(ConfigError::Rejected(
            "private secret references must not be empty".into(),
        ));
    }
    if private
        .account_credentials
        .iter()
        .any(|(logical_id, reference)| {
            logical_id.trim().is_empty()
                || reference.trim().is_empty()
                || !private.secret_references.contains(reference)
        })
    {
        return Err(ConfigError::Rejected(
            "account credential references must use a declared non-empty secret reference".into(),
        ));
    }
    Ok(private)
}

fn validate_private_config(path: &Path) -> Result<(), ConfigError> {
    load_private_config(path).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step::{ExpectedResponse, Idempotency, RetryPolicy};

    fn config() -> LoadTestConfig {
        LoadTestConfig {
            schema_version: SCHEMA_VERSION,
            environment: EnvironmentProfile {
                name: "local".into(),
                kind: EnvironmentKind::Local,
                approval_reference: None,
                allowed_hosts: BTreeSet::new(),
                allowed_ips: BTreeSet::new(),
            },
            targets: PlayerTargets {
                auth_http: "http://127.0.0.1:3000".into(),
                game_proxy: "kcp://127.0.0.1:4000".into(),
            },
            budget: HardBudget {
                max_virtual_players: 10,
                max_login_qps: 5.0,
                max_new_connections_per_second: 5.0,
                max_business_messages_per_second: 10.0,
                max_messages_per_connection_per_second: 2.0,
                max_duration_secs: 30,
                max_total_operations: 100,
                max_error_rate: 0.1,
                max_connection_failure_rate: 0.1,
                max_p99_ms: 1000,
                max_data_writes: 0,
            },
            scenario: Scenario {
                name: "dry".into(),
                load: LoadModel::FixedConcurrency {
                    virtual_players: 1,
                    duration_secs: 1,
                },
                steps: vec![ScenarioStep {
                    name: "ping".into(),
                    timeout_ms: 100,
                    think_time_ms: 0,
                    expected: ExpectedResponse::Success,
                    idempotency: Idempotency::ReadOnly,
                    retry: RetryPolicy::Never,
                }],
                writes_data: false,
                auth: None,
                reconnect_burst: None,
                live_gameplay: None,
            },
            reports_root: "reports".into(),
            prepare_reports_root: "prepare-reports".into(),
            stop_file: None,
            deadline_unix_ms: None,
            graceful_shutdown_ms: default_graceful_shutdown_ms(),
            account_prepare: AccountPrepareConfig::default(),
            calibration: CalibrationThresholds::default(),
        }
    }

    #[test]
    fn local_rejects_remote_and_direct_game_server() {
        let mut value = config();
        value.targets.game_proxy = "kcp://game-server:7000".into();
        assert!(
            value
                .validate_structural()
                .unwrap_err()
                .to_string()
                .contains("game-server")
        );
        let mut value = config();
        value.targets.auth_http = "http://10.0.0.5:3000".into();
        value.validate_structural().unwrap();
        assert!(
            value
                .validate_access(RunAccess::default())
                .unwrap_err()
                .to_string()
                .contains("loopback")
        );
    }

    #[test]
    fn cli_budget_cannot_expand_profile_limit() {
        let value = config();
        assert_eq!(
            value
                .effective_budget(&BudgetOverride {
                    max_virtual_players: Some(5),
                    ..Default::default()
                })
                .unwrap()
                .max_virtual_players,
            5
        );
        assert!(
            value
                .effective_budget(&BudgetOverride {
                    max_login_qps: Some(6.0),
                    ..Default::default()
                })
                .is_err()
        );
    }

    #[test]
    fn rejects_unbounded_models_duplicate_stages_and_write_retries() {
        let mut value = config();
        value.scenario.load = LoadModel::ArrivalRate {
            arrivals_per_second: -1.0,
            duration_secs: 1,
        };
        assert!(value.validate_structural().is_err());
        value.scenario.load = LoadModel::Staged {
            stages: vec![
                LoadStage {
                    name: "a".into(),
                    virtual_players: 1,
                    duration_secs: 1,
                },
                LoadStage {
                    name: "a".into(),
                    virtual_players: 1,
                    duration_secs: 1,
                },
            ],
        };
        assert!(value.validate_structural().is_err());
        value.scenario.load = LoadModel::FixedConcurrency {
            virtual_players: 1,
            duration_secs: 1,
        };
        value.scenario.steps[0].idempotency = Idempotency::Write;
        value.scenario.steps[0].retry = RetryPolicy::Bounded { attempts: 2 };
        assert!(value.validate_structural().is_err());
    }

    #[test]
    fn live_gameplay_is_default_off_and_requires_explicit_bounded_room_inputs() {
        let value = config();
        assert!(value.scenario.live_gameplay.is_none());
        value.validate_structural().unwrap();

        let mut enabled = config();
        enabled.scenario.writes_data = true;
        enabled.budget.max_data_writes = 16;
        enabled.scenario.live_gameplay = Some(LiveGameplayScenario {
            room_id: "approved-room".into(),
            policy_id: "approved-policy".into(),
            profile: PlayerProfile::Normal,
            lockstep_scenario_json: include_str!("../../lockstep-client/scenarios/move_stop.json")
                .into(),
            max_frame_inputs: 1,
            reconnect: None,
        });
        enabled.validate_structural().unwrap();

        enabled.scenario.writes_data = false;
        assert!(
            enabled
                .validate_structural()
                .unwrap_err()
                .to_string()
                .contains("writes_data")
        );
        enabled.scenario.writes_data = true;
        enabled
            .scenario
            .live_gameplay
            .as_mut()
            .unwrap()
            .room_id
            .clear();
        assert!(
            enabled
                .validate_structural()
                .unwrap_err()
                .to_string()
                .contains("room_id")
        );
    }

    #[test]
    fn every_config_layer_rejects_unknown_fields() {
        let template = serde_json::to_value(config()).unwrap();
        for path in [
            &[][..],
            &["environment"][..],
            &["targets"][..],
            &["budget"][..],
            &["scenario"][..],
            &["scenario", "load"][..],
            &["scenario", "steps", "0"][..],
            &["scenario", "steps", "0", "expected"][..],
            &["scenario", "steps", "0", "retry"][..],
        ] {
            let mut value = template.clone();
            let mut cursor = &mut value;
            for segment in path {
                cursor = if let Ok(index) = segment.parse::<usize>() {
                    cursor.as_array_mut().unwrap().get_mut(index).unwrap()
                } else {
                    cursor.as_object_mut().unwrap().get_mut(*segment).unwrap()
                };
            }
            cursor
                .as_object_mut()
                .unwrap()
                .insert("unexpected".into(), serde_json::json!(true));
            assert!(
                serde_json::from_value::<LoadTestConfig>(value).is_err(),
                "unknown field was accepted at {}",
                path.join(".")
            );
        }
    }

    #[test]
    fn private_config_rejects_unknown_fields() {
        let path =
            std::env::temp_dir().join(format!("loadtest-private-{}.json", std::process::id()));
        fs::write(&path, r#"{"secret_references":[],"unexpected":true}"#).unwrap();
        assert!(validate_private_config(&path).is_err());
        fs::remove_file(path).unwrap();
    }
}
