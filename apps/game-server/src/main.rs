mod admin_server;
mod authority_bridge;
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
mod server;
mod session;
mod ticket;

use std::fs;
use std::path::Path;
use std::time::Duration;

use config::Config;
use core::config_table::{ConfigTableRuntime, spawn_hot_reload_task};
use db_store::PgAuditStore;
use service_registry::{RegistryClient, ServiceEndpoint, ServiceInstance};
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
        "csv config tables loaded"
    );

    // Service Registry
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
                let instance = ServiceInstance::new(
                    config.service_instance_id.clone(),
                    config.service_name.clone(),
                    config.host.clone(),
                    config.port,
                )
                .with_admin_port(config.admin_port)
                .with_local_socket(config.local_socket_name.clone())
                .with_endpoints(vec![
                    ServiceEndpoint {
                        name: "client".to_string(),
                        protocol: "tcp".to_string(),
                        host: config.host.clone(),
                        port: config.port,
                        socket: String::new(),
                        visibility: "internal".to_string(),
                        metadata: serde_json::json!({
                            "instance_id": config.service_instance_id.clone(),
                            "server_id": config.service_instance_id.clone()
                        }),
                        healthy: true,
                    },
                    ServiceEndpoint {
                        name: "admin".to_string(),
                        protocol: "http".to_string(),
                        host: config.admin_host.clone(),
                        port: config.admin_port,
                        socket: String::new(),
                        visibility: "admin".to_string(),
                        metadata: serde_json::json!({
                            "instance_id": config.service_instance_id.clone(),
                            "server_id": config.service_instance_id.clone()
                        }),
                        healthy: true,
                    },
                    ServiceEndpoint {
                        name: "internal".to_string(),
                        protocol: "local_socket".to_string(),
                        host: String::new(),
                        port: 0,
                        socket: config.internal_socket_name.clone(),
                        visibility: "local".to_string(),
                        metadata: serde_json::json!({
                            "instance_id": config.service_instance_id.clone(),
                            "server_id": config.service_instance_id.clone()
                        }),
                        healthy: true,
                    },
                    ServiceEndpoint {
                        name: "proxy-local".to_string(),
                        protocol: "local_socket".to_string(),
                        host: String::new(),
                        port: 0,
                        socket: config.local_socket_name.clone(),
                        visibility: "local".to_string(),
                        metadata: serde_json::json!({
                            "instance_id": config.service_instance_id.clone(),
                            "server_id": config.service_instance_id.clone()
                        }),
                        healthy: true,
                    },
                ])
                .with_tags(vec!["game".to_string(), "tcp".to_string()])
                .with_metadata(config.service_instance_metadata());

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

    // 启动心跳任务
    let heartbeat_handle = registry_client
        .as_ref()
        .map(|client| client.start_heartbeat_task());

    let csv_reload_task = if config.csv_reload_enabled {
        Some(spawn_hot_reload_task(
            config_table_runtime.clone(),
            Duration::from_secs(config.csv_reload_interval_secs),
        ))
    } else {
        tracing::info!(csv_dir = %config.csv_dir, "csv config hot reload disabled");
        None
    };

    let db_store = PgAuditStore::new(&config).await?;

    // 启动 metrics 上报任务
    let metrics_nats_url = config.nats_url.clone();
    let metrics_instance_id = config.service_instance_id.clone();
    tokio::spawn(async move {
        metrics::METRICS
            .start_reporting(&metrics_nats_url, metrics_instance_id, 5)
            .await;
    });

    let result = server::run(&config, db_store.clone(), config_table_runtime.clone()).await;

    // 关闭时注销服务
    if let Some(client) = registry_client {
        // 停止心跳任务
        if let Some(handle) = heartbeat_handle {
            handle.abort();
        }
        // 注销服务
        if let Err(e) = client.deregister().await {
            tracing::error!(error = %e, "failed to deregister service");
        } else {
            tracing::info!(
                service = %config.service_name,
                instance = %config.service_instance_id,
                "service deregistered from registry"
            );
        }
    }

    if let Some(task) = csv_reload_task {
        task.abort();
        let _ = task.await;
    }

    db_store.close().await;
    result
}

fn registry_failure_is_fatal() -> bool {
    env_flag("DISCOVERY_REQUIRED")
        || env_name_is("NODE_ENV", "production")
        || env_name_is("APP_ENV", "production")
        || env_name_is("NODE_ENV", "test")
        || env_name_is("APP_ENV", "test")
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
