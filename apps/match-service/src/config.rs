//! 配置读取

use std::collections::HashMap;
use std::net::SocketAddr;

#[derive(Clone)]
pub struct Config {
    pub bind_addr: String,
    pub public_host: String,
    pub port: u16,
    pub match_timeout_secs: u64,
    pub max_concurrent_matches: usize,
    pub modes: HashMap<String, ModeConfig>,
    pub log_level: String,
    pub log_enable_console: bool,
    pub log_enable_file: bool,
    pub log_dir: String,
    pub redis_url: String,
    pub registry_enabled: bool,
    pub registry_url: String,
    pub registry_heartbeat_interval_secs: u64,
    pub service_name: String,
    pub service_instance_id: String,
}

#[derive(Clone, Debug)]
pub struct ModeConfig {
    pub team_size: usize,
    pub total_size: usize,
    pub match_timeout_secs: u64,
}

impl Config {
    pub fn from_env() -> Self {
        let bind_addr = std::env::var("MATCH_BIND_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:9002".to_string());
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

        Self {
            bind_addr,
            public_host: std::env::var("MATCH_PUBLIC_HOST")
                .unwrap_or_else(|_| "127.0.0.1".to_string()),
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
            registry_enabled: std::env::var("REGISTRY_ENABLED")
                .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "True"))
                .unwrap_or(false),
            registry_url: std::env::var("REGISTRY_URL")
                .or_else(|_| std::env::var("REDIS_URL"))
                .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
            registry_heartbeat_interval_secs: std::env::var("REGISTRY_HEARTBEAT_INTERVAL")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .unwrap_or(10),
            service_name: std::env::var("SERVICE_NAME")
                .unwrap_or_else(|_| "match-service".to_string()),
            service_instance_id: std::env::var("SERVICE_INSTANCE_ID")
                .unwrap_or_else(|_| format!("match-service-{}", port)),
        }
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

fn parse_port(bind_addr: &str) -> Option<u16> {
    let addr: SocketAddr = bind_addr.parse().ok()?;
    Some(addr.port())
}
