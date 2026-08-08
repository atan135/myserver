use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prost::Message;
use redis::AsyncCommands;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::chat_push::ChatPushRouter;
use crate::chat_service::{self, ChatSessionMap};
use crate::chat_store::ChatStore;
use crate::metrics::{METRICS, MetricTransport, MetricsCollector};
use crate::online_route;
use crate::proto::chat::{ChatAuthReq, ChatAuthRes, ErrorRes};
use crate::protocol::{HEADER_LEN, OutboundMessage, Packet, encode_packet, parse_header};
use crate::ticket::{hash_ticket, verify_ticket};
use crate::websocket::{self, AdapterConfig};

const PLAYER_CONNECTION_LIMIT_EXCEEDED: &str = "PLAYER_CONNECTION_LIMIT_EXCEEDED";
const IP_CONNECTION_LIMIT_EXCEEDED: &str = "IP_CONNECTION_LIMIT_EXCEEDED";
const UNKNOWN_MESSAGE_TYPE: &str = "UNKNOWN_MESSAGE_TYPE";
const OUTBOUND_QUEUE_FULL: &str = "OUTBOUND_QUEUE_FULL";
static NEXT_CONNECTION_TOKEN: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
struct HandshakeRateWindow {
    started_at: Instant,
    admitted: u64,
}

/// A bounded, process-local admission gate for expensive HTTP upgrades. The
/// Caddy edge still owns TLS, while this guard keeps one chat instance from
/// accepting unlimited upgrade work during a reconnect storm.
#[derive(Clone, Debug)]
struct HandshakeRateLimiter {
    window: Duration,
    max_admitted: u64,
    state: Arc<Mutex<HandshakeRateWindow>>,
}

impl HandshakeRateLimiter {
    fn new(window: Duration, max_admitted: u64) -> Self {
        Self {
            window,
            max_admitted,
            state: Arc::new(Mutex::new(HandshakeRateWindow {
                started_at: Instant::now(),
                admitted: 0,
            })),
        }
    }

    fn try_admit(&self) -> bool {
        if self.max_admitted == 0 {
            return true;
        }

        let now = Instant::now();
        let mut state = self.state.lock().expect("handshake rate mutex poisoned");
        if now.duration_since(state.started_at) >= self.window {
            state.started_at = now;
            state.admitted = 0;
        }
        if state.admitted >= self.max_admitted {
            return false;
        }
        state.admitted += 1;
        true
    }
}

fn new_connection_token(instance_id: &str) -> String {
    let sequence = NEXT_CONNECTION_TOKEN.fetch_add(1, Ordering::Relaxed);
    format!("{instance_id}:{}:{sequence}", std::process::id())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Tcp,
    WebSocket,
}

impl Transport {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::WebSocket => "websocket",
        }
    }

    const fn metric(self) -> MetricTransport {
        match self {
            Self::Tcp => MetricTransport::Tcp,
            Self::WebSocket => MetricTransport::WebSocket,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ConnectionContext {
    transport: Transport,
    client_ip: IpAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionEnd {
    PeerClosed,
    AuthRejected,
    ConnectionLimitExceeded,
    SessionReplaced,
    OutboundQueueFull,
    OutboundQueueClosed,
    IdleTimeout,
    ProtocolViolation,
    TransportError,
}

impl ConnectionEnd {
    const fn websocket_close(self) -> websocket::ApplicationClose {
        match self {
            Self::PeerClosed => websocket::ApplicationClose::normal("peer_closed"),
            Self::AuthRejected => websocket::ApplicationClose::policy("auth_rejected"),
            Self::ConnectionLimitExceeded => {
                websocket::ApplicationClose::policy("connection_limit_exceeded")
            }
            Self::SessionReplaced => websocket::ApplicationClose::policy("session_replaced"),
            Self::OutboundQueueFull => {
                websocket::ApplicationClose::overloaded("outbound_queue_full")
            }
            Self::OutboundQueueClosed => {
                websocket::ApplicationClose::internal("outbound_queue_closed")
            }
            Self::IdleTimeout => websocket::ApplicationClose::policy("idle_timeout"),
            Self::ProtocolViolation => {
                websocket::ApplicationClose::policy("application_protocol_violation")
            }
            Self::TransportError => websocket::ApplicationClose::internal("handler_io_failed"),
        }
    }

    const fn from_outbound_queue_error(error: chat_service::OutboundQueueError) -> Self {
        match error {
            chat_service::OutboundQueueError::Full => Self::OutboundQueueFull,
            chat_service::OutboundQueueError::Closed => Self::OutboundQueueClosed,
        }
    }

    const fn is_outbound_queue_failure(self) -> bool {
        matches!(self, Self::OutboundQueueFull | Self::OutboundQueueClosed)
    }
}

fn record_connection_end_metrics(
    metrics: &MetricsCollector,
    transport: Transport,
    connection_end: ConnectionEnd,
) {
    if connection_end.is_outbound_queue_failure() {
        metrics.record_outbound_queue_failure(transport.metric());
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MessageType {
    ChatAuthReq = 20001,
    ChatAuthRes = 20002,
    ChatPrivateReq = 20101,
    ChatPrivateRes = 20102,
    ChatGroupReq = 20103,
    ChatGroupRes = 20104,
    ChatPush = 20105,
    GroupCreateReq = 20201,
    GroupCreateRes = 20202,
    GroupJoinReq = 20203,
    GroupJoinRes = 20204,
    GroupLeaveReq = 20205,
    GroupLeaveRes = 20206,
    GroupDismissReq = 20207,
    GroupDismissRes = 20208,
    GroupListReq = 20209,
    GroupListRes = 20210,
    ChatHistoryReq = 20211,
    ChatHistoryRes = 20212,
    MailNotifyPush = 20301,
    ErrorRes = 9000,
}

impl MessageType {
    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            20001 => Some(Self::ChatAuthReq),
            20002 => Some(Self::ChatAuthRes),
            20101 => Some(Self::ChatPrivateReq),
            20102 => Some(Self::ChatPrivateRes),
            20103 => Some(Self::ChatGroupReq),
            20104 => Some(Self::ChatGroupRes),
            20105 => Some(Self::ChatPush),
            20201 => Some(Self::GroupCreateReq),
            20202 => Some(Self::GroupCreateRes),
            20203 => Some(Self::GroupJoinReq),
            20204 => Some(Self::GroupJoinRes),
            20205 => Some(Self::GroupLeaveReq),
            20206 => Some(Self::GroupLeaveRes),
            20207 => Some(Self::GroupDismissReq),
            20208 => Some(Self::GroupDismissRes),
            20209 => Some(Self::GroupListReq),
            20210 => Some(Self::GroupListRes),
            20211 => Some(Self::ChatHistoryReq),
            20212 => Some(Self::ChatHistoryRes),
            20301 => Some(Self::MailNotifyPush),
            9000 => Some(Self::ErrorRes),
            _ => None,
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
            .is_none_or(|started_at| now.saturating_duration_since(started_at) >= window)
        {
            self.window_started_at = Some(now);
            self.count = 0;
        }

        self.count = self.count.saturating_add(1);
        self.count <= max_messages
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchDecision {
    Dispatch,
    RateLimited,
}

pub fn dispatch_decision(
    limiter: &mut ConnectionRateLimiter,
    now: Instant,
    window_ms: u64,
    max_messages: u64,
) -> DispatchDecision {
    if limiter.allow(now, window_ms, max_messages) {
        DispatchDecision::Dispatch
    } else {
        DispatchDecision::RateLimited
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionLimitExceeded {
    Player {
        player_id: String,
        current: u64,
        limit: u64,
    },
    Ip {
        ip: IpAddr,
        current: u64,
        limit: u64,
    },
}

impl ConnectionLimitExceeded {
    fn error_code(&self) -> &'static str {
        match self {
            Self::Player { .. } => PLAYER_CONNECTION_LIMIT_EXCEEDED,
            Self::Ip { .. } => IP_CONNECTION_LIMIT_EXCEEDED,
        }
    }

    fn current(&self) -> u64 {
        match self {
            Self::Player { current, .. } | Self::Ip { current, .. } => *current,
        }
    }

    fn limit(&self) -> u64 {
        match self {
            Self::Player { limit, .. } | Self::Ip { limit, .. } => *limit,
        }
    }
}

#[derive(Debug, Default)]
struct ConnectionLimitCounts {
    by_player: HashMap<String, u64>,
    by_ip: HashMap<IpAddr, u64>,
}

#[derive(Debug)]
pub struct ConnectionLimitTracker {
    max_per_player: u64,
    max_per_ip: u64,
    counts: Mutex<ConnectionLimitCounts>,
}

impl ConnectionLimitTracker {
    pub fn new(max_per_player: u64, max_per_ip: u64) -> Self {
        Self {
            max_per_player,
            max_per_ip,
            counts: Mutex::new(ConnectionLimitCounts::default()),
        }
    }

    pub fn acquire(
        self: &Arc<Self>,
        player_id: &str,
        ip: IpAddr,
    ) -> Result<ConnectionLimitGuard, ConnectionLimitExceeded> {
        let mut counts = self.counts.lock().expect("connection limit mutex poisoned");
        let player_current = counts.by_player.get(player_id).copied().unwrap_or(0);
        if self.max_per_player > 0 && player_current >= self.max_per_player {
            return Err(ConnectionLimitExceeded::Player {
                player_id: player_id.to_string(),
                current: player_current,
                limit: self.max_per_player,
            });
        }

        let ip_current = counts.by_ip.get(&ip).copied().unwrap_or(0);
        if self.max_per_ip > 0 && ip_current >= self.max_per_ip {
            return Err(ConnectionLimitExceeded::Ip {
                ip,
                current: ip_current,
                limit: self.max_per_ip,
            });
        }

        if self.max_per_player > 0 {
            counts
                .by_player
                .insert(player_id.to_string(), player_current.saturating_add(1));
        }
        if self.max_per_ip > 0 {
            counts.by_ip.insert(ip, ip_current.saturating_add(1));
        }

        Ok(ConnectionLimitGuard {
            tracker: Arc::clone(self),
            player_id: player_id.to_string(),
            ip,
            count_player: self.max_per_player > 0,
            count_ip: self.max_per_ip > 0,
            released: false,
        })
    }

    #[cfg(test)]
    fn count_for_player(&self, player_id: &str) -> u64 {
        self.counts
            .lock()
            .expect("connection limit mutex poisoned")
            .by_player
            .get(player_id)
            .copied()
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn count_for_ip(&self, ip: IpAddr) -> u64 {
        self.counts
            .lock()
            .expect("connection limit mutex poisoned")
            .by_ip
            .get(&ip)
            .copied()
            .unwrap_or(0)
    }

    fn release(&self, player_id: &str, ip: IpAddr, count_player: bool, count_ip: bool) {
        let mut counts = self.counts.lock().expect("connection limit mutex poisoned");
        if count_player {
            decrement_player_count(&mut counts.by_player, player_id);
        }
        if count_ip {
            decrement_ip_count(&mut counts.by_ip, ip);
        }
    }
}

#[derive(Debug)]
pub struct ConnectionLimitGuard {
    tracker: Arc<ConnectionLimitTracker>,
    player_id: String,
    ip: IpAddr,
    count_player: bool,
    count_ip: bool,
    released: bool,
}

impl Drop for ConnectionLimitGuard {
    fn drop(&mut self) {
        if !self.released {
            self.tracker
                .release(&self.player_id, self.ip, self.count_player, self.count_ip);
            self.released = true;
        }
    }
}

fn decrement_player_count(counts: &mut HashMap<String, u64>, player_id: &str) {
    if let Some(count) = counts.get_mut(player_id) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            counts.remove(player_id);
        }
    }
}

fn decrement_ip_count(counts: &mut HashMap<IpAddr, u64>, ip: IpAddr) {
    if let Some(count) = counts.get_mut(&ip) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            counts.remove(&ip);
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub bind_addr: String,
    pub ws_enabled: bool,
    pub ws_bind_addr: String,
    pub ws_handshake_timeout_secs: u64,
    pub ws_handshake_max_bytes: usize,
    pub ws_max_pending_handshakes: usize,
    pub ws_handshake_rate_window_secs: u64,
    pub ws_handshake_rate_max: u64,
    pub ws_trusted_proxies: websocket::TrustedProxySet,
    pub ws_max_frame_len: usize,
    pub ws_bridge_capacity: usize,
    pub heartbeat_timeout_secs: u64,
    pub max_body_len: usize,
    pub msg_rate_window_ms: u64,
    pub msg_rate_max: u64,
    pub max_connections_per_player: u64,
    pub max_connections_per_ip: u64,
    pub max_connections: u64,
    pub ticket_secret: String,
    pub redis_url: String,
    pub redis_key_prefix: String,
    pub service_instance_id: String,
    pub online_route_ttl_secs: u64,
    pub outbound_queue_capacity: usize,
}

pub struct BoundListeners {
    tcp: TcpListener,
    websocket: Option<TcpListener>,
}

impl BoundListeners {
    pub fn tcp_port(&self) -> std::io::Result<u16> {
        self.tcp.local_addr().map(|addr| addr.port())
    }

    pub fn websocket_port(&self) -> std::io::Result<Option<u16>> {
        self.websocket
            .as_ref()
            .map(TcpListener::local_addr)
            .transpose()
            .map(|addr| addr.map(|addr| addr.port()))
    }
}

pub async fn bind_listeners(config: &Config) -> std::io::Result<BoundListeners> {
    bind_listener_pair(&config.bind_addr, config.ws_enabled, &config.ws_bind_addr).await
}

async fn bind_listener_pair(
    tcp_addr: &str,
    websocket_enabled: bool,
    websocket_addr: &str,
) -> std::io::Result<BoundListeners> {
    let tcp = TcpListener::bind(tcp_addr).await?;
    let websocket = if websocket_enabled {
        Some(TcpListener::bind(websocket_addr).await?)
    } else {
        None
    };
    Ok(BoundListeners { tcp, websocket })
}

pub async fn run(
    config: Config,
    listeners: BoundListeners,
    chat_store: ChatStore,
    chat_sessions: ChatSessionMap,
    chat_push_router: ChatPushRouter,
    mut lease_loss_rx: watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error>> {
    let BoundListeners {
        tcp: listener,
        websocket: ws_listener,
    } = listeners;

    info!(
        addr = %config.bind_addr,
        max_connections = config.max_connections,
        max_connections_per_player = config.max_connections_per_player,
        max_connections_per_ip = config.max_connections_per_ip,
        "chat server listening"
    );
    if ws_listener.is_some() {
        info!(addr = %config.ws_bind_addr, "chat WebSocket adapter listening");
    } else {
        info!("chat WebSocket adapter disabled");
    }

    let connection_limits = Arc::new(ConnectionLimitTracker::new(
        config.max_connections_per_player,
        config.max_connections_per_ip,
    ));
    let connection_slots = match usize::try_from(config.max_connections) {
        Ok(0) => None,
        Ok(max_connections) => Some(Arc::new(Semaphore::new(max_connections))),
        Err(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "CHAT_MAX_CONNECTIONS exceeds this platform's usize range",
            )
            .into());
        }
    };
    let ws_handshake_slots = Arc::new(Semaphore::new(config.ws_max_pending_handshakes));
    let ws_handshake_rate = HandshakeRateLimiter::new(
        Duration::from_secs(config.ws_handshake_rate_window_secs),
        config.ws_handshake_rate_max,
    );
    let readiness_task = service_registry::readiness::spawn_from_env("chat-server").await?;

    let mut lease_lost = false;
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        let accept_result = tokio::select! {
            result = listener.accept() => Some((Transport::Tcp, result)),
            result = accept_optional(ws_listener.as_ref()) => Some((Transport::WebSocket, result)),
            _ = &mut shutdown => None,
            changed = lease_loss_rx.changed() => {
                if changed.is_err() || *lease_loss_rx.borrow_and_update() {
                    lease_lost = true;
                }
                None
            },
        };

        let Some((transport, accepted)) = accept_result else {
            if lease_lost {
                warn!("global id worker lease lost, stopping chat server");
            } else {
                info!("shutdown signal received, stopping chat server");
            }
            break;
        };
        let (socket, peer_addr) = accepted?;

        let connection_slot = match connection_slots.as_ref() {
            Some(slots) => match Arc::clone(slots).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    METRICS.record_connection_capacity_rejected();
                    warn!(
                        peer = %peer_addr.ip(),
                        transport = transport.as_str(),
                        error_category = "connection_capacity_exceeded",
                        "chat connection rejected before protocol handling"
                    );
                    drop(socket);
                    continue;
                }
            },
            None => None,
        };

        let chat_store = chat_store.clone();
        let chat_sessions = chat_sessions.clone();
        let chat_push_router = chat_push_router.clone();
        let config = config.clone();
        let connection_limits = Arc::clone(&connection_limits);

        match transport {
            Transport::Tcp => {
                tokio::spawn(async move {
                    let _connection_capacity_permit = connection_slot;
                    let _connection_capacity_metric = METRICS.track_connection_capacity();
                    let _connection_metric = METRICS.track_connection(MetricTransport::Tcp);
                    if let Err(e) = handle_connection(
                        socket,
                        ConnectionContext {
                            transport: Transport::Tcp,
                            client_ip: peer_addr.ip(),
                        },
                        chat_store,
                        chat_sessions,
                        chat_push_router,
                        config,
                        connection_limits,
                    )
                    .await
                    {
                        warn!(
                            peer = %peer_addr.ip(),
                            transport = Transport::Tcp.as_str(),
                            error_category = connection_error_category(e.as_ref()),
                            "connection handler error"
                        );
                    }
                });
            }
            Transport::WebSocket => {
                if !ws_handshake_rate.try_admit() {
                    METRICS.record_websocket_handshake_failure();
                    METRICS.record_websocket_handshake_rate_limited();
                    warn!(
                        peer = %peer_addr.ip(),
                        transport = Transport::WebSocket.as_str(),
                        error_category = "handshake_rate_exceeded",
                        "WebSocket handshake rejected before upgrade"
                    );
                    drop(socket);
                    continue;
                }
                let handshake_permit = match Arc::clone(&ws_handshake_slots).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        METRICS.record_websocket_handshake_failure();
                        warn!(
                            peer = %peer_addr.ip(),
                            transport = Transport::WebSocket.as_str(),
                            error_category = "handshake_capacity_exceeded",
                            "WebSocket handshake rejected before upgrade"
                        );
                        drop(socket);
                        continue;
                    }
                };
                tokio::spawn(async move {
                    let _connection_capacity_permit = connection_slot;
                    let _connection_capacity_metric = METRICS.track_connection_capacity();
                    let adapter_config = AdapterConfig {
                        handshake_timeout: Duration::from_secs(config.ws_handshake_timeout_secs),
                        handshake_max_bytes: config.ws_handshake_max_bytes,
                        max_frame_len: config.ws_max_frame_len,
                        max_body_len: config.max_body_len,
                        bridge_capacity: config.ws_bridge_capacity,
                        io_timeout: Duration::from_secs(config.heartbeat_timeout_secs.max(1)),
                    };
                    let handler_config = config.clone();
                    let result = websocket::serve(
                        socket,
                        adapter_config,
                        peer_addr.ip(),
                        config.ws_trusted_proxies.clone(),
                        handshake_permit,
                        move |stream, client_ip| async move {
                            match handle_connection(
                                stream,
                                ConnectionContext {
                                    transport: Transport::WebSocket,
                                    client_ip,
                                },
                                chat_store,
                                chat_sessions,
                                chat_push_router,
                                handler_config,
                                connection_limits,
                            )
                            .await
                            {
                                Ok(end) => end.websocket_close(),
                                Err(error) => {
                                    warn!(
                                        peer = %client_ip,
                                        transport = Transport::WebSocket.as_str(),
                                        error_category = connection_error_category(error.as_ref()),
                                        "connection handler error"
                                    );
                                    websocket::ApplicationClose::internal("handler_failed")
                                }
                            }
                        },
                    )
                    .await;
                    if let Err(error) = result {
                        warn!(
                            peer = %peer_addr.ip(),
                            transport = Transport::WebSocket.as_str(),
                            error_category = error.category(),
                            "WebSocket connection ended with adapter error"
                        );
                    }
                });
            }
        }
    }

    if let Some(task) = readiness_task {
        task.abort();
        let _ = task.await;
    }

    if lease_lost {
        Err(std::io::Error::other("global id worker lease lost").into())
    } else {
        Ok(())
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn accept_optional(
    listener: Option<&TcpListener>,
) -> std::io::Result<(TcpStream, std::net::SocketAddr)> {
    match listener {
        Some(listener) => listener.accept().await,
        None => std::future::pending().await,
    }
}

pub fn ticket_key(prefix: &str, ticket: &str) -> String {
    format!("{}ticket:{}", prefix, hash_ticket(ticket))
}

pub fn ticket_version_key(prefix: &str, player_id: &str) -> String {
    format!("{}player-ticket-version:{}", prefix, player_id)
}

pub fn validate_ticket_owner(
    stored_owner: Option<&str>,
    player_id: &str,
) -> Result<(), &'static str> {
    if stored_owner == Some(player_id) {
        Ok(())
    } else {
        Err("TICKET_REVOKED")
    }
}

pub fn validate_ticket_version(
    ticket_version: Option<u64>,
    current_ticket_version: Option<u64>,
) -> Result<(), &'static str> {
    if ticket_version.unwrap_or(1) == current_ticket_version.unwrap_or(1) {
        Ok(())
    } else {
        Err("TICKET_REVOKED")
    }
}

async fn write_auth_response<W>(
    writer: &mut W,
    seq: u32,
    ok: bool,
    error_code: &str,
) -> Result<(), Box<dyn std::error::Error>>
where
    W: AsyncWrite + Unpin,
{
    let res = ChatAuthRes {
        ok,
        error_code: error_code.to_string(),
    };
    let mut buf = Vec::new();
    res.encode(&mut buf)?;
    let packet = encode_packet(MessageType::ChatAuthRes as u16, seq, &buf);
    writer.write_all(&packet).await?;
    Ok(())
}

async fn handle_connection<S>(
    socket: S,
    context: ConnectionContext,
    chat_store: ChatStore,
    chat_sessions: ChatSessionMap,
    chat_push_router: ChatPushRouter,
    config: Config,
    connection_limits: Arc<ConnectionLimitTracker>,
) -> Result<ConnectionEnd, Box<dyn std::error::Error>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(socket);
    let (tx, mut rx) = mpsc::channel::<OutboundMessage>(config.outbound_queue_capacity);
    let (session_shutdown_tx, mut session_shutdown_rx) =
        watch::channel::<Option<chat_service::SessionCloseReason>>(None);
    let mut rate_limiter = ConnectionRateLimiter::new();

    // === 认证阶段 ===
    let auth_started_at = Instant::now();
    let auth = match read_auth_request(&mut reader, &mut writer, &config).await {
        Ok(auth) => auth,
        Err(e) => {
            METRICS.record_request();
            METRICS.record_latency(auth_started_at.elapsed().as_millis() as u64);
            warn!(
                peer = %context.client_ip,
                transport = context.transport.as_str(),
                error_category = auth_error_category(e.as_ref()),
                "auth failed"
            );
            return Ok(ConnectionEnd::AuthRejected);
        }
    };
    METRICS.record_request();
    METRICS.record_latency(auth_started_at.elapsed().as_millis() as u64);

    let player_id = auth.player_id;
    let _connection_limit_guard = match connection_limits.acquire(&player_id, context.client_ip) {
        Ok(guard) => guard,
        Err(error) => {
            let error_code = error.error_code();
            warn!(
                peer = %context.client_ip,
                transport = context.transport.as_str(),
                current = error.current(),
                limit = error.limit(),
                error_category = error_code,
                "chat connection limit exceeded"
            );
            write_auth_response(&mut writer, auth.seq, false, error_code).await?;
            return Ok(ConnectionEnd::ConnectionLimitExceeded);
        }
    };

    write_auth_response(&mut writer, auth.seq, true, "").await?;
    info!(
        peer = %context.client_ip,
        transport = context.transport.as_str(),
        "player authenticated"
    );
    let connection_token = new_connection_token(&config.service_instance_id);

    // 注册聊天会话
    chat_service::register_session(
        &chat_sessions,
        player_id.clone(),
        tx.clone(),
        session_shutdown_tx,
        connection_token.clone(),
    )
    .await;
    let (route_renewal_shutdown_tx, route_renewal_shutdown_rx) = watch::channel(false);
    let route_renewal_handle = if online_route::set_online_route(
        &config.redis_url,
        &config.redis_key_prefix,
        &player_id,
        &config.service_instance_id,
        &connection_token,
        config.online_route_ttl_secs,
    )
    .await
    .is_ok()
    {
        Some(tokio::spawn(
            online_route::renew_online_route_until_shutdown(
                config.redis_url.clone(),
                config.redis_key_prefix.clone(),
                player_id.clone(),
                connection_token.clone(),
                config.online_route_ttl_secs,
                route_renewal_shutdown_rx,
            ),
        ))
    } else {
        warn!(
            peer = %context.client_ip,
            transport = context.transport.as_str(),
            error_category = "online_route_set_failed",
            "failed to set chat online route"
        );
        None
    };

    // 写线程：处理所有出站消息
    let mut writer_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let packet = encode_packet(message.message_type, message.seq, &message.body);
            writer.write_all(&packet).await?;
        }
        Ok::<(), std::io::Error>(())
    });

    // === 主消息循环 ===
    let connection_end = loop {
        let mut header_buf = [0u8; HEADER_LEN];
        let read_header = tokio::select! {
            changed = session_shutdown_rx.changed() => {
                let reason = if changed.is_ok() {
                    *session_shutdown_rx.borrow_and_update()
                } else {
                    None
                };
                if let Some(reason) = reason {
                    warn!(
                        peer = %context.client_ip,
                        transport = context.transport.as_str(),
                        error_category = reason.category(),
                        "chat session requested connection close"
                    );
                    let _ = queue_terminal_error(
                        &tx,
                        reason.error_code(),
                        Duration::from_secs(config.heartbeat_timeout_secs.max(1)),
                    )
                    .await;
                    break match reason {
                        chat_service::SessionCloseReason::Replaced => {
                            ConnectionEnd::SessionReplaced
                        }
                        chat_service::SessionCloseReason::OutboundQueueFull => {
                            ConnectionEnd::OutboundQueueFull
                        }
                    };
                }
                break ConnectionEnd::TransportError;
            }
            result = timeout(
                Duration::from_secs(config.heartbeat_timeout_secs),
                reader.read_exact(&mut header_buf),
            ) => result,
        };

        match read_header {
            Ok(Ok(_)) => {}
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                info!(
                    peer = %context.client_ip,
                    transport = context.transport.as_str(),
                    "peer closed connection"
                );
                break ConnectionEnd::PeerClosed;
            }
            Ok(Err(_)) => break ConnectionEnd::TransportError,
            Err(_) => {
                warn!(
                    peer = %context.client_ip,
                    transport = context.transport.as_str(),
                    error_category = "heartbeat_timeout",
                    "heartbeat timeout"
                );
                break ConnectionEnd::IdleTimeout;
            }
        }

        let header = match parse_header(header_buf) {
            Ok(value) => value,
            Err(_) => {
                warn!(
                    peer = %context.client_ip,
                    transport = context.transport.as_str(),
                    error_category = "invalid_packet_header",
                    "invalid header"
                );
                break ConnectionEnd::ProtocolViolation;
            }
        };

        if header.body_len as usize > config.max_body_len {
            warn!(
                peer = %context.client_ip,
                transport = context.transport.as_str(),
                message_type = header.msg_type,
                error_category = "packet_body_too_large",
                "body too large"
            );
            break ConnectionEnd::ProtocolViolation;
        }

        let mut body = vec![0u8; header.body_len as usize];
        if reader.read_exact(&mut body).await.is_err() {
            break ConnectionEnd::TransportError;
        }
        let packet = Packet::new(header, body);

        let msg_type = MessageType::from_u16(packet.header.msg_type);

        let started_at = Instant::now();
        if dispatch_decision(
            &mut rate_limiter,
            started_at,
            config.msg_rate_window_ms,
            config.msg_rate_max,
        ) == DispatchDecision::RateLimited
        {
            warn!(
                peer = %context.client_ip,
                transport = context.transport.as_str(),
                message_type = packet.header.msg_type,
                window_ms = config.msg_rate_window_ms,
                max_messages = config.msg_rate_max,
                error_category = "message_rate_exceeded",
                "chat message rate exceeded"
            );
            if let Err(error) = chat_service::queue_error(
                &tx,
                packet.header.seq,
                "MSG_RATE_EXCEEDED",
                "message rate exceeded",
            ) {
                warn!(
                    peer = %context.client_ip,
                    transport = context.transport.as_str(),
                    message_type = packet.header.msg_type,
                    error_category = error.category(),
                    "failed to queue chat message rate exceeded error"
                );
                if error == chat_service::OutboundQueueError::Full {
                    let _ = queue_terminal_error(
                        &tx,
                        OUTBOUND_QUEUE_FULL,
                        Duration::from_secs(config.heartbeat_timeout_secs.max(1)),
                    )
                    .await;
                }
                break ConnectionEnd::from_outbound_queue_error(error);
            }
            continue;
        }

        let Some(msg_type) = msg_type else {
            warn!(
                peer = %context.client_ip,
                transport = context.transport.as_str(),
                message_type = packet.header.msg_type,
                error_category = "unknown_message_type",
                "unknown message type"
            );
            if let Err(error) = chat_service::queue_error(
                &tx,
                packet.header.seq,
                UNKNOWN_MESSAGE_TYPE,
                "unknown message type",
            ) {
                warn!(
                    peer = %context.client_ip,
                    transport = context.transport.as_str(),
                    message_type = packet.header.msg_type,
                    error_category = error.category(),
                    "failed to queue unknown message type response"
                );
                break ConnectionEnd::from_outbound_queue_error(error);
            }
            continue;
        };

        debug!(
            peer = %context.client_ip,
            transport = context.transport.as_str(),
            message_type = packet.header.msg_type,
            "dispatching chat client message"
        );

        // 处理聊天消息
        let queue_error = {
            let dispatch_result: Result<(), Box<dyn std::error::Error>> = match msg_type {
                MessageType::ChatPrivateReq => {
                    chat_service::handle_chat_private(
                        &chat_store,
                        &chat_sessions,
                        &chat_push_router,
                        &player_id,
                        &packet,
                        &tx,
                    )
                    .await
                }
                MessageType::ChatGroupReq => {
                    chat_service::handle_chat_group(
                        &chat_store,
                        &chat_sessions,
                        &chat_push_router,
                        &player_id,
                        &packet,
                        &tx,
                    )
                    .await
                }
                MessageType::GroupCreateReq => {
                    chat_service::handle_group_create(&chat_store, &player_id, &packet, &tx).await
                }
                MessageType::GroupJoinReq => {
                    chat_service::handle_group_join(&chat_store, &player_id, &packet, &tx).await
                }
                MessageType::GroupLeaveReq => {
                    chat_service::handle_group_leave(&chat_store, &player_id, &packet, &tx).await
                }
                MessageType::GroupDismissReq => {
                    chat_service::handle_group_dismiss(&chat_store, &player_id, &packet, &tx).await
                }
                MessageType::GroupListReq => {
                    chat_service::handle_group_list(&chat_store, &player_id, &packet, &tx).await
                }
                MessageType::ChatHistoryReq => {
                    chat_service::handle_chat_history(&chat_store, &player_id, &packet, &tx).await
                }
                _ => chat_service::queue_error(
                    &tx,
                    packet.header.seq,
                    "UNSUPPORTED_MESSAGE_TYPE",
                    "message type is not a client request",
                )
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error>),
            };

            match dispatch_result {
                Ok(()) => None,
                Err(error) => {
                    let queue_error = error
                        .downcast_ref::<chat_service::OutboundQueueError>()
                        .copied();
                    warn!(
                        peer = %context.client_ip,
                        transport = context.transport.as_str(),
                        message_type = packet.header.msg_type,
                        error_category = queue_error
                            .map(chat_service::OutboundQueueError::category)
                            .unwrap_or("business_handler_failed"),
                        "chat message handler failed"
                    );
                    queue_error
                }
            }
        };
        if let Some(queue_error) = queue_error {
            if queue_error == chat_service::OutboundQueueError::Full {
                let _ = queue_terminal_error(
                    &tx,
                    OUTBOUND_QUEUE_FULL,
                    Duration::from_secs(config.heartbeat_timeout_secs.max(1)),
                )
                .await;
            }
            break ConnectionEnd::from_outbound_queue_error(queue_error);
        }
        METRICS.record_request();
        METRICS.record_latency(started_at.elapsed().as_millis() as u64);
    };

    record_connection_end_metrics(&METRICS, context.transport, connection_end);

    let _ = route_renewal_shutdown_tx.send(true);
    if let Some(route_renewal_handle) = route_renewal_handle {
        let _ = route_renewal_handle.await;
    }

    // 注销聊天会话
    let removed_current_session =
        chat_service::unregister_session(&chat_sessions, &player_id, &tx).await;
    if removed_current_session
        && online_route::clear_online_route(
            &config.redis_url,
            &config.redis_key_prefix,
            &player_id,
            &config.service_instance_id,
            &connection_token,
        )
        .await
        .is_err()
    {
        warn!(
            peer = %context.client_ip,
            transport = context.transport.as_str(),
            error_category = "online_route_clear_failed",
            "failed to clear chat online route"
        );
    }

    drop(tx);
    if timeout(
        Duration::from_secs(config.heartbeat_timeout_secs.max(1)),
        &mut writer_task,
    )
    .await
    .is_err()
    {
        writer_task.abort();
        let _ = writer_task.await;
    }

    Ok(connection_end)
}

#[derive(Debug)]
struct ClassifiedError {
    category: &'static str,
}

impl ClassifiedError {
    const fn new(category: &'static str) -> Self {
        Self { category }
    }
}

impl std::fmt::Display for ClassifiedError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.category)
    }
}

impl std::error::Error for ClassifiedError {}

fn connection_error_category(error: &(dyn std::error::Error + 'static)) -> &'static str {
    error
        .downcast_ref::<ClassifiedError>()
        .map(|error| error.category)
        .unwrap_or("connection_handler_failed")
}

fn auth_error_category(error: &(dyn std::error::Error + 'static)) -> &'static str {
    error
        .downcast_ref::<ClassifiedError>()
        .map(|error| error.category)
        .unwrap_or("auth_failed")
}

async fn queue_terminal_error(
    tx: &mpsc::Sender<OutboundMessage>,
    error_code: &'static str,
    send_timeout: Duration,
) -> bool {
    let res = ErrorRes {
        error_code: error_code.to_string(),
        message: "connection closed by server policy".to_string(),
    };
    let mut body = Vec::new();
    if res.encode(&mut body).is_err() {
        return false;
    }
    timeout(
        send_timeout,
        tx.send(OutboundMessage {
            message_type: MessageType::ErrorRes as u16,
            seq: 0,
            body,
        }),
    )
    .await
    .is_ok_and(|result| result.is_ok())
}

#[derive(Debug)]
struct AuthenticatedPlayer {
    player_id: String,
    seq: u32,
}

async fn read_auth_request<R, W>(
    reader: &mut R,
    writer: &mut W,
    config: &Config,
) -> Result<AuthenticatedPlayer, Box<dyn std::error::Error>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // 读取认证请求头
    let mut header_buf = [0u8; HEADER_LEN];
    timeout(
        Duration::from_secs(config.heartbeat_timeout_secs),
        reader.read_exact(&mut header_buf),
    )
    .await
    .map_err(|_| ClassifiedError::new("auth_timeout"))?
    .map_err(|_| ClassifiedError::new("auth_read_failed"))?;

    let header =
        parse_header(header_buf).map_err(|_| ClassifiedError::new("invalid_auth_packet_header"))?;
    match MessageType::from_u16(header.msg_type) {
        Some(MessageType::ChatAuthReq) => {}
        _ => {
            write_auth_response(writer, header.seq, false, "AUTH_REQUIRED")
                .await
                .map_err(|_| ClassifiedError::new("auth_response_write_failed"))?;
            return Err(ClassifiedError::new("auth_message_required").into());
        }
    }

    if header.body_len as usize > config.max_body_len {
        write_auth_response(writer, header.seq, false, "AUTH_PACKET_TOO_LARGE")
            .await
            .map_err(|_| ClassifiedError::new("auth_response_write_failed"))?;
        return Err(ClassifiedError::new("auth_packet_too_large").into());
    }

    let mut body = vec![0u8; header.body_len as usize];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|_| ClassifiedError::new("auth_body_read_failed"))?;

    // 解析认证请求
    let auth_req = match ChatAuthReq::decode(&*body) {
        Ok(request) => request,
        Err(_) => {
            write_auth_response(writer, header.seq, false, "INVALID_AUTH_REQUEST")
                .await
                .map_err(|_| ClassifiedError::new("auth_response_write_failed"))?;
            return Err(ClassifiedError::new("invalid_auth_request").into());
        }
    };

    // 使用与 game-server 相同的票据验证逻辑
    match verify_ticket(&config.ticket_secret, &auth_req.token) {
        Ok(ticket_payload) => {
            let player_id = ticket_payload.player_id;
            let redis_client = match redis::Client::open(config.redis_url.as_str()) {
                Ok(client) => client,
                Err(_) => {
                    write_auth_response(writer, header.seq, false, "AUTH_UNAVAILABLE")
                        .await
                        .map_err(|_| ClassifiedError::new("auth_response_write_failed"))?;
                    return Err(ClassifiedError::new("auth_store_unavailable").into());
                }
            };
            let mut redis = match redis_client.get_multiplexed_async_connection().await {
                Ok(redis) => redis,
                Err(_) => {
                    write_auth_response(writer, header.seq, false, "AUTH_UNAVAILABLE")
                        .await
                        .map_err(|_| ClassifiedError::new("auth_response_write_failed"))?;
                    return Err(ClassifiedError::new("auth_store_unavailable").into());
                }
            };
            let ticket_key = ticket_key(&config.redis_key_prefix, &auth_req.token);
            let ticket_version_key = ticket_version_key(&config.redis_key_prefix, &player_id);
            let ticket_owner: Option<String> = match redis.get(ticket_key).await {
                Ok(owner) => owner,
                Err(_) => {
                    write_auth_response(writer, header.seq, false, "AUTH_UNAVAILABLE")
                        .await
                        .map_err(|_| ClassifiedError::new("auth_response_write_failed"))?;
                    return Err(ClassifiedError::new("auth_store_unavailable").into());
                }
            };
            if let Err(error_code) = validate_ticket_owner(ticket_owner.as_deref(), &player_id) {
                write_auth_response(writer, header.seq, false, error_code)
                    .await
                    .map_err(|_| ClassifiedError::new("auth_response_write_failed"))?;
                return Err(ClassifiedError::new("ticket_revoked").into());
            }

            let current_ticket_version: Option<u64> = match redis.get(ticket_version_key).await {
                Ok(version) => version,
                Err(_) => {
                    write_auth_response(writer, header.seq, false, "AUTH_UNAVAILABLE")
                        .await
                        .map_err(|_| ClassifiedError::new("auth_response_write_failed"))?;
                    return Err(ClassifiedError::new("auth_store_unavailable").into());
                }
            };
            if let Err(error_code) =
                validate_ticket_version(ticket_payload.ver, current_ticket_version)
            {
                write_auth_response(writer, header.seq, false, error_code)
                    .await
                    .map_err(|_| ClassifiedError::new("auth_response_write_failed"))?;
                return Err(ClassifiedError::new("ticket_revoked").into());
            }

            Ok(AuthenticatedPlayer {
                player_id,
                seq: header.seq,
            })
        }
        Err(error_code) => {
            write_auth_response(writer, header.seq, false, error_code)
                .await
                .map_err(|_| ClassifiedError::new("auth_response_write_failed"))?;
            Err(ClassifiedError::new("ticket_invalid").into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ticket::hash_ticket;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;

    #[test]
    fn websocket_handshake_rate_limiter_rejects_after_window_capacity() {
        let limiter = HandshakeRateLimiter::new(Duration::from_secs(1), 2);

        assert!(limiter.try_admit());
        assert!(limiter.try_admit());
        assert!(!limiter.try_admit());
    }

    #[test]
    fn ticket_key_uses_prefix_and_sha256_hash() {
        let ticket = "payload.signature";
        assert_eq!(
            ticket_key("dev:", ticket),
            format!("dev:ticket:{}", hash_ticket(ticket))
        );
    }

    #[test]
    fn ticket_version_key_uses_prefix_and_player_id() {
        assert_eq!(
            ticket_version_key("dev:", "player-1"),
            "dev:player-ticket-version:player-1"
        );
    }

    #[test]
    fn connection_tokens_are_unique_within_an_instance_process() {
        let first = new_connection_token("chat-a");
        let second = new_connection_token("chat-a");

        assert_ne!(first, second);
        assert!(first.starts_with("chat-a:"));
        assert!(second.starts_with("chat-a:"));
    }

    #[test]
    fn validate_ticket_owner_accepts_matching_owner() {
        assert_eq!(validate_ticket_owner(Some("player-1"), "player-1"), Ok(()));
    }

    #[test]
    fn validate_ticket_owner_rejects_missing_owner_as_revoked() {
        assert_eq!(
            validate_ticket_owner(None, "player-1"),
            Err("TICKET_REVOKED")
        );
    }

    #[test]
    fn validate_ticket_owner_rejects_mismatch_as_revoked() {
        assert_eq!(
            validate_ticket_owner(Some("player-2"), "player-1"),
            Err("TICKET_REVOKED")
        );
    }

    #[test]
    fn validate_ticket_version_accepts_matching_or_missing_versions() {
        assert_eq!(validate_ticket_version(Some(2), Some(2)), Ok(()));
        assert_eq!(validate_ticket_version(None, None), Ok(()));
    }

    #[test]
    fn validate_ticket_version_rejects_mismatch_as_revoked() {
        assert_eq!(
            validate_ticket_version(Some(2), Some(3)),
            Err("TICKET_REVOKED")
        );
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
    fn connection_limits_disabled_do_not_track_counts() {
        let tracker = Arc::new(ConnectionLimitTracker::new(0, 0));
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        let guard = tracker.acquire("player-1", ip).unwrap();

        assert_eq!(tracker.count_for_player("player-1"), 0);
        assert_eq!(tracker.count_for_ip(ip), 0);

        drop(guard);
        assert_eq!(tracker.count_for_player("player-1"), 0);
        assert_eq!(tracker.count_for_ip(ip), 0);
    }

    #[test]
    fn connection_limits_reject_player_at_boundary_and_release_on_drop() {
        let tracker = Arc::new(ConnectionLimitTracker::new(1, 0));
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let first = tracker.acquire("player-1", ip).unwrap();

        assert_eq!(
            tracker.acquire("player-1", ip).unwrap_err(),
            ConnectionLimitExceeded::Player {
                player_id: "player-1".to_string(),
                current: 1,
                limit: 1,
            }
        );
        assert_eq!(tracker.count_for_player("player-1"), 1);

        drop(first);
        assert_eq!(tracker.count_for_player("player-1"), 0);
        let second = tracker.acquire("player-1", ip).unwrap();
        assert_eq!(tracker.count_for_player("player-1"), 1);
        drop(second);
    }

    #[test]
    fn connection_limits_reject_ip_without_incrementing_player_count() {
        let tracker = Arc::new(ConnectionLimitTracker::new(2, 1));
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let first = tracker.acquire("player-1", ip).unwrap();

        assert_eq!(
            tracker.acquire("player-2", ip).unwrap_err(),
            ConnectionLimitExceeded::Ip {
                ip,
                current: 1,
                limit: 1,
            }
        );
        assert_eq!(tracker.count_for_player("player-2"), 0);
        assert_eq!(tracker.count_for_ip(ip), 1);

        drop(first);
        assert_eq!(tracker.count_for_ip(ip), 0);
    }

    #[test]
    fn dispatch_decision_blocks_over_limit_before_business_dispatch() {
        let mut limiter = ConnectionRateLimiter::new();
        let now = Instant::now();

        assert_eq!(
            dispatch_decision(&mut limiter, now, 1000, 1),
            DispatchDecision::Dispatch
        );
        assert_eq!(
            dispatch_decision(&mut limiter, now, 1000, 1),
            DispatchDecision::RateLimited
        );
    }

    #[test]
    fn tcp_and_websocket_connections_share_the_same_player_and_ip_quota() {
        let tracker = Arc::new(ConnectionLimitTracker::new(1, 1));
        let client_ip = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 8));
        let _tcp_connection = tracker.acquire("player-1", client_ip).unwrap();

        assert!(matches!(
            tracker.acquire("player-1", IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9))),
            Err(ConnectionLimitExceeded::Player { .. })
        ));
        assert!(matches!(
            tracker.acquire("player-2", client_ip),
            Err(ConnectionLimitExceeded::Ip { .. })
        ));
    }

    #[test]
    fn websocket_application_close_categories_are_stable() {
        assert_eq!(
            ConnectionEnd::AuthRejected.websocket_close(),
            websocket::ApplicationClose::policy("auth_rejected")
        );
        assert_eq!(
            ConnectionEnd::SessionReplaced.websocket_close(),
            websocket::ApplicationClose::policy("session_replaced")
        );
        assert_eq!(
            ConnectionEnd::OutboundQueueFull.websocket_close(),
            websocket::ApplicationClose::overloaded("outbound_queue_full")
        );
        assert_eq!(
            ConnectionEnd::OutboundQueueClosed.websocket_close(),
            websocket::ApplicationClose::internal("outbound_queue_closed")
        );
    }

    #[test]
    fn outbound_queue_full_and_closed_map_to_exact_transport_metrics() {
        let metrics = MetricsCollector::new();

        for error in [
            chat_service::OutboundQueueError::Full,
            chat_service::OutboundQueueError::Closed,
        ] {
            let end = ConnectionEnd::from_outbound_queue_error(error);
            assert!(end.is_outbound_queue_failure());
            record_connection_end_metrics(&metrics, Transport::Tcp, end);
            record_connection_end_metrics(&metrics, Transport::WebSocket, end);
        }
        record_connection_end_metrics(&metrics, Transport::Tcp, ConnectionEnd::TransportError);
        record_connection_end_metrics(
            &metrics,
            Transport::WebSocket,
            ConnectionEnd::ProtocolViolation,
        );

        assert_eq!(
            metrics.outbound_queue_failure_count(MetricTransport::Tcp),
            2
        );
        assert_eq!(
            metrics.outbound_queue_failure_count(MetricTransport::WebSocket),
            2
        );
    }

    #[tokio::test]
    async fn websocket_bind_failure_rejects_the_complete_listener_set() {
        let occupied = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let occupied_addr = occupied.local_addr().unwrap().to_string();

        let result = bind_listener_pair("127.0.0.1:0", true, &occupied_addr).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn terminal_queue_failure_response_uses_a_stable_protocol_error() {
        let (tx, mut rx) = mpsc::channel(1);

        assert!(queue_terminal_error(&tx, OUTBOUND_QUEUE_FULL, Duration::from_secs(1)).await);
        let outbound = rx.recv().await.unwrap();
        let error = ErrorRes::decode(outbound.body.as_slice()).unwrap();

        assert_eq!(outbound.message_type, MessageType::ErrorRes as u16);
        assert_eq!(outbound.seq, 0);
        assert_eq!(error.error_code, OUTBOUND_QUEUE_FULL);
    }
}
