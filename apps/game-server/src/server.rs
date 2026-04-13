use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use interprocess::local_socket::traits::tokio::Listener as _;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{RwLock, mpsc};
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use crate::config::Config;
use crate::config_table::ConfigTableRuntime;
use crate::core::context::{ConnectionContext, ServerSharedState, ServiceContext};
use crate::core::room::OutboundMessage;
use crate::core::room_manager::RoomManager;
use crate::core::service::{core_service, room_service};
use crate::gameservice::test_service;
use crate::mysql_store::MySqlAuditStore;
use crate::protocol::{HEADER_LEN, MessageType, Packet, encode_packet, parse_header};
use crate::session::Session;

#[derive(Clone, Copy, Debug)]
pub struct RuntimeConfig {
    pub heartbeat_timeout_secs: u64,
    pub max_body_len: usize,
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
    let redis_client = redis::Client::open(config.redis_url.clone())?;
    let shared_state = ServerSharedState {
        room_manager: Arc::new(RoomManager::new()),
        runtime_config: Arc::new(RwLock::new(RuntimeConfig {
            heartbeat_timeout_secs: config.heartbeat_timeout_secs,
            max_body_len: config.max_body_len,
        })),
        connection_count: Arc::new(AtomicU64::new(0)),
    };
    let services = ServiceContext {
        config: config.clone(),
        mysql_store: mysql_store.clone(),
        room_manager: shared_state.room_manager.clone(),
        config_tables,
    };
    info!(
        addr = %config.bind_addr(),
        admin_addr = %config.admin_bind_addr(),
        local_socket_name = %config.local_socket_name,
        redis = %config.redis_url,
        mysql_enabled = mysql_store.enabled(),
        "game server listening"
    );

    let admin_task = tokio::spawn(crate::admin_server::run_listener(
        admin_listener,
        shared_state.room_manager.clone(),
        shared_state.runtime_config.clone(),
        shared_state.connection_count.clone(),
    ));

    let local_socket_task = tokio::spawn(run_local_socket_listener(
        local_socket_listener,
        redis_client.clone(),
        services.clone(),
        shared_state.runtime_config.clone(),
        shared_state.connection_count.clone(),
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

    info!("game server shutdown completed");
    Ok(())
}

async fn run_local_socket_listener(
    mut listener: interprocess::local_socket::tokio::Listener,
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
    let (tx, mut rx) = mpsc::unbounded_channel::<OutboundMessage>();
    let mut connection = ConnectionContext {
        peer_addr,
        redis,
        session: Session::new(session_id),
        tx,
    };

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
        let read_header = timeout(
            Duration::from_secs(runtime.heartbeat_timeout_secs),
            reader.read_exact(&mut header_buf),
        )
        .await;

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
                connection.queue_error(0, "HEARTBEAT_TIMEOUT", "connection timed out")?;
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
                connection.queue_error(0, error_code, "invalid header")?;
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
            connection.queue_error(header.seq, "BODY_TOO_LARGE", "body too large")?;
            discard_body(&mut reader, header.body_len as usize).await?;
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
        reader.read_exact(&mut body).await?;
        let packet = Packet::new(header, body);

        dispatch_packet(&services, &mut connection, &packet).await?;
    }

    room_service::handle_disconnect_cleanup(&services, &connection).await;

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
    match packet.message_type() {
        Some(MessageType::AuthReq) => core_service::handle_auth(services, connection, packet).await,
        Some(MessageType::PingReq) => core_service::handle_ping(connection, packet)
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>),
        Some(MessageType::GetRoomDataReq) => {
            test_service::handle_get_room_data(services, connection, packet).await
        }
        Some(MessageType::RoomJoinReq) => room_service::handle_room_join(services, connection, packet).await,
        Some(MessageType::RoomLeaveReq) => room_service::handle_room_leave(services, connection, packet).await,
        Some(MessageType::RoomReadyReq) => room_service::handle_room_ready(services, connection, packet).await,
        Some(MessageType::RoomStartReq) => room_service::handle_room_start(services, connection, packet).await,
        Some(MessageType::PlayerInputReq) => {
            room_service::handle_player_input(services, connection, packet).await
        }
        Some(MessageType::RoomEndReq) => room_service::handle_room_end(services, connection, packet).await,
        Some(MessageType::RoomReconnectReq) => room_service::handle_room_reconnect(services, connection, packet).await,
        Some(MessageType::RoomJoinAsObserverReq) => room_service::handle_join_as_observer(services, connection, packet).await,
        Some(_) => {
            connection.queue_error(
                packet.header.seq,
                "MESSAGE_NOT_SUPPORTED",
                "message not supported in this phase",
            )?;
            Ok(())
        }
        None => {
            connection.queue_error(packet.header.seq, "UNKNOWN_MESSAGE_TYPE", "unknown message type")?;
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



