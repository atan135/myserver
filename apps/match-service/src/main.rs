//! Match-Service 入口

mod config;
mod error;
mod game_server_client;
mod matcher;
mod metrics;
mod pool;
mod proto;
mod server;
mod service;
mod state;

use std::fs;

use service_registry::{RegistryClient, ServiceInstance};
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
                let client = client.with_heartbeat_interval(config.registry_heartbeat_interval_secs);
                let instance = ServiceInstance::new(
                    config.service_instance_id.clone(),
                    config.service_name.clone(),
                    config.public_host.clone(),
                    config.port,
                )
                .with_tags(vec!["match".to_string(), "grpc".to_string()])
                .with_metadata(serde_json::json!({
                    "protocol": "grpc"
                }));

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

    let heartbeat_handle = registry_client
        .as_ref()
        .map(|client| client.start_heartbeat_task());

    // 启动 metrics 上报任务
    let metrics_redis_url = config.redis_url.clone();
    tokio::spawn(async move {
        metrics::METRICS.start_reporting(&metrics_redis_url, 5).await;
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
