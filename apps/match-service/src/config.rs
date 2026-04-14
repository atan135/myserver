//! 配置读取

use std::collections::HashMap;

#[derive(Clone)]
pub struct Config {
    pub bind_addr: String,
    pub match_timeout_secs: u64,
    pub max_concurrent_matches: usize,
    pub modes: HashMap<String, ModeConfig>,
    pub log_level: String,
    pub log_enable_console: bool,
    pub log_enable_file: bool,
    pub log_dir: String,
}

#[derive(Clone, Debug)]
pub struct ModeConfig {
    pub team_size: usize,
    pub total_size: usize,
    pub match_timeout_secs: u64,
}

impl Config {
    pub fn from_env() -> Self {
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
            bind_addr: std::env::var("MATCH_BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:9002".to_string()),
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
