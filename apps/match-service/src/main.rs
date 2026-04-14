//! Match-Service 入口

mod config;
mod error;
mod matcher;
mod metrics;
mod pool;
mod proto;
mod server;
mod service;
mod state;

use std::fs;

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
        "match-service starting"
    );

    // 启动 metrics 上报任务
    let metrics_redis_url = config.redis_url.clone();
    tokio::spawn(async move {
        metrics::METRICS.start_reporting(&metrics_redis_url, 5).await;
    });

    server::run(config).await
}
