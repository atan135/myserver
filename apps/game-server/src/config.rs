use std::env;

pub const DEFAULT_TICKET_SECRET: &str = "dev-only-change-this-ticket-secret";
pub const DEFAULT_ADMIN_TOKEN: &str = "dev-only-change-this-game-admin-token";
pub const DEFAULT_INTERNAL_TOKEN: &str = "dev-only-change-this-game-internal-token";
pub const DEFAULT_OUTBOUND_QUEUE_CAPACITY: usize = 1024;

#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub csv_dir: String,
    pub csv_reload_enabled: bool,
    pub csv_reload_interval_secs: u64,
    pub room_cleanup_interval_secs: u64,
    pub admin_host: String,
    pub admin_port: u16,
    pub admin_token: String,
    pub admin_audit_enabled: bool,
    pub admin_audit_path: String,
    pub admin_audit_require_actor: bool,
    pub internal_token: String,
    pub local_socket_name: String,
    pub internal_socket_name: String,
    pub log_level: String,
    pub log_enable_console: bool,
    pub log_enable_file: bool,
    pub log_dir: String,
    pub redis_url: String,
    pub redis_key_prefix: String,
    pub global_id_origin_id: u64,
    pub global_id_worker_id: Option<u64>,
    pub nats_url: String,
    pub db_enabled: bool,
    pub database_url: String,
    pub db_pool_size: u32,
    pub ticket_secret: String,
    pub heartbeat_timeout_secs: u64,
    pub max_body_len: usize,
    pub outbound_queue_capacity: usize,
    pub msg_rate_window_ms: u64,
    pub msg_rate_max: u64,
    pub player_msg_rate_window_ms: u64,
    pub player_msg_rate_max: u64,
    pub input_timestamp_required: bool,
    pub input_timestamp_max_skew_ms: u64,
    pub input_anomaly_window_ms: u64,
    pub input_anomaly_max: u64,
    // Service Registry
    pub registry_enabled: bool,
    pub discovery_required: bool,
    pub registry_url: String,
    pub registry_key_prefix: String,
    pub registry_heartbeat_interval_secs: u64,
    pub service_name: String,
    pub service_instance_id: String,
    pub service_build_version: String,
    pub service_zone: String,
    pub service_rollout_epoch: String,
}

fn parse_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(default)
}

fn parse_u64(name: &str, default: u64) -> u64 {
    parse_u64_value(env::var(name).ok(), default)
}

fn parse_optional_u64(name: &str) -> Option<u64> {
    let raw = env::var(name).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    trimmed.parse::<u64>().ok()
}

fn parse_u64_value(value: Option<String>, default: u64) -> u64 {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn parse_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn parse_positive_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn parse_non_empty_string(name: &str, default: &str) -> String {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn parse_first_non_empty_string(names: &[&str], default: &str) -> String {
    names
        .iter()
        .find_map(|name| {
            env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| default.to_string())
}

impl Config {
    pub fn from_env() -> Self {
        let host = env::var("GAME_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port = env::var("GAME_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(7000);
        let csv_dir = env::var("CSV_DIR").unwrap_or_else(|_| "csv".to_string());
        let csv_reload_enabled = parse_bool("CSV_RELOAD_ENABLED", true);
        let csv_reload_interval_secs = env::var("CSV_RELOAD_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(3);
        let room_cleanup_interval_secs = env::var("ROOM_CLEANUP_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(10);
        let admin_host = env::var("ADMIN_HOST").unwrap_or_else(|_| host.clone());
        let admin_port = env::var("ADMIN_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(7500);
        let admin_token =
            env::var("GAME_ADMIN_TOKEN").unwrap_or_else(|_| DEFAULT_ADMIN_TOKEN.to_string());
        let admin_audit_enabled = parse_bool("GAME_ADMIN_AUDIT_ENABLED", true);
        let admin_audit_path = env::var("GAME_ADMIN_AUDIT_PATH")
            .unwrap_or_else(|_| "logs/game-server/admin-audit.jsonl".to_string());
        let admin_audit_require_actor = parse_bool("GAME_ADMIN_AUDIT_REQUIRE_ACTOR", false);
        let internal_token =
            env::var("GAME_INTERNAL_TOKEN").unwrap_or_else(|_| DEFAULT_INTERNAL_TOKEN.to_string());
        let local_socket_name = env::var("GAME_LOCAL_SOCKET_NAME")
            .unwrap_or_else(|_| "myserver-game-server.sock".to_string());
        let internal_socket_name = env::var("GAME_INTERNAL_SOCKET_NAME")
            .unwrap_or_else(|_| derive_internal_socket_name(&local_socket_name));
        let log_level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
        let log_enable_console = parse_bool("LOG_ENABLE_CONSOLE", true);
        let log_enable_file = parse_bool("LOG_ENABLE_FILE", true);
        let log_dir = env::var("LOG_DIR").unwrap_or_else(|_| "logs/game-server".to_string());
        let redis_url =
            env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let redis_key_prefix = env::var("REDIS_KEY_PREFIX").unwrap_or_default();
        let global_id_origin_id = parse_u64("GLOBAL_ID_ORIGIN_ID", 0);
        let global_id_worker_id = parse_optional_u64("GLOBAL_ID_WORKER_ID");
        let nats_url = env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let db_enabled = parse_bool("DB_ENABLED", false);
        let database_url = env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://postgres:password@127.0.0.1:5432/myserver_game".to_string()
        });
        let db_pool_size = env::var("DB_POOL_SIZE")
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(10);
        let ticket_secret =
            env::var("TICKET_SECRET").unwrap_or_else(|_| DEFAULT_TICKET_SECRET.to_string());
        let heartbeat_timeout_secs = env::var("HEARTBEAT_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30);
        let max_body_len = parse_usize("MAX_BODY_LEN", 4096);
        let outbound_queue_capacity =
            parse_positive_usize("OUTBOUND_QUEUE_CAPACITY", DEFAULT_OUTBOUND_QUEUE_CAPACITY);
        let msg_rate_window_ms = parse_u64("MSG_RATE_WINDOW_MS", 1000);
        let msg_rate_max = parse_u64("MSG_RATE_MAX", 0);
        let player_msg_rate_window_ms = parse_u64("PLAYER_MSG_RATE_WINDOW_MS", 1000);
        let player_msg_rate_max = parse_u64("PLAYER_MSG_RATE_MAX", 0);
        let input_timestamp_required = parse_bool("INPUT_TIMESTAMP_REQUIRED", false);
        let input_timestamp_max_skew_ms = parse_u64("INPUT_TIMESTAMP_MAX_SKEW_MS", 5000);
        let input_anomaly_window_ms = parse_u64("INPUT_ANOMALY_WINDOW_MS", 10_000);
        let input_anomaly_max = parse_u64("INPUT_ANOMALY_MAX", 0);

        // Service Registry
        let registry_enabled = parse_bool("REGISTRY_ENABLED", false);
        let discovery_required = discovery_required_from_env();
        let registry_url = env::var("REGISTRY_URL")
            .or_else(|_| env::var("REDIS_URL"))
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let registry_key_prefix = env::var("REGISTRY_KEY_PREFIX")
            .or_else(|_| env::var("REDIS_KEY_PREFIX"))
            .unwrap_or_default();
        let registry_heartbeat_interval_secs = parse_u64("REGISTRY_HEARTBEAT_INTERVAL", 10);
        let service_name = env::var("SERVICE_NAME").unwrap_or_else(|_| "game-server".to_string());
        let service_instance_id = env::var("SERVICE_INSTANCE_ID")
            .unwrap_or_else(|_| format!("{}-{}", service_name, port));
        let service_build_version = parse_non_empty_string("SERVICE_BUILD_VERSION", "dev");
        let service_zone = parse_non_empty_string("SERVICE_ZONE", "local");
        let service_rollout_epoch =
            parse_first_non_empty_string(&["SERVICE_ROLLOUT_EPOCH", "ROLLOUT_EPOCH"], "default");

        let config = Self {
            host,
            port,
            csv_dir,
            csv_reload_enabled,
            csv_reload_interval_secs,
            room_cleanup_interval_secs,
            admin_host,
            admin_port,
            admin_token,
            admin_audit_enabled,
            admin_audit_path,
            admin_audit_require_actor,
            internal_token,
            local_socket_name,
            internal_socket_name,
            log_level,
            log_enable_console,
            log_enable_file,
            log_dir,
            redis_url,
            redis_key_prefix,
            global_id_origin_id,
            global_id_worker_id,
            nats_url,
            db_enabled,
            database_url,
            db_pool_size,
            ticket_secret,
            heartbeat_timeout_secs,
            max_body_len,
            outbound_queue_capacity,
            msg_rate_window_ms,
            msg_rate_max,
            player_msg_rate_window_ms,
            player_msg_rate_max,
            input_timestamp_required,
            input_timestamp_max_skew_ms,
            input_anomaly_window_ms,
            input_anomaly_max,
            registry_enabled,
            discovery_required,
            registry_url,
            registry_key_prefix,
            registry_heartbeat_interval_secs,
            service_name,
            service_instance_id,
            service_build_version,
            service_zone,
            service_rollout_epoch,
        };

        validate_production_config(&config);
        validate_discovery_config(&config);

        config
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn admin_bind_addr(&self) -> String {
        format!("{}:{}", self.admin_host, self.admin_port)
    }

    pub fn service_instance_metadata(&self) -> serde_json::Value {
        serde_json::json!({
            "internal_socket": self.internal_socket_name.clone(),
            "instance_id": self.service_instance_id.clone(),
            "server_id": self.service_instance_id.clone(),
            "rollout_epoch": self.service_rollout_epoch.clone(),
            "drain_mode": false,
            "build_version": self.service_build_version.clone(),
            "zone": self.service_zone.clone()
        })
    }
}

fn derive_internal_socket_name(local_socket_name: &str) -> String {
    if let Some(prefix) = local_socket_name.strip_suffix(".sock") {
        return format!("{prefix}-internal.sock");
    }

    format!("{local_socket_name}-internal")
}

fn is_production_env() -> bool {
    ["NODE_ENV", "APP_ENV"].iter().any(|name| {
        env::var(name)
            .ok()
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("production"))
    })
}

fn is_strict_discovery_env() -> bool {
    ["NODE_ENV", "APP_ENV"].iter().any(|name| {
        env::var(name).ok().is_some_and(|value| {
            let value = value.trim();
            value.eq_ignore_ascii_case("production") || value.eq_ignore_ascii_case("test")
        })
    })
}

fn discovery_required_from_env() -> bool {
    parse_bool("DISCOVERY_REQUIRED", false) || is_strict_discovery_env()
}

fn validate_discovery_config(config: &Config) {
    if config.discovery_required && !config.registry_enabled {
        panic!(
            "invalid game-server discovery config: DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true"
        );
    }
}

fn validate_production_config(config: &Config) {
    if !is_production_env() {
        return;
    }

    let mut errors = Vec::new();

    if is_default_secret(&config.ticket_secret, DEFAULT_TICKET_SECRET) {
        errors.push("TICKET_SECRET must be set to a non-default value in production");
    }

    if is_default_secret(&config.admin_token, DEFAULT_ADMIN_TOKEN) {
        errors.push("GAME_ADMIN_TOKEN must be set to a non-default value in production");
    }

    if !config.admin_audit_enabled {
        errors.push("GAME_ADMIN_AUDIT_ENABLED=false is not allowed in production");
    }

    if is_default_secret(&config.internal_token, DEFAULT_INTERNAL_TOKEN) {
        errors.push("GAME_INTERNAL_TOKEN must be set to a non-default value in production");
    }

    if !config.db_enabled {
        errors.push("DB_ENABLED must be true in production");
    }

    if config.global_id_origin_id == 0 || config.global_id_origin_id > 1023 {
        errors.push("GLOBAL_ID_ORIGIN_ID must be set to 1-1023 in production");
    }

    if config
        .global_id_worker_id
        .is_some_and(|worker_id| worker_id > 63)
    {
        errors.push("GLOBAL_ID_WORKER_ID must be set to 0-63 in production");
    }

    if !errors.is_empty() {
        panic!(
            "invalid game-server production config: {}",
            errors.join("; ")
        );
    }
}

fn is_default_secret(value: &str, service_default: &str) -> bool {
    let normalized = value.trim();

    normalized.is_empty()
        || matches!(
            normalized,
            "replace-with-a-long-random-string" | "change-me" | "changeme" | "default" | "password"
        )
        || normalized == service_default
}

#[cfg(test)]
mod tests {
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::{Mutex, OnceLock};

    use super::*;

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

    const SECURITY_ENV_NAMES: &[&str] = &[
        "NODE_ENV",
        "APP_ENV",
        "DISCOVERY_REQUIRED",
        "REGISTRY_ENABLED",
        "REGISTRY_KEY_PREFIX",
        "REDIS_KEY_PREFIX",
        "TICKET_SECRET",
        "GAME_ADMIN_TOKEN",
        "GAME_ADMIN_AUDIT_ENABLED",
        "GAME_ADMIN_AUDIT_PATH",
        "GAME_ADMIN_AUDIT_REQUIRE_ACTOR",
        "GAME_INTERNAL_TOKEN",
        "DB_ENABLED",
        "GLOBAL_ID_ORIGIN_ID",
        "GLOBAL_ID_WORKER_ID",
    ];

    const OUTBOUND_QUEUE_ENV_NAMES: &[&str] = &[
        "NODE_ENV",
        "APP_ENV",
        "DISCOVERY_REQUIRED",
        "REGISTRY_ENABLED",
        "REGISTRY_KEY_PREFIX",
        "REDIS_KEY_PREFIX",
        "OUTBOUND_QUEUE_CAPACITY",
        "GLOBAL_ID_ORIGIN_ID",
        "GLOBAL_ID_WORKER_ID",
    ];

    const SERVICE_METADATA_ENV_NAMES: &[&str] = &[
        "NODE_ENV",
        "APP_ENV",
        "DISCOVERY_REQUIRED",
        "REGISTRY_ENABLED",
        "REGISTRY_KEY_PREFIX",
        "REDIS_KEY_PREFIX",
        "GAME_PORT",
        "GAME_LOCAL_SOCKET_NAME",
        "GAME_INTERNAL_SOCKET_NAME",
        "SERVICE_NAME",
        "SERVICE_INSTANCE_ID",
        "SERVICE_BUILD_VERSION",
        "SERVICE_ZONE",
        "SERVICE_ROLLOUT_EPOCH",
        "ROLLOUT_EPOCH",
        "GLOBAL_ID_ORIGIN_ID",
        "GLOBAL_ID_WORKER_ID",
    ];

    fn clear_production_env() {
        unsafe {
            env::remove_var("NODE_ENV");
            env::remove_var("APP_ENV");
        }
    }

    fn set_custom_production_tokens() {
        unsafe {
            env::set_var("TICKET_SECRET", "prod-ticket-secret-123");
            env::set_var("GAME_ADMIN_TOKEN", "prod-game-admin-token-123");
            env::set_var("GAME_INTERNAL_TOKEN", "prod-game-internal-token-123");
        }
    }

    fn set_valid_production_infra() {
        unsafe {
            env::set_var("REGISTRY_ENABLED", "true");
            env::set_var("DB_ENABLED", "true");
            env::set_var("GLOBAL_ID_ORIGIN_ID", "1");
            env::set_var("GLOBAL_ID_WORKER_ID", "1");
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
    fn parse_u64_value_uses_default_for_missing_or_invalid_value() {
        assert_eq!(parse_u64_value(None, 1000), 1000);
        assert_eq!(parse_u64_value(Some("invalid".to_string()), 1000), 1000);
    }

    #[test]
    fn parse_u64_value_accepts_valid_value() {
        assert_eq!(parse_u64_value(Some("250".to_string()), 1000), 250);
    }

    #[test]
    fn outbound_queue_capacity_defaults_when_env_missing() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(OUTBOUND_QUEUE_ENV_NAMES);

        unsafe {
            clear_production_env();
            env::remove_var("OUTBOUND_QUEUE_CAPACITY");
        }

        let config = Config::from_env();

        assert_eq!(
            config.outbound_queue_capacity,
            DEFAULT_OUTBOUND_QUEUE_CAPACITY
        );
    }

    #[test]
    fn outbound_queue_capacity_accepts_positive_env_value() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(OUTBOUND_QUEUE_ENV_NAMES);

        unsafe {
            clear_production_env();
            env::set_var("OUTBOUND_QUEUE_CAPACITY", "2048");
        }

        let config = Config::from_env();

        assert_eq!(config.outbound_queue_capacity, 2048);
    }

    #[test]
    fn outbound_queue_capacity_falls_back_for_invalid_or_zero_value() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(OUTBOUND_QUEUE_ENV_NAMES);

        unsafe {
            clear_production_env();
            env::set_var("OUTBOUND_QUEUE_CAPACITY", "invalid");
        }
        let config = Config::from_env();
        assert_eq!(
            config.outbound_queue_capacity,
            DEFAULT_OUTBOUND_QUEUE_CAPACITY
        );

        unsafe {
            env::set_var("OUTBOUND_QUEUE_CAPACITY", "0");
        }
        let config = Config::from_env();
        assert_eq!(
            config.outbound_queue_capacity,
            DEFAULT_OUTBOUND_QUEUE_CAPACITY
        );
    }

    #[test]
    fn global_id_config_defaults_and_accepts_env_overrides() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(OUTBOUND_QUEUE_ENV_NAMES);

        unsafe {
            clear_production_env();
            env::remove_var("GLOBAL_ID_ORIGIN_ID");
            env::remove_var("GLOBAL_ID_WORKER_ID");
        }

        let config = Config::from_env();

        assert_eq!(config.global_id_origin_id, 0);
        assert_eq!(config.global_id_worker_id, None);

        unsafe {
            env::set_var("GLOBAL_ID_ORIGIN_ID", "7");
            env::set_var("GLOBAL_ID_WORKER_ID", "3");
        }

        let config = Config::from_env();

        assert_eq!(config.global_id_origin_id, 7);
        assert_eq!(config.global_id_worker_id, Some(3));
    }

    #[test]
    fn registry_key_prefix_prefers_registry_specific_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SERVICE_METADATA_ENV_NAMES);

        unsafe {
            clear_production_env();
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
    fn service_metadata_config_uses_stable_defaults() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SERVICE_METADATA_ENV_NAMES);

        unsafe {
            clear_production_env();
            env::remove_var("GAME_PORT");
            env::remove_var("GAME_LOCAL_SOCKET_NAME");
            env::remove_var("GAME_INTERNAL_SOCKET_NAME");
            env::remove_var("SERVICE_NAME");
            env::remove_var("SERVICE_INSTANCE_ID");
            env::remove_var("SERVICE_BUILD_VERSION");
            env::remove_var("SERVICE_ZONE");
            env::remove_var("SERVICE_ROLLOUT_EPOCH");
            env::remove_var("ROLLOUT_EPOCH");
        }

        let config = Config::from_env();
        let metadata = config.service_instance_metadata();

        assert_eq!(config.service_build_version, "dev");
        assert_eq!(config.service_zone, "local");
        assert_eq!(config.service_rollout_epoch, "default");
        assert_eq!(metadata["instance_id"], "game-server-7000");
        assert_eq!(metadata["server_id"], "game-server-7000");
        assert_eq!(metadata["build_version"], "dev");
        assert_eq!(metadata["zone"], "local");
        assert_eq!(metadata["rollout_epoch"], "default");
        assert_eq!(metadata["drain_mode"], false);
        assert_eq!(
            metadata["internal_socket"],
            "myserver-game-server-internal.sock"
        );
    }

    #[test]
    fn service_metadata_config_accepts_env_overrides_and_rollout_fallback() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SERVICE_METADATA_ENV_NAMES);

        unsafe {
            clear_production_env();
            env::set_var("SERVICE_INSTANCE_ID", "gs-42");
            env::set_var("SERVICE_BUILD_VERSION", " 2026.06.18 ");
            env::set_var("SERVICE_ZONE", " zone-a ");
            env::remove_var("SERVICE_ROLLOUT_EPOCH");
            env::set_var("ROLLOUT_EPOCH", " epoch-fallback ");
            env::set_var("GAME_INTERNAL_SOCKET_NAME", "gs-42-internal.sock");
        }

        let config = Config::from_env();
        let metadata = config.service_instance_metadata();

        assert_eq!(config.service_build_version, "2026.06.18");
        assert_eq!(config.service_zone, "zone-a");
        assert_eq!(config.service_rollout_epoch, "epoch-fallback");
        assert_eq!(metadata["instance_id"], "gs-42");
        assert_eq!(metadata["server_id"], "gs-42");
        assert_eq!(metadata["build_version"], "2026.06.18");
        assert_eq!(metadata["zone"], "zone-a");
        assert_eq!(metadata["rollout_epoch"], "epoch-fallback");
        assert_eq!(metadata["drain_mode"], false);
        assert_eq!(metadata["internal_socket"], "gs-42-internal.sock");

        unsafe {
            env::set_var("SERVICE_ROLLOUT_EPOCH", " epoch-primary ");
        }

        let config = Config::from_env();
        assert_eq!(config.service_rollout_epoch, "epoch-primary");
        assert_eq!(
            config.service_instance_metadata()["rollout_epoch"],
            "epoch-primary"
        );
    }

    #[test]
    fn rejects_default_ticket_secret_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::remove_var("TICKET_SECRET");
            env::set_var("GAME_ADMIN_TOKEN", "prod-game-admin-token-123");
            env::set_var("GAME_INTERNAL_TOKEN", "prod-game-internal-token-123");
        }
        set_valid_production_infra();

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("invalid game-server production config"));
        assert!(error.contains("TICKET_SECRET"));
    }

    #[test]
    fn rejects_env_example_ticket_secret_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("TICKET_SECRET", "replace-with-a-long-random-string");
            env::set_var("GAME_ADMIN_TOKEN", "prod-game-admin-token-123");
            env::set_var("GAME_INTERNAL_TOKEN", "prod-game-internal-token-123");
        }
        set_valid_production_infra();

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("TICKET_SECRET"));
    }

    #[test]
    fn rejects_default_game_admin_token_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("TICKET_SECRET", "prod-ticket-secret-123");
            env::remove_var("GAME_ADMIN_TOKEN");
            env::set_var("GAME_INTERNAL_TOKEN", "prod-game-internal-token-123");
        }
        set_valid_production_infra();

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("GAME_ADMIN_TOKEN"));
    }

    #[test]
    fn rejects_empty_game_admin_token_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("TICKET_SECRET", "prod-ticket-secret-123");
            env::set_var("GAME_ADMIN_TOKEN", "");
            env::set_var("GAME_INTERNAL_TOKEN", "prod-game-internal-token-123");
        }
        set_valid_production_infra();

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("GAME_ADMIN_TOKEN"));
    }

    #[test]
    fn rejects_default_game_internal_token_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("TICKET_SECRET", "prod-ticket-secret-123");
            env::set_var("GAME_ADMIN_TOKEN", "prod-game-admin-token-123");
            env::remove_var("GAME_INTERNAL_TOKEN");
        }
        set_valid_production_infra();

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("GAME_INTERNAL_TOKEN"));
    }

    #[test]
    fn rejects_empty_game_internal_token_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("TICKET_SECRET", "prod-ticket-secret-123");
            env::set_var("GAME_ADMIN_TOKEN", "prod-game-admin-token-123");
            env::set_var("GAME_INTERNAL_TOKEN", "");
        }
        set_valid_production_infra();

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("GAME_INTERNAL_TOKEN"));
    }

    #[test]
    fn accepts_custom_tokens_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
        }
        set_custom_production_tokens();
        unsafe {
            env::set_var("GAME_ADMIN_AUDIT_ENABLED", "true");
        }
        set_valid_production_infra();

        let config = Config::from_env();

        assert_eq!(config.ticket_secret, "prod-ticket-secret-123");
        assert_eq!(config.admin_token, "prod-game-admin-token-123");
        assert_eq!(config.internal_token, "prod-game-internal-token-123");
    }

    #[test]
    fn app_env_production_triggers_production_validation() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "development");
            env::set_var("APP_ENV", "production");
            env::remove_var("TICKET_SECRET");
            env::set_var("GAME_ADMIN_TOKEN", "prod-game-admin-token-123");
            env::set_var("GAME_INTERNAL_TOKEN", "prod-game-internal-token-123");
        }
        set_valid_production_infra();

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("TICKET_SECRET"));
    }

    #[test]
    fn keeps_development_default_tokens_compatible() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            clear_production_env();
            env::remove_var("TICKET_SECRET");
            env::remove_var("GAME_ADMIN_TOKEN");
            env::remove_var("GAME_INTERNAL_TOKEN");
        }

        let config = Config::from_env();

        assert_eq!(config.ticket_secret, DEFAULT_TICKET_SECRET);
        assert_eq!(config.admin_token, DEFAULT_ADMIN_TOKEN);
        assert_eq!(config.internal_token, DEFAULT_INTERNAL_TOKEN);
    }

    #[test]
    fn required_discovery_rejects_registry_disabled() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            clear_production_env();
            env::set_var("DISCOVERY_REQUIRED", "true");
            env::set_var("REGISTRY_ENABLED", "false");
        }

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true"));
    }

    #[test]
    fn test_environment_requires_registry_discovery() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            env::set_var("APP_ENV", "test");
            env::remove_var("NODE_ENV");
            env::remove_var("DISCOVERY_REQUIRED");
            env::set_var("REGISTRY_ENABLED", "false");
        }

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true"));
    }

    #[test]
    fn admin_audit_defaults_are_enabled_and_optional_actor() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            clear_production_env();
            env::remove_var("GAME_ADMIN_AUDIT_ENABLED");
            env::remove_var("GAME_ADMIN_AUDIT_PATH");
            env::remove_var("GAME_ADMIN_AUDIT_REQUIRE_ACTOR");
        }

        let config = Config::from_env();

        assert!(config.admin_audit_enabled);
        assert_eq!(
            config.admin_audit_path,
            "logs/game-server/admin-audit.jsonl"
        );
        assert!(!config.admin_audit_require_actor);
    }

    #[test]
    fn admin_audit_env_overrides_are_parsed() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            clear_production_env();
            env::set_var("GAME_ADMIN_AUDIT_ENABLED", "false");
            env::set_var("GAME_ADMIN_AUDIT_PATH", "tmp/admin-audit.jsonl");
            env::set_var("GAME_ADMIN_AUDIT_REQUIRE_ACTOR", "true");
        }

        let config = Config::from_env();

        assert!(!config.admin_audit_enabled);
        assert_eq!(config.admin_audit_path, "tmp/admin-audit.jsonl");
        assert!(config.admin_audit_require_actor);
    }

    #[test]
    fn rejects_disabled_admin_audit_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("GAME_ADMIN_AUDIT_ENABLED", "false");
        }
        set_custom_production_tokens();
        set_valid_production_infra();

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("GAME_ADMIN_AUDIT_ENABLED"));
    }

    #[test]
    fn rejects_disabled_database_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("DB_ENABLED", "false");
            env::set_var("GLOBAL_ID_ORIGIN_ID", "1");
            env::set_var("GLOBAL_ID_WORKER_ID", "1");
        }
        set_custom_production_tokens();

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("DB_ENABLED"));
    }

    #[test]
    fn rejects_invalid_global_id_config_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SECURITY_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("DB_ENABLED", "true");
            env::set_var("GLOBAL_ID_ORIGIN_ID", "0");
            env::set_var("GLOBAL_ID_WORKER_ID", "64");
        }
        set_custom_production_tokens();

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("GLOBAL_ID_ORIGIN_ID"));
        assert!(error.contains("GLOBAL_ID_WORKER_ID"));
    }
}
