use std::env;

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

#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub admin_host: String,
    pub admin_port: u16,
    pub admin_token: String,
    pub tcp_fallback_host: String,
    pub tcp_fallback_port: u16,
    pub log_level: String,
    pub log_enable_console: bool,
    pub log_enable_file: bool,
    pub log_dir: String,
    pub local_socket_name: String,
    pub redis_url: String,
    pub redis_key_prefix: String,
    pub nats_url: String,
    pub ticket_secret: String,
    pub proxy_max_connections: u64,
    pub proxy_max_preauth_failures: u32,
    // Service Registry
    pub registry_enabled: bool,
    pub registry_url: String,
    pub registry_discover_interval_secs: u64,
    pub upstream_service_name: String,
    pub service_instance_id: String,
    // 保留旧配置用于向后兼容（当 registry 禁用时）
    pub upstream_server_id: String,
    pub upstream_local_socket_name: String,
}

impl Config {
    pub fn from_env() -> Self {
        let host = env::var("PROXY_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port = env::var("PROXY_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(4000);
        let admin_host = env::var("PROXY_ADMIN_HOST").unwrap_or_else(|_| host.clone());
        let admin_port = env::var("PROXY_ADMIN_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(7101);
        let admin_token = env::var("PROXY_ADMIN_TOKEN")
            .unwrap_or_else(|_| "dev-only-change-this-proxy-admin-token".to_string());
        let tcp_fallback_host =
            env::var("PROXY_TCP_FALLBACK_HOST").unwrap_or_else(|_| host.clone());
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
        let nats_url = env::var("NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let ticket_secret = env::var("TICKET_SECRET")
            .unwrap_or_else(|_| "dev-only-change-this-ticket-secret".to_string());
        let proxy_max_connections = parse_u64("PROXY_MAX_CONNECTIONS", 0);
        let proxy_max_preauth_failures = parse_u32("PROXY_MAX_PREAUTH_FAILURES", 3);

        // Service Registry
        let registry_enabled = parse_bool("REGISTRY_ENABLED", false);
        let registry_url = env::var("REGISTRY_URL")
            .or_else(|_| env::var("REDIS_URL"))
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let registry_discover_interval_secs = env::var("REGISTRY_DISCOVER_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(5);
        let upstream_service_name =
            env::var("UPSTREAM_SERVICE_NAME").unwrap_or_else(|_| "game-server".to_string());
        let service_instance_id =
            env::var("SERVICE_INSTANCE_ID").unwrap_or_else(|_| format!("game-proxy-{}", port));

        // 向后兼容的旧配置
        let upstream_server_id =
            env::var("UPSTREAM_SERVER_ID").unwrap_or_else(|_| "game-server-1".to_string());
        let upstream_local_socket_name = env::var("UPSTREAM_LOCAL_SOCKET_NAME")
            .unwrap_or_else(|_| "myserver-game-server.sock".to_string());

        Self {
            host,
            port,
            admin_host,
            admin_port,
            admin_token,
            tcp_fallback_host,
            tcp_fallback_port,
            log_level,
            log_enable_console,
            log_enable_file,
            log_dir,
            local_socket_name,
            redis_url,
            redis_key_prefix,
            nats_url,
            ticket_secret,
            proxy_max_connections,
            proxy_max_preauth_failures,
            registry_enabled,
            registry_url,
            registry_discover_interval_secs,
            upstream_service_name,
            service_instance_id,
            upstream_server_id,
            upstream_local_socket_name,
        }
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
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::sync::{Mutex, OnceLock};

    use super::Config;

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn parses_proxy_security_limits_from_env() {
        let _guard = env_lock().lock().unwrap();
        let old_max_connections = env::var("PROXY_MAX_CONNECTIONS").ok();
        let old_max_preauth_failures = env::var("PROXY_MAX_PREAUTH_FAILURES").ok();

        unsafe {
            env::set_var("PROXY_MAX_CONNECTIONS", "42");
            env::set_var("PROXY_MAX_PREAUTH_FAILURES", "5");
        }

        let config = Config::from_env();

        assert_eq!(config.proxy_max_connections, 42);
        assert_eq!(config.proxy_max_preauth_failures, 5);

        unsafe {
            match old_max_connections {
                Some(value) => env::set_var("PROXY_MAX_CONNECTIONS", value),
                None => env::remove_var("PROXY_MAX_CONNECTIONS"),
            }
            match old_max_preauth_failures {
                Some(value) => env::set_var("PROXY_MAX_PREAUTH_FAILURES", value),
                None => env::remove_var("PROXY_MAX_PREAUTH_FAILURES"),
            }
        }
    }

    #[test]
    fn uses_proxy_security_limit_defaults_for_invalid_env() {
        let _guard = env_lock().lock().unwrap();
        let old_max_connections = env::var("PROXY_MAX_CONNECTIONS").ok();
        let old_max_preauth_failures = env::var("PROXY_MAX_PREAUTH_FAILURES").ok();

        unsafe {
            env::set_var("PROXY_MAX_CONNECTIONS", "not-a-number");
            env::set_var("PROXY_MAX_PREAUTH_FAILURES", "not-a-number");
        }

        let config = Config::from_env();

        assert_eq!(config.proxy_max_connections, 0);
        assert_eq!(config.proxy_max_preauth_failures, 3);

        unsafe {
            match old_max_connections {
                Some(value) => env::set_var("PROXY_MAX_CONNECTIONS", value),
                None => env::remove_var("PROXY_MAX_CONNECTIONS"),
            }
            match old_max_preauth_failures {
                Some(value) => env::set_var("PROXY_MAX_PREAUTH_FAILURES", value),
                None => env::remove_var("PROXY_MAX_PREAUTH_FAILURES"),
            }
        }
    }
}
