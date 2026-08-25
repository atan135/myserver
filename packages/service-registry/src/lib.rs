mod client;
mod convergence;
mod discovery_metrics;
pub mod health;
mod publication;
pub mod readiness;
mod route_observation;
pub mod startup_contract;
mod types;

pub use client::{
    DiscoverySnapshot, DiscoveryWatch, DiscoveryWatchConfig, HeartbeatOutcome,
    REGISTRY_HEARTBEAT_TTL_SECONDS, RegistryCapacityError, RegistryClient,
};
pub use convergence::{
    ConvergenceAttempt, ConvergenceConfig, ConvergenceJitter, ConvergencePhase,
    ConvergenceSnapshot, ConvergenceTask, spawn_convergence, spawn_convergence_with_jitter,
};
pub use discovery_metrics::{
    DiscoveryMetricEntry, collect_discovery_metric_fields, get_discovery_metrics_snapshot,
    record_discovery_metric, reset_discovery_metrics,
};
pub use health::{
    DependencySnapshot, DependencySpec, DependencyStatus, HealthConfig, HealthConfigError,
    HealthMetricsSnapshot, HealthSnapshot, HealthState,
};
pub use publication::spawn_registry_publication;
pub use route_observation::{
    PROXY_ROUTE_OBSERVATION_KEY_NAMESPACE, PROXY_ROUTE_OBSERVATION_SCHEMA_VERSION,
    PROXY_ROUTE_OBSERVATION_TTL_SECS, ProxyRouteObservation, ProxyRouteObservationPublisher,
    RouteObservationError, proxy_route_observation_key,
};
pub use startup_contract::{DependencyRequirement, StartupErrorCode, StartupState};
pub use types::{SERVICE_INSTANCE_SCHEMA_VERSION, ServiceEndpoint, ServiceInstance};
