//! Read-only registry and metrics observation contract.
//!
//! This module intentionally has no registry mutation surface. The Redis
//! adapter uses the current service-registry schema v2 and metrics-v2 read
//! model, while tests use the scripted transport below instead of Redis.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;

use redis::{AsyncCommands, ErrorKind as RedisErrorKind, RedisError};
use serde::{Deserialize, Serialize};
use service_registry::{
    REGISTRY_HEARTBEAT_TTL_SECONDS, SERVICE_INSTANCE_SCHEMA_VERSION, ServiceInstance,
};

use crate::config::RegistryObservationConfig;
use crate::control_plane::ObservationSnapshot;
use crate::metrics::Metrics;

pub const REGISTRY_OBSERVATION_SOURCE: &str = "registry_readonly_v1";
pub const MAX_REGISTRY_INSTANCES_PER_SERVICE: usize = 64;
pub const MAX_METRIC_FIELDS_PER_INSTANCE: usize = 128;
pub const MIN_REGISTRY_RECHECK_INTERVAL_MS: u64 = 100;
pub const MAX_REGISTRY_RECHECK_INTERVAL_MS: u64 = 30_000;

/// Commands issued by the read-only adapter. These are deliberately a fixed
/// vocabulary so reports cannot contain a key, identifier, or command input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryRedisCommand {
    Zrange,
    Hget,
    Pttl,
    Zrangebyscore,
    Hgetall,
}

impl RegistryRedisCommand {
    fn as_str(self) -> &'static str {
        match self {
            Self::Zrange => "zrange",
            Self::Hget => "hget",
            Self::Pttl => "pttl",
            Self::Zrangebyscore => "zrangebyscore",
            Self::Hgetall => "hgetall",
        }
    }
}

/// A safe, low-cardinality projection of `redis::RedisError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryRedisErrorClass {
    PermissionDenied,
    ResponseError,
    AuthenticationFailed,
    TypeError,
    BusyLoading,
    ClusterUnavailable,
    Retryable,
    ReadOnly,
    Io,
    Client,
    Other,
}

impl RegistryRedisErrorClass {
    fn from_redis_error(error: &RedisError) -> Self {
        if error
            .detail()
            .is_some_and(|detail| detail.trim_start().starts_with("NOPERM"))
        {
            return Self::PermissionDenied;
        }
        match error.kind() {
            RedisErrorKind::ResponseError | RedisErrorKind::ExtensionError => Self::ResponseError,
            RedisErrorKind::AuthenticationFailed => Self::AuthenticationFailed,
            RedisErrorKind::TypeError => Self::TypeError,
            RedisErrorKind::BusyLoadingError => Self::BusyLoading,
            RedisErrorKind::ClusterDown
            | RedisErrorKind::MasterDown
            | RedisErrorKind::ClusterConnectionNotFound
            | RedisErrorKind::MasterNameNotFoundBySentinel
            | RedisErrorKind::NoValidReplicasFoundBySentinel => Self::ClusterUnavailable,
            RedisErrorKind::TryAgain | RedisErrorKind::Moved | RedisErrorKind::Ask => {
                Self::Retryable
            }
            RedisErrorKind::ReadOnly => Self::ReadOnly,
            RedisErrorKind::IoError => Self::Io,
            RedisErrorKind::InvalidClientConfig | RedisErrorKind::ClientError => Self::Client,
            _ => Self::Other,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::ResponseError => "response_error",
            Self::AuthenticationFailed => "authentication_failed",
            Self::TypeError => "type_error",
            Self::BusyLoading => "busy_loading",
            Self::ClusterUnavailable => "cluster_unavailable",
            Self::Retryable => "retryable",
            Self::ReadOnly => "read_only",
            Self::Io => "io",
            Self::Client => "client",
            Self::Other => "other",
        }
    }
}

/// Safe connection-stage classes. These intentionally have no command or
/// server detail because a connection failure cannot be attributed to a
/// read-only adapter command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryRedisConnectionErrorClass {
    AuthenticationFailed,
    Io,
    Client,
    Retryable,
    ClusterUnavailable,
    Other,
}

impl RegistryRedisConnectionErrorClass {
    fn from_redis_error(error: &RedisError) -> Option<Self> {
        match error.kind() {
            RedisErrorKind::AuthenticationFailed => Some(Self::AuthenticationFailed),
            RedisErrorKind::IoError => Some(Self::Io),
            RedisErrorKind::InvalidClientConfig | RedisErrorKind::ClientError => Some(Self::Client),
            RedisErrorKind::BusyLoadingError
            | RedisErrorKind::TryAgain
            | RedisErrorKind::Moved
            | RedisErrorKind::Ask => Some(Self::Retryable),
            RedisErrorKind::ClusterDown
            | RedisErrorKind::MasterDown
            | RedisErrorKind::ClusterConnectionNotFound
            | RedisErrorKind::MasterNameNotFoundBySentinel
            | RedisErrorKind::NoValidReplicasFoundBySentinel => Some(Self::ClusterUnavailable),
            // A Redis server response at connection setup has no adapter
            // command attribution. Keep the existing generic transport class.
            RedisErrorKind::ResponseError
            | RedisErrorKind::ExtensionError
            | RedisErrorKind::TypeError => None,
            _ => Some(Self::Other),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::AuthenticationFailed => "authentication_failed",
            Self::Io => "io",
            Self::Client => "client",
            Self::Retryable => "retryable",
            Self::ClusterUnavailable => "cluster_unavailable",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ObservedService {
    GameProxy,
    GameServer,
    ChatServer,
    MatchService,
    MailService,
    AnnounceService,
}

impl ObservedService {
    pub const ALL: [Self; 6] = [
        Self::GameProxy,
        Self::GameServer,
        Self::ChatServer,
        Self::MatchService,
        Self::MailService,
        Self::AnnounceService,
    ];

    pub fn registry_name(self) -> &'static str {
        match self {
            Self::GameProxy => "game-proxy",
            Self::GameServer => "game-server",
            Self::ChatServer => "chat-server",
            Self::MatchService => "match-service",
            Self::MailService => "mail-service",
            Self::AnnounceService => "announce-service",
        }
    }

    fn required_endpoint(self) -> &'static str {
        match self {
            Self::GameProxy | Self::GameServer => "client",
            Self::ChatServer => "tcp",
            Self::MatchService => "grpc",
            Self::MailService | Self::AnnounceService => "http",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Postgres,
    Redis,
    Nats,
    Host,
}

impl DependencyKind {
    const SHARED: [Self; 3] = [Self::Postgres, Self::Redis, Self::Nats];
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegistryObservationRequest {
    pub run_id: String,
    pub window_start_unix_ms: u64,
    pub window_end_unix_ms: u64,
    pub config: RegistryObservationConfig,
}

impl RegistryObservationRequest {
    pub fn validate(&self) -> Result<(), RegistryObservationError> {
        if self.run_id.trim().is_empty() || self.window_start_unix_ms >= self.window_end_unix_ms {
            return Err(RegistryObservationError::InvalidRequest);
        }
        self.config
            .validate(crate::config::EnvironmentKind::Local)
            .map_err(|_| RegistryObservationError::InvalidRequest)
    }
}

/// Recheck at half the tightest observation budget, with a bounded floor so
/// the synchronous runner never turns registry reads into a sub-10ms poll.
pub fn registry_recheck_interval_ms(config: &RegistryObservationConfig) -> u64 {
    [
        config.max_heartbeat_age_ms,
        config.max_discovery_latency_ms,
        config.max_stale_cleanup_latency_ms,
        config.max_metric_age_ms,
    ]
    .into_iter()
    .min()
    .unwrap_or(MIN_REGISTRY_RECHECK_INTERVAL_MS)
    .saturating_div(2)
    .clamp(
        MIN_REGISTRY_RECHECK_INTERVAL_MS,
        MAX_REGISTRY_RECHECK_INTERVAL_MS,
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EndpointObservation {
    pub name: String,
    pub protocol: String,
    pub visibility: String,
    pub healthy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegistryInstanceObservation {
    pub service: ObservedService,
    pub instance_id: String,
    pub schema_version: u32,
    pub registered_at_unix_ms: u64,
    pub heartbeat_observed_at_unix_ms: u64,
    pub heartbeat_ttl_ms: i64,
    pub healthy: bool,
    pub weight: u32,
    pub endpoints: Vec<EndpointObservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StaleCleanupObservation {
    pub service: ObservedService,
    pub instance_id: String,
    pub heartbeat_expired_at_unix_ms: u64,
    pub removed_at_unix_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RouteConvergenceObservation {
    pub source_service: ObservedService,
    pub target_service: ObservedService,
    pub expected_instance_ids: BTreeSet<String>,
    pub routed_instance_ids: BTreeSet<String>,
    pub registry_changed_at_unix_ms: u64,
    pub observed_at_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InstanceMetricsObservation {
    pub service: ObservedService,
    pub instance_id: String,
    pub reported_at_unix_ms: u64,
    pub received_at_unix_ms: u64,
    pub metrics: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DependencyMetricsObservation {
    pub dependency: DependencyKind,
    pub service: Option<ObservedService>,
    pub instance_id: Option<String>,
    pub reported_at_unix_ms: u64,
    pub metrics: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegistryReadResponse {
    pub run_id: String,
    pub window_start_unix_ms: u64,
    pub window_end_unix_ms: u64,
    pub collection_started_unix_ms: u64,
    pub collected_at_unix_ms: u64,
    pub instances: Vec<RegistryInstanceObservation>,
    pub stale_cleanups: Vec<StaleCleanupObservation>,
    pub routes: Vec<RouteConvergenceObservation>,
    pub instance_metrics: Vec<InstanceMetricsObservation>,
    pub dependency_metrics: Vec<DependencyMetricsObservation>,
}

pub trait RegistryReadTransport {
    fn collect<'a>(
        &'a mut self,
        request: &'a RegistryObservationRequest,
    ) -> Pin<Box<dyn Future<Output = Result<RegistryReadResponse, RegistryObservationError>> + 'a>>;
}

/// Explicit opt-in Redis adapter. `new_lazy` only validates and retains a
/// runtime connection value; it does not establish a connection. It exposes no
/// mutation commands and never serializes or logs its connection string.
pub struct RedisReadonlyRegistryTransport {
    client: redis::Client,
    registry_key_prefix: String,
    metrics_key_prefix: String,
}

impl RedisReadonlyRegistryTransport {
    pub const REDIS_URL_ENV: &'static str = "MYSERVER_LOADTEST_REGISTRY_URL";
    /// Registry keys follow the service-registry prefix, which can differ
    /// from the metrics collector's prefix.
    pub const KEY_PREFIX_ENV: &'static str = "MYSERVER_LOADTEST_REGISTRY_KEY_PREFIX";
    pub const METRICS_KEY_PREFIX_ENV: &'static str = "MYSERVER_LOADTEST_METRICS_KEY_PREFIX";

    /// Builds the adapter from process-local runtime configuration only. The
    /// URL stays inside the Redis client and is deliberately absent from every
    /// serializable observation and report type.
    pub fn from_runtime_environment() -> Result<Self, RegistryObservationError> {
        let redis_url = std::env::var(Self::REDIS_URL_ENV)
            .map_err(|_| RegistryObservationError::InvalidRuntimeConfiguration)?;
        let registry_key_prefix = std::env::var(Self::KEY_PREFIX_ENV).unwrap_or_default();
        let metrics_key_prefix = std::env::var(Self::METRICS_KEY_PREFIX_ENV)
            .unwrap_or_else(|_| registry_key_prefix.clone());
        Self::new_lazy_with_prefixes(&redis_url, &registry_key_prefix, &metrics_key_prefix)
    }

    pub fn new_lazy(redis_url: &str, key_prefix: &str) -> Result<Self, RegistryObservationError> {
        Self::new_lazy_with_prefixes(redis_url, key_prefix, key_prefix)
    }

    pub fn new_lazy_with_prefixes(
        redis_url: &str,
        registry_key_prefix: &str,
        metrics_key_prefix: &str,
    ) -> Result<Self, RegistryObservationError> {
        if redis_url.trim().is_empty() {
            return Err(RegistryObservationError::InvalidRuntimeConfiguration);
        }
        let client = redis::Client::open(redis_url)
            .map_err(|_| RegistryObservationError::InvalidRuntimeConfiguration)?;
        Ok(Self {
            client,
            registry_key_prefix: registry_key_prefix.to_string(),
            metrics_key_prefix: metrics_key_prefix.to_string(),
        })
    }

    async fn collect_readonly(
        &self,
        request: &RegistryObservationRequest,
    ) -> Result<RegistryReadResponse, RegistryObservationError> {
        request.validate()?;
        let collection_started_unix_ms = unix_ms();
        let mut connection = self
            .client
            .get_multiplexed_async_connection()
            .await
            .map_err(registry_redis_connection_error)?;
        let mut instances = Vec::new();
        let mut stale_cleanups = Vec::new();
        let mut instance_metrics = Vec::new();

        for service in ObservedService::ALL {
            let name = service.registry_name();
            let index_key = format!("{}service:{name}:instance-index", self.registry_key_prefix);
            let indexed: Vec<(String, f64)> = redis::cmd("ZRANGE")
                .arg(&index_key)
                .arg(0)
                .arg(MAX_REGISTRY_INSTANCES_PER_SERVICE.saturating_sub(1))
                .arg("WITHSCORES")
                .query_async(&mut connection)
                .await
                .map_err(|error| {
                    registry_redis_command_error(RegistryRedisCommand::Zrange, error)
                })?;
            for (instance_id, heartbeat_score) in indexed {
                let heartbeat_seconds = heartbeat_score_to_seconds(heartbeat_score)?;
                let instance_key = format!(
                    "{}service:{name}:instances:{instance_id}",
                    self.registry_key_prefix
                );
                let heartbeat_key =
                    format!("{}heartbeat:{name}:{instance_id}", self.registry_key_prefix);
                let data: Option<String> =
                    connection
                        .hget(&instance_key, "data")
                        .await
                        .map_err(|error| {
                            registry_redis_command_error(RegistryRedisCommand::Hget, error)
                        })?;
                let ttl_ms: i64 = redis::cmd("PTTL")
                    .arg(&heartbeat_key)
                    .query_async(&mut connection)
                    .await
                    .map_err(|error| {
                        registry_redis_command_error(RegistryRedisCommand::Pttl, error)
                    })?;
                if ttl_ms <= 0 {
                    stale_cleanups.push(StaleCleanupObservation {
                        service,
                        instance_id,
                        heartbeat_expired_at_unix_ms: (heartbeat_seconds as u64)
                            .saturating_mul(1_000)
                            .saturating_add(REGISTRY_HEARTBEAT_TTL_SECONDS.saturating_mul(1_000)),
                        // A single read can prove that cleanup is still pending,
                        // but cannot prove when another process will remove it.
                        removed_at_unix_ms: None,
                    });
                    continue;
                }
                let Some(data) = data else { continue };
                let instance = serde_json::from_str::<ServiceInstance>(&data)
                    .map_err(|_| RegistryObservationError::MalformedRegistryRecord)?
                    .normalized();
                if instance.name != name || instance.id != instance_id {
                    return Err(RegistryObservationError::MalformedRegistryRecord);
                }
                instances.push(project_registry_instance(
                    service,
                    instance,
                    heartbeat_seconds,
                    ttl_ms,
                )?);
            }

            let metrics_index =
                format!("{}metrics:v2:latest-index:{name}", self.metrics_key_prefix);
            let metric_ids: Vec<String> = redis::cmd("ZRANGEBYSCORE")
                .arg(&metrics_index)
                .arg("-inf")
                .arg("+inf")
                .arg("LIMIT")
                .arg(0)
                .arg(MAX_REGISTRY_INSTANCES_PER_SERVICE)
                .query_async(&mut connection)
                .await
                .map_err(|error| {
                    registry_redis_command_error(RegistryRedisCommand::Zrangebyscore, error)
                })?;
            for instance_id in metric_ids {
                let metrics_key = format!(
                    "{}metrics:v2:latest:{name}:{instance_id}",
                    self.metrics_key_prefix
                );
                let fields: BTreeMap<String, String> =
                    connection.hgetall(&metrics_key).await.map_err(|error| {
                        registry_redis_command_error(RegistryRedisCommand::Hgetall, error)
                    })?;
                if let Some(observation) = parse_metrics_hash(service, &instance_id, fields)? {
                    instance_metrics.push(observation);
                }
            }
        }

        Ok(RegistryReadResponse {
            run_id: request.run_id.clone(),
            window_start_unix_ms: request.window_start_unix_ms,
            window_end_unix_ms: request.window_end_unix_ms,
            collection_started_unix_ms,
            collected_at_unix_ms: unix_ms(),
            instances,
            stale_cleanups,
            routes: Vec::new(),
            instance_metrics,
            // PostgreSQL/NATS/host read models are intentionally not guessed
            // from registry keys. A later read-only collector supplies them.
            dependency_metrics: Vec::new(),
        })
    }
}

impl RegistryReadTransport for RedisReadonlyRegistryTransport {
    fn collect<'a>(
        &'a mut self,
        request: &'a RegistryObservationRequest,
    ) -> Pin<Box<dyn Future<Output = Result<RegistryReadResponse, RegistryObservationError>> + 'a>>
    {
        Box::pin(self.collect_readonly(request))
    }
}

#[derive(Debug, Clone)]
pub struct ScriptedRegistryReadTransport {
    responses: VecDeque<Result<RegistryReadResponse, RegistryObservationError>>,
}

impl ScriptedRegistryReadTransport {
    pub fn scripted(
        responses: impl IntoIterator<Item = Result<RegistryReadResponse, RegistryObservationError>>,
    ) -> Self {
        Self {
            responses: responses.into_iter().collect(),
        }
    }
}

impl RegistryReadTransport for ScriptedRegistryReadTransport {
    fn collect<'a>(
        &'a mut self,
        _request: &'a RegistryObservationRequest,
    ) -> Pin<Box<dyn Future<Output = Result<RegistryReadResponse, RegistryObservationError>> + 'a>>
    {
        let response = self
            .responses
            .pop_front()
            .unwrap_or(Err(RegistryObservationError::TransportUnavailable));
        Box::pin(async move { response })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ObservationHoleKind {
    RegistryInstanceMissing,
    RegistryEndpointMissing,
    RegistryHeartbeatStale,
    RegistryDiscoveryLate,
    RegistryStaleCleanupPending,
    RegistryRouteUnconverged,
    ServiceInstanceMetricsMissing,
    ServiceInstanceMetricsStale,
    ClockSkewDetected,
    DependencyMetricsMissing,
    DependencyMetricsStale,
    HostMetricsMissing,
    HostMetricsStale,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ObservationHole {
    pub kind: ObservationHoleKind,
    pub service: Option<ObservedService>,
    pub dependency: Option<DependencyKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegistryObservationReport {
    pub snapshot: ObservationSnapshot,
    pub registry_instance_count: u64,
    pub instance_metric_count: u64,
    pub instances: Vec<RegistryInstanceObservation>,
    pub stale_cleanups: Vec<StaleCleanupObservation>,
    pub routes: Vec<RouteConvergenceObservation>,
    pub holes: BTreeSet<ObservationHole>,
}

impl RegistryObservationReport {
    pub fn merge_into_metrics(&self, metrics: &mut Metrics) {
        metrics.increment("registry_read_observations", 1);
        metrics.increment("registry_instances_observed", self.registry_instance_count);
        metrics.increment(
            "registry_instance_metrics_observed",
            self.instance_metric_count,
        );
        if self.snapshot.complete {
            metrics.increment("registry_observation_complete", 1);
        }
        for hole in &self.holes {
            metrics.increment("registry_observation_holes", 1);
            match hole.kind {
                ObservationHoleKind::RegistryHeartbeatStale => {
                    metrics.increment("registry_heartbeat_stale", 1)
                }
                ObservationHoleKind::RegistryDiscoveryLate => {
                    metrics.increment("registry_discovery_late", 1)
                }
                ObservationHoleKind::RegistryStaleCleanupPending => {
                    metrics.increment("registry_stale_cleanup_pending", 1)
                }
                ObservationHoleKind::RegistryRouteUnconverged => {
                    metrics.increment("registry_route_unconverged", 1)
                }
                ObservationHoleKind::ServiceInstanceMetricsMissing => {
                    metrics.increment("registry_instance_metrics_missing", 1)
                }
                ObservationHoleKind::ServiceInstanceMetricsStale => {
                    metrics.increment("registry_instance_metrics_stale", 1)
                }
                ObservationHoleKind::ClockSkewDetected => {
                    metrics.increment("registry_clock_skew_detected", 1)
                }
                ObservationHoleKind::DependencyMetricsMissing => {
                    metrics.increment("registry_dependency_metrics_missing", 1)
                }
                ObservationHoleKind::DependencyMetricsStale => {
                    metrics.increment("registry_dependency_metrics_stale", 1)
                }
                ObservationHoleKind::HostMetricsMissing => {
                    metrics.increment("registry_host_metrics_missing", 1)
                }
                ObservationHoleKind::HostMetricsStale => {
                    metrics.increment("registry_host_metrics_stale", 1)
                }
                ObservationHoleKind::RegistryInstanceMissing
                | ObservationHoleKind::RegistryEndpointMissing => {
                    metrics.increment("registry_instance_observation_missing", 1)
                }
            }
        }
    }
}

pub async fn collect_registry_observation<T: RegistryReadTransport>(
    transport: &mut T,
    request: &RegistryObservationRequest,
    now_unix_ms: u64,
) -> Result<RegistryObservationReport, RegistryObservationError> {
    request.validate()?;
    let response = transport.collect(request).await?;
    evaluate_registry_observation(request, response, now_unix_ms)
}

/// Opens a short-lived current-thread runtime only when the caller has
/// explicitly enabled registry observation in a validated local/test run.
/// Constructing the adapter remains side-effect free; the Redis connection is
/// opened by `collect` and its URL never leaves the runtime client.
pub fn collect_runtime_registry_observation(
    request: &RegistryObservationRequest,
) -> Result<RegistryObservationReport, RegistryObservationError> {
    let mut transport = RedisReadonlyRegistryTransport::from_runtime_environment()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|_| RegistryObservationError::TransportUnavailable)?;
    runtime.block_on(async {
        request.validate()?;
        let response = transport.collect(request).await?;
        evaluate_registry_observation(request, response, unix_ms())
    })
}

pub fn evaluate_registry_observation(
    request: &RegistryObservationRequest,
    response: RegistryReadResponse,
    now_unix_ms: u64,
) -> Result<RegistryObservationReport, RegistryObservationError> {
    request.validate()?;
    if response.run_id != request.run_id
        || response.window_start_unix_ms != request.window_start_unix_ms
        || response.window_end_unix_ms != request.window_end_unix_ms
        || response.collection_started_unix_ms < request.window_start_unix_ms
        || response.collected_at_unix_ms < response.collection_started_unix_ms
        || response.collected_at_unix_ms > now_unix_ms
    {
        return Err(RegistryObservationError::InconsistentWindow);
    }

    let mut holes = BTreeSet::new();
    let discovery_latency_ms = response
        .collected_at_unix_ms
        .saturating_sub(response.collection_started_unix_ms);
    if discovery_latency_ms > request.config.max_discovery_latency_ms {
        holes.insert(hole(ObservationHoleKind::RegistryDiscoveryLate, None, None));
    }

    let mut active_by_service = BTreeMap::<ObservedService, BTreeSet<String>>::new();
    for instance in &response.instances {
        validate_instance(instance)?;
        if instance.heartbeat_observed_at_unix_ms > response.collected_at_unix_ms {
            holes.insert(hole(
                ObservationHoleKind::ClockSkewDetected,
                Some(instance.service),
                None,
            ));
        }
        let heartbeat_age = response
            .collected_at_unix_ms
            .saturating_sub(instance.heartbeat_observed_at_unix_ms);
        let discoverable = instance.healthy
            && instance.weight > 0
            && instance.endpoints.iter().any(|endpoint| {
                endpoint.name == instance.service.required_endpoint() && endpoint.healthy
            });
        if instance.heartbeat_ttl_ms <= 0 || heartbeat_age > request.config.max_heartbeat_age_ms {
            holes.insert(hole(
                ObservationHoleKind::RegistryHeartbeatStale,
                Some(instance.service),
                None,
            ));
        } else if discoverable {
            active_by_service
                .entry(instance.service)
                .or_default()
                .insert(instance.instance_id.clone());
        }
        if !instance
            .endpoints
            .iter()
            .any(|endpoint| endpoint.name == instance.service.required_endpoint())
        {
            holes.insert(hole(
                ObservationHoleKind::RegistryEndpointMissing,
                Some(instance.service),
                None,
            ));
        }
    }

    for service in ObservedService::ALL {
        if active_by_service
            .get(&service)
            .is_none_or(BTreeSet::is_empty)
        {
            holes.insert(hole(
                ObservationHoleKind::RegistryInstanceMissing,
                Some(service),
                None,
            ));
        }
    }

    for cleanup in &response.stale_cleanups {
        if cleanup.instance_id.trim().is_empty()
            || cleanup.heartbeat_expired_at_unix_ms > response.collected_at_unix_ms
            || cleanup
                .removed_at_unix_ms
                .is_some_and(|removed| removed < cleanup.heartbeat_expired_at_unix_ms)
        {
            return Err(RegistryObservationError::MalformedRegistryRecord);
        }
        if cleanup.removed_at_unix_ms.is_none_or(|removed| {
            removed.saturating_sub(cleanup.heartbeat_expired_at_unix_ms)
                > request.config.max_stale_cleanup_latency_ms
        }) {
            holes.insert(hole(
                ObservationHoleKind::RegistryStaleCleanupPending,
                Some(cleanup.service),
                None,
            ));
        }
    }

    for route in &response.routes {
        if route.source_service != ObservedService::GameProxy
            || route.target_service != ObservedService::GameServer
            || route.registry_changed_at_unix_ms > route.observed_at_unix_ms
            || route.observed_at_unix_ms > response.collected_at_unix_ms
        {
            return Err(RegistryObservationError::MalformedRouteObservation);
        }
        let expected = active_by_service
            .get(&route.target_service)
            .cloned()
            .unwrap_or_default();
        if route.expected_instance_ids != expected
            || route.routed_instance_ids != expected
            || route
                .observed_at_unix_ms
                .saturating_sub(route.registry_changed_at_unix_ms)
                > request.config.max_discovery_latency_ms
        {
            holes.insert(hole(
                ObservationHoleKind::RegistryRouteUnconverged,
                Some(route.target_service),
                None,
            ));
        }
    }
    if !active_by_service
        .get(&ObservedService::GameServer)
        .is_none_or(BTreeSet::is_empty)
        && response.routes.is_empty()
    {
        holes.insert(hole(
            ObservationHoleKind::RegistryRouteUnconverged,
            Some(ObservedService::GameServer),
            None,
        ));
    }

    let mut metrics_by_instance = BTreeSet::new();
    for metrics in &response.instance_metrics {
        validate_metric_observation(metrics)?;
        if metrics.reported_at_unix_ms > response.collected_at_unix_ms
            || metrics.received_at_unix_ms > response.collected_at_unix_ms
        {
            holes.insert(hole(
                ObservationHoleKind::ClockSkewDetected,
                Some(metrics.service),
                None,
            ));
        }
        let metric_age = response
            .collected_at_unix_ms
            .saturating_sub(metrics.reported_at_unix_ms);
        if metric_age > request.config.max_metric_age_ms {
            holes.insert(hole(
                ObservationHoleKind::ServiceInstanceMetricsStale,
                Some(metrics.service),
                None,
            ));
        }
        metrics_by_instance.insert((metrics.service, metrics.instance_id.clone()));
    }
    for (service, instances) in &active_by_service {
        for instance_id in instances {
            if !metrics_by_instance.contains(&(*service, instance_id.clone())) {
                holes.insert(hole(
                    ObservationHoleKind::ServiceInstanceMetricsMissing,
                    Some(*service),
                    None,
                ));
            }
        }
    }

    for dependency in DependencyKind::SHARED {
        observe_dependency(
            dependency,
            None,
            &response.dependency_metrics,
            response.collected_at_unix_ms,
            request.config.max_metric_age_ms,
            &mut holes,
        );
    }
    for (service, instances) in &active_by_service {
        for instance_id in instances {
            observe_dependency(
                DependencyKind::Host,
                Some((*service, instance_id)),
                &response.dependency_metrics,
                response.collected_at_unix_ms,
                request.config.max_metric_age_ms,
                &mut holes,
            );
        }
    }

    Ok(RegistryObservationReport {
        snapshot: ObservationSnapshot {
            run_id: response.run_id,
            window_start_unix_ms: response.window_start_unix_ms,
            window_end_unix_ms: response.window_end_unix_ms,
            source: REGISTRY_OBSERVATION_SOURCE.into(),
            freshness_ms: now_unix_ms.saturating_sub(response.collected_at_unix_ms),
            complete: holes.is_empty(),
        },
        registry_instance_count: response.instances.len() as u64,
        instance_metric_count: response.instance_metrics.len() as u64,
        instances: response.instances,
        stale_cleanups: response.stale_cleanups,
        routes: response.routes,
        holes,
    })
}

fn observe_dependency(
    dependency: DependencyKind,
    target: Option<(ObservedService, &String)>,
    observations: &[DependencyMetricsObservation],
    collected_at_unix_ms: u64,
    max_metric_age_ms: u64,
    holes: &mut BTreeSet<ObservationHole>,
) {
    let matching = observations.iter().find(|observation| {
        observation.dependency == dependency
            && match target {
                Some((service, instance_id)) => {
                    observation.service == Some(service)
                        && observation.instance_id.as_deref() == Some(instance_id.as_str())
                }
                None => observation.service.is_none() && observation.instance_id.is_none(),
            }
    });
    let (missing, stale) = match matching {
        None => (true, false),
        Some(observation) => (
            false,
            observation.metrics.is_empty()
                || collected_at_unix_ms.saturating_sub(observation.reported_at_unix_ms)
                    > max_metric_age_ms,
        ),
    };
    let service = target.map(|(service, _)| service);
    if missing {
        holes.insert(hole(
            if dependency == DependencyKind::Host {
                ObservationHoleKind::HostMetricsMissing
            } else {
                ObservationHoleKind::DependencyMetricsMissing
            },
            service,
            Some(dependency),
        ));
    }
    if stale {
        holes.insert(hole(
            if dependency == DependencyKind::Host {
                ObservationHoleKind::HostMetricsStale
            } else {
                ObservationHoleKind::DependencyMetricsStale
            },
            service,
            Some(dependency),
        ));
    }
}

fn hole(
    kind: ObservationHoleKind,
    service: Option<ObservedService>,
    dependency: Option<DependencyKind>,
) -> ObservationHole {
    ObservationHole {
        kind,
        service,
        dependency,
    }
}

fn validate_instance(
    instance: &RegistryInstanceObservation,
) -> Result<(), RegistryObservationError> {
    if instance.schema_version != SERVICE_INSTANCE_SCHEMA_VERSION
        || instance.instance_id.trim().is_empty()
        || instance.registered_at_unix_ms == 0
        || instance.heartbeat_observed_at_unix_ms == 0
        || instance.endpoints.is_empty()
        || instance.endpoints.iter().any(|endpoint| {
            endpoint.name.trim().is_empty()
                || endpoint.protocol.trim().is_empty()
                || endpoint.visibility.trim().is_empty()
        })
    {
        return Err(RegistryObservationError::MalformedRegistryRecord);
    }
    Ok(())
}

fn validate_metric_observation(
    metrics: &InstanceMetricsObservation,
) -> Result<(), RegistryObservationError> {
    if metrics.instance_id.trim().is_empty()
        || metrics.reported_at_unix_ms == 0
        || metrics.received_at_unix_ms < metrics.reported_at_unix_ms
        || metrics.metrics.is_empty()
        || metrics.metrics.len() > MAX_METRIC_FIELDS_PER_INSTANCE
        || metrics.metrics.keys().any(|key| !valid_metric_key(key))
    {
        return Err(RegistryObservationError::MalformedMetricsRecord);
    }
    Ok(())
}

fn valid_metric_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 96
        && key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn project_registry_instance(
    service: ObservedService,
    instance: ServiceInstance,
    heartbeat_seconds: i64,
    heartbeat_ttl_ms: i64,
) -> Result<RegistryInstanceObservation, RegistryObservationError> {
    if heartbeat_seconds <= 0 || instance.registered_at < 0 || instance.endpoints.is_empty() {
        return Err(RegistryObservationError::MalformedRegistryRecord);
    }
    Ok(RegistryInstanceObservation {
        service,
        instance_id: instance.id,
        schema_version: instance.schema_version,
        registered_at_unix_ms: (instance.registered_at as u64).saturating_mul(1_000),
        heartbeat_observed_at_unix_ms: (heartbeat_seconds as u64).saturating_mul(1_000),
        heartbeat_ttl_ms,
        healthy: instance.healthy,
        weight: instance.weight,
        endpoints: instance
            .endpoints
            .into_iter()
            .map(|endpoint| EndpointObservation {
                name: endpoint.name,
                protocol: endpoint.protocol,
                visibility: endpoint.visibility,
                healthy: endpoint.healthy,
            })
            .collect(),
    })
}

fn heartbeat_score_to_seconds(score: f64) -> Result<i64, RegistryObservationError> {
    if !score.is_finite() || score <= 0.0 || score > i64::MAX as f64 {
        return Err(RegistryObservationError::MalformedRegistryRecord);
    }
    Ok(score as i64)
}

fn parse_metrics_hash(
    service: ObservedService,
    instance_id: &str,
    fields: BTreeMap<String, String>,
) -> Result<Option<InstanceMetricsObservation>, RegistryObservationError> {
    if fields.is_empty() {
        return Ok(None);
    }
    let expected_service = service.registry_name();
    if fields.get("_schema").map(String::as_str) != Some("metrics-v2")
        || fields.get("_service").map(String::as_str) != Some(expected_service)
        || fields.get("_instance_id").map(String::as_str) != Some(instance_id)
    {
        return Err(RegistryObservationError::MalformedMetricsRecord);
    }
    let parse_timestamp = |name: &str| {
        fields
            .get(name)
            .and_then(|value| value.parse::<u64>().ok())
            .map(|seconds| seconds.saturating_mul(1_000))
            .ok_or(RegistryObservationError::MalformedMetricsRecord)
    };
    let reported_at_unix_ms = parse_timestamp("_reported_at")?;
    let received_at_unix_ms = parse_timestamp("_received_at")?;
    let metrics = fields
        .into_iter()
        .filter(|(key, _)| !key.starts_with('_') && key != "instance_id")
        .filter_map(|(key, value)| value.parse::<u64>().ok().map(|value| (key, value)))
        .collect::<BTreeMap<_, _>>();
    Ok(Some(InstanceMetricsObservation {
        service,
        instance_id: instance_id.to_string(),
        reported_at_unix_ms,
        received_at_unix_ms,
        metrics,
    }))
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryObservationError {
    InvalidRequest,
    InvalidRuntimeConfiguration,
    TransportUnavailable,
    RedisConnectionFailed {
        class: RegistryRedisConnectionErrorClass,
    },
    RedisCommandRejected {
        command: RegistryRedisCommand,
    },
    RedisCommandFailed {
        command: RegistryRedisCommand,
        class: RegistryRedisErrorClass,
    },
    InconsistentWindow,
    MalformedRegistryRecord,
    MalformedRouteObservation,
    MalformedMetricsRecord,
}

impl RegistryObservationError {
    /// Safe report metadata. This intentionally drops Redis details because
    /// they may include key names, instance IDs, routes, or credentials.
    pub fn report_category(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "registry_observation_invalid_request",
            Self::InvalidRuntimeConfiguration => "registry_observation_invalid_runtime",
            Self::TransportUnavailable => "registry_observation_transport_unavailable",
            Self::RedisConnectionFailed { .. } => "registry_redis_connection_failed",
            Self::RedisCommandRejected { .. } => "registry_redis_command_rejected",
            Self::RedisCommandFailed { .. } => "registry_redis_command_failed",
            Self::InconsistentWindow => "registry_observation_inconsistent_window",
            Self::MalformedRegistryRecord => "registry_observation_malformed_registry_record",
            Self::MalformedRouteObservation => "registry_observation_malformed_route_record",
            Self::MalformedMetricsRecord => "registry_observation_malformed_metrics_record",
        }
    }

    pub fn report_message(&self) -> &'static str {
        match self {
            Self::InvalidRequest => "read-only registry observation request is invalid",
            Self::InvalidRuntimeConfiguration => {
                "read-only registry observer runtime configuration is invalid"
            }
            Self::TransportUnavailable => "read-only registry transport could not be established",
            Self::RedisConnectionFailed { .. } => "read-only registry Redis connection failed",
            Self::RedisCommandRejected { .. } => {
                "read-only registry Redis command was rejected by its access policy"
            }
            Self::RedisCommandFailed { .. } => "read-only registry Redis command failed",
            Self::InconsistentWindow => {
                "read-only registry observation returned an inconsistent window"
            }
            Self::MalformedRegistryRecord => {
                "read-only registry observation returned malformed data"
            }
            Self::MalformedRouteObservation => {
                "read-only registry observation returned malformed route data"
            }
            Self::MalformedMetricsRecord => {
                "read-only registry observation returned malformed metrics data"
            }
        }
    }

    pub fn report_context(&self) -> BTreeMap<String, String> {
        let mut context = BTreeMap::new();
        match self {
            Self::RedisConnectionFailed { class } => {
                context.insert("redis_connection_error_class".into(), class.as_str().into());
            }
            Self::RedisCommandRejected { command } => {
                context.insert("redis_command".into(), command.as_str().into());
                context.insert("redis_error_class".into(), "permission_denied".into());
            }
            Self::RedisCommandFailed { command, class } => {
                context.insert("redis_command".into(), command.as_str().into());
                context.insert("redis_error_class".into(), class.as_str().into());
            }
            _ => {}
        }
        context
    }
}

fn registry_redis_connection_error(error: RedisError) -> RegistryObservationError {
    RegistryRedisConnectionErrorClass::from_redis_error(&error)
        .map(|class| RegistryObservationError::RedisConnectionFailed { class })
        .unwrap_or(RegistryObservationError::TransportUnavailable)
}

fn registry_redis_command_error(
    command: RegistryRedisCommand,
    error: RedisError,
) -> RegistryObservationError {
    let class = RegistryRedisErrorClass::from_redis_error(&error);
    if class == RegistryRedisErrorClass::PermissionDenied {
        RegistryObservationError::RedisCommandRejected { command }
    } else {
        RegistryObservationError::RedisCommandFailed { command, class }
    }
}

impl std::fmt::Display for RegistryObservationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidRequest => "registry observation request is invalid",
            Self::InvalidRuntimeConfiguration => {
                "registry observer runtime configuration is invalid"
            }
            Self::TransportUnavailable => "registry read-only transport is unavailable",
            Self::RedisConnectionFailed { .. } => "registry read-only Redis connection failed",
            Self::RedisCommandRejected { .. } => "registry read-only Redis command was rejected",
            Self::RedisCommandFailed { .. } => "registry read-only Redis command failed",
            Self::InconsistentWindow => "registry observation run_id or window is inconsistent",
            Self::MalformedRegistryRecord => "registry observation has an invalid instance record",
            Self::MalformedRouteObservation => "registry observation has an invalid route record",
            Self::MalformedMetricsRecord => "registry observation has an invalid metrics record",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for RegistryObservationError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RegistryObservationConfig {
        RegistryObservationConfig {
            read_only: true,
            max_heartbeat_age_ms: 5_000,
            max_discovery_latency_ms: 500,
            max_stale_cleanup_latency_ms: 5_000,
            max_metric_age_ms: 5_000,
        }
    }

    fn request() -> RegistryObservationRequest {
        RegistryObservationRequest {
            run_id: "run-registry-1".into(),
            window_start_unix_ms: 10_000,
            window_end_unix_ms: 20_000,
            config: config(),
        }
    }

    fn instance(service: ObservedService, id: &str) -> RegistryInstanceObservation {
        RegistryInstanceObservation {
            service,
            instance_id: id.into(),
            schema_version: SERVICE_INSTANCE_SCHEMA_VERSION,
            registered_at_unix_ms: 1_000,
            heartbeat_observed_at_unix_ms: 19_900,
            heartbeat_ttl_ms: 20_000,
            healthy: true,
            weight: 100,
            endpoints: vec![EndpointObservation {
                name: service.required_endpoint().into(),
                protocol: "http".into(),
                visibility: "internal".into(),
                healthy: true,
            }],
        }
    }

    fn metric(service: ObservedService, id: &str) -> InstanceMetricsObservation {
        InstanceMetricsObservation {
            service,
            instance_id: id.into(),
            reported_at_unix_ms: 19_900,
            received_at_unix_ms: 19_950,
            metrics: BTreeMap::from([("qps".into(), 1), ("latency_ms".into(), 3)]),
        }
    }

    fn dependency(kind: DependencyKind) -> DependencyMetricsObservation {
        DependencyMetricsObservation {
            dependency: kind,
            service: None,
            instance_id: None,
            reported_at_unix_ms: 19_900,
            metrics: BTreeMap::from([("healthy".into(), 1)]),
        }
    }

    fn response() -> RegistryReadResponse {
        let instances = ObservedService::ALL
            .into_iter()
            .map(|service| instance(service, service.registry_name()))
            .collect::<Vec<_>>();
        let instance_metrics = ObservedService::ALL
            .into_iter()
            .map(|service| metric(service, service.registry_name()))
            .collect::<Vec<_>>();
        let mut dependency_metrics = DependencyKind::SHARED
            .into_iter()
            .map(dependency)
            .collect::<Vec<_>>();
        for service in ObservedService::ALL {
            dependency_metrics.push(DependencyMetricsObservation {
                dependency: DependencyKind::Host,
                service: Some(service),
                instance_id: Some(service.registry_name().into()),
                reported_at_unix_ms: 19_900,
                metrics: BTreeMap::from([("working_set_bytes".into(), 1)]),
            });
        }
        RegistryReadResponse {
            run_id: "run-registry-1".into(),
            window_start_unix_ms: 10_000,
            window_end_unix_ms: 20_000,
            collection_started_unix_ms: 20_000,
            collected_at_unix_ms: 20_100,
            instances,
            stale_cleanups: vec![StaleCleanupObservation {
                service: ObservedService::GameServer,
                instance_id: "game-server-old".into(),
                heartbeat_expired_at_unix_ms: 15_000,
                removed_at_unix_ms: Some(15_100),
            }],
            routes: vec![RouteConvergenceObservation {
                source_service: ObservedService::GameProxy,
                target_service: ObservedService::GameServer,
                expected_instance_ids: ["game-server".into()].into(),
                routed_instance_ids: ["game-server".into()].into(),
                registry_changed_at_unix_ms: 19_900,
                observed_at_unix_ms: 20_000,
            }],
            instance_metrics,
            dependency_metrics,
        }
    }

    fn empty_response() -> RegistryReadResponse {
        RegistryReadResponse {
            run_id: "run-registry-1".into(),
            window_start_unix_ms: 10_000,
            window_end_unix_ms: 20_000,
            collection_started_unix_ms: 20_000,
            collected_at_unix_ms: 20_100,
            instances: Vec::new(),
            stale_cleanups: Vec::new(),
            routes: Vec::new(),
            instance_metrics: Vec::new(),
            dependency_metrics: Vec::new(),
        }
    }

    #[test]
    fn redis_command_errors_are_classified_without_exporting_details() {
        let rejected = registry_redis_command_error(
            RegistryRedisCommand::Zrange,
            RedisError::from((
                RedisErrorKind::ResponseError,
                "server error",
                "NOPERM key=service:private password=top-secret redis://observer:secret@host"
                    .to_string(),
            )),
        );
        assert!(matches!(
            rejected,
            RegistryObservationError::RedisCommandRejected {
                command: RegistryRedisCommand::Zrange
            }
        ));
        assert_eq!(
            rejected.report_context(),
            BTreeMap::from([
                ("redis_command".into(), "zrange".into()),
                ("redis_error_class".into(), "permission_denied".into()),
            ])
        );

        let failed = registry_redis_command_error(
            RegistryRedisCommand::Hgetall,
            RedisError::from((
                RedisErrorKind::TypeError,
                "server error",
                "WRONGTYPE key=metrics:v2:latest:private token=top-secret".to_string(),
            )),
        );
        assert!(matches!(
            failed,
            RegistryObservationError::RedisCommandFailed {
                command: RegistryRedisCommand::Hgetall,
                class: RegistryRedisErrorClass::TypeError,
            }
        ));
        let report_text = serde_json::to_string(&serde_json::json!({
            "category": failed.report_category(),
            "message": failed.report_message(),
            "context": failed.report_context(),
        }))
        .unwrap();
        assert!(report_text.contains("hgetall"));
        assert!(report_text.contains("type_error"));
        for forbidden in [
            "service:private",
            "metrics:v2:latest:private",
            "top-secret",
            "redis://",
            "WRONGTYPE",
        ] {
            assert!(!report_text.contains(forbidden));
        }
    }

    #[test]
    fn redis_connection_errors_are_classified_without_command_or_details() {
        let cases = [
            (
                RedisErrorKind::AuthenticationFailed,
                RegistryRedisConnectionErrorClass::AuthenticationFailed,
            ),
            (
                RedisErrorKind::IoError,
                RegistryRedisConnectionErrorClass::Io,
            ),
            (
                RedisErrorKind::ClientError,
                RegistryRedisConnectionErrorClass::Client,
            ),
            (
                RedisErrorKind::BusyLoadingError,
                RegistryRedisConnectionErrorClass::Retryable,
            ),
            (
                RedisErrorKind::ClusterDown,
                RegistryRedisConnectionErrorClass::ClusterUnavailable,
            ),
            (
                RedisErrorKind::ReadOnly,
                RegistryRedisConnectionErrorClass::Other,
            ),
        ];

        for (kind, class) in cases {
            let error = registry_redis_connection_error(RedisError::from((
                kind,
                "connection failed",
                "redis://observer:secret@host token=top-secret key=service:private".to_string(),
            )));
            assert_eq!(
                error,
                RegistryObservationError::RedisConnectionFailed { class }
            );
            assert_eq!(
                error.report_context(),
                BTreeMap::from([("redis_connection_error_class".into(), class.as_str().into(),)])
            );

            let report_text = serde_json::to_string(&serde_json::json!({
                "category": error.report_category(),
                "message": error.report_message(),
                "context": error.report_context(),
            }))
            .unwrap();
            assert!(report_text.contains("registry_redis_connection_failed"));
            assert!(report_text.contains(class.as_str()));
            assert!(!report_text.contains("redis_command"));
            for forbidden in ["redis://", "top-secret", "service:private", "token="] {
                assert!(!report_text.contains(forbidden));
            }
        }
    }

    #[test]
    fn unattributable_connection_responses_keep_transport_generic() {
        let error = registry_redis_connection_error(RedisError::from((
            RedisErrorKind::ResponseError,
            "server error",
            "NOPERM key=service:private password=top-secret".to_string(),
        )));
        assert_eq!(error, RegistryObservationError::TransportUnavailable);
        assert_eq!(
            error.report_category(),
            "registry_observation_transport_unavailable"
        );
        assert!(error.report_context().is_empty());
        let report_text = serde_json::to_string(&serde_json::json!({
            "category": error.report_category(),
            "message": error.report_message(),
            "context": error.report_context(),
        }))
        .unwrap();
        for forbidden in ["NOPERM", "service:private", "top-secret"] {
            assert!(!report_text.contains(forbidden));
        }
    }

    #[test]
    fn malformed_metrics_records_keep_a_static_safe_report_category() {
        let error = parse_metrics_hash(
            ObservedService::GameServer,
            "instance-private",
            BTreeMap::from([("_schema".into(), "unexpected-schema".into())]),
        )
        .unwrap_err();
        assert_eq!(error, RegistryObservationError::MalformedMetricsRecord);
        assert_eq!(
            error.report_category(),
            "registry_observation_malformed_metrics_record"
        );
        assert!(error.report_context().is_empty());
    }

    #[tokio::test]
    async fn empty_registry_indexes_remain_observation_holes_not_transport_errors() {
        let mut transport = ScriptedRegistryReadTransport::scripted([Ok(empty_response())]);
        let report = collect_registry_observation(&mut transport, &request(), 20_200).await;
        assert!(report.is_ok());
        let report = report.unwrap();
        assert!(!report.snapshot.complete);
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::RegistryInstanceMissing,
            Some(ObservedService::GameServer),
            None,
        )));
    }

    #[tokio::test]
    async fn scripted_read_only_collection_tracks_complete_instance_observation() {
        let request = request();
        let mut transport = ScriptedRegistryReadTransport::scripted([Ok(response())]);
        let report = collect_registry_observation(&mut transport, &request, 20_200)
            .await
            .unwrap();
        assert!(report.snapshot.complete);
        assert_eq!(report.registry_instance_count, 6);
        assert_eq!(report.instance_metric_count, 6);
        assert!(report.holes.is_empty());

        let mut metrics = Metrics::default();
        report.merge_into_metrics(&mut metrics);
        assert_eq!(
            metrics.snapshot().counters["registry_observation_complete"],
            1
        );
    }

    #[tokio::test]
    async fn missing_stale_and_unconverged_inputs_become_explicit_holes() {
        let request = request();
        let mut response = response();
        response
            .instances
            .retain(|item| item.service != ObservedService::MailService);
        response
            .instance_metrics
            .retain(|item| item.service != ObservedService::ChatServer);
        response.routes[0].routed_instance_ids.clear();
        response
            .dependency_metrics
            .retain(|item| item.dependency != DependencyKind::Nats);
        response.dependency_metrics.retain(|item| {
            !(item.dependency == DependencyKind::Host
                && item.service == Some(ObservedService::GameProxy))
        });
        let mut transport = ScriptedRegistryReadTransport::scripted([Ok(response)]);
        let report = collect_registry_observation(&mut transport, &request, 20_200)
            .await
            .unwrap();
        assert!(!report.snapshot.complete);
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::RegistryInstanceMissing,
            Some(ObservedService::MailService),
            None,
        )));
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::ServiceInstanceMetricsMissing,
            Some(ObservedService::ChatServer),
            None,
        )));
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::RegistryRouteUnconverged,
            Some(ObservedService::GameServer),
            None,
        )));
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::DependencyMetricsMissing,
            None,
            Some(DependencyKind::Nats),
        )));
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::HostMetricsMissing,
            Some(ObservedService::GameProxy),
            Some(DependencyKind::Host),
        )));
    }

    #[test]
    fn inconsistent_run_or_window_is_rejected_before_reporting() {
        let request = request();
        let mut mismatched_response = response();
        mismatched_response.run_id = "other-run".into();
        assert_eq!(
            evaluate_registry_observation(&request, mismatched_response, 20_200),
            Err(RegistryObservationError::InconsistentWindow)
        );

        let mut missing_run = request.clone();
        missing_run.run_id = " ".into();
        assert_eq!(
            evaluate_registry_observation(&missing_run, response(), 20_200),
            Err(RegistryObservationError::InvalidRequest)
        );

        let mut invalid_window = request;
        invalid_window.window_end_unix_ms = invalid_window.window_start_unix_ms;
        assert_eq!(
            evaluate_registry_observation(&invalid_window, response(), 20_200),
            Err(RegistryObservationError::InvalidRequest)
        );
    }

    #[test]
    fn stale_heartbeat_cleanup_and_multi_instance_route_convergence_are_explicit() {
        let request = request();
        let mut stale = response();
        let game_server = stale
            .instances
            .iter_mut()
            .find(|item| item.service == ObservedService::GameServer)
            .unwrap();
        game_server.heartbeat_observed_at_unix_ms = 10_000;
        game_server.heartbeat_ttl_ms = 0;
        stale.stale_cleanups = vec![StaleCleanupObservation {
            service: ObservedService::GameServer,
            instance_id: "game-server".into(),
            heartbeat_expired_at_unix_ms: 15_000,
            removed_at_unix_ms: None,
        }];
        let report = evaluate_registry_observation(&request, stale, 20_200).unwrap();
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::RegistryHeartbeatStale,
            Some(ObservedService::GameServer),
            None,
        )));
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::RegistryStaleCleanupPending,
            Some(ObservedService::GameServer),
            None,
        )));
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::RegistryInstanceMissing,
            Some(ObservedService::GameServer),
            None,
        )));

        let mut converged = response();
        converged.instances.push(instance(
            ObservedService::GameServer,
            "game-server-secondary",
        ));
        converged
            .instance_metrics
            .push(metric(ObservedService::GameServer, "game-server-secondary"));
        converged
            .dependency_metrics
            .push(DependencyMetricsObservation {
                dependency: DependencyKind::Host,
                service: Some(ObservedService::GameServer),
                instance_id: Some("game-server-secondary".into()),
                reported_at_unix_ms: 19_900,
                metrics: BTreeMap::from([("working_set_bytes".into(), 1)]),
            });
        converged.routes[0].expected_instance_ids =
            ["game-server".into(), "game-server-secondary".into()].into();
        converged.routes[0].routed_instance_ids = converged.routes[0].expected_instance_ids.clone();
        assert!(
            evaluate_registry_observation(&request, converged.clone(), 20_200)
                .unwrap()
                .snapshot
                .complete
        );

        converged.routes[0]
            .routed_instance_ids
            .remove("game-server-secondary");
        let report = evaluate_registry_observation(&request, converged, 20_200).unwrap();
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::RegistryRouteUnconverged,
            Some(ObservedService::GameServer),
            None,
        )));
    }

    #[test]
    fn only_healthy_weighted_endpoints_are_considered_discoverable() {
        let request = request();
        let mut unavailable = response();
        let game_server = unavailable
            .instances
            .iter_mut()
            .find(|item| item.service == ObservedService::GameServer)
            .unwrap();
        game_server.healthy = false;
        game_server.endpoints[0].healthy = false;
        game_server.weight = 0;

        let report = evaluate_registry_observation(&request, unavailable, 20_200).unwrap();
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::RegistryInstanceMissing,
            Some(ObservedService::GameServer),
            None,
        )));
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::RegistryRouteUnconverged,
            Some(ObservedService::GameServer),
            None,
        )));
    }

    #[test]
    fn future_service_timestamps_are_clock_skew_observation_holes() {
        let request = request();
        let mut response = response();
        let game_server = response
            .instances
            .iter_mut()
            .find(|item| item.service == ObservedService::GameServer)
            .unwrap();
        game_server.heartbeat_observed_at_unix_ms = 20_101;
        let game_server_metrics = response
            .instance_metrics
            .iter_mut()
            .find(|item| item.service == ObservedService::GameServer)
            .unwrap();
        game_server_metrics.reported_at_unix_ms = 20_101;
        game_server_metrics.received_at_unix_ms = 20_102;

        let report = evaluate_registry_observation(&request, response, 20_200).unwrap();
        assert!(report.holes.contains(&hole(
            ObservationHoleKind::ClockSkewDetected,
            Some(ObservedService::GameServer),
            None,
        )));
    }

    #[test]
    fn runtime_url_is_never_kept_in_a_serializable_observation() {
        let transport = RedisReadonlyRegistryTransport::new_lazy("redis://127.0.0.1:6379", "");
        assert!(transport.is_ok());
        let json = serde_json::to_string(&response()).unwrap();
        assert!(!json.contains("redis://"));
    }

    #[test]
    fn registry_and_metrics_prefixes_are_independent_runtime_inputs() {
        let transport = RedisReadonlyRegistryTransport::new_lazy_with_prefixes(
            "redis://127.0.0.1:6379",
            "registry:",
            "metrics:",
        )
        .unwrap();
        assert_eq!(transport.registry_key_prefix, "registry:");
        assert_eq!(transport.metrics_key_prefix, "metrics:");
    }

    #[test]
    fn recheck_interval_is_bounded_and_derived_from_the_tightest_budget() {
        assert_eq!(registry_recheck_interval_ms(&config()), 250);
        let tight = RegistryObservationConfig {
            max_metric_age_ms: 1,
            ..config()
        };
        assert_eq!(
            registry_recheck_interval_ms(&tight),
            MIN_REGISTRY_RECHECK_INTERVAL_MS
        );
        let relaxed = RegistryObservationConfig {
            max_heartbeat_age_ms: 300_000,
            max_discovery_latency_ms: 300_000,
            max_stale_cleanup_latency_ms: 300_000,
            max_metric_age_ms: 300_000,
            ..config()
        };
        assert_eq!(
            registry_recheck_interval_ms(&relaxed),
            MAX_REGISTRY_RECHECK_INTERVAL_MS
        );
    }
}
