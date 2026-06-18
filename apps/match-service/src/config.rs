//! 配置读取

use std::collections::HashMap;
use std::net::SocketAddr;

const DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME: &str = "DISALLOW_LEGACY_DIRECT_CONFIG";

#[derive(Clone)]
pub struct Config {
    pub bind_addr: String,
    pub public_host: String,
    pub port: u16,
    pub match_timeout_secs: u64,
    pub max_concurrent_matches: usize,
    pub modes: HashMap<String, ModeConfig>,
    pub match_cleanup_interval_secs: u64,
    pub game_server_service_name: String,
    pub game_server_internal_socket_name: String,
    pub local_discovery_fallback_enabled: bool,
    pub game_server_discovery_cache_ttl_secs: u64,
    pub game_server_target_zone: String,
    pub game_internal_token: String,
    pub log_level: String,
    pub log_enable_console: bool,
    pub log_enable_file: bool,
    pub log_dir: String,
    pub redis_url: String,
    pub redis_key_prefix: String,
    pub global_id_origin_id: u64,
    pub global_id_worker_id: Option<u64>,
    pub nats_url: String,
    pub registry_enabled: bool,
    pub discovery_required: bool,
    pub registry_url: String,
    pub registry_key_prefix: String,
    pub registry_heartbeat_interval_secs: u64,
    pub service_name: String,
    pub service_instance_id: String,
    pub service_zone: String,
    pub service_build_version: String,
    pub match_runtime_store: String,
    pub match_runtime_key_prefix: String,
    pub match_runtime_lease_ttl_secs: u64,
    pub match_recovery_enabled: bool,
    pub legacy_direct_config_warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ModeConfig {
    pub team_size: usize,
    pub total_size: usize,
    pub match_timeout_secs: u64,
}

impl Config {
    pub fn from_env() -> Self {
        let bind_addr = bind_addr_from_env("MATCH_BIND_ADDR", "0.0.0.0:9002");
        let port = parse_port(&bind_addr).unwrap_or(9002);
        let mut modes = HashMap::new();
        modes.insert(
            "1v1".to_string(),
            ModeConfig {
                team_size: 1,
                total_size: 2,
                match_timeout_secs: 30,
            },
        );
        modes.insert(
            "3v3".to_string(),
            ModeConfig {
                team_size: 3,
                total_size: 6,
                match_timeout_secs: 60,
            },
        );
        modes.insert(
            "5v5".to_string(),
            ModeConfig {
                team_size: 5,
                total_size: 10,
                match_timeout_secs: 90,
            },
        );
        let local_discovery_fallback_enabled = is_local_discovery_fallback_env();
        let discovery_required = discovery_required_from_env();
        validate_legacy_direct_config(
            "match-service",
            &[
                "GAME_SERVER_INTERNAL_SOCKET_NAME",
                "GAME_INTERNAL_SOCKET_NAME",
            ],
        );
        let legacy_direct_config_warnings = collect_legacy_direct_config_warnings(
            &[
                "GAME_SERVER_INTERNAL_SOCKET_NAME",
                "GAME_INTERNAL_SOCKET_NAME",
            ],
            discovery_required || !local_discovery_fallback_enabled,
        );
        let game_server_local_socket_name = if local_discovery_fallback_enabled {
            std::env::var("GAME_LOCAL_SOCKET_NAME")
                .unwrap_or_else(|_| "myserver-game-server.sock".to_string())
        } else {
            "myserver-game-server.sock".to_string()
        };

        let config = Self {
            bind_addr: bind_addr.clone(),
            public_host: advertised_host_from_env(
                &[
                    "SERVICE_ADVERTISED_HOST",
                    "SERVICE_PUBLIC_HOST",
                    "MATCH_PUBLIC_HOST",
                ],
                &bind_addr,
            ),
            port,
            match_timeout_secs: std::env::var("MATCH_TIMEOUT_SECS")
                .unwrap_or_else(|_| "30".to_string())
                .parse()
                .unwrap_or(30),
            max_concurrent_matches: std::env::var("MAX_CONCURRENT_MATCHES")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .unwrap_or(1000),
            modes,
            match_cleanup_interval_secs: std::env::var("MATCH_CLEANUP_INTERVAL_SECS")
                .unwrap_or_else(|_| "1".to_string())
                .parse()
                .unwrap_or(1),
            game_server_service_name: std::env::var("GAME_SERVER_SERVICE_NAME")
                .unwrap_or_else(|_| "game-server".to_string()),
            game_server_internal_socket_name: if local_discovery_fallback_enabled {
                std::env::var("GAME_SERVER_INTERNAL_SOCKET_NAME")
                    .or_else(|_| std::env::var("GAME_INTERNAL_SOCKET_NAME"))
                    .unwrap_or_else(|_| derive_internal_socket_name(&game_server_local_socket_name))
            } else {
                derive_internal_socket_name(&game_server_local_socket_name)
            },
            local_discovery_fallback_enabled,
            game_server_discovery_cache_ttl_secs: std::env::var(
                "GAME_SERVER_DISCOVERY_CACHE_TTL_SECS",
            )
            .unwrap_or_else(|_| "5".to_string())
            .parse()
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(5),
            game_server_target_zone: std::env::var("GAME_SERVER_TARGET_ZONE")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .unwrap_or_default(),
            game_internal_token: std::env::var("GAME_INTERNAL_TOKEN")
                .unwrap_or_else(|_| "dev-only-change-this-game-internal-token".to_string()),
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
            discovery_required,
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
                .unwrap_or_else(|_| "match-service".to_string()),
            service_instance_id: std::env::var("SERVICE_INSTANCE_ID")
                .unwrap_or_else(|_| format!("match-service-{}", port)),
            service_zone: std::env::var("SERVICE_ZONE").unwrap_or_else(|_| "local".to_string()),
            service_build_version: std::env::var("SERVICE_BUILD_VERSION")
                .unwrap_or_else(|_| "dev".to_string()),
            match_runtime_store: std::env::var("MATCH_RUNTIME_STORE")
                .unwrap_or_else(|_| "memory".to_string()),
            match_runtime_key_prefix: std::env::var("MATCH_RUNTIME_KEY_PREFIX")
                .unwrap_or_else(|_| "myserver:".to_string()),
            match_runtime_lease_ttl_secs: std::env::var("MATCH_RUNTIME_LEASE_TTL_SECS")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .unwrap_or(10),
            match_recovery_enabled: std::env::var("MATCH_RECOVERY_ENABLED")
                .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "True"))
                .unwrap_or(true),
            legacy_direct_config_warnings,
        };

        emit_legacy_direct_config_warnings("match-service", &config.legacy_direct_config_warnings);
        validate_production_config(&config);
        validate_discovery_config(&config);
        config
    }

    pub fn get_mode(&self, mode: &str) -> Option<&ModeConfig> {
        self.modes.get(mode)
    }

    pub fn log_level(&self) -> &str {
        &self.log_level
    }

    pub fn log_enable_console(&self) -> bool {
        self.log_enable_console
    }

    pub fn log_enable_file(&self) -> bool {
        self.log_enable_file
    }

    pub fn log_dir(&self) -> &str {
        &self.log_dir
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

fn parse_port(bind_addr: &str) -> Option<u16> {
    let addr: SocketAddr = bind_addr.parse().ok()?;
    Some(addr.port())
}

fn derive_internal_socket_name(local_socket_name: &str) -> String {
    if let Some(prefix) = local_socket_name.strip_suffix(".sock") {
        return format!("{prefix}-internal.sock");
    }

    format!("{local_socket_name}-internal")
}

fn parse_u64_env(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn parse_optional_u64_env(name: &str) -> Option<u64> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .and_then(|value| value.parse().ok())
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

fn is_local_discovery_fallback_env() -> bool {
    if is_strict_discovery_env() {
        return false;
    }

    let names = ["NODE_ENV", "APP_ENV"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    names.is_empty()
        || names
            .iter()
            .any(|value| matches!(value.as_str(), "development" | "local"))
}

fn parse_bool_env(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(default)
}

fn discovery_required_from_env() -> bool {
    parse_bool_env("DISCOVERY_REQUIRED", false) || is_strict_discovery_env()
}

fn collect_legacy_direct_config_warnings(names: &[&str], strict_discovery: bool) -> Vec<String> {
    if !strict_discovery {
        return Vec::new();
    }

    collect_configured_legacy_direct_config_names(names)
        .into_iter()
        .map(|name| {
            format!(
                "{name} is ignored while strict service discovery is active; use service registry endpoints instead"
            )
        })
        .collect()
}

fn collect_configured_legacy_direct_config_names(names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter(|name| std::env::var_os(name).is_some())
        .map(|name| (*name).to_string())
        .collect()
}

fn validate_legacy_direct_config(app_name: &str, names: &[&str]) {
    if !parse_bool_env(DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME, false) {
        return;
    }

    let configured = collect_configured_legacy_direct_config_names(names);
    if !configured.is_empty() {
        panic!(
            "invalid {app_name} discovery config: {DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME}=true forbids legacy direct config: {}; remove these variables and use service registry endpoints instead",
            configured.join(", ")
        );
    }
}

fn emit_legacy_direct_config_warnings(app_name: &str, warnings: &[String]) {
    for warning in warnings {
        tracing::warn!(service = app_name, warning = %warning, "legacy direct discovery config ignored");
    }
}

fn validate_discovery_config(config: &Config) {
    if config.discovery_required && !config.registry_enabled {
        panic!(
            "invalid match-service discovery config: DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true"
        );
    }
}

fn validate_production_config(config: &Config) {
    if !is_production_env() {
        return;
    }

    if config.global_id_origin_id == 0 || config.global_id_origin_id > 1023 {
        panic!(
            "invalid match-service production config: GLOBAL_ID_ORIGIN_ID must be set to 1-1023 in production"
        );
    }

    if config
        .global_id_worker_id
        .is_some_and(|worker_id| worker_id > 63)
    {
        panic!(
            "invalid match-service production config: GLOBAL_ID_WORKER_ID must be set to 0-63 in production"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::panic::{self, AssertUnwindSafe};
    use std::sync::{Mutex, OnceLock};

    use super::*;

    const GLOBAL_ID_ENV_NAMES: &[&str] = &[
        "NODE_ENV",
        "APP_ENV",
        "DISCOVERY_REQUIRED",
        "DISALLOW_LEGACY_DIRECT_CONFIG",
        "REGISTRY_ENABLED",
        "REGISTRY_KEY_PREFIX",
        "REDIS_KEY_PREFIX",
        "GLOBAL_ID_ORIGIN_ID",
        "GLOBAL_ID_WORKER_ID",
    ];
    const SERVICE_BUILD_VERSION_ENV_NAMES: &[&str] = &[
        "NODE_ENV",
        "APP_ENV",
        "DISCOVERY_REQUIRED",
        "DISALLOW_LEGACY_DIRECT_CONFIG",
        "REGISTRY_ENABLED",
        "REGISTRY_KEY_PREFIX",
        "REDIS_KEY_PREFIX",
        "SERVICE_BIND_HOST",
        "SERVICE_PUBLIC_HOST",
        "SERVICE_ADVERTISED_HOST",
        "MATCH_BIND_ADDR",
        "MATCH_PUBLIC_HOST",
        "GAME_LOCAL_SOCKET_NAME",
        "GAME_SERVER_INTERNAL_SOCKET_NAME",
        "GAME_INTERNAL_SOCKET_NAME",
        "SERVICE_NAME",
        "SERVICE_INSTANCE_ID",
        "SERVICE_ZONE",
        "SERVICE_BUILD_VERSION",
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

    fn catch_config_from_env() -> Result<Config, Box<dyn std::any::Any + Send>> {
        panic::catch_unwind(AssertUnwindSafe(Config::from_env))
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

    #[test]
    fn global_id_config_defaults_to_local_origin_without_explicit_worker() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(GLOBAL_ID_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::remove_var("APP_ENV");
            env::remove_var("GLOBAL_ID_ORIGIN_ID");
            env::remove_var("GLOBAL_ID_WORKER_ID");
        }

        let config = Config::from_env();

        assert_eq!(config.global_id_origin_id, 0);
        assert_eq!(config.global_id_worker_id, None);
    }

    #[test]
    fn global_id_config_accepts_explicit_worker() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(GLOBAL_ID_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::remove_var("APP_ENV");
            env::set_var("GLOBAL_ID_ORIGIN_ID", "7");
            env::set_var("GLOBAL_ID_WORKER_ID", "6");
        }

        let config = Config::from_env();

        assert_eq!(config.global_id_origin_id, 7);
        assert_eq!(config.global_id_worker_id, Some(6));
    }

    #[test]
    fn service_build_version_defaults_to_dev() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SERVICE_BUILD_VERSION_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::remove_var("APP_ENV");
            env::remove_var("SERVICE_NAME");
            env::remove_var("SERVICE_INSTANCE_ID");
            env::remove_var("SERVICE_ZONE");
            env::remove_var("SERVICE_BUILD_VERSION");
        }

        let config = Config::from_env();

        assert_eq!(config.service_name, "match-service");
        assert_eq!(config.service_instance_id, "match-service-9002");
        assert_eq!(config.service_zone, "local");
        assert_eq!(config.service_build_version, "dev");
    }

    #[test]
    fn service_identity_reads_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SERVICE_BUILD_VERSION_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::remove_var("APP_ENV");
            env::set_var("SERVICE_NAME", "match-service-blue");
            env::set_var("SERVICE_INSTANCE_ID", "match-blue-001");
            env::set_var("SERVICE_ZONE", "zone-a");
            env::set_var("SERVICE_BUILD_VERSION", "2026.06.18");
        }

        let config = Config::from_env();

        assert_eq!(config.service_name, "match-service-blue");
        assert_eq!(config.service_instance_id, "match-blue-001");
        assert_eq!(config.service_zone, "zone-a");
        assert_eq!(config.service_build_version, "2026.06.18");
    }

    #[test]
    fn endpoint_publish_hosts_prefer_unified_env_and_never_advertise_wildcard_bind() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SERVICE_BUILD_VERSION_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::remove_var("APP_ENV");
            env::set_var("SERVICE_BIND_HOST", "0.0.0.0");
            env::set_var("SERVICE_PUBLIC_HOST", "10.0.0.60");
            env::set_var("MATCH_BIND_ADDR", "127.0.0.9:9002");
            env::set_var("MATCH_PUBLIC_HOST", "10.0.0.99");
        }

        let config = Config::from_env();

        assert_eq!(config.bind_addr, "0.0.0.0:9002");
        assert_eq!(config.public_host, "10.0.0.60");

        unsafe {
            env::remove_var("SERVICE_PUBLIC_HOST");
            env::remove_var("SERVICE_ADVERTISED_HOST");
            env::remove_var("MATCH_PUBLIC_HOST");
        }

        let config = Config::from_env();

        assert_eq!(config.public_host, "127.0.0.1");
    }

    #[test]
    fn ignores_game_server_internal_socket_env_outside_local_fallback() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SERVICE_BUILD_VERSION_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::remove_var("DISALLOW_LEGACY_DIRECT_CONFIG");
            env::set_var("APP_ENV", "test");
            env::set_var("REGISTRY_ENABLED", "true");
            env::set_var("GAME_LOCAL_SOCKET_NAME", "custom-local.sock");
            env::set_var("GAME_SERVER_INTERNAL_SOCKET_NAME", "custom-internal.sock");
            env::set_var("GAME_INTERNAL_SOCKET_NAME", "legacy-internal.sock");
        }

        let config = Config::from_env();

        assert!(!config.local_discovery_fallback_enabled);
        assert_eq!(
            config.game_server_internal_socket_name,
            "myserver-game-server-internal.sock"
        );
        assert_eq!(config.legacy_direct_config_warnings.len(), 2);
        assert!(
            config.legacy_direct_config_warnings[0]
                .contains("GAME_SERVER_INTERNAL_SOCKET_NAME is ignored")
        );
        assert!(
            config.legacy_direct_config_warnings[1]
                .contains("GAME_INTERNAL_SOCKET_NAME is ignored")
        );
    }

    #[test]
    fn local_fallback_legacy_socket_envs_do_not_warn() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SERVICE_BUILD_VERSION_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::remove_var("DISALLOW_LEGACY_DIRECT_CONFIG");
            env::set_var("APP_ENV", "development");
            env::set_var("GAME_SERVER_INTERNAL_SOCKET_NAME", "custom-internal.sock");
            env::set_var("GAME_INTERNAL_SOCKET_NAME", "legacy-internal.sock");
        }

        let config = Config::from_env();

        assert!(config.local_discovery_fallback_enabled);
        assert!(config.legacy_direct_config_warnings.is_empty());
    }

    #[test]
    fn env_example_does_not_enable_legacy_internal_socket_fallback_by_default() {
        let env_example = include_str!("../.env.example");
        let active_names = env_example
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_once('=').map(|(name, _)| name.trim()))
            .collect::<Vec<_>>();

        assert!(!active_names.contains(&"GAME_SERVER_INTERNAL_SOCKET_NAME"));
        assert!(!active_names.contains(&"GAME_INTERNAL_SOCKET_NAME"));
        assert!(
            env_example.contains("# GAME_SERVER_INTERNAL_SOCKET_NAME="),
            ".env.example should keep GAME_SERVER_INTERNAL_SOCKET_NAME only as a commented local fallback example"
        );
        assert!(
            env_example.contains("# GAME_INTERNAL_SOCKET_NAME="),
            ".env.example should document GAME_INTERNAL_SOCKET_NAME only as a commented legacy local fallback alias"
        );
    }

    #[test]
    fn rejects_legacy_internal_socket_env_when_migration_complete_switch_is_enabled() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SERVICE_BUILD_VERSION_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::set_var("APP_ENV", "development");
            env::set_var("DISALLOW_LEGACY_DIRECT_CONFIG", "true");
            env::set_var("GAME_SERVER_INTERNAL_SOCKET_NAME", "custom-internal.sock");
            env::set_var("GAME_INTERNAL_SOCKET_NAME", "legacy-internal.sock");
        }

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("DISALLOW_LEGACY_DIRECT_CONFIG=true forbids legacy direct config"));
        assert!(error.contains("GAME_SERVER_INTERNAL_SOCKET_NAME"));
        assert!(error.contains("GAME_INTERNAL_SOCKET_NAME"));
    }

    #[test]
    fn test_environment_rejects_legacy_internal_socket_env_when_migration_complete_switch_is_enabled(
    ) {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SERVICE_BUILD_VERSION_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::set_var("APP_ENV", "test");
            env::set_var("REGISTRY_ENABLED", "true");
            env::set_var("DISCOVERY_REQUIRED", "true");
            env::set_var("DISALLOW_LEGACY_DIRECT_CONFIG", "true");
            env::set_var("GAME_SERVER_INTERNAL_SOCKET_NAME", "custom-internal.sock");
            env::set_var("GAME_INTERNAL_SOCKET_NAME", "legacy-internal.sock");
        }

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("DISALLOW_LEGACY_DIRECT_CONFIG=true forbids legacy direct config"));
        assert!(error.contains("GAME_SERVER_INTERNAL_SOCKET_NAME"));
        assert!(error.contains("GAME_INTERNAL_SOCKET_NAME"));
    }

    #[test]
    fn accepts_migration_complete_switch_without_legacy_internal_socket_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SERVICE_BUILD_VERSION_ENV_NAMES);

        unsafe {
            env::remove_var("NODE_ENV");
            env::set_var("APP_ENV", "development");
            env::set_var("DISALLOW_LEGACY_DIRECT_CONFIG", "true");
            env::remove_var("GAME_SERVER_INTERNAL_SOCKET_NAME");
            env::remove_var("GAME_INTERNAL_SOCKET_NAME");
        }

        let config = Config::from_env();

        assert!(config.legacy_direct_config_warnings.is_empty());
    }

    #[test]
    fn registry_key_prefix_prefers_registry_specific_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(SERVICE_BUILD_VERSION_ENV_NAMES);

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
    fn rejects_zero_origin_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(GLOBAL_ID_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("REGISTRY_ENABLED", "true");
            env::set_var("GLOBAL_ID_ORIGIN_ID", "0");
            env::set_var("GLOBAL_ID_WORKER_ID", "6");
        }

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("GLOBAL_ID_ORIGIN_ID"));
    }

    #[test]
    fn rejects_invalid_worker_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(GLOBAL_ID_ENV_NAMES);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("REGISTRY_ENABLED", "true");
            env::set_var("GLOBAL_ID_ORIGIN_ID", "1");
            env::set_var("GLOBAL_ID_WORKER_ID", "64");
        }

        let error = panic_message(catch_config_from_env());

        assert!(error.contains("GLOBAL_ID_WORKER_ID"));
    }

    #[test]
    fn required_discovery_rejects_registry_disabled() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(GLOBAL_ID_ENV_NAMES);

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
        let _env = EnvGuard::capture(GLOBAL_ID_ENV_NAMES);

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
