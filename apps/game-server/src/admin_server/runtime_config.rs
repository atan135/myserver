use tracing::info;

use super::current_unix_ms_u64;
use crate::core::context::SharedRuntimeConfig;
use crate::server::{DEFAULT_DRAIN_MODE_REASON, DEFAULT_DRAIN_MODE_SOURCE};

pub(super) async fn apply_runtime_config(
    runtime_config: &SharedRuntimeConfig,
    key: &str,
    value: &str,
) -> Result<(), &'static str> {
    let mut runtime = runtime_config.write().await;

    match key {
        "max_body_len" => {
            let parsed = value.parse::<usize>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if !(1..=1024 * 1024).contains(&parsed) {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.max_body_len = parsed;
            Ok(())
        }
        "heartbeat_timeout_secs" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if !(1..=3600).contains(&parsed) {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.heartbeat_timeout_secs = parsed;
            Ok(())
        }
        "msg_rate_window_ms" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if !(1..=60_000).contains(&parsed) {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.msg_rate_window_ms = parsed;
            Ok(())
        }
        "msg_rate_max" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if parsed > 10_000 {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.msg_rate_max = parsed;
            Ok(())
        }
        "player_msg_rate_window_ms" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if !(1..=60_000).contains(&parsed) {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.player_msg_rate_window_ms = parsed;
            Ok(())
        }
        "player_msg_rate_max" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if parsed > 10_000 {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.player_msg_rate_max = parsed;
            Ok(())
        }
        "input_timestamp_required" => {
            runtime.input_timestamp_required = parse_bool_config_value(value)?;
            Ok(())
        }
        "input_timestamp_max_skew_ms" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if parsed > 300_000 {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.input_timestamp_max_skew_ms = parsed;
            Ok(())
        }
        "input_anomaly_window_ms" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if !(1..=300_000).contains(&parsed) {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.input_anomaly_window_ms = parsed;
            Ok(())
        }
        "input_anomaly_max" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if parsed > 10_000 {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.input_anomaly_max = parsed;
            Ok(())
        }
        "drain_mode" | "drain_mode_enabled" => {
            let parsed = parse_bool_config_value(value)?;
            let previous = runtime.drain_mode_enabled;
            runtime.drain_mode_enabled = parsed;
            runtime.drain_mode_entered_at_ms = if parsed {
                runtime
                    .drain_mode_entered_at_ms
                    .or(Some(current_unix_ms_u64()))
            } else {
                None
            };
            runtime.drain_mode_reason = if parsed {
                normalized_drain_metadata(
                    &runtime.drain_mode_reason,
                    DEFAULT_DRAIN_MODE_REASON,
                    "INVALID_DRAIN_MODE_REASON",
                )?
            } else {
                DEFAULT_DRAIN_MODE_REASON.to_string()
            };
            runtime.drain_mode_source = if parsed {
                normalized_drain_metadata(
                    &runtime.drain_mode_source,
                    DEFAULT_DRAIN_MODE_SOURCE,
                    "INVALID_DRAIN_MODE_SOURCE",
                )?
            } else {
                DEFAULT_DRAIN_MODE_SOURCE.to_string()
            };

            if previous != parsed {
                info!(
                    drain_mode_enabled = parsed,
                    drain_mode_entered_at_ms = ?runtime.drain_mode_entered_at_ms,
                    drain_mode_reason = %runtime.drain_mode_reason,
                    drain_mode_source = %runtime.drain_mode_source,
                    "game-server drain mode updated"
                );
            }
            Ok(())
        }
        "drain_mode_reason" => {
            runtime.drain_mode_reason = normalized_drain_metadata(
                value,
                DEFAULT_DRAIN_MODE_REASON,
                "INVALID_DRAIN_MODE_REASON",
            )?;
            Ok(())
        }
        "drain_mode_source" => {
            runtime.drain_mode_source = normalized_drain_metadata(
                value,
                DEFAULT_DRAIN_MODE_SOURCE,
                "INVALID_DRAIN_MODE_SOURCE",
            )?;
            Ok(())
        }
        _ => Err("UNSUPPORTED_CONFIG_KEY"),
    }
}

fn normalized_drain_metadata(
    value: &str,
    default_value: &str,
    too_long_error: &'static str,
) -> Result<String, &'static str> {
    let normalized = value.trim();
    if normalized.len() > 128 {
        return Err(too_long_error);
    }
    if normalized.is_empty() {
        Ok(default_value.to_string())
    } else {
        Ok(normalized.to_string())
    }
}

fn parse_bool_config_value(value: &str) -> Result<bool, &'static str> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "on" | "enabled" => Ok(true),
        "0" | "false" | "off" | "disabled" => Ok(false),
        _ => Err("INVALID_CONFIG_VALUE"),
    }
}
