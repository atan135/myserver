mod chat_server;
mod chat_service;
mod chat_store;
mod mail_subscriber;
mod metrics;
mod online_route;
mod proto;
mod protocol;
mod ticket;

use std::fs;
use std::net::SocketAddr;

use service_registry::{RegistryClient, ServiceInstance};
use tracing_appender::rolling;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

const DEFAULT_OUTBOUND_QUEUE_CAPACITY: usize = 1024;
const DEFAULT_TICKET_SECRET: &str = "default_secret_change_in_production";

struct Config {
    mysql_url: String,
    mysql_pool_size: u32,
    bind_addr: String,
    heartbeat_timeout_secs: u64,
    max_body_len: usize,
    msg_rate_window_ms: u64,
    msg_rate_max: u64,
    outbound_queue_capacity: usize,
    ticket_secret: String,
    redis_url: String,
    redis_key_prefix: String,
    nats_url: String,
    registry_enabled: bool,
    registry_url: String,
    registry_heartbeat_interval_secs: u64,
    service_name: String,
    service_instance_id: String,
    online_route_ttl_secs: u64,
    public_host: String,
    log_level: String,
    log_enable_console: bool,
    log_enable_file: bool,
    log_dir: String,
}

impl Config {
    fn from_env() -> Self {
        let config = Self {
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
            msg_rate_window_ms: parse_u64_env("CHAT_MSG_RATE_WINDOW_MS", 1000),
            msg_rate_max: parse_u64_env("CHAT_MSG_RATE_MAX", 0),
            outbound_queue_capacity: parse_outbound_queue_capacity(
                std::env::var("CHAT_OUTBOUND_QUEUE_CAPACITY").ok(),
            ),
            ticket_secret: std::env::var("TICKET_SECRET")
                .unwrap_or_else(|_| DEFAULT_TICKET_SECRET.to_string()),
            redis_url: std::env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
            redis_key_prefix: std::env::var("REDIS_KEY_PREFIX").unwrap_or_default(),
            nats_url: std::env::var("NATS_URL")
                .unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string()),
            registry_enabled: std::env::var("REGISTRY_ENABLED")
                .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "True"))
                .unwrap_or(false),
            registry_url: std::env::var("REGISTRY_URL")
                .or_else(|_| std::env::var("REDIS_URL"))
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
            registry_heartbeat_interval_secs: std::env::var("REGISTRY_HEARTBEAT_INTERVAL")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .unwrap_or(10),
            service_name: std::env::var("SERVICE_NAME")
                .unwrap_or_else(|_| "chat-server".to_string()),
            service_instance_id: std::env::var("SERVICE_INSTANCE_ID")
                .unwrap_or_else(|_| "chat-server-001".to_string()),
            online_route_ttl_secs: std::env::var("CHAT_ONLINE_ROUTE_TTL_SECS")
                .unwrap_or_else(|_| "60".to_string())
                .parse()
                .unwrap_or(60),
            public_host: std::env::var("CHAT_PUBLIC_HOST")
                .unwrap_or_else(|_| "127.0.0.1".to_string()),
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
        };

        validate_production_config(&config);

        config
    }
}

fn is_production_env() -> bool {
    ["NODE_ENV", "APP_ENV"].iter().any(|name| {
        std::env::var(name)
            .ok()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("production"))
    })
}

fn validate_production_config(config: &Config) {
    if !is_production_env() {
        return;
    }

    if is_default_ticket_secret(&config.ticket_secret) {
        panic!(
            "invalid chat-server production config: TICKET_SECRET must be set to a non-default value in production"
        );
    }
}

fn is_default_ticket_secret(value: &str) -> bool {
    let normalized = value.trim();

    normalized.is_empty()
        || matches!(
            normalized,
            DEFAULT_TICKET_SECRET
                | "replace-with-a-long-random-string"
                | "change-me"
                | "changeme"
                | "default"
                | "password"
        )
}

fn parse_outbound_queue_capacity(value: Option<String>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_OUTBOUND_QUEUE_CAPACITY)
}

fn parse_u64_env(name: &str, default_value: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default_value)
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
        registry_enabled = config.registry_enabled,
        "chat-server starting"
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
                let port = extract_port(&config.bind_addr)?;
                let instance = ServiceInstance::new(
                    config.service_instance_id.clone(),
                    config.service_name.clone(),
                    config.public_host.clone(),
                    port,
                )
                .with_tags(vec!["chat".to_string(), "tcp".to_string()]);

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

    let chat_store = chat_store::ChatStore::new(&config.mysql_url, config.mysql_pool_size).await?;

    // 启动 metrics 上报任务
    let metrics_nats_url = config.nats_url.clone();
    let metrics_instance_id = config.service_instance_id.clone();
    tokio::spawn(async move {
        metrics::METRICS
            .start_reporting(&metrics_nats_url, metrics_instance_id, 5)
            .await;
    });

    let server_config = chat_server::Config {
        bind_addr: config.bind_addr.clone(),
        heartbeat_timeout_secs: config.heartbeat_timeout_secs,
        max_body_len: config.max_body_len,
        msg_rate_window_ms: config.msg_rate_window_ms,
        msg_rate_max: config.msg_rate_max,
        ticket_secret: config.ticket_secret.clone(),
        redis_url: config.redis_url.clone(),
        redis_key_prefix: config.redis_key_prefix.clone(),
        service_instance_id: config.service_instance_id.clone(),
        online_route_ttl_secs: config.online_route_ttl_secs,
        outbound_queue_capacity: config.outbound_queue_capacity,
    };

    // Create chat sessions map for mail notification pusher
    let chat_sessions = chat_service::new_chat_session_map();

    // Start mail notification subscriber
    let sessions_for_mail = chat_sessions.clone();
    let nats_url_for_mail = config.nats_url.clone();
    let instance_id_for_mail = config.service_instance_id.clone();
    tokio::spawn(async move {
        if let Err(e) = mail_subscriber::subscribe_mail_notifications(nats_url_for_mail, instance_id_for_mail, sessions_for_mail).await {
            tracing::error!("mail subscriber error: {}", e);
        }
    });

    tracing::info!("mail notification subscriber started");

    let result = chat_server::run(server_config, chat_store.clone(), chat_sessions).await;

    if let Some(client) = registry_client {
        if let Some(handle) = heartbeat_handle {
            handle.abort();
        }
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

    let _ = chat_store.close().await;
    result
}

fn extract_port(bind_addr: &str) -> Result<u16, Box<dyn std::error::Error>> {
    let addr: SocketAddr = bind_addr.parse()?;
    Ok(addr.port())
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::{Mutex, OnceLock};

    use super::*;

    const TICKET_SECRET_ENV_NAMES: &[&str] = &["NODE_ENV", "APP_ENV", "TICKET_SECRET"];

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn capture(names: &[&'static str]) -> Self {
            Self {
                saved: names
                    .iter()
                    .map(|name| (*name, env::var(name).ok()))
                    .collect(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in self.saved.drain(..) {
                unsafe {
                    match value {
                        Some(value) => env::set_var(name, value),
                        None => env::remove_var(name),
                    }
                }
            }
        }
    }

    fn panic_message(result: Result<Config, Box<dyn std::any::Any + Send>>) -> String {
        match result {
            Ok(_) => panic!("production config should be rejected"),
            Err(payload) => {
                if let Some(message) = payload.downcast_ref::<String>() {
                    message.clone()
                } else if let Some(message) = payload.downcast_ref::<&str>() {
                    message.to_string()
                } else {
                    panic!("panic payload should be a string");
                }
            }
        }
    }

    fn catch_config_from_env() -> Result<Config, Box<dyn std::any::Any + Send>> {
        panic::catch_unwind(AssertUnwindSafe(Config::from_env))
    }

    #[test]
    fn outbound_queue_capacity_uses_default_for_missing_zero_or_invalid_value() {
        assert_eq!(
            parse_outbound_queue_capacity(None),
            DEFAULT_OUTBOUND_QUEUE_CAPACITY
        );
        assert_eq!(
            parse_outbound_queue_capacity(Some("0".to_string())),
            DEFAULT_OUTBOUND_QUEUE_CAPACITY
        );
        assert_eq!(
            parse_outbound_queue_capacity(Some("invalid".to_string())),
            DEFAULT_OUTBOUND_QUEUE_CAPACITY
        );
    }

    #[test]
    fn outbound_queue_capacity_accepts_positive_value() {
        assert_eq!(parse_outbound_queue_capacity(Some("64".to_string())), 64);
    }

    #[test]
    fn chat_message_rate_limit_config_defaults_to_disabled() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&["CHAT_MSG_RATE_WINDOW_MS", "CHAT_MSG_RATE_MAX"]);

        unsafe {
            env::remove_var("CHAT_MSG_RATE_WINDOW_MS");
            env::remove_var("CHAT_MSG_RATE_MAX");
        }

        let config = Config::from_env();

        assert_eq!(config.msg_rate_window_ms, 1000);
        assert_eq!(config.msg_rate_max, 0);
    }

    #[test]
    fn chat_message_rate_limit_config_accepts_env_values() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&["CHAT_MSG_RATE_WINDOW_MS", "CHAT_MSG_RATE_MAX"]);

        unsafe {
            env::set_var("CHAT_MSG_RATE_WINDOW_MS", "500");
            env::set_var("CHAT_MSG_RATE_MAX", "20");
        }

        let config = Config::from_env();

        assert_eq!(config.msg_rate_window_ms, 500);
        assert_eq!(config.msg_rate_max, 20);
    }

    #[test]
    fn ticket_secret_rejects_unset_default_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(TICKET_SECRET_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::remove_var("TICKET_SECRET");
        }

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("invalid chat-server production config"));
        assert!(error.contains("TICKET_SECRET"));
        assert!(error.contains("non-default value in production"));
    }

    #[test]
    fn ticket_secret_rejects_env_example_placeholder_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(TICKET_SECRET_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("TICKET_SECRET", "replace-with-a-long-random-string");
        }

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("TICKET_SECRET"));
    }

    #[test]
    fn ticket_secret_rejects_empty_value_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(TICKET_SECRET_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("TICKET_SECRET", "   ");
        }

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("TICKET_SECRET"));
    }

    #[test]
    fn ticket_secret_rejects_common_placeholder_values() {
        for placeholder in ["change-me", "changeme", "default", "password"] {
            assert!(is_default_ticket_secret(placeholder));
        }
    }

    #[test]
    fn ticket_secret_accepts_custom_value_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(TICKET_SECRET_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("TICKET_SECRET", "prod-chat-ticket-secret-123");
        }

        let config = Config::from_env();

        assert_eq!(config.ticket_secret, "prod-chat-ticket-secret-123");
    }

    #[test]
    fn ticket_secret_app_env_production_triggers_validation() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(TICKET_SECRET_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "development");
            env::set_var("APP_ENV", "production");
            env::remove_var("TICKET_SECRET");
        }

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("TICKET_SECRET"));
    }

    #[test]
    fn ticket_secret_development_allows_default_value() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(TICKET_SECRET_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::remove_var("APP_ENV");
            env::remove_var("TICKET_SECRET");
        }

        let config = Config::from_env();

        assert_eq!(config.ticket_secret, DEFAULT_TICKET_SECRET);
    }
}
