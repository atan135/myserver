use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use interprocess::local_socket::tokio::Stream as LocalSocketStream;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf, copy_bidirectional};
use tokio::net::TcpStream as TokioTcpStream;
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use crate::auth::ProxyAuthService;
use crate::config::Config;
use crate::connection_limits::{ConnectionLimiter, PlayerConnectionTracker};
use crate::maintenance::{
    GlobalMaintenanceChecker, MAINTENANCE_MODE_ERROR, should_reject_new_auth,
};
use crate::metrics::METRICS;
use crate::pb::{
    AuthReq, AuthRes, ErrorRes, PingRes, RoomJoinAsObserverReq, RoomJoinAsObserverRes, RoomJoinReq,
    RoomJoinRes, RoomReconnectReq, RoomReconnectRes,
};
use crate::protocol::{MessageType, Packet, encode_body, encode_packet, read_packet};
use crate::route_store::{
    ProxyRouteStore, UpstreamHealthState, UpstreamOperationState, UpstreamRoute,
};
use crate::session::{ProxySession, ProxySessionState};
use crate::upstream::connect_upstream;
use service_registry::RegistryClient;

const MAX_PROXY_BODY_LEN: usize = 1024 * 1024;
const PREAUTH_MESSAGE_NOT_ALLOWED: &str = "PREAUTH_MESSAGE_NOT_ALLOWED";
const MSG_RATE_EXCEEDED: &str = "MSG_RATE_EXCEEDED";

pub type SharedConnectionCount = Arc<AtomicU64>;
pub type SharedMaintenanceFlag = Arc<RwLock<bool>>;

#[derive(Default)]
struct DeferredAuthState {
    auth_req_packet: Option<Vec<u8>>,
    player_id: Option<String>,
}

struct ActiveConnectionGuard {
    connection_count: SharedConnectionCount,
    released: bool,
}

impl ActiveConnectionGuard {
    fn try_acquire(
        connection_count: SharedConnectionCount,
        max_connections: u64,
    ) -> Result<Self, u64> {
        if max_connections == 0 {
            let connections = connection_count.fetch_add(1, Ordering::Relaxed) + 1;
            METRICS.set_connections(connections);
            return Ok(Self {
                connection_count,
                released: false,
            });
        }

        let mut current = connection_count.load(Ordering::Relaxed);
        loop {
            if current >= max_connections {
                return Err(current);
            }

            match connection_count.compare_exchange_weak(
                current,
                current + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    METRICS.set_connections(current + 1);
                    return Ok(Self {
                        connection_count,
                        released: false,
                    });
                }
                Err(actual) => current = actual,
            }
        }
    }

    fn release(&mut self) {
        if self.released {
            return;
        }

        let previous = self.connection_count.fetch_sub(1, Ordering::Relaxed);
        METRICS.set_connections(previous.saturating_sub(1));
        self.released = true;
    }
}

impl Drop for ActiveConnectionGuard {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreauthDecision {
    HandleLocally,
    AllowUpstreamSelection,
    Reject(&'static str),
}

enum ProxyStream {
    Kcp(tokio_kcp::KcpStream),
    Tcp(TokioTcpStream),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MsgRateLimitConfig {
    window: Duration,
    max: u64,
}

impl MsgRateLimitConfig {
    fn new(window_ms: u64, max: u64) -> Self {
        Self {
            window: Duration::from_millis(window_ms.max(1)),
            max,
        }
    }
}

#[derive(Debug)]
struct MsgRateLimiter {
    config: MsgRateLimitConfig,
    window_started_at: Instant,
    count: u64,
}

impl MsgRateLimiter {
    fn new(config: MsgRateLimitConfig, now: Instant) -> Self {
        Self {
            config,
            window_started_at: now,
            count: 0,
        }
    }

    fn check(&mut self, now: Instant) -> MsgRateDecision {
        if self.config.max == 0 {
            return MsgRateDecision::Allowed;
        }

        if now.duration_since(self.window_started_at) >= self.config.window {
            self.window_started_at = now;
            self.count = 0;
        }

        self.count = self.count.saturating_add(1);
        if self.count > self.config.max {
            MsgRateDecision::Exceeded
        } else {
            MsgRateDecision::Allowed
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MsgRateDecision {
    Allowed,
    Exceeded,
}

impl AsyncRead for ProxyStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            ProxyStream::Kcp(stream) => Pin::new(stream).poll_read(cx, buf),
            ProxyStream::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ProxyStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            ProxyStream::Kcp(stream) => Pin::new(stream).poll_write(cx, buf),
            ProxyStream::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            ProxyStream::Kcp(stream) => Pin::new(stream).poll_flush(cx),
            ProxyStream::Tcp(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            ProxyStream::Kcp(stream) => Pin::new(stream).poll_shutdown(cx),
            ProxyStream::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

pub async fn run(
    config: &Config,
    route_store: ProxyRouteStore,
    auth_service: Arc<ProxyAuthService>,
    global_maintenance: Arc<GlobalMaintenanceChecker>,
    connection_count: SharedConnectionCount,
    maintenance: SharedMaintenanceFlag,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut kcp_frontend =
        crate::transport::kcp_frontend::KcpFrontend::bind(&config.bind_addr()).await?;
    info!(addr = %config.bind_addr(), protocol = "kcp", "game-proxy frontend listening");

    let tcp_addr = config.tcp_fallback_addr();
    let mut tcp_frontend = crate::transport::tcp_frontend::TcpFrontend::bind(&tcp_addr).await?;
    info!(addr = %tcp_addr, protocol = "tcp", "game-proxy tcp fallback frontend listening");

    if config.registry_enabled {
        let registry_url = config.registry_url.clone();
        let service_name = config.upstream_service_name.clone();
        let discover_interval = config.registry_discover_interval_secs;
        let route_store_clone = route_store.clone();

        tokio::spawn(async move {
            if let Err(error) = run_upstream_discovery(
                registry_url,
                service_name,
                discover_interval,
                route_store_clone,
            )
            .await
            {
                tracing::error!(error = %error, "upstream discovery stopped");
            }
        });
    } else {
        tracing::info!(
            upstream_server_id = %config.upstream_server_id,
            upstream_local_socket_name = %config.upstream_local_socket_name,
            "using static upstream config"
        );
    }

    let mut next_session_id = 1u64;
    let connection_limiter = ConnectionLimiter::new(config.connection_limits.clone());

    loop {
        tokio::select! {
            kcp_result = kcp_frontend.accept() => {
                match kcp_result {
                    Ok((client_stream, client_addr)) => {
                        let route_store = route_store.clone();
                        let auth_service = auth_service.clone();
                        let global_maintenance = global_maintenance.clone();
                        let connection_count = connection_count.clone();
                        let maintenance = maintenance.clone();
                        let session_id = next_session_id;
                        let max_connections = config.proxy_max_connections;
                        let max_preauth_failures = config.proxy_max_preauth_failures;
                        let msg_rate_config = MsgRateLimitConfig::new(
                            config.proxy_msg_rate_window_ms,
                            config.proxy_msg_rate_max,
                        );
                        let connection_limiter = connection_limiter.clone();
                        next_session_id = next_session_id.saturating_add(1);

                        tokio::spawn(async move {
                            let stream = ProxyStream::Kcp(client_stream);
                            if let Err(error) = handle_session(
                                session_id,
                                client_addr,
                                stream,
                                max_connections,
                                max_preauth_failures,
                                msg_rate_config,
                                route_store,
                                auth_service,
                                global_maintenance,
                                connection_count,
                                maintenance,
                                connection_limiter,
                            )
                            .await
                            {
                                warn!(session_id = session_id, error = %error, "proxy session failed");
                            }
                        });
                    }
                    Err(error) => warn!(error = %error, "kcp accept failed"),
                }
            }
            tcp_result = tcp_frontend.accept() => {
                match tcp_result {
                    Ok((client_stream, client_addr)) => {
                        let route_store = route_store.clone();
                        let auth_service = auth_service.clone();
                        let global_maintenance = global_maintenance.clone();
                        let connection_count = connection_count.clone();
                        let maintenance = maintenance.clone();
                        let session_id = next_session_id;
                        let max_connections = config.proxy_max_connections;
                        let max_preauth_failures = config.proxy_max_preauth_failures;
                        let msg_rate_config = MsgRateLimitConfig::new(
                            config.proxy_msg_rate_window_ms,
                            config.proxy_msg_rate_max,
                        );
                        let connection_limiter = connection_limiter.clone();
                        next_session_id = next_session_id.saturating_add(1);

                        tokio::spawn(async move {
                            let stream = ProxyStream::Tcp(client_stream);
                            if let Err(error) = handle_session(
                                session_id,
                                client_addr,
                                stream,
                                max_connections,
                                max_preauth_failures,
                                msg_rate_config,
                                route_store,
                                auth_service,
                                global_maintenance,
                                connection_count,
                                maintenance,
                                connection_limiter,
                            )
                            .await
                            {
                                warn!(session_id = session_id, error = %error, "proxy session failed");
                            }
                        });
                    }
                    Err(error) => warn!(error = %error, "tcp accept failed"),
                }
            }
        }
    }
}

async fn run_upstream_discovery(
    registry_url: String,
    service_name: String,
    discover_interval_secs: u64,
    route_store: ProxyRouteStore,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = RegistryClient::new(&registry_url, "proxy", "proxy-static").await?;
    discover_and_update_routes(&client, &service_name, &route_store).await?;

    let mut ticker =
        tokio::time::interval(tokio::time::Duration::from_secs(discover_interval_secs));
    loop {
        ticker.tick().await;
        if let Err(error) = discover_and_update_routes(&client, &service_name, &route_store).await {
            tracing::warn!(error = %error, "failed to discover upstream");
        }
    }
}

async fn discover_and_update_routes(
    client: &RegistryClient,
    service_name: &str,
    route_store: &ProxyRouteStore,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let routes: Vec<UpstreamRoute> = client
        .discover(service_name)
        .await?
        .into_iter()
        .map(|instance| UpstreamRoute {
            server_id: instance.id,
            local_socket_name: instance.local_socket,
            operation_state: UpstreamOperationState::Active,
            health_state: UpstreamHealthState::Healthy,
        })
        .collect();

    route_store.sync_discovered_routes(routes).await;

    tracing::info!(service = %service_name, "upstream routes refreshed");
    Ok(())
}

async fn handle_session<S: AsyncRead + AsyncWrite + Send + Unpin + 'static>(
    session_id: u64,
    client_addr: std::net::SocketAddr,
    mut client_stream: S,
    max_connections: u64,
    max_preauth_failures: u32,
    msg_rate_config: MsgRateLimitConfig,
    route_store: ProxyRouteStore,
    auth_service: Arc<ProxyAuthService>,
    global_maintenance: Arc<GlobalMaintenanceChecker>,
    connection_count: SharedConnectionCount,
    maintenance: SharedMaintenanceFlag,
    connection_limiter: ConnectionLimiter,
) -> Result<(), Box<dyn std::error::Error>> {
    let client_ip = client_addr.ip();
    let _ip_connection_guard = match connection_limiter.try_acquire_ip(client_ip) {
        Ok(guard) => guard,
        Err(error) => {
            warn!(
                session_id,
                client_addr = %client_addr,
                client_ip = %client_ip,
                error_code = error.code(),
                "proxy connection rejected by local connection governance"
            );
            return Err(Box::new(std::io::Error::other(error.code())));
        }
    };

    let mut connection_guard =
        match ActiveConnectionGuard::try_acquire(connection_count, max_connections) {
            Ok(guard) => guard,
            Err(current) => {
                warn!(
                    session_id,
                    client_addr = %client_addr,
                    current_connections = current,
                    max_connections,
                    "proxy connection rejected by max connection limit"
                );
                return Err(Box::new(std::io::Error::other(
                    "proxy max connections exceeded",
                )));
            }
        };
    let mut session = ProxySession::new(session_id);
    let mut deferred_auth = DeferredAuthState::default();
    let mut player_connection_tracker = PlayerConnectionTracker::default();
    let mut preauth_failures = 0u32;
    let mut msg_rate_limiter = MsgRateLimiter::new(msg_rate_config, Instant::now());

    loop {
        let Some(packet) = read_packet(&mut client_stream, MAX_PROXY_BODY_LEN).await? else {
            session.state = ProxySessionState::Closed;
            info!(session_id = session.id, client_addr = %client_addr, "proxy session closed before upstream bind");
            return Ok(());
        };

        if msg_rate_limiter.check(Instant::now()) == MsgRateDecision::Exceeded {
            write_msg_rate_exceeded(&mut client_stream, packet.header.seq).await?;
            warn!(
                session_id = session.id,
                client_addr = %client_addr,
                peer = %client_addr,
                player_id = session.player_id.as_deref().or(deferred_auth.player_id.as_deref()).unwrap_or_default(),
                msg_type = packet.header.msg_type,
                window_ms = msg_rate_config.window.as_millis() as u64,
                max = msg_rate_config.max,
                "proxy inbound message rate exceeded before preauth decision"
            );
            continue;
        }

        match preauth_decision(&session, &packet) {
            PreauthDecision::Reject(error_code) => {
                preauth_failures = preauth_failures.saturating_add(1);
                write_proxy_error(
                    &mut client_stream,
                    packet.header.seq,
                    error_code,
                    error_code,
                )
                .await?;
                warn!(
                    session_id = session.id,
                    client_addr = %client_addr,
                    msg_type = packet.header.msg_type,
                    preauth_failures,
                    max_preauth_failures,
                    "pre-auth message rejected"
                );

                if max_preauth_failures > 0 && preauth_failures >= max_preauth_failures {
                    session.state = ProxySessionState::Closed;
                    warn!(
                        session_id = session.id,
                        client_addr = %client_addr,
                        preauth_failures,
                        "proxy session closed by pre-auth failure threshold"
                    );
                    return Ok(());
                }

                continue;
            }
            PreauthDecision::HandleLocally | PreauthDecision::AllowUpstreamSelection => {}
        }

        match packet.message_type() {
            Some(MessageType::AuthReq) => {
                session.state = ProxySessionState::Authenticating;
                let auth_ok = handle_local_auth(
                    &mut client_stream,
                    &packet,
                    &auth_service,
                    &global_maintenance,
                    &maintenance,
                    &mut deferred_auth,
                    &mut session,
                    &connection_limiter,
                    &mut player_connection_tracker,
                )
                .await?;
                if auth_ok {
                    session.state = ProxySessionState::Authenticated;
                    preauth_failures = 0;
                } else {
                    session.state = ProxySessionState::Connected;
                    preauth_failures = preauth_failures.saturating_add(1);
                    if max_preauth_failures > 0 && preauth_failures >= max_preauth_failures {
                        session.state = ProxySessionState::Closed;
                        warn!(
                            session_id = session.id,
                            client_addr = %client_addr,
                            preauth_failures,
                            "proxy session closed by auth failure threshold"
                        );
                        return Ok(());
                    }
                }
            }
            Some(MessageType::PingReq) if session.upstream_server_id.is_none() => {
                write_message(
                    &mut client_stream,
                    MessageType::PingRes,
                    packet.header.seq,
                    &PingRes {
                        server_time: current_unix_ms() as i64,
                    },
                )
                .await?;
            }
            _ => {
                session.state = ProxySessionState::SelectingUpstream;

                let route = match select_route_for_packet(
                    &route_store,
                    &packet,
                    deferred_auth.player_id.as_deref(),
                )
                .await
                {
                    Ok(route) => route,
                    Err(error_code) => {
                        restore_authenticated_after_local_routing_error(&mut session);
                        write_proxy_error(
                            &mut client_stream,
                            packet.header.seq,
                            error_code,
                            error_code,
                        )
                        .await?;
                        continue;
                    }
                };

                let connect_started_at = Instant::now();
                let mut upstream = connect_upstream(&route).await?;
                METRICS.record_request();
                METRICS.record_latency(connect_started_at.elapsed().as_millis() as u64);

                session.upstream_server_id = Some(route.server_id.clone());
                if let Some(player_id) = deferred_auth.player_id.clone() {
                    session.player_id = Some(player_id);
                }

                if let Some(auth_packet) = deferred_auth.auth_req_packet.as_deref() {
                    session.state = ProxySessionState::ReplayingAuth;
                    replay_auth_to_upstream(
                        &mut upstream,
                        auth_packet,
                        deferred_auth.player_id.as_deref(),
                    )
                    .await?;
                }

                session.state = ProxySessionState::Proxying;

                info!(
                    session_id = session.id,
                    client_addr = %client_addr,
                    upstream_server_id = %route.server_id,
                    upstream_local_socket_name = %route.local_socket_name,
                    player_id = session.player_id.as_deref().unwrap_or_default(),
                    "proxy upstream bound"
                );

                match async {
                    upstream.write_all(&packet.to_bytes()).await?;
                    let first_response = read_packet(&mut upstream, MAX_PROXY_BODY_LEN)
                        .await?
                        .ok_or_else(|| {
                            std::io::Error::other("upstream closed before first response")
                        })?;

                    update_routing_metadata(
                        &route_store,
                        &packet,
                        &first_response,
                        &route,
                        &deferred_auth,
                        &mut session,
                    )
                    .await;

                    client_stream.write_all(&first_response.to_bytes()).await?;
                    Ok::<(), std::io::Error>(())
                }
                .await
                {
                    Ok(()) => {}
                    Err(error) => {
                        session.state = ProxySessionState::Closed;
                        return Err(Box::new(error));
                    }
                }

                let result = if msg_rate_config.max == 0 {
                    copy_bidirectional(&mut client_stream, &mut upstream).await
                } else {
                    proxy_bound_streams(
                        client_stream,
                        upstream,
                        msg_rate_limiter,
                        msg_rate_config,
                        session.id,
                        client_addr,
                        session.player_id.clone().unwrap_or_default(),
                    )
                    .await
                };
                session.state = ProxySessionState::Closed;
                connection_guard.release();

                return match result {
                    Ok((from_client, from_upstream)) => {
                        info!(
                            session_id = session.id,
                            room_id = session.room_id.as_deref().unwrap_or_default(),
                            bytes_from_client = from_client,
                            bytes_from_upstream = from_upstream,
                            "proxy session closed"
                        );
                        Ok(())
                    }
                    Err(error) => Err(Box::new(error)),
                };
            }
        }
    }
}

fn preauth_decision(session: &ProxySession, packet: &Packet) -> PreauthDecision {
    if is_authenticated_for_upstream(session) {
        return PreauthDecision::AllowUpstreamSelection;
    }

    match packet.message_type() {
        Some(MessageType::AuthReq) | Some(MessageType::PingReq) => PreauthDecision::HandleLocally,
        _ => PreauthDecision::Reject(PREAUTH_MESSAGE_NOT_ALLOWED),
    }
}

fn is_authenticated_for_upstream(session: &ProxySession) -> bool {
    session.state == ProxySessionState::Authenticated && session.player_id.is_some()
}

fn restore_authenticated_after_local_routing_error(session: &mut ProxySession) {
    if session.player_id.is_some() && session.upstream_server_id.is_none() {
        session.state = ProxySessionState::Authenticated;
    }
}

async fn handle_local_auth<S: AsyncWrite + Unpin>(
    client_stream: &mut S,
    packet: &Packet,
    auth_service: &ProxyAuthService,
    global_maintenance: &GlobalMaintenanceChecker,
    local_maintenance: &SharedMaintenanceFlag,
    deferred_auth: &mut DeferredAuthState,
    session: &mut ProxySession,
    connection_limiter: &ConnectionLimiter,
    player_connection_tracker: &mut PlayerConnectionTracker,
) -> Result<bool, Box<dyn std::error::Error>> {
    let request = match packet.decode_body::<AuthReq>("INVALID_AUTH_BODY") {
        Ok(request) => request,
        Err(error_code) => {
            deferred_auth.auth_req_packet = None;
            deferred_auth.player_id = None;
            session.player_id = None;
            player_connection_tracker.clear();
            write_proxy_error(
                client_stream,
                packet.header.seq,
                error_code,
                "invalid auth body",
            )
            .await?;
            return Ok(false);
        }
    };

    let local_enabled = *local_maintenance.read().await;
    let global_enabled = if local_enabled {
        false
    } else {
        match global_maintenance.is_enabled().await {
            Ok(enabled) => enabled,
            Err(error_code) => {
                deferred_auth.auth_req_packet = None;
                deferred_auth.player_id = None;
                session.player_id = None;
                player_connection_tracker.clear();
                write_message(
                    client_stream,
                    MessageType::AuthRes,
                    packet.header.seq,
                    &AuthRes {
                        ok: false,
                        player_id: String::new(),
                        error_code: error_code.to_string(),
                    },
                )
                .await?;
                return Ok(false);
            }
        }
    };
    if should_reject_new_auth(local_enabled, global_enabled) {
        deferred_auth.auth_req_packet = None;
        deferred_auth.player_id = None;
        session.player_id = None;
        player_connection_tracker.clear();
        write_message(
            client_stream,
            MessageType::AuthRes,
            packet.header.seq,
            &AuthRes {
                ok: false,
                player_id: String::new(),
                error_code: MAINTENANCE_MODE_ERROR.to_string(),
            },
        )
        .await?;
        return Ok(false);
    }

    match auth_service.authenticate_ticket(&request.ticket).await {
        Ok(player_id) => {
            if let Err(error) = player_connection_tracker
                .replace_authenticated_player(connection_limiter, &player_id)
            {
                deferred_auth.auth_req_packet = None;
                deferred_auth.player_id = None;
                session.player_id = None;
                player_connection_tracker.clear();
                let error_code = error.code();
                warn!(
                    session_id = session.id,
                    player_id = %player_id,
                    error_code,
                    "proxy auth rejected by player connection limit"
                );
                write_message(
                    client_stream,
                    MessageType::AuthRes,
                    packet.header.seq,
                    &AuthRes {
                        ok: false,
                        player_id: String::new(),
                        error_code: error_code.to_string(),
                    },
                )
                .await?;
                return Ok(false);
            }

            deferred_auth.auth_req_packet = Some(packet.to_bytes());
            deferred_auth.player_id = Some(player_id.clone());
            session.player_id = Some(player_id.clone());
            write_message(
                client_stream,
                MessageType::AuthRes,
                packet.header.seq,
                &AuthRes {
                    ok: true,
                    player_id,
                    error_code: String::new(),
                },
            )
            .await?;
            Ok(true)
        }
        Err(error_code) => {
            deferred_auth.auth_req_packet = None;
            deferred_auth.player_id = None;
            session.player_id = None;
            player_connection_tracker.clear();
            write_message(
                client_stream,
                MessageType::AuthRes,
                packet.header.seq,
                &AuthRes {
                    ok: false,
                    player_id: String::new(),
                    error_code: error_code.to_string(),
                },
            )
            .await?;
            Ok(false)
        }
    }
}

async fn replay_auth_to_upstream<U: AsyncRead + AsyncWrite + Unpin>(
    upstream: &mut U,
    auth_packet: &[u8],
    expected_player_id: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    upstream.write_all(auth_packet).await?;
    let auth_response = read_packet(upstream, MAX_PROXY_BODY_LEN)
        .await?
        .ok_or_else(|| std::io::Error::other("upstream closed during auth replay"))?;

    if auth_response.message_type() != Some(MessageType::AuthRes) {
        return Err(Box::new(std::io::Error::other(
            "upstream did not return AuthRes during auth replay",
        )));
    }

    let decoded = auth_response
        .decode_body::<AuthRes>("INVALID_AUTH_RES")
        .map_err(std::io::Error::other)?;

    if !decoded.ok {
        return Err(Box::new(std::io::Error::other(format!(
            "upstream auth failed: {}",
            decoded.error_code
        ))));
    }

    if let Some(expected_player_id) = expected_player_id {
        if decoded.player_id != expected_player_id {
            return Err(Box::new(std::io::Error::other(
                "upstream auth returned mismatched player id",
            )));
        }
    }

    Ok(())
}

async fn select_route_for_packet(
    route_store: &ProxyRouteStore,
    packet: &Packet,
    authenticated_player_id: Option<&str>,
) -> Result<UpstreamRoute, &'static str> {
    let route = match packet.message_type() {
        Some(MessageType::RoomJoinReq) => {
            let request = packet.decode_body::<RoomJoinReq>("INVALID_ROOM_JOIN_BODY")?;
            let room_id = normalize_room_id(&request.room_id);
            route_store.select_upstream_for_room(&room_id).await
        }
        Some(MessageType::RoomJoinAsObserverReq) => {
            let request =
                packet.decode_body::<RoomJoinAsObserverReq>("INVALID_OBSERVER_JOIN_BODY")?;
            route_store.select_upstream_for_room(&request.room_id).await
        }
        Some(MessageType::RoomReconnectReq) => {
            let request = packet.decode_body::<RoomReconnectReq>("INVALID_ROOM_RECONNECT_BODY")?;
            if let Some(authenticated_player_id) = authenticated_player_id {
                if !request.player_id.is_empty() && request.player_id != authenticated_player_id {
                    return Err("PLAYER_ID_MISMATCH");
                }
            }
            route_store
                .select_upstream_for_player(&request.player_id)
                .await
        }
        _ => route_store.select_default_upstream().await,
    };

    route.ok_or("NO_UPSTREAM_AVAILABLE")
}

async fn update_routing_metadata(
    route_store: &ProxyRouteStore,
    request: &Packet,
    response: &Packet,
    route: &UpstreamRoute,
    deferred_auth: &DeferredAuthState,
    session: &mut ProxySession,
) {
    match request.message_type() {
        Some(MessageType::RoomJoinReq) => {
            if let Ok(join_response) = response.decode_body::<RoomJoinRes>("INVALID_ROOM_JOIN_RES")
            {
                if join_response.ok {
                    session.room_id = Some(join_response.room_id.clone());
                    route_store
                        .bind_room_owner(
                            &join_response.room_id,
                            &route.server_id,
                            deferred_auth.player_id.as_deref(),
                            false,
                        )
                        .await;
                }
            }
        }
        Some(MessageType::RoomReconnectReq) => {
            if let Ok(reconnect_request) =
                request.decode_body::<RoomReconnectReq>("INVALID_ROOM_RECONNECT_BODY")
            {
                if let Ok(reconnect_response) =
                    response.decode_body::<RoomReconnectRes>("INVALID_ROOM_RECONNECT_RES")
                {
                    if reconnect_response.ok {
                        session.room_id = Some(reconnect_response.room_id.clone());
                        route_store
                            .bind_room_owner(
                                &reconnect_response.room_id,
                                &route.server_id,
                                Some(&reconnect_request.player_id),
                                false,
                            )
                            .await;
                        log_rollout_redirect_reconnect(
                            route_store,
                            route,
                            session,
                            &reconnect_request.player_id,
                            &reconnect_response.room_id,
                        )
                        .await;
                    }
                }
            }
        }
        Some(MessageType::RoomJoinAsObserverReq) => {
            if let Ok(observer_response) =
                response.decode_body::<RoomJoinAsObserverRes>("INVALID_OBSERVER_JOIN_RES")
            {
                if observer_response.ok {
                    session.room_id = Some(observer_response.room_id.clone());
                    route_store
                        .bind_room_owner(
                            &observer_response.room_id,
                            &route.server_id,
                            deferred_auth.player_id.as_deref(),
                            true,
                        )
                        .await;
                }
            }
        }
        _ => {}
    }
}

async fn log_rollout_redirect_reconnect(
    route_store: &ProxyRouteStore,
    route: &UpstreamRoute,
    session: &ProxySession,
    player_id: &str,
    room_id: &str,
) {
    let Some(rollout_session) = route_store.get_rollout_session().await else {
        return;
    };

    if route.server_id != rollout_session.new_server_id {
        return;
    }

    info!(
        session_id = session.id,
        player_id = player_id,
        room_id = room_id,
        upstream_server_id = %route.server_id,
        rollout_epoch = %rollout_session.rollout_epoch,
        old_server_id = %rollout_session.old_server_id,
        new_server_id = %rollout_session.new_server_id,
        "player reconnected after redirect"
    );
}

async fn write_message<W, M>(
    writer: &mut W,
    message_type: MessageType,
    seq: u32,
    message: &M,
) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
    M: prost::Message,
{
    let body = encode_body(message);
    writer
        .write_all(&encode_packet(message_type, seq, &body))
        .await
}

async fn proxy_bound_streams<S>(
    client_stream: S,
    upstream: LocalSocketStream,
    msg_rate_limiter: MsgRateLimiter,
    msg_rate_config: MsgRateLimitConfig,
    session_id: u64,
    client_addr: std::net::SocketAddr,
    player_id: String,
) -> Result<(u64, u64), std::io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut client_reader, client_writer) = tokio::io::split(client_stream);
    let client_writer = Arc::new(Mutex::new(client_writer));
    let (mut upstream_reader, mut upstream_writer) = tokio::io::split(upstream);

    let bytes_from_client = Arc::new(AtomicU64::new(0));
    let bytes_from_upstream = Arc::new(AtomicU64::new(0));

    let client_writer_for_rate_limit = Arc::clone(&client_writer);
    let client_bytes = Arc::clone(&bytes_from_client);
    let mut msg_rate_limiter = msg_rate_limiter;
    let player_id_for_rate_limit = player_id.clone();
    let mut client_to_upstream = tokio::spawn(async move {
        loop {
            let Some(packet) = read_packet(&mut client_reader, MAX_PROXY_BODY_LEN).await? else {
                upstream_writer.shutdown().await?;
                return Ok::<(), std::io::Error>(());
            };

            if msg_rate_limiter.check(Instant::now()) == MsgRateDecision::Exceeded {
                {
                    let mut writer = client_writer_for_rate_limit.lock().await;
                    write_msg_rate_exceeded(&mut *writer, packet.header.seq).await?;
                }
                warn!(
                    session_id,
                    client_addr = %client_addr,
                    peer = %client_addr,
                    player_id = %player_id_for_rate_limit,
                    msg_type = packet.header.msg_type,
                    window_ms = msg_rate_config.window.as_millis() as u64,
                    max = msg_rate_config.max,
                    "proxy inbound message rate exceeded during upstream forwarding"
                );
                continue;
            }

            let bytes = packet.to_bytes();
            upstream_writer.write_all(&bytes).await?;
            client_bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }
    });

    let upstream_bytes = Arc::clone(&bytes_from_upstream);
    let client_writer_for_upstream = Arc::clone(&client_writer);
    let mut upstream_to_client = tokio::spawn(async move {
        loop {
            let Some(packet) = read_packet(&mut upstream_reader, MAX_PROXY_BODY_LEN).await? else {
                return Ok::<(), std::io::Error>(());
            };

            let bytes = packet.to_bytes();
            {
                let mut writer = client_writer_for_upstream.lock().await;
                writer.write_all(&bytes).await?;
            }
            upstream_bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        }
    });

    let result = tokio::select! {
        client_result = &mut client_to_upstream => {
            match flatten_forward_task_result(client_result) {
                Ok(()) => flatten_forward_task_result(upstream_to_client.await),
                Err(error) => {
                    upstream_to_client.abort();
                    let _ = upstream_to_client.await;
                    Err(error)
                }
            }
        }
        upstream_result = &mut upstream_to_client => {
            client_to_upstream.abort();
            let _ = client_to_upstream.await;
            flatten_forward_task_result(upstream_result)
        }
    };

    result?;
    Ok((
        bytes_from_client.load(Ordering::Relaxed),
        bytes_from_upstream.load(Ordering::Relaxed),
    ))
}

fn flatten_forward_task_result(
    result: Result<Result<(), std::io::Error>, tokio::task::JoinError>,
) -> Result<(), std::io::Error> {
    match result {
        Ok(inner) => inner,
        Err(error) => Err(std::io::Error::other(format!(
            "proxy forwarding task failed: {}",
            error
        ))),
    }
}

async fn write_msg_rate_exceeded<W: AsyncWrite + Unpin>(
    writer: &mut W,
    seq: u32,
) -> Result<(), std::io::Error> {
    write_proxy_error(writer, seq, MSG_RATE_EXCEEDED, MSG_RATE_EXCEEDED).await
}

async fn write_proxy_error<W: AsyncWrite + Unpin>(
    writer: &mut W,
    seq: u32,
    error_code: &str,
    message: &str,
) -> Result<(), std::io::Error> {
    write_message(
        writer,
        MessageType::ErrorRes,
        seq,
        &ErrorRes {
            error_code: error_code.to_string(),
            message: message.to_string(),
        },
    )
    .await
}

fn normalize_room_id(room_id: &str) -> String {
    if room_id.trim().is_empty() {
        "room-default".to_string()
    } else {
        room_id.to_string()
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
    use super::{
        MsgRateDecision, MsgRateLimitConfig, MsgRateLimiter, PREAUTH_MESSAGE_NOT_ALLOWED,
        PreauthDecision, preauth_decision, restore_authenticated_after_local_routing_error,
    };
    use crate::protocol::{MessageType, Packet, PacketHeader};
    use crate::session::{ProxySession, ProxySessionState};
    use std::time::{Duration, Instant};

    fn packet(msg_type: u16) -> Packet {
        Packet::new(
            PacketHeader {
                msg_type,
                seq: 1,
                body_len: 0,
            },
            Vec::new(),
        )
    }

    #[test]
    fn rejects_room_join_before_authentication() {
        let session = ProxySession::new(1);
        let packet = packet(MessageType::RoomJoinReq as u16);

        assert_eq!(
            preauth_decision(&session, &packet),
            PreauthDecision::Reject(PREAUTH_MESSAGE_NOT_ALLOWED)
        );
    }

    #[test]
    fn rejects_unknown_message_before_authentication() {
        let session = ProxySession::new(1);
        let packet = packet(65535);

        assert_eq!(
            preauth_decision(&session, &packet),
            PreauthDecision::Reject(PREAUTH_MESSAGE_NOT_ALLOWED)
        );
    }

    #[test]
    fn auth_failure_keeps_business_messages_in_preauth_reject_state() {
        let mut session = ProxySession::new(1);
        session.state = ProxySessionState::Connected;
        session.player_id = None;
        let packet = packet(MessageType::RoomReconnectReq as u16);

        assert_eq!(
            preauth_decision(&session, &packet),
            PreauthDecision::Reject(PREAUTH_MESSAGE_NOT_ALLOWED)
        );
    }

    #[test]
    fn authenticated_session_allows_business_message_to_reach_routing() {
        let mut session = ProxySession::new(1);
        session.state = ProxySessionState::Authenticated;
        session.player_id = Some("player-1".to_string());
        let packet = packet(MessageType::RoomJoinReq as u16);

        assert_eq!(
            preauth_decision(&session, &packet),
            PreauthDecision::AllowUpstreamSelection
        );
    }

    #[test]
    fn preauth_allows_auth_and_ping_for_local_handling() {
        let session = ProxySession::new(1);

        assert_eq!(
            preauth_decision(&session, &packet(MessageType::AuthReq as u16)),
            PreauthDecision::HandleLocally
        );
        assert_eq!(
            preauth_decision(&session, &packet(MessageType::PingReq as u16)),
            PreauthDecision::HandleLocally
        );
    }

    #[test]
    fn routing_error_keeps_authenticated_session_authenticated() {
        let mut session = ProxySession::new(1);
        session.state = ProxySessionState::SelectingUpstream;
        session.player_id = Some("player-1".to_string());

        restore_authenticated_after_local_routing_error(&mut session);

        assert_eq!(session.state, ProxySessionState::Authenticated);
    }

    #[test]
    fn msg_rate_limiter_is_disabled_when_max_is_zero() {
        let now = Instant::now();
        let mut limiter = MsgRateLimiter::new(MsgRateLimitConfig::new(1000, 0), now);

        for _ in 0..10 {
            assert_eq!(limiter.check(now), MsgRateDecision::Allowed);
        }
    }

    #[test]
    fn msg_rate_limiter_rejects_after_window_quota() {
        let now = Instant::now();
        let mut limiter = MsgRateLimiter::new(MsgRateLimitConfig::new(1000, 2), now);

        assert_eq!(limiter.check(now), MsgRateDecision::Allowed);
        assert_eq!(limiter.check(now), MsgRateDecision::Allowed);
        assert_eq!(limiter.check(now), MsgRateDecision::Exceeded);
    }

    #[test]
    fn msg_rate_limiter_resets_after_window() {
        let now = Instant::now();
        let mut limiter = MsgRateLimiter::new(MsgRateLimitConfig::new(1000, 1), now);

        assert_eq!(limiter.check(now), MsgRateDecision::Allowed);
        assert_eq!(limiter.check(now), MsgRateDecision::Exceeded);
        assert_eq!(
            limiter.check(now + Duration::from_millis(1000)),
            MsgRateDecision::Allowed
        );
    }

    #[test]
    fn msg_rate_limit_can_reject_before_preauth_decision() {
        let now = Instant::now();
        let mut limiter = MsgRateLimiter::new(MsgRateLimitConfig::new(1000, 0), now);
        let session = ProxySession::new(1);
        let business_packet = packet(MessageType::RoomJoinReq as u16);

        assert_eq!(limiter.check(now), MsgRateDecision::Allowed);
        assert_eq!(
            preauth_decision(&session, &business_packet),
            PreauthDecision::Reject(PREAUTH_MESSAGE_NOT_ALLOWED)
        );

        let mut limiter = MsgRateLimiter::new(MsgRateLimitConfig::new(1000, 1), now);
        assert_eq!(limiter.check(now), MsgRateDecision::Allowed);
        assert_eq!(limiter.check(now), MsgRateDecision::Exceeded);
    }
}
