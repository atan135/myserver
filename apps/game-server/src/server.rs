use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use global_id::{
    DEFAULT_WORKER_LEASE_RENEW_INTERVAL_SECONDS, DEFAULT_WORKER_LEASE_TTL_SECONDS, WorkerLease,
};
use interprocess::local_socket::traits::tokio::Listener as _;
use serde_json::{Value, json};
use service_registry::{
    ConvergenceConfig, ConvergenceTask, HealthState, RegistryClient, StartupErrorCode,
    spawn_registry_publication,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Notify, RwLock, mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};
use tracing::{error, info, warn};

use crate::adapters::persistence::PgCharacterElementStore;
use crate::business::character_element::CharacterElementFacade;
use crate::config::Config;
use crate::core::character_discipline::{DisciplineService, PgDisciplineStore};
use crate::core::character_progress::CharacterProgressService;
use crate::core::character_title::{PgTitleStore, TitleService};
use crate::core::character_title_unlock::TitleUnlockService;
use crate::core::config_table::{ConfigTableRuntime, spawn_hot_reload_task};
use crate::core::context::{ConnectionContext, PlayerRegistry, ServerSharedState, ServiceContext};
use crate::core::logic::SharedRoomLogicFactory;
use crate::core::online_route::{
    MissingRouteOwnership, RouteRefreshAction, RouteRefreshState, clear_online_route,
    online_route_refresh_secs, online_route_ttl_secs, refresh_action, refresh_online_route,
    restore_missing_online_route,
};
use crate::core::player::{PgPlayerStore, PlayerManager};
use crate::core::room::{
    ConnectionCloseState, OutboundMessage, outbound_queue_error_kind_from_error,
};
use crate::core::runtime::RoomManager;
use crate::core::service::{
    character_progress_service, character_title_service, core_service, inventory_service,
    match_service, room_service,
};
use crate::db_store::PgAuditStore;
use crate::gameroom::GameRoomLogicFactory;
use crate::gameservice::{character_element, room_query};
use crate::match_client::{MatchClientConfig, init_match_client, spawn_match_client_rediscovery};
use crate::metrics::METRICS;
use crate::pb::SessionKickPush;
use crate::protocol::{HEADER_LEN, MessageType, Packet, encode_packet, parse_header};
use crate::session::{Session, SessionState};
use crate::startup::{
    CleanupExecutor, CleanupStep, LeaseWaitConfig, LeaseWaitError, OwnedResource, StartupOwnership,
    run_cleanup, run_then_cleanup, shutdown_signal, wait_for_worker_lease,
};

pub const DEFAULT_DRAIN_MODE_REASON: &str = "rollout";
pub const DEFAULT_DRAIN_MODE_SOURCE: &str = "admin";
const CLEANUP_OPERATION_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_DRAIN_SHUTDOWN_TIMEOUT_SECS: u64 = 300;
const DEFAULT_DRAIN_SHUTDOWN_POLL_MS: u64 = 250;

#[derive(Clone, Copy, Debug)]
struct DrainShutdownConfig {
    timeout: Duration,
    poll_interval: Duration,
}

#[derive(Debug)]
pub struct DrainShutdownControl {
    arm_tx: mpsc::Sender<()>,
    armed: AtomicBool,
}

impl DrainShutdownControl {
    pub(crate) fn channel() -> (Arc<Self>, mpsc::Receiver<()>) {
        let (arm_tx, arm_rx) = mpsc::channel(1);
        (
            Arc::new(Self {
                arm_tx,
                armed: AtomicBool::new(false),
            }),
            arm_rx,
        )
    }

    pub fn try_arm(&self) -> bool {
        if self
            .armed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        if self.arm_tx.try_send(()).is_err() {
            self.armed.store(false, Ordering::Release);
            return false;
        }
        true
    }

    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::Acquire)
    }

    fn disarm(&self) {
        self.armed.store(false, Ordering::Release);
    }
}

impl DrainShutdownConfig {
    fn try_from_env() -> std::io::Result<Self> {
        Ok(Self {
            timeout: strict_duration_env(
                "GAME_DRAIN_SHUTDOWN_TIMEOUT_SECS",
                DEFAULT_DRAIN_SHUTDOWN_TIMEOUT_SECS,
                1,
                3_600,
                Duration::from_secs,
            )?,
            poll_interval: strict_duration_env(
                "GAME_DRAIN_SHUTDOWN_POLL_MS",
                DEFAULT_DRAIN_SHUTDOWN_POLL_MS,
                10,
                10_000,
                Duration::from_millis,
            )?,
        })
    }
}

fn strict_duration_env(
    name: &str,
    default: u64,
    minimum: u64,
    maximum: u64,
    convert: fn(u64) -> Duration,
) -> std::io::Result<Duration> {
    let value = match std::env::var(name) {
        Ok(value) => value
            .trim()
            .parse::<u64>()
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, name))?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error @ std::env::VarError::NotUnicode(_)) => {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, error));
        }
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} must be in {minimum}..={maximum}"),
        ));
    }
    Ok(convert(value))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DrainShutdownDecision {
    Wait,
    Shutdown,
    TimedOut,
}

fn drain_shutdown_decision(
    connection_count: u64,
    owned_room_count: u64,
    migrating_room_count: u64,
    elapsed: Duration,
    timeout: Duration,
) -> DrainShutdownDecision {
    if connection_count == 0 && owned_room_count == 0 && migrating_room_count == 0 {
        DrainShutdownDecision::Shutdown
    } else if elapsed >= timeout {
        DrainShutdownDecision::TimedOut
    } else {
        DrainShutdownDecision::Wait
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub heartbeat_timeout_secs: u64,
    pub max_body_len: usize,
    pub msg_rate_window_ms: u64,
    pub msg_rate_max: u64,
    pub player_msg_rate_window_ms: u64,
    pub player_msg_rate_max: u64,
    pub input_timestamp_required: bool,
    pub input_timestamp_max_skew_ms: u64,
    pub input_anomaly_window_ms: u64,
    pub input_anomaly_max: u64,
    pub drain_mode_enabled: bool,
    pub drain_mode_entered_at_ms: Option<u64>,
    pub drain_mode_reason: String,
    pub drain_mode_source: String,
    pub drain_state_tx: watch::Sender<bool>,
    pub drain_shutdown: Arc<DrainShutdownControl>,
}

impl RuntimeConfig {
    pub fn status_label(&self) -> &'static str {
        if self.drain_mode_enabled {
            "draining"
        } else {
            "ok"
        }
    }
}

#[derive(Debug)]
pub struct ConnectionRateLimiter {
    window_started_at: Option<Instant>,
    count: u64,
}

impl ConnectionRateLimiter {
    pub fn new() -> Self {
        Self {
            window_started_at: None,
            count: 0,
        }
    }

    pub fn allow(&mut self, now: Instant, window_ms: u64, max_messages: u64) -> bool {
        if window_ms == 0 || max_messages == 0 {
            return true;
        }

        let window = Duration::from_millis(window_ms);
        if self
            .window_started_at
            .is_none_or(|started_at| now.duration_since(started_at) >= window)
        {
            self.window_started_at = Some(now);
            self.count = 0;
        }

        self.count = self.count.saturating_add(1);
        self.count <= max_messages
    }
}

#[derive(Debug, Default)]
pub struct PlayerMessageRateLimiter {
    windows: HashMap<String, PlayerMessageRateWindow>,
}

#[derive(Debug)]
struct PlayerMessageRateWindow {
    window_started_at: Instant,
    count: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputAnomalyKind {
    Duplicate,
    Expired,
    Future,
    Timestamp,
}

impl InputAnomalyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Duplicate => "duplicate",
            Self::Expired => "expired",
            Self::Future => "future",
            Self::Timestamp => "timestamp",
        }
    }
}

#[derive(Debug, Default)]
pub struct PlayerInputAnomalyTracker {
    windows: HashMap<String, PlayerInputAnomalyWindow>,
}

#[derive(Debug)]
struct PlayerInputAnomalyWindow {
    window_started_at: Instant,
    count: u64,
    last_room_id: Option<String>,
    last_frame_id: Option<u32>,
    last_input_fingerprint: Option<String>,
}

impl PlayerInputAnomalyTracker {
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
        }
    }

    pub fn record(
        &mut self,
        player_id: &str,
        now: Instant,
        window_ms: u64,
        max_anomalies: u64,
    ) -> InputAnomalyRecordOutcome {
        if window_ms == 0 {
            self.windows.clear();
            return InputAnomalyRecordOutcome {
                count: 0,
                blocked: false,
            };
        }

        self.cleanup_expired(now, window_ms);

        let window = Duration::from_millis(window_ms);
        let entry = self
            .windows
            .entry(player_id.to_string())
            .or_insert(PlayerInputAnomalyWindow {
                window_started_at: now,
                count: 0,
                last_room_id: None,
                last_frame_id: None,
                last_input_fingerprint: None,
            });

        if now.saturating_duration_since(entry.window_started_at) >= window {
            entry.window_started_at = now;
            entry.count = 0;
        }

        if entry.count == 0 {
            entry.window_started_at = now;
        }

        entry.count = entry.count.saturating_add(1);
        InputAnomalyRecordOutcome {
            count: entry.count,
            blocked: max_anomalies > 0 && entry.count >= max_anomalies,
        }
    }

    pub fn remember_frame(
        &mut self,
        player_id: &str,
        room_id: &str,
        frame_id: u32,
        input_fingerprint: &str,
        now: Instant,
        window_ms: u64,
    ) -> bool {
        if window_ms == 0 {
            self.windows.clear();
            return false;
        }

        self.cleanup_expired(now, window_ms);

        let window = Duration::from_millis(window_ms);
        let entry = self
            .windows
            .entry(player_id.to_string())
            .or_insert(PlayerInputAnomalyWindow {
                window_started_at: now,
                count: 0,
                last_room_id: None,
                last_frame_id: None,
                last_input_fingerprint: None,
            });

        if now.saturating_duration_since(entry.window_started_at) >= window {
            entry.window_started_at = now;
            entry.count = 0;
            entry.last_room_id = None;
            entry.last_frame_id = None;
            entry.last_input_fingerprint = None;
        }

        let duplicate = entry.last_room_id.as_deref() == Some(room_id)
            && entry.last_frame_id == Some(frame_id)
            && entry.last_input_fingerprint.as_deref() == Some(input_fingerprint);
        entry.last_room_id = Some(room_id.to_string());
        entry.last_frame_id = Some(frame_id);
        entry.last_input_fingerprint = Some(input_fingerprint.to_string());
        duplicate
    }

    pub fn is_blocked(
        &mut self,
        player_id: &str,
        now: Instant,
        window_ms: u64,
        max_anomalies: u64,
    ) -> bool {
        if window_ms == 0 || max_anomalies == 0 {
            if window_ms == 0 {
                self.windows.clear();
            }
            return false;
        }

        self.cleanup_expired(now, window_ms);
        self.windows
            .get(player_id)
            .is_some_and(|entry| entry.count >= max_anomalies)
    }

    pub fn cleanup_expired(&mut self, now: Instant, window_ms: u64) -> usize {
        if window_ms == 0 {
            let removed = self.windows.len();
            self.windows.clear();
            return removed;
        }

        let window = Duration::from_millis(window_ms);
        let before = self.windows.len();
        self.windows
            .retain(|_, entry| now.saturating_duration_since(entry.window_started_at) < window);
        before.saturating_sub(self.windows.len())
    }

    #[cfg(test)]
    pub fn tracked_player_count(&self) -> usize {
        self.windows.len()
    }

    #[cfg(test)]
    pub fn anomaly_count(&self, player_id: &str) -> u64 {
        self.windows
            .get(player_id)
            .map(|entry| entry.count)
            .unwrap_or_default()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct InputAnomalyRecordOutcome {
    pub count: u64,
    pub blocked: bool,
}

impl PlayerMessageRateLimiter {
    pub fn new() -> Self {
        Self {
            windows: HashMap::new(),
        }
    }

    pub fn allow(
        &mut self,
        player_id: &str,
        now: Instant,
        window_ms: u64,
        max_messages: u64,
    ) -> bool {
        if window_ms == 0 || max_messages == 0 {
            self.windows.clear();
            return true;
        }

        self.cleanup_expired(now, window_ms);

        let window = Duration::from_millis(window_ms);
        let entry = self
            .windows
            .entry(player_id.to_string())
            .or_insert(PlayerMessageRateWindow {
                window_started_at: now,
                count: 0,
            });

        if now.saturating_duration_since(entry.window_started_at) >= window {
            entry.window_started_at = now;
            entry.count = 0;
        }

        entry.count = entry.count.saturating_add(1);
        entry.count <= max_messages
    }

    pub fn cleanup_expired(&mut self, now: Instant, window_ms: u64) -> usize {
        if window_ms == 0 {
            let removed = self.windows.len();
            self.windows.clear();
            return removed;
        }

        let window = Duration::from_millis(window_ms);
        let before = self.windows.len();
        self.windows
            .retain(|_, entry| now.saturating_duration_since(entry.window_started_at) < window);
        before.saturating_sub(self.windows.len())
    }

    #[cfg(test)]
    pub fn tracked_player_count(&self) -> usize {
        self.windows.len()
    }
}

pub fn preauth_message_allowed(
    session_state: SessionState,
    message_type: Option<MessageType>,
) -> bool {
    session_state == SessionState::Authenticated
        || matches!(
            message_type,
            Some(MessageType::AuthReq) | Some(MessageType::PingReq)
        )
}

struct ConnectionCountGuard {
    connection_count: Arc<AtomicU64>,
}

impl Drop for ConnectionCountGuard {
    fn drop(&mut self) {
        self.connection_count.fetch_sub(1, Ordering::Relaxed);
    }
}

struct GameServerResources {
    health_state: HealthState,
    redis_client: Option<redis::Client>,
    worker_lease: Option<WorkerLease>,
    registry_client: Option<Arc<RegistryClient>>,
    registry_started: bool,
    socket_names: Vec<crate::local_socket::OwnedSocketPath>,
    tasks: Vec<JoinHandle<()>>,
    connection_tasks: Arc<tokio::sync::Mutex<Vec<JoinHandle<()>>>>,
    convergence_tasks: Vec<ConvergenceTask>,
    db_store: Option<PgAuditStore>,
    player_store: Option<PgPlayerStore>,
    character_store: Option<PgCharacterElementStore>,
    discipline_store: Option<PgDisciplineStore>,
    title_store: Option<PgTitleStore>,
}

impl GameServerResources {
    fn new(health_state: HealthState) -> Self {
        Self {
            health_state,
            redis_client: None,
            worker_lease: None,
            registry_client: None,
            registry_started: false,
            socket_names: Vec::new(),
            tasks: Vec::new(),
            connection_tasks: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            convergence_tasks: Vec::new(),
            db_store: None,
            player_store: None,
            character_store: None,
            discipline_store: None,
            title_store: None,
        }
    }

    async fn verify_worker_lease_ownership(&self) -> Result<(), String> {
        let lease = self
            .worker_lease
            .as_ref()
            .ok_or_else(|| "worker lease unavailable during destructive cleanup".to_string())?;
        let client = self
            .redis_client
            .as_ref()
            .ok_or_else(|| "redis client unavailable during ownership verification".to_string())?;
        match timeout(CLEANUP_OPERATION_TIMEOUT, async {
            let mut redis = client
                .get_multiplexed_async_connection()
                .await
                .map_err(|error| error.to_string())?;
            lease
                .owns_redis(&mut redis)
                .await
                .map_err(|error| error.to_string())
        })
        .await
        {
            Ok(Ok(true)) => Ok(()),
            Ok(Ok(false)) => Err(
                "worker lease token no longer owns key; destructive cleanup skipped".to_string(),
            ),
            Ok(Err(error)) => Err(format!(
                "worker lease ownership verification failed; destructive cleanup skipped: {error}"
            )),
            Err(_) => Err(
                "worker lease ownership verification timed out; destructive cleanup skipped"
                    .to_string(),
            ),
        }
    }
}

async fn close_store_with_timeout<F>(name: &str, close: F, errors: &mut Vec<String>)
where
    F: Future<Output = ()>,
{
    if timeout(CLEANUP_OPERATION_TIMEOUT, close).await.is_err() {
        errors.push(format!("timed out closing {name} store"));
    }
}

fn collect_task_join_error(
    name: &str,
    result: Result<(), tokio::task::JoinError>,
    errors: &mut Vec<String>,
) {
    if let Err(error) = result
        && !error.is_cancelled()
    {
        errors.push(format!("{name} task failed: {error}"));
    }
}

impl CleanupExecutor for GameServerResources {
    async fn execute(&mut self, step: CleanupStep) -> Result<(), String> {
        match step {
            CleanupStep::StopBackgroundTasks => {
                self.health_state.mark_shutting_down();
                let mut errors = Vec::new();
                for task in self.convergence_tasks.drain(..) {
                    collect_task_join_error(
                        "convergence",
                        task.stop_and_wait_result().await,
                        &mut errors,
                    );
                }
                for task in self.tasks.drain(..) {
                    task.abort();
                    collect_task_join_error("background", task.await, &mut errors);
                }
                let connection_tasks = {
                    let mut tasks = self.connection_tasks.lock().await;
                    std::mem::take(&mut *tasks)
                };
                for task in connection_tasks {
                    task.abort();
                    collect_task_join_error("connection", task.await, &mut errors);
                }
                if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors.join("; "))
                }
            }
            CleanupStep::ReleaseListenersAndSockets => {
                if self.socket_names.is_empty() {
                    return Ok(());
                }
                self.verify_worker_lease_ownership().await?;
                let mut errors = Vec::new();
                for socket in self.socket_names.drain(..) {
                    if let Err(error) = crate::local_socket::remove_owned_socket_path(&socket) {
                        errors.push(format!("failed to remove socket {}: {error}", socket.name));
                    }
                }
                if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors.join("; "))
                }
            }
            CleanupStep::DeregisterInstance => {
                if !self.registry_started {
                    return Ok(());
                }
                self.verify_worker_lease_ownership().await?;
                self.registry_started = false;
                let Some(client) = &self.registry_client else {
                    return Ok(());
                };
                match timeout(CLEANUP_OPERATION_TIMEOUT, client.deregister()).await {
                    Ok(result) => result.map_err(|error| error.to_string()),
                    Err(_) => Err("timed out deregistering service instance".to_string()),
                }
            }
            CleanupStep::ReleaseWorkerLease => {
                let Some(lease) = self.worker_lease.take() else {
                    return Ok(());
                };
                let Some(client) = &self.redis_client else {
                    lease.deactivate();
                    return Err("redis client unavailable during worker lease release".to_string());
                };
                lease.deactivate();
                match timeout(CLEANUP_OPERATION_TIMEOUT, async {
                    let mut redis = client
                        .get_multiplexed_async_connection()
                        .await
                        .map_err(|error| error.to_string())?;
                    match lease.release_redis(&mut redis).await {
                        Ok(true) => Ok(()),
                        Ok(false) => Err("worker lease token no longer owns the key".to_string()),
                        Err(error) => Err(error.to_string()),
                    }
                })
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err("timed out releasing worker lease".to_string()),
                }
            }
            CleanupStep::CloseStores => {
                let mut errors = Vec::new();
                if let Some(store) = &self.player_store {
                    close_store_with_timeout("player", store.close(), &mut errors).await;
                }
                if let Some(store) = &self.character_store {
                    close_store_with_timeout("character", store.close(), &mut errors).await;
                }
                if let Some(store) = &self.discipline_store {
                    close_store_with_timeout("discipline", store.close(), &mut errors).await;
                }
                if let Some(store) = &self.title_store {
                    close_store_with_timeout("title", store.close(), &mut errors).await;
                }
                if let Some(store) = &self.db_store {
                    close_store_with_timeout("audit", store.close(), &mut errors).await;
                }
                if errors.is_empty() {
                    Ok(())
                } else {
                    Err(errors.join("; "))
                }
            }
        }
    }
}

pub async fn run(
    config: &Config,
    config_tables: ConfigTableRuntime,
    health_state: HealthState,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut resources = GameServerResources::new(health_state.clone());
    let run_result: Result<(), Box<dyn std::error::Error>> = async {
        let lease_wait = LeaseWaitConfig::try_from_env()?;
        let socket_reclaim = crate::local_socket::SocketReclaimConfig::try_from_env()?;
        let drain_shutdown_config = DrainShutdownConfig::try_from_env()?;
        let global_id_origin_id = u16::try_from(config.global_id_origin_id).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "GLOBAL_ID_ORIGIN_ID out of range: {}",
                    config.global_id_origin_id
                ),
            )
        })?;
        let global_id_worker_id = config
            .global_id_worker_id
            .map(|worker_id| {
                u8::try_from(worker_id).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("GLOBAL_ID_WORKER_ID out of range: {worker_id}"),
                    )
                })
            })
            .transpose()?;

        let registry_client = if config.registry_enabled {
            Some(Arc::new(
                RegistryClient::new_lazy(
                    &config.registry_url,
                    &config.service_name,
                    &config.service_instance_id,
                )
                .map_err(|error| std::io::Error::other(error.to_string()))?
                .with_key_prefix(config.registry_key_prefix.clone())
                .with_heartbeat_interval(config.registry_heartbeat_interval_secs),
            ))
        } else {
            tracing::info!("service registry disabled");
            None
        };
        resources.registry_client = registry_client.clone();

        // Required external stores are initialized before claiming the worker identity.
        let db_store = PgAuditStore::new(config).await?;
        resources.db_store = Some(db_store.clone());
        let db_player_store = PgPlayerStore::new(config).await?;
        resources.player_store = Some(db_player_store.clone());
        let character_element_store = PgCharacterElementStore::new(config).await?;
        resources.character_store = Some(character_element_store.clone());
        let discipline_store = PgDisciplineStore::new(config).await?;
        resources.discipline_store = Some(discipline_store.clone());
        let title_store = PgTitleStore::new(config).await?;
        resources.title_store = Some(title_store.clone());
        health_state.mark_ready("local-runtime", "gameplay-stores");

        let redis_client = redis::Client::open(config.redis_url.clone())?;
        resources.redis_client = Some(redis_client.clone());
        let lease_redis_client = redis_client.clone();
        let redis_key_prefix = config.redis_key_prefix.clone();
        let service_name = config.service_name.clone();
        let service_instance_id = config.service_instance_id.clone();
        let worker_lease = wait_for_worker_lease(
            lease_wait,
            move || {
                let client = lease_redis_client.clone();
                let redis_key_prefix = redis_key_prefix.clone();
                let service_name = service_name.clone();
                let service_instance_id = service_instance_id.clone();
                async move {
                    let mut redis = client
                        .get_multiplexed_async_connection()
                        .await
                        .map_err(|error| error.to_string())?;
                    WorkerLease::acquire_redis(
                        &mut redis,
                        &redis_key_prefix,
                        global_id_origin_id,
                        global_id_worker_id,
                        &service_name,
                        &service_instance_id,
                        DEFAULT_WORKER_LEASE_TTL_SECONDS,
                    )
                    .await
                    .map_err(|error| error.to_string())
                }
            },
            shutdown_signal(),
        )
        .await
        .map_err(|error| match error {
            LeaseWaitError::TimedOut { attempts, .. } => std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("global id worker lease wait timed out after {attempts} attempts"),
            ),
            LeaseWaitError::Cancelled { attempts } => std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                format!("global id worker lease wait cancelled after {attempts} attempts"),
            ),
        })?;

        let mut ownership = StartupOwnership::default();
        ownership.claim(OwnedResource::WorkerLease)?;
        info!(
            origin_id = worker_lease.origin_id,
            worker_id = worker_lease.worker_id,
            lease_key = %worker_lease.key,
            "global id worker lease acquired"
        );
        health_state.mark_ready("local-runtime", "worker-lease");
        resources.worker_lease = Some(worker_lease.clone());

        let lease_renew_client = redis_client.clone();
        let lease_for_renewal = worker_lease.clone();
        let (lease_loss_tx, mut lease_loss_rx) = watch::channel(false);
        resources.tasks.push(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(
                    DEFAULT_WORKER_LEASE_RENEW_INTERVAL_SECONDS,
                ))
                .await;
                let lease_is_active = match lease_renew_client
                    .get_multiplexed_async_connection()
                    .await
                {
                    Ok(mut redis) => match lease_for_renewal.renew_redis(&mut redis).await {
                        Ok(active) => active,
                        Err(error) => {
                            warn!(lease_key = %lease_for_renewal.key, error = %error, "global id worker lease renewal failed");
                            false
                        }
                    },
                    Err(error) => {
                        lease_for_renewal.deactivate();
                        warn!(lease_key = %lease_for_renewal.key, error = %error, "global id worker lease renewal failed");
                        false
                    }
                };
                if !lease_is_active {
                    warn!(lease_key = %lease_for_renewal.key, "global id worker lease lost; requesting process shutdown");
                    let _ = lease_loss_tx.send(true);
                    break;
                }
            }
        }));

        ownership.claim(OwnedResource::NetworkListeners)?;
        let tcp_listener = TcpListener::bind(config.bind_addr()).await?;
        let admin_listener = TcpListener::bind(config.admin_bind_addr()).await?;
        ownership.claim(OwnedResource::LocalSockets)?;
        let owned_socket_targets = vec![
            config.local_socket_name.clone(),
            config.internal_socket_name.clone(),
        ];
        crate::local_socket::prepare_owned_socket_root(&owned_socket_targets, true)?;
        let local_socket_listener = crate::local_socket::create_owned_listener(
            &config.local_socket_name,
            &owned_socket_targets,
            true,
            socket_reclaim,
        )
        .await?;
        resources
            .socket_names
            .push(crate::local_socket::capture_owned_socket(
                &config.local_socket_name,
            )?);
        let internal_socket_listener = crate::local_socket::create_owned_listener(
            &config.internal_socket_name,
            &owned_socket_targets,
            true,
            socket_reclaim,
        )
        .await?;
        resources
            .socket_names
            .push(crate::local_socket::capture_owned_socket(
                &config.internal_socket_name,
            )?);
        health_state.mark_ready("local-runtime", "server-listeners");

    // Initialize MatchClient for communicating with MatchService
    let match_client = crate::match_client::create_match_client_shared();
    let match_client_config = MatchClientConfig::from_env().await;
    if match_client_config.registry_enabled {
        health_state.mark_pending("match-service", "grpc", StartupErrorCode::DependencyPending);
    } else if let Err(e) = init_match_client(&match_client, match_client_config.clone()).await {
        health_state.mark_degraded("match-service", "grpc", StartupErrorCode::DependencyPending);
        tracing::error!(error = %e, "failed to connect to local match-service fallback, match notifications will be disabled");
    } else {
        health_state.mark_ready("match-service", "grpc");
    }
    if let Some(task) = spawn_match_client_rediscovery(
        match_client.clone(),
        match_client_config.clone(),
        health_state.clone(),
    ) {
        resources.convergence_tasks.push(task);
    }

    let item_uid_generator =
        crate::core::global_id::ItemUidGenerator::from_worker_lease(&worker_lease)?;

    let room_logic_factory: SharedRoomLogicFactory =
        Arc::new(GameRoomLogicFactory::new(config_tables.clone()));
    let (drain_state_tx, mut drain_state_rx) = watch::channel(false);
    let (drain_shutdown, drain_shutdown_arm_rx) = DrainShutdownControl::channel();
    let shared_state = ServerSharedState {
        room_manager: Arc::new(RoomManager::with_policy_registry_and_cleanup_interval(
            match_client.clone(),
            room_logic_factory,
            config_tables.room_policy_registry(),
            config.room_cleanup_interval_secs,
        )),
        runtime_config: Arc::new(RwLock::new(RuntimeConfig {
            heartbeat_timeout_secs: config.heartbeat_timeout_secs,
            max_body_len: config.max_body_len,
            msg_rate_window_ms: config.msg_rate_window_ms,
            msg_rate_max: config.msg_rate_max,
            player_msg_rate_window_ms: config.player_msg_rate_window_ms,
            player_msg_rate_max: config.player_msg_rate_max,
            input_timestamp_required: config.input_timestamp_required,
            input_timestamp_max_skew_ms: config.input_timestamp_max_skew_ms,
            input_anomaly_window_ms: config.input_anomaly_window_ms,
            input_anomaly_max: config.input_anomaly_max,
            drain_mode_enabled: false,
            drain_mode_entered_at_ms: None,
            drain_mode_reason: DEFAULT_DRAIN_MODE_REASON.to_string(),
            drain_mode_source: DEFAULT_DRAIN_MODE_SOURCE.to_string(),
            drain_state_tx,
            drain_shutdown: drain_shutdown.clone(),
        })),
        connection_count: Arc::new(AtomicU64::new(0)),
        online_player_count: Arc::new(AtomicU64::new(0)),
        player_msg_rate_limiter: Arc::new(tokio::sync::Mutex::new(PlayerMessageRateLimiter::new())),
        player_input_anomaly_tracker: Arc::new(tokio::sync::Mutex::new(
            PlayerInputAnomalyTracker::new(),
        )),
        shutdown_signal: Arc::new(Notify::new()),
    };

    resources.tasks.push(tokio::spawn(run_drain_shutdown_monitor(
        shared_state.runtime_config.read().await.drain_state_tx.subscribe(),
        drain_shutdown_arm_rx,
        drain_shutdown,
        shared_state.connection_count.clone(),
        shared_state.room_manager.clone(),
        config.service_instance_id.clone(),
        shared_state.shutdown_signal.clone(),
        drain_shutdown_config,
    )));

    let character_element_facade =
        CharacterElementFacade::new(Arc::new(character_element_store.clone()));
    let title_config_tables = config_tables.clone();
    let title_unlock_config_tables = config_tables.clone();
    let discipline_service = DisciplineService::new(discipline_store);
    let title_service = TitleService::new(title_store, title_config_tables);
    let title_unlock_service = TitleUnlockService::new(
        title_service.clone(),
        discipline_service.clone(),
        character_element_facade.clone(),
        title_unlock_config_tables,
    );
    let character_progress_service = CharacterProgressService::new(
        character_element_facade.clone(),
        discipline_service.clone(),
        title_service.clone(),
    );
    let character_push_service = crate::core::character_push::CharacterPushService::new();
    let player_registry: PlayerRegistry = PlayerRegistry::default();
    let player_manager = PlayerManager::new(db_player_store);
    let (activity_engine, reward_mail_dispatcher) = if config.activity_enabled {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(config.db_pool_size.max(1))
            .connect(&config.database_url)
            .await
            .map_err(|error| {
                std::io::Error::other(format!("activity database connection failed: {error}"))
            })?;
        let inventory = crate::core::inventory::PlayerManagerRewardInventoryPort::new(
            player_manager.clone(),
            config_tables.clone(),
            item_uid_generator.clone(),
        );
        let delivery_store = crate::core::inventory::PgRewardDeliveryStore::from_pool(pool.clone())
            .await
            .map_err(|error| {
                std::io::Error::other(format!("activity reward store failed: {error}"))
            })?;
        let delivery = Arc::new(crate::core::inventory::RewardDeliveryService::new(
            inventory.clone(),
            delivery_store,
            crate::core::inventory::NoopRewardDeliveryNotifier,
        ));
        let lottery_gateway = crate::activity::PlayerManagerLotteryAssetGateway::new(
            player_manager.clone(),
            inventory,
            config_tables.clone(),
            item_uid_generator.clone(),
            delivery.clone(),
        );
        let activity_engine = crate::activity::ActivityEngine::postgres(pool.clone(), delivery)
            .with_lottery_asset_gateway(Arc::new(lottery_gateway))
            .with_cache(Arc::new(crate::activity::RedisActivityCache::new(
                redis_client.clone(),
                config.redis_key_prefix.clone(),
            )));
        let dispatcher = if config.reward_mail_dispatch_enabled {
            let registry = registry_client.clone().ok_or_else(|| {
                std::io::Error::other(
                    "reward mail dispatch requires service registry endpoint discovery",
                )
            })?;
            let store = crate::core::inventory::PgRewardMailDispatchStore::new(pool).await?;
            let client = crate::core::inventory::RegistryRewardMailClient::new(
                registry,
                config.mail_service_token.clone(),
            )
            .map_err(std::io::Error::other)?;
            Some(crate::core::inventory::RewardMailDispatcher::new(
                store,
                client,
                config.service_instance_id.clone(),
            ))
        } else {
            None
        };
        (activity_engine, dispatcher)
    } else {
        (crate::activity::ActivityEngine::disabled(), None)
    };

    let hot_reload_runtime = config_tables.clone();
    let services = ServiceContext {
        config: config.clone(),
        db_store: db_store.clone(),
        room_manager: shared_state.room_manager.clone(),
        match_client,
        runtime_config: shared_state.runtime_config.clone(),
        connection_count: shared_state.connection_count.clone(),
        config_tables,
        item_uid_generator,
        player_manager,
        character_element_facade,
        discipline_service,
        title_service,
        character_progress_service,
        title_unlock_service,
        character_push_service,
        activity_engine,
        online_player_count: shared_state.online_player_count.clone(),
        player_registry: player_registry.clone(),
        online_route_coordinator: Default::default(),
        player_msg_rate_limiter: shared_state.player_msg_rate_limiter.clone(),
        player_input_anomaly_tracker: shared_state.player_input_anomaly_tracker.clone(),
        shutdown_signal: shared_state.shutdown_signal.clone(),
    };
    info!(
        addr = %config.bind_addr(),
        admin_addr = %config.admin_bind_addr(),
        local_socket_name = %config.local_socket_name,
        internal_socket_name = %config.internal_socket_name,
        redis_configured = true,
        db_enabled = db_store.enabled(),
        "game server listening"
    );

    if let Some(dispatcher) = reward_mail_dispatcher {
        resources
            .tasks
            .push(tokio::spawn(dispatcher.run(Duration::from_secs(1))));
    }

    let health_task = service_registry::readiness::spawn_health_from_env(health_state.clone())
        .await?
        .into_iter();
        resources.tasks.extend(health_task);

        if let Some(client) = registry_client {
            resources.registry_started = true;
            resources.convergence_tasks.push(spawn_registry_publication(
                client,
                crate::build_service_instance(config),
                health_state.clone(),
                ConvergenceConfig {
                    steady_interval: Duration::from_secs(1),
                    ..ConvergenceConfig::default()
                },
            ));
        }

        if config.csv_reload_enabled {
            resources.tasks.push(spawn_hot_reload_task(
                hot_reload_runtime,
                Duration::from_secs(config.csv_reload_interval_secs),
            ));
        } else {
            tracing::info!(csv_dir = %config.csv_dir, "csv config hot reload disabled");
        }
        let metrics_nats_url = config.nats_url.clone();
        let metrics_instance_id = config.service_instance_id.clone();
        resources.tasks.push(tokio::spawn(async move {
            crate::metrics::METRICS
                .start_reporting(&metrics_nats_url, metrics_instance_id, 5)
                .await;
        }));

        let (fatal_task_tx, mut fatal_task_rx) = mpsc::unbounded_channel::<String>();
        let fatal_admin_tx = fatal_task_tx.clone();
        let admin_shutdown_signal = shared_state.shutdown_signal.clone();
        let admin_room_manager = shared_state.room_manager.clone();
        let admin_runtime_config = shared_state.runtime_config.clone();
        let admin_connection_count = shared_state.connection_count.clone();
        let admin_player_registry = services.player_registry.clone();
        let admin_player_manager = services.player_manager.clone();
        let admin_config_tables = services.config_tables.clone();
        let admin_item_uid_generator = services.item_uid_generator.clone();
        let admin_redis_client = redis_client.clone();
        let admin_redis_key_prefix = config.redis_key_prefix.clone();
        let admin_owner_server_id = config.service_instance_id.clone();
        let admin_token = config.admin_token.clone();
        let admin_assertion_verifier = crate::admin_server::AdminAssertionVerifier::new(
            config.admin_assertion_issuer.clone(),
            &config.admin_assertion_public_keys,
            config.admin_assertion_max_ttl_ms,
        );
        let mail_assertion_verifier = crate::admin_server::MailGrantAssertionVerifier::new(
            config.mail_grant_assertion_issuer.clone(),
            &config.mail_grant_assertion_public_keys,
            config.mail_grant_assertion_max_ttl_ms,
        );
        let admin_audit_logger =
            crate::admin_server::AdminAuditLogger::new(crate::admin_server::AdminAuditConfig::new(
                config.admin_audit_enabled,
                config.admin_audit_path.clone(),
                config.admin_audit_require_actor,
            ));
        resources.tasks.push(tokio::spawn(async move {
            if let Err(error) = crate::admin_server::run_listener(
                admin_listener,
                admin_room_manager,
                admin_runtime_config,
                admin_connection_count,
                admin_player_registry,
                admin_player_manager,
                admin_config_tables,
                admin_item_uid_generator,
                admin_redis_client,
                admin_redis_key_prefix,
                admin_owner_server_id,
                admin_token,
                admin_assertion_verifier,
                mail_assertion_verifier,
                admin_audit_logger,
                admin_shutdown_signal,
            )
            .await
            {
                error!(error = %error, "admin listener stopped unexpectedly");
                let _ = fatal_admin_tx.send(format!("admin listener failed: {error}"));
            }
        }));

        let fatal_local_tx = fatal_task_tx.clone();
        let local_redis_client = resources.redis_client.as_ref().unwrap().clone();
        let local_services = services.clone();
        let local_runtime_config = shared_state.runtime_config.clone();
        let local_drain_state_rx = local_runtime_config.read().await.drain_state_tx.subscribe();
        let local_connection_count = shared_state.connection_count.clone();
        let local_connection_tasks = Arc::clone(&resources.connection_tasks);
        resources.tasks.push(tokio::spawn(async move {
            if let Err(error) = run_local_socket_listener(
                local_socket_listener,
                local_redis_client,
                local_services,
                local_runtime_config,
                local_drain_state_rx,
                local_connection_count,
                local_connection_tasks,
            )
            .await
            {
                error!(error = %error, "proxy-local listener stopped unexpectedly");
                let _ = fatal_local_tx.send(format!("proxy-local listener failed: {error}"));
            }
        }));

        let fatal_internal_tx = fatal_task_tx.clone();
        let internal_services = services.clone();
        let internal_token = config.internal_token.clone();
        resources.tasks.push(tokio::spawn(async move {
            if let Err(error) = crate::internal_server::run_listener(
                internal_socket_listener,
                internal_services,
                internal_token,
            )
            .await
            {
                error!(error = %error, "internal listener stopped unexpectedly");
                let _ = fatal_internal_tx.send(format!("internal listener failed: {error}"));
            }
        }));
        drop(fatal_task_tx);

        resources.tasks.push(tokio::spawn(crate::kick_subscriber::subscribe_session_kicks(
        config.nats_url.clone(),
        player_registry.clone(),
    )));
        resources.tasks.push(tokio::spawn(crate::gm_broadcast::subscribe_gm_broadcasts(
        config.nats_url.clone(),
        player_registry,
    )));
    let mut next_session_id: u64 = 1;
    let mut lease_lost = false;
    let mut fatal_task_error = None;

    loop {
        let mut drain_state_changed = false;
        let accept_result = tokio::select! {
            biased;
            changed = drain_state_rx.changed() => {
                if changed.is_err() {
                    fatal_task_error = Some("drain state channel closed".to_string());
                } else {
                    drain_state_changed = true;
                }
                None
            },
            result = tcp_listener.accept() => Some(result),
            _ = shared_state.shutdown_signal.notified() => None,
            _ = shutdown_signal() => None,
            fatal = fatal_task_rx.recv() => {
                fatal_task_error = Some(fatal.unwrap_or_else(|| "critical listener task channel closed".to_string()));
                None
            },
            changed = lease_loss_rx.changed() => {
                if changed.is_err() || *lease_loss_rx.borrow_and_update() {
                    lease_lost = true;
                }
                None
            },
        };

        if drain_state_changed {
            if *drain_state_rx.borrow_and_update() {
                health_state.mark_degraded(
                    "local-runtime",
                    "server-listeners",
                    StartupErrorCode::DependencyPending,
                );
                info!("drain mode active; player listeners remain available for existing-session reconnects");
            } else {
                health_state.mark_ready("local-runtime", "server-listeners");
                info!("drain mode disabled; player listeners resumed accepting connections");
            }
            continue;
        }

        let Some((socket, peer_addr)) = accept_result.transpose()? else {
            if lease_lost {
                warn!("global id worker lease lost, stopping game server accept loop");
            } else {
                info!("shutdown signal received, stopping game server accept loop");
            }
            break;
        };

        let session_id = next_session_id;
        next_session_id += 1;

        spawn_connection_task(
            socket,
            peer_addr.to_string(),
            session_id,
            redis_client.clone(),
            services.clone(),
            shared_state.runtime_config.clone(),
            shared_state.connection_count.clone(),
            db_store.clone(),
            Arc::clone(&resources.connection_tasks),
        )
        .await;
    }

        if let Some(error) = fatal_task_error {
            Err(std::io::Error::other(error).into())
        } else if lease_lost {
            Err(std::io::Error::other("global id worker lease lost").into())
        } else {
            Ok(())
        }
    }
    .await;

    let (run_result, cleanup_report) =
        run_then_cleanup(std::future::ready(run_result), &mut resources).await;
    for (step, cleanup_error) in &cleanup_report.failures {
        error!(cleanup_step = ?step, error = %cleanup_error, "game-server cleanup step failed");
    }
    info!(
        cleanup_failures = cleanup_report.failures.len(),
        "game server shutdown completed"
    );

    match (run_result, cleanup_report.failures.is_empty()) {
        (Err(error), _) => Err(error),
        (Ok(()), true) => Ok(()),
        (Ok(()), false) => Err(std::io::Error::other(format!(
            "game-server shutdown cleanup failed in {} step(s)",
            cleanup_report.failures.len()
        ))
        .into()),
    }
}

async fn run_local_socket_listener(
    listener: interprocess::local_socket::tokio::Listener,
    redis_client: redis::Client,
    services: ServiceContext,
    runtime_config: Arc<RwLock<RuntimeConfig>>,
    mut drain_state_rx: watch::Receiver<bool>,
    connection_count: Arc<AtomicU64>,
    connection_tasks: Arc<tokio::sync::Mutex<Vec<JoinHandle<()>>>>,
) -> Result<(), std::io::Error> {
    let mut next_session_id = 1_000_000u64;
    loop {
        let socket = tokio::select! {
            biased;
            changed = drain_state_rx.changed() => {
                changed.map_err(|_| std::io::Error::other("drain state channel closed"))?;
                continue;
            }
            socket = listener.accept() => socket?,
        };
        let session_id = next_session_id;
        next_session_id = next_session_id.saturating_add(1);
        spawn_connection_task(
            socket,
            format!("local:{}", session_id),
            session_id,
            redis_client.clone(),
            services.clone(),
            runtime_config.clone(),
            connection_count.clone(),
            services.db_store.clone(),
            Arc::clone(&connection_tasks),
        )
        .await;
    }
}

async fn run_drain_shutdown_monitor(
    mut drain_state_rx: watch::Receiver<bool>,
    mut arm_rx: mpsc::Receiver<()>,
    control: Arc<DrainShutdownControl>,
    connection_count: Arc<AtomicU64>,
    room_manager: Arc<RoomManager>,
    owner_server_id: String,
    shutdown_signal: Arc<Notify>,
    config: DrainShutdownConfig,
) {
    while arm_rx.recv().await.is_some() {
        if !*drain_state_rx.borrow() {
            control.disarm();
            continue;
        }
        let started_at = tokio::time::Instant::now();
        loop {
            if !*drain_state_rx.borrow() {
                control.disarm();
                break;
            }
            let snapshot = room_manager
                .rollout_drain_snapshot(
                    &owner_server_id,
                    crate::core::runtime::room_manager::ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT,
                )
                .await;
            match drain_shutdown_decision(
                connection_count.load(Ordering::Relaxed),
                snapshot.owned_room_count,
                snapshot.migrating_room_count,
                started_at.elapsed(),
                config.timeout,
            ) {
                DrainShutdownDecision::Shutdown => {
                    info!("drain completed within bounded window; requesting graceful shutdown");
                    shutdown_signal.notify_one();
                    return;
                }
                DrainShutdownDecision::TimedOut => {
                    warn!(
                        connection_count = connection_count.load(Ordering::Relaxed),
                        owned_room_count = snapshot.owned_room_count,
                        migrating_room_count = snapshot.migrating_room_count,
                        "drain shutdown window expired; active sessions and rooms remain protected"
                    );
                    control.disarm();
                    break;
                }
                DrainShutdownDecision::Wait => {}
            }
            tokio::select! {
                biased;
                changed = drain_state_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                _ = tokio::time::sleep(config.poll_interval) => {}
            }
        }
    }
}

async fn spawn_connection_task<S>(
    socket: S,
    peer_addr: String,
    session_id: u64,
    redis_client: redis::Client,
    services: ServiceContext,
    runtime_config: Arc<RwLock<RuntimeConfig>>,
    connection_count: Arc<AtomicU64>,
    db_store: PgAuditStore,
    connection_tasks: Arc<tokio::sync::Mutex<Vec<JoinHandle<()>>>>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    connection_count.fetch_add(1, Ordering::Relaxed);
    info!(session_id = session_id, peer = %peer_addr, "accepted game connection");
    db_store
        .append_connection_event(session_id, None, Some(&peer_addr), "connected", None)
        .await;

    let task = tokio::spawn(async move {
        let _connection_guard = ConnectionCountGuard { connection_count };
        if let Err(error) = handle_connection(
            socket,
            peer_addr,
            session_id,
            redis_client,
            services,
            runtime_config,
        )
        .await
        {
            warn!(session_id = session_id, error = %error, "connection task failed");
        }
    });
    let mut tracked = connection_tasks.lock().await;
    tracked.retain(|task| !task.is_finished());
    tracked.push(task);
}

async fn handle_connection<S>(
    socket: S,
    peer_addr: String,
    session_id: u64,
    redis_client: redis::Client,
    services: ServiceContext,
    runtime_config: Arc<RwLock<RuntimeConfig>>,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let redis = redis_client.get_multiplexed_async_connection().await?;
    let (mut reader, mut writer) = tokio::io::split(socket);
    let (tx, mut rx) = mpsc::channel::<OutboundMessage>(services.config.outbound_queue_capacity);
    let close_state = ConnectionCloseState::new();
    let mut connection = ConnectionContext {
        peer_addr,
        redis,
        session: Session::new(session_id),
        tx,
        close_state,
        kick_notify: Arc::new(Notify::new()),
        kick_reason: Arc::new(RwLock::new("session_kicked".to_string())),
    };
    let mut rate_limiter = ConnectionRateLimiter::new();
    let mut close_event_appended = false;
    let mut next_online_route_refresh_at: Option<Instant> = None;

    let writer_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let packet = encode_packet(message.message_type, message.seq, &message.body);
            if let Err(error) = writer.write_all(&packet).await {
                return Err(error);
            }
        }

        writer.shutdown().await?;
        Ok::<(), std::io::Error>(())
    });

    loop {
        let runtime = runtime_config.read().await.clone();
        let mut header_buf = [0u8; HEADER_LEN];

        // select! between kick notification and normal packet read
        let read_header = tokio::select! {
            _ = connection.kick_notify.notified() => {
                let kick_reason = connection.kick_reason.read().await.clone();
                info!(
                    session_id = connection.session.id,
                    account_player_id = ?connection.session.account_player_id,
                    character_id = ?connection.session.character_id,
                    reason = %kick_reason,
                    "session kicked"
                );
                if let Err(error) = connection.queue_message(
                    MessageType::SessionKickPush,
                    0,
                    SessionKickPush {
                        reason: kick_reason.clone(),
                        timestamp: current_unix_ms(),
                    },
                ) {
                    warn!(
                        session_id = connection.session.id,
                        error = %error,
                        "failed to queue session kick push"
                    );
                }
                append_connection_event_for_session(
                    &services.db_store,
                    &connection.session,
                    &connection.peer_addr,
                    "session_kicked",
                    Some(json!({ "reason": kick_reason })),
                )
                .await;
                break;
            }
            _ = connection.close_state.notified() => {
                let close_reason = connection
                    .close_state
                    .reason()
                    .unwrap_or_else(|| "server_close_requested".to_string());
                warn!(
                    session_id = connection.session.id,
                    account_player_id = ?connection.session.account_player_id,
                    character_id = ?connection.session.character_id,
                    peer = %connection.peer_addr,
                    reason = %close_reason,
                    "server requested connection close"
                );
                append_connection_event_for_session(
                    &services.db_store,
                    &connection.session,
                    &connection.peer_addr,
                    "server_close_requested",
                    Some(json!({ "reason": close_reason })),
                )
                .await;
                close_event_appended = true;
                break;
            }
            result = timeout(
                Duration::from_secs(runtime.heartbeat_timeout_secs),
                reader.read_exact(&mut header_buf),
            ) => result,
        };

        match read_header {
            Ok(Ok(_)) => {}
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                info!(session_id = connection.session.id, "peer closed connection");
                append_connection_event_for_session(
                    &services.db_store,
                    &connection.session,
                    &connection.peer_addr,
                    "closed",
                    None,
                )
                .await;
                break;
            }
            Ok(Err(error)) => {
                // Connection error (e.g., reset, broken pipe) - break to run cleanup
                warn!(session_id = connection.session.id, error = %error, "connection read error, will cleanup");
                break;
            }
            Err(_) => {
                if let Err(error) =
                    connection.queue_error(0, "HEARTBEAT_TIMEOUT", "connection timed out")
                {
                    warn!(
                        session_id = connection.session.id,
                        error = %error,
                        "failed to queue heartbeat timeout error"
                    );
                }
                append_connection_event_for_session(
                    &services.db_store,
                    &connection.session,
                    &connection.peer_addr,
                    "heartbeat_timeout",
                    None,
                )
                .await;
                break;
            }
        }

        let header = match parse_header(header_buf) {
            Ok(value) => value,
            Err(error_code) => {
                if let Err(error) = connection.queue_error(0, error_code, "invalid header") {
                    warn!(
                        session_id = connection.session.id,
                        error = %error,
                        "failed to queue invalid header error"
                    );
                }
                append_connection_event_for_session(
                    &services.db_store,
                    &connection.session,
                    &connection.peer_addr,
                    "invalid_header",
                    Some(json!({ "errorCode": error_code })),
                )
                .await;
                break;
            }
        };

        if header.body_len as usize > runtime.max_body_len {
            if let Err(error) =
                connection.queue_error(header.seq, "BODY_TOO_LARGE", "body too large")
            {
                warn!(
                    session_id = connection.session.id,
                    error = %error,
                    "failed to queue body too large error"
                );
            }
            if let Err(error) = discard_body(&mut reader, header.body_len as usize).await {
                warn!(
                    session_id = connection.session.id,
                    error = %error,
                    "failed to discard oversized body"
                );
            }
            append_connection_event_for_session(
                &services.db_store,
                &connection.session,
                &connection.peer_addr,
                "body_too_large",
                Some(json!({
                    "seq": header.seq,
                    "bodyLen": header.body_len,
                    "maxBodyLen": runtime.max_body_len
                })),
            )
            .await;
            break;
        }

        let mut body = vec![0u8; header.body_len as usize];
        if let Err(error) = reader.read_exact(&mut body).await {
            warn!(
                session_id = connection.session.id,
                error = %error,
                "connection body read error, will cleanup"
            );
            append_connection_event_for_session(
                &services.db_store,
                &connection.session,
                &connection.peer_addr,
                "body_read_error",
                Some(json!({ "seq": header.seq, "error": error.to_string() })),
            )
            .await;
            break;
        }
        let packet = Packet::new(header, body);
        let started_at = Instant::now();

        if !rate_limiter.allow(started_at, runtime.msg_rate_window_ms, runtime.msg_rate_max) {
            if let Err(error) = connection.queue_error(
                packet.header.seq,
                "MSG_RATE_EXCEEDED",
                "message rate exceeded",
            ) {
                warn!(
                    session_id = connection.session.id,
                    error = %error,
                    "failed to queue message rate exceeded error"
                );
                break;
            }
            append_connection_event_for_session(
                &services.db_store,
                &connection.session,
                &connection.peer_addr,
                "msg_rate_exceeded",
                Some(json!({
                    "msgType": packet.header.msg_type,
                    "seq": packet.header.seq,
                    "windowMs": runtime.msg_rate_window_ms,
                    "max": runtime.msg_rate_max
                })),
            )
            .await;
            continue;
        }

        if connection.session.state == SessionState::Authenticated {
            if let Some(account_player_id) = connection.session.account_player_id.as_deref() {
                let player_message_allowed = {
                    let mut limiter = services.player_msg_rate_limiter.lock().await;
                    limiter.allow(
                        account_player_id,
                        started_at,
                        runtime.player_msg_rate_window_ms,
                        runtime.player_msg_rate_max,
                    )
                };

                if !player_message_allowed {
                    if let Err(error) = connection.queue_error(
                        packet.header.seq,
                        "MSG_RATE_EXCEEDED",
                        "player message rate exceeded",
                    ) {
                        warn!(
                            session_id = connection.session.id,
                            account_player_id = %account_player_id,
                            error = %error,
                            "failed to queue client message rate exceeded error"
                        );
                        break;
                    }
                    append_connection_event_for_session(
                        &services.db_store,
                        &connection.session,
                        &connection.peer_addr,
                        "player_msg_rate_exceeded",
                        Some(json!({
                            "msgType": packet.header.msg_type,
                            "seq": packet.header.seq,
                            "windowMs": runtime.player_msg_rate_window_ms,
                            "max": runtime.player_msg_rate_max
                        })),
                    )
                    .await;
                    continue;
                }
            }
        }

        let dispatch_failure: Option<(String, Option<&'static str>)> =
            match dispatch_packet(&services, &mut connection, &packet).await {
                Ok(()) => None,
                Err(error) => {
                    let outbound_error_kind = outbound_queue_error_kind_from_error(error.as_ref())
                        .map(|kind| kind.as_str());
                    let error_message = error.to_string();
                    Some((error_message, outbound_error_kind))
                }
            };
        METRICS.record_request();
        METRICS.record_latency(started_at.elapsed().as_millis() as u64);
        if let Some((error_message, outbound_error_kind)) = dispatch_failure {
            warn!(
                session_id = connection.session.id,
                error = %error_message,
                "packet dispatch failed, will cleanup"
            );
            append_connection_event_for_session(
                &services.db_store,
                &connection.session,
                &connection.peer_addr,
                "dispatch_error",
                Some(json!({
                    "msgType": packet.header.msg_type,
                    "seq": packet.header.seq,
                    "error": error_message,
                    "outboundQueueErrorKind": outbound_error_kind
                })),
            )
            .await;
            break;
        }

        if next_online_route_refresh_at.is_none_or(|deadline| started_at >= deadline)
            && let (Some(account_player_id), Some(character_id), Some(authority)) = (
                connection.session.account_player_id.clone(),
                connection.session.character_id.clone(),
                connection.session.online_authority.clone(),
            )
        {
            let route_ttl_secs = online_route_ttl_secs(runtime.heartbeat_timeout_secs);
            next_online_route_refresh_at = Some(
                started_at
                    + Duration::from_secs(online_route_refresh_secs(
                        runtime.heartbeat_timeout_secs,
                    )),
            );
            let _route_guard = services
                .online_route_coordinator
                .lock_account(&account_player_id)
                .await;
            let is_current_local_session = services
                .player_registry
                .read()
                .await
                .is_current_session(&account_player_id, &character_id, connection.session.id);
            if !is_current_local_session {
                warn!(
                    character_id = %character_id,
                    instance_id = %services.config.service_instance_id,
                    session_id = connection.session.id,
                    "stale local session cannot refresh or restore game online route"
                );
                *connection.kick_reason.write().await = "authority_changed".to_string();
                connection.kick_notify.notify_one();
                break;
            }

            let mut authority_lost = false;
            match refresh_online_route(
                &mut connection.redis,
                &services.config.redis_key_prefix,
                &character_id,
                &services.config.service_instance_id,
                connection.session.id,
                &authority,
                route_ttl_secs,
            )
            .await
            {
                Ok(state) if refresh_action(true, state) == RouteRefreshAction::RestoreMissing => {
                    match restore_missing_online_route(
                        &mut connection.redis,
                        &services.config.redis_key_prefix,
                        &character_id,
                        &services.config.service_instance_id,
                        connection.session.id,
                        &authority,
                        route_ttl_secs,
                    )
                    .await
                    {
                        Ok(true) => info!(
                            character_id = %character_id,
                            instance_id = %services.config.service_instance_id,
                            session_id = connection.session.id,
                            "restored missing game online route from matching owner witness"
                        ),
                        Ok(false) => {
                            authority_lost = true;
                            warn!(
                                character_id = %character_id,
                                instance_id = %services.config.service_instance_id,
                                session_id = connection.session.id,
                                "game online route changed before missing route restore"
                            );
                        }
                        Err(error) => {
                            authority_lost = true;
                            warn!(
                                character_id = %character_id,
                                instance_id = %services.config.service_instance_id,
                                error = %error,
                                "failed to restore missing game online route"
                            );
                        }
                    }
                }
                Ok(RouteRefreshState::Refreshed) => {}
                Ok(RouteRefreshState::Missing(MissingRouteOwnership::Unknown)) => {
                    authority_lost = true;
                    warn!(
                        character_id = %character_id,
                        instance_id = %services.config.service_instance_id,
                        session_id = connection.session.id,
                        "game online route authority is unprovable; disconnecting local session"
                    );
                }
                Ok(RouteRefreshState::OwnershipChanged) => {
                    authority_lost = true;
                    warn!(
                        character_id = %character_id,
                        instance_id = %services.config.service_instance_id,
                        session_id = connection.session.id,
                        "game online route ownership changed; disconnecting local session"
                    );
                }
                Ok(RouteRefreshState::Missing(MissingRouteOwnership::Proven)) => unreachable!(),
                Err(error) => {
                    authority_lost = true;
                    warn!(
                        character_id = %character_id,
                        instance_id = %services.config.service_instance_id,
                        error = %error,
                        "failed to refresh game online route; disconnecting fail closed"
                    );
                }
            }
            if authority_lost {
                services
                    .player_registry
                    .write()
                    .await
                    .remove_by_account_if_session(&account_player_id, connection.session.id);
                *connection.kick_reason.write().await = "authority_changed".to_string();
                connection.kick_notify.notify_one();
                break;
            }
        }
    }

    if !close_event_appended {
        if let Some(close_reason) = connection.close_state.reason() {
            append_connection_event_for_session(
                &services.db_store,
                &connection.session,
                &connection.peer_addr,
                "server_close_requested",
                Some(json!({ "reason": close_reason })),
            )
            .await;
        }
    }

    room_service::handle_disconnect_cleanup(&services, &connection).await;

    // Unregister from online registry (only if our session_id still matches).
    // P0 registry uniqueness is account-scoped; character lookup is secondary.
    if let (Some(account_player_id), Some(character_id)) = (
        connection.session.account_player_id.clone(),
        connection.session.character_id.clone(),
    ) {
        let _route_guard = services
            .online_route_coordinator
            .lock_account(&account_player_id)
            .await;
        let removed = services
            .player_registry
            .write()
            .await
            .remove_by_account_if_session(&account_player_id, connection.session.id);
        if removed.is_some()
            && let Some(authority) = connection.session.online_authority.as_ref()
            && let Err(error) = clear_online_route(
                &mut connection.redis,
                &services.config.redis_key_prefix,
                &character_id,
                &services.config.service_instance_id,
                connection.session.id,
                authority,
            )
            .await
        {
            warn!(
                character_id = %character_id,
                instance_id = %services.config.service_instance_id,
                error = %error,
                "failed to clear game online route during disconnect"
            );
        }
    }

    if connection.session.state == crate::session::SessionState::Authenticated {
        let previous = services.online_player_count.fetch_sub(1, Ordering::Relaxed);
        let online_players = previous.saturating_sub(1);
        METRICS.set_online_players(online_players);
    }

    drop(connection.tx);
    writer_task.await??;
    Ok(())
}

async fn append_connection_event_for_session(
    db_store: &PgAuditStore,
    session: &Session,
    peer_addr: &str,
    event_type: &str,
    details: Option<Value>,
) {
    let identity = session.authenticated_identity();
    let account_player_id = identity
        .as_ref()
        .map(|value| value.account_player_id.as_str())
        .or(session.account_player_id.as_deref());
    let character_id = identity.as_ref().map(|value| value.character_id.as_str());

    db_store
        .append_connection_event_with_identity(
            session.id,
            session.account_player_id.as_deref(),
            account_player_id,
            character_id,
            Some(peer_addr),
            event_type,
            details,
        )
        .await;
}

async fn discard_body<R>(reader: &mut R, body_len: usize) -> Result<(), std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut remaining = body_len;
    let mut buffer = [0u8; 4096];
    while remaining > 0 {
        let chunk_len = remaining.min(buffer.len());
        reader.read_exact(&mut buffer[..chunk_len]).await?;
        remaining -= chunk_len;
    }
    Ok(())
}
async fn dispatch_packet(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    if !preauth_message_allowed(connection.session.state, packet.message_type()) {
        connection.queue_error(
            packet.header.seq,
            "PREAUTH_MESSAGE_NOT_ALLOWED",
            "authenticate before sending business messages",
        )?;
        append_connection_event_for_session(
            &services.db_store,
            &connection.session,
            &connection.peer_addr,
            "preauth_message_rejected",
            Some(json!({
                "msgType": packet.header.msg_type,
                "seq": packet.header.seq
            })),
        )
        .await;
        return Ok(());
    }

    match packet.message_type() {
        Some(MessageType::AuthReq) => core_service::handle_auth(services, connection, packet).await,
        Some(MessageType::PingReq) => core_service::handle_ping(connection, packet)
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>),
        Some(MessageType::GetRoomDataReq) => {
            room_query::handle_get_room_data(services, connection, packet).await
        }
        Some(MessageType::RoomJoinReq) => {
            room_service::handle_room_join(services, connection, packet).await
        }
        Some(MessageType::RoomLeaveReq) => {
            room_service::handle_room_leave(services, connection, packet).await
        }
        Some(MessageType::RoomReadyReq) => {
            room_service::handle_room_ready(services, connection, packet).await
        }
        Some(MessageType::RoomStartReq) => {
            room_service::handle_room_start(services, connection, packet).await
        }
        Some(MessageType::PlayerInputReq) => {
            room_service::handle_player_input(services, connection, packet).await
        }
        Some(MessageType::MoveInputReq) => {
            room_service::handle_move_input(services, connection, packet).await
        }
        Some(MessageType::RoomEndReq) => {
            room_service::handle_room_end(services, connection, packet).await
        }
        Some(MessageType::RoomReconnectReq) => {
            room_service::handle_room_reconnect(services, connection, packet).await
        }
        Some(MessageType::RoomJoinAsObserverReq) => {
            room_service::handle_join_as_observer(services, connection, packet).await
        }
        Some(MessageType::CreateMatchedRoomReq) => {
            room_service::handle_create_matched_room(services, connection, packet).await
        }
        Some(MessageType::MatchStartReq) => {
            match_service::handle_match_start(services, connection, packet).await
        }
        Some(MessageType::MatchCancelReq) => {
            match_service::handle_match_cancel(services, connection, packet).await
        }
        Some(MessageType::MatchStatusReq) => {
            match_service::handle_match_status(services, connection, packet).await
        }
        Some(MessageType::MatchEventStreamReq) => {
            match_service::handle_match_event_stream(services, connection, packet).await
        }
        // Inventory handlers
        Some(MessageType::ItemEquipReq) => {
            inventory_service::handle_item_equip(services, connection, packet).await
        }
        Some(MessageType::ItemUseReq) => {
            inventory_service::handle_item_use(services, connection, packet).await
        }
        Some(MessageType::ItemDiscardReq) => {
            inventory_service::handle_item_discard(services, connection, packet).await
        }
        Some(MessageType::DeprecatedItemAddReq | MessageType::DeprecatedItemAddRes) => {
            connection.queue_error(
                packet.header.seq,
                "MESSAGE_TYPE_DEPRECATED",
                "message type is permanently retired",
            )?;
            Ok(())
        }
        Some(MessageType::WarehouseAccessReq) => {
            inventory_service::handle_warehouse_access(services, connection, packet).await
        }
        Some(MessageType::GetInventoryReq) => {
            inventory_service::handle_get_inventory(services, connection, packet).await
        }
        Some(MessageType::GetCharacterElementsReq) => {
            character_element::handle_get_character_elements(services, connection, packet).await
        }
        Some(MessageType::DebugApplyCharacterElementChangeReq) => {
            character_element::handle_debug_apply_character_element_change(
                services, connection, packet,
            )
            .await
        }
        Some(MessageType::GetCharacterTitlesReq) => {
            character_title_service::handle_get_character_titles(services, connection, packet).await
        }
        Some(MessageType::EquipCharacterTitleReq) => {
            character_title_service::handle_equip_character_title(services, connection, packet)
                .await
        }
        Some(MessageType::GetCharacterDisciplinesReq) => {
            character_title_service::handle_get_character_disciplines(services, connection, packet)
                .await
        }
        Some(MessageType::LearnCharacterDisciplineReq) => {
            character_title_service::handle_learn_character_discipline(services, connection, packet)
                .await
        }
        Some(MessageType::SetCharacterDisciplineActiveReq) => {
            character_title_service::handle_set_character_discipline_active(
                services, connection, packet,
            )
            .await
        }
        Some(MessageType::SwitchCharacterDisciplineReq) => {
            character_title_service::handle_switch_character_discipline(
                services, connection, packet,
            )
            .await
        }
        Some(MessageType::AddCharacterDisciplinePointsReq) => {
            character_title_service::handle_add_character_discipline_points(
                services, connection, packet,
            )
            .await
        }
        Some(MessageType::ApplyCharacterProgressReq) => {
            character_progress_service::handle_apply_character_progress(
                services, connection, packet,
            )
            .await
        }
        Some(
            MessageType::ActivityListReq
            | MessageType::ActivityDetailReq
            | MessageType::ActivityProgressReq
            | MessageType::ActivityClaimReq
            | MessageType::ActivityActionReq,
        ) => crate::activity::handle_packet(&services.activity_engine, connection, packet)
            .await
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>),
        Some(MessageType::DebugCharacterTitleReq) => {
            character_title_service::handle_debug_character_title(services, connection, packet)
                .await
        }
        Some(_) => {
            connection.queue_error(
                packet.header.seq,
                "MESSAGE_NOT_SUPPORTED",
                "message not supported in this phase",
            )?;
            Ok(())
        }
        None => {
            connection.queue_error(
                packet.header.seq,
                "UNKNOWN_MESSAGE_TYPE",
                "unknown message type",
            )?;
            append_connection_event_for_session(
                &services.db_store,
                &connection.session,
                &connection.peer_addr,
                "unknown_message_type",
                Some(json!({
                    "msgType": packet.header.msg_type,
                    "seq": packet.header.seq
                })),
            )
            .await;
            Ok(())
        }
    }
}

pub fn current_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MonitorRoomLogic;
    impl crate::core::logic::RoomLogic for MonitorRoomLogic {}
    impl crate::core::logic::RoomLogicTransfer for MonitorRoomLogic {}
    struct MonitorRoomLogicFactory;
    impl crate::core::logic::RoomLogicFactory for MonitorRoomLogicFactory {
        fn create(
            &self,
            _policy_id: &str,
        ) -> Result<Box<dyn crate::core::logic::RoomLogic>, &'static str> {
            Ok(Box::new(MonitorRoomLogic))
        }
    }

    fn monitor_room_manager() -> Arc<RoomManager> {
        Arc::new(RoomManager::new(Arc::new(MonitorRoomLogicFactory)))
    }

    fn spawn_test_drain_monitor(
        draining: bool,
        connection_count: Arc<AtomicU64>,
        timeout_duration: Duration,
    ) -> (
        Arc<DrainShutdownControl>,
        Arc<Notify>,
        watch::Sender<bool>,
        JoinHandle<()>,
    ) {
        let (drain_tx, drain_rx) = watch::channel(draining);
        let (control, arm_rx) = DrainShutdownControl::channel();
        let shutdown = Arc::new(Notify::new());
        let task = tokio::spawn(run_drain_shutdown_monitor(
            drain_rx,
            arm_rx,
            control.clone(),
            connection_count,
            monitor_room_manager(),
            "game-server-test".to_string(),
            shutdown.clone(),
            DrainShutdownConfig {
                timeout: timeout_duration,
                poll_interval: Duration::from_millis(5),
            },
        ));
        (control, shutdown, drain_tx, task)
    }

    #[test]
    fn drain_shutdown_decision_waits_shuts_down_and_never_forces_timeout() {
        let timeout = Duration::from_secs(30);
        assert_eq!(
            drain_shutdown_decision(1, 0, 0, Duration::from_secs(1), timeout),
            DrainShutdownDecision::Wait
        );
        assert_eq!(
            drain_shutdown_decision(0, 1, 0, Duration::from_secs(1), timeout),
            DrainShutdownDecision::Wait
        );
        assert_eq!(
            drain_shutdown_decision(0, 0, 1, Duration::from_secs(1), timeout),
            DrainShutdownDecision::Wait
        );
        assert_eq!(
            drain_shutdown_decision(0, 0, 0, Duration::from_secs(1), timeout),
            DrainShutdownDecision::Shutdown
        );
        assert_eq!(
            drain_shutdown_decision(1, 0, 0, timeout, timeout),
            DrainShutdownDecision::TimedOut
        );
    }

    #[tokio::test]
    async fn ordinary_drain_does_not_request_shutdown_without_explicit_arm() {
        let (_control, shutdown, _drain_tx, task) =
            spawn_test_drain_monitor(true, Arc::new(AtomicU64::new(0)), Duration::from_millis(20));
        assert!(
            timeout(Duration::from_millis(30), shutdown.notified())
                .await
                .is_err()
        );
        task.abort();
    }

    #[tokio::test]
    async fn explicit_arm_with_zero_blockers_requests_shutdown() {
        let (control, shutdown, _drain_tx, task) =
            spawn_test_drain_monitor(true, Arc::new(AtomicU64::new(0)), Duration::from_millis(50));
        assert!(control.try_arm());
        timeout(Duration::from_millis(50), shutdown.notified())
            .await
            .expect("explicit zero-blocker request should trigger shutdown");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn drain_timeout_protects_sessions_and_later_request_can_rearm() {
        let connections = Arc::new(AtomicU64::new(1));
        let (control, shutdown, _drain_tx, task) =
            spawn_test_drain_monitor(true, connections.clone(), Duration::from_millis(20));
        assert!(control.try_arm());
        assert!(!control.try_arm());
        assert!(
            timeout(Duration::from_millis(35), shutdown.notified())
                .await
                .is_err()
        );
        assert!(!control.is_armed());

        connections.store(0, Ordering::Relaxed);
        assert!(
            timeout(Duration::from_millis(15), shutdown.notified())
                .await
                .is_err()
        );
        assert!(control.try_arm());
        timeout(Duration::from_millis(50), shutdown.notified())
            .await
            .expect("new explicit request after timeout should rearm");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn background_task_panic_is_reported_without_skipping_later_cleanup_steps() {
        let health_state = HealthState::new(
            "game-server",
            "cleanup-test",
            service_registry::HealthConfig::for_tests(1, 0, u64::MAX),
            [],
        );
        let mut resources = GameServerResources::new(health_state);
        let task = tokio::spawn(async {
            panic!("injected background task failure");
        });
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }
        resources.tasks.push(task);

        let report = run_cleanup(&mut resources).await;

        assert_eq!(report.attempted.last(), Some(&CleanupStep::CloseStores));
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].0, CleanupStep::StopBackgroundTasks);
        assert!(
            report.failures[0]
                .1
                .contains("injected background task failure")
        );
    }

    #[test]
    fn preauth_allows_auth_and_ping_before_authentication() {
        assert!(preauth_message_allowed(
            SessionState::Connected,
            Some(MessageType::AuthReq)
        ));
        assert!(preauth_message_allowed(
            SessionState::Connected,
            Some(MessageType::PingReq)
        ));
    }

    #[test]
    fn preauth_rejects_business_and_unknown_messages_before_authentication() {
        assert!(!preauth_message_allowed(
            SessionState::Connected,
            Some(MessageType::RoomJoinReq)
        ));
        assert!(!preauth_message_allowed(SessionState::Connected, None));
    }

    #[test]
    fn preauth_allows_business_messages_after_authentication() {
        assert!(preauth_message_allowed(
            SessionState::Authenticated,
            Some(MessageType::RoomJoinReq)
        ));
        assert!(preauth_message_allowed(SessionState::Authenticated, None));
    }

    #[test]
    fn rate_limiter_disabled_allows_all_messages() {
        let mut limiter = ConnectionRateLimiter::new();
        let now = Instant::now();

        assert!(limiter.allow(now, 1000, 0));
        assert!(limiter.allow(now, 0, 1));
    }

    #[test]
    fn rate_limiter_rejects_after_window_limit() {
        let mut limiter = ConnectionRateLimiter::new();
        let now = Instant::now();

        assert!(limiter.allow(now, 1000, 2));
        assert!(limiter.allow(now, 1000, 2));
        assert!(!limiter.allow(now, 1000, 2));
    }

    #[test]
    fn rate_limiter_resets_after_window_rolls() {
        let mut limiter = ConnectionRateLimiter::new();
        let now = Instant::now();

        assert!(limiter.allow(now, 1000, 1));
        assert!(!limiter.allow(now + Duration::from_millis(999), 1000, 1));
        assert!(limiter.allow(now + Duration::from_millis(1000), 1000, 1));
    }

    #[test]
    fn player_rate_limiter_disabled_allows_all_messages() {
        let mut limiter = PlayerMessageRateLimiter::new();
        let now = Instant::now();

        assert!(limiter.allow("player-a", now, 1000, 0));
        assert!(limiter.allow("player-a", now, 1000, 0));
        assert!(limiter.allow("player-a", now, 0, 1));
        assert_eq!(limiter.tracked_player_count(), 0);
    }

    #[test]
    fn player_rate_limiter_rejects_same_player_across_trackers() {
        let mut limiter = PlayerMessageRateLimiter::new();
        let now = Instant::now();

        assert!(limiter.allow("player-a", now, 1000, 2));
        assert!(limiter.allow("player-a", now + Duration::from_millis(10), 1000, 2));
        assert!(!limiter.allow("player-a", now + Duration::from_millis(20), 1000, 2));
        assert_eq!(limiter.tracked_player_count(), 1);
    }

    #[test]
    fn player_rate_limiter_resets_after_window_rolls() {
        let mut limiter = PlayerMessageRateLimiter::new();
        let now = Instant::now();

        assert!(limiter.allow("player-a", now, 1000, 1));
        assert!(!limiter.allow("player-a", now + Duration::from_millis(999), 1000, 1));
        assert!(limiter.allow("player-a", now + Duration::from_millis(1000), 1000, 1));
    }

    #[test]
    fn player_rate_limiter_tracks_players_independently() {
        let mut limiter = PlayerMessageRateLimiter::new();
        let now = Instant::now();

        assert!(limiter.allow("player-a", now, 1000, 1));
        assert!(!limiter.allow("player-a", now, 1000, 1));
        assert!(limiter.allow("player-b", now, 1000, 1));
        assert!(!limiter.allow("player-b", now, 1000, 1));
        assert_eq!(limiter.tracked_player_count(), 2);
    }

    #[test]
    fn player_rate_limiter_cleanup_removes_expired_windows() {
        let mut limiter = PlayerMessageRateLimiter::new();
        let now = Instant::now();

        assert!(limiter.allow("player-a", now, 1000, 1));
        assert!(limiter.allow("player-b", now + Duration::from_millis(200), 1000, 1));
        assert_eq!(limiter.tracked_player_count(), 2);

        assert_eq!(
            limiter.cleanup_expired(now + Duration::from_millis(1000), 1000),
            1
        );
        assert_eq!(limiter.tracked_player_count(), 1);

        assert_eq!(
            limiter.cleanup_expired(now + Duration::from_millis(1200), 1000),
            1
        );
        assert_eq!(limiter.tracked_player_count(), 0);
    }

    #[test]
    fn input_anomaly_tracker_records_until_threshold() {
        let mut tracker = PlayerInputAnomalyTracker::new();
        let now = Instant::now();

        let first = tracker.record("player-a", now, 1000, 2);
        assert_eq!(
            first,
            InputAnomalyRecordOutcome {
                count: 1,
                blocked: false
            }
        );
        assert!(!tracker.is_blocked("player-a", now, 1000, 2));

        let second = tracker.record("player-a", now + Duration::from_millis(10), 1000, 2);
        assert_eq!(
            second,
            InputAnomalyRecordOutcome {
                count: 2,
                blocked: true
            }
        );
        assert!(tracker.is_blocked("player-a", now + Duration::from_millis(20), 1000, 2));
    }

    #[test]
    fn input_anomaly_tracker_disabled_threshold_never_blocks() {
        let mut tracker = PlayerInputAnomalyTracker::new();
        let now = Instant::now();

        let first = tracker.record("player-a", now, 1000, 0);
        let second = tracker.record("player-a", now + Duration::from_millis(10), 1000, 0);

        assert_eq!(first.count, 1);
        assert_eq!(second.count, 2);
        assert!(!second.blocked);
        assert!(!tracker.is_blocked("player-a", now + Duration::from_millis(20), 1000, 0));
    }

    #[test]
    fn input_anomaly_tracker_resets_after_window_rolls() {
        let mut tracker = PlayerInputAnomalyTracker::new();
        let now = Instant::now();

        assert!(tracker.record("player-a", now, 1000, 1).blocked);
        assert!(tracker.is_blocked("player-a", now + Duration::from_millis(999), 1000, 1));
        assert!(!tracker.is_blocked("player-a", now + Duration::from_millis(1000), 1000, 1));
        assert_eq!(tracker.tracked_player_count(), 0);
    }

    #[test]
    fn input_anomaly_tracker_detects_duplicate_identical_inputs_only() {
        let mut tracker = PlayerInputAnomalyTracker::new();
        let now = Instant::now();

        assert!(!tracker.remember_frame("player-a", "room-a", 1, "move:{}", now, 1000));
        assert_eq!(tracker.anomaly_count("player-a"), 0);
        assert!(!tracker.remember_frame(
            "player-a",
            "room-a",
            2,
            "move:{\"x\":1}",
            now + Duration::from_millis(10),
            1000
        ));
        assert_eq!(tracker.anomaly_count("player-a"), 0);
        assert!(!tracker.remember_frame(
            "player-a",
            "room-a",
            2,
            "move:{\"x\":2}",
            now + Duration::from_millis(15),
            1000
        ));
        assert!(tracker.remember_frame(
            "player-a",
            "room-a",
            2,
            "move:{\"x\":2}",
            now + Duration::from_millis(20),
            1000
        ));
        assert!(!tracker.remember_frame(
            "player-a",
            "room-b",
            2,
            "move:{\"x\":2}",
            now + Duration::from_millis(30),
            1000
        ));
    }

    #[test]
    fn input_anomaly_tracker_starts_window_on_first_anomaly() {
        let mut tracker = PlayerInputAnomalyTracker::new();
        let now = Instant::now();

        assert!(!tracker.remember_frame("player-a", "room-a", 1, "move:{}", now, 1000));
        let first_anomaly_at = now + Duration::from_millis(900);
        assert!(
            tracker
                .record("player-a", first_anomaly_at, 1000, 1)
                .blocked
        );

        assert!(tracker.is_blocked(
            "player-a",
            first_anomaly_at + Duration::from_millis(999),
            1000,
            1
        ));
        assert!(!tracker.is_blocked(
            "player-a",
            first_anomaly_at + Duration::from_millis(1000),
            1000,
            1
        ));
    }
}
