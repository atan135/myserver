mod client;
mod discovery_metrics;
mod types;

pub use client::{DiscoverySnapshot, DiscoveryWatch, DiscoveryWatchConfig, RegistryClient};
pub use discovery_metrics::{
    DiscoveryMetricEntry, collect_discovery_metric_fields, get_discovery_metrics_snapshot,
    record_discovery_metric, reset_discovery_metrics,
};
pub use types::{SERVICE_INSTANCE_SCHEMA_VERSION, ServiceEndpoint, ServiceInstance};
