use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use redis::AsyncCommands;
use serde::Deserialize;
use tokio::sync::RwLock;

pub const IP_BLOCKED_ERROR: &str = "IP_BLOCKED";
pub const PLAYER_BLOCKED_ERROR: &str = "PLAYER_BLOCKED";
pub const BLOCKLIST_UNAVAILABLE_ERROR: &str = "BLOCKLIST_UNAVAILABLE";

#[derive(Clone)]
pub struct RedisBlocklistChecker {
    redis_client: Option<redis::Client>,
    key_prefix: String,
    cache_ttl: Duration,
    cache: Arc<RwLock<BlocklistCache>>,
}

#[derive(Debug, Default)]
struct BlocklistCache {
    snapshots: HashMap<BlocklistCacheKey, BlocklistCacheSnapshot>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
enum BlocklistCacheKey {
    Ip(String),
    Player(String),
}

#[derive(Clone, Copy, Debug)]
struct BlocklistCacheSnapshot {
    decision: BlocklistDecision,
    expires_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlocklistDecision {
    Allowed,
    Blocked(&'static str),
}

#[derive(Deserialize)]
struct BlocklistEntry {
    until: Option<u64>,
}

impl RedisBlocklistChecker {
    pub fn disabled() -> Self {
        Self {
            redis_client: None,
            key_prefix: String::new(),
            cache_ttl: Duration::from_millis(0),
            cache: Arc::new(RwLock::new(BlocklistCache::default())),
        }
    }

    pub fn new(
        enabled: bool,
        redis_url: &str,
        key_prefix: impl Into<String>,
        cache_ttl: Duration,
    ) -> Result<Self, redis::RedisError> {
        if !enabled {
            return Ok(Self::disabled());
        }

        Ok(Self {
            redis_client: Some(redis::Client::open(redis_url)?),
            key_prefix: key_prefix.into(),
            cache_ttl,
            cache: Arc::new(RwLock::new(BlocklistCache::default())),
        })
    }

    pub async fn check_ip(&self, ip: IpAddr) -> Result<BlocklistDecision, &'static str> {
        self.check(
            BlocklistCacheKey::Ip(ip.to_string()),
            blocklist_ip_key(&self.key_prefix, ip),
            IP_BLOCKED_ERROR,
        )
        .await
    }

    pub async fn check_player(&self, player_id: &str) -> Result<BlocklistDecision, &'static str> {
        self.check(
            BlocklistCacheKey::Player(player_id.to_string()),
            blocklist_player_key(&self.key_prefix, player_id),
            PLAYER_BLOCKED_ERROR,
        )
        .await
    }

    async fn check(
        &self,
        cache_key: BlocklistCacheKey,
        redis_key: String,
        blocked_error: &'static str,
    ) -> Result<BlocklistDecision, &'static str> {
        let Some(redis_client) = &self.redis_client else {
            return Ok(BlocklistDecision::Allowed);
        };

        let now = Instant::now();
        if let Some(decision) = self.cache.read().await.current(&cache_key, now) {
            return Ok(decision);
        }

        let mut conn = redis_client
            .get_multiplexed_async_connection()
            .await
            .map_err(|_| BLOCKLIST_UNAVAILABLE_ERROR)?;
        let raw: Option<String> = conn
            .get(&redis_key)
            .await
            .map_err(|_| BLOCKLIST_UNAVAILABLE_ERROR)?;
        let decision = parse_blocklist_decision(raw.as_deref(), current_unix_ms(), blocked_error);

        self.cache
            .write()
            .await
            .store(cache_key, decision, now + self.cache_ttl);
        Ok(decision)
    }
}

impl BlocklistCache {
    fn current(&self, key: &BlocklistCacheKey, now: Instant) -> Option<BlocklistDecision> {
        let snapshot = self.snapshots.get(key)?;
        if snapshot.expires_at <= now {
            return None;
        }
        Some(snapshot.decision)
    }

    fn store(&mut self, key: BlocklistCacheKey, decision: BlocklistDecision, expires_at: Instant) {
        self.snapshots.insert(
            key,
            BlocklistCacheSnapshot {
                decision,
                expires_at,
            },
        );
    }
}

pub fn blocklist_ip_key(redis_key_prefix: &str, ip: IpAddr) -> String {
    format!("{redis_key_prefix}security:blocklist:ip:{ip}")
}

pub fn blocklist_player_key(redis_key_prefix: &str, player_id: &str) -> String {
    format!("{redis_key_prefix}security:blocklist:player:{player_id}")
}

pub fn parse_blocklist_decision(
    raw: Option<&str>,
    now_unix_ms: u64,
    blocked_error: &'static str,
) -> BlocklistDecision {
    let Some(raw) = raw else {
        return BlocklistDecision::Allowed;
    };

    match serde_json::from_str::<BlocklistEntry>(raw) {
        Ok(entry) if entry.until.is_some_and(|until| until < now_unix_ms) => {
            BlocklistDecision::Allowed
        }
        _ => BlocklistDecision::Blocked(blocked_error),
    }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::time::{Duration, Instant};

    use super::{
        BlocklistCache, BlocklistCacheKey, BlocklistDecision, IP_BLOCKED_ERROR,
        PLAYER_BLOCKED_ERROR, RedisBlocklistChecker, blocklist_ip_key, blocklist_player_key,
        parse_blocklist_decision,
    };

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    #[test]
    fn blocklist_keys_use_shared_prefix() {
        assert_eq!(
            blocklist_ip_key("dev:", ip("203.0.113.10")),
            "dev:security:blocklist:ip:203.0.113.10"
        );
        assert_eq!(
            blocklist_player_key("", "player-1"),
            "security:blocklist:player:player-1"
        );
    }

    #[test]
    fn parses_missing_key_as_allowed() {
        assert_eq!(
            parse_blocklist_decision(None, 1000, IP_BLOCKED_ERROR),
            BlocklistDecision::Allowed
        );
    }

    #[test]
    fn parses_any_existing_non_expired_value_as_blocked() {
        assert_eq!(
            parse_blocklist_decision(Some("manual"), 1000, IP_BLOCKED_ERROR),
            BlocklistDecision::Blocked(IP_BLOCKED_ERROR)
        );
        assert_eq!(
            parse_blocklist_decision(
                Some(r#"{"reason":"chargeback","until":2000}"#),
                1000,
                PLAYER_BLOCKED_ERROR
            ),
            BlocklistDecision::Blocked(PLAYER_BLOCKED_ERROR)
        );
        assert_eq!(
            parse_blocklist_decision(Some(r#"{"reason":"abuse"}"#), 1000, PLAYER_BLOCKED_ERROR),
            BlocklistDecision::Blocked(PLAYER_BLOCKED_ERROR)
        );
    }

    #[test]
    fn parses_expired_until_as_allowed() {
        assert_eq!(
            parse_blocklist_decision(
                Some(r#"{"reason":"expired","until":999}"#),
                1000,
                IP_BLOCKED_ERROR
            ),
            BlocklistDecision::Allowed
        );
    }

    #[test]
    fn cache_returns_snapshot_until_expiry() {
        let now = Instant::now();
        let mut cache = BlocklistCache::default();
        let key = BlocklistCacheKey::Player("player-1".to_string());

        assert_eq!(cache.current(&key, now), None);

        cache.store(
            key.clone(),
            BlocklistDecision::Blocked(PLAYER_BLOCKED_ERROR),
            now + Duration::from_secs(2),
        );
        assert_eq!(
            cache.current(&key, now + Duration::from_secs(1)),
            Some(BlocklistDecision::Blocked(PLAYER_BLOCKED_ERROR))
        );
        assert_eq!(cache.current(&key, now + Duration::from_secs(2)), None);
    }

    #[tokio::test]
    async fn disabled_checker_is_noop_without_redis() {
        let checker = RedisBlocklistChecker::disabled();

        assert_eq!(
            checker.check_ip(ip("203.0.113.10")).await.unwrap(),
            BlocklistDecision::Allowed
        );
        assert_eq!(
            checker.check_player("player-1").await.unwrap(),
            BlocklistDecision::Allowed
        );
    }
}
