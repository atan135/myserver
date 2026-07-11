use std::fs;

use tracing_appender::rolling;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::config::LoggingConfig;
use crate::error::AgentError;

pub fn init_logging(config: &LoggingConfig) -> Result<(), AgentError> {
    let env_filter = EnvFilter::try_new(config.level())
        .map_err(|_| AgentError::config("LOG_LEVEL", "invalid tracing filter"))?;
    let mut layers = Vec::new();

    if config.enable_console() {
        layers.push(
            fmt::layer()
                .with_target(false)
                .with_ansi(true)
                .compact()
                .boxed(),
        );
    }

    if config.enable_file() {
        fs::create_dir_all(config.directory())
            .map_err(|_| AgentError::config("LOG_DIR", "log directory is unavailable"))?;
        let file_appender = rolling::daily(config.directory(), "myforge-agent.log");
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
        .try_init()
        .map_err(|_| AgentError::config("logging", "subscriber initialization failed"))
}
