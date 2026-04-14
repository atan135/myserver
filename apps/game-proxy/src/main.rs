mod admin_server;
mod config;
mod local_socket;
mod metrics;
mod proxy_server;
mod route_store;
mod session;
mod transport;
mod upstream;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use config::Config;
use route_store::{ProxyRouteStore, UpstreamRoute, UpstreamState};
use tokio::sync::RwLock;
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
        let file_appender = rolling::daily(&config.log_dir, "game-proxy.log");
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

    // 启动 metrics 上报任务
    let metrics_redis_url = config.registry_url.clone();
    tokio::spawn(async move {
        metrics::METRICS.start_reporting(&metrics_redis_url, 5).await;
    });

    let route_store = ProxyRouteStore::default();
    route_store
        .set_routes(vec![UpstreamRoute {
            server_id: config.upstream_server_id.clone(),
            local_socket_name: config.upstream_local_socket_name.clone(),
            state: UpstreamState::Active,
        }])
        .await;

    let connection_count = Arc::new(AtomicU64::new(0));
    let maintenance = Arc::new(RwLock::new(false));

    let admin_bind_addr = config.admin_bind_addr();
    let admin_route_store = route_store.clone();
    let admin_connection_count = connection_count.clone();
    let admin_maintenance = maintenance.clone();
    let admin_task = tokio::spawn(async move {
        if let Err(error) = admin_server::run(
            &admin_bind_addr,
            admin_route_store,
            admin_connection_count,
            admin_maintenance,
        ).await {
            tracing::warn!(error = %error, "proxy admin server stopped");
        }
    });

    let result = proxy_server::run(&config, route_store, connection_count, maintenance).await;

    admin_task.abort();
    let _ = admin_task.await;
    result
}
