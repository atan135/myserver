use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, OwnedMutexGuard};

const ROUTE_VERSION: u8 = 2;
static AUTHORITY_TOKEN_NONCE: AtomicU64 = AtomicU64::new(1);

const RESERVE_AUTHORITY_SCRIPT: &str = r#"
local generation = redis.call('INCR', KEYS[1])
local fence = tostring(generation) .. ':' .. ARGV[1]
redis.call('SET', KEYS[2], fence, 'EX', ARGV[2])
redis.call('DEL', KEYS[3], KEYS[4])
return generation
"#;

const REFRESH_IF_OWNER_SCRIPT: &str = r#"
if redis.call('GET', KEYS[3]) ~= ARGV[2] then
    return 3
end
local route = redis.call('GET', KEYS[1])
if route == ARGV[1] then
    local owner = redis.call('GET', KEYS[2])
    if owner and owner ~= ARGV[1] then
        return 3
    end
    redis.call('SET', KEYS[2], ARGV[1], 'EX', ARGV[4])
    redis.call('EXPIRE', KEYS[1], ARGV[3])
    redis.call('EXPIRE', KEYS[3], ARGV[4])
    return 1
end
if route then
    return 3
end
local owner = redis.call('GET', KEYS[2])
if owner == ARGV[1] then
    redis.call('EXPIRE', KEYS[2], ARGV[4])
    redis.call('EXPIRE', KEYS[3], ARGV[4])
    return 2
end
if owner then
    return 3
end
return 4
"#;

const PUBLISH_ROUTE_SCRIPT: &str = r#"
if redis.call('GET', KEYS[3]) ~= ARGV[2] then
    return 0
end
redis.call('SET', KEYS[2], ARGV[1], 'EX', ARGV[4])
redis.call('SET', KEYS[1], ARGV[1], 'EX', ARGV[3])
redis.call('EXPIRE', KEYS[3], ARGV[4])
return 1
"#;

const RESTORE_MISSING_ROUTE_SCRIPT: &str = r#"
if redis.call('GET', KEYS[3]) ~= ARGV[2] then
    return 0
end
local route = redis.call('GET', KEYS[1])
if route == ARGV[1] then
    redis.call('EXPIRE', KEYS[1], ARGV[3])
    return 1
end
if route then
    return 0
end
if redis.call('GET', KEYS[2]) ~= ARGV[1] then
    return 0
end
redis.call('SET', KEYS[1], ARGV[1], 'EX', ARGV[3])
redis.call('EXPIRE', KEYS[2], ARGV[4])
redis.call('EXPIRE', KEYS[3], ARGV[4])
return 1
"#;

const DELETE_IF_OWNER_SCRIPT: &str = r#"
if redis.call('GET', KEYS[3]) ~= ARGV[2] then
    return 0
end
if redis.call('GET', KEYS[2]) ~= ARGV[1] then
    return 0
end
local deleted = 0
if redis.call('GET', KEYS[1]) == ARGV[1] then
    deleted = deleted + redis.call('DEL', KEYS[1])
end
deleted = deleted + redis.call('DEL', KEYS[2])
deleted = deleted + redis.call('DEL', KEYS[3])
return deleted
"#;

const VALIDATE_AUTHORITY_SCRIPT: &str = r#"
if redis.call('GET', KEYS[3]) ~= ARGV[2] then
    return 0
end
if redis.call('GET', KEYS[1]) ~= ARGV[1] then
    return 0
end
if redis.call('GET', KEYS[2]) ~= ARGV[1] then
    return 0
end
return 1
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingRouteOwnership {
    Proven,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteRefreshState {
    Refreshed,
    Missing(MissingRouteOwnership),
    OwnershipChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteRefreshAction {
    None,
    RestoreMissing,
}

#[derive(Clone, Default)]
pub struct OnlineRouteCoordinator {
    account_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
}

impl OnlineRouteCoordinator {
    pub async fn lock_account(&self, account_player_id: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.account_locks.lock().await;
            locks.retain(|_, lock| lock.strong_count() > 0);
            match locks.get(account_player_id).and_then(Weak::upgrade) {
                Some(lock) => lock,
                None => {
                    let lock = Arc::new(Mutex::new(()));
                    locks.insert(account_player_id.to_string(), Arc::downgrade(&lock));
                    lock
                }
            }
        };
        lock.lock_owned().await
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnlineRoute {
    pub version: u8,
    pub character_id: String,
    pub instance_id: String,
    pub session_id: String,
    pub authority_generation: String,
    pub authority_token: String,
}

impl OnlineRoute {
    pub fn new(
        character_id: &str,
        instance_id: &str,
        session_id: u64,
        authority: &OnlineAuthority,
    ) -> Self {
        Self {
            version: ROUTE_VERSION,
            character_id: character_id.to_string(),
            instance_id: instance_id.to_string(),
            session_id: session_id.to_string(),
            authority_generation: authority.generation.to_string(),
            authority_token: authority.token.clone(),
        }
    }

    fn encoded(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnlineAuthority {
    pub generation: u64,
    pub token: String,
}

impl OnlineAuthority {
    pub fn fence_value(&self) -> String {
        format!("{}:{}", self.generation, self.token)
    }
}

pub fn online_route_key(prefix: &str, character_id: &str) -> String {
    let digest = Sha256::digest(character_id.as_bytes());
    format!("{prefix}game:online-route:{digest:x}")
}

pub fn online_route_owner_key(prefix: &str, character_id: &str) -> String {
    let digest = Sha256::digest(character_id.as_bytes());
    format!("{prefix}game:online-route-owner:{digest:x}")
}

pub fn online_route_generation_key(prefix: &str, character_id: &str) -> String {
    let digest = Sha256::digest(character_id.as_bytes());
    format!("{prefix}game:online-route-generation:{digest:x}")
}

pub fn online_route_fence_key(prefix: &str, character_id: &str) -> String {
    let digest = Sha256::digest(character_id.as_bytes());
    format!("{prefix}game:online-route-fence:{digest:x}")
}

pub fn session_can_replace(existing_session_id: Option<u64>, candidate_session_id: u64) -> bool {
    existing_session_id.is_none_or(|existing| candidate_session_id >= existing)
}

pub fn refresh_action(
    is_current_local_session: bool,
    state: RouteRefreshState,
) -> RouteRefreshAction {
    if is_current_local_session
        && state == RouteRefreshState::Missing(MissingRouteOwnership::Proven)
    {
        RouteRefreshAction::RestoreMissing
    } else {
        RouteRefreshAction::None
    }
}

pub fn online_route_ttl_secs(heartbeat_timeout_secs: u64) -> u64 {
    heartbeat_timeout_secs.saturating_mul(3).max(60)
}

pub fn online_route_refresh_secs(heartbeat_timeout_secs: u64) -> u64 {
    (online_route_ttl_secs(heartbeat_timeout_secs) / 3).max(1)
}

pub fn online_route_owner_ttl_secs(route_ttl_secs: u64) -> u64 {
    route_ttl_secs.saturating_mul(3)
}

pub async fn reserve_online_route_authority(
    redis: &mut redis::aio::MultiplexedConnection,
    prefix: &str,
    character_id: &str,
    instance_id: &str,
    session_id: u64,
    ttl_secs: u64,
) -> Result<OnlineAuthority, Box<dyn std::error::Error + Send + Sync>> {
    let token = new_authority_token(instance_id, session_id);
    let generation: i64 = redis::Script::new(RESERVE_AUTHORITY_SCRIPT)
        .key(online_route_generation_key(prefix, character_id))
        .key(online_route_fence_key(prefix, character_id))
        .key(online_route_key(prefix, character_id))
        .key(online_route_owner_key(prefix, character_id))
        .arg(&token)
        .arg(online_route_owner_ttl_secs(ttl_secs))
        .invoke_async(redis)
        .await?;
    let generation = u64::try_from(generation)
        .ok()
        .filter(|generation| *generation > 0)
        .ok_or_else(|| format!("invalid online route authority generation: {generation}"))?;
    Ok(OnlineAuthority { generation, token })
}

pub async fn publish_online_route(
    redis: &mut redis::aio::MultiplexedConnection,
    prefix: &str,
    character_id: &str,
    instance_id: &str,
    session_id: u64,
    authority: &OnlineAuthority,
    ttl_secs: u64,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let key = online_route_key(prefix, character_id);
    let owner_key = online_route_owner_key(prefix, character_id);
    let fence_key = online_route_fence_key(prefix, character_id);
    let value = OnlineRoute::new(character_id, instance_id, session_id, authority).encoded()?;
    let owner_ttl_secs = online_route_owner_ttl_secs(ttl_secs);
    let published: i32 = redis::Script::new(PUBLISH_ROUTE_SCRIPT)
        .key(key)
        .key(owner_key)
        .key(fence_key)
        .arg(value)
        .arg(authority.fence_value())
        .arg(ttl_secs)
        .arg(owner_ttl_secs)
        .invoke_async(redis)
        .await?;
    if !matches!(published, 0 | 1) {
        return Err(format!("invalid online route publish result: {published}").into());
    }
    Ok(published == 1)
}

pub async fn refresh_online_route(
    redis: &mut redis::aio::MultiplexedConnection,
    prefix: &str,
    character_id: &str,
    instance_id: &str,
    session_id: u64,
    authority: &OnlineAuthority,
    ttl_secs: u64,
) -> Result<RouteRefreshState, Box<dyn std::error::Error + Send + Sync>> {
    let key = online_route_key(prefix, character_id);
    let owner_key = online_route_owner_key(prefix, character_id);
    let fence_key = online_route_fence_key(prefix, character_id);
    let value = OnlineRoute::new(character_id, instance_id, session_id, authority).encoded()?;
    let owner_ttl_secs = online_route_owner_ttl_secs(ttl_secs);
    let state: i32 = redis::Script::new(REFRESH_IF_OWNER_SCRIPT)
        .key(key)
        .key(owner_key)
        .key(fence_key)
        .arg(value)
        .arg(authority.fence_value())
        .arg(ttl_secs)
        .arg(owner_ttl_secs)
        .invoke_async(redis)
        .await?;
    decode_refresh_state(state)
        .ok_or_else(|| format!("invalid online route refresh state: {state}").into())
}

pub async fn restore_missing_online_route(
    redis: &mut redis::aio::MultiplexedConnection,
    prefix: &str,
    character_id: &str,
    instance_id: &str,
    session_id: u64,
    authority: &OnlineAuthority,
    ttl_secs: u64,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let key = online_route_key(prefix, character_id);
    let owner_key = online_route_owner_key(prefix, character_id);
    let fence_key = online_route_fence_key(prefix, character_id);
    let value = OnlineRoute::new(character_id, instance_id, session_id, authority).encoded()?;
    let owner_ttl_secs = online_route_owner_ttl_secs(ttl_secs);
    let restored: i32 = redis::Script::new(RESTORE_MISSING_ROUTE_SCRIPT)
        .key(key)
        .key(owner_key)
        .key(fence_key)
        .arg(value)
        .arg(authority.fence_value())
        .arg(ttl_secs)
        .arg(owner_ttl_secs)
        .invoke_async(redis)
        .await?;
    Ok(restored == 1)
}

pub async fn clear_online_route(
    redis: &mut redis::aio::MultiplexedConnection,
    prefix: &str,
    character_id: &str,
    instance_id: &str,
    session_id: u64,
    authority: &OnlineAuthority,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let key = online_route_key(prefix, character_id);
    let owner_key = online_route_owner_key(prefix, character_id);
    let fence_key = online_route_fence_key(prefix, character_id);
    let value = OnlineRoute::new(character_id, instance_id, session_id, authority).encoded()?;
    let deleted: i32 = redis::Script::new(DELETE_IF_OWNER_SCRIPT)
        .key(key)
        .key(owner_key)
        .key(fence_key)
        .arg(value)
        .arg(authority.fence_value())
        .invoke_async(redis)
        .await?;
    Ok(deleted > 0)
}

pub async fn validate_online_route_authority(
    redis: &mut redis::aio::MultiplexedConnection,
    prefix: &str,
    character_id: &str,
    instance_id: &str,
    session_id: u64,
    authority: &OnlineAuthority,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let value = OnlineRoute::new(character_id, instance_id, session_id, authority).encoded()?;
    let valid: i32 = redis::Script::new(VALIDATE_AUTHORITY_SCRIPT)
        .key(online_route_key(prefix, character_id))
        .key(online_route_owner_key(prefix, character_id))
        .key(online_route_fence_key(prefix, character_id))
        .arg(value)
        .arg(authority.fence_value())
        .invoke_async(redis)
        .await?;
    Ok(valid == 1)
}

#[cfg(test)]
pub fn online_route_authority_matches_snapshot(
    current_route: Option<&str>,
    current_owner: Option<&str>,
    current_fence: Option<&str>,
    character_id: &str,
    instance_id: &str,
    session_id: u64,
    authority: &OnlineAuthority,
) -> bool {
    let Ok(expected_route) =
        OnlineRoute::new(character_id, instance_id, session_id, authority).encoded()
    else {
        return false;
    };
    current_route == Some(expected_route.as_str())
        && current_owner == Some(expected_route.as_str())
        && current_fence == Some(authority.fence_value().as_str())
}

fn new_authority_token(instance_id: &str, session_id: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let nonce = AUTHORITY_TOKEN_NONCE.fetch_add(1, Ordering::Relaxed);
    let mut hasher = Sha256::new();
    hasher.update(instance_id.as_bytes());
    hasher.update(session_id.to_be_bytes());
    hasher.update(now.to_be_bytes());
    hasher.update(nonce.to_be_bytes());
    format!("{:x}", hasher.finalize())
}

fn decode_refresh_state(state: i32) -> Option<RouteRefreshState> {
    match state {
        1 => Some(RouteRefreshState::Refreshed),
        2 => Some(RouteRefreshState::Missing(MissingRouteOwnership::Proven)),
        3 => Some(RouteRefreshState::OwnershipChanged),
        4 => Some(RouteRefreshState::Missing(MissingRouteOwnership::Unknown)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_key_hashes_character_id_and_respects_prefix() {
        let key = online_route_key("dev:", "chr_0000000000001");

        assert!(key.starts_with("dev:game:online-route:"));
        assert_eq!(key.len(), "dev:game:online-route:".len() + 64);
        assert!(!key.contains("chr_0000000000001"));
        assert!(
            online_route_owner_key("dev:", "chr_0000000000001")
                .starts_with("dev:game:online-route-owner:")
        );
        assert!(
            online_route_generation_key("dev:", "chr_0000000000001")
                .starts_with("dev:game:online-route-generation:")
        );
        assert!(
            online_route_fence_key("dev:", "chr_0000000000001")
                .starts_with("dev:game:online-route-fence:")
        );
    }

    #[test]
    fn route_value_carries_session_ownership_without_account_data() {
        let authority = OnlineAuthority {
            generation: 7,
            token: "a".repeat(64),
        };
        let route = OnlineRoute::new("chr_1", "game-server-a", 42, &authority);
        let json = route.encoded().unwrap();

        assert_eq!(serde_json::from_str::<OnlineRoute>(&json).unwrap(), route);
        assert_eq!(route.session_id, "42");
        assert_eq!(route.authority_generation, "7");
        assert_eq!(route.authority_token, "a".repeat(64));
        assert_eq!(route.version, 2);
        assert!(!json.contains("player"));
    }

    #[test]
    fn cross_instance_new_generation_fences_old_mutations() {
        let old = OnlineAuthority {
            generation: 11,
            token: "a".repeat(64),
        };
        let new = OnlineAuthority {
            generation: 12,
            token: "b".repeat(64),
        };
        let current_fence = new.fence_value();
        let old_route = OnlineRoute::new("chr_1", "game-server-a", 7, &old)
            .encoded()
            .unwrap();
        let new_route = OnlineRoute::new("chr_1", "game-server-b", 3, &new)
            .encoded()
            .unwrap();

        assert_ne!(old.fence_value(), current_fence);
        assert_eq!(new.fence_value(), current_fence);
        assert_ne!(old_route, new_route);
        assert!(!(new_route == old_route && new.fence_value() == old.fence_value()));
        for script in [
            PUBLISH_ROUTE_SCRIPT,
            REFRESH_IF_OWNER_SCRIPT,
            RESTORE_MISSING_ROUTE_SCRIPT,
            DELETE_IF_OWNER_SCRIPT,
            VALIDATE_AUTHORITY_SCRIPT,
        ] {
            assert!(script.contains("redis.call('GET', KEYS[3]) ~= ARGV[2]"));
        }
        assert!(RESERVE_AUTHORITY_SCRIPT.contains("redis.call('INCR', KEYS[1])"));
        assert!(RESERVE_AUTHORITY_SCRIPT.contains("redis.call('DEL', KEYS[3], KEYS[4])"));
    }

    #[test]
    fn authority_tokens_are_unique_and_lower_hex() {
        let first = new_authority_token("game-server-a", 1);
        let second = new_authority_token("game-server-a", 1);

        assert_ne!(first, second);
        assert!(first.len() == 64 && first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(second.len() == 64 && second.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn route_ttl_is_longer_than_heartbeat_and_refreshes_before_expiry() {
        assert_eq!(online_route_ttl_secs(30), 90);
        assert_eq!(online_route_refresh_secs(30), 30);
        assert_eq!(online_route_ttl_secs(1), 60);
        assert_eq!(online_route_refresh_secs(1), 20);
        assert_eq!(online_route_owner_ttl_secs(90), 270);
        assert!(online_route_owner_ttl_secs(90) > online_route_ttl_secs(30));
    }

    #[test]
    fn refresh_state_distinguishes_missing_from_ownership_change() {
        assert_eq!(decode_refresh_state(1), Some(RouteRefreshState::Refreshed));
        assert_eq!(
            decode_refresh_state(2),
            Some(RouteRefreshState::Missing(MissingRouteOwnership::Proven))
        );
        assert_eq!(
            decode_refresh_state(3),
            Some(RouteRefreshState::OwnershipChanged)
        );
        assert_eq!(
            decode_refresh_state(4),
            Some(RouteRefreshState::Missing(MissingRouteOwnership::Unknown))
        );
    }

    #[test]
    fn only_current_session_with_proven_missing_owner_can_restore() {
        let proven_missing = RouteRefreshState::Missing(MissingRouteOwnership::Proven);
        let unknown_missing = RouteRefreshState::Missing(MissingRouteOwnership::Unknown);

        assert_eq!(
            refresh_action(true, proven_missing),
            RouteRefreshAction::RestoreMissing
        );
        assert_eq!(
            refresh_action(false, proven_missing),
            RouteRefreshAction::None
        );
        assert_eq!(
            refresh_action(true, unknown_missing),
            RouteRefreshAction::None
        );
        assert_eq!(
            refresh_action(true, RouteRefreshState::OwnershipChanged),
            RouteRefreshAction::None
        );
        assert_eq!(
            refresh_action(false, RouteRefreshState::OwnershipChanged),
            RouteRefreshAction::None
        );
    }

    #[tokio::test]
    async fn same_instance_delayed_old_publish_is_serialized_before_new_session_publish() {
        use tokio::sync::Notify;

        let coordinator = OnlineRouteCoordinator::default();
        let installed = Arc::new(Mutex::new(None::<u64>));
        let published = Arc::new(Mutex::new(Vec::<u64>::new()));
        let old_installed = Arc::new(Notify::new());
        let release_old = Arc::new(Notify::new());

        let old_task = {
            let coordinator = coordinator.clone();
            let installed = installed.clone();
            let published = published.clone();
            let old_installed = old_installed.clone();
            let release_old = release_old.clone();
            tokio::spawn(async move {
                let _guard = coordinator.lock_account("player-1").await;
                *installed.lock().await = Some(1);
                old_installed.notify_one();
                release_old.notified().await;
                published.lock().await.push(1);
            })
        };

        old_installed.notified().await;
        let new_task = {
            let coordinator = coordinator.clone();
            let installed = installed.clone();
            let published = published.clone();
            tokio::spawn(async move {
                let _guard = coordinator.lock_account("player-1").await;
                let existing = *installed.lock().await;
                assert!(session_can_replace(existing, 2));
                *installed.lock().await = Some(2);
                published.lock().await.push(2);
            })
        };

        tokio::task::yield_now().await;
        assert!(published.lock().await.is_empty());
        release_old.notify_one();
        old_task.await.unwrap();
        new_task.await.unwrap();

        assert_eq!(*installed.lock().await, Some(2));
        assert_eq!(*published.lock().await, vec![1, 2]);
    }

    #[test]
    fn stale_local_session_cannot_replace_newer_registry_session() {
        assert!(session_can_replace(None, 1));
        assert!(session_can_replace(Some(1), 2));
        assert!(session_can_replace(Some(2), 2));
        assert!(!session_can_replace(Some(2), 1));
    }
}
