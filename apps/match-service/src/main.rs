//! Match-Service 入口

mod config;
mod error;
mod game_server_client;
mod matcher;
mod metrics;
mod pool;
mod proto;
mod runtime_store;
mod server;
mod service;
mod state;

use std::fs;

use serde_json::{Value, json};
use service_registry::{RegistryClient, ServiceEndpoint, ServiceInstance};
use tracing_appender::rolling;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::config::Config;

fn init_logging(config: &Config) {
    let env_filter = EnvFilter::new(config.log_level());
    let mut layers = Vec::new();

    if config.log_enable_console() {
        layers.push(
            fmt::layer()
                .with_target(false)
                .with_ansi(true)
                .compact()
                .boxed(),
        );
    }

    if config.log_enable_file() {
        fs::create_dir_all(&config.log_dir()).expect("failed to create log dir");
        let file_appender = rolling::daily(&config.log_dir(), "match-service.log");
        layers.push(
            fmt::layer()
                .with_ansi(false)
                .with_target(false)
                .with_writer(file_appender)
                .compact()
                .boxed(),
        );
    }

    if layers.is_empty() {
        layers.push(
            fmt::layer()
                .with_target(false)
                .with_ansi(true)
                .compact()
                .boxed(),
        );
    }

    tracing_subscriber::registry()
        .with(env_filter)
        .with(layers)
        .init();
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let config = Config::from_env();
    init_logging(&config);

    tracing::info!(
        bind_addr = %config.bind_addr,
        registry_enabled = config.registry_enabled,
        "match-service starting"
    );

    let registry_client: Option<RegistryClient> = if config.registry_enabled {
        match RegistryClient::new(
            &config.registry_url,
            &config.service_name,
            &config.service_instance_id,
        )
        .await
        {
            Ok(client) => {
                let client = client
                    .with_key_prefix(config.registry_key_prefix.clone())
                    .with_heartbeat_interval(config.registry_heartbeat_interval_secs);
                let instance = build_service_instance(&config);

                if let Err(e) = client.register(&instance).await {
                    tracing::error!(error = %e, "failed to register service");
                    if registry_failure_is_fatal() {
                        return Err(std::io::Error::other(e.to_string()).into());
                    }
                } else {
                    tracing::info!(
                        service = %config.service_name,
                        instance = %config.service_instance_id,
                        "service registered to registry"
                    );
                }

                Some(client)
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to create registry client");
                if registry_failure_is_fatal() {
                    return Err(std::io::Error::other(e.to_string()).into());
                }
                None
            }
        }
    } else {
        tracing::info!("service registry disabled");
        None
    };

    let heartbeat_handle = registry_client
        .as_ref()
        .map(|client| client.start_heartbeat_task());

    // 启动 metrics 上报任务
    let metrics_nats_url = config.nats_url.clone();
    let metrics_instance_id = config.service_instance_id.clone();
    tokio::spawn(async move {
        metrics::METRICS
            .start_reporting(&metrics_nats_url, metrics_instance_id, 5)
            .await;
    });

    let result = server::run(config).await;

    if let Some(client) = registry_client {
        if let Some(handle) = heartbeat_handle {
            handle.abort();
        }
        if let Err(e) = client.deregister().await {
            tracing::error!(error = %e, "failed to deregister service");
        } else {
            tracing::info!("service deregistered from registry");
        }
    }

    result
}

fn registry_failure_is_fatal() -> bool {
    env_flag("DISCOVERY_REQUIRED")
        || env_name_is("NODE_ENV", "production")
        || env_name_is("APP_ENV", "production")
        || env_name_is("NODE_ENV", "prod")
        || env_name_is("APP_ENV", "prod")
        || env_name_is("NODE_ENV", "staging")
        || env_name_is("APP_ENV", "staging")
        || env_name_is("NODE_ENV", "stage")
        || env_name_is("APP_ENV", "stage")
        || env_name_is("NODE_ENV", "test")
        || env_name_is("APP_ENV", "test")
        || env_name_is("NODE_ENV", "testing")
        || env_name_is("APP_ENV", "testing")
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(false)
}

fn env_name_is(name: &str, expected: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

fn build_match_service_metadata(config: &Config) -> Value {
    let mut modes = config.modes.keys().cloned().collect::<Vec<_>>();
    modes.sort();

    json!({
        "service_name": config.service_name,
        "service_instance_id": config.service_instance_id,
        "instance_id": config.service_instance_id,
        "protocol": "grpc",
        "modes": modes,
        "runtime_store": config.match_runtime_store,
        "runtime_store_backend": config.match_runtime_store,
        "build_version": config.service_build_version,
        "zone": config.service_zone
    })
}

fn build_service_instance(config: &Config) -> ServiceInstance {
    let public_host = published_host(&config.public_host);
    let metadata = build_match_service_metadata(config);

    ServiceInstance::new(
        config.service_instance_id.clone(),
        config.service_name.clone(),
        public_host.clone(),
        config.port,
    )
    .with_endpoints(vec![ServiceEndpoint {
        name: "grpc".to_string(),
        protocol: "grpc".to_string(),
        host: public_host,
        port: config.port,
        socket: String::new(),
        visibility: "internal".to_string(),
        metadata: metadata.clone(),
        healthy: true,
    }])
    .with_tags(vec!["match".to_string(), "grpc".to_string()])
    .with_metadata(metadata)
}

fn published_host(host: &str) -> String {
    let host = host.trim();
    if matches!(host, "" | "0.0.0.0" | "::" | "[::]") {
        "127.0.0.1".to_string()
    } else {
        host.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ModeConfig;
    use std::collections::HashMap;

    fn test_config() -> Config {
        let mut modes = HashMap::new();
        modes.insert(
            "5v5".to_string(),
            ModeConfig {
                team_size: 5,
                total_size: 10,
                match_timeout_secs: 90,
            },
        );
        modes.insert(
            "1v1".to_string(),
            ModeConfig {
                team_size: 1,
                total_size: 2,
                match_timeout_secs: 30,
            },
        );
        modes.insert(
            "3v3".to_string(),
            ModeConfig {
                team_size: 3,
                total_size: 6,
                match_timeout_secs: 60,
            },
        );

        Config {
            bind_addr: "0.0.0.0:9002".to_string(),
            public_host: "127.0.0.1".to_string(),
            port: 9002,
            match_timeout_secs: 30,
            max_concurrent_matches: 1000,
            modes,
            match_cleanup_interval_secs: 1,
            game_server_service_name: "game-server".to_string(),
            game_server_internal_socket_name: "myserver-game-server-internal.sock".to_string(),
            local_discovery_fallback_enabled: true,
            game_server_discovery_cache_ttl_secs: 5,
            game_server_target_zone: String::new(),
            game_internal_token: "dev-only-change-this-game-internal-token".to_string(),
            log_level: "info".to_string(),
            log_enable_console: true,
            log_enable_file: false,
            log_dir: "logs".to_string(),
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: String::new(),
            global_id_origin_id: 0,
            global_id_worker_id: None,
            nats_url: "nats://127.0.0.1:4222".to_string(),
            registry_enabled: false,
            discovery_required: false,
            registry_url: "redis://127.0.0.1:6379".to_string(),
            registry_key_prefix: String::new(),
            registry_heartbeat_interval_secs: 10,
            service_name: "match-service".to_string(),
            service_instance_id: "match-service-test".to_string(),
            service_zone: "zone-match".to_string(),
            service_build_version: "2026.06.18-test".to_string(),
            match_runtime_store: "redis".to_string(),
            match_runtime_key_prefix: "myserver:".to_string(),
            match_runtime_lease_ttl_secs: 10,
            match_recovery_enabled: true,
            legacy_direct_config_warnings: Vec::new(),
        }
    }

    #[test]
    fn match_service_metadata_contains_sorted_modes_runtime_store_and_build_version() {
        let config = test_config();

        let metadata = build_match_service_metadata(&config);

        assert_eq!(metadata["service_name"], "match-service");
        assert_eq!(metadata["service_instance_id"], "match-service-test");
        assert_eq!(metadata["instance_id"], "match-service-test");
        assert_eq!(metadata["protocol"], "grpc");
        assert_eq!(metadata["modes"], json!(["1v1", "3v3", "5v5"]));
        assert_eq!(metadata["runtime_store"], "redis");
        assert_eq!(metadata["runtime_store_backend"], "redis");
        assert_eq!(metadata["build_version"], "2026.06.18-test");
        assert_eq!(metadata["zone"], "zone-match");
    }

    #[test]
    fn match_service_endpoint_metadata_keeps_protocol_for_compatibility() {
        let config = test_config();

        let metadata = build_match_service_metadata(&config);

        assert_eq!(metadata["protocol"], "grpc");
        assert_eq!(metadata["modes"], json!(["1v1", "3v3", "5v5"]));
        assert_eq!(metadata["runtime_store_backend"], "redis");
        assert_eq!(metadata["build_version"], "2026.06.18-test");
    }

    #[test]
    fn service_instance_registers_grpc_endpoint_as_internal() {
        let config = test_config();

        let instance = build_service_instance(&config);

        assert_eq!(instance.endpoints.len(), 1);
        let endpoint = &instance.endpoints[0];
        assert_eq!(endpoint.name, "grpc");
        assert_eq!(endpoint.protocol, "grpc");
        assert_eq!(endpoint.host, "127.0.0.1");
        assert_eq!(endpoint.port, 9002);
        assert_eq!(endpoint.visibility, "internal");
    }

    #[test]
    fn service_instance_never_publishes_wildcard_network_hosts() {
        let mut config = test_config();
        config.public_host = "0.0.0.0".to_string();

        let instance = build_service_instance(&config);

        assert_eq!(instance.host, "127.0.0.1");
        assert_eq!(instance.endpoints[0].host, "127.0.0.1");

        config.public_host = "::".to_string();
        let instance = build_service_instance(&config);

        assert_eq!(instance.host, "127.0.0.1");
        assert_eq!(instance.endpoints[0].host, "127.0.0.1");
    }
}
