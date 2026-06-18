mod client;
mod types;

pub use client::{DiscoverySnapshot, DiscoveryWatch, DiscoveryWatchConfig, RegistryClient};
pub use types::{SERVICE_INSTANCE_SCHEMA_VERSION, ServiceEndpoint, ServiceInstance};
