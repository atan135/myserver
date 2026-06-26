//! Match runtime state storage.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::pool::{MatchCandidate, MatchTask};
use crate::proto::myserver::matchservice::MatchEvent;
use crate::state::{CharacterMatchContext, CharacterMatchStatus};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn duration_since_now_ms(deadline_ms: u64) -> Duration {
    Duration::from_millis(deadline_ms.saturating_sub(now_ms()))
}

fn instant_deadline_ms(duration_until_deadline: Duration) -> u64 {
    now_ms().saturating_add(duration_until_deadline.as_millis() as u64)
}

fn lease_expires_at_ms(ttl: Duration) -> u64 {
    instant_deadline_ms(ttl)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoredCharacterMatchStatus {
    Idle,
    Matching,
    Matched,
    InRoom,
}

impl From<CharacterMatchStatus> for StoredCharacterMatchStatus {
    fn from(status: CharacterMatchStatus) -> Self {
        match status {
            CharacterMatchStatus::Idle => Self::Idle,
            CharacterMatchStatus::Matching => Self::Matching,
            CharacterMatchStatus::Matched => Self::Matched,
            CharacterMatchStatus::InRoom => Self::InRoom,
        }
    }
}

impl From<StoredCharacterMatchStatus> for CharacterMatchStatus {
    fn from(status: StoredCharacterMatchStatus) -> Self {
        match status {
            StoredCharacterMatchStatus::Idle => Self::Idle,
            StoredCharacterMatchStatus::Matching => Self::Matching,
            StoredCharacterMatchStatus::Matched => Self::Matched,
            StoredCharacterMatchStatus::InRoom => Self::InRoom,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCharacterMatchContext {
    pub match_id: String,
    pub mode: String,
    #[serde(default)]
    pub room_id: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
}

impl From<CharacterMatchContext> for StoredCharacterMatchContext {
    fn from(ctx: CharacterMatchContext) -> Self {
        Self {
            match_id: ctx.match_id,
            mode: ctx.mode,
            room_id: ctx.room_id,
            token: ctx.token,
        }
    }
}

impl From<StoredCharacterMatchContext> for CharacterMatchContext {
    fn from(ctx: StoredCharacterMatchContext) -> Self {
        Self {
            match_id: ctx.match_id,
            mode: ctx.mode,
            room_id: ctx.room_id,
            token: ctx.token,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMatchEvent {
    pub event: String,
    pub match_id: String,
    pub room_id: String,
    pub token: String,
    pub error_code: String,
    pub created_at_ms: u64,
}

impl StoredMatchEvent {
    pub fn new(event: MatchEvent) -> Self {
        Self {
            event: event.event,
            match_id: event.match_id,
            room_id: event.room_id,
            token: event.token,
            error_code: event.error_code,
            created_at_ms: now_ms(),
        }
    }

    pub fn into_event(self) -> MatchEvent {
        MatchEvent {
            event: self.event,
            match_id: self.match_id,
            room_id: self.room_id,
            token: self.token,
            error_code: self.error_code,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMatchCandidate {
    pub character_id: String,
    pub match_id: String,
    pub mode: String,
    pub created_at_ms: u64,
    pub timeout_at_ms: u64,
}

impl StoredMatchCandidate {
    pub fn from_candidate(candidate: &MatchCandidate) -> Self {
        let now_instant = std::time::Instant::now();
        let now_wall_clock = now_ms();
        let timeout_at_ms = if candidate.timeout_at > now_instant {
            instant_deadline_ms(candidate.timeout_at.duration_since(now_instant))
        } else {
            now_wall_clock
        };
        let created_at_ms =
            now_wall_clock.saturating_sub(candidate.created_at.elapsed().as_millis() as u64);

        Self {
            character_id: candidate.character_id.clone(),
            match_id: candidate.match_id.clone(),
            mode: candidate.mode.clone(),
            created_at_ms,
            timeout_at_ms,
        }
    }

    pub fn into_candidate(self) -> MatchCandidate {
        let now_instant = std::time::Instant::now();
        let elapsed_since_created =
            Duration::from_millis(now_ms().saturating_sub(self.created_at_ms));
        let created_at = now_instant
            .checked_sub(elapsed_since_created)
            .unwrap_or(now_instant);
        MatchCandidate {
            character_id: self.character_id,
            match_id: self.match_id,
            mode: self.mode,
            created_at,
            timeout_at: now_instant + duration_since_now_ms(self.timeout_at_ms),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredMatchTask {
    pub match_id: String,
    pub mode: String,
    pub character_ids: Vec<String>,
    #[serde(default)]
    pub room_id: Option<String>,
    #[serde(default)]
    pub joined_characters: HashSet<String>,
    #[serde(default)]
    pub active_characters: HashSet<String>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

impl StoredMatchTask {
    pub fn from_task(task: &MatchTask) -> Self {
        let timestamp = now_ms();
        Self {
            match_id: task.match_id.clone(),
            mode: task.mode.clone(),
            character_ids: task.character_ids.clone(),
            room_id: task.room_id.clone(),
            joined_characters: task.joined_characters.clone(),
            active_characters: task.active_characters.clone(),
            created_at_ms: timestamp,
            updated_at_ms: timestamp,
        }
    }

    pub fn into_task(self) -> MatchTask {
        MatchTask {
            match_id: self.match_id,
            mode: self.mode,
            character_ids: self.character_ids,
            room_id: self.room_id,
            joined_characters: self.joined_characters,
            active_characters: self.active_characters,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchRuntimeLease {
    pub scope: String,
    pub owner_instance_id: String,
    pub expires_at_ms: u64,
}

impl MatchRuntimeLease {
    fn is_expired(&self) -> bool {
        self.expires_at_ms <= now_ms()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchRuntimeSnapshot {
    #[serde(default)]
    pub candidates_by_mode: HashMap<String, Vec<StoredMatchCandidate>>,
    #[serde(default)]
    pub matches: HashMap<String, StoredMatchTask>,
    #[serde(default)]
    pub character_status: HashMap<String, StoredCharacterMatchStatus>,
    #[serde(default)]
    pub character_context: HashMap<String, StoredCharacterMatchContext>,
    #[serde(default)]
    pub latest_events: HashMap<String, StoredMatchEvent>,
    #[serde(default)]
    pub leases: HashMap<String, MatchRuntimeLease>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseAcquireResult {
    Acquired,
    AlreadyOwned,
    Busy { owner_instance_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MatchRuntimeStoreError {
    Persistence(String),
    Json(String),
}

impl fmt::Display for MatchRuntimeStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Persistence(message) => write!(formatter, "match runtime store error: {message}"),
            Self::Json(message) => write!(formatter, "match runtime store json error: {message}"),
        }
    }
}

impl std::error::Error for MatchRuntimeStoreError {}

impl From<serde_json::Error> for MatchRuntimeStoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error.to_string())
    }
}

type StoreFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, MatchRuntimeStoreError>> + Send + 'a>>;

#[async_trait]
pub trait MatchRuntimeStore: Send + Sync {
    async fn load_snapshot(&self) -> Result<MatchRuntimeSnapshot, MatchRuntimeStoreError>;
    async fn save_candidate(
        &self,
        candidate: StoredMatchCandidate,
    ) -> Result<(), MatchRuntimeStoreError>;
    async fn remove_candidate(
        &self,
        character_id: &str,
        mode: &str,
    ) -> Result<(), MatchRuntimeStoreError>;
    async fn save_match_task(&self, task: StoredMatchTask) -> Result<(), MatchRuntimeStoreError>;
    async fn remove_match_task(&self, match_id: &str) -> Result<(), MatchRuntimeStoreError>;
    async fn set_character_status(
        &self,
        character_id: &str,
        status: StoredCharacterMatchStatus,
    ) -> Result<(), MatchRuntimeStoreError>;
    async fn set_character_context(
        &self,
        character_id: &str,
        context: StoredCharacterMatchContext,
    ) -> Result<(), MatchRuntimeStoreError>;
    async fn clear_character_context(
        &self,
        character_id: &str,
    ) -> Result<(), MatchRuntimeStoreError>;
    async fn save_latest_event(
        &self,
        character_id: &str,
        event: StoredMatchEvent,
    ) -> Result<(), MatchRuntimeStoreError>;
    async fn acquire_lease(
        &self,
        scope: &str,
        owner_instance_id: &str,
        ttl: Duration,
    ) -> Result<LeaseAcquireResult, MatchRuntimeStoreError>;
    async fn release_lease(
        &self,
        scope: &str,
        owner_instance_id: &str,
    ) -> Result<(), MatchRuntimeStoreError>;
}

#[derive(Default)]
pub struct MemoryMatchRuntimeStore {
    snapshot: RwLock<MatchRuntimeSnapshot>,
}

impl MemoryMatchRuntimeStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MatchRuntimeStore for MemoryMatchRuntimeStore {
    async fn load_snapshot(&self) -> Result<MatchRuntimeSnapshot, MatchRuntimeStoreError> {
        let mut snapshot = self.snapshot.write().await;
        snapshot.leases.retain(|_, lease| !lease.is_expired());
        Ok(snapshot.clone())
    }

    async fn save_candidate(
        &self,
        candidate: StoredMatchCandidate,
    ) -> Result<(), MatchRuntimeStoreError> {
        let mut snapshot = self.snapshot.write().await;
        let candidates = snapshot
            .candidates_by_mode
            .entry(candidate.mode.clone())
            .or_default();
        candidates.retain(|existing| existing.character_id != candidate.character_id);
        candidates.push(candidate);
        Ok(())
    }

    async fn remove_candidate(
        &self,
        character_id: &str,
        mode: &str,
    ) -> Result<(), MatchRuntimeStoreError> {
        let mut snapshot = self.snapshot.write().await;
        if let Some(candidates) = snapshot.candidates_by_mode.get_mut(mode) {
            candidates.retain(|candidate| candidate.character_id != character_id);
            if candidates.is_empty() {
                snapshot.candidates_by_mode.remove(mode);
            }
        }
        Ok(())
    }

    async fn save_match_task(&self, task: StoredMatchTask) -> Result<(), MatchRuntimeStoreError> {
        self.snapshot
            .write()
            .await
            .matches
            .insert(task.match_id.clone(), task);
        Ok(())
    }

    async fn remove_match_task(&self, match_id: &str) -> Result<(), MatchRuntimeStoreError> {
        self.snapshot.write().await.matches.remove(match_id);
        Ok(())
    }

    async fn set_character_status(
        &self,
        character_id: &str,
        status: StoredCharacterMatchStatus,
    ) -> Result<(), MatchRuntimeStoreError> {
        self.snapshot
            .write()
            .await
            .character_status
            .insert(character_id.to_string(), status);
        Ok(())
    }

    async fn set_character_context(
        &self,
        character_id: &str,
        context: StoredCharacterMatchContext,
    ) -> Result<(), MatchRuntimeStoreError> {
        self.snapshot
            .write()
            .await
            .character_context
            .insert(character_id.to_string(), context);
        Ok(())
    }

    async fn clear_character_context(
        &self,
        character_id: &str,
    ) -> Result<(), MatchRuntimeStoreError> {
        let mut snapshot = self.snapshot.write().await;
        snapshot.character_context.remove(character_id);
        snapshot
            .character_status
            .insert(character_id.to_string(), StoredCharacterMatchStatus::Idle);
        Ok(())
    }

    async fn save_latest_event(
        &self,
        character_id: &str,
        event: StoredMatchEvent,
    ) -> Result<(), MatchRuntimeStoreError> {
        self.snapshot
            .write()
            .await
            .latest_events
            .insert(character_id.to_string(), event);
        Ok(())
    }

    async fn acquire_lease(
        &self,
        scope: &str,
        owner_instance_id: &str,
        ttl: Duration,
    ) -> Result<LeaseAcquireResult, MatchRuntimeStoreError> {
        let mut snapshot = self.snapshot.write().await;
        let result = match snapshot.leases.get(scope) {
            Some(lease) if !lease.is_expired() && lease.owner_instance_id == owner_instance_id => {
                snapshot.leases.insert(
                    scope.to_string(),
                    MatchRuntimeLease {
                        scope: scope.to_string(),
                        owner_instance_id: owner_instance_id.to_string(),
                        expires_at_ms: lease_expires_at_ms(ttl),
                    },
                );
                LeaseAcquireResult::AlreadyOwned
            }
            Some(lease) if !lease.is_expired() => LeaseAcquireResult::Busy {
                owner_instance_id: lease.owner_instance_id.clone(),
            },
            _ => {
                snapshot.leases.insert(
                    scope.to_string(),
                    MatchRuntimeLease {
                        scope: scope.to_string(),
                        owner_instance_id: owner_instance_id.to_string(),
                        expires_at_ms: lease_expires_at_ms(ttl),
                    },
                );
                LeaseAcquireResult::Acquired
            }
        };
        Ok(result)
    }

    async fn release_lease(
        &self,
        scope: &str,
        owner_instance_id: &str,
    ) -> Result<(), MatchRuntimeStoreError> {
        let mut snapshot = self.snapshot.write().await;
        let should_remove = snapshot
            .leases
            .get(scope)
            .map(|lease| lease.owner_instance_id == owner_instance_id)
            .unwrap_or(false);
        if should_remove {
            snapshot.leases.remove(scope);
        }
        Ok(())
    }
}

pub struct RedisMatchRuntimeStore {
    pool: deadpool_redis::Pool,
    state_key: String,
    lease_prefix: String,
    update_lock: tokio::sync::Mutex<()>,
}

const REDIS_MATCH_RUNTIME_CAS_SCRIPT: &str = r#"
local current = redis.call("GET", KEYS[1])
local state = {}
if current then
    state = cjson.decode(current)
end
local value = cjson.decode(ARGV[1])
local op = ARGV[2]

if op == "save_candidate" then
    local mode = value["mode"]
    state["candidates_by_mode"] = state["candidates_by_mode"] or {}
    state["candidates_by_mode"][mode] = state["candidates_by_mode"][mode] or {}
    local next = {}
    for _, candidate in ipairs(state["candidates_by_mode"][mode]) do
        if candidate["character_id"] ~= value["character_id"] then
            table.insert(next, candidate)
        end
    end
    table.insert(next, value)
    state["candidates_by_mode"][mode] = next
elseif op == "remove_candidate" then
    local mode = value["mode"]
    state["candidates_by_mode"] = state["candidates_by_mode"] or {}
    local current_candidates = state["candidates_by_mode"][mode] or {}
    local next = {}
    for _, candidate in ipairs(current_candidates) do
        if candidate["character_id"] ~= value["character_id"] then
            table.insert(next, candidate)
        end
    end
    if #next == 0 then
        state["candidates_by_mode"][mode] = nil
    else
        state["candidates_by_mode"][mode] = next
    end
elseif op == "save_match_task" then
    state["matches"] = state["matches"] or {}
    state["matches"][value["match_id"]] = value
elseif op == "remove_match_task" then
    state["matches"] = state["matches"] or {}
    state["matches"][value["match_id"]] = nil
elseif op == "set_character_status" then
    state["character_status"] = state["character_status"] or {}
    state["character_status"][value["character_id"]] = value["status"]
elseif op == "set_character_context" then
    state["character_context"] = state["character_context"] or {}
    state["character_context"][value["character_id"]] = value["context"]
elseif op == "clear_character_context" then
    state["character_context"] = state["character_context"] or {}
    state["character_status"] = state["character_status"] or {}
    state["character_context"][value["character_id"]] = nil
    state["character_status"][value["character_id"]] = "Idle"
elseif op == "save_latest_event" then
    state["latest_events"] = state["latest_events"] or {}
    state["latest_events"][value["character_id"]] = value["event"]
end

redis.call("SET", KEYS[1], cjson.encode(state))
return 1
"#;

impl RedisMatchRuntimeStore {
    pub fn new(
        redis_url: &str,
        key_prefix: impl Into<String>,
    ) -> Result<Self, MatchRuntimeStoreError> {
        let redis_config = deadpool_redis::Config::from_url(redis_url);
        let pool = redis_config
            .create_pool(Some(deadpool_redis::Runtime::Tokio1))
            .map_err(|error| MatchRuntimeStoreError::Persistence(error.to_string()))?;
        Self::from_pool(pool, key_prefix)
    }

    pub fn from_pool(
        pool: deadpool_redis::Pool,
        key_prefix: impl Into<String>,
    ) -> Result<Self, MatchRuntimeStoreError> {
        let key_prefix = key_prefix.into();
        Ok(Self {
            pool,
            state_key: format!("{key_prefix}match-service:runtime:state"),
            lease_prefix: format!("{key_prefix}match-service:runtime:lease:"),
            update_lock: tokio::sync::Mutex::new(()),
        })
    }

    fn lease_key(&self, scope: &str) -> String {
        format!("{}{}", self.lease_prefix, scope)
    }

    fn update_state<'a>(
        &'a self,
        op: &'static str,
        value: serde_json::Value,
    ) -> StoreFuture<'a, ()> {
        Box::pin(async move {
            let _guard = self.update_lock.lock().await;
            let payload = serde_json::to_string(&value)?;
            let mut conn = self
                .pool
                .get()
                .await
                .map_err(|error| MatchRuntimeStoreError::Persistence(error.to_string()))?;
            let _: i32 = deadpool_redis::redis::cmd("EVAL")
                .arg(REDIS_MATCH_RUNTIME_CAS_SCRIPT)
                .arg(1)
                .arg(&self.state_key)
                .arg(payload)
                .arg(op)
                .query_async(&mut conn)
                .await
                .map_err(|error| MatchRuntimeStoreError::Persistence(error.to_string()))?;
            Ok(())
        })
    }
}

#[async_trait]
impl MatchRuntimeStore for RedisMatchRuntimeStore {
    async fn load_snapshot(&self) -> Result<MatchRuntimeSnapshot, MatchRuntimeStoreError> {
        use deadpool_redis::redis::AsyncCommands;

        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|error| MatchRuntimeStoreError::Persistence(error.to_string()))?;
        let json: Option<String> = conn
            .get(&self.state_key)
            .await
            .map_err(|error| MatchRuntimeStoreError::Persistence(error.to_string()))?;
        match json {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(MatchRuntimeSnapshot::default()),
        }
    }

    async fn save_candidate(
        &self,
        candidate: StoredMatchCandidate,
    ) -> Result<(), MatchRuntimeStoreError> {
        self.update_state("save_candidate", serde_json::to_value(candidate)?)
            .await
    }

    async fn remove_candidate(
        &self,
        character_id: &str,
        mode: &str,
    ) -> Result<(), MatchRuntimeStoreError> {
        self.update_state(
            "remove_candidate",
            serde_json::json!({
                "character_id": character_id,
                "mode": mode,
            }),
        )
        .await
    }

    async fn save_match_task(&self, task: StoredMatchTask) -> Result<(), MatchRuntimeStoreError> {
        self.update_state("save_match_task", serde_json::to_value(task)?)
            .await
    }

    async fn remove_match_task(&self, match_id: &str) -> Result<(), MatchRuntimeStoreError> {
        self.update_state(
            "remove_match_task",
            serde_json::json!({
                "match_id": match_id,
            }),
        )
        .await
    }

    async fn set_character_status(
        &self,
        character_id: &str,
        status: StoredCharacterMatchStatus,
    ) -> Result<(), MatchRuntimeStoreError> {
        self.update_state(
            "set_character_status",
            serde_json::json!({
                "character_id": character_id,
                "status": status,
            }),
        )
        .await
    }

    async fn set_character_context(
        &self,
        character_id: &str,
        context: StoredCharacterMatchContext,
    ) -> Result<(), MatchRuntimeStoreError> {
        self.update_state(
            "set_character_context",
            serde_json::json!({
                "character_id": character_id,
                "context": context,
            }),
        )
        .await
    }

    async fn clear_character_context(
        &self,
        character_id: &str,
    ) -> Result<(), MatchRuntimeStoreError> {
        self.update_state(
            "clear_character_context",
            serde_json::json!({
                "character_id": character_id,
            }),
        )
        .await
    }

    async fn save_latest_event(
        &self,
        character_id: &str,
        event: StoredMatchEvent,
    ) -> Result<(), MatchRuntimeStoreError> {
        self.update_state(
            "save_latest_event",
            serde_json::json!({
                "character_id": character_id,
                "event": event,
            }),
        )
        .await
    }

    async fn acquire_lease(
        &self,
        scope: &str,
        owner_instance_id: &str,
        ttl: Duration,
    ) -> Result<LeaseAcquireResult, MatchRuntimeStoreError> {
        use deadpool_redis::redis::AsyncCommands;

        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|error| MatchRuntimeStoreError::Persistence(error.to_string()))?;
        let key = self.lease_key(scope);
        let ttl_secs = ttl.as_secs().max(1);
        let existing: Option<String> = conn
            .get(&key)
            .await
            .map_err(|error| MatchRuntimeStoreError::Persistence(error.to_string()))?;
        if let Some(existing_owner) = existing {
            if existing_owner == owner_instance_id {
                let _: () = conn
                    .expire(&key, ttl_secs as i64)
                    .await
                    .map_err(|error| MatchRuntimeStoreError::Persistence(error.to_string()))?;
                return Ok(LeaseAcquireResult::AlreadyOwned);
            }
            return Ok(LeaseAcquireResult::Busy {
                owner_instance_id: existing_owner,
            });
        }

        let acquired: Option<String> = deadpool_redis::redis::cmd("SET")
            .arg(&key)
            .arg(owner_instance_id)
            .arg("NX")
            .arg("EX")
            .arg(ttl_secs)
            .query_async(&mut conn)
            .await
            .map_err(|error| MatchRuntimeStoreError::Persistence(error.to_string()))?;
        if acquired.is_some() {
            Ok(LeaseAcquireResult::Acquired)
        } else {
            let owner: Option<String> = conn
                .get(&key)
                .await
                .map_err(|error| MatchRuntimeStoreError::Persistence(error.to_string()))?;
            Ok(LeaseAcquireResult::Busy {
                owner_instance_id: owner.unwrap_or_default(),
            })
        }
    }

    async fn release_lease(
        &self,
        scope: &str,
        owner_instance_id: &str,
    ) -> Result<(), MatchRuntimeStoreError> {
        let key = self.lease_key(scope);
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|error| MatchRuntimeStoreError::Persistence(error.to_string()))?;
        let _: i32 = deadpool_redis::redis::cmd("EVAL")
            .arg(
                r#"
if redis.call("GET", KEYS[1]) == ARGV[1] then
    return redis.call("DEL", KEYS[1])
end
return 0
"#,
            )
            .arg(1)
            .arg(key)
            .arg(owner_instance_id)
            .query_async(&mut conn)
            .await
            .map_err(|error| MatchRuntimeStoreError::Persistence(error.to_string()))?;
        Ok(())
    }
}

pub type SharedMatchRuntimeStore = Arc<dyn MatchRuntimeStore>;

pub fn new_memory_match_runtime_store() -> SharedMatchRuntimeStore {
    Arc::new(MemoryMatchRuntimeStore::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn memory_store_acquires_single_owner_per_scope() {
        let store = MemoryMatchRuntimeStore::new();

        let first = store
            .acquire_lease("mode:1v1", "instance-a", Duration::from_secs(30))
            .await
            .unwrap();
        let repeated = store
            .acquire_lease("mode:1v1", "instance-a", Duration::from_secs(30))
            .await
            .unwrap();
        let competing = store
            .acquire_lease("mode:1v1", "instance-b", Duration::from_secs(30))
            .await
            .unwrap();

        assert_eq!(first, LeaseAcquireResult::Acquired);
        assert_eq!(repeated, LeaseAcquireResult::AlreadyOwned);
        assert_eq!(
            competing,
            LeaseAcquireResult::Busy {
                owner_instance_id: "instance-a".to_string()
            }
        );
    }

    #[tokio::test]
    async fn memory_store_snapshot_keeps_recoverable_state() {
        let store = MemoryMatchRuntimeStore::new();

        store
            .set_character_status("chr-a", StoredCharacterMatchStatus::Matching)
            .await
            .unwrap();
        store
            .set_character_context(
                "chr-a",
                StoredCharacterMatchContext {
                    match_id: "match-a".to_string(),
                    mode: "1v1".to_string(),
                    room_id: None,
                    token: None,
                },
            )
            .await
            .unwrap();
        store
            .save_latest_event(
                "chr-a",
                StoredMatchEvent::new(MatchEvent {
                    event: "match_failed".to_string(),
                    match_id: "match-a".to_string(),
                    room_id: String::new(),
                    token: String::new(),
                    error_code: "MATCH_TIMEOUT".to_string(),
                }),
            )
            .await
            .unwrap();

        let snapshot = store.load_snapshot().await.unwrap();

        assert_eq!(
            snapshot.character_status.get("chr-a"),
            Some(&StoredCharacterMatchStatus::Matching)
        );
        assert_eq!(
            snapshot
                .character_context
                .get("chr-a")
                .map(|ctx| ctx.match_id.as_str()),
            Some("match-a")
        );
        assert_eq!(
            snapshot
                .latest_events
                .get("chr-a")
                .map(|event| event.error_code.as_str()),
            Some("MATCH_TIMEOUT")
        );
    }
}
