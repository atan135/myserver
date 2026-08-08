mod client;
mod discovery_metrics;
pub mod health;
pub mod readiness;
pub mod startup_contract;
mod types;

pub use client::{
    DiscoverySnapshot, DiscoveryWatch, DiscoveryWatchConfig, HeartbeatOutcome,
    RegistryCapacityError, RegistryClient,
};
pub use discovery_metrics::{
    DiscoveryMetricEntry, collect_discovery_metric_fields, get_discovery_metrics_snapshot,
    record_discovery_metric, reset_discovery_metrics,
};
pub use health::{
    DependencySnapshot, DependencySpec, DependencyStatus, HealthConfig, HealthConfigError,
    HealthMetricsSnapshot, HealthSnapshot, HealthState,
};
pub use startup_contract::{DependencyRequirement, StartupErrorCode, StartupState};
pub use types::{SERVICE_INSTANCE_SCHEMA_VERSION, ServiceEndpoint, ServiceInstance};
