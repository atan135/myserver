use std::env;

pub const DEFAULT_TICKET_SECRET: &str = "dev-only-change-this-ticket-secret";
pub const DEFAULT_ADMIN_TOKEN: &str = "dev-only-change-this-game-admin-token";
pub const DEFAULT_INTERNAL_TOKEN: &str = "dev-only-change-this-game-internal-token";

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
    pub internal_token: String,
    pub local_socket_name: String,
    pub internal_socket_name: String,
    pub log_level: String,
    pub log_enable_console: bool,
    pub log_enable_file: bool,
    pub log_dir: String,
    pub redis_url: String,
    pub redis_key_prefix: String,
    pub nats_url: String,
    pub mysql_enabled: bool,
    pub mysql_url: String,
    pub mysql_pool_size: usize,
    pub ticket_secret: String,
    pub heartbeat_timeout_secs: u64,
    pub max_body_len: usize,
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
    pub registry_url: String,
    pub registry_heartbeat_interval_secs: u64,
    pub service_name: String,
    pub service_instance_id: String,
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
        let nats_url = env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let mysql_enabled = parse_bool("MYSQL_ENABLED", false);
        let mysql_url = env::var("MYSQL_URL")
            .unwrap_or_else(|_| "mysql://root:password@127.0.0.1:3306/myserver_game".to_string());
        let mysql_pool_size = env::var("MYSQL_POOL_SIZE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(10);
        let ticket_secret =
            env::var("TICKET_SECRET").unwrap_or_else(|_| DEFAULT_TICKET_SECRET.to_string());
        let heartbeat_timeout_secs = env::var("HEARTBEAT_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30);
        let max_body_len = parse_usize("MAX_BODY_LEN", 4096);
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
        let registry_url = env::var("REGISTRY_URL")
            .or_else(|_| env::var("REDIS_URL"))
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let registry_heartbeat_interval_secs = parse_u64("REGISTRY_HEARTBEAT_INTERVAL", 10);
        let service_name = env::var("SERVICE_NAME").unwrap_or_else(|_| "game-server".to_string());
        let service_instance_id = env::var("SERVICE_INSTANCE_ID")
            .unwrap_or_else(|_| format!("{}-{}", service_name, port));

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
            internal_token,
            local_socket_name,
            internal_socket_name,
            log_level,
            log_enable_console,
            log_enable_file,
            log_dir,
            redis_url,
            redis_key_prefix,
            nats_url,
            mysql_enabled,
            mysql_url,
            mysql_pool_size,
            ticket_secret,
            heartbeat_timeout_secs,
            max_body_len,
            msg_rate_window_ms,
            msg_rate_max,
            player_msg_rate_window_ms,
            player_msg_rate_max,
            input_timestamp_required,
            input_timestamp_max_skew_ms,
            input_anomaly_window_ms,
            input_anomaly_max,
            registry_enabled,
            registry_url,
            registry_heartbeat_interval_secs,
            service_name,
            service_instance_id,
        };

        validate_production_config(&config);

        config
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn admin_bind_addr(&self) -> String {
        format!("{}:{}", self.admin_host, self.admin_port)
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

    if is_default_secret(&config.internal_token, DEFAULT_INTERNAL_TOKEN) {
        errors.push("GAME_INTERNAL_TOKEN must be set to a non-default value in production");
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
        "TICKET_SECRET",
        "GAME_ADMIN_TOKEN",
        "GAME_INTERNAL_TOKEN",
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
}
