mod admin_server;
mod config;
mod config_table;
mod core;
mod gameroom;
mod gameservice;
mod local_socket;
mod proto;
pub use proto::admin as admin_pb;
pub use proto::game as pb;
#[allow(dead_code)]
mod csv_code;
mod mysql_store;
mod protocol;
mod server;
mod session;
mod ticket;

use std::fs;
use std::path::Path;
use std::time::Duration;

use config::Config;
use config_table::{ConfigTableRuntime, spawn_hot_reload_task};
use mysql_store::MySqlAuditStore;
use service_registry::{RegistryClient, ServiceInstance};
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
        mysql_enabled = config.mysql_enabled,
        game_addr = %config.bind_addr(),
        admin_addr = %config.admin_bind_addr(),
        local_socket_name = %config.local_socket_name,
        "game-server logging initialized"
    );

    let config_table_runtime = ConfigTableRuntime::load(Path::new(&config.csv_dir))?;
    let initial_tables = config_table_runtime.snapshot().await;
    let row_counts = initial_tables.row_counts();
    tracing::info!(
        testtable_100_rows = row_counts.testtable_100,
        testtable_110_rows = row_counts.testtable_110,
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
                let instance = ServiceInstance::new(
                    config.service_instance_id.clone(),
                    config.service_name.clone(),
                    config.host.clone(),
                    config.port,
                )
                .with_admin_port(config.admin_port)
                .with_local_socket(config.local_socket_name.clone())
                .with_tags(vec!["game".to_string(), "tcp".to_string()]);

                if let Err(e) = client.register(&instance).await {
                    tracing::error!(error = %e, "failed to register service");
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

    let mysql_store = MySqlAuditStore::new(&config).await?;
    let result = server::run(&config, mysql_store.clone(), config_table_runtime.clone()).await;

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

    let _ = mysql_store.close().await;
    result
}


