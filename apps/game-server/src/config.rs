use std::env;

#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub csv_dir: String,
    pub csv_reload_enabled: bool,
    pub csv_reload_interval_secs: u64,
    pub admin_host: String,
    pub admin_port: u16,
    pub local_socket_name: String,
    pub log_level: String,
    pub log_enable_console: bool,
    pub log_enable_file: bool,
    pub log_dir: String,
    pub redis_url: String,
    pub redis_key_prefix: String,
    pub mysql_enabled: bool,
    pub mysql_url: String,
    pub mysql_pool_size: usize,
    pub ticket_secret: String,
    pub heartbeat_timeout_secs: u64,
    pub max_body_len: usize,
}

fn parse_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "True"))
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
        let admin_host = env::var("ADMIN_HOST").unwrap_or_else(|_| host.clone());
        let admin_port = env::var("ADMIN_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(7001);
        let local_socket_name = env::var("GAME_LOCAL_SOCKET_NAME")
            .unwrap_or_else(|_| "myserver-game-server.sock".to_string());
        let log_level = env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
        let log_enable_console = parse_bool("LOG_ENABLE_CONSOLE", true);
        let log_enable_file = parse_bool("LOG_ENABLE_FILE", true);
        let log_dir = env::var("LOG_DIR").unwrap_or_else(|_| "logs/game-server".to_string());
        let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let redis_key_prefix = env::var("REDIS_KEY_PREFIX").unwrap_or_default();
        let mysql_enabled = parse_bool("MYSQL_ENABLED", false);
        let mysql_url = env::var("MYSQL_URL")
            .unwrap_or_else(|_| "mysql://root:password@127.0.0.1:3306/myserver_game".to_string());
        let mysql_pool_size = env::var("MYSQL_POOL_SIZE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(10);
        let ticket_secret =
            env::var("TICKET_SECRET").unwrap_or_else(|_| "dev-only-change-this-ticket-secret".to_string());
        let heartbeat_timeout_secs = env::var("HEARTBEAT_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(30);
        let max_body_len = env::var("MAX_BODY_LEN")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(4096);

        Self {
            host,
            port,
            csv_dir,
            csv_reload_enabled,
            csv_reload_interval_secs,
            admin_host,
            admin_port,
            local_socket_name,
            log_level,
            log_enable_console,
            log_enable_file,
            log_dir,
            redis_url,
            redis_key_prefix,
            mysql_enabled,
            mysql_url,
            mysql_pool_size,
            ticket_secret,
            heartbeat_timeout_secs,
            max_body_len,
        }
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn admin_bind_addr(&self) -> String {
        format!("{}:{}", self.admin_host, self.admin_port)
    }
}
