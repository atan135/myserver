use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use interprocess::local_socket::traits::tokio::Listener as _;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{Notify, RwLock, mpsc};
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use crate::config::Config;
use crate::core::config_table::ConfigTableRuntime;
use crate::core::context::{ConnectionContext, PlayerRegistry, ServerSharedState, ServiceContext};
use crate::core::logic::SharedRoomLogicFactory;
use crate::core::player::{MySqlPlayerStore, PlayerManager};
use crate::core::room::OutboundMessage;
use crate::core::runtime::RoomManager;
use crate::core::service::{core_service, inventory_service, room_service};
use crate::core::system::combat::{CsvCombatCatalog, SharedCombatCatalog};
use crate::core::system::scene::SceneCatalog;
use crate::gameroom::GameRoomLogicFactory;
use crate::gameservice::room_query;
use crate::match_client::{MatchClientConfig, init_match_client};
use crate::metrics::METRICS;
use crate::mysql_store::MySqlAuditStore;
use crate::pb::SessionKickPush;
use crate::protocol::{HEADER_LEN, MessageType, Packet, encode_packet, parse_header};
use crate::session::{Session, SessionState};

pub const DEFAULT_OUTBOUND_QUEUE_CAPACITY: usize = 1024;

#[derive(Clone, Copy, Debug)]
pub struct RuntimeConfig {
    pub heartbeat_timeout_secs: u64,
    pub max_body_len: usize,
    pub msg_rate_window_ms: u64,
    pub msg_rate_max: u64,
    pub drain_mode_enabled: bool,
    pub drain_mode_entered_at_ms: Option<u64>,
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

pub async fn run(
    config: &Config,
    mysql_store: MySqlAuditStore,
    config_tables: ConfigTableRuntime,
) -> Result<(), Box<dyn std::error::Error>> {
    let tcp_listener = TcpListener::bind(config.bind_addr()).await?;
    let admin_listener = TcpListener::bind(config.admin_bind_addr()).await?;
    let local_socket_listener = crate::local_socket::create_listener(&config.local_socket_name)?;
    let internal_socket_listener =
        crate::local_socket::create_listener(&config.internal_socket_name)?;
    let redis_client = redis::Client::open(config.redis_url.clone())?;

    // Initialize MatchClient for communicating with MatchService
    let match_client = crate::match_client::create_match_client_shared();
    let match_client_config = MatchClientConfig::from_env().await;
    if let Err(e) = init_match_client(&match_client, match_client_config.clone()).await {
        tracing::error!(error = %e, "failed to connect to match-service, match notifications will be disabled");
    }

    let tables_snapshot = config_tables.snapshot().await;
    let scene_dir = std::path::Path::new(&config.csv_dir)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("scene");
    let scene_catalog = Arc::new(SceneCatalog::load_from_dir(
        &scene_dir,
        tables_snapshot.as_ref(),
    )?);
    let combat_catalog: SharedCombatCatalog =
        Arc::new(CsvCombatCatalog::from_tables(tables_snapshot.as_ref())?);
    let movement_demo_scene_id = scene_catalog
        .scene_id_by_code("grassland_01")
        .or_else(|| scene_catalog.scenes.keys().min().copied())
        .ok_or("scene catalog is empty")?;
    let room_logic_factory: SharedRoomLogicFactory = Arc::new(GameRoomLogicFactory::new(
        scene_catalog,
        movement_demo_scene_id,
        combat_catalog,
    ));
    let shared_state = ServerSharedState {
        room_manager: Arc::new(RoomManager::with_match_client_and_cleanup_interval(
            match_client,
            room_logic_factory,
            config.room_cleanup_interval_secs,
        )),
        runtime_config: Arc::new(RwLock::new(RuntimeConfig {
            heartbeat_timeout_secs: config.heartbeat_timeout_secs,
            max_body_len: config.max_body_len,
            msg_rate_window_ms: config.msg_rate_window_ms,
            msg_rate_max: config.msg_rate_max,
            drain_mode_enabled: false,
            drain_mode_entered_at_ms: None,
        })),
        connection_count: Arc::new(AtomicU64::new(0)),
        online_player_count: Arc::new(AtomicU64::new(0)),
    };

    // Initialize MySqlPlayerStore for inventory persistence
    let mysql_player_store = MySqlPlayerStore::new(config).await?;

    let player_registry: PlayerRegistry = Arc::new(RwLock::new(HashMap::new()));

    let services = ServiceContext {
        config: config.clone(),
        mysql_store: mysql_store.clone(),
        room_manager: shared_state.room_manager.clone(),
        runtime_config: shared_state.runtime_config.clone(),
        config_tables,
        player_manager: PlayerManager::new(mysql_player_store),
        online_player_count: shared_state.online_player_count.clone(),
        player_registry: player_registry.clone(),
    };
    info!(
        addr = %config.bind_addr(),
        admin_addr = %config.admin_bind_addr(),
        local_socket_name = %config.local_socket_name,
        internal_socket_name = %config.internal_socket_name,
        redis = %config.redis_url,
        mysql_enabled = mysql_store.enabled(),
        "game server listening"
    );

    let admin_task = tokio::spawn(crate::admin_server::run_listener(
        admin_listener,
        shared_state.room_manager.clone(),
        shared_state.runtime_config.clone(),
        shared_state.connection_count.clone(),
        services.player_registry.clone(),
        services.player_manager.clone(),
        services.config_tables.clone(),
        config.admin_token.clone(),
    ));

    let local_socket_task = tokio::spawn(run_local_socket_listener(
        local_socket_listener,
        redis_client.clone(),
        services.clone(),
        shared_state.runtime_config.clone(),
        shared_state.connection_count.clone(),
    ));
    let internal_socket_task = tokio::spawn(crate::internal_server::run_listener(
        internal_socket_listener,
        services.clone(),
        config.internal_token.clone(),
    ));

    let kick_task = tokio::spawn(crate::kick_subscriber::subscribe_session_kicks(
        config.nats_url.clone(),
        player_registry,
    ));

    let mut next_session_id: u64 = 1;

    loop {
        let accept_result = tokio::select! {
            result = tcp_listener.accept() => Some(result),
            _ = tokio::signal::ctrl_c() => None,
        };

        let Some((socket, peer_addr)) = accept_result.transpose()? else {
            info!("shutdown signal received, stopping game server accept loop");
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
            mysql_store.clone(),
        )
        .await;
    }

    admin_task.abort();
    let _ = admin_task.await;
    local_socket_task.abort();
    let _ = local_socket_task.await;
    internal_socket_task.abort();
    let _ = internal_socket_task.await;
    kick_task.abort();
    let _ = kick_task.await;

    info!("game server shutdown completed");
    Ok(())
}

async fn run_local_socket_listener(
    listener: interprocess::local_socket::tokio::Listener,
    redis_client: redis::Client,
    services: ServiceContext,
    runtime_config: Arc<RwLock<RuntimeConfig>>,
    connection_count: Arc<AtomicU64>,
) -> Result<(), std::io::Error> {
    let mut next_session_id = 1_000_000u64;
    loop {
        let socket = listener.accept().await?;
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
            services.mysql_store.clone(),
        )
        .await;
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
    mysql_store: MySqlAuditStore,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    connection_count.fetch_add(1, Ordering::Relaxed);
    info!(session_id = session_id, peer = %peer_addr, "accepted game connection");
    mysql_store
        .append_connection_event(session_id, None, Some(&peer_addr), "connected", None)
        .await;

    tokio::spawn(async move {
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
    let (tx, mut rx) = mpsc::channel::<OutboundMessage>(DEFAULT_OUTBOUND_QUEUE_CAPACITY);
    let mut connection = ConnectionContext {
        peer_addr,
        redis,
        session: Session::new(session_id),
        tx,
        kick_notify: Arc::new(Notify::new()),
        kick_reason: Arc::new(RwLock::new("session_kicked".to_string())),
    };
    let mut rate_limiter = ConnectionRateLimiter::new();

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
        let runtime = *runtime_config.read().await;
        let mut header_buf = [0u8; HEADER_LEN];

        // select! between kick notification and normal packet read
        let read_header = tokio::select! {
            _ = connection.kick_notify.notified() => {
                let kick_reason = connection.kick_reason.read().await.clone();
                info!(
                    session_id = connection.session.id,
                    player_id = ?connection.session.player_id,
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
                services
                    .mysql_store
                    .append_connection_event(
                        connection.session.id,
                        connection.session.player_id.as_deref(),
                        Some(&connection.peer_addr),
                        "session_kicked",
                        Some(json!({ "reason": kick_reason })),
                    )
                    .await;
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
                services
                    .mysql_store
                    .append_connection_event(
                        connection.session.id,
                        connection.session.player_id.as_deref(),
                        Some(&connection.peer_addr),
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
                services
                    .mysql_store
                    .append_connection_event(
                        connection.session.id,
                        connection.session.player_id.as_deref(),
                        Some(&connection.peer_addr),
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
                services
                    .mysql_store
                    .append_connection_event(
                        connection.session.id,
                        connection.session.player_id.as_deref(),
                        Some(&connection.peer_addr),
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
            services
                .mysql_store
                .append_connection_event(
                    connection.session.id,
                    connection.session.player_id.as_deref(),
                    Some(&connection.peer_addr),
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
            services
                .mysql_store
                .append_connection_event(
                    connection.session.id,
                    connection.session.player_id.as_deref(),
                    Some(&connection.peer_addr),
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
            services
                .mysql_store
                .append_connection_event(
                    connection.session.id,
                    connection.session.player_id.as_deref(),
                    Some(&connection.peer_addr),
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

        let result = dispatch_packet(&services, &mut connection, &packet).await;
        METRICS.record_request();
        METRICS.record_latency(started_at.elapsed().as_millis() as u64);
        let dispatch_error = result.err().map(|error| error.to_string());
        if let Some(error_message) = dispatch_error {
            warn!(
                session_id = connection.session.id,
                error = %error_message,
                "packet dispatch failed, will cleanup"
            );
            services
                .mysql_store
                .append_connection_event(
                    connection.session.id,
                    connection.session.player_id.as_deref(),
                    Some(&connection.peer_addr),
                    "dispatch_error",
                    Some(json!({
                        "msgType": packet.header.msg_type,
                        "seq": packet.header.seq,
                        "error": error_message
                    })),
                )
                .await;
            break;
        }
    }

    room_service::handle_disconnect_cleanup(&services, &connection).await;

    // Unregister from player registry (only if our session_id still matches)
    if let Some(player_id) = &connection.session.player_id {
        let mut registry = services.player_registry.write().await;
        if let Some(handle) = registry.get(player_id) {
            if handle.session_id == connection.session.id {
                registry.remove(player_id);
            }
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
        services
            .mysql_store
            .append_connection_event(
                connection.session.id,
                connection.session.player_id.as_deref(),
                Some(&connection.peer_addr),
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
        Some(MessageType::ItemAddReq) => {
            inventory_service::handle_item_add(services, connection, packet).await
        }
        Some(MessageType::WarehouseAccessReq) => {
            inventory_service::handle_warehouse_access(services, connection, packet).await
        }
        Some(MessageType::GetInventoryReq) => {
            inventory_service::handle_get_inventory(services, connection, packet).await
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
            services
                .mysql_store
                .append_connection_event(
                    connection.session.id,
                    connection.session.player_id.as_deref(),
                    Some(&connection.peer_addr),
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
}
