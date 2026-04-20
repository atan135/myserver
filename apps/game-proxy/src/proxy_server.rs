use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf, copy_bidirectional};
use tokio::net::TcpStream as TokioTcpStream;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::auth::ProxyAuthService;
use crate::config::Config;
use crate::metrics::METRICS;
use crate::pb::{
    AuthReq, AuthRes, ErrorRes, PingRes, RoomJoinAsObserverReq, RoomJoinAsObserverRes,
    RoomJoinReq, RoomJoinRes, RoomReconnectReq, RoomReconnectRes,
};
use crate::protocol::{MessageType, Packet, encode_body, encode_packet, read_packet};
use crate::route_store::{
    ProxyRouteStore, UpstreamHealthState, UpstreamOperationState, UpstreamRoute,
};
use crate::session::{ProxySession, ProxySessionState};
use crate::upstream::connect_upstream;
use service_registry::RegistryClient;

const MAX_PROXY_BODY_LEN: usize = 1024 * 1024;

pub type SharedConnectionCount = Arc<AtomicU64>;
pub type SharedMaintenanceFlag = Arc<RwLock<bool>>;

#[derive(Default)]
struct DeferredAuthState {
    auth_req_packet: Option<Vec<u8>>,
    player_id: Option<String>,
}

enum ProxyStream {
    Kcp(tokio_kcp::KcpStream),
    Tcp(TokioTcpStream),
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
            if let Err(error) =
                run_upstream_discovery(registry_url, service_name, discover_interval, route_store_clone).await
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

    loop {
        tokio::select! {
            kcp_result = kcp_frontend.accept() => {
                match kcp_result {
                    Ok((client_stream, client_addr)) => {
                        let route_store = route_store.clone();
                        let auth_service = auth_service.clone();
                        let connection_count = connection_count.clone();
                        let maintenance = maintenance.clone();
                        let session_id = next_session_id;
                        next_session_id = next_session_id.saturating_add(1);

                        tokio::spawn(async move {
                            let stream = ProxyStream::Kcp(client_stream);
                            if let Err(error) = handle_session(
                                session_id,
                                client_addr,
                                stream,
                                route_store,
                                auth_service,
                                connection_count,
                                maintenance,
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
                        let connection_count = connection_count.clone();
                        let maintenance = maintenance.clone();
                        let session_id = next_session_id;
                        next_session_id = next_session_id.saturating_add(1);

                        tokio::spawn(async move {
                            let stream = ProxyStream::Tcp(client_stream);
                            if let Err(error) = handle_session(
                                session_id,
                                client_addr,
                                stream,
                                route_store,
                                auth_service,
                                connection_count,
                                maintenance,
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

    let mut ticker = tokio::time::interval(tokio::time::Duration::from_secs(discover_interval_secs));
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
    route_store: ProxyRouteStore,
    auth_service: Arc<ProxyAuthService>,
    connection_count: SharedConnectionCount,
    maintenance: SharedMaintenanceFlag,
) -> Result<(), Box<dyn std::error::Error>> {
    if *maintenance.read().await {
        return Err(Box::new(std::io::Error::other("proxy is in maintenance")));
    }

    let mut session = ProxySession::new(session_id);
    let mut deferred_auth = DeferredAuthState::default();

    loop {
        let Some(packet) = read_packet(&mut client_stream, MAX_PROXY_BODY_LEN).await? else {
            session.state = ProxySessionState::Closed;
            info!(session_id = session.id, client_addr = %client_addr, "proxy session closed before upstream bind");
            return Ok(());
        };

        match packet.message_type() {
            Some(MessageType::AuthReq) => {
                session.state = ProxySessionState::Authenticating;
                handle_local_auth(
                    &mut client_stream,
                    &packet,
                    &auth_service,
                    &mut deferred_auth,
                    &mut session,
                )
                .await?;
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
                let connections = connection_count.fetch_add(1, Ordering::Relaxed) + 1;
                METRICS.set_connections(connections);

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
                        .ok_or_else(|| std::io::Error::other("upstream closed before first response"))?;

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
                        let previous = connection_count.fetch_sub(1, Ordering::Relaxed);
                        METRICS.set_connections(previous.saturating_sub(1));
                        session.state = ProxySessionState::Closed;
                        return Err(Box::new(error));
                    }
                }

                let result = copy_bidirectional(&mut client_stream, &mut upstream).await;
                let previous = connection_count.fetch_sub(1, Ordering::Relaxed);
                METRICS.set_connections(previous.saturating_sub(1));
                session.state = ProxySessionState::Closed;

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

async fn handle_local_auth<S: AsyncWrite + Unpin>(
    client_stream: &mut S,
    packet: &Packet,
    auth_service: &ProxyAuthService,
    deferred_auth: &mut DeferredAuthState,
    session: &mut ProxySession,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = match packet.decode_body::<AuthReq>("INVALID_AUTH_BODY") {
        Ok(request) => request,
        Err(error_code) => {
            deferred_auth.auth_req_packet = None;
            deferred_auth.player_id = None;
            session.player_id = None;
            write_proxy_error(client_stream, packet.header.seq, error_code, "invalid auth body")
                .await?;
            return Ok(());
        }
    };

    match auth_service.authenticate_ticket(&request.ticket).await {
        Ok(player_id) => {
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
        }
        Err(error_code) => {
            deferred_auth.auth_req_packet = None;
            deferred_auth.player_id = None;
            session.player_id = None;
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
        }
    }

    Ok(())
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
            let request = packet
                .decode_body::<RoomJoinReq>("INVALID_ROOM_JOIN_BODY")?;
            let room_id = normalize_room_id(&request.room_id);
            route_store.select_upstream_for_room(&room_id).await
        }
        Some(MessageType::RoomJoinAsObserverReq) => {
            let request = packet
                .decode_body::<RoomJoinAsObserverReq>("INVALID_OBSERVER_JOIN_BODY")?;
            route_store.select_upstream_for_room(&request.room_id).await
        }
        Some(MessageType::RoomReconnectReq) => {
            let request = packet
                .decode_body::<RoomReconnectReq>("INVALID_ROOM_RECONNECT_BODY")?;
            if let Some(authenticated_player_id) = authenticated_player_id {
                if !request.player_id.is_empty() && request.player_id != authenticated_player_id {
                    return Err("PLAYER_ID_MISMATCH");
                }
            }
            route_store.select_upstream_for_player(&request.player_id).await
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
            if let Ok(join_response) = response.decode_body::<RoomJoinRes>("INVALID_ROOM_JOIN_RES") {
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
    writer.write_all(&encode_packet(message_type, seq, &body)).await
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
