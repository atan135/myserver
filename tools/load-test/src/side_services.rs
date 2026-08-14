use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{EnvironmentKind, HardBudget};

const MAX_SIDE_STEPS: usize = 32;
const MAX_SIDE_WEIGHT: u32 = 100;
const MAX_DESCRIPTOR_HOST: usize = 253;
const ANNOUNCE_BURST_READ_OPERATIONS: u64 = 8;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SideServiceKind {
    Chat,
    Mail,
    Announce,
    Match,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SideTransportKind {
    Kcp,
    Ws,
    Wss,
    Grpc,
    Http,
    Https,
}

impl SideTransportKind {
    pub fn protocol(self) -> &'static str {
        match self {
            Self::Kcp => "kcp",
            Self::Ws => "ws",
            Self::Wss => "wss",
            Self::Grpc => "grpc",
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServiceDescriptor {
    pub host: String,
    pub port: u16,
    pub protocol: SideTransportKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthServicesPayload {
    pub game: Option<ServiceDescriptor>,
    pub chat: Option<ServiceDescriptor>,
    pub mail: Option<ServiceDescriptor>,
    pub announce: Option<ServiceDescriptor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthDescriptorSet {
    pub services: AuthServicesPayload,
    pub observations: Vec<DescriptorObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptorValidationError {
    PayloadInvalid,
    ServicesMissing,
    DescriptorMissing(SideServiceKind),
    DescriptorInvalid(SideServiceKind, String),
    DescriptorOutsideAllowlist(SideServiceKind, String),
}

impl std::fmt::Display for DescriptorValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PayloadInvalid => write!(f, "auth services payload is invalid"),
            Self::ServicesMissing => write!(f, "auth response is missing services descriptors"),
            Self::DescriptorMissing(service) => {
                write!(f, "auth response is missing {service:?} descriptor")
            }
            Self::DescriptorInvalid(service, reason) => {
                write!(f, "auth {service:?} descriptor rejected: {reason}")
            }
            Self::DescriptorOutsideAllowlist(service, reason) => write!(
                f,
                "auth {service:?} descriptor rejected by allowlist: {reason}"
            ),
        }
    }
}

pub fn parse_auth_service_descriptors(
    payload: &str,
    required: &BTreeSet<SideServiceKind>,
    allowlists: &BTreeMap<SideServiceKind, DescriptorAllowlist>,
    tracker: &mut DescriptorChangeTracker,
) -> Result<AuthDescriptorSet, DescriptorValidationError> {
    let root: serde_json::Value =
        serde_json::from_str(payload).map_err(|_| DescriptorValidationError::PayloadInvalid)?;
    let services = root
        .get("services")
        .ok_or(DescriptorValidationError::ServicesMissing)?;
    let services: AuthServicesPayload = serde_json::from_value(services.clone())
        .map_err(|_| DescriptorValidationError::PayloadInvalid)?;
    validate_auth_service_descriptors(&services, required, allowlists, tracker)
}

/// Validates descriptors supplied by a successful auth response. This is kept
/// separate from JSON parsing so the live runner can retain the parsed payload
/// in memory without reparsing or logging a full auth response.
pub fn validate_auth_service_descriptors(
    services: &AuthServicesPayload,
    required: &BTreeSet<SideServiceKind>,
    allowlists: &BTreeMap<SideServiceKind, DescriptorAllowlist>,
    tracker: &mut DescriptorChangeTracker,
) -> Result<AuthDescriptorSet, DescriptorValidationError> {
    for (kind, descriptor) in [
        (SideServiceKind::Chat, services.chat.as_ref()),
        (SideServiceKind::Mail, services.mail.as_ref()),
        (SideServiceKind::Announce, services.announce.as_ref()),
    ] {
        if required.contains(&kind) {
            let descriptor =
                descriptor.ok_or(DescriptorValidationError::DescriptorMissing(kind))?;
            descriptor
                .validate(kind)
                .map_err(|e| DescriptorValidationError::DescriptorInvalid(kind, e))?;
            if let Some(allowlist) = allowlists.get(&kind) {
                allowlist
                    .validate(descriptor)
                    .map_err(|e| DescriptorValidationError::DescriptorOutsideAllowlist(kind, e))?;
            }
            tracker
                .observe(kind, descriptor)
                .map_err(|e| DescriptorValidationError::DescriptorInvalid(kind, e))?;
        }
    }
    Ok(AuthDescriptorSet {
        services: services.clone(),
        observations: tracker.observations().to_vec(),
    })
}

/// Resolves public chat/mail/announce endpoints from the authenticated auth
/// response. An explicit scenario descriptor remains a local/test diagnostic
/// fallback when discovery returns no endpoint; the runner never invents a
/// host or fixed port. Match remains an explicit local/test gRPC diagnostic
/// because auth-http does not expose a public Match descriptor.
pub fn resolve_auth_service_descriptors(
    scenario: &SideServicesScenario,
    services: Option<&AuthServicesPayload>,
    required: &BTreeSet<SideServiceKind>,
    tracker: &mut DescriptorChangeTracker,
) -> Result<SideServicesScenario, DescriptorValidationError> {
    let Some(services) = services else {
        return Ok(scenario.clone());
    };
    let mut resolved = scenario.clone();
    let allowlists = BTreeMap::from([
        (
            SideServiceKind::Chat,
            resolved
                .chat
                .as_ref()
                .map(|config| config.allowlist.clone())
                .unwrap_or_default(),
        ),
        (
            SideServiceKind::Mail,
            resolved
                .mail
                .as_ref()
                .map(|config| config.allowlist.clone())
                .unwrap_or_default(),
        ),
        (
            SideServiceKind::Announce,
            resolved
                .announce
                .as_ref()
                .map(|config| config.allowlist.clone())
                .unwrap_or_default(),
        ),
    ]);
    let discovered = [
        (SideServiceKind::Chat, services.chat.as_ref()),
        (SideServiceKind::Mail, services.mail.as_ref()),
        (SideServiceKind::Announce, services.announce.as_ref()),
    ]
    .into_iter()
    .filter_map(|(kind, descriptor)| {
        required
            .contains(&kind)
            .then_some(descriptor?)
            .map(|_| kind)
    })
    .collect();
    validate_auth_service_descriptors(services, &discovered, &allowlists, tracker)?;
    if let Some(descriptor) = services
        .chat
        .as_ref()
        .filter(|_| required.contains(&SideServiceKind::Chat))
    {
        resolved
            .chat
            .as_mut()
            .expect("required chat has config")
            .descriptor = Some(descriptor.clone());
    }
    if let Some(descriptor) = services
        .mail
        .as_ref()
        .filter(|_| required.contains(&SideServiceKind::Mail))
    {
        resolved
            .mail
            .as_mut()
            .expect("required mail has config")
            .descriptor = Some(descriptor.clone());
    }
    if let Some(descriptor) = services
        .announce
        .as_ref()
        .filter(|_| required.contains(&SideServiceKind::Announce))
    {
        resolved
            .announce
            .as_mut()
            .expect("required announce has config")
            .descriptor = Some(descriptor.clone());
    }
    Ok(resolved)
}

impl ServiceDescriptor {
    pub fn validate(&self, service: SideServiceKind) -> Result<(), String> {
        let host = self.host.trim();
        if host.is_empty()
            || host.len() > MAX_DESCRIPTOR_HOST
            || host != self.host
            || host.contains(char::is_whitespace)
            || host.contains('/')
            || host.contains('@')
        {
            return Err("descriptor host is invalid".into());
        }
        if self.port == 0 {
            return Err("descriptor port must not be zero".into());
        }
        let valid = match service {
            SideServiceKind::Chat => {
                matches!(
                    self.protocol,
                    SideTransportKind::Ws | SideTransportKind::Wss
                )
            }
            SideServiceKind::Mail | SideServiceKind::Announce => {
                matches!(
                    self.protocol,
                    SideTransportKind::Http | SideTransportKind::Https
                )
            }
            SideServiceKind::Match => matches!(self.protocol, SideTransportKind::Grpc),
        };
        if !valid {
            return Err(format!(
                "{} descriptor protocol is not allowed for {:?}",
                self.protocol.protocol(),
                service
            ));
        }
        Ok(())
    }

    pub fn safe_summary(&self) -> String {
        let digest = Sha256::digest(format!(
            "{}://{}:{}",
            self.protocol.protocol(),
            self.host,
            self.port
        ));
        format!(
            "{}://side-target-{:x}:{}",
            self.protocol.protocol(),
            digest,
            self.port
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DescriptorObservation {
    pub service: SideServiceKind,
    pub descriptor_summary: String,
    pub changed: bool,
}

#[derive(Debug, Clone, Default)]
pub struct DescriptorChangeTracker {
    current: BTreeMap<SideServiceKind, String>,
    observations: Vec<DescriptorObservation>,
}

impl DescriptorChangeTracker {
    pub fn observe(
        &mut self,
        service: SideServiceKind,
        descriptor: &ServiceDescriptor,
    ) -> Result<DescriptorObservation, String> {
        descriptor.validate(service)?;
        let summary = descriptor.safe_summary();
        let changed = self
            .current
            .get(&service)
            .is_some_and(|old| old != &summary);
        self.current.insert(service, summary.clone());
        let observation = DescriptorObservation {
            service,
            descriptor_summary: summary,
            changed,
        };
        self.observations.push(observation.clone());
        Ok(observation)
    }

    pub fn observations(&self) -> &[DescriptorObservation] {
        &self.observations
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DescriptorAllowlist {
    #[serde(default)]
    pub hosts: BTreeSet<String>,
    #[serde(default)]
    pub protocols: BTreeSet<SideTransportKind>,
}

impl DescriptorAllowlist {
    pub fn validate(&self, descriptor: &ServiceDescriptor) -> Result<(), String> {
        if !self.hosts.is_empty() && !self.hosts.contains(&descriptor.host) {
            return Err("descriptor host is outside the allowlist".into());
        }
        if !self.protocols.is_empty() && !self.protocols.contains(&descriptor.protocol) {
            return Err("descriptor protocol is outside the allowlist".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SideServiceOperation {
    ChatAuth,
    ChatPrivate,
    ChatGroup,
    ChatHistory,
    MailList,
    MailDetail,
    MailRead,
    MailClaim,
    MailNotify,
    MatchStart,
    MatchCancel,
    MatchStatus,
    MatchEventStream,
    MatchInternalCreateRoomAndJoin,
    MatchInternalPlayerJoined,
    MatchInternalStatus,
    MatchInternalPlayerLeft,
    MatchInternalEnd,
    AnnounceList,
    AnnounceDetail,
    AnnounceBurstRead,
    AnnounceCreate,
    AnnounceUpdate,
    AnnounceDelete,
}

impl SideServiceOperation {
    pub fn is_write(&self) -> bool {
        matches!(
            self,
            Self::ChatPrivate
                | Self::ChatGroup
                | Self::MailRead
                | Self::MailClaim
                | Self::MailNotify
                | Self::MatchStart
                | Self::MatchCancel
                | Self::MatchInternalCreateRoomAndJoin
                | Self::MatchInternalPlayerJoined
                | Self::MatchInternalPlayerLeft
                | Self::MatchInternalEnd
                | Self::AnnounceCreate
                | Self::AnnounceUpdate
                | Self::AnnounceDelete
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SideServiceStep {
    pub operation: SideServiceOperation,
    #[serde(default = "default_side_weight")]
    pub weight: u32,
    #[serde(default)]
    pub think_time_ms: u64,
}

fn default_side_weight() -> u32 {
    1
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SideServiceConfig {
    pub descriptor: Option<ServiceDescriptor>,
    #[serde(default)]
    pub allowlist: DescriptorAllowlist,
    #[serde(default)]
    pub steps: Vec<SideServiceStep>,
    #[serde(default)]
    pub writes: bool,
    /// A real chat WebSocket is never a default capacity path. It is exposed
    /// only for explicit local/test diagnostics.
    #[serde(default, alias = "live_wss")]
    pub live_websocket: bool,
    #[serde(default)]
    pub live_grpc: bool,
    /// Explicit local/test gate for bounded MatchInternal diagnostics.
    #[serde(default, alias = "live_match_internal")]
    pub live_internal: bool,
    /// Explicit local/test gate for bounded live HTTP diagnostics.
    #[serde(default)]
    pub live_http: bool,
    /// Required batch identity for any live HTTP write operation.
    #[serde(default)]
    pub write_batch: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CompositePlayerProfile {
    #[serde(default)]
    pub weights: BTreeMap<SideServiceKind, u32>,
    #[serde(default)]
    pub max_operations_per_player: u32,
    /// Optional per-service operation ceilings for one virtual player. A
    /// configured service entry must not exceed this cap after its operation
    /// and composition weights are expanded.
    #[serde(default)]
    pub max_operations_per_service_per_player: BTreeMap<SideServiceKind, u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SideServicesScenario {
    #[serde(default)]
    pub chat: Option<SideServiceConfig>,
    #[serde(default)]
    pub mail: Option<SideServiceConfig>,
    #[serde(default)]
    pub announce: Option<SideServiceConfig>,
    #[serde(default)]
    pub r#match: Option<SideServiceConfig>,
    #[serde(default)]
    pub composition: CompositePlayerProfile,
}

impl SideServicesScenario {
    pub fn writes_data(&self) -> bool {
        [
            self.chat.as_ref(),
            self.mail.as_ref(),
            self.announce.as_ref(),
            self.r#match.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|config| config.writes)
    }

    pub fn validate(&self) -> Result<(), String> {
        for (kind, config) in [
            (SideServiceKind::Chat, self.chat.as_ref()),
            (SideServiceKind::Mail, self.mail.as_ref()),
            (SideServiceKind::Announce, self.announce.as_ref()),
            (SideServiceKind::Match, self.r#match.as_ref()),
        ] {
            let Some(config) = config else { continue };
            let invalid_transport_gate = match kind {
                SideServiceKind::Chat => {
                    config.live_grpc
                        || config.live_internal
                        || config.live_http
                        || config.write_batch.is_some()
                }
                SideServiceKind::Mail | SideServiceKind::Announce => {
                    config.live_websocket || config.live_grpc || config.live_internal
                }
                SideServiceKind::Match => {
                    config.live_websocket || config.live_http || config.write_batch.is_some()
                }
            };
            if invalid_transport_gate {
                return Err(format!(
                    "{kind:?} contains a live transport gate that is not applicable to its protocol"
                ));
            }
            if config.write_batch.is_some()
                && !(matches!(kind, SideServiceKind::Mail | SideServiceKind::Announce)
                    && config.live_http
                    && config.writes)
            {
                return Err(format!(
                    "{kind:?} write_batch requires an enabled live HTTP write scenario"
                ));
            }
            if let Some(descriptor) = &config.descriptor {
                descriptor.validate(kind)?;
                config.allowlist.validate(descriptor)?;
            }
            if config.steps.len() > MAX_SIDE_STEPS {
                return Err(format!("{kind:?} has too many side-service steps"));
            }
            if config
                .steps
                .iter()
                .any(|step| step.weight == 0 || step.weight > MAX_SIDE_WEIGHT)
            {
                return Err(format!(
                    "{kind:?} step weight is outside 1..={MAX_SIDE_WEIGHT}"
                ));
            }
            let writes = config.steps.iter().any(|step| step.operation.is_write());
            if writes != config.writes {
                return Err(format!(
                    "{kind:?} writes flag must match configured operations"
                ));
            }
        }
        if self.composition.max_operations_per_player > 10_000 {
            return Err("side-service composition operation cap is too large".into());
        }
        if self
            .composition
            .max_operations_per_service_per_player
            .values()
            .any(|cap| *cap == 0 || *cap > 10_000)
        {
            return Err("side-service per-service operation cap must be within 1..=10000".into());
        }
        for weight in self.composition.weights.values() {
            if *weight == 0 || *weight > MAX_SIDE_WEIGHT {
                return Err("side-service composition weight is outside allowed range".into());
            }
        }
        Ok(())
    }

    pub fn validate_for_environment(&self, kind: EnvironmentKind) -> Result<(), String> {
        self.validate()?;
        if self
            .r#match
            .as_ref()
            .and_then(|config| config.descriptor.as_ref())
            .is_some_and(|descriptor| descriptor.protocol == SideTransportKind::Grpc)
            && !matches!(kind, EnvironmentKind::Local | EnvironmentKind::Test)
        {
            return Err(
                "direct match gRPC diagnostics require development/isolated profile".into(),
            );
        }
        if kind == EnvironmentKind::Production && self.announce.as_ref().is_some_and(|c| c.writes) {
            return Err("announce writes are forbidden in production profiles".into());
        }
        if let Some(chat) = &self.chat {
            if chat.live_websocket && chat.descriptor.is_none() {
                return Err("live chat WebSocket requires an explicit chat descriptor".into());
            }
            if chat.live_websocket
                && !matches!(kind, EnvironmentKind::Local | EnvironmentKind::Test)
            {
                return Err(
                    "live chat WebSocket is restricted to explicit local/test diagnostics".into(),
                );
            }
            if chat
                .descriptor
                .as_ref()
                .is_some_and(|descriptor| descriptor.protocol == SideTransportKind::Ws)
                && (!chat.live_websocket
                    || !matches!(kind, EnvironmentKind::Local | EnvironmentKind::Test))
            {
                return Err(
                    "plain chat ws requires the explicit local/test live_websocket diagnostic gate"
                        .into(),
                );
            }
        }
        if let Some(matcher) = &self.r#match {
            if matcher.live_grpc && matcher.descriptor.is_none() {
                return Err("live match gRPC requires an explicit descriptor".into());
            }
            if matcher.live_grpc && !matches!(kind, EnvironmentKind::Local | EnvironmentKind::Test)
            {
                return Err("live match gRPC is restricted to local/test diagnostics".into());
            }
            if matcher.live_internal && matcher.descriptor.is_none() {
                return Err("live MatchInternal diagnostics require an explicit descriptor".into());
            }
            if matcher.live_internal
                && !matches!(kind, EnvironmentKind::Local | EnvironmentKind::Test)
            {
                return Err(
                    "live MatchInternal diagnostics are restricted to local/test diagnostics"
                        .into(),
                );
            }
        }
        for (service, config) in [
            (SideServiceKind::Mail, self.mail.as_ref()),
            (SideServiceKind::Announce, self.announce.as_ref()),
        ] {
            let Some(config) = config else { continue };
            if config.live_http && config.descriptor.is_none() {
                return Err(format!(
                    "live {service:?} HTTP requires an explicit descriptor"
                ));
            }
            if config.live_http && !matches!(kind, EnvironmentKind::Local | EnvironmentKind::Test) {
                return Err(format!(
                    "live {service:?} HTTP is restricted to local/test diagnostics"
                ));
            }
            if config.writes && kind == EnvironmentKind::Production {
                return Err(format!(
                    "{service:?} writes are forbidden in production profiles"
                ));
            }
            if config.writes && config.live_http && config.write_batch.is_none() {
                return Err(format!("live {service:?} writes require a dedicated batch"));
            }
        }
        Ok(())
    }

    pub fn executable_plan(&self, budget: &HardBudget) -> Result<SideServicePlan, String> {
        self.validate()?;
        let mut steps = Vec::new();
        let mut per_service = BTreeMap::new();
        let per_player_cap = if self.composition.max_operations_per_player == 0 {
            MAX_SIDE_STEPS as u32
        } else {
            self.composition.max_operations_per_player
        };
        let global_cap = budget
            .max_virtual_players
            .checked_mul(per_player_cap)
            .ok_or_else(|| "side-service global operation budget overflowed".to_string())?;
        for (kind, config) in [
            (SideServiceKind::Chat, self.chat.as_ref()),
            (SideServiceKind::Mail, self.mail.as_ref()),
            (SideServiceKind::Announce, self.announce.as_ref()),
            (SideServiceKind::Match, self.r#match.as_ref()),
        ] {
            if let Some(config) = config {
                let service_weight = self.composition.weights.get(&kind).copied().unwrap_or(1);
                for step in &config.steps {
                    let repetitions = step
                        .weight
                        .checked_mul(service_weight)
                        .ok_or_else(|| "side-service step weight overflowed".to_string())?;
                    for _ in 0..repetitions {
                        let operation_cost = side_operation_cost(&step.operation);
                        let service_count = per_service.entry(kind).or_insert(0_u64);
                        *service_count = service_count
                            .checked_add(operation_cost)
                            .ok_or_else(|| "side-service operation count overflowed".to_string())?;
                        if self
                            .composition
                            .max_operations_per_service_per_player
                            .get(&kind)
                            .is_some_and(|cap| *service_count > u64::from(*cap))
                        {
                            return Err(format!(
                                "side-service {kind:?} plan exceeds per-service/player operation budget"
                            ));
                        }
                        steps.push(PlannedSideServiceStep {
                            service: kind,
                            operation: step.operation.clone(),
                            weight: step.weight,
                            think_time_ms: step.think_time_ms,
                        });
                        let planned_operations: u64 = per_service.values().sum();
                        if planned_operations > u64::from(global_cap) {
                            return Err(
                                "side-service plan exceeds global/player operation budget".into()
                            );
                        }
                    }
                }
            }
        }
        let virtual_players = u64::from(budget.max_virtual_players);
        let per_player_operations: u64 = per_service.values().sum();
        let total_operations = per_player_operations
            .checked_mul(virtual_players)
            .ok_or_else(|| "side-service total operation count overflowed".to_string())?;
        let global_rate_capacity =
            budget.max_business_messages_per_second * budget.max_duration_secs as f64;
        let connection_rate_capacity =
            budget.max_messages_per_connection_per_second * budget.max_duration_secs as f64;
        if total_operations > budget.max_total_operations
            || total_operations as f64 > global_rate_capacity
            || per_player_operations as f64 > connection_rate_capacity
        {
            return Err("side-service plan exceeds total operation budget".into());
        }
        Ok(SideServicePlan {
            steps,
            per_player_cap,
            per_service,
            total_operations,
        })
    }
}

fn side_operation_cost(operation: &SideServiceOperation) -> u64 {
    match operation {
        SideServiceOperation::AnnounceBurstRead => ANNOUNCE_BURST_READ_OPERATIONS,
        _ => 1,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlannedSideServiceStep {
    pub service: SideServiceKind,
    pub operation: SideServiceOperation,
    pub weight: u32,
    pub think_time_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SideServicePlan {
    pub steps: Vec<PlannedSideServiceStep>,
    pub per_player_cap: u32,
    pub per_service: BTreeMap<SideServiceKind, u64>,
    pub total_operations: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum SideOutcomeCategory {
    Success,
    RateLimited,
    Slow,
    Timeout,
    Disconnect,
    OutOfOrderPush,
    DuplicatePush,
    SlowConsumer,
    BusinessError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SideFakeOutcome {
    Success,
    RateLimited,
    Slow,
    Timeout,
    Disconnect,
    OutOfOrderPush,
    DuplicatePush,
    SlowConsumer,
    BusinessError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SideRequest {
    pub service: SideServiceKind,
    pub transport: SideTransportKind,
    pub operation: SideServiceOperation,
    pub sequence: u64,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SideResponse {
    pub sequence: u64,
    pub outcome: SideOutcomeCategory,
    pub push_sequence: Option<u64>,
    pub latency_ms: u64,
}

pub trait SideTransportContract {
    fn transport_kind(&self) -> SideTransportKind;
    fn send(&mut self, request: SideRequest) -> SideResponse;
}

/// Executes a side-service plan only against deterministic fakes. Real WSS,
/// gRPC and HTTP transports remain outside the dry-run boundary.
pub fn execute_side_services_dry(
    scenario: &SideServicesScenario,
    budget: &HardBudget,
    metrics: &mut crate::metrics::Metrics,
) -> Result<SideServicePlan, String> {
    let plan = scenario.executable_plan(budget)?;
    let mut fake = DeterministicSideFake::scripted_with_transport(
        SideTransportKind::Http,
        std::iter::repeat(SideFakeOutcome::Success).take(plan.steps.len()),
    );
    let transport = fake.transport_kind();
    for (sequence, step) in plan.steps.iter().enumerate() {
        let _ = fake.send(SideRequest {
            service: step.service,
            transport,
            operation: step.operation.clone(),
            sequence: sequence as u64 + 1,
            body: Vec::new(),
        });
    }
    fake.metrics.merge_into_metrics(metrics);
    Ok(plan)
}

impl SideFakeOutcome {
    pub fn category(&self) -> SideOutcomeCategory {
        match self {
            Self::Success => SideOutcomeCategory::Success,
            Self::RateLimited => SideOutcomeCategory::RateLimited,
            Self::Slow => SideOutcomeCategory::Slow,
            Self::Timeout => SideOutcomeCategory::Timeout,
            Self::Disconnect => SideOutcomeCategory::Disconnect,
            Self::OutOfOrderPush => SideOutcomeCategory::OutOfOrderPush,
            Self::DuplicatePush => SideOutcomeCategory::DuplicatePush,
            Self::SlowConsumer => SideOutcomeCategory::SlowConsumer,
            Self::BusinessError(_) => SideOutcomeCategory::BusinessError,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeterministicSideFake {
    outcomes: VecDeque<SideFakeOutcome>,
    pub requests: u64,
    pub metrics: SideServiceMetrics,
    transport: SideTransportKind,
}

impl DeterministicSideFake {
    pub fn scripted(outcomes: impl IntoIterator<Item = SideFakeOutcome>) -> Self {
        Self {
            outcomes: outcomes.into_iter().collect(),
            requests: 0,
            metrics: SideServiceMetrics::default(),
            transport: SideTransportKind::Http,
        }
    }

    pub fn scripted_with_transport(
        transport: SideTransportKind,
        outcomes: impl IntoIterator<Item = SideFakeOutcome>,
    ) -> Self {
        Self {
            outcomes: outcomes.into_iter().collect(),
            requests: 0,
            metrics: SideServiceMetrics::default(),
            transport,
        }
    }

    pub fn request(&mut self, step: &PlannedSideServiceStep) -> SideFakeOutcome {
        self.execute(step)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SideServiceMetrics {
    pub counters: BTreeMap<String, u64>,
    pub latencies_ms: BTreeMap<String, Vec<u64>>,
    #[serde(skip)]
    last_outcome: Option<SideOutcomeCategory>,
}

impl SideServiceMetrics {
    pub fn record(&mut self, step: &PlannedSideServiceStep, outcome: SideFakeOutcome) {
        let prefix = match step.service {
            SideServiceKind::Chat => "side_chat",
            SideServiceKind::Mail => "side_mail",
            SideServiceKind::Announce => "side_announce",
            SideServiceKind::Match => "side_match",
        };
        *self
            .counters
            .entry(format!("{prefix}_operations"))
            .or_default() += 1;
        let suffix = match outcome.category() {
            SideOutcomeCategory::Success => "success",
            SideOutcomeCategory::RateLimited => "rate_limited",
            SideOutcomeCategory::Slow => "slow",
            SideOutcomeCategory::Timeout => "timeout",
            SideOutcomeCategory::Disconnect => "disconnect",
            SideOutcomeCategory::OutOfOrderPush => "push_out_of_order",
            SideOutcomeCategory::DuplicatePush => "push_duplicate",
            SideOutcomeCategory::SlowConsumer => "slow_consumer",
            SideOutcomeCategory::BusinessError => "business_error",
        };
        let key = format!("{prefix}_{suffix}");
        *self.counters.entry(key).or_default() += 1;
        self.last_outcome = Some(outcome.category());
    }

    pub fn merge_into_metrics(&self, metrics: &mut crate::metrics::Metrics) {
        for (key, value) in &self.counters {
            metrics.increment(key, *value);
        }
        for (service, values) in &self.latencies_ms {
            let key = match service.as_str() {
                "chat" => "side_chat_ms",
                "mail" => "side_mail_ms",
                "announce" => "side_announce_ms",
                "match" => "side_match_ms",
                _ => continue,
            };
            for value in values {
                metrics.observe_latency(key, *value);
            }
        }
    }

    pub fn record_latency(&mut self, service: SideServiceKind, latency_ms: u64) {
        let key = match service {
            SideServiceKind::Chat => "chat",
            SideServiceKind::Mail => "mail",
            SideServiceKind::Announce => "announce",
            SideServiceKind::Match => "match",
        };
        self.latencies_ms
            .entry(key.into())
            .or_default()
            .push(latency_ms);
    }
}

impl SideOutcomeCategory {
    pub fn into_fake(self) -> SideFakeOutcome {
        match self {
            Self::Success => SideFakeOutcome::Success,
            Self::RateLimited => SideFakeOutcome::RateLimited,
            Self::Slow => SideFakeOutcome::Slow,
            Self::Timeout => SideFakeOutcome::Timeout,
            Self::Disconnect => SideFakeOutcome::Disconnect,
            Self::OutOfOrderPush => SideFakeOutcome::OutOfOrderPush,
            Self::DuplicatePush => SideFakeOutcome::DuplicatePush,
            Self::SlowConsumer => SideFakeOutcome::SlowConsumer,
            Self::BusinessError => SideFakeOutcome::BusinessError("business_error".into()),
        }
    }
}

pub trait SideServiceTransport {
    fn kind(&self) -> SideTransportKind;
    fn execute(&mut self, step: &PlannedSideServiceStep) -> SideFakeOutcome;
}

impl SideServiceTransport for DeterministicSideFake {
    fn kind(&self) -> SideTransportKind {
        SideTransportKind::Http
    }

    fn execute(&mut self, step: &PlannedSideServiceStep) -> SideFakeOutcome {
        self.send(SideRequest {
            service: step.service,
            transport: self.transport,
            operation: step.operation.clone(),
            sequence: self.requests + 1,
            body: Vec::new(),
        })
        .outcome
        .into_fake()
    }
}

impl SideTransportContract for DeterministicSideFake {
    fn transport_kind(&self) -> SideTransportKind {
        self.transport
    }

    fn send(&mut self, request: SideRequest) -> SideResponse {
        self.requests += 1;
        let outcome = self
            .outcomes
            .pop_front()
            .unwrap_or(SideFakeOutcome::Success);
        let category = outcome.category();
        let latency_ms = match &outcome {
            SideFakeOutcome::Slow | SideFakeOutcome::SlowConsumer => 500,
            SideFakeOutcome::Timeout | SideFakeOutcome::Disconnect => 1_000,
            _ => 10,
        };
        let step = PlannedSideServiceStep {
            service: request.service,
            operation: request.operation,
            weight: 1,
            think_time_ms: 0,
        };
        self.metrics.record(&step, outcome.clone());
        self.metrics.record_latency(request.service, latency_ms);
        if matches!(outcome, SideFakeOutcome::SlowConsumer) {
            let key = match request.service {
                SideServiceKind::Chat => "side_chat_queue_backlog",
                SideServiceKind::Mail => "side_mail_queue_backlog",
                SideServiceKind::Announce => "side_announce_queue_backlog",
                SideServiceKind::Match => "side_match_queue_backlog",
            };
            *self.metrics.counters.entry(key.into()).or_default() += 1;
        }
        if matches!(
            outcome,
            SideFakeOutcome::OutOfOrderPush | SideFakeOutcome::DuplicatePush
        ) {
            let key = match request.service {
                SideServiceKind::Chat => "side_chat_push_events",
                SideServiceKind::Mail => "side_mail_push_events",
                SideServiceKind::Announce => "side_announce_push_events",
                SideServiceKind::Match => "side_match_push_events",
            };
            *self.metrics.counters.entry(key.into()).or_default() += 1;
        }
        SideResponse {
            sequence: request.sequence,
            outcome: category,
            push_sequence: matches!(
                category,
                SideOutcomeCategory::OutOfOrderPush | SideOutcomeCategory::DuplicatePush
            )
            .then_some(request.sequence + 1),
            latency_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(protocol: SideTransportKind) -> ServiceDescriptor {
        ServiceDescriptor {
            host: "side.example".into(),
            port: 443,
            protocol,
        }
    }

    #[test]
    fn descriptor_whitelist_and_change_digest_are_stable() {
        let d = descriptor(SideTransportKind::Wss);
        d.validate(SideServiceKind::Chat).unwrap();
        assert!(d.validate(SideServiceKind::Mail).is_err());
        let allow = DescriptorAllowlist {
            hosts: ["side.example".into()].into(),
            protocols: [SideTransportKind::Wss].into(),
        };
        allow.validate(&d).unwrap();
        assert!(d.safe_summary().contains("side-target-"));
        let mut tracker = DescriptorChangeTracker::default();
        assert!(!tracker.observe(SideServiceKind::Chat, &d).unwrap().changed);
        let changed = ServiceDescriptor {
            host: "other.example".into(),
            ..d
        };
        assert!(
            tracker
                .observe(SideServiceKind::Chat, &changed)
                .unwrap()
                .changed
        );
    }

    #[test]
    fn auth_payload_requires_descriptors_and_tracks_changes() {
        let mut tracker = DescriptorChangeTracker::default();
        let required = [SideServiceKind::Chat, SideServiceKind::Mail].into();
        let allowlists = BTreeMap::new();
        let payload = r#"{"services":{"chat":{"host":"chat.example","port":443,"protocol":"wss"},"mail":{"host":"api.example","port":443,"protocol":"https"},"announce":null}}"#;
        let parsed =
            parse_auth_service_descriptors(payload, &required, &allowlists, &mut tracker).unwrap();
        assert_eq!(parsed.observations.len(), 2);
        assert!(
            parse_auth_service_descriptors(
                r#"{"services":{"chat":null,"mail":null}}"#,
                &required,
                &allowlists,
                &mut tracker
            )
            .is_err()
        );
        let outside = BTreeMap::from([(
            SideServiceKind::Chat,
            DescriptorAllowlist {
                hosts: ["other.example".into()].into(),
                protocols: BTreeSet::new(),
            },
        )]);
        assert!(matches!(
            parse_auth_service_descriptors(payload, &required, &outside, &mut tracker),
            Err(DescriptorValidationError::DescriptorOutsideAllowlist(
                SideServiceKind::Chat,
                _
            ))
        ));
    }

    #[test]
    fn auth_descriptor_resolution_prefers_discovery_and_keeps_explicit_fallback() {
        let static_chat = ServiceDescriptor {
            host: "127.0.0.1".into(),
            port: 9001,
            protocol: SideTransportKind::Wss,
        };
        let scenario = SideServicesScenario {
            chat: Some(SideServiceConfig {
                descriptor: Some(static_chat.clone()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let required = [SideServiceKind::Chat].into();
        let discovered = AuthServicesPayload {
            game: None,
            chat: Some(ServiceDescriptor {
                host: "chat.example".into(),
                port: 443,
                protocol: SideTransportKind::Wss,
            }),
            mail: None,
            announce: None,
        };
        let mut tracker = DescriptorChangeTracker::default();
        let resolved =
            resolve_auth_service_descriptors(&scenario, Some(&discovered), &required, &mut tracker)
                .unwrap();
        assert_eq!(
            resolved.chat.unwrap().descriptor.unwrap().host,
            "chat.example"
        );
        assert_eq!(tracker.observations().len(), 1);

        let fallback = resolve_auth_service_descriptors(
            &scenario,
            Some(&AuthServicesPayload {
                game: None,
                chat: None,
                mail: None,
                announce: None,
            }),
            &required,
            &mut tracker,
        )
        .unwrap();
        assert_eq!(fallback.chat.unwrap().descriptor.unwrap(), static_chat);
    }

    #[test]
    fn side_plan_and_fake_cover_bounded_service_failures() {
        let scenario = SideServicesScenario {
            chat: Some(SideServiceConfig {
                descriptor: Some(descriptor(SideTransportKind::Wss)),
                allowlist: DescriptorAllowlist::default(),
                steps: vec![SideServiceStep {
                    operation: SideServiceOperation::ChatAuth,
                    weight: 1,
                    think_time_ms: 0,
                }],
                writes: false,
                live_websocket: false,
                live_grpc: false,
                live_internal: false,
                live_http: false,
                write_batch: None,
            }),
            ..Default::default()
        };
        let plan = scenario
            .executable_plan(&HardBudget {
                max_total_operations: 2,
                ..budget()
            })
            .unwrap();
        let mut fake = DeterministicSideFake::scripted([
            SideFakeOutcome::RateLimited,
            SideFakeOutcome::DuplicatePush,
            SideFakeOutcome::SlowConsumer,
        ]);
        assert_eq!(
            fake.execute(&plan.steps[0]).category(),
            SideOutcomeCategory::RateLimited
        );
        assert_eq!(
            fake.execute(&plan.steps[0]).category(),
            SideOutcomeCategory::DuplicatePush
        );
        assert_eq!(
            fake.execute(&plan.steps[0]).category(),
            SideOutcomeCategory::SlowConsumer
        );
        assert_eq!(fake.requests, 3);
    }

    #[test]
    fn weighted_plan_is_deterministic_and_respects_player_and_global_caps() {
        let scenario = SideServicesScenario {
            chat: Some(SideServiceConfig {
                descriptor: Some(descriptor(SideTransportKind::Wss)),
                steps: vec![SideServiceStep {
                    operation: SideServiceOperation::ChatAuth,
                    weight: 2,
                    think_time_ms: 12,
                }],
                ..Default::default()
            }),
            composition: CompositePlayerProfile {
                weights: BTreeMap::from([(SideServiceKind::Chat, 2)]),
                max_operations_per_player: 5,
                ..Default::default()
            },
            ..Default::default()
        };
        let plan = scenario
            .executable_plan(&HardBudget {
                max_total_operations: 8,
                max_business_messages_per_second: 8.0,
                ..budget()
            })
            .unwrap();
        assert_eq!(plan.steps.len(), 4);
        assert_eq!(plan.steps[0].think_time_ms, 12);
        assert!(
            scenario
                .executable_plan(&HardBudget {
                    max_total_operations: 2,
                    ..budget()
                })
                .is_err()
        );
    }

    #[test]
    fn composite_plan_applies_per_service_and_virtual_player_budgets() {
        let scenario = SideServicesScenario {
            chat: Some(SideServiceConfig {
                descriptor: Some(descriptor(SideTransportKind::Wss)),
                steps: vec![SideServiceStep {
                    operation: SideServiceOperation::ChatHistory,
                    weight: 2,
                    think_time_ms: 25,
                }],
                ..Default::default()
            }),
            composition: CompositePlayerProfile {
                weights: BTreeMap::from([(SideServiceKind::Chat, 1)]),
                max_operations_per_player: 4,
                max_operations_per_service_per_player: BTreeMap::from([(SideServiceKind::Chat, 3)]),
            },
            ..Default::default()
        };
        let plan = scenario
            .executable_plan(&HardBudget {
                max_virtual_players: 2,
                max_total_operations: 4,
                max_business_messages_per_second: 1.0,
                max_messages_per_connection_per_second: 1.0,
                max_duration_secs: 4,
                ..budget()
            })
            .unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.total_operations, 4);
        assert_eq!(plan.per_service[&SideServiceKind::Chat], 2);

        let rejected = SideServicesScenario {
            composition: CompositePlayerProfile {
                max_operations_per_service_per_player: BTreeMap::from([(SideServiceKind::Chat, 1)]),
                ..scenario.composition.clone()
            },
            ..scenario.clone()
        };
        assert!(rejected.executable_plan(&budget()).is_err());
    }

    #[test]
    fn announce_burst_reserves_all_repeated_reads_in_operation_budget() {
        let scenario = SideServicesScenario {
            announce: Some(SideServiceConfig {
                descriptor: Some(descriptor(SideTransportKind::Https)),
                steps: vec![SideServiceStep {
                    operation: SideServiceOperation::AnnounceBurstRead,
                    weight: 1,
                    think_time_ms: 0,
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let plan = scenario
            .executable_plan(&HardBudget {
                max_virtual_players: 1,
                max_total_operations: 8,
                max_business_messages_per_second: 8.0,
                max_messages_per_connection_per_second: 8.0,
                ..budget()
            })
            .unwrap();
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.per_service[&SideServiceKind::Announce], 8);
        assert_eq!(plan.total_operations, 8);
    }

    #[test]
    fn fake_metrics_project_latency_queue_and_push_into_fixed_metrics() {
        let step = PlannedSideServiceStep {
            service: SideServiceKind::Chat,
            operation: SideServiceOperation::ChatAuth,
            weight: 1,
            think_time_ms: 0,
        };
        let mut fake = DeterministicSideFake::scripted([
            SideFakeOutcome::SlowConsumer,
            SideFakeOutcome::DuplicatePush,
        ]);
        fake.execute(&step);
        fake.execute(&step);
        let mut metrics = crate::metrics::Metrics::default();
        fake.metrics.merge_into_metrics(&mut metrics);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.counters["side_chat_queue_backlog"], 1);
        assert_eq!(snapshot.counters["side_chat_push_events"], 1);
        assert_eq!(snapshot.histograms["side_chat_ms"].count(), 2);
    }

    #[test]
    fn production_forbids_announce_writes_and_match_diagnostics() {
        let announce = SideServicesScenario {
            announce: Some(SideServiceConfig {
                descriptor: Some(descriptor(SideTransportKind::Https)),
                allowlist: DescriptorAllowlist::default(),
                steps: vec![SideServiceStep {
                    operation: SideServiceOperation::AnnounceCreate,
                    weight: 1,
                    think_time_ms: 0,
                }],
                writes: true,
                live_websocket: false,
                live_grpc: false,
                live_internal: false,
                live_http: false,
                write_batch: None,
            }),
            ..Default::default()
        };
        assert!(
            announce
                .validate_for_environment(EnvironmentKind::Production)
                .is_err()
        );
        let mut match_scenario = announce.clone();
        match_scenario.announce = None;
        match_scenario.r#match = Some(SideServiceConfig {
            descriptor: Some(descriptor(SideTransportKind::Grpc)),
            allowlist: DescriptorAllowlist::default(),
            steps: vec![SideServiceStep {
                operation: SideServiceOperation::MatchStart,
                weight: 1,
                think_time_ms: 0,
            }],
            writes: true,
            live_websocket: false,
            live_grpc: false,
            live_internal: false,
            live_http: false,
            write_batch: None,
        });
        assert!(
            match_scenario
                .validate_for_environment(EnvironmentKind::Staging)
                .is_err()
        );
    }

    #[test]
    fn plain_chat_ws_requires_explicit_local_or_test_diagnostic_gate() {
        let mut scenario = SideServicesScenario {
            chat: Some(SideServiceConfig {
                descriptor: Some(ServiceDescriptor {
                    host: "127.0.0.1".into(),
                    port: 9011,
                    protocol: SideTransportKind::Ws,
                }),
                live_websocket: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        scenario
            .validate_for_environment(EnvironmentKind::Local)
            .unwrap();
        assert!(
            scenario
                .validate_for_environment(EnvironmentKind::Production)
                .is_err()
        );
        scenario.chat.as_mut().unwrap().live_websocket = false;
        assert!(
            scenario
                .validate_for_environment(EnvironmentKind::Test)
                .is_err()
        );
    }

    #[test]
    fn live_http_requires_local_test_and_dedicated_write_batch() {
        let mut scenario = SideServicesScenario {
            mail: Some(SideServiceConfig {
                descriptor: Some(descriptor(SideTransportKind::Http)),
                steps: vec![SideServiceStep {
                    operation: SideServiceOperation::MailRead,
                    weight: 1,
                    think_time_ms: 0,
                }],
                writes: true,
                live_http: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(
            scenario
                .validate_for_environment(EnvironmentKind::Local)
                .is_err()
        );
        scenario.mail.as_mut().unwrap().write_batch = Some("loadtest-local".into());
        scenario
            .validate_for_environment(EnvironmentKind::Local)
            .unwrap();
        assert!(
            scenario
                .validate_for_environment(EnvironmentKind::Production)
                .is_err()
        );
    }

    #[test]
    fn service_configs_reject_transport_gates_for_another_protocol() {
        let mut match_scenario = SideServicesScenario {
            r#match: Some(SideServiceConfig {
                descriptor: Some(descriptor(SideTransportKind::Grpc)),
                live_http: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(match_scenario.validate().is_err());
        match_scenario.r#match.as_mut().unwrap().live_http = false;
        match_scenario.r#match.as_mut().unwrap().write_batch = Some("batch".into());
        assert!(match_scenario.validate().is_err());

        let mail_scenario = SideServicesScenario {
            mail: Some(SideServiceConfig {
                descriptor: Some(descriptor(SideTransportKind::Http)),
                live_websocket: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(mail_scenario.validate().is_err());
    }

    fn budget() -> HardBudget {
        HardBudget {
            max_virtual_players: 1,
            max_login_qps: 1.0,
            max_new_connections_per_second: 1.0,
            max_business_messages_per_second: 10.0,
            max_messages_per_connection_per_second: 10.0,
            max_duration_secs: 10,
            max_total_operations: 10,
            max_error_rate: 0.1,
            max_connection_failure_rate: 0.1,
            max_p99_ms: 1000,
            max_data_writes: 10,
        }
    }
}
