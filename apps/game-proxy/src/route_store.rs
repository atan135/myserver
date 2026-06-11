use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::StreamExt;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpstreamOperationState {
    Active,
    Draining,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpstreamHealthState {
    Healthy,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum UpstreamState {
    Active,
    Draining,
    Disabled,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpstreamRoute {
    pub server_id: String,
    pub local_socket_name: String,
    pub operation_state: UpstreamOperationState,
    pub health_state: UpstreamHealthState,
}

impl UpstreamRoute {
    pub fn effective_state(&self) -> UpstreamState {
        match (self.health_state, self.operation_state) {
            (UpstreamHealthState::Unavailable, _) => UpstreamState::Unavailable,
            (UpstreamHealthState::Healthy, UpstreamOperationState::Active) => UpstreamState::Active,
            (UpstreamHealthState::Healthy, UpstreamOperationState::Draining) => {
                UpstreamState::Draining
            }
            (UpstreamHealthState::Healthy, UpstreamOperationState::Disabled) => {
                UpstreamState::Disabled
            }
        }
    }

    pub fn can_accept_bound_sessions(&self) -> bool {
        matches!(
            self.effective_state(),
            UpstreamState::Active | UpstreamState::Draining
        )
    }

    pub fn accepts_new_rooms(&self) -> bool {
        self.effective_state() == UpstreamState::Active
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RolloutSessionState {
    Active,
    Ending,
    Interrupted,
}

impl RolloutSessionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "Active",
            Self::Ending => "Ending",
            Self::Interrupted => "Interrupted",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RolloutSession {
    pub rollout_epoch: String,
    pub old_server_id: String,
    pub new_server_id: String,
    pub state: RolloutSessionState,
    pub started_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomMigrationState {
    OwnedByOld,
    DrainingOnOld,
    FrozenForTransfer,
    ImportingToNew,
    OwnedByNew,
    TransferFailed,
    RetiredOnOld,
}

impl RoomMigrationState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OwnedByOld => "OwnedByOld",
            Self::DrainingOnOld => "DrainingOnOld",
            Self::FrozenForTransfer => "FrozenForTransfer",
            Self::ImportingToNew => "ImportingToNew",
            Self::OwnedByNew => "OwnedByNew",
            Self::TransferFailed => "TransferFailed",
            Self::RetiredOnOld => "RetiredOnOld",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "OwnedByOld" => Some(Self::OwnedByOld),
            "DrainingOnOld" => Some(Self::DrainingOnOld),
            "FrozenForTransfer" => Some(Self::FrozenForTransfer),
            "ImportingToNew" => Some(Self::ImportingToNew),
            "OwnedByNew" => Some(Self::OwnedByNew),
            "TransferFailed" => Some(Self::TransferFailed),
            "RetiredOnOld" => Some(Self::RetiredOnOld),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoomRouteRecord {
    pub room_id: String,
    pub owner_server_id: String,
    pub migration_state: RoomMigrationState,
    pub member_count: u32,
    pub online_member_count: u32,
    pub empty_since_ms: Option<u64>,
    pub room_version: u64,
    pub rollout_epoch: String,
    pub last_transfer_checksum: String,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlayerRouteRecord {
    pub player_id: String,
    pub current_room_id: Option<String>,
    pub preferred_server_id: Option<String>,
    pub rollout_epoch: String,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug)]
pub struct RouteCounts {
    pub room_routes: usize,
    pub player_routes: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RolloutDrainStatus {
    NoActiveRollout,
    Blocked,
    Drained,
}

impl RolloutDrainStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoActiveRollout => "NoActiveRollout",
            Self::Blocked => "Blocked",
            Self::Drained => "Drained",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutDrainEvaluation {
    pub status: RolloutDrainStatus,
    pub rollout_epoch: Option<String>,
    pub old_server_id: Option<String>,
    pub new_server_id: Option<String>,
    pub blocked_room_count: usize,
    pub blocked_player_count: usize,
    pub blocked_room_samples: Vec<String>,
    pub blocked_player_samples: Vec<String>,
    pub stale_room_route_count: usize,
    pub stale_player_route_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutEndSummary {
    pub rollout_epoch: Option<String>,
    pub old_server_id: Option<String>,
    pub new_server_id: Option<String>,
    pub removed_room_route_count: usize,
    pub removed_player_route_count: usize,
    pub remaining_room_route_count: usize,
    pub remaining_player_route_count: usize,
}

impl RolloutEndSummary {
    fn has_changes(&self) -> bool {
        self.rollout_epoch.is_some()
            || self.removed_room_route_count > 0
            || self.removed_player_route_count > 0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RolloutCompleteIfDrainedResult {
    NoActiveRollout {
        evaluation: RolloutDrainEvaluation,
    },
    Blocked {
        evaluation: RolloutDrainEvaluation,
    },
    Completed {
        evaluation: RolloutDrainEvaluation,
        end_summary: RolloutEndSummary,
    },
}

#[derive(Clone, Default)]
struct RouteStoreState {
    store_revision: u64,
    routes: HashMap<String, UpstreamRoute>,
    rollout_session: Option<RolloutSession>,
    room_routes: HashMap<String, RoomRouteRecord>,
    player_routes: HashMap<String, PlayerRouteRecord>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PersistedRouteStoreState {
    #[serde(default)]
    pub store_revision: u64,
    #[serde(default)]
    pub rollout_session: Option<RolloutSession>,
    #[serde(default)]
    pub room_routes: HashMap<String, RoomRouteRecord>,
    #[serde(default)]
    pub player_routes: HashMap<String, PlayerRouteRecord>,
}

#[derive(Debug)]
pub enum RouteStorePersistenceError {
    Redis(redis::RedisError),
    Json(serde_json::Error),
    RevisionConflict {
        expected_revision: u64,
        actual_revision: u64,
    },
}

impl fmt::Display for RouteStorePersistenceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Redis(error) => write!(formatter, "redis route store error: {error}"),
            Self::Json(error) => write!(formatter, "route store json error: {error}"),
            Self::RevisionConflict {
                expected_revision,
                actual_revision,
            } => write!(
                formatter,
                "route store revision conflict: expected {expected_revision}, actual {actual_revision}"
            ),
        }
    }
}

impl Error for RouteStorePersistenceError {}

impl From<redis::RedisError> for RouteStorePersistenceError {
    fn from(error: redis::RedisError) -> Self {
        Self::Redis(error)
    }
}

impl From<serde_json::Error> for RouteStorePersistenceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug)]
pub enum RouteStoreUpdateError {
    Validation(&'static str),
    Persistence(RouteStorePersistenceError),
}

impl RouteStoreUpdateError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Validation(code) => code,
            Self::Persistence(RouteStorePersistenceError::RevisionConflict { .. }) => {
                "ROUTE_STORE_REVISION_CONFLICT"
            }
            Self::Persistence(_) => "ROUTE_STORE_PERSISTENCE_ERROR",
        }
    }
}

impl fmt::Display for RouteStoreUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(code) => formatter.write_str(code),
            Self::Persistence(error) => write!(formatter, "{error}"),
        }
    }
}

impl Error for RouteStoreUpdateError {}

impl From<&'static str> for RouteStoreUpdateError {
    fn from(code: &'static str) -> Self {
        Self::Validation(code)
    }
}

impl From<RouteStorePersistenceError> for RouteStoreUpdateError {
    fn from(error: RouteStorePersistenceError) -> Self {
        Self::Persistence(error)
    }
}

type PersistenceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, RouteStorePersistenceError>> + Send + 'a>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteStoreSaveResult {
    Saved,
    RevisionConflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteStoreReloadResult {
    Reloaded {
        previous_revision: u64,
        new_revision: u64,
    },
    IgnoredStale {
        current_revision: u64,
        notified_revision: u64,
    },
}

pub trait RouteStorePersistence: Send + Sync {
    fn load<'a>(&'a self) -> PersistenceFuture<'a, PersistedRouteStoreState>;
    fn save<'a>(
        &'a self,
        expected_revision: u64,
        state: PersistedRouteStoreState,
    ) -> PersistenceFuture<'a, RouteStoreSaveResult>;
    fn publish_update<'a>(&'a self, _store_revision: u64) -> PersistenceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

pub struct RedisRouteStorePersistence {
    client: redis::Client,
    state_key: String,
    update_channel: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RouteStoreUpdateNotification {
    pub store_revision: u64,
}

const ROLLOUT_DRAIN_SAMPLE_LIMIT: usize = 5;

const REDIS_ROUTE_STORE_CAS_SCRIPT: &str = r#"
local current = redis.call("GET", KEYS[1])
local expected_revision = tonumber(ARGV[1])
if current then
    local state = cjson.decode(current)
    local current_revision = tonumber(state["store_revision"] or 0)
    if current_revision ~= expected_revision then
        return 0
    end
else
    if expected_revision ~= 0 then
        return 0
    end
end
redis.call("SET", KEYS[1], ARGV[2])
return 1
"#;

impl RedisRouteStorePersistence {
    pub fn new(redis_url: &str, key_prefix: impl Into<String>) -> Result<Self, redis::RedisError> {
        let key_prefix = key_prefix.into();
        Ok(Self {
            client: redis::Client::open(redis_url)?,
            state_key: format!("{key_prefix}proxy:route-store:state"),
            update_channel: route_store_update_channel(&key_prefix),
        })
    }

    pub fn update_channel(&self) -> &str {
        &self.update_channel
    }

    pub async fn subscribe_updates(
        &self,
    ) -> Result<redis::aio::PubSub, RouteStorePersistenceError> {
        let mut pubsub = self.client.get_async_pubsub().await?;
        pubsub.subscribe(&self.update_channel).await?;
        Ok(pubsub)
    }
}

impl RouteStorePersistence for RedisRouteStorePersistence {
    fn load<'a>(&'a self) -> PersistenceFuture<'a, PersistedRouteStoreState> {
        Box::pin(async move {
            let mut conn = self.client.get_multiplexed_async_connection().await?;
            let json: Option<String> = conn.get(&self.state_key).await?;
            match json {
                Some(json) => Ok(serde_json::from_str(&json)?),
                None => Ok(PersistedRouteStoreState::default()),
            }
        })
    }

    fn save<'a>(
        &'a self,
        expected_revision: u64,
        state: PersistedRouteStoreState,
    ) -> PersistenceFuture<'a, RouteStoreSaveResult> {
        Box::pin(async move {
            let json = serde_json::to_string(&state)?;
            let mut conn = self.client.get_multiplexed_async_connection().await?;
            let saved: i32 = redis::Script::new(REDIS_ROUTE_STORE_CAS_SCRIPT)
                .key(&self.state_key)
                .arg(expected_revision)
                .arg(json)
                .invoke_async(&mut conn)
                .await?;
            if saved == 1 {
                Ok(RouteStoreSaveResult::Saved)
            } else {
                Ok(RouteStoreSaveResult::RevisionConflict)
            }
        })
    }

    fn publish_update<'a>(&'a self, store_revision: u64) -> PersistenceFuture<'a, ()> {
        Box::pin(async move {
            let notification = RouteStoreUpdateNotification { store_revision };
            let payload = serde_json::to_string(&notification)?;
            let mut conn = self.client.get_multiplexed_async_connection().await?;
            let _: usize = conn.publish(&self.update_channel, payload).await?;
            Ok(())
        })
    }
}

#[derive(Clone, Default)]
pub struct ProxyRouteStore {
    state: Arc<RwLock<RouteStoreState>>,
    persistence: Option<Arc<dyn RouteStorePersistence>>,
    persist_lock: Arc<tokio::sync::Mutex<()>>,
}

impl ProxyRouteStore {
    pub fn with_persistence(persistence: Arc<dyn RouteStorePersistence>) -> Self {
        Self {
            state: Arc::new(RwLock::new(RouteStoreState::default())),
            persistence: Some(persistence),
            persist_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub async fn load_persisted_state(&self) -> Result<(), RouteStorePersistenceError> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(());
        };
        let _guard = self.persist_lock.lock().await;
        let persisted = persistence.load().await?;
        let store_revision = persisted.store_revision;
        let mut state = self.state.write().await;
        state.apply_persisted(persisted);
        info!(store_revision, "proxy route store loaded persisted state");
        Ok(())
    }

    async fn update_persisted_state<F>(
        &self,
        update_source: &'static str,
        update: F,
    ) -> Result<bool, RouteStoreUpdateError>
    where
        F: FnOnce(&mut RouteStoreState) -> Result<bool, RouteStoreUpdateError>,
    {
        let Some(persistence) = self.persistence.as_ref() else {
            let mut state = self.state.write().await;
            return update(&mut state);
        };

        let _guard = self.persist_lock.lock().await;
        let (expected_revision, next_snapshot) = {
            let state = self.state.read().await;
            let mut candidate = state.clone();
            drop(state);

            if !update(&mut candidate)? {
                let expected_revision = candidate.store_revision;
                let latest = persistence.load().await?;
                let actual_revision = latest.store_revision;
                if actual_revision != expected_revision {
                    let mut state = self.state.write().await;
                    state.apply_persisted(latest);
                    warn!(
                        update_source,
                        expected_revision,
                        actual_revision,
                        "proxy route store revision conflict on no-op update; reloaded persisted state"
                    );
                    return Err(RouteStorePersistenceError::RevisionConflict {
                        expected_revision,
                        actual_revision,
                    }
                    .into());
                }
                return Ok(false);
            }

            let expected_revision = candidate.store_revision;
            candidate.store_revision = candidate.store_revision.saturating_add(1);
            (expected_revision, candidate.persisted_snapshot())
        };

        match persistence
            .save(expected_revision, next_snapshot.clone())
            .await?
        {
            RouteStoreSaveResult::Saved => {
                let mut state = self.state.write().await;
                state.apply_persisted(next_snapshot);
                drop(state);
                drop(_guard);
                if let Err(error) = persistence
                    .publish_update(expected_revision.saturating_add(1))
                    .await
                {
                    warn!(
                        update_source,
                        store_revision = expected_revision.saturating_add(1),
                        error = %error,
                        "failed to publish proxy route store update notification"
                    );
                }
                Ok(true)
            }
            RouteStoreSaveResult::RevisionConflict => {
                let actual_revision = self
                    .reload_after_revision_conflict(
                        persistence.as_ref(),
                        update_source,
                        expected_revision,
                    )
                    .await?;
                Err(RouteStorePersistenceError::RevisionConflict {
                    expected_revision,
                    actual_revision,
                }
                .into())
            }
        }
    }

    async fn reload_after_revision_conflict(
        &self,
        persistence: &dyn RouteStorePersistence,
        update_source: &'static str,
        expected_revision: u64,
    ) -> Result<u64, RouteStorePersistenceError> {
        let persisted = persistence.load().await?;
        let actual_revision = persisted.store_revision;
        let mut state = self.state.write().await;
        state.apply_persisted(persisted);
        warn!(
            update_source,
            expected_revision,
            actual_revision,
            "proxy route store revision conflict; reloaded persisted state"
        );
        Ok(actual_revision)
    }

    pub async fn reload_if_newer_revision(
        &self,
        notified_revision: u64,
    ) -> Result<RouteStoreReloadResult, RouteStorePersistenceError> {
        let Some(persistence) = self.persistence.as_ref() else {
            return Ok(RouteStoreReloadResult::IgnoredStale {
                current_revision: 0,
                notified_revision,
            });
        };

        let _guard = self.persist_lock.lock().await;
        let current_revision = self.state.read().await.store_revision;
        if notified_revision <= current_revision {
            return Ok(RouteStoreReloadResult::IgnoredStale {
                current_revision,
                notified_revision,
            });
        }

        let persisted = persistence.load().await?;
        let new_revision = persisted.store_revision;
        if new_revision <= current_revision {
            return Ok(RouteStoreReloadResult::IgnoredStale {
                current_revision,
                notified_revision,
            });
        }

        let mut state = self.state.write().await;
        state.apply_persisted(persisted);
        info!(
            previous_revision = current_revision,
            new_revision, notified_revision, "proxy route store reloaded after update notification"
        );
        Ok(RouteStoreReloadResult::Reloaded {
            previous_revision: current_revision,
            new_revision,
        })
    }

    async fn update_bind_metadata<F>(&self, update_source: &'static str, update: F) -> bool
    where
        F: FnOnce(&mut RouteStoreState) -> Result<bool, RouteStoreUpdateError>,
    {
        match self.update_persisted_state(update_source, update).await {
            Ok(changed) => changed,
            Err(error) => {
                warn!(
                    update_source,
                    error = %error,
                    "failed to persist proxy route store metadata update"
                );
                false
            }
        }
    }

    pub async fn set_static_routes(&self, routes: Vec<UpstreamRoute>) {
        let mut state = self.state.write().await;
        state.routes.clear();
        for route in routes {
            state.routes.insert(route.server_id.clone(), route);
        }
    }

    pub async fn sync_discovered_routes(&self, routes: Vec<UpstreamRoute>) {
        let mut state = self.state.write().await;
        for route in state.routes.values_mut() {
            route.health_state = UpstreamHealthState::Unavailable;
        }

        for route in routes {
            match state.routes.get_mut(&route.server_id) {
                Some(existing) => {
                    existing.local_socket_name = route.local_socket_name;
                    existing.health_state = route.health_state;
                }
                None => {
                    state.routes.insert(route.server_id.clone(), route);
                }
            }
        }
    }

    pub async fn list_routes(&self) -> Vec<UpstreamRoute> {
        let state = self.state.read().await;
        let mut routes: Vec<_> = state.routes.values().cloned().collect();
        routes.sort_by(|left, right| left.server_id.cmp(&right.server_id));
        routes
    }

    pub async fn update_operation_state(
        &self,
        server_id: &str,
        operation_state: UpstreamOperationState,
    ) -> bool {
        let mut state = self.state.write().await;
        let Some(route) = state.routes.get_mut(server_id) else {
            return false;
        };
        route.operation_state = operation_state;
        true
    }

    pub async fn active_upstream_server_id(&self) -> Option<String> {
        self.select_default_upstream()
            .await
            .map(|route| route.server_id)
    }

    pub async fn get_rollout_session(&self) -> Option<RolloutSession> {
        self.state.read().await.rollout_session.clone()
    }

    pub async fn evaluate_rollout_drain(&self) -> RolloutDrainEvaluation {
        self.state.read().await.evaluate_rollout_drain()
    }

    pub async fn begin_rollout(
        &self,
        rollout_epoch: String,
        old_server_id: String,
        new_server_id: String,
    ) -> Result<(), RouteStoreUpdateError> {
        let log_session = RolloutSession {
            rollout_epoch: rollout_epoch.clone(),
            old_server_id: old_server_id.clone(),
            new_server_id: new_server_id.clone(),
            state: RolloutSessionState::Active,
            started_at_ms: 0,
        };
        self.update_persisted_state("begin_rollout", move |state| {
            state.rollout_session = Some(RolloutSession {
                rollout_epoch,
                old_server_id,
                new_server_id,
                state: RolloutSessionState::Active,
                started_at_ms: now_ms(),
            });
            Ok(true)
        })
        .await
        .map(|_| {
            log_rollout_session_lifecycle("begin_rollout", "started", &log_session);
        })
    }

    pub async fn end_rollout(&self) -> Result<(), RouteStoreUpdateError> {
        self.end_rollout_with_summary().await.map(|_| ())
    }

    pub async fn end_rollout_with_summary(
        &self,
    ) -> Result<RolloutEndSummary, RouteStoreUpdateError> {
        let mut end_summary = RolloutEndSummary::default();
        let result = self
            .update_persisted_state("end_rollout", |state| {
                end_summary = state.finish_rollout();
                Ok(end_summary.has_changes())
            })
            .await;

        if result.is_ok() {
            log_rollout_end_summary("end_rollout", &end_summary);
        }

        result.map(|_| end_summary)
    }

    pub async fn complete_rollout_if_drained(
        &self,
    ) -> Result<RolloutCompleteIfDrainedResult, RouteStoreUpdateError> {
        let mut outcome = None;
        let result = self
            .update_persisted_state("complete_rollout_if_drained", |state| {
                let evaluation = state.evaluate_rollout_drain();
                match evaluation.status {
                    RolloutDrainStatus::NoActiveRollout => {
                        outcome =
                            Some(RolloutCompleteIfDrainedResult::NoActiveRollout { evaluation });
                        Ok(false)
                    }
                    RolloutDrainStatus::Blocked => {
                        outcome = Some(RolloutCompleteIfDrainedResult::Blocked { evaluation });
                        Ok(false)
                    }
                    RolloutDrainStatus::Drained => {
                        let end_summary = state.finish_rollout();
                        outcome = Some(RolloutCompleteIfDrainedResult::Completed {
                            evaluation,
                            end_summary,
                        });
                        Ok(true)
                    }
                }
            })
            .await;

        if result.is_ok() {
            match outcome.as_ref() {
                Some(RolloutCompleteIfDrainedResult::Completed {
                    evaluation,
                    end_summary,
                }) => {
                    log_rollout_completion_evaluation(
                        "complete_rollout_if_drained",
                        "completed",
                        evaluation,
                    );
                    log_rollout_end_summary("complete_rollout_if_drained", end_summary);
                }
                Some(RolloutCompleteIfDrainedResult::Blocked { evaluation }) => {
                    log_rollout_completion_evaluation(
                        "complete_rollout_if_drained",
                        "blocked",
                        evaluation,
                    );
                }
                Some(RolloutCompleteIfDrainedResult::NoActiveRollout { evaluation }) => {
                    log_rollout_completion_evaluation(
                        "complete_rollout_if_drained",
                        "no_active",
                        evaluation,
                    );
                }
                None => {}
            }
        }

        result.map(|_| outcome.expect("rollout completion outcome should be set"))
    }

    pub async fn mark_rollout_state(
        &self,
        rollout_state: RolloutSessionState,
    ) -> Result<(), RouteStoreUpdateError> {
        let mut updated_session = None;
        self.update_persisted_state("mark_rollout_state", |state| {
            if let Some(session) = state.rollout_session.as_mut() {
                session.state = rollout_state;
                updated_session = Some(session.clone());
                Ok(true)
            } else {
                Ok(false)
            }
        })
        .await
        .map(|_| {
            if let Some(session) = updated_session.as_ref() {
                log_rollout_session_lifecycle("mark_rollout_state", "state_changed", session);
            }
        })
    }

    pub async fn list_room_routes(&self) -> Vec<RoomRouteRecord> {
        let state = self.state.read().await;
        let mut routes: Vec<_> = state.room_routes.values().cloned().collect();
        routes.sort_by(|left, right| left.room_id.cmp(&right.room_id));
        routes
    }

    pub async fn list_player_routes(&self) -> Vec<PlayerRouteRecord> {
        let state = self.state.read().await;
        let mut routes: Vec<_> = state.player_routes.values().cloned().collect();
        routes.sort_by(|left, right| left.player_id.cmp(&right.player_id));
        routes
    }

    pub async fn route_counts(&self) -> RouteCounts {
        let state = self.state.read().await;
        RouteCounts {
            room_routes: state.room_routes.len(),
            player_routes: state.player_routes.len(),
        }
    }

    pub async fn upsert_room_route(
        &self,
        mut record: RoomRouteRecord,
        expected_room_version: Option<u64>,
        expected_last_transfer_checksum: Option<String>,
    ) -> Result<(), RouteStoreUpdateError> {
        let mut log_context = None;
        let result = self
            .update_persisted_state("upsert_room_route", |state| {
                validate_rollout_epoch(&state.rollout_session, &record.rollout_epoch)?;
                let existing = state.room_routes.get(&record.room_id).cloned();

                match existing.as_ref() {
                    Some(existing) if room_route_records_match(existing, &record) => {
                        return Ok(false);
                    }
                    Some(existing) if record.room_version < existing.room_version => {
                        return Err("STALE_ROOM_ROUTE_UPDATE".into());
                    }
                    Some(existing) if record.room_version == existing.room_version => {
                        return Err("ROOM_ROUTE_CONFLICT".into());
                    }
                    Some(existing) => {
                        if let Some(expected_room_version) = expected_room_version {
                            if expected_room_version != existing.room_version {
                                return Err("ROOM_ROUTE_VERSION_MISMATCH".into());
                            }
                        }

                        if let Some(expected_last_transfer_checksum) =
                            expected_last_transfer_checksum.as_deref()
                        {
                            if existing.last_transfer_checksum != expected_last_transfer_checksum {
                                return Err("ROOM_ROUTE_CHECKSUM_MISMATCH".into());
                            }
                        }

                        if record.room_version != existing.room_version.saturating_add(1) {
                            return Err("ROOM_ROUTE_VERSION_GAP".into());
                        }

                        validate_transition_checksum(existing, &record)?;
                    }
                    None => {
                        validate_room_route_create(
                            &record,
                            expected_room_version,
                            expected_last_transfer_checksum.as_deref(),
                        )?;
                    }
                }

                record.updated_at_ms = now_ms();
                state
                    .room_routes
                    .insert(record.room_id.clone(), record.clone());
                log_context = Some((existing, record.clone()));
                Ok(true)
            })
            .await;

        if result.is_ok() {
            if let Some((existing, current)) = log_context {
                log_room_route_update("admin_upsert", existing.as_ref(), &current);
            }
        }

        result.map(|_| ())
    }

    pub async fn upsert_player_route(
        &self,
        mut record: PlayerRouteRecord,
    ) -> Result<(), RouteStoreUpdateError> {
        let mut log_context = None;
        let result = self
            .update_persisted_state("upsert_player_route", |state| {
                validate_rollout_epoch(&state.rollout_session, &record.rollout_epoch)?;
                let existing = state.player_routes.get(&record.player_id).cloned();
                record.updated_at_ms = now_ms();
                state
                    .player_routes
                    .insert(record.player_id.clone(), record.clone());
                log_context = Some((existing, record.clone()));
                Ok(true)
            })
            .await;

        if result.is_ok() {
            if let Some((existing, current)) = log_context {
                log_player_route_update("admin_upsert", existing.as_ref(), &current);
            }
        }

        result.map(|_| ())
    }

    pub async fn bind_room_owner(
        &self,
        room_id: &str,
        owner_server_id: &str,
        player_id: Option<&str>,
        observer_only: bool,
    ) {
        let mut room_log_context = None;
        let mut player_log_context = None;
        let changed = self
            .update_bind_metadata("bind_room_owner", |state| {
                let current_rollout_epoch = state
                    .rollout_session
                    .as_ref()
                    .map(|session| session.rollout_epoch.clone())
                    .unwrap_or_default();
                let existing_room_route = state.room_routes.get(room_id).cloned();
                let initial_member_count = if observer_only { 0 } else { 1 };
                let rollout_active = state.rollout_session.is_some();
                let (
                    bound_owner_server_id,
                    migration_state,
                    member_count,
                    online_member_count,
                    empty_since_ms,
                    room_version,
                    checksum,
                    rollout_epoch,
                ) = match existing_room_route.as_ref() {
                    Some(record) if record.owner_server_id == owner_server_id => (
                        owner_server_id.to_string(),
                        record.migration_state,
                        record.member_count.max(initial_member_count),
                        record.online_member_count.max(initial_member_count),
                        if record.online_member_count.max(initial_member_count) == 0 {
                            record.empty_since_ms
                        } else {
                            None
                        },
                        record.room_version,
                        record.last_transfer_checksum.clone(),
                        preserve_observed_rollout_epoch(record, &current_rollout_epoch),
                    ),
                    Some(record) if rollout_active => {
                        warn!(
                            room_id = room_id,
                            existing_owner_server_id = %record.owner_server_id,
                            observed_owner_server_id = %owner_server_id,
                            "ignored observed room owner mismatch during rollout"
                        );
                        (
                            record.owner_server_id.clone(),
                            record.migration_state,
                            record.member_count.max(initial_member_count),
                            record.online_member_count.max(initial_member_count),
                            if record.online_member_count.max(initial_member_count) == 0 {
                                record.empty_since_ms
                            } else {
                                None
                            },
                            record.room_version,
                            record.last_transfer_checksum.clone(),
                            preserve_observed_rollout_epoch(record, &current_rollout_epoch),
                        )
                    }
                    Some(record) => (
                        owner_server_id.to_string(),
                        infer_migration_state(owner_server_id, state.rollout_session.as_ref()),
                        record.member_count.max(initial_member_count),
                        record.online_member_count.max(initial_member_count),
                        None,
                        record.room_version.saturating_add(1),
                        record.last_transfer_checksum.clone(),
                        current_rollout_epoch.clone(),
                    ),
                    None => {
                        let migration_state =
                            infer_migration_state(owner_server_id, state.rollout_session.as_ref());
                        let online_member_count = initial_member_count;
                        (
                            owner_server_id.to_string(),
                            migration_state,
                            initial_member_count,
                            online_member_count,
                            None,
                            1,
                            String::new(),
                            current_rollout_epoch.clone(),
                        )
                    }
                };

                let next_room_route = RoomRouteRecord {
                    room_id: room_id.to_string(),
                    owner_server_id: bound_owner_server_id.clone(),
                    migration_state,
                    member_count,
                    online_member_count,
                    empty_since_ms,
                    room_version,
                    rollout_epoch: rollout_epoch.clone(),
                    last_transfer_checksum: checksum,
                    updated_at_ms: now_ms(),
                };
                state
                    .room_routes
                    .insert(room_id.to_string(), next_room_route.clone());
                room_log_context = Some((existing_room_route, next_room_route));

                if let Some(player_id) = player_id {
                    let existing_player_route = state.player_routes.get(player_id).cloned();
                    let next_player_route = PlayerRouteRecord {
                        player_id: player_id.to_string(),
                        current_room_id: Some(room_id.to_string()),
                        preferred_server_id: Some(bound_owner_server_id),
                        rollout_epoch,
                        updated_at_ms: now_ms(),
                    };
                    state
                        .player_routes
                        .insert(player_id.to_string(), next_player_route.clone());
                    player_log_context = Some((existing_player_route, next_player_route));
                }
                Ok(true)
            })
            .await;

        if changed {
            if let Some((existing, current)) = room_log_context {
                log_room_route_update("bind_room_owner", existing.as_ref(), &current);
            }
            if let Some((existing, current)) = player_log_context {
                log_player_route_update("bind_room_owner", existing.as_ref(), &current);
            }
        }
    }

    pub async fn select_upstream_for_room(&self, room_id: &str) -> Option<UpstreamRoute> {
        let state = self.state.read().await;
        if let Some(record) = state.room_routes.get(room_id) {
            if let Some(route) = state.find_connectable_route(&record.owner_server_id) {
                return Some(route.clone());
            }
        }

        state.select_default_for_new_room().cloned()
    }

    pub async fn select_upstream_for_player(&self, player_id: &str) -> Option<UpstreamRoute> {
        let state = self.state.read().await;
        if let Some(record) = state.player_routes.get(player_id) {
            if let Some(room_id) = record.current_room_id.as_deref() {
                if let Some(room_route) = state.room_routes.get(room_id) {
                    if let Some(route) = state.find_connectable_route(&room_route.owner_server_id) {
                        return Some(route.clone());
                    }
                }
            }
            if let Some(server_id) = record.preferred_server_id.as_deref() {
                if let Some(route) = state.find_connectable_route(server_id) {
                    return Some(route.clone());
                }
            }
        }

        state.select_default_for_new_room().cloned()
    }

    pub async fn select_default_upstream(&self) -> Option<UpstreamRoute> {
        self.state
            .read()
            .await
            .select_default_for_new_room()
            .cloned()
    }
}

pub fn route_store_update_channel(redis_key_prefix: &str) -> String {
    format!("{redis_key_prefix}proxy:route-store:updates")
}

pub fn parse_route_store_update_notification(
    payload: &str,
) -> Option<RouteStoreUpdateNotification> {
    serde_json::from_str::<RouteStoreUpdateNotification>(payload)
        .ok()
        .or_else(|| {
            payload
                .trim()
                .parse::<u64>()
                .ok()
                .map(|store_revision| RouteStoreUpdateNotification { store_revision })
        })
}

pub async fn run_redis_route_store_update_listener(
    route_store: ProxyRouteStore,
    channel: String,
    mut pubsub: redis::aio::PubSub,
) -> Result<(), RouteStorePersistenceError> {
    info!(
        redis_channel = %channel,
        "proxy route store update listener started"
    );
    let mut messages = pubsub.on_message();

    while let Some(message) = messages.next().await {
        let payload = match message.get_payload::<String>() {
            Ok(payload) => payload,
            Err(error) => {
                warn!(
                    error = %error,
                    "ignored invalid proxy route store update notification payload"
                );
                continue;
            }
        };
        let Some(notification) = parse_route_store_update_notification(&payload) else {
            warn!(
                payload = %payload,
                "ignored malformed proxy route store update notification"
            );
            continue;
        };

        match route_store
            .reload_if_newer_revision(notification.store_revision)
            .await
        {
            Ok(RouteStoreReloadResult::Reloaded {
                previous_revision,
                new_revision,
            }) => {
                info!(
                    previous_revision,
                    new_revision,
                    notified_revision = notification.store_revision,
                    "proxy route store update notification applied"
                );
            }
            Ok(RouteStoreReloadResult::IgnoredStale {
                current_revision,
                notified_revision,
            }) => {
                tracing::debug!(
                    current_revision,
                    notified_revision,
                    "ignored stale proxy route store update notification"
                );
            }
            Err(error) => {
                warn!(
                    notified_revision = notification.store_revision,
                    error = %error,
                    "failed to reload proxy route store after update notification"
                );
            }
        }
    }

    warn!(
        redis_channel = %channel,
        "proxy route store update listener stopped"
    );
    Ok(())
}

impl RouteStoreState {
    fn evaluate_rollout_drain(&self) -> RolloutDrainEvaluation {
        let Some(session) = self.rollout_session.as_ref() else {
            return RolloutDrainEvaluation {
                status: RolloutDrainStatus::NoActiveRollout,
                rollout_epoch: None,
                old_server_id: None,
                new_server_id: None,
                blocked_room_count: 0,
                blocked_player_count: 0,
                blocked_room_samples: Vec::new(),
                blocked_player_samples: Vec::new(),
                stale_room_route_count: self.room_routes.len(),
                stale_player_route_count: self.player_routes.len(),
            };
        };

        let mut blocked_room_ids = Vec::new();
        let mut blocked_room_lookup = HashMap::new();
        let mut stale_room_route_count = 0;
        let mut room_routes: Vec<_> = self.room_routes.values().collect();
        room_routes.sort_by(|left, right| left.room_id.cmp(&right.room_id));

        for record in room_routes {
            if !route_epoch_matches_current(&record.rollout_epoch, &session.rollout_epoch) {
                stale_room_route_count += 1;
                continue;
            }

            if room_route_blocks_rollout(record, session) {
                blocked_room_lookup.insert(record.room_id.clone(), true);
                blocked_room_ids.push(record.room_id.clone());
            }
        }

        let mut blocked_player_ids = Vec::new();
        let mut stale_player_route_count = 0;
        let mut player_routes: Vec<_> = self.player_routes.values().collect();
        player_routes.sort_by(|left, right| left.player_id.cmp(&right.player_id));

        for record in player_routes {
            if !route_epoch_matches_current(&record.rollout_epoch, &session.rollout_epoch) {
                stale_player_route_count += 1;
                continue;
            }

            let preferred_old =
                record.preferred_server_id.as_deref() == Some(session.old_server_id.as_str());
            let current_room_blocked = record
                .current_room_id
                .as_ref()
                .is_some_and(|room_id| blocked_room_lookup.contains_key(room_id));
            if preferred_old || current_room_blocked {
                blocked_player_ids.push(record.player_id.clone());
            }
        }

        let status = if blocked_room_ids.is_empty() && blocked_player_ids.is_empty() {
            RolloutDrainStatus::Drained
        } else {
            RolloutDrainStatus::Blocked
        };

        RolloutDrainEvaluation {
            status,
            rollout_epoch: Some(session.rollout_epoch.clone()),
            old_server_id: Some(session.old_server_id.clone()),
            new_server_id: Some(session.new_server_id.clone()),
            blocked_room_count: blocked_room_ids.len(),
            blocked_player_count: blocked_player_ids.len(),
            blocked_room_samples: blocked_room_ids
                .into_iter()
                .take(ROLLOUT_DRAIN_SAMPLE_LIMIT)
                .collect(),
            blocked_player_samples: blocked_player_ids
                .into_iter()
                .take(ROLLOUT_DRAIN_SAMPLE_LIMIT)
                .collect(),
            stale_room_route_count,
            stale_player_route_count,
        }
    }

    fn finish_rollout(&mut self) -> RolloutEndSummary {
        let ended_session = self.rollout_session.take();
        let removed_player_route_count = self.player_routes.len();
        let room_route_count_before = self.room_routes.len();
        self.player_routes.clear();

        let (rollout_epoch, old_server_id, new_server_id) = if let Some(session) = ended_session {
            self.room_routes.retain(|_, record| {
                !route_epoch_matches_current(&record.rollout_epoch, &session.rollout_epoch)
            });
            (
                Some(session.rollout_epoch),
                Some(session.old_server_id),
                Some(session.new_server_id),
            )
        } else {
            (None, None, None)
        };

        RolloutEndSummary {
            rollout_epoch,
            old_server_id,
            new_server_id,
            removed_room_route_count: room_route_count_before
                .saturating_sub(self.room_routes.len()),
            removed_player_route_count,
            remaining_room_route_count: self.room_routes.len(),
            remaining_player_route_count: self.player_routes.len(),
        }
    }

    fn persisted_snapshot(&self) -> PersistedRouteStoreState {
        PersistedRouteStoreState {
            store_revision: self.store_revision,
            rollout_session: self.rollout_session.clone(),
            room_routes: self.room_routes.clone(),
            player_routes: self.player_routes.clone(),
        }
    }

    fn apply_persisted(&mut self, persisted: PersistedRouteStoreState) {
        self.store_revision = persisted.store_revision;
        self.rollout_session = persisted.rollout_session;
        self.room_routes = persisted.room_routes;
        self.player_routes = persisted.player_routes;
    }

    fn find_connectable_route(&self, server_id: &str) -> Option<&UpstreamRoute> {
        self.routes
            .get(server_id)
            .filter(|route| route.can_accept_bound_sessions())
    }

    fn select_default_for_new_room(&self) -> Option<&UpstreamRoute> {
        if let Some(session) = &self.rollout_session {
            return self
                .routes
                .get(&session.new_server_id)
                .filter(|route| route.accepts_new_rooms());
        }

        let mut routes: Vec<_> = self.routes.values().collect();
        routes.sort_by(|left, right| left.server_id.cmp(&right.server_id));

        routes
            .iter()
            .copied()
            .find(|route| route.accepts_new_rooms())
            .or_else(|| {
                routes
                    .iter()
                    .copied()
                    .find(|route| route.can_accept_bound_sessions())
            })
    }
}

fn route_epoch_matches_current(record_rollout_epoch: &str, current_rollout_epoch: &str) -> bool {
    record_rollout_epoch.is_empty() || record_rollout_epoch == current_rollout_epoch
}

fn room_route_blocks_rollout(record: &RoomRouteRecord, session: &RolloutSession) -> bool {
    if record.owner_server_id == session.old_server_id {
        return true;
    }

    matches!(
        record.migration_state,
        RoomMigrationState::OwnedByOld
            | RoomMigrationState::DrainingOnOld
            | RoomMigrationState::FrozenForTransfer
            | RoomMigrationState::ImportingToNew
            | RoomMigrationState::TransferFailed
    )
}

fn validate_rollout_epoch(
    session: &Option<RolloutSession>,
    record_rollout_epoch: &str,
) -> Result<(), &'static str> {
    let Some(session) = session else {
        return Ok(());
    };
    if record_rollout_epoch.is_empty() || record_rollout_epoch == session.rollout_epoch {
        return Ok(());
    }
    Err("ROLLOUT_EPOCH_MISMATCH")
}

fn infer_migration_state(
    owner_server_id: &str,
    session: Option<&RolloutSession>,
) -> RoomMigrationState {
    let Some(session) = session else {
        return RoomMigrationState::OwnedByNew;
    };

    if owner_server_id == session.old_server_id {
        RoomMigrationState::OwnedByOld
    } else if owner_server_id == session.new_server_id {
        RoomMigrationState::OwnedByNew
    } else {
        RoomMigrationState::OwnedByNew
    }
}

fn preserve_observed_rollout_epoch(
    existing: &RoomRouteRecord,
    current_rollout_epoch: &str,
) -> String {
    if current_rollout_epoch.is_empty() {
        existing.rollout_epoch.clone()
    } else {
        current_rollout_epoch.to_string()
    }
}

fn room_route_records_match(left: &RoomRouteRecord, right: &RoomRouteRecord) -> bool {
    left.room_id == right.room_id
        && left.owner_server_id == right.owner_server_id
        && left.migration_state == right.migration_state
        && left.member_count == right.member_count
        && left.online_member_count == right.online_member_count
        && left.empty_since_ms == right.empty_since_ms
        && left.room_version == right.room_version
        && left.rollout_epoch == right.rollout_epoch
        && left.last_transfer_checksum == right.last_transfer_checksum
}

fn player_route_records_match(left: &PlayerRouteRecord, right: &PlayerRouteRecord) -> bool {
    left.player_id == right.player_id
        && left.current_room_id == right.current_room_id
        && left.preferred_server_id == right.preferred_server_id
        && left.rollout_epoch == right.rollout_epoch
}

fn log_room_route_update(
    update_source: &'static str,
    previous: Option<&RoomRouteRecord>,
    current: &RoomRouteRecord,
) {
    if previous.is_some_and(|previous| room_route_records_match(previous, current)) {
        return;
    }

    info!(
        update_source,
        action = if previous.is_some() { "updated" } else { "created" },
        room_id = %current.room_id,
        owner_server_id = %current.owner_server_id,
        migration_state = current.migration_state.as_str(),
        member_count = current.member_count,
        online_member_count = current.online_member_count,
        empty_since_ms = ?current.empty_since_ms,
        room_version = current.room_version,
        rollout_epoch = %current.rollout_epoch,
        last_transfer_checksum = %current.last_transfer_checksum,
        previous_owner_server_id = %previous.map(|record| record.owner_server_id.as_str()).unwrap_or_default(),
        previous_migration_state = previous.map(|record| record.migration_state.as_str()).unwrap_or_default(),
        previous_member_count = previous.map(|record| record.member_count).unwrap_or(0),
        previous_online_member_count = previous.map(|record| record.online_member_count).unwrap_or(0),
        previous_empty_since_ms = ?previous.and_then(|record| record.empty_since_ms),
        previous_room_version = previous.map(|record| record.room_version).unwrap_or(0),
        previous_rollout_epoch = %previous.map(|record| record.rollout_epoch.as_str()).unwrap_or_default(),
        previous_last_transfer_checksum = %previous.map(|record| record.last_transfer_checksum.as_str()).unwrap_or_default(),
        "room route updated"
    );
}

fn log_player_route_update(
    update_source: &'static str,
    previous: Option<&PlayerRouteRecord>,
    current: &PlayerRouteRecord,
) {
    if previous.is_some_and(|previous| player_route_records_match(previous, current)) {
        return;
    }

    info!(
        update_source,
        action = if previous.is_some() { "updated" } else { "created" },
        player_id = %current.player_id,
        current_room_id = %current.current_room_id.as_deref().unwrap_or_default(),
        preferred_server_id = %current.preferred_server_id.as_deref().unwrap_or_default(),
        rollout_epoch = %current.rollout_epoch,
        previous_current_room_id = %previous
            .and_then(|record| record.current_room_id.as_deref())
            .unwrap_or_default(),
        previous_preferred_server_id = %previous
            .and_then(|record| record.preferred_server_id.as_deref())
            .unwrap_or_default(),
        previous_rollout_epoch = %previous.map(|record| record.rollout_epoch.as_str()).unwrap_or_default(),
        "player route updated"
    );
}

fn log_rollout_session_lifecycle(
    update_source: &'static str,
    event: &'static str,
    session: &RolloutSession,
) {
    info!(
        update_source,
        event,
        rollout_epoch = %session.rollout_epoch,
        old_server_id = %session.old_server_id,
        new_server_id = %session.new_server_id,
        rollout_state = session.state.as_str(),
        drain_status = "not_evaluated",
        blocked_room_count = 0,
        blocked_player_count = 0,
        stale_room_route_count = 0,
        stale_player_route_count = 0,
        removed_room_route_count = 0,
        removed_player_route_count = 0,
        remaining_room_route_count = 0,
        remaining_player_route_count = 0,
        "proxy rollout lifecycle updated"
    );
}

fn log_rollout_completion_evaluation(
    update_source: &'static str,
    event: &'static str,
    evaluation: &RolloutDrainEvaluation,
) {
    info!(
        update_source,
        event,
        rollout_epoch = %evaluation.rollout_epoch.as_deref().unwrap_or_default(),
        old_server_id = %evaluation.old_server_id.as_deref().unwrap_or_default(),
        new_server_id = %evaluation.new_server_id.as_deref().unwrap_or_default(),
        drain_status = evaluation.status.as_str(),
        blocked_room_count = evaluation.blocked_room_count,
        blocked_player_count = evaluation.blocked_player_count,
        stale_room_route_count = evaluation.stale_room_route_count,
        stale_player_route_count = evaluation.stale_player_route_count,
        removed_room_route_count = 0,
        removed_player_route_count = 0,
        remaining_room_route_count = 0,
        remaining_player_route_count = 0,
        "proxy rollout complete-if-drained evaluated"
    );
}

fn log_rollout_end_summary(update_source: &'static str, summary: &RolloutEndSummary) {
    let Some(rollout_epoch) = summary.rollout_epoch.as_deref() else {
        return;
    };

    info!(
        update_source,
        event = "ended",
        rollout_epoch,
        old_server_id = %summary.old_server_id.as_deref().unwrap_or_default(),
        new_server_id = %summary.new_server_id.as_deref().unwrap_or_default(),
        drain_status = "completed",
        blocked_room_count = 0,
        blocked_player_count = 0,
        stale_room_route_count = 0,
        stale_player_route_count = 0,
        removed_room_route_count = summary.removed_room_route_count,
        removed_player_route_count = summary.removed_player_route_count,
        remaining_room_route_count = summary.remaining_room_route_count,
        remaining_player_route_count = summary.remaining_player_route_count,
        "proxy rollout ended"
    );
}

fn validate_room_route_create(
    record: &RoomRouteRecord,
    expected_room_version: Option<u64>,
    expected_last_transfer_checksum: Option<&str>,
) -> Result<(), &'static str> {
    if let Some(expected_room_version) = expected_room_version {
        if expected_room_version != 0 {
            return Err("ROOM_ROUTE_VERSION_MISMATCH");
        }
    }

    if let Some(expected_last_transfer_checksum) = expected_last_transfer_checksum {
        if !expected_last_transfer_checksum.is_empty() {
            return Err("ROOM_ROUTE_CHECKSUM_MISMATCH");
        }
    }

    if record.room_version != 1 {
        return Err("INVALID_ROOM_ROUTE_VERSION");
    }

    if transition_requires_checksum(record.migration_state)
        && record.last_transfer_checksum.is_empty()
    {
        return Err("MISSING_TRANSFER_CHECKSUM");
    }

    Ok(())
}

fn validate_transition_checksum(
    existing: &RoomRouteRecord,
    incoming: &RoomRouteRecord,
) -> Result<(), &'static str> {
    if transition_requires_checksum(incoming.migration_state)
        && incoming.last_transfer_checksum.is_empty()
    {
        return Err("MISSING_TRANSFER_CHECKSUM");
    }

    if !existing.last_transfer_checksum.is_empty() && incoming.last_transfer_checksum.is_empty() {
        return Err("MISSING_TRANSFER_CHECKSUM");
    }

    Ok(())
}

fn transition_requires_checksum(migration_state: RoomMigrationState) -> bool {
    matches!(
        migration_state,
        RoomMigrationState::ImportingToNew
            | RoomMigrationState::OwnedByNew
            | RoomMigrationState::TransferFailed
            | RoomMigrationState::RetiredOnOld
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct MemoryRouteStorePersistence {
        state: Mutex<PersistedRouteStoreState>,
        published_revisions: Mutex<Vec<u64>>,
    }

    impl MemoryRouteStorePersistence {
        async fn persisted_state(&self) -> PersistedRouteStoreState {
            self.state.lock().await.clone()
        }

        async fn overwrite_state(&self, state: PersistedRouteStoreState) {
            *self.state.lock().await = state;
        }

        async fn published_revisions(&self) -> Vec<u64> {
            self.published_revisions.lock().await.clone()
        }
    }

    impl RouteStorePersistence for MemoryRouteStorePersistence {
        fn load<'a>(&'a self) -> PersistenceFuture<'a, PersistedRouteStoreState> {
            Box::pin(async move { Ok(self.state.lock().await.clone()) })
        }

        fn save<'a>(
            &'a self,
            expected_revision: u64,
            state: PersistedRouteStoreState,
        ) -> PersistenceFuture<'a, RouteStoreSaveResult> {
            Box::pin(async move {
                let mut current = self.state.lock().await;
                if current.store_revision != expected_revision {
                    return Ok(RouteStoreSaveResult::RevisionConflict);
                }
                *current = state;
                Ok(RouteStoreSaveResult::Saved)
            })
        }

        fn publish_update<'a>(&'a self, store_revision: u64) -> PersistenceFuture<'a, ()> {
            Box::pin(async move {
                self.published_revisions.lock().await.push(store_revision);
                Ok(())
            })
        }
    }

    struct JsonRouteStorePersistence {
        json: String,
    }

    impl RouteStorePersistence for JsonRouteStorePersistence {
        fn load<'a>(&'a self) -> PersistenceFuture<'a, PersistedRouteStoreState> {
            Box::pin(async move { Ok(serde_json::from_str(&self.json)?) })
        }

        fn save<'a>(
            &'a self,
            _expected_revision: u64,
            _state: PersistedRouteStoreState,
        ) -> PersistenceFuture<'a, RouteStoreSaveResult> {
            Box::pin(async move { Ok(RouteStoreSaveResult::Saved) })
        }
    }

    fn room_record(
        room_id: &str,
        owner_server_id: &str,
        migration_state: RoomMigrationState,
        room_version: u64,
        checksum: &str,
    ) -> RoomRouteRecord {
        RoomRouteRecord {
            room_id: room_id.to_string(),
            owner_server_id: owner_server_id.to_string(),
            migration_state,
            member_count: 0,
            online_member_count: 0,
            empty_since_ms: Some(123),
            room_version,
            rollout_epoch: "rollout-1".to_string(),
            last_transfer_checksum: checksum.to_string(),
            updated_at_ms: 0,
        }
    }

    trait TestRoomRecordExt {
        fn with_rollout_epoch(self, rollout_epoch: &str) -> Self;
    }

    impl TestRoomRecordExt for RoomRouteRecord {
        fn with_rollout_epoch(mut self, rollout_epoch: &str) -> Self {
            self.rollout_epoch = rollout_epoch.to_string();
            self
        }
    }

    async fn get_room_route(store: &ProxyRouteStore, room_id: &str) -> RoomRouteRecord {
        store
            .list_room_routes()
            .await
            .into_iter()
            .find(|record| record.room_id == room_id)
            .expect("room route should exist")
    }

    #[tokio::test]
    async fn room_route_replay_is_idempotent() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let route = get_room_route(&store, "room-1").await;
        assert_eq!(route.room_version, 1);
    }

    #[tokio::test]
    async fn room_route_rejects_stale_version() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let result = store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 0, ""),
                None,
                None,
            )
            .await;

        assert_eq!(result.unwrap_err().code(), "STALE_ROOM_ROUTE_UPDATE");
    }

    #[tokio::test]
    async fn room_route_rejects_same_version_conflict() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let mut conflicting = room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, "");
        conflicting.member_count = 2;

        let result = store.upsert_room_route(conflicting, None, None).await;

        assert_eq!(result.unwrap_err().code(), "ROOM_ROUTE_CONFLICT");
    }

    #[tokio::test]
    async fn room_route_rejects_version_gap() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let result = store
            .upsert_room_route(
                room_record(
                    "room-1",
                    "old",
                    RoomMigrationState::FrozenForTransfer,
                    3,
                    "checksum-1",
                ),
                Some(1),
                None,
            )
            .await;

        assert_eq!(result.unwrap_err().code(), "ROOM_ROUTE_VERSION_GAP");
    }

    #[tokio::test]
    async fn room_route_rejects_checksum_mismatch() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record(
                    "room-1",
                    "old",
                    RoomMigrationState::FrozenForTransfer,
                    1,
                    "checksum-1",
                ),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let result = store
            .upsert_room_route(
                room_record(
                    "room-1",
                    "new",
                    RoomMigrationState::OwnedByNew,
                    2,
                    "checksum-2",
                ),
                Some(1),
                Some("checksum-legacy".to_string()),
            )
            .await;

        assert_eq!(result.unwrap_err().code(), "ROOM_ROUTE_CHECKSUM_MISMATCH");
    }

    #[tokio::test]
    async fn bind_room_owner_preserves_version_for_same_owner() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record(
                    "room-1",
                    "server-a",
                    RoomMigrationState::FrozenForTransfer,
                    1,
                    "checksum-1",
                ),
                Some(0),
                None,
            )
            .await
            .unwrap();

        store
            .bind_room_owner("room-1", "server-a", Some("player-1"), false)
            .await;

        let route = get_room_route(&store, "room-1").await;
        assert_eq!(route.owner_server_id, "server-a");
        assert_eq!(route.room_version, 1);
        assert_eq!(route.last_transfer_checksum, "checksum-1");
        assert_eq!(route.member_count, 1);
        assert_eq!(route.online_member_count, 1);
    }

    #[tokio::test]
    async fn bind_room_owner_does_not_override_owner_during_rollout() {
        let store = ProxyRouteStore::default();
        store
            .begin_rollout(
                "rollout-1".to_string(),
                "old".to_string(),
                "new".to_string(),
            )
            .await
            .unwrap();
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        store
            .bind_room_owner("room-1", "new", Some("player-1"), false)
            .await;

        let route = get_room_route(&store, "room-1").await;
        assert_eq!(route.owner_server_id, "old");
        assert_eq!(route.room_version, 1);

        let player_route = store
            .list_player_routes()
            .await
            .into_iter()
            .find(|record| record.player_id == "player-1")
            .expect("player route should exist");
        assert_eq!(player_route.preferred_server_id.as_deref(), Some("old"));
    }

    #[tokio::test]
    async fn player_reconnect_prefers_transferred_room_owner_route() {
        let store = ProxyRouteStore::default();
        store
            .set_static_routes(vec![
                UpstreamRoute {
                    server_id: "old".to_string(),
                    local_socket_name: "old.sock".to_string(),
                    operation_state: UpstreamOperationState::Draining,
                    health_state: UpstreamHealthState::Healthy,
                },
                UpstreamRoute {
                    server_id: "new".to_string(),
                    local_socket_name: "new.sock".to_string(),
                    operation_state: UpstreamOperationState::Active,
                    health_state: UpstreamHealthState::Healthy,
                },
            ])
            .await;
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();
        store
            .bind_room_owner("room-1", "old", Some("player-1"), false)
            .await;
        store
            .upsert_room_route(
                room_record(
                    "room-1",
                    "new",
                    RoomMigrationState::OwnedByNew,
                    2,
                    "checksum-1",
                ),
                Some(1),
                Some(String::new()),
            )
            .await
            .unwrap();

        let route = store
            .select_upstream_for_player("player-1")
            .await
            .expect("route should be selected");

        assert_eq!(route.server_id, "new");
    }

    #[tokio::test]
    async fn rollout_drain_evaluation_reports_no_active_rollout() {
        let store = ProxyRouteStore::default();

        let evaluation = store.evaluate_rollout_drain().await;

        assert_eq!(evaluation.status, RolloutDrainStatus::NoActiveRollout);
        assert_eq!(evaluation.blocked_room_count, 0);
        assert_eq!(evaluation.blocked_player_count, 0);

        let result = store.complete_rollout_if_drained().await.unwrap();
        assert!(matches!(
            result,
            RolloutCompleteIfDrainedResult::NoActiveRollout { .. }
        ));
    }

    #[tokio::test]
    async fn rollout_drain_evaluation_blocks_old_room_routes() {
        let store = ProxyRouteStore::default();
        store
            .begin_rollout(
                "rollout-1".to_string(),
                "old".to_string(),
                "new".to_string(),
            )
            .await
            .unwrap();
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let evaluation = store.evaluate_rollout_drain().await;

        assert_eq!(evaluation.status, RolloutDrainStatus::Blocked);
        assert_eq!(evaluation.blocked_room_count, 1);
        assert_eq!(evaluation.blocked_room_samples, vec!["room-1"]);

        let result = store.complete_rollout_if_drained().await.unwrap();
        assert!(matches!(
            result,
            RolloutCompleteIfDrainedResult::Blocked { .. }
        ));
        assert!(store.get_rollout_session().await.is_some());
    }

    #[tokio::test]
    async fn rollout_drain_evaluation_blocks_old_player_routes() {
        let store = ProxyRouteStore::default();
        store
            .begin_rollout(
                "rollout-1".to_string(),
                "old".to_string(),
                "new".to_string(),
            )
            .await
            .unwrap();
        store
            .upsert_player_route(PlayerRouteRecord {
                player_id: "player-1".to_string(),
                current_room_id: None,
                preferred_server_id: Some("old".to_string()),
                rollout_epoch: "rollout-1".to_string(),
                updated_at_ms: 0,
            })
            .await
            .unwrap();

        let evaluation = store.evaluate_rollout_drain().await;

        assert_eq!(evaluation.status, RolloutDrainStatus::Blocked);
        assert_eq!(evaluation.blocked_player_count, 1);
        assert_eq!(evaluation.blocked_player_samples, vec!["player-1"]);
    }

    #[tokio::test]
    async fn rollout_complete_if_drained_ends_and_cleans_current_epoch_routes() {
        let store = ProxyRouteStore::default();
        store
            .upsert_room_route(
                room_record("room-stale", "old", RoomMigrationState::OwnedByOld, 1, "")
                    .with_rollout_epoch("rollout-legacy"),
                Some(0),
                None,
            )
            .await
            .unwrap();
        store
            .begin_rollout(
                "rollout-1".to_string(),
                "old".to_string(),
                "new".to_string(),
            )
            .await
            .unwrap();
        store
            .upsert_room_route(
                room_record(
                    "room-new",
                    "new",
                    RoomMigrationState::OwnedByNew,
                    1,
                    "checksum-1",
                ),
                Some(0),
                None,
            )
            .await
            .unwrap();
        store
            .upsert_room_route(
                room_record(
                    "room-unscoped",
                    "new",
                    RoomMigrationState::OwnedByNew,
                    1,
                    "checksum-2",
                )
                .with_rollout_epoch(""),
                Some(0),
                None,
            )
            .await
            .unwrap();
        store
            .upsert_player_route(PlayerRouteRecord {
                player_id: "player-new".to_string(),
                current_room_id: Some("room-new".to_string()),
                preferred_server_id: Some("new".to_string()),
                rollout_epoch: "rollout-1".to_string(),
                updated_at_ms: 0,
            })
            .await
            .unwrap();

        let result = store.complete_rollout_if_drained().await.unwrap();

        let RolloutCompleteIfDrainedResult::Completed {
            evaluation,
            end_summary,
        } = result
        else {
            panic!("rollout should complete");
        };
        assert_eq!(evaluation.status, RolloutDrainStatus::Drained);
        assert_eq!(evaluation.stale_room_route_count, 1);
        assert_eq!(end_summary.rollout_epoch.as_deref(), Some("rollout-1"));
        assert_eq!(end_summary.removed_room_route_count, 2);
        assert_eq!(end_summary.removed_player_route_count, 1);
        assert!(store.get_rollout_session().await.is_none());

        let routes = store.list_room_routes().await;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].room_id, "room-stale");
    }

    #[tokio::test]
    async fn persisted_state_restores_rollout_room_and_player_routes() {
        let persistence = Arc::new(MemoryRouteStorePersistence::default());
        let store = ProxyRouteStore::with_persistence(persistence.clone());
        store
            .begin_rollout(
                "rollout-1".to_string(),
                "old".to_string(),
                "new".to_string(),
            )
            .await
            .unwrap();
        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();
        store
            .upsert_player_route(PlayerRouteRecord {
                player_id: "player-1".to_string(),
                current_room_id: Some("room-1".to_string()),
                preferred_server_id: Some("old".to_string()),
                rollout_epoch: "rollout-1".to_string(),
                updated_at_ms: 0,
            })
            .await
            .unwrap();

        let restored = ProxyRouteStore::with_persistence(persistence);
        restored.load_persisted_state().await.unwrap();

        let rollout = restored
            .get_rollout_session()
            .await
            .expect("rollout should be restored");
        assert_eq!(rollout.rollout_epoch, "rollout-1");
        assert_eq!(rollout.old_server_id, "old");
        assert_eq!(rollout.new_server_id, "new");

        let room_route = get_room_route(&restored, "room-1").await;
        assert_eq!(room_route.owner_server_id, "old");
        assert_eq!(room_route.room_version, 1);

        let player_route = restored
            .list_player_routes()
            .await
            .into_iter()
            .find(|record| record.player_id == "player-1")
            .expect("player route should be restored");
        assert_eq!(player_route.current_room_id.as_deref(), Some("room-1"));
        assert_eq!(player_route.preferred_server_id.as_deref(), Some("old"));
    }

    #[tokio::test]
    async fn persisted_state_save_advances_revision() {
        let persistence = Arc::new(MemoryRouteStorePersistence::default());
        let store = ProxyRouteStore::with_persistence(persistence.clone());

        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();
        assert_eq!(persistence.persisted_state().await.store_revision, 1);

        store
            .upsert_player_route(PlayerRouteRecord {
                player_id: "player-1".to_string(),
                current_room_id: Some("room-1".to_string()),
                preferred_server_id: Some("old".to_string()),
                rollout_epoch: "rollout-1".to_string(),
                updated_at_ms: 0,
            })
            .await
            .unwrap();
        assert_eq!(persistence.persisted_state().await.store_revision, 2);
    }

    #[tokio::test]
    async fn persisted_state_save_publishes_update_revision() {
        let persistence = Arc::new(MemoryRouteStorePersistence::default());
        let store = ProxyRouteStore::with_persistence(persistence.clone());

        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        assert_eq!(persistence.published_revisions().await, vec![1]);
    }

    #[tokio::test]
    async fn stale_update_notification_does_not_reload() {
        let persistence = Arc::new(MemoryRouteStorePersistence::default());
        let store = ProxyRouteStore::with_persistence(persistence.clone());

        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let mut newer_state = PersistedRouteStoreState::default();
        newer_state.store_revision = 2;
        newer_state.room_routes.insert(
            "room-2".to_string(),
            room_record(
                "room-2",
                "new",
                RoomMigrationState::OwnedByNew,
                1,
                "checksum-2",
            ),
        );
        persistence.overwrite_state(newer_state).await;

        let result = store.reload_if_newer_revision(1).await.unwrap();

        assert_eq!(
            result,
            RouteStoreReloadResult::IgnoredStale {
                current_revision: 1,
                notified_revision: 1,
            }
        );
        assert_eq!(
            get_room_route(&store, "room-1").await.owner_server_id,
            "old"
        );
        assert!(
            store
                .list_room_routes()
                .await
                .iter()
                .all(|record| record.room_id != "room-2")
        );
    }

    #[tokio::test]
    async fn newer_update_notification_reloads_persisted_state() {
        let persistence = Arc::new(MemoryRouteStorePersistence::default());
        let store = ProxyRouteStore::with_persistence(persistence.clone());

        store
            .upsert_room_route(
                room_record("room-1", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let mut newer_state = PersistedRouteStoreState::default();
        newer_state.store_revision = 2;
        newer_state.room_routes.insert(
            "room-2".to_string(),
            room_record(
                "room-2",
                "new",
                RoomMigrationState::OwnedByNew,
                1,
                "checksum-2",
            ),
        );
        persistence.overwrite_state(newer_state).await;

        let result = store.reload_if_newer_revision(2).await.unwrap();

        assert_eq!(
            result,
            RouteStoreReloadResult::Reloaded {
                previous_revision: 1,
                new_revision: 2,
            }
        );
        assert_eq!(
            get_room_route(&store, "room-2").await.owner_server_id,
            "new"
        );
        assert!(
            store
                .list_room_routes()
                .await
                .iter()
                .all(|record| record.room_id != "room-1")
        );
    }

    #[tokio::test]
    async fn cas_conflict_does_not_overwrite_and_reloads_latest_state() {
        let persistence = Arc::new(MemoryRouteStorePersistence::default());
        let stale_store = ProxyRouteStore::with_persistence(persistence.clone());
        let current_store = ProxyRouteStore::with_persistence(persistence.clone());

        stale_store.load_persisted_state().await.unwrap();
        current_store.load_persisted_state().await.unwrap();

        current_store
            .upsert_room_route(
                room_record(
                    "room-current",
                    "new",
                    RoomMigrationState::OwnedByNew,
                    1,
                    "checksum-1",
                ),
                Some(0),
                None,
            )
            .await
            .unwrap();

        let conflict = stale_store
            .upsert_room_route(
                room_record("room-stale", "old", RoomMigrationState::OwnedByOld, 1, ""),
                Some(0),
                None,
            )
            .await
            .unwrap_err();

        assert_eq!(conflict.code(), "ROUTE_STORE_REVISION_CONFLICT");
        assert_eq!(persistence.persisted_state().await.store_revision, 1);
        assert!(
            get_room_route(&stale_store, "room-current")
                .await
                .room_version
                == 1
        );
        assert!(
            stale_store
                .list_room_routes()
                .await
                .iter()
                .all(|record| record.room_id != "room-stale")
        );
        assert!(
            persistence
                .persisted_state()
                .await
                .room_routes
                .contains_key("room-current")
        );
        assert!(
            !persistence
                .persisted_state()
                .await
                .room_routes
                .contains_key("room-stale")
        );
        assert_eq!(persistence.published_revisions().await, vec![1]);
    }

    #[test]
    fn route_store_update_notification_accepts_json_and_revision_string() {
        assert_eq!(
            route_store_update_channel("prod:"),
            "prod:proxy:route-store:updates"
        );
        assert_eq!(
            parse_route_store_update_notification(r#"{"store_revision":42}"#)
                .unwrap()
                .store_revision,
            42
        );
        assert_eq!(
            parse_route_store_update_notification("43")
                .unwrap()
                .store_revision,
            43
        );
        assert!(parse_route_store_update_notification("not-a-revision").is_none());
    }

    #[tokio::test]
    async fn old_persisted_json_without_revision_loads_as_revision_zero() {
        let legacy_json = serde_json::json!({
            "rollout_session": null,
            "room_routes": {
                "room-legacy": room_record(
                    "room-legacy",
                    "old",
                    RoomMigrationState::OwnedByOld,
                    1,
                    ""
                )
            },
            "player_routes": {}
        })
        .to_string();
        let legacy = Arc::new(JsonRouteStorePersistence { json: legacy_json });
        let store = ProxyRouteStore::with_persistence(legacy);

        store.load_persisted_state().await.unwrap();

        let state = store.state.read().await;
        assert_eq!(state.store_revision, 0);
        assert!(state.room_routes.contains_key("room-legacy"));
    }

    #[tokio::test]
    async fn legacy_revision_zero_snapshot_can_be_saved_as_revision_one() {
        let mut legacy_state = PersistedRouteStoreState::default();
        legacy_state.room_routes.insert(
            "room-legacy".to_string(),
            room_record("room-legacy", "old", RoomMigrationState::OwnedByOld, 1, ""),
        );
        let persistence = Arc::new(MemoryRouteStorePersistence::default());
        persistence.overwrite_state(legacy_state).await;
        let store = ProxyRouteStore::with_persistence(persistence.clone());
        store.load_persisted_state().await.unwrap();

        store
            .upsert_room_route(
                room_record(
                    "room-legacy",
                    "new",
                    RoomMigrationState::OwnedByNew,
                    2,
                    "checksum-1",
                ),
                Some(1),
                Some(String::new()),
            )
            .await
            .unwrap();

        let persisted = persistence.persisted_state().await;
        assert_eq!(persisted.store_revision, 1);
        assert_eq!(
            persisted
                .room_routes
                .get("room-legacy")
                .unwrap()
                .owner_server_id,
            "new"
        );
    }
}
