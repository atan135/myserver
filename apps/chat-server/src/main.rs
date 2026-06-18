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

use global_id::{DEFAULT_WORKER_LEASE_TTL_SECONDS, WorkerLease};
use service_registry::{RegistryClient, ServiceEndpoint, ServiceInstance};
use tracing_appender::rolling;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

const DEFAULT_OUTBOUND_QUEUE_CAPACITY: usize = 1024;
const DEFAULT_TICKET_SECRET: &str = "default_secret_change_in_production";

struct Config {
    db_enabled: bool,
    database_url: String,
    db_pool_size: u32,
    bind_addr: String,
    heartbeat_timeout_secs: u64,
    max_body_len: usize,
    msg_rate_window_ms: u64,
    msg_rate_max: u64,
    max_connections_per_player: u64,
    max_connections_per_ip: u64,
    outbound_queue_capacity: usize,
    ticket_secret: String,
    redis_url: String,
    redis_key_prefix: String,
    global_id_origin_id: u64,
    global_id_worker_id: Option<u64>,
    nats_url: String,
    registry_enabled: bool,
    discovery_required: bool,
    registry_url: String,
    registry_key_prefix: String,
    registry_heartbeat_interval_secs: u64,
    service_name: String,
    service_instance_id: String,
    service_zone: String,
    service_build_version: String,
    online_route_ttl_secs: u64,
    public_host: String,
    log_level: String,
    log_enable_console: bool,
    log_enable_file: bool,
    log_dir: String,
}

impl Config {
    fn from_env() -> Self {
        let bind_addr = bind_addr_from_env("CHAT_BIND_ADDR", "0.0.0.0:9001");
        let config = Self {
            db_enabled: parse_bool_env("DB_ENABLED", false),
            database_url: std::env::var("DATABASE_URL").unwrap_or_else(|_| {
                "postgres://postgres:password@127.0.0.1:5432/myserver_chat".to_string()
            }),
            db_pool_size: std::env::var("DB_POOL_SIZE")
                .unwrap_or_else(|_| "5".to_string())
                .parse()
                .unwrap_or(5),
            bind_addr: bind_addr.clone(),
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
            max_connections_per_player: parse_u64_env("CHAT_MAX_CONNECTIONS_PER_PLAYER", 0),
            max_connections_per_ip: parse_u64_env("CHAT_MAX_CONNECTIONS_PER_IP", 0),
            outbound_queue_capacity: parse_outbound_queue_capacity(
                std::env::var("CHAT_OUTBOUND_QUEUE_CAPACITY").ok(),
            ),
            ticket_secret: std::env::var("TICKET_SECRET")
                .unwrap_or_else(|_| DEFAULT_TICKET_SECRET.to_string()),
            redis_url: std::env::var("REDIS_URL")
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
            redis_key_prefix: std::env::var("REDIS_KEY_PREFIX").unwrap_or_default(),
            global_id_origin_id: parse_u64_env("GLOBAL_ID_ORIGIN_ID", 0),
            global_id_worker_id: parse_optional_u64_env("GLOBAL_ID_WORKER_ID"),
            nats_url: std::env::var("NATS_URL")
                .unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string()),
            registry_enabled: std::env::var("REGISTRY_ENABLED")
                .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "True"))
                .unwrap_or(false),
            discovery_required: discovery_required_from_env(),
            registry_url: std::env::var("REGISTRY_URL")
                .or_else(|_| std::env::var("REDIS_URL"))
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
            registry_key_prefix: std::env::var("REGISTRY_KEY_PREFIX")
                .or_else(|_| std::env::var("REDIS_KEY_PREFIX"))
                .unwrap_or_default(),
            registry_heartbeat_interval_secs: std::env::var("REGISTRY_HEARTBEAT_INTERVAL")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .unwrap_or(10),
            service_name: std::env::var("SERVICE_NAME")
                .unwrap_or_else(|_| "chat-server".to_string()),
            service_instance_id: std::env::var("SERVICE_INSTANCE_ID")
                .unwrap_or_else(|_| "chat-server-001".to_string()),
            service_zone: std::env::var("SERVICE_ZONE").unwrap_or_else(|_| "local".to_string()),
            service_build_version: std::env::var("SERVICE_BUILD_VERSION")
                .unwrap_or_else(|_| "dev".to_string()),
            online_route_ttl_secs: std::env::var("CHAT_ONLINE_ROUTE_TTL_SECS")
                .unwrap_or_else(|_| "60".to_string())
                .parse()
                .unwrap_or(60),
            public_host: advertised_host_from_env(
                &[
                    "SERVICE_ADVERTISED_HOST",
                    "SERVICE_PUBLIC_HOST",
                    "CHAT_PUBLIC_HOST",
                ],
                &bind_addr,
            ),
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
        validate_discovery_config(&config);

        config
    }
}

fn bind_addr_from_env(bind_addr_name: &str, default: &str) -> String {
    let bind_addr = std::env::var(bind_addr_name).unwrap_or_else(|_| default.to_string());
    let Some(bind_host) = first_non_empty_env(&["SERVICE_BIND_HOST"]) else {
        return bind_addr;
    };

    match bind_addr.parse::<SocketAddr>() {
        Ok(addr) => format!("{bind_host}:{}", addr.port()),
        Err(_) => bind_addr,
    }
}

fn first_non_empty_env(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn advertised_host_from_env(names: &[&str], bind_addr: &str) -> String {
    if let Some(host) = first_non_empty_env(names) {
        return normalize_advertised_host(&host);
    }

    let bind_host = bind_addr
        .parse::<SocketAddr>()
        .map(|addr| addr.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string());
    normalize_advertised_host(&bind_host)
}

fn normalize_advertised_host(host: &str) -> String {
    if matches!(host.trim(), "0.0.0.0" | "::" | "[::]") {
        "127.0.0.1".to_string()
    } else {
        host.trim().to_string()
    }
}

fn is_production_env() -> bool {
    ["NODE_ENV", "APP_ENV"].iter().any(|name| {
        std::env::var(name)
            .ok()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("production"))
    })
}

fn is_strict_discovery_env() -> bool {
    ["NODE_ENV", "APP_ENV"].iter().any(|name| {
        std::env::var(name).ok().is_some_and(|value| {
            let value = value.trim();
            value.eq_ignore_ascii_case("production") || value.eq_ignore_ascii_case("test")
        })
    })
}

fn discovery_required_from_env() -> bool {
    parse_bool_env("DISCOVERY_REQUIRED", false) || is_strict_discovery_env()
}

fn validate_discovery_config(config: &Config) {
    if config.discovery_required && !config.registry_enabled {
        panic!(
            "invalid chat-server discovery config: DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true"
        );
    }
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

    if !config.db_enabled {
        panic!("invalid chat-server production config: DB_ENABLED must be true in production");
    }

    if config.global_id_origin_id == 0 || config.global_id_origin_id > 1023 {
        panic!(
            "invalid chat-server production config: GLOBAL_ID_ORIGIN_ID must be set to 1-1023 in production"
        );
    }

    if config
        .global_id_worker_id
        .is_some_and(|worker_id| worker_id > 63)
    {
        panic!(
            "invalid chat-server production config: GLOBAL_ID_WORKER_ID must be set to 0-63 in production"
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

fn parse_optional_u64_env(name: &str) -> Option<u64> {
    let raw = std::env::var(name).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<u64>().ok()
}

fn parse_bool_env(name: &str, default_value: bool) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(default_value)
}

fn registry_metadata(config: &Config) -> serde_json::Value {
    serde_json::json!({
        "service_name": config.service_name,
        "service_instance_id": config.service_instance_id,
        "instance_id": config.service_instance_id,
        "online_route_ttl_secs": config.online_route_ttl_secs,
        "build_version": config.service_build_version,
        "zone": config.service_zone
    })
}

fn build_service_instance(config: &Config, port: u16) -> ServiceInstance {
    let public_host = published_host(&config.public_host);
    let metadata = registry_metadata(config);
    ServiceInstance::new(
        config.service_instance_id.clone(),
        config.service_name.clone(),
        public_host.clone(),
        port,
    )
    .with_endpoints(vec![ServiceEndpoint {
        name: "tcp".to_string(),
        protocol: "tcp".to_string(),
        host: public_host,
        port,
        socket: String::new(),
        visibility: "internal".to_string(),
        metadata: metadata.clone(),
        healthy: true,
    }])
    .with_metadata(metadata)
    .with_tags(vec!["chat".to_string(), "tcp".to_string()])
}

fn published_host(host: &str) -> String {
    let host = host.trim();
    if matches!(host, "" | "0.0.0.0" | "::" | "[::]") {
        "127.0.0.1".to_string()
    } else {
        host.to_string()
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
        db_enabled = config.db_enabled,
        redis_url = %config.redis_url,
        global_id_origin_id = config.global_id_origin_id,
        global_id_worker_id = ?config.global_id_worker_id,
        registry_enabled = config.registry_enabled,
        "chat-server starting"
    );

    let redis_client = redis::Client::open(config.redis_url.clone())?;
    let mut global_id_redis = redis_client.get_multiplexed_async_connection().await?;
    let global_id_origin_id = u16::try_from(config.global_id_origin_id).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "GLOBAL_ID_ORIGIN_ID out of range: {}",
                config.global_id_origin_id
            ),
        )
    })?;
    let global_id_worker_id = config
        .global_id_worker_id
        .map(|worker_id| {
            u8::try_from(worker_id).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("GLOBAL_ID_WORKER_ID out of range: {worker_id}"),
                )
            })
        })
        .transpose()?;
    let worker_lease = WorkerLease::acquire_redis(
        &mut global_id_redis,
        &config.redis_key_prefix,
        global_id_origin_id,
        global_id_worker_id,
        &config.service_name,
        &config.service_instance_id,
        DEFAULT_WORKER_LEASE_TTL_SECONDS,
    )
    .await?;
    tracing::info!(
        origin_id = worker_lease.origin_id,
        worker_id = worker_lease.worker_id,
        lease_key = %worker_lease.key,
        "global id worker lease acquired"
    );
    chat_service::initialize_global_id_generator(worker_lease.generator()?)?;
    let lease_for_renewal = worker_lease.clone();
    let lease_renew_client = redis_client.clone();
    let lease_renew_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            match lease_renew_client.get_multiplexed_async_connection().await {
                Ok(mut redis) => {
                    if !lease_for_renewal
                        .renew_redis(&mut redis)
                        .await
                        .unwrap_or(false)
                    {
                        tracing::warn!(
                            lease_key = %lease_for_renewal.key,
                            "global id worker lease renewal lost ownership"
                        );
                    }
                }
                Err(error) => {
                    lease_for_renewal.deactivate();
                    tracing::warn!(
                        lease_key = %lease_for_renewal.key,
                        error = %error,
                        "global id worker lease renewal failed"
                    );
                }
            }
        }
    });

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
                let port = extract_port(&config.bind_addr)?;
                let instance = build_service_instance(&config, port);

                if let Err(e) = client.register(&instance).await {
                    tracing::error!(error = %e, "failed to register service");
                    if registry_failure_is_fatal(&config) {
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
                if registry_failure_is_fatal(&config) {
                    return Err(std::io::Error::other(e.to_string()).into());
                }
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

    let chat_store =
        chat_store::ChatStore::new(config.db_enabled, &config.database_url, config.db_pool_size)
            .await?;

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
        max_connections_per_player: config.max_connections_per_player,
        max_connections_per_ip: config.max_connections_per_ip,
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
        if let Err(e) = mail_subscriber::subscribe_mail_notifications(
            nats_url_for_mail,
            instance_id_for_mail,
            sessions_for_mail,
        )
        .await
        {
            tracing::error!("mail subscriber error: {}", e);
        }
    });

    tracing::info!("mail notification subscriber started");

    let result = chat_server::run(server_config, chat_store.clone(), chat_sessions).await;

    lease_renew_task.abort();
    let _ = lease_renew_task.await;
    match redis_client.get_multiplexed_async_connection().await {
        Ok(mut redis) => {
            if let Err(error) = worker_lease.release_redis(&mut redis).await {
                tracing::error!(
                    lease_key = %worker_lease.key,
                    error = %error,
                    "failed to release global id worker lease"
                );
            }
        }
        Err(error) => {
            tracing::error!(
                lease_key = %worker_lease.key,
                error = %error,
                "failed to connect redis for global id worker lease release"
            );
        }
    }

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

fn registry_failure_is_fatal(config: &Config) -> bool {
    config.discovery_required
        || env_name_is("NODE_ENV", "production")
        || env_name_is("APP_ENV", "production")
        || env_name_is("NODE_ENV", "test")
        || env_name_is("APP_ENV", "test")
}

fn env_name_is(name: &str, expected: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::{Mutex, OnceLock};

    use super::*;

    const TICKET_SECRET_ENV_NAMES: &[&str] = &[
        "NODE_ENV",
        "APP_ENV",
        "DISCOVERY_REQUIRED",
        "REGISTRY_ENABLED",
        "REGISTRY_KEY_PREFIX",
        "REDIS_KEY_PREFIX",
        "SERVICE_ZONE",
        "TICKET_SECRET",
        "DB_ENABLED",
        "GLOBAL_ID_ORIGIN_ID",
        "GLOBAL_ID_WORKER_ID",
    ];

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

    fn set_valid_production_global_id_env() {
        unsafe {
            env::set_var("GLOBAL_ID_ORIGIN_ID", "1");
            env::set_var("GLOBAL_ID_WORKER_ID", "4");
        }
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
    fn chat_connection_limit_config_defaults_to_disabled() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "CHAT_MAX_CONNECTIONS_PER_PLAYER",
            "CHAT_MAX_CONNECTIONS_PER_IP",
        ]);

        unsafe {
            env::remove_var("CHAT_MAX_CONNECTIONS_PER_PLAYER");
            env::remove_var("CHAT_MAX_CONNECTIONS_PER_IP");
        }

        let config = Config::from_env();

        assert_eq!(config.max_connections_per_player, 0);
        assert_eq!(config.max_connections_per_ip, 0);
    }

    #[test]
    fn chat_connection_limit_config_accepts_env_values() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "CHAT_MAX_CONNECTIONS_PER_PLAYER",
            "CHAT_MAX_CONNECTIONS_PER_IP",
        ]);

        unsafe {
            env::set_var("CHAT_MAX_CONNECTIONS_PER_PLAYER", "2");
            env::set_var("CHAT_MAX_CONNECTIONS_PER_IP", "10");
        }

        let config = Config::from_env();

        assert_eq!(config.max_connections_per_player, 2);
        assert_eq!(config.max_connections_per_ip, 10);
    }

    #[test]
    fn service_build_version_defaults_to_dev() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&["SERVICE_BUILD_VERSION", "SERVICE_ZONE"]);

        unsafe {
            env::remove_var("SERVICE_BUILD_VERSION");
            env::remove_var("SERVICE_ZONE");
        }

        let config = Config::from_env();

        assert_eq!(config.service_build_version, "dev");
        assert_eq!(config.service_zone, "local");
    }

    #[test]
    fn service_identity_accepts_env_values() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "SERVICE_NAME",
            "SERVICE_INSTANCE_ID",
            "SERVICE_ZONE",
            "SERVICE_BUILD_VERSION",
        ]);

        unsafe {
            env::set_var("SERVICE_NAME", "chat-server-blue");
            env::set_var("SERVICE_INSTANCE_ID", "chat-blue-001");
            env::set_var("SERVICE_ZONE", "zone-a");
            env::set_var("SERVICE_BUILD_VERSION", "2026.06.18+abc123");
        }

        let config = Config::from_env();

        assert_eq!(config.service_name, "chat-server-blue");
        assert_eq!(config.service_instance_id, "chat-blue-001");
        assert_eq!(config.service_zone, "zone-a");
        assert_eq!(config.service_build_version, "2026.06.18+abc123");
    }

    #[test]
    fn endpoint_publish_hosts_prefer_unified_env_and_never_advertise_wildcard_bind() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "SERVICE_BIND_HOST",
            "SERVICE_PUBLIC_HOST",
            "SERVICE_ADVERTISED_HOST",
            "CHAT_BIND_ADDR",
            "CHAT_PUBLIC_HOST",
        ]);

        unsafe {
            env::set_var("SERVICE_BIND_HOST", "0.0.0.0");
            env::set_var("SERVICE_PUBLIC_HOST", "10.0.0.50");
            env::set_var("CHAT_BIND_ADDR", "127.0.0.9:9001");
            env::set_var("CHAT_PUBLIC_HOST", "10.0.0.99");
        }

        let config = Config::from_env();

        assert_eq!(config.bind_addr, "0.0.0.0:9001");
        assert_eq!(config.public_host, "10.0.0.50");

        unsafe {
            env::remove_var("SERVICE_PUBLIC_HOST");
            env::remove_var("SERVICE_ADVERTISED_HOST");
            env::remove_var("CHAT_PUBLIC_HOST");
        }

        let config = Config::from_env();

        assert_eq!(config.public_host, "127.0.0.1");
    }

    #[test]
    fn registry_key_prefix_prefers_registry_specific_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(TICKET_SECRET_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::remove_var("APP_ENV");
            env::set_var("REGISTRY_KEY_PREFIX", "registry:");
            env::set_var("REDIS_KEY_PREFIX", "redis:");
        }

        let config = Config::from_env();
        assert_eq!(config.registry_key_prefix, "registry:");

        unsafe {
            env::remove_var("REGISTRY_KEY_PREFIX");
        }

        let config = Config::from_env();
        assert_eq!(config.registry_key_prefix, "redis:");
    }

    #[test]
    fn service_registry_instance_and_endpoint_include_discovery_metadata() {
        let config = Config {
            db_enabled: false,
            database_url: "postgres://postgres:password@127.0.0.1:5432/myserver_chat".to_string(),
            db_pool_size: 5,
            bind_addr: "0.0.0.0:9001".to_string(),
            heartbeat_timeout_secs: 30,
            max_body_len: 4096,
            msg_rate_window_ms: 1000,
            msg_rate_max: 0,
            max_connections_per_player: 0,
            max_connections_per_ip: 0,
            outbound_queue_capacity: DEFAULT_OUTBOUND_QUEUE_CAPACITY,
            ticket_secret: "test-secret".to_string(),
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: String::new(),
            global_id_origin_id: 1,
            global_id_worker_id: Some(4),
            nats_url: "nats://127.0.0.1:4222".to_string(),
            registry_enabled: true,
            discovery_required: false,
            registry_url: "redis://127.0.0.1:6379".to_string(),
            registry_key_prefix: String::new(),
            registry_heartbeat_interval_secs: 10,
            service_name: "chat-server".to_string(),
            service_instance_id: "chat-a".to_string(),
            service_zone: "zone-chat".to_string(),
            service_build_version: "build-42".to_string(),
            online_route_ttl_secs: 75,
            public_host: "10.0.0.8".to_string(),
            log_level: "info".to_string(),
            log_enable_console: true,
            log_enable_file: false,
            log_dir: "logs".to_string(),
        };

        let instance = build_service_instance(&config, 9001);

        assert_eq!(instance.metadata["service_name"], "chat-server");
        assert_eq!(instance.metadata["service_instance_id"], "chat-a");
        assert_eq!(instance.metadata["instance_id"], "chat-a");
        assert_eq!(instance.metadata["online_route_ttl_secs"], 75);
        assert_eq!(instance.metadata["build_version"], "build-42");
        assert_eq!(instance.metadata["zone"], "zone-chat");
        assert_eq!(instance.endpoints.len(), 1);

        let endpoint = &instance.endpoints[0];
        assert_eq!(endpoint.name, "tcp");
        assert_eq!(endpoint.protocol, "tcp");
        assert_eq!(endpoint.host, "10.0.0.8");
        assert_eq!(endpoint.port, 9001);
        assert_eq!(endpoint.visibility, "internal");
        assert_eq!(endpoint.metadata["service_name"], "chat-server");
        assert_eq!(endpoint.metadata["service_instance_id"], "chat-a");
        assert_eq!(endpoint.metadata["instance_id"], "chat-a");
        assert_eq!(endpoint.metadata["online_route_ttl_secs"], 75);
        assert_eq!(endpoint.metadata["build_version"], "build-42");
        assert_eq!(endpoint.metadata["zone"], "zone-chat");
    }

    #[test]
    fn service_instance_never_publishes_wildcard_network_hosts() {
        let mut config = Config {
            db_enabled: false,
            database_url: "postgres://postgres:password@127.0.0.1:5432/myserver_chat".to_string(),
            db_pool_size: 5,
            bind_addr: "0.0.0.0:9001".to_string(),
            heartbeat_timeout_secs: 30,
            max_body_len: 4096,
            msg_rate_window_ms: 1000,
            msg_rate_max: 0,
            max_connections_per_player: 0,
            max_connections_per_ip: 0,
            outbound_queue_capacity: DEFAULT_OUTBOUND_QUEUE_CAPACITY,
            ticket_secret: "test-secret".to_string(),
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: String::new(),
            global_id_origin_id: 1,
            global_id_worker_id: Some(4),
            nats_url: "nats://127.0.0.1:4222".to_string(),
            registry_enabled: true,
            discovery_required: false,
            registry_url: "redis://127.0.0.1:6379".to_string(),
            registry_key_prefix: String::new(),
            registry_heartbeat_interval_secs: 10,
            service_name: "chat-server".to_string(),
            service_instance_id: "chat-a".to_string(),
            service_zone: "zone-chat".to_string(),
            service_build_version: "build-42".to_string(),
            online_route_ttl_secs: 75,
            public_host: "0.0.0.0".to_string(),
            log_level: "info".to_string(),
            log_enable_console: true,
            log_enable_file: false,
            log_dir: "logs".to_string(),
        };

        let instance = build_service_instance(&config, 9001);

        assert_eq!(instance.host, "127.0.0.1");
        assert_eq!(instance.endpoints[0].host, "127.0.0.1");

        config.public_host = "::".to_string();
        let instance = build_service_instance(&config, 9001);

        assert_eq!(instance.host, "127.0.0.1");
        assert_eq!(instance.endpoints[0].host, "127.0.0.1");
    }

    #[test]
    fn ticket_secret_rejects_unset_default_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(TICKET_SECRET_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("REGISTRY_ENABLED", "true");
            env::remove_var("TICKET_SECRET");
            env::set_var("DB_ENABLED", "true");
        }
        set_valid_production_global_id_env();

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
            env::set_var("REGISTRY_ENABLED", "true");
            env::set_var("TICKET_SECRET", "replace-with-a-long-random-string");
            env::set_var("DB_ENABLED", "true");
        }
        set_valid_production_global_id_env();

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
            env::set_var("REGISTRY_ENABLED", "true");
            env::set_var("TICKET_SECRET", "   ");
            env::set_var("DB_ENABLED", "true");
        }
        set_valid_production_global_id_env();

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
            env::set_var("REGISTRY_ENABLED", "true");
            env::set_var("TICKET_SECRET", "prod-chat-ticket-secret-123");
            env::set_var("DB_ENABLED", "true");
        }
        set_valid_production_global_id_env();

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
            env::set_var("REGISTRY_ENABLED", "true");
            env::remove_var("TICKET_SECRET");
            env::set_var("DB_ENABLED", "true");
        }
        set_valid_production_global_id_env();

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

    #[test]
    fn required_discovery_rejects_registry_disabled() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(TICKET_SECRET_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::remove_var("APP_ENV");
            env::set_var("DISCOVERY_REQUIRED", "true");
            env::set_var("REGISTRY_ENABLED", "false");
        }

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true"));
    }

    #[test]
    fn test_environment_requires_registry_discovery() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(TICKET_SECRET_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::set_var("APP_ENV", "test");
            env::remove_var("DISCOVERY_REQUIRED");
            env::set_var("REGISTRY_ENABLED", "false");
        }

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true"));
    }
}
