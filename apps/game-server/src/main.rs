mod config;
#[allow(dead_code)]
mod pb {
    include!(concat!(env!("OUT_DIR"), "/myserver.game.rs"));
}
mod protocol;
mod room;
mod server;
mod session;
mod ticket;

use std::fs;

use config::Config;
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
    let config = Config::from_env();
    init_logging(&config);

    tracing::info!(
        log_enable_console = config.log_enable_console,
        log_enable_file = config.log_enable_file,
        log_dir = %config.log_dir,
        "game-server logging initialized"
    );

    server::run(&config).await
}
