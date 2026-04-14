mod chat_server;
mod chat_service;
mod chat_store;
mod mail_subscriber;
mod metrics;
mod proto;
mod protocol;
mod ticket;

use std::fs;

use tracing_appender::rolling;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

struct Config {
    mysql_url: String,
    mysql_pool_size: u32,
    bind_addr: String,
    heartbeat_timeout_secs: u64,
    max_body_len: usize,
    ticket_secret: String,
    redis_url: String,
    log_level: String,
    log_enable_console: bool,
    log_enable_file: bool,
    log_dir: String,
}

impl Config {
    fn from_env() -> Self {
        Self {
            mysql_url: std::env::var("MYSQL_URL")
                .unwrap_or_else(|_| "mysql://root:password@localhost:3306/chat".to_string()),
            mysql_pool_size: std::env::var("MYSQL_POOL_SIZE")
                .unwrap_or_else(|_| "5".to_string())
                .parse()
                .unwrap_or(5),
            bind_addr: std::env::var("CHAT_BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:9001".to_string()),
            heartbeat_timeout_secs: std::env::var("HEARTBEAT_TIMEOUT_SECS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()
                .unwrap_or(30),
            max_body_len: std::env::var("MAX_BODY_LEN")
                .unwrap_or_else(|_| "4096".to_string())
                .parse()
                .unwrap_or(4096),
            ticket_secret: std::env::var("TICKET_SECRET")
                .unwrap_or_else(|_| "default_secret_change_in_production".to_string()),
            redis_url: std::env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
            log_level: std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string()),
            log_enable_console: std::env::var("LOG_ENABLE_CONSOLE")
                .unwrap_or_else(|_| "true".to_string())
                .parse()
                .unwrap_or(true),
            log_enable_file: std::env::var("LOG_ENABLE_FILE")
                .unwrap_or_else(|_| "false".to_string())
                .parse()
                .unwrap_or(false),
            log_dir: std::env::var("LOG_DIR").unwrap_or_else(|_| "logs".to_string()),
        }
    }
}

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
        let file_appender = rolling::daily(&config.log_dir, "chat-server.log");
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
        mysql_url = %config.mysql_url,
        redis_url = %config.redis_url,
        "chat-server starting"
    );

    let chat_store = chat_store::ChatStore::new(&config.mysql_url, config.mysql_pool_size).await?;

    // Create Redis client for mail notification subscriber
    let redis_client = redis::Client::open(config.redis_url.clone())?;
    let _redis_conn = redis_client.get_multiplexed_async_connection().await?;

    // 启动 metrics 上报任务
    let metrics_redis_url = config.redis_url.clone();
    tokio::spawn(async move {
        metrics::METRICS.start_reporting(&metrics_redis_url, 5).await;
    });

    let server_config = chat_server::Config {
        bind_addr: config.bind_addr.clone(),
        heartbeat_timeout_secs: config.heartbeat_timeout_secs,
        max_body_len: config.max_body_len,
        ticket_secret: config.ticket_secret.clone(),
    };

    // Create chat sessions map for mail notification pusher
    let chat_sessions = chat_service::new_chat_session_map();

    // Start mail notification subscriber
    let sessions_for_mail = chat_sessions.clone();
    tokio::spawn(async move {
        if let Err(e) = mail_subscriber::subscribe_mail_notifications(redis_client, sessions_for_mail).await {
            tracing::error!("mail subscriber error: {}", e);
        }
    });

    tracing::info!("mail notification subscriber started");

    let result = chat_server::run(server_config, chat_store.clone(), chat_sessions).await;

    let _ = chat_store.close().await;
    result
}
