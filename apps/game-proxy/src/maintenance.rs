use std::sync::Arc;
use std::time::{Duration, Instant};

use redis::AsyncCommands;
use serde::Deserialize;
use tokio::sync::RwLock;

pub const MAINTENANCE_MODE_ERROR: &str = "MAINTENANCE_MODE";

#[derive(Clone)]
pub struct GlobalMaintenanceChecker {
    redis_client: redis::Client,
    state_key: String,
    cache_ttl: Duration,
    cache: Arc<RwLock<MaintenanceCache>>,
}

#[derive(Debug, Default)]
struct MaintenanceCache {
    snapshot: Option<MaintenanceCacheSnapshot>,
}

#[derive(Clone, Copy, Debug)]
struct MaintenanceCacheSnapshot {
    enabled: bool,
    expires_at: Instant,
}

#[derive(Deserialize)]
struct MaintenanceState {
    enabled: Option<bool>,
}

impl GlobalMaintenanceChecker {
    pub fn new(
        redis_url: &str,
        redis_key_prefix: impl AsRef<str>,
        cache_ttl: Duration,
    ) -> Result<Self, redis::RedisError> {
        Ok(Self {
            redis_client: redis::Client::open(redis_url)?,
            state_key: maintenance_state_key(redis_key_prefix.as_ref()),
            cache_ttl,
            cache: Arc::new(RwLock::new(MaintenanceCache::default())),
        })
    }

    pub async fn is_enabled(&self) -> Result<bool, &'static str> {
        let now = Instant::now();
        if let Some(enabled) = self.cache.read().await.current(now) {
            return Ok(enabled);
        }

        let mut conn = self
            .redis_client
            .get_multiplexed_async_connection()
            .await
            .map_err(|_| "AUTH_BACKEND_UNAVAILABLE")?;
        let raw: Option<String> = conn
            .get(&self.state_key)
            .await
            .map_err(|_| "AUTH_BACKEND_UNAVAILABLE")?;
        let enabled = parse_maintenance_enabled(raw.as_deref());

        self.cache
            .write()
            .await
            .store(enabled, now + self.cache_ttl);
        Ok(enabled)
    }
}

impl MaintenanceCache {
    fn current(&self, now: Instant) -> Option<bool> {
        let snapshot = self.snapshot?;
        if snapshot.expires_at <= now {
            return None;
        }
        Some(snapshot.enabled)
    }

    fn store(&mut self, enabled: bool, expires_at: Instant) {
        self.snapshot = Some(MaintenanceCacheSnapshot {
            enabled,
            expires_at,
        });
    }
}

pub fn maintenance_state_key(redis_key_prefix: &str) -> String {
    format!("{redis_key_prefix}maintenance:global")
}

pub fn parse_maintenance_enabled(raw: Option<&str>) -> bool {
    let Some(raw) = raw else {
        return false;
    };

    serde_json::from_str::<MaintenanceState>(raw)
        .ok()
        .and_then(|state| state.enabled)
        .unwrap_or(false)
}

pub fn should_reject_new_auth(local_enabled: bool, global_enabled: bool) -> bool {
    local_enabled || global_enabled
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{
        MaintenanceCache, maintenance_state_key, parse_maintenance_enabled, should_reject_new_auth,
    };

    #[test]
    fn maintenance_key_uses_shared_prefix() {
        assert_eq!(maintenance_state_key("dev:"), "dev:maintenance:global");
        assert_eq!(maintenance_state_key(""), "maintenance:global");
    }

    #[test]
    fn parses_global_maintenance_state_as_boolean_flag() {
        assert!(parse_maintenance_enabled(Some(
            r#"{"enabled":true,"reason":"deploy"}"#
        )));
        assert!(!parse_maintenance_enabled(Some(r#"{"enabled":false}"#)));
        assert!(!parse_maintenance_enabled(Some(r#"{"reason":"missing"}"#)));
        assert!(!parse_maintenance_enabled(Some("not json")));
        assert!(!parse_maintenance_enabled(None));
    }

    #[test]
    fn cache_returns_snapshot_until_expiry() {
        let now = Instant::now();
        let mut cache = MaintenanceCache::default();

        assert_eq!(cache.current(now), None);

        cache.store(true, now + Duration::from_secs(2));
        assert_eq!(cache.current(now + Duration::from_secs(1)), Some(true));
        assert_eq!(cache.current(now + Duration::from_secs(2)), None);
    }

    #[test]
    fn local_or_global_flag_rejects_new_auth() {
        assert!(should_reject_new_auth(true, false));
        assert!(should_reject_new_auth(false, true));
        assert!(should_reject_new_auth(true, true));
        assert!(!should_reject_new_auth(false, false));
    }
}
