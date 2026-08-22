mod activity;
mod adapters;
mod admin_server;
mod authority_bridge;
mod business;
mod config;
mod config_table;
mod core;
mod gameconfig;
mod gameroom;
mod gameservice;
mod gm_broadcast;
mod internal_server;
mod kick_subscriber;
mod local_socket;
mod match_client;
mod metrics;
mod proto;
pub use proto::myserver::admin as admin_pb;
pub use proto::myserver::game as pb;
#[allow(dead_code)]
mod csv_code;
mod db_store;
mod protocol;
mod protocol_version_policy {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/proto/compatibility/version-policy.rs"
    ));
}
mod server;
mod session;
mod startup;
mod ticket;

use std::fs;
use std::path::Path;

use config::Config;
use core::config_table::ConfigTableRuntime;
use service_registry::{
    DependencySpec, HealthConfig, HealthState, ServiceEndpoint, ServiceInstance,
};
use tracing_appender::rolling;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

fn init_logging(config: &Config) {
    let env_filter = EnvFilter::new(config.log_level.clone());
    let mut layers = Vec::new();

    if config.log_enable_console {
        layers.push(
            fmt::layer()
                .with_target(false)
                .with_ansi(true)
                .compact()
                .boxed(),
        );
    }

    if config.log_enable_file {
        fs::create_dir_all(&config.log_dir).expect("failed to create log dir");
        let file_appender = rolling::daily(&config.log_dir, "game-server.log");
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

fn validate_match_refresh_cadence(
    registry_enabled: bool,
    rediscovery_interval_secs: u64,
    health_config: &HealthConfig,
) -> Result<(), service_registry::HealthConfigError> {
    if registry_enabled {
        let rediscovery = match_client::rediscovery_convergence_config(rediscovery_interval_secs);
        health_config.validate_dependency_refresh_cadence(
            "MATCH_SERVICE_REDISCOVERY_INTERVAL_SECS",
            rediscovery.maximum_success_refresh_interval(),
        )?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = dotenvy::dotenv();

    let config = Config::from_env();
    init_logging(&config);

    tracing::info!(
        log_enable_console = config.log_enable_console,
        log_enable_file = config.log_enable_file,
        log_dir = %config.log_dir,
        csv_dir = %config.csv_dir,
        csv_reload_enabled = config.csv_reload_enabled,
        csv_reload_interval_secs = config.csv_reload_interval_secs,
        room_cleanup_interval_secs = config.room_cleanup_interval_secs,
        db_enabled = config.db_enabled,
        game_addr = %config.bind_addr(),
        admin_addr = %config.admin_bind_addr(),
        local_socket_name = %config.local_socket_name,
        internal_socket_name = %config.internal_socket_name,
        global_id_origin_id = config.global_id_origin_id,
        global_id_worker_id = config.global_id_worker_id,
        "game-server logging initialized"
    );

    let match_dependency = if config.registry_enabled {
        DependencySpec::required("match-service", "grpc")
    } else {
        DependencySpec::required_without_stale_detection("match-service", "grpc")
    };
    let mut health_dependencies = vec![
        DependencySpec::local_required("server-listeners"),
        DependencySpec::local_required("worker-lease"),
        DependencySpec::local_required("gameplay-stores"),
        match_dependency,
    ];
    if config.registry_enabled {
        health_dependencies.push(DependencySpec::required_without_stale_detection(
            "service-registry",
            "self-registration",
        ));
    }
    let health_config = HealthConfig::try_from_env()?;
    validate_match_refresh_cadence(
        config.registry_enabled,
        match_client::MatchClientConfig::rediscovery_interval_secs_from_env(),
        &health_config,
    )?;
    let health_state = HealthState::new(
        &config.service_name,
        &config.service_instance_id,
        health_config,
        health_dependencies,
    );
    let config_table_runtime = ConfigTableRuntime::load(Path::new(&config.csv_dir))?;
    let initial_config = config_table_runtime.snapshot().await;
    let initial_tables = initial_config.tables.clone();
    let row_counts = initial_tables.row_counts();
    tracing::info!(
        config_version = initial_config.version,
        scenetable_rows = row_counts.scenetable,
        scenespawnpoint_rows = row_counts.scenespawnpoint,
        sceneportal_rows = row_counts.sceneportal,
        sceneregion_rows = row_counts.sceneregion,
        scenemonsterspawn_rows = row_counts.scenemonsterspawn,
        testtable_100_rows = row_counts.testtable_100,
        testtable_110_rows = row_counts.testtable_110,
        itemtable_rows = row_counts.itemtable,
        skillbase_rows = row_counts.skillbase,
        bufferbase_rows = row_counts.bufferbase,
        titletable_rows = row_counts.titletable,
        characterprogresstable_rows = row_counts.characterprogresstable,
        "csv config tables loaded"
    );

    server::run(&config, config_table_runtime, health_state).await
}

pub(crate) fn build_service_instance(config: &Config) -> ServiceInstance {
    let client_host = published_host(&config.public_host);
    let admin_host = published_host(&config.admin_advertised_host);
    let endpoint_metadata = serde_json::json!({
        "service_name": config.service_name.clone(),
        "service_instance_id": config.service_instance_id.clone(),
        "instance_id": config.service_instance_id.clone(),
        "server_id": config.service_instance_id.clone(),
        "build_version": config.service_build_version.clone(),
        "zone": config.service_zone.clone()
    });

    ServiceInstance::new(
        config.service_instance_id.clone(),
        config.service_name.clone(),
        client_host.clone(),
        config.port,
    )
    .with_admin_port(config.admin_port)
    .with_local_socket(config.local_socket_name.clone())
    .with_endpoints(vec![
        ServiceEndpoint {
            name: "client".to_string(),
            protocol: "tcp".to_string(),
            host: client_host,
            port: config.port,
            socket: String::new(),
            visibility: "internal".to_string(),
            metadata: endpoint_metadata.clone(),
            healthy: true,
        },
        ServiceEndpoint {
            name: "admin".to_string(),
            protocol: "tcp".to_string(),
            host: admin_host,
            port: config.admin_port,
            socket: String::new(),
            visibility: "admin".to_string(),
            metadata: endpoint_metadata.clone(),
            healthy: true,
        },
        ServiceEndpoint {
            name: "internal".to_string(),
            protocol: "local_socket".to_string(),
            host: String::new(),
            port: 0,
            socket: config.internal_socket_name.clone(),
            visibility: "local".to_string(),
            metadata: endpoint_metadata.clone(),
            healthy: true,
        },
        ServiceEndpoint {
            name: "proxy-local".to_string(),
            protocol: "local_socket".to_string(),
            host: String::new(),
            port: 0,
            socket: config.local_socket_name.clone(),
            visibility: "local".to_string(),
            metadata: endpoint_metadata,
            healthy: true,
        },
    ])
    .with_tags(vec!["game".to_string(), "tcp".to_string()])
    .with_metadata(config.service_instance_metadata())
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

    #[test]
    fn match_refresh_cadence_is_rejected_before_server_startup() {
        let health_config = HealthConfig::for_tests(8_000, 2_000, 35_000);
        let error = validate_match_refresh_cadence(true, 30, &health_config).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("MATCH_SERVICE_REDISCOVERY_INTERVAL_SECS")
        );
        validate_match_refresh_cadence(false, 30, &health_config).unwrap();
        validate_match_refresh_cadence(true, 30, &HealthConfig::default()).unwrap();
    }

    fn test_config() -> Config {
        Config {
            host: "0.0.0.0".to_string(),
            public_host: "10.0.0.20".to_string(),
            port: 7000,
            csv_dir: "csv".to_string(),
            csv_reload_enabled: false,
            csv_reload_interval_secs: 3,
            room_cleanup_interval_secs: 10,
            admin_host: "0.0.0.0".to_string(),
            admin_advertised_host: "10.0.0.21".to_string(),
            admin_port: 7500,
            admin_token: config::DEFAULT_ADMIN_TOKEN.to_string(),
            admin_assertion_issuer: "admin-api".to_string(),
            admin_assertion_public_keys: std::collections::HashMap::new(),
            admin_assertion_max_ttl_ms: 60_000,
            mail_grant_assertion_issuer: "mail-service".to_string(),
            mail_grant_assertion_public_keys: std::collections::HashMap::new(),
            mail_grant_assertion_max_ttl_ms: 60_000,
            admin_audit_enabled: true,
            admin_audit_path: "logs/game-server/admin-audit.jsonl".to_string(),
            admin_audit_require_actor: false,
            internal_token: config::DEFAULT_INTERNAL_TOKEN.to_string(),
            local_socket_name: "myserver-game-server.sock".to_string(),
            internal_socket_name: "myserver-game-server-internal.sock".to_string(),
            log_level: "info".to_string(),
            log_enable_console: true,
            log_enable_file: false,
            log_dir: "logs/game-server".to_string(),
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: String::new(),
            activity_cache_operation_timeout_ms:
                config::DEFAULT_ACTIVITY_CACHE_OPERATION_TIMEOUT_MS,
            global_id_origin_id: 0,
            global_id_worker_id: None,
            nats_url: "nats://127.0.0.1:4222".to_string(),
            db_enabled: false,
            activity_enabled: false,
            reward_mail_dispatch_enabled: false,
            mail_service_token: "dev-only-change-this-mail-service-token".to_string(),
            database_url: "postgres://postgres:password@127.0.0.1:5432/myserver_game".to_string(),
            db_pool_size: 10,
            ticket_secret: config::DEFAULT_TICKET_SECRET.to_string(),
            heartbeat_timeout_secs: 30,
            max_body_len: 4096,
            outbound_queue_capacity: config::DEFAULT_OUTBOUND_QUEUE_CAPACITY,
            msg_rate_window_ms: 1000,
            msg_rate_max: 0,
            player_msg_rate_window_ms: 1000,
            player_msg_rate_max: 0,
            input_timestamp_required: false,
            input_timestamp_max_skew_ms: 5000,
            input_anomaly_window_ms: 10_000,
            input_anomaly_max: 0,
            max_learned_disciplines: config::DEFAULT_MAX_LEARNED_DISCIPLINES,
            max_active_disciplines: config::DEFAULT_MAX_ACTIVE_DISCIPLINES,
            registry_enabled: true,
            discovery_required: false,
            registry_url: "redis://127.0.0.1:6379".to_string(),
            registry_key_prefix: String::new(),
            registry_heartbeat_interval_secs: 10,
            service_name: "game-server".to_string(),
            service_instance_id: "game-server-a".to_string(),
            service_build_version: "dev".to_string(),
            service_zone: "local".to_string(),
            service_rollout_epoch: "default".to_string(),
            legacy_direct_config_warnings: Vec::new(),
        }
    }

    #[test]
    fn service_instance_uses_advertised_hosts_for_registered_endpoints() {
        let instance = build_service_instance(&test_config());

        assert_eq!(instance.host, "10.0.0.20");
        assert_eq!(instance.endpoints[0].name, "client");
        assert_eq!(instance.endpoints[0].host, "10.0.0.20");
        assert_eq!(instance.endpoints[1].name, "admin");
        assert_eq!(instance.endpoints[1].protocol, "tcp");
        assert_eq!(instance.endpoints[1].host, "10.0.0.21");
        assert_eq!(instance.endpoints[1].port, 7500);
        assert_eq!(instance.endpoints[2].name, "internal");
        assert_eq!(
            instance.endpoints[2].socket,
            "myserver-game-server-internal.sock"
        );
        assert_eq!(instance.endpoints[3].name, "proxy-local");
        assert_eq!(instance.endpoints[3].socket, "myserver-game-server.sock");
    }

    #[test]
    fn service_instance_never_publishes_wildcard_network_hosts() {
        let mut config = test_config();
        config.public_host = "0.0.0.0".to_string();
        config.admin_advertised_host = "::".to_string();

        let instance = build_service_instance(&config);

        assert_eq!(instance.host, "127.0.0.1");
        assert_eq!(instance.endpoints[0].host, "127.0.0.1");
        assert_eq!(instance.endpoints[1].host, "127.0.0.1");
    }
}
