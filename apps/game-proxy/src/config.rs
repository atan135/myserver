use std::collections::HashSet;
use std::env;

use crate::connection_limits::{ConnectionLimitConfig, IpDenyList};
use crate::rollout_drain_status::{
    DEFAULT_ROLLOUT_DRAIN_STATUS_MAX_BODY_BYTES, DEFAULT_ROLLOUT_DRAIN_STATUS_TIMEOUT_MS,
    DEFAULT_ROLLOUT_DRAIN_STATUS_URL, RolloutDrainStatusCheckConfig,
};

pub const DEFAULT_ADMIN_TOKEN: &str = "dev-only-change-this-proxy-admin-token";
pub const DEFAULT_ADMIN_READ_TOKEN: &str = "dev-only-change-this-proxy-admin-read-token";
const DEFAULT_MAINTENANCE_CACHE_TTL_MS: u64 = 2000;
const DEFAULT_BLOCKLIST_CACHE_TTL_MS: u64 = 2000;
const DEFAULT_PROXY_MSG_RATE_WINDOW_MS: u64 = 1000;
const DEFAULT_PROXY_ADMIN_AUDIT_PATH: &str = "logs/game-proxy/admin-audit.jsonl";
const DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME: &str = "DISALLOW_LEGACY_DIRECT_CONFIG";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AdminPermissionScope {
    Read,
    MaintenanceWrite,
    RolloutWrite,
    RouteWrite,
    Write,
    All,
}

impl AdminPermissionScope {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "proxy.read" => Some(Self::Read),
            "proxy.maintenance.write" => Some(Self::MaintenanceWrite),
            "proxy.rollout.write" => Some(Self::RolloutWrite),
            "proxy.route.write" => Some(Self::RouteWrite),
            "proxy.write" => Some(Self::Write),
            "*" => Some(Self::All),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminScopedTokenConfig {
    pub token: String,
    pub permissions: Vec<AdminPermissionScope>,
}

fn parse_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(default)
}

fn parse_u32(name: &str, default: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

fn parse_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn parse_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
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

fn advertised_host_from_env(names: &[&str], fallback_host: &str) -> String {
    let host = parse_first_non_empty_string(names, fallback_host);
    if matches!(host.trim(), "0.0.0.0" | "::" | "[::]") {
        "127.0.0.1".to_string()
    } else {
        host
    }
}

#[derive(Clone)]
pub enum RouteStoreBackend {
    Memory,
    Redis,
}

impl RouteStoreBackend {
    fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "memory" => Ok(Self::Memory),
            "redis" => Ok(Self::Redis),
            _ => Err("PROXY_ROUTE_STORE_BACKEND must be memory or redis".to_string()),
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub public_host: String,
    pub port: u16,
    pub admin_host: String,
    pub admin_advertised_host: String,
    pub admin_port: u16,
    pub admin_token: String,
    pub admin_read_token: Option<String>,
    pub admin_scoped_tokens: Vec<AdminScopedTokenConfig>,
    pub admin_audit_enabled: bool,
    pub admin_audit_path: String,
    pub admin_audit_require_actor: bool,
    pub tcp_fallback_host: String,
    pub tcp_fallback_advertised_host: String,
    pub tcp_fallback_port: u16,
    pub log_level: String,
    pub log_enable_console: bool,
    pub log_enable_file: bool,
    pub log_dir: String,
    pub local_socket_name: String,
    pub redis_url: String,
    pub redis_key_prefix: String,
    pub route_store_backend: RouteStoreBackend,
    pub route_store_redis_url: String,
    pub route_store_key_prefix: String,
    pub nats_url: String,
    pub ticket_secret: String,
    pub proxy_max_connections: u64,
    pub proxy_max_preauth_failures: u32,
    pub proxy_msg_rate_window_ms: u64,
    pub proxy_msg_rate_max: u64,
    pub maintenance_cache_ttl_ms: u64,
    pub redis_blocklist_enabled: bool,
    pub redis_blocklist_cache_ttl_ms: u64,
    pub connection_limits: ConnectionLimitConfig,
    pub rollout_drain_status_check: RolloutDrainStatusCheckConfig,
    // Service Registry
    pub registry_enabled: bool,
    pub registry_url: String,
    pub registry_key_prefix: String,
    pub registry_discover_interval_secs: u64,
    pub upstream_service_name: String,
    pub service_name: String,
    pub service_instance_id: String,
    pub service_build_version: String,
    pub service_zone: String,
    pub local_discovery_fallback_enabled: bool,
    // 保留旧配置用于向后兼容（当 registry 禁用时）
    pub upstream_server_id: String,
    pub upstream_local_socket_name: String,
    pub legacy_direct_config_warnings: Vec<String>,
}

impl Config {
    pub fn from_env() -> Self {
        Self::try_from_env().expect("invalid game-proxy configuration")
    }

    pub fn try_from_env() -> Result<Self, String> {
        let host = parse_first_non_empty_string(&["SERVICE_BIND_HOST", "PROXY_HOST"], "127.0.0.1");
        let public_host = advertised_host_from_env(
            &[
                "SERVICE_ADVERTISED_HOST",
                "SERVICE_PUBLIC_HOST",
                "PROXY_PUBLIC_HOST",
                "PROXY_HOST",
            ],
            &host,
        );
        let port = env::var("PROXY_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(4000);
        let admin_host =
            parse_first_non_empty_string(&["SERVICE_ADMIN_BIND_HOST", "PROXY_ADMIN_HOST"], &host);
        let admin_advertised_host = advertised_host_from_env(
            &[
                "SERVICE_ADMIN_ADVERTISED_HOST",
                "SERVICE_ADVERTISED_HOST",
                "SERVICE_PUBLIC_HOST",
                "PROXY_ADMIN_PUBLIC_HOST",
                "PROXY_ADMIN_HOST",
            ],
            &admin_host,
        );
        let admin_port = env::var("PROXY_ADMIN_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(7101);
        let admin_token = env::var("PROXY_ADMIN_TOKEN")
            .unwrap_or_else(|_| DEFAULT_ADMIN_TOKEN.to_string())
            .trim()
            .to_string();
        let admin_read_token = env::var("PROXY_ADMIN_READ_TOKEN")
            .ok()
            .map(|value| value.trim().to_string());
        validate_admin_tokens(&admin_token, admin_read_token.as_deref())?;
        let admin_read_token = admin_read_token.filter(|token| !token.is_empty());
        let admin_scoped_tokens = parse_admin_scoped_tokens(
            env::var("PROXY_ADMIN_SCOPED_TOKENS").ok().as_deref(),
            &admin_token,
            admin_read_token.as_deref(),
        )?;
        let admin_audit_enabled = parse_bool("PROXY_ADMIN_AUDIT_ENABLED", true);
        let admin_audit_path = env::var("PROXY_ADMIN_AUDIT_PATH")
            .map(|value| value.trim().to_string())
            .ok()
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_PROXY_ADMIN_AUDIT_PATH.to_string());
        let admin_audit_require_actor = parse_bool("PROXY_ADMIN_AUDIT_REQUIRE_ACTOR", false);
        let tcp_fallback_host =
            env::var("PROXY_TCP_FALLBACK_HOST").unwrap_or_else(|_| host.clone());
        let tcp_fallback_advertised_host = advertised_host_from_env(
            &[
                "SERVICE_TCP_FALLBACK_ADVERTISED_HOST",
                "SERVICE_ADVERTISED_HOST",
                "SERVICE_PUBLIC_HOST",
                "PROXY_TCP_FALLBACK_PUBLIC_HOST",
                "PROXY_TCP_FALLBACK_HOST",
            ],
            &tcp_fallback_host,
        );
        let tcp_fallback_port = env::var("PROXY_TCP_FALLBACK_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(port + 10000);
        let log_level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
        let log_enable_console = parse_bool("LOG_ENABLE_CONSOLE", true);
        let log_enable_file = parse_bool("LOG_ENABLE_FILE", true);
        let log_dir = env::var("LOG_DIR").unwrap_or_else(|_| "logs/game-proxy".to_string());
        let local_socket_name = env::var("PROXY_LOCAL_SOCKET_NAME")
            .unwrap_or_else(|_| "myserver-game-proxy.sock".to_string());
        let redis_url = env::var("REDIS_URL")
            .or_else(|_| env::var("REGISTRY_URL"))
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let redis_key_prefix = env::var("REDIS_KEY_PREFIX").unwrap_or_default();
        let route_store_backend = RouteStoreBackend::parse(
            &env::var("PROXY_ROUTE_STORE_BACKEND").unwrap_or_else(|_| "memory".to_string()),
        )?;
        let route_store_redis_url = env::var("PROXY_ROUTE_STORE_REDIS_URL")
            .or_else(|_| env::var("REGISTRY_URL"))
            .or_else(|_| env::var("REDIS_URL"))
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let route_store_key_prefix = env::var("PROXY_ROUTE_STORE_KEY_PREFIX")
            .or_else(|_| env::var("REDIS_KEY_PREFIX"))
            .unwrap_or_default();
        let nats_url = env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let ticket_secret = env::var("TICKET_SECRET")
            .unwrap_or_else(|_| "dev-only-change-this-ticket-secret".to_string());
        let proxy_max_connections = parse_u64("PROXY_MAX_CONNECTIONS", 0);
        let proxy_max_preauth_failures = parse_u32("PROXY_MAX_PREAUTH_FAILURES", 3);
        let proxy_msg_rate_window_ms =
            parse_u64("PROXY_MSG_RATE_WINDOW_MS", DEFAULT_PROXY_MSG_RATE_WINDOW_MS);
        let proxy_msg_rate_max = parse_u64("PROXY_MSG_RATE_MAX", 0);
        let maintenance_cache_ttl_ms = parse_u64(
            "PROXY_MAINTENANCE_CACHE_TTL_MS",
            DEFAULT_MAINTENANCE_CACHE_TTL_MS,
        );
        let redis_blocklist_enabled = parse_bool("PROXY_REDIS_BLOCKLIST_ENABLED", false);
        let redis_blocklist_cache_ttl_ms = parse_u64(
            "PROXY_REDIS_BLOCKLIST_CACHE_TTL_MS",
            DEFAULT_BLOCKLIST_CACHE_TTL_MS,
        );
        let connection_limits = ConnectionLimitConfig {
            ip_denylist: IpDenyList::parse_csv(&env::var("PROXY_IP_DENYLIST").unwrap_or_default())?,
            max_connections_per_ip: parse_u64("PROXY_MAX_CONNECTIONS_PER_IP", 0),
            max_connections_per_player: parse_u64("PROXY_MAX_CONNECTIONS_PER_PLAYER", 0),
        };
        let rollout_drain_status_check = RolloutDrainStatusCheckConfig {
            enabled: parse_bool("PROXY_ROLLOUT_DRAIN_STATUS_CHECK_ENABLED", false),
            url: env::var("PROXY_ROLLOUT_DRAIN_STATUS_URL")
                .map(|value| value.trim().to_string())
                .ok()
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_ROLLOUT_DRAIN_STATUS_URL.to_string()),
            token: env::var("PROXY_ROLLOUT_DRAIN_STATUS_TOKEN")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            connect_timeout_ms: parse_u64(
                "PROXY_ROLLOUT_DRAIN_STATUS_CONNECT_TIMEOUT_MS",
                DEFAULT_ROLLOUT_DRAIN_STATUS_TIMEOUT_MS,
            ),
            read_timeout_ms: parse_u64(
                "PROXY_ROLLOUT_DRAIN_STATUS_READ_TIMEOUT_MS",
                DEFAULT_ROLLOUT_DRAIN_STATUS_TIMEOUT_MS,
            ),
            overall_timeout_ms: parse_u64(
                "PROXY_ROLLOUT_DRAIN_STATUS_OVERALL_TIMEOUT_MS",
                DEFAULT_ROLLOUT_DRAIN_STATUS_TIMEOUT_MS,
            ),
            max_body_bytes: parse_usize(
                "PROXY_ROLLOUT_DRAIN_STATUS_MAX_BODY_BYTES",
                DEFAULT_ROLLOUT_DRAIN_STATUS_MAX_BODY_BYTES,
            ),
        };

        // Service Registry
        let registry_enabled = parse_bool("REGISTRY_ENABLED", false);
        let discovery_required = discovery_required_from_env();
        let local_discovery_fallback_enabled = is_local_discovery_fallback_env();
        validate_legacy_direct_config(
            &["UPSTREAM_SERVER_ID", "UPSTREAM_LOCAL_SOCKET_NAME"],
            discovery_required,
        )?;
        let legacy_direct_config_warnings = collect_legacy_direct_config_warnings(
            &["UPSTREAM_SERVER_ID", "UPSTREAM_LOCAL_SOCKET_NAME"],
            discovery_required || !local_discovery_fallback_enabled,
        );
        let registry_url = env::var("REGISTRY_URL")
            .or_else(|_| env::var("REDIS_URL"))
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let registry_key_prefix = env::var("REGISTRY_KEY_PREFIX")
            .or_else(|_| env::var("REDIS_KEY_PREFIX"))
            .unwrap_or_default();
        let registry_discover_interval_secs = env::var("REGISTRY_DISCOVER_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(5);
        let upstream_service_name =
            env::var("UPSTREAM_SERVICE_NAME").unwrap_or_else(|_| "game-server".to_string());
        let service_name = parse_non_empty_string("SERVICE_NAME", "game-proxy");
        let service_instance_id = env::var("SERVICE_INSTANCE_ID")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("{}-{}", service_name, port));
        let service_build_version = parse_non_empty_string("SERVICE_BUILD_VERSION", "dev");
        let service_zone = parse_non_empty_string("SERVICE_ZONE", "local");

        // 向后兼容的旧配置
        let upstream_server_id = if local_discovery_fallback_enabled {
            env::var("UPSTREAM_SERVER_ID").unwrap_or_else(|_| "game-server-1".to_string())
        } else {
            "game-server-1".to_string()
        };
        let upstream_local_socket_name = if local_discovery_fallback_enabled {
            env::var("UPSTREAM_LOCAL_SOCKET_NAME")
                .unwrap_or_else(|_| "myserver-game-server.sock".to_string())
        } else {
            "myserver-game-server.sock".to_string()
        };

        let config = Self {
            host,
            public_host,
            port,
            admin_host,
            admin_advertised_host,
            admin_port,
            admin_token,
            admin_read_token,
            admin_scoped_tokens,
            admin_audit_enabled,
            admin_audit_path,
            admin_audit_require_actor,
            tcp_fallback_host,
            tcp_fallback_advertised_host,
            tcp_fallback_port,
            log_level,
            log_enable_console,
            log_enable_file,
            log_dir,
            local_socket_name,
            redis_url,
            redis_key_prefix,
            route_store_backend,
            route_store_redis_url,
            route_store_key_prefix,
            nats_url,
            ticket_secret,
            proxy_max_connections,
            proxy_max_preauth_failures,
            proxy_msg_rate_window_ms,
            proxy_msg_rate_max,
            maintenance_cache_ttl_ms,
            redis_blocklist_enabled,
            redis_blocklist_cache_ttl_ms,
            connection_limits,
            rollout_drain_status_check,
            registry_enabled,
            registry_url,
            registry_key_prefix,
            registry_discover_interval_secs,
            upstream_service_name,
            service_name,
            service_instance_id,
            service_build_version,
            service_zone,
            local_discovery_fallback_enabled,
            upstream_server_id,
            upstream_local_socket_name,
            legacy_direct_config_warnings,
        };

        emit_legacy_direct_config_warnings("game-proxy", &config.legacy_direct_config_warnings);
        config.validate_upstream_discovery()?;
        Ok(config)
    }

    pub fn discovery_required(&self) -> bool {
        discovery_required_from_env()
    }

    pub fn static_upstream_fallback_allowed(&self) -> bool {
        !self.discovery_required() && self.local_discovery_fallback_enabled
    }

    pub fn validate_upstream_discovery(&self) -> Result<(), String> {
        if self.discovery_required() && !self.registry_enabled {
            return Err(
                "REGISTRY_ENABLED=true is required when DISCOVERY_REQUIRED=true or NODE_ENV/APP_ENV is production/staging/test"
                    .to_string(),
            );
        }
        if !self.registry_enabled && !self.static_upstream_fallback_allowed() {
            return Err(
                "REGISTRY_ENABLED=true is required when static upstream local fallback is unavailable; set NODE_ENV=development or APP_ENV=local to use local fallback"
                    .to_string(),
            );
        }
        Ok(())
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn admin_bind_addr(&self) -> String {
        format!("{}:{}", self.admin_host, self.admin_port)
    }

    pub fn tcp_fallback_addr(&self) -> String {
        format!("{}:{}", self.tcp_fallback_host, self.tcp_fallback_port)
    }

    pub fn route_store_backend_name(&self) -> &'static str {
        match &self.route_store_backend {
            RouteStoreBackend::Memory => "memory",
            RouteStoreBackend::Redis => "redis",
        }
    }

    pub fn service_instance_metadata(&self) -> serde_json::Value {
        serde_json::json!({
            "service_name": self.service_name.clone(),
            "service_instance_id": self.service_instance_id.clone(),
            "instance_id": self.service_instance_id.clone(),
            "route_store_backend": self.route_store_backend_name(),
            "build_version": self.service_build_version.clone(),
            "zone": self.service_zone.clone()
        })
    }
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
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "production" | "prod" | "staging" | "stage" | "test" | "testing"
            )
        })
    })
}

pub fn discovery_required_from_env() -> bool {
    env_flag("DISCOVERY_REQUIRED") || is_strict_discovery_env()
}

fn is_local_discovery_fallback_env() -> bool {
    if discovery_required_from_env() {
        return false;
    }

    let node_env = env::var("NODE_ENV")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let app_env = env::var("APP_ENV")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();

    node_env == "development" || app_env == "local"
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(false)
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
        .filter(|name| env::var_os(name).is_some())
        .map(|name| (*name).to_string())
        .collect()
}

fn validate_legacy_direct_config(names: &[&str], strict_discovery: bool) -> Result<(), String> {
    let disallow_legacy_direct_config = env_flag(DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME);
    if !disallow_legacy_direct_config && !strict_discovery {
        return Ok(());
    }

    let configured = collect_configured_legacy_direct_config_names(names);
    if configured.is_empty() {
        return Ok(());
    }

    if disallow_legacy_direct_config {
        return Err(format!(
            "{DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME}=true forbids legacy direct config: {}; remove these variables and use service registry endpoints instead",
            configured.join(", ")
        ));
    }

    Err(format!(
        "strict service discovery forbids legacy direct config: {}; remove these variables and use service registry endpoints instead",
        configured.join(", ")
    ))
}

fn emit_legacy_direct_config_warnings(app_name: &str, warnings: &[String]) {
    for warning in warnings {
        tracing::warn!(service = app_name, warning = %warning, "legacy direct discovery config ignored");
    }
}

fn validate_admin_tokens(admin_token: &str, admin_read_token: Option<&str>) -> Result<(), String> {
    if !is_production_env() {
        return Ok(());
    }

    let trimmed = admin_token.trim();
    if trimmed.is_empty() || is_default_admin_token(trimmed) {
        return Err(
            "PROXY_ADMIN_TOKEN must be set to a non-default value in production".to_string(),
        );
    }

    if let Some(read_token) = admin_read_token {
        let read_token = read_token.trim();
        if read_token.is_empty() || is_default_admin_token(read_token) {
            return Err(
                "PROXY_ADMIN_READ_TOKEN must be set to a non-default value in production"
                    .to_string(),
            );
        }
        if read_token == trimmed {
            return Err(
                "PROXY_ADMIN_READ_TOKEN must be different from PROXY_ADMIN_TOKEN in production"
                    .to_string(),
            );
        }
    }

    Ok(())
}

fn parse_admin_scoped_tokens(
    raw: Option<&str>,
    admin_token: &str,
    admin_read_token: Option<&str>,
) -> Result<Vec<AdminScopedTokenConfig>, String> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut seen_tokens = HashSet::new();
    let admin_token = admin_token.trim();
    if !admin_token.is_empty() {
        seen_tokens.insert(admin_token.to_string());
    }
    if let Some(read_token) = admin_read_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        seen_tokens.insert(read_token.to_string());
    }

    let mut scoped_tokens = Vec::new();
    for entry in raw.split(';') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }

        let Some((token, permissions_raw)) = entry.split_once(':') else {
            return Err(
                "PROXY_ADMIN_SCOPED_TOKENS entries must use token:permission1,permission2"
                    .to_string(),
            );
        };
        let token = token.trim();
        if token.is_empty() {
            return Err("PROXY_ADMIN_SCOPED_TOKENS contains empty token".to_string());
        }
        if is_default_admin_token(token) {
            return Err("PROXY_ADMIN_SCOPED_TOKENS contains default token".to_string());
        }
        if is_production_env() && is_weak_admin_token(token) {
            return Err("PROXY_ADMIN_SCOPED_TOKENS contains weak token in production".to_string());
        }
        if !seen_tokens.insert(token.to_string()) {
            return Err("PROXY_ADMIN_SCOPED_TOKENS contains duplicate token".to_string());
        }

        let mut permissions = Vec::new();
        let mut seen_permissions = HashSet::new();
        for permission in permissions_raw.split(',') {
            let permission = permission.trim();
            if permission.is_empty() {
                return Err("PROXY_ADMIN_SCOPED_TOKENS contains empty permission".to_string());
            }
            let Some(scope) = AdminPermissionScope::parse(permission) else {
                return Err(format!(
                    "PROXY_ADMIN_SCOPED_TOKENS contains unknown permission '{}'",
                    permission
                ));
            };
            if seen_permissions.insert(scope) {
                permissions.push(scope);
            }
        }
        if permissions.is_empty() {
            return Err("PROXY_ADMIN_SCOPED_TOKENS token has no permissions".to_string());
        }

        scoped_tokens.push(AdminScopedTokenConfig {
            token: token.to_string(),
            permissions,
        });
    }

    Ok(scoped_tokens)
}

fn is_default_admin_token(admin_token: &str) -> bool {
    let normalized = admin_token.trim().to_ascii_lowercase();
    admin_token == DEFAULT_ADMIN_TOKEN
        || admin_token == DEFAULT_ADMIN_READ_TOKEN
        || matches!(
            normalized.as_str(),
            "change-me" | "changeme" | "default" | "password"
        )
}

fn is_weak_admin_token(admin_token: &str) -> bool {
    let token = admin_token.trim();
    token.len() < 16
        || matches!(
            token.to_ascii_lowercase().as_str(),
            "admin" | "root" | "test" | "token" | "secret"
        )
        || token
            .chars()
            .next()
            .is_some_and(|first| token.chars().all(|ch| ch == first))
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::sync::{Mutex, OnceLock};

    use super::{
        AdminPermissionScope, Config, DEFAULT_ADMIN_READ_TOKEN, DEFAULT_ADMIN_TOKEN,
        RouteStoreBackend,
    };

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn capture(names: &[&'static str]) -> Self {
            let mut names = names.to_vec();
            if !names.contains(&"PROXY_ADMIN_SCOPED_TOKENS") {
                names.push("PROXY_ADMIN_SCOPED_TOKENS");
            }
            for name in [
                "PROXY_ROLLOUT_DRAIN_STATUS_CHECK_ENABLED",
                "PROXY_ROLLOUT_DRAIN_STATUS_URL",
                "PROXY_ROLLOUT_DRAIN_STATUS_TOKEN",
                "PROXY_ROLLOUT_DRAIN_STATUS_CONNECT_TIMEOUT_MS",
                "PROXY_ROLLOUT_DRAIN_STATUS_READ_TIMEOUT_MS",
                "PROXY_ROLLOUT_DRAIN_STATUS_OVERALL_TIMEOUT_MS",
                "PROXY_ROLLOUT_DRAIN_STATUS_MAX_BODY_BYTES",
            ] {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
            Self {
                saved: names
                    .iter()
                    .map(|name| (*name, env::var(name).ok()))
                    .collect(),
            }
            .without_ambient_scoped_tokens()
        }

        fn without_ambient_scoped_tokens(self) -> Self {
            unsafe {
                env::remove_var("PROXY_ADMIN_SCOPED_TOKENS");
                env::remove_var("PROXY_ROLLOUT_DRAIN_STATUS_CHECK_ENABLED");
                env::remove_var("PROXY_ROLLOUT_DRAIN_STATUS_URL");
                env::remove_var("PROXY_ROLLOUT_DRAIN_STATUS_TOKEN");
                env::remove_var("PROXY_ROLLOUT_DRAIN_STATUS_CONNECT_TIMEOUT_MS");
                env::remove_var("PROXY_ROLLOUT_DRAIN_STATUS_READ_TIMEOUT_MS");
                env::remove_var("PROXY_ROLLOUT_DRAIN_STATUS_OVERALL_TIMEOUT_MS");
                env::remove_var("PROXY_ROLLOUT_DRAIN_STATUS_MAX_BODY_BYTES");
            }
            self
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

    fn clear_production_env() {
        unsafe {
            env::set_var("NODE_ENV", "development");
            env::remove_var("APP_ENV");
        }
    }

    fn clear_route_store_env() {
        unsafe {
            env::remove_var("PROXY_ROUTE_STORE_BACKEND");
            env::remove_var("PROXY_ROUTE_STORE_REDIS_URL");
            env::remove_var("PROXY_ROUTE_STORE_KEY_PREFIX");
            env::remove_var("REGISTRY_URL");
            env::remove_var("REDIS_URL");
            env::remove_var("REDIS_KEY_PREFIX");
        }
    }

    fn clear_service_metadata_env() {
        unsafe {
            env::remove_var("SERVICE_BIND_HOST");
            env::remove_var("SERVICE_PUBLIC_HOST");
            env::remove_var("SERVICE_ADVERTISED_HOST");
            env::remove_var("SERVICE_ADMIN_BIND_HOST");
            env::remove_var("SERVICE_ADMIN_ADVERTISED_HOST");
            env::remove_var("SERVICE_TCP_FALLBACK_ADVERTISED_HOST");
            env::remove_var("PROXY_HOST");
            env::remove_var("PROXY_PORT");
            env::remove_var("PROXY_PUBLIC_HOST");
            env::remove_var("PROXY_ADMIN_HOST");
            env::remove_var("PROXY_ADMIN_PUBLIC_HOST");
            env::remove_var("PROXY_TCP_FALLBACK_HOST");
            env::remove_var("PROXY_TCP_FALLBACK_PUBLIC_HOST");
            env::remove_var("SERVICE_NAME");
            env::remove_var("SERVICE_INSTANCE_ID");
            env::remove_var("SERVICE_BUILD_VERSION");
            env::remove_var("SERVICE_ZONE");
        }
    }

    fn clear_upstream_discovery_env() {
        unsafe {
            env::remove_var("DISCOVERY_REQUIRED");
            env::remove_var("DISALLOW_LEGACY_DIRECT_CONFIG");
            env::remove_var("REGISTRY_ENABLED");
            env::remove_var("UPSTREAM_SERVER_ID");
            env::remove_var("UPSTREAM_LOCAL_SOCKET_NAME");
        }
    }

    #[test]
    fn parses_proxy_security_limits_from_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "PROXY_MAX_CONNECTIONS",
            "PROXY_MAX_PREAUTH_FAILURES",
            "PROXY_MSG_RATE_WINDOW_MS",
            "PROXY_MSG_RATE_MAX",
            "PROXY_IP_DENYLIST",
            "PROXY_MAX_CONNECTIONS_PER_IP",
            "PROXY_MAX_CONNECTIONS_PER_PLAYER",
            "PROXY_REDIS_BLOCKLIST_ENABLED",
            "PROXY_REDIS_BLOCKLIST_CACHE_TTL_MS",
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "PROXY_ADMIN_AUDIT_ENABLED",
            "PROXY_ADMIN_AUDIT_PATH",
            "PROXY_ADMIN_AUDIT_REQUIRE_ACTOR",
        ]);

        unsafe {
            clear_production_env();
            env::set_var("PROXY_MAX_CONNECTIONS", "42");
            env::set_var("PROXY_MAX_PREAUTH_FAILURES", "5");
            env::set_var("PROXY_MSG_RATE_WINDOW_MS", "250");
            env::set_var("PROXY_MSG_RATE_MAX", "30");
            env::set_var("PROXY_IP_DENYLIST", "203.0.113.10,198.51.100.0/24");
            env::set_var("PROXY_MAX_CONNECTIONS_PER_IP", "20");
            env::set_var("PROXY_MAX_CONNECTIONS_PER_PLAYER", "2");
            env::set_var("PROXY_REDIS_BLOCKLIST_ENABLED", "true");
            env::set_var("PROXY_REDIS_BLOCKLIST_CACHE_TTL_MS", "500");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
            env::remove_var("PROXY_ADMIN_AUDIT_ENABLED");
            env::remove_var("PROXY_ADMIN_AUDIT_PATH");
            env::remove_var("PROXY_ADMIN_AUDIT_REQUIRE_ACTOR");
        }

        let config = Config::from_env();

        assert_eq!(config.proxy_max_connections, 42);
        assert_eq!(config.proxy_max_preauth_failures, 5);
        assert_eq!(config.proxy_msg_rate_window_ms, 250);
        assert_eq!(config.proxy_msg_rate_max, 30);
        assert!(
            config
                .connection_limits
                .ip_denylist
                .contains("203.0.113.10".parse().unwrap())
        );
        assert!(
            config
                .connection_limits
                .ip_denylist
                .contains("198.51.100.8".parse().unwrap())
        );
        assert_eq!(config.connection_limits.max_connections_per_ip, 20);
        assert_eq!(config.connection_limits.max_connections_per_player, 2);
        assert!(config.redis_blocklist_enabled);
        assert_eq!(config.redis_blocklist_cache_ttl_ms, 500);
    }

    #[test]
    fn uses_proxy_security_limit_defaults_for_invalid_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "PROXY_MAX_CONNECTIONS",
            "PROXY_MAX_PREAUTH_FAILURES",
            "PROXY_MSG_RATE_WINDOW_MS",
            "PROXY_MSG_RATE_MAX",
            "PROXY_IP_DENYLIST",
            "PROXY_MAX_CONNECTIONS_PER_IP",
            "PROXY_MAX_CONNECTIONS_PER_PLAYER",
            "PROXY_REDIS_BLOCKLIST_ENABLED",
            "PROXY_REDIS_BLOCKLIST_CACHE_TTL_MS",
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            clear_production_env();
            env::set_var("PROXY_MAX_CONNECTIONS", "not-a-number");
            env::set_var("PROXY_MAX_PREAUTH_FAILURES", "not-a-number");
            env::set_var("PROXY_MSG_RATE_WINDOW_MS", "not-a-number");
            env::set_var("PROXY_MSG_RATE_MAX", "not-a-number");
            env::remove_var("PROXY_IP_DENYLIST");
            env::set_var("PROXY_MAX_CONNECTIONS_PER_IP", "not-a-number");
            env::set_var("PROXY_MAX_CONNECTIONS_PER_PLAYER", "not-a-number");
            env::set_var("PROXY_REDIS_BLOCKLIST_ENABLED", "not-a-bool");
            env::set_var("PROXY_REDIS_BLOCKLIST_CACHE_TTL_MS", "not-a-number");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let config = Config::from_env();

        assert_eq!(config.proxy_max_connections, 0);
        assert_eq!(config.proxy_max_preauth_failures, 3);
        assert_eq!(config.proxy_msg_rate_window_ms, 1000);
        assert_eq!(config.proxy_msg_rate_max, 0);
        assert_eq!(config.connection_limits.max_connections_per_ip, 0);
        assert_eq!(config.connection_limits.max_connections_per_player, 0);
        assert!(!config.redis_blocklist_enabled);
        assert_eq!(config.redis_blocklist_cache_ttl_ms, 2000);
    }

    #[test]
    fn parses_rollout_drain_status_check_config_from_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "PROXY_ROLLOUT_DRAIN_STATUS_CHECK_ENABLED",
            "PROXY_ROLLOUT_DRAIN_STATUS_URL",
            "PROXY_ROLLOUT_DRAIN_STATUS_TOKEN",
            "PROXY_ROLLOUT_DRAIN_STATUS_CONNECT_TIMEOUT_MS",
            "PROXY_ROLLOUT_DRAIN_STATUS_READ_TIMEOUT_MS",
            "PROXY_ROLLOUT_DRAIN_STATUS_OVERALL_TIMEOUT_MS",
            "PROXY_ROLLOUT_DRAIN_STATUS_MAX_BODY_BYTES",
        ]);

        unsafe {
            clear_production_env();
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
            env::set_var("PROXY_ROLLOUT_DRAIN_STATUS_CHECK_ENABLED", "true");
            env::set_var(
                "PROXY_ROLLOUT_DRAIN_STATUS_URL",
                "http://127.0.0.1:3000/api/v1/internal/game-server/rollout-drain-status",
            );
            env::set_var("PROXY_ROLLOUT_DRAIN_STATUS_TOKEN", "internal-token");
            env::set_var("PROXY_ROLLOUT_DRAIN_STATUS_CONNECT_TIMEOUT_MS", "500");
            env::set_var("PROXY_ROLLOUT_DRAIN_STATUS_READ_TIMEOUT_MS", "600");
            env::set_var("PROXY_ROLLOUT_DRAIN_STATUS_OVERALL_TIMEOUT_MS", "700");
            env::set_var("PROXY_ROLLOUT_DRAIN_STATUS_MAX_BODY_BYTES", "2048");
        }

        let config = Config::try_from_env().unwrap();

        assert!(config.rollout_drain_status_check.enabled);
        assert_eq!(
            config.rollout_drain_status_check.url,
            "http://127.0.0.1:3000/api/v1/internal/game-server/rollout-drain-status"
        );
        assert_eq!(
            config.rollout_drain_status_check.token.as_deref(),
            Some("internal-token")
        );
        assert_eq!(config.rollout_drain_status_check.connect_timeout_ms, 500);
        assert_eq!(config.rollout_drain_status_check.read_timeout_ms, 600);
        assert_eq!(config.rollout_drain_status_check.overall_timeout_ms, 700);
        assert_eq!(config.rollout_drain_status_check.max_body_bytes, 2048);
    }

    #[test]
    fn rejects_invalid_proxy_ip_denylist() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "PROXY_IP_DENYLIST",
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            clear_production_env();
            env::set_var("PROXY_IP_DENYLIST", "192.0.2.0/33");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("invalid proxy ip denylist should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("PROXY_IP_DENYLIST"));
    }

    #[test]
    fn keeps_development_default_admin_token_compatible() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            clear_production_env();
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let config = Config::try_from_env().unwrap();

        assert_eq!(config.admin_token, DEFAULT_ADMIN_TOKEN);
        assert_eq!(config.admin_read_token, None);
        assert!(config.admin_audit_enabled);
        assert_eq!(config.admin_audit_path, "logs/game-proxy/admin-audit.jsonl");
        assert!(!config.admin_audit_require_actor);
    }

    #[test]
    fn parses_proxy_admin_audit_config_from_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "PROXY_ADMIN_AUDIT_ENABLED",
            "PROXY_ADMIN_AUDIT_PATH",
            "PROXY_ADMIN_AUDIT_REQUIRE_ACTOR",
        ]);

        unsafe {
            clear_production_env();
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
            env::set_var("PROXY_ADMIN_AUDIT_ENABLED", "false");
            env::set_var("PROXY_ADMIN_AUDIT_PATH", "logs/custom/proxy-admin.jsonl");
            env::set_var("PROXY_ADMIN_AUDIT_REQUIRE_ACTOR", "true");
        }

        let config = Config::try_from_env().unwrap();

        assert!(!config.admin_audit_enabled);
        assert_eq!(config.admin_audit_path, "logs/custom/proxy-admin.jsonl");
        assert!(config.admin_audit_require_actor);
    }

    #[test]
    fn rejects_default_admin_token_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            env::set_var("APP_ENV", "production");
            env::remove_var("NODE_ENV");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("production default admin token should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("PROXY_ADMIN_TOKEN"));
    }

    #[test]
    fn rejects_empty_admin_token_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("PROXY_ADMIN_TOKEN", "");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("production empty admin token should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("PROXY_ADMIN_TOKEN"));
    }

    #[test]
    fn rejects_default_admin_token_when_app_env_is_production_even_if_node_env_is_not() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            env::set_var("NODE_ENV", "development");
            env::set_var("APP_ENV", "production");
            env::set_var("PROXY_ADMIN_TOKEN", DEFAULT_ADMIN_TOKEN);
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("APP_ENV=production should reject default admin token"),
            Err(error) => error,
        };

        assert!(error.contains("PROXY_ADMIN_TOKEN"));
    }

    #[test]
    fn accepts_custom_admin_token_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "REGISTRY_ENABLED",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("REGISTRY_ENABLED", "true");
            env::set_var("PROXY_ADMIN_TOKEN", "prod-proxy-admin-token-123");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let config = Config::try_from_env().unwrap();

        assert_eq!(config.admin_token, "prod-proxy-admin-token-123");
        assert_eq!(config.admin_read_token, None);
    }

    #[test]
    fn accepts_custom_admin_read_token_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "REGISTRY_ENABLED",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("REGISTRY_ENABLED", "true");
            env::set_var("PROXY_ADMIN_TOKEN", "prod-proxy-admin-token-123");
            env::set_var("PROXY_ADMIN_READ_TOKEN", "prod-proxy-admin-read-token-123");
        }

        let config = Config::try_from_env().unwrap();

        assert_eq!(
            config.admin_read_token.as_deref(),
            Some("prod-proxy-admin-read-token-123")
        );
    }

    #[test]
    fn parses_admin_scoped_tokens_from_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "PROXY_ADMIN_SCOPED_TOKENS",
        ]);

        unsafe {
            clear_production_env();
            env::set_var("PROXY_ADMIN_TOKEN", "write-token");
            env::set_var("PROXY_ADMIN_READ_TOKEN", "read-token");
            env::set_var(
                "PROXY_ADMIN_SCOPED_TOKENS",
                "maintenance-token:proxy.maintenance.write;route-token:proxy.route.write,proxy.read;all-token:*",
            );
        }

        let config = Config::try_from_env().unwrap();

        assert_eq!(config.admin_scoped_tokens.len(), 3);
        assert_eq!(config.admin_scoped_tokens[0].token, "maintenance-token");
        assert_eq!(
            config.admin_scoped_tokens[0].permissions,
            vec![AdminPermissionScope::MaintenanceWrite]
        );
        assert_eq!(
            config.admin_scoped_tokens[1].permissions,
            vec![AdminPermissionScope::RouteWrite, AdminPermissionScope::Read]
        );
        assert_eq!(
            config.admin_scoped_tokens[2].permissions,
            vec![AdminPermissionScope::All]
        );
    }

    #[test]
    fn rejects_invalid_admin_scoped_token_config() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "PROXY_ADMIN_SCOPED_TOKENS",
        ]);

        unsafe {
            clear_production_env();
            env::set_var("PROXY_ADMIN_TOKEN", "write-token");
            env::set_var("PROXY_ADMIN_READ_TOKEN", "read-token");
            env::set_var(
                "PROXY_ADMIN_SCOPED_TOKENS",
                "scoped-token:proxy.route.delete",
            );
        }
        let error = match Config::try_from_env() {
            Ok(_) => panic!("unknown scoped admin permission should be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("unknown permission"));

        unsafe {
            env::set_var(
                "PROXY_ADMIN_SCOPED_TOKENS",
                "same-token:proxy.read;same-token:proxy.route.write",
            );
        }
        let error = match Config::try_from_env() {
            Ok(_) => panic!("duplicate scoped admin token should be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("duplicate token"));

        unsafe {
            env::set_var("PROXY_ADMIN_SCOPED_TOKENS", ":proxy.read");
        }
        let error = match Config::try_from_env() {
            Ok(_) => panic!("empty scoped admin token should be rejected"),
            Err(error) => error,
        };
        assert!(error.contains("empty token"));
    }

    #[test]
    fn rejects_admin_scoped_token_reusing_compat_tokens() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "PROXY_ADMIN_SCOPED_TOKENS",
        ]);

        unsafe {
            clear_production_env();
            env::set_var("PROXY_ADMIN_TOKEN", "write-token");
            env::set_var("PROXY_ADMIN_READ_TOKEN", "read-token");
            env::set_var("PROXY_ADMIN_SCOPED_TOKENS", "read-token:proxy.read");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("scoped admin token must not reuse compatibility tokens"),
            Err(error) => error,
        };

        assert!(error.contains("duplicate token"));
    }

    #[test]
    fn rejects_weak_admin_scoped_token_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "PROXY_ADMIN_SCOPED_TOKENS",
        ]);

        unsafe {
            env::set_var("APP_ENV", "production");
            env::remove_var("NODE_ENV");
            env::set_var("PROXY_ADMIN_TOKEN", "prod-proxy-admin-token-123");
            env::set_var("PROXY_ADMIN_READ_TOKEN", "prod-proxy-admin-read-token-123");
            env::set_var("PROXY_ADMIN_SCOPED_TOKENS", "short:proxy.route.write");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("weak scoped admin token should be rejected in production"),
            Err(error) => error,
        };

        assert!(error.contains("weak token"));
    }

    #[test]
    fn rejects_default_admin_read_token_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("PROXY_ADMIN_TOKEN", "prod-proxy-admin-token-123");
            env::set_var("PROXY_ADMIN_READ_TOKEN", DEFAULT_ADMIN_READ_TOKEN);
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("production default admin read token should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("PROXY_ADMIN_READ_TOKEN"));
    }

    #[test]
    fn rejects_admin_read_token_equal_to_write_token_in_production() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            env::set_var("NODE_ENV", "production");
            env::remove_var("APP_ENV");
            env::set_var("PROXY_ADMIN_TOKEN", "prod-proxy-admin-token-123");
            env::set_var("PROXY_ADMIN_READ_TOKEN", "prod-proxy-admin-token-123");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("production duplicated admin read token should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("PROXY_ADMIN_READ_TOKEN"));
    }

    #[test]
    fn uses_memory_route_store_by_default() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "PROXY_ROUTE_STORE_BACKEND",
            "PROXY_ROUTE_STORE_REDIS_URL",
            "PROXY_ROUTE_STORE_KEY_PREFIX",
            "REGISTRY_URL",
            "REDIS_URL",
            "REDIS_KEY_PREFIX",
        ]);

        unsafe {
            clear_production_env();
            clear_route_store_env();
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let config = Config::try_from_env().unwrap();

        assert!(matches!(
            config.route_store_backend,
            RouteStoreBackend::Memory
        ));
    }

    #[test]
    fn local_static_upstream_fallback_is_allowed_in_node_development() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "DISCOVERY_REQUIRED",
            "DISALLOW_LEGACY_DIRECT_CONFIG",
            "REGISTRY_ENABLED",
            "REGISTRY_KEY_PREFIX",
            "REDIS_KEY_PREFIX",
            "UPSTREAM_SERVER_ID",
            "UPSTREAM_LOCAL_SOCKET_NAME",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            clear_production_env();
            clear_upstream_discovery_env();
            env::set_var("NODE_ENV", "development");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let config = Config::try_from_env().unwrap();

        assert!(!config.registry_enabled);
        assert!(!config.discovery_required());
        assert!(config.local_discovery_fallback_enabled);
        assert!(config.static_upstream_fallback_allowed());
        assert_eq!(config.upstream_server_id, "game-server-1");
        assert_eq!(
            config.upstream_local_socket_name,
            "myserver-game-server.sock"
        );
        assert!(config.validate_upstream_discovery().is_ok());
    }

    #[test]
    fn registry_disabled_static_upstream_fallback_requires_explicit_local_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "DISCOVERY_REQUIRED",
            "DISALLOW_LEGACY_DIRECT_CONFIG",
            "REGISTRY_ENABLED",
            "REGISTRY_KEY_PREFIX",
            "REDIS_KEY_PREFIX",
            "UPSTREAM_SERVER_ID",
            "UPSTREAM_LOCAL_SOCKET_NAME",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            clear_production_env();
            clear_upstream_discovery_env();
            env::remove_var("NODE_ENV");
            env::set_var("APP_ENV", "development");
            env::set_var("UPSTREAM_SERVER_ID", "game-server-blue");
            env::set_var("UPSTREAM_LOCAL_SOCKET_NAME", "game-blue.sock");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("non-local registry-disabled static upstream fallback should fail"),
            Err(error) => error,
        };

        assert!(error.contains("REGISTRY_ENABLED=true is required"));
        assert!(error.contains("NODE_ENV=development"));
        assert!(error.contains("APP_ENV=local"));

        unsafe {
            env::remove_var("APP_ENV");
            env::set_var("UPSTREAM_SERVER_ID", "game-server-green");
            env::set_var("UPSTREAM_LOCAL_SOCKET_NAME", "game-green.sock");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("registry-disabled static upstream fallback without env should fail"),
            Err(error) => error,
        };

        assert!(error.contains("REGISTRY_ENABLED=true is required"));
        assert!(error.contains("NODE_ENV=development"));
        assert!(error.contains("APP_ENV=local"));
    }

    #[test]
    fn app_env_local_allows_static_upstream_fallback() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "DISCOVERY_REQUIRED",
            "DISALLOW_LEGACY_DIRECT_CONFIG",
            "REGISTRY_ENABLED",
            "UPSTREAM_SERVER_ID",
            "UPSTREAM_LOCAL_SOCKET_NAME",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            clear_production_env();
            clear_upstream_discovery_env();
            env::set_var("APP_ENV", "local");
            env::set_var("UPSTREAM_SERVER_ID", "game-server-blue");
            env::set_var("UPSTREAM_LOCAL_SOCKET_NAME", "game-blue.sock");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let config = Config::try_from_env().unwrap();

        assert!(config.local_discovery_fallback_enabled);
        assert!(config.static_upstream_fallback_allowed());
        assert_eq!(config.upstream_server_id, "game-server-blue");
        assert_eq!(config.upstream_local_socket_name, "game-blue.sock");
        assert!(config.legacy_direct_config_warnings.is_empty());
    }

    #[test]
    fn env_example_does_not_enable_static_upstream_fallback_by_default() {
        let env_example = include_str!("../.env.example");
        let active_keys = env_example
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_once('=').map(|(key, _)| key.trim()))
            .collect::<Vec<_>>();

        assert!(!active_keys.contains(&"UPSTREAM_SERVER_ID"));
        assert!(!active_keys.contains(&"UPSTREAM_LOCAL_SOCKET_NAME"));
        assert!(
            env_example.contains("Strict/test/production must use registry discovery"),
            ".env.example must document that strict/test/production use registry discovery"
        );
    }

    #[test]
    fn strict_discovery_rejects_disabled_registry() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "DISCOVERY_REQUIRED",
            "DISALLOW_LEGACY_DIRECT_CONFIG",
            "REGISTRY_ENABLED",
            "UPSTREAM_SERVER_ID",
            "UPSTREAM_LOCAL_SOCKET_NAME",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            clear_production_env();
            clear_upstream_discovery_env();
            env::set_var("DISCOVERY_REQUIRED", "true");
            env::set_var("REGISTRY_ENABLED", "false");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("expected disabled registry to fail strict discovery config load"),
            Err(error) => error,
        };

        assert!(error.contains("REGISTRY_ENABLED=true"));
    }

    #[test]
    fn strict_discovery_rejects_static_upstream_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "DISCOVERY_REQUIRED",
            "DISALLOW_LEGACY_DIRECT_CONFIG",
            "REGISTRY_ENABLED",
            "UPSTREAM_SERVER_ID",
            "UPSTREAM_LOCAL_SOCKET_NAME",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        let strict_cases = [
            ("NODE_ENV", "test"),
            ("NODE_ENV", "testing"),
            ("NODE_ENV", "staging"),
            ("NODE_ENV", "prod"),
            ("NODE_ENV", "production"),
            ("APP_ENV", "test"),
            ("APP_ENV", "staging"),
            ("APP_ENV", "prod"),
            ("APP_ENV", "production"),
            ("APP_ENV", "testing"),
        ];

        for (name, value) in strict_cases {
            unsafe {
                clear_production_env();
                clear_upstream_discovery_env();
                env::set_var(name, value);
                env::set_var("REGISTRY_ENABLED", "true");
                env::set_var("UPSTREAM_SERVER_ID", "game-server-blue");
                env::set_var("UPSTREAM_LOCAL_SOCKET_NAME", "game-blue.sock");
                env::set_var("PROXY_ADMIN_TOKEN", "prod-proxy-admin-token-123");
                env::remove_var("PROXY_ADMIN_READ_TOKEN");
            }

            let error = match Config::try_from_env() {
                Ok(_) => panic!("strict discovery static upstream env should be rejected"),
                Err(error) => error,
            };

            assert!(error.contains("strict service discovery forbids legacy direct config"));
            assert!(error.contains("UPSTREAM_SERVER_ID"));
            assert!(error.contains("UPSTREAM_LOCAL_SOCKET_NAME"));
        }

        unsafe {
            clear_production_env();
            clear_upstream_discovery_env();
            env::set_var("NODE_ENV", "development");
            env::set_var("DISCOVERY_REQUIRED", "true");
            env::set_var("REGISTRY_ENABLED", "true");
            env::set_var("UPSTREAM_SERVER_ID", "game-server-blue");
            env::set_var("UPSTREAM_LOCAL_SOCKET_NAME", "game-blue.sock");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("DISCOVERY_REQUIRED=true static upstream env should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("strict service discovery forbids legacy direct config"));
        assert!(error.contains("UPSTREAM_SERVER_ID"));
        assert!(error.contains("UPSTREAM_LOCAL_SOCKET_NAME"));
    }

    #[test]
    fn rejects_static_upstream_env_when_migration_complete_switch_is_enabled() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "DISCOVERY_REQUIRED",
            "DISALLOW_LEGACY_DIRECT_CONFIG",
            "REGISTRY_ENABLED",
            "UPSTREAM_SERVER_ID",
            "UPSTREAM_LOCAL_SOCKET_NAME",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            clear_production_env();
            clear_upstream_discovery_env();
            env::set_var("DISALLOW_LEGACY_DIRECT_CONFIG", "true");
            env::set_var("UPSTREAM_SERVER_ID", "game-server-blue");
            env::set_var("UPSTREAM_LOCAL_SOCKET_NAME", "game-blue.sock");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("legacy static upstream env should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("DISALLOW_LEGACY_DIRECT_CONFIG=true forbids legacy direct config"));
        assert!(error.contains("UPSTREAM_SERVER_ID"));
        assert!(error.contains("UPSTREAM_LOCAL_SOCKET_NAME"));
    }

    #[test]
    fn test_environment_rejects_static_upstream_env_when_migration_complete_switch_is_enabled() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "DISCOVERY_REQUIRED",
            "DISALLOW_LEGACY_DIRECT_CONFIG",
            "REGISTRY_ENABLED",
            "UPSTREAM_SERVER_ID",
            "UPSTREAM_LOCAL_SOCKET_NAME",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            clear_production_env();
            clear_upstream_discovery_env();
            env::set_var("APP_ENV", "test");
            env::set_var("REGISTRY_ENABLED", "true");
            env::set_var("DISCOVERY_REQUIRED", "true");
            env::set_var("DISALLOW_LEGACY_DIRECT_CONFIG", "true");
            env::set_var("UPSTREAM_SERVER_ID", "game-server-blue");
            env::set_var("UPSTREAM_LOCAL_SOCKET_NAME", "game-blue.sock");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("legacy static upstream env should be rejected"),
            Err(error) => error,
        };

        assert!(error.contains("DISALLOW_LEGACY_DIRECT_CONFIG=true forbids legacy direct config"));
        assert!(error.contains("UPSTREAM_SERVER_ID"));
        assert!(error.contains("UPSTREAM_LOCAL_SOCKET_NAME"));
    }

    #[test]
    fn accepts_migration_complete_switch_without_static_upstream_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "DISCOVERY_REQUIRED",
            "DISALLOW_LEGACY_DIRECT_CONFIG",
            "REGISTRY_ENABLED",
            "UPSTREAM_SERVER_ID",
            "UPSTREAM_LOCAL_SOCKET_NAME",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            clear_production_env();
            clear_upstream_discovery_env();
            env::set_var("DISALLOW_LEGACY_DIRECT_CONFIG", "true");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let config = Config::try_from_env().unwrap();

        assert!(config.legacy_direct_config_warnings.is_empty());
    }

    #[test]
    fn test_environment_requires_registry_for_discovery() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "DISCOVERY_REQUIRED",
            "REGISTRY_ENABLED",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
        ]);

        unsafe {
            clear_upstream_discovery_env();
            env::set_var("APP_ENV", "test");
            env::remove_var("NODE_ENV");
            env::set_var("REGISTRY_ENABLED", "false");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let error = match Config::try_from_env() {
            Ok(_) => panic!("expected test environment to fail without registry discovery"),
            Err(error) => error,
        };

        assert!(error.contains("REGISTRY_ENABLED=true"));
    }

    #[test]
    fn service_metadata_config_uses_defaults() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "PROXY_ROUTE_STORE_BACKEND",
            "PROXY_PORT",
            "SERVICE_NAME",
            "SERVICE_INSTANCE_ID",
            "SERVICE_BUILD_VERSION",
            "SERVICE_ZONE",
        ]);

        unsafe {
            clear_production_env();
            clear_service_metadata_env();
            env::remove_var("PROXY_ROUTE_STORE_BACKEND");
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
        }

        let config = Config::try_from_env().unwrap();
        let metadata = config.service_instance_metadata();

        assert_eq!(config.service_name, "game-proxy");
        assert_eq!(config.service_instance_id, "game-proxy-4000");
        assert_eq!(config.service_build_version, "dev");
        assert_eq!(config.service_zone, "local");
        assert_eq!(config.route_store_backend_name(), "memory");
        assert_eq!(metadata["service_name"], "game-proxy");
        assert_eq!(metadata["service_instance_id"], "game-proxy-4000");
        assert_eq!(metadata["instance_id"], "game-proxy-4000");
        assert_eq!(metadata["route_store_backend"], "memory");
        assert_eq!(metadata["build_version"], "dev");
        assert_eq!(metadata["zone"], "local");
    }

    #[test]
    fn service_metadata_config_accepts_env_overrides() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "PROXY_ROUTE_STORE_BACKEND",
            "PROXY_PORT",
            "SERVICE_NAME",
            "SERVICE_INSTANCE_ID",
            "SERVICE_BUILD_VERSION",
            "SERVICE_ZONE",
        ]);

        unsafe {
            clear_production_env();
            clear_service_metadata_env();
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
            env::set_var("PROXY_PORT", "4100");
            env::set_var("PROXY_ROUTE_STORE_BACKEND", "redis");
            env::set_var("SERVICE_NAME", " edge-proxy ");
            env::set_var("SERVICE_BUILD_VERSION", " 2026.06.18 ");
            env::set_var("SERVICE_ZONE", " zone-a ");
        }

        let config = Config::try_from_env().unwrap();
        let metadata = config.service_instance_metadata();

        assert_eq!(config.service_name, "edge-proxy");
        assert_eq!(config.service_instance_id, "edge-proxy-4100");
        assert_eq!(config.service_build_version, "2026.06.18");
        assert_eq!(config.service_zone, "zone-a");
        assert_eq!(config.route_store_backend_name(), "redis");
        assert_eq!(metadata["service_name"], "edge-proxy");
        assert_eq!(metadata["service_instance_id"], "edge-proxy-4100");
        assert_eq!(metadata["instance_id"], "edge-proxy-4100");
        assert_eq!(metadata["route_store_backend"], "redis");
        assert_eq!(metadata["build_version"], "2026.06.18");
        assert_eq!(metadata["zone"], "zone-a");

        unsafe {
            env::set_var("SERVICE_INSTANCE_ID", " edge-proxy-a ");
        }

        let config = Config::try_from_env().unwrap();
        assert_eq!(config.service_instance_id, "edge-proxy-a");
        assert_eq!(
            config.service_instance_metadata()["instance_id"],
            "edge-proxy-a"
        );
    }

    #[test]
    fn endpoint_publish_hosts_prefer_unified_env_and_never_advertise_wildcard_bind() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "SERVICE_BIND_HOST",
            "SERVICE_PUBLIC_HOST",
            "SERVICE_ADVERTISED_HOST",
            "SERVICE_ADMIN_BIND_HOST",
            "SERVICE_ADMIN_ADVERTISED_HOST",
            "SERVICE_TCP_FALLBACK_ADVERTISED_HOST",
            "PROXY_HOST",
            "PROXY_PUBLIC_HOST",
            "PROXY_ADMIN_HOST",
            "PROXY_ADMIN_PUBLIC_HOST",
            "PROXY_TCP_FALLBACK_HOST",
            "PROXY_TCP_FALLBACK_PUBLIC_HOST",
        ]);

        unsafe {
            clear_production_env();
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
            env::set_var("SERVICE_BIND_HOST", "0.0.0.0");
            env::set_var("SERVICE_PUBLIC_HOST", "10.0.0.40");
            env::set_var("SERVICE_ADMIN_BIND_HOST", "0.0.0.0");
            env::set_var("SERVICE_ADMIN_ADVERTISED_HOST", "10.0.0.41");
            env::set_var("SERVICE_TCP_FALLBACK_ADVERTISED_HOST", "10.0.0.42");
            env::set_var("PROXY_HOST", "127.0.0.9");
            env::set_var("PROXY_PUBLIC_HOST", "10.0.0.99");
            env::set_var("PROXY_ADMIN_HOST", "127.0.0.10");
            env::set_var("PROXY_ADMIN_PUBLIC_HOST", "10.0.0.98");
            env::set_var("PROXY_TCP_FALLBACK_HOST", "0.0.0.0");
            env::set_var("PROXY_TCP_FALLBACK_PUBLIC_HOST", "10.0.0.97");
        }

        let config = Config::try_from_env().unwrap();

        assert_eq!(config.host, "0.0.0.0");
        assert_eq!(config.public_host, "10.0.0.40");
        assert_eq!(config.admin_host, "0.0.0.0");
        assert_eq!(config.admin_advertised_host, "10.0.0.41");
        assert_eq!(config.tcp_fallback_host, "0.0.0.0");
        assert_eq!(config.tcp_fallback_advertised_host, "10.0.0.42");

        unsafe {
            env::remove_var("SERVICE_PUBLIC_HOST");
            env::remove_var("SERVICE_ADVERTISED_HOST");
            env::remove_var("SERVICE_ADMIN_ADVERTISED_HOST");
            env::remove_var("SERVICE_TCP_FALLBACK_ADVERTISED_HOST");
            env::remove_var("PROXY_PUBLIC_HOST");
            env::remove_var("PROXY_HOST");
            env::remove_var("PROXY_ADMIN_HOST");
            env::remove_var("PROXY_ADMIN_PUBLIC_HOST");
            env::remove_var("PROXY_TCP_FALLBACK_HOST");
            env::remove_var("PROXY_TCP_FALLBACK_PUBLIC_HOST");
        }

        let config = Config::try_from_env().unwrap();

        assert_eq!(config.public_host, "127.0.0.1");
        assert_eq!(config.admin_advertised_host, "127.0.0.1");
        assert_eq!(config.tcp_fallback_advertised_host, "127.0.0.1");
    }

    #[test]
    fn parses_redis_route_store_config_with_fallbacks() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "PROXY_ROUTE_STORE_BACKEND",
            "PROXY_ROUTE_STORE_REDIS_URL",
            "PROXY_ROUTE_STORE_KEY_PREFIX",
            "REGISTRY_URL",
            "REDIS_URL",
            "REGISTRY_KEY_PREFIX",
            "REDIS_KEY_PREFIX",
        ]);

        unsafe {
            clear_production_env();
            clear_route_store_env();
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
            env::set_var("PROXY_ROUTE_STORE_BACKEND", "redis");
            env::set_var("REGISTRY_URL", "redis://registry:6379");
            env::set_var("REDIS_URL", "redis://redis:6379");
            env::set_var("REDIS_KEY_PREFIX", "dev:");
        }

        let config = Config::try_from_env().unwrap();

        assert!(matches!(
            config.route_store_backend,
            RouteStoreBackend::Redis
        ));
        assert_eq!(config.route_store_redis_url, "redis://registry:6379");
        assert_eq!(config.route_store_key_prefix, "dev:");
    }

    #[test]
    fn registry_key_prefix_prefers_registry_specific_env() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&[
            "NODE_ENV",
            "APP_ENV",
            "PROXY_ADMIN_TOKEN",
            "PROXY_ADMIN_READ_TOKEN",
            "REGISTRY_KEY_PREFIX",
            "REDIS_KEY_PREFIX",
        ]);

        unsafe {
            clear_production_env();
            env::remove_var("PROXY_ADMIN_TOKEN");
            env::remove_var("PROXY_ADMIN_READ_TOKEN");
            env::set_var("REGISTRY_KEY_PREFIX", "registry:");
            env::set_var("REDIS_KEY_PREFIX", "redis:");
        }

        let config = Config::try_from_env().unwrap();
        assert_eq!(config.registry_key_prefix, "registry:");

        unsafe {
            env::remove_var("REGISTRY_KEY_PREFIX");
        }

        let config = Config::try_from_env().unwrap();
        assert_eq!(config.registry_key_prefix, "redis:");
    }
}
