use std::env;

fn parse_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(default)
}

#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub admin_host: String,
    pub admin_port: u16,
    pub tcp_fallback_host: String,
    pub tcp_fallback_port: u16,
    pub log_level: String,
    pub log_enable_console: bool,
    pub log_enable_file: bool,
    pub log_dir: String,
    pub local_socket_name: String,
    // Service Registry
    pub registry_enabled: bool,
    pub registry_url: String,
    pub registry_discover_interval_secs: u64,
    pub upstream_service_name: String,
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
            .unwrap_or(7002);
        let admin_host = env::var("PROXY_ADMIN_HOST").unwrap_or_else(|_| host.clone());
        let admin_port = env::var("PROXY_ADMIN_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(7101);
        let tcp_fallback_host = env::var("PROXY_TCP_FALLBACK_HOST").unwrap_or_else(|_| host.clone());
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

        // 向后兼容的旧配置
        let upstream_server_id = env::var("UPSTREAM_SERVER_ID")
            .unwrap_or_else(|_| "game-server-1".to_string());
        let upstream_local_socket_name = env::var("UPSTREAM_LOCAL_SOCKET_NAME")
            .unwrap_or_else(|_| "myserver-game-server.sock".to_string());

        Self {
            host,
            port,
            admin_host,
            admin_port,
            tcp_fallback_host,
            tcp_fallback_port,
            log_level,
            log_enable_console,
            log_enable_file,
            log_dir,
            local_socket_name,
            registry_enabled,
            registry_url,
            registry_discover_interval_secs,
            upstream_service_name,
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
