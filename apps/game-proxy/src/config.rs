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
    pub log_level: String,
    pub log_enable_console: bool,
    pub log_enable_file: bool,
    pub log_dir: String,
    pub local_socket_name: String,
    pub upstream_server_id: String,
    pub upstream_local_socket_name: String,
}

impl Config {
    pub fn from_env() -> Self {
        let host = env::var("PROXY_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port = env::var("PROXY_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(7100);
        let admin_host = env::var("PROXY_ADMIN_HOST").unwrap_or_else(|_| host.clone());
        let admin_port = env::var("PROXY_ADMIN_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(7101);
        let log_level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
        let log_enable_console = parse_bool("LOG_ENABLE_CONSOLE", true);
        let log_enable_file = parse_bool("LOG_ENABLE_FILE", true);
        let log_dir = env::var("LOG_DIR").unwrap_or_else(|_| "logs/game-proxy".to_string());
        let local_socket_name = env::var("PROXY_LOCAL_SOCKET_NAME")
            .unwrap_or_else(|_| "myserver-game-proxy.sock".to_string());
        let upstream_server_id = env::var("UPSTREAM_SERVER_ID")
            .unwrap_or_else(|_| "game-server-1".to_string());
        let upstream_local_socket_name = env::var("UPSTREAM_LOCAL_SOCKET_NAME")
            .unwrap_or_else(|_| "myserver-game-server.sock".to_string());

        Self {
            host,
            port,
            admin_host,
            admin_port,
            log_level,
            log_enable_console,
            log_enable_file,
            log_dir,
            local_socket_name,
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
}
