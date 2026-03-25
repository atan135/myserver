use std::env;

#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub log_level: String,
    pub redis_url: String,
    pub ticket_secret: String,
    pub heartbeat_timeout_secs: u64,
    pub max_body_len: usize,
}

impl Config {
    pub fn from_env() -> Self {
        let host = env::var("GAME_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port = env::var("GAME_PORT")
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(7000);
        let log_level = env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
        let redis_url = env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
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
            log_level,
            redis_url,
            ticket_secret,
            heartbeat_timeout_secs,
            max_body_len,
        }
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
