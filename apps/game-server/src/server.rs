use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use redis::AsyncCommands;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{RwLock, mpsc};
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use crate::config::Config;
use crate::config_table::ConfigTableRuntime;
use crate::mysql_store::MySqlAuditStore;
use crate::pb::{
    AuthReq, AuthRes, ErrorRes, GetRoomDataReq, GetRoomDataRes, PingRes, PlayerInputReq,
    PlayerInputRes, RoomEndReq, RoomEndRes, RoomJoinReq, RoomJoinRes, RoomLeaveRes,
    RoomReadyReq, RoomReadyRes, RoomStartRes,
};
use crate::protocol::{HEADER_LEN, MessageType, Packet, encode_body, encode_packet, parse_header};
use crate::room::OutboundMessage;
use crate::room_manager::RoomManager;
use crate::session::{Session, SessionState};
use crate::ticket::verify_ticket;

pub type SharedRoomManager = Arc<RoomManager>;
pub type SharedRuntimeConfig = Arc<RwLock<RuntimeConfig>>;

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
    let listener = TcpListener::bind(config.bind_addr()).await?;
    let admin_listener = TcpListener::bind(config.admin_bind_addr()).await?;
    let redis_client = redis::Client::open(config.redis_url.clone())?;
    let room_manager: SharedRoomManager = Arc::new(RoomManager::new());
    let runtime_config: SharedRuntimeConfig = Arc::new(RwLock::new(RuntimeConfig {
        heartbeat_timeout_secs: config.heartbeat_timeout_secs,
        max_body_len: config.max_body_len,
    }));
    let connection_count = Arc::new(AtomicU64::new(0));
    info!(
        addr = %config.bind_addr(),
        admin_addr = %config.admin_bind_addr(),
        redis = %config.redis_url,
        mysql_enabled = mysql_store.enabled(),
        "game server listening"
    );

    let admin_task = tokio::spawn(crate::admin_server::run_listener(
        admin_listener,
        room_manager.clone(),
        runtime_config.clone(),
        connection_count.clone(),
    ));

    let mut next_session_id: u64 = 1;

    loop {
        let accept_result = tokio::select! {
            result = listener.accept() => Some(result),
            _ = tokio::signal::ctrl_c() => None,
        };

        let Some((socket, peer_addr)) = accept_result.transpose()? else {
            info!("shutdown signal received, stopping game server accept loop");
            break;
        };

        let session_id = next_session_id;
        next_session_id += 1;

        connection_count.fetch_add(1, Ordering::Relaxed);
        info!(session_id = session_id, peer = %peer_addr, "accepted tcp connection");
        mysql_store
            .append_connection_event(
                session_id,
                None,
                Some(&peer_addr.to_string()),
                "tcp_connected",
                None,
            )
            .await;

        let connection_config = config.clone();
        let redis_client = redis_client.clone();
        let room_manager = room_manager.clone();
        let mysql_store = mysql_store.clone();
        let runtime_config = runtime_config.clone();
        let config_tables = config_tables.clone();
        let connection_count = connection_count.clone();
        tokio::spawn(async move {
            let _connection_guard = ConnectionCountGuard { connection_count };
            if let Err(error) = handle_connection(
                socket,
                peer_addr.to_string(),
                session_id,
                &connection_config,
                redis_client,
                mysql_store,
                room_manager,
                runtime_config,
                config_tables,
            )
            .await
            {
                warn!(session_id = session_id, error = %error, "connection task failed");
            }
        });
    }

    admin_task.abort();
    let _ = admin_task.await;

    info!("game server shutdown completed");
    Ok(())
}

async fn handle_connection(
    socket: TcpStream,
    peer_addr: String,
    session_id: u64,
    config: &Config,
    redis_client: redis::Client,
    mysql_store: MySqlAuditStore,
    room_manager: SharedRoomManager,
    runtime_config: SharedRuntimeConfig,
    config_tables: ConfigTableRuntime,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut session = Session::new(session_id);
    let mut redis = redis_client.get_multiplexed_async_connection().await?;
    let (mut reader, mut writer) = socket.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<OutboundMessage>();

    let writer_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let packet = encode_packet(message.message_type, message.seq, &message.body);
            if let Err(error) = writer.write_all(&packet).await {
                return Err(error);
            }
        }

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
                info!(session_id = session.id, "peer closed connection");
                mysql_store
                    .append_connection_event(
                        session.id,
                        session.player_id.as_deref(),
                        Some(&peer_addr),
                        "tcp_closed",
                        None,
                    )
                    .await;
                break;
            }
            Ok(Err(error)) => return Err(Box::new(error)),
            Err(_) => {
                queue_error(&tx, 0, "HEARTBEAT_TIMEOUT", "connection timed out")?;
                mysql_store
                    .append_connection_event(
                        session.id,
                        session.player_id.as_deref(),
                        Some(&peer_addr),
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
                queue_error(&tx, 0, error_code, "invalid header")?;
                mysql_store
                    .append_connection_event(
                        session.id,
                        session.player_id.as_deref(),
                        Some(&peer_addr),
                        "invalid_header",
                        Some(json!({ "errorCode": error_code })),
                    )
                    .await;
                break;
            }
        };

        if header.body_len as usize > runtime.max_body_len {
            queue_error(&tx, header.seq, "BODY_TOO_LARGE", "body too large")?;
            mysql_store
                .append_connection_event(
                    session.id,
                    session.player_id.as_deref(),
                    Some(&peer_addr),
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

        let Some(message_type) = packet.message_type() else {
            queue_error(&tx, header.seq, "UNKNOWN_MESSAGE_TYPE", "unknown message type")?;
            mysql_store
                .append_connection_event(
                    session.id,
                    session.player_id.as_deref(),
                    Some(&peer_addr),
                    "unknown_message_type",
                    Some(json!({ "msgType": packet.header.msg_type, "seq": packet.header.seq })),
                )
                .await;
            continue;
        };

        match message_type {
            MessageType::AuthReq => {
                let request = match packet.decode_body::<AuthReq>("INVALID_AUTH_BODY") {
                    Ok(value) => value,
                    Err(error_code) => {
                        queue_error(&tx, header.seq, error_code, "invalid auth body")?;
                        mysql_store
                            .append_connection_event(
                                session.id,
                                session.player_id.as_deref(),
                                Some(&peer_addr),
                                "invalid_auth_body",
                                Some(json!({ "seq": header.seq })),
                            )
                            .await;
                        continue;
                    }
                };

                match verify_ticket(&config.ticket_secret, &request.ticket) {
                    Ok(player_id) => {
                        let ticket_key = format!(
                            "{}ticket:{}",
                            config.redis_key_prefix,
                            crate::ticket::hash_ticket(&request.ticket)
                        );
                        let ticket_owner: Option<String> = redis.get(ticket_key).await?;

                        if ticket_owner.as_deref() != Some(player_id.as_str()) {
                            queue_message(
                                &tx,
                                MessageType::AuthRes,
                                header.seq,
                                AuthRes {
                                    ok: false,
                                    player_id: String::new(),
                                    error_code: "TICKET_NOT_FOUND".to_string(),
                                },
                            )?;
                            mysql_store
                                .append_connection_event(
                                    session.id,
                                    Some(&player_id),
                                    Some(&peer_addr),
                                    "auth_ticket_not_found",
                                    Some(json!({ "seq": header.seq })),
                                )
                                .await;
                            continue;
                        }

                        session.state = SessionState::Authenticated;
                        session.player_id = Some(player_id.clone());

                        queue_message(
                            &tx,
                            MessageType::AuthRes,
                            header.seq,
                            AuthRes {
                                ok: true,
                                player_id: player_id.clone(),
                                error_code: String::new(),
                            },
                        )?;
                        mysql_store
                            .append_connection_event(
                                session.id,
                                Some(&player_id),
                                Some(&peer_addr),
                                "auth_success",
                                Some(json!({ "seq": header.seq })),
                            )
                            .await;
                    }
                    Err(error_code) => {
                        queue_message(
                            &tx,
                            MessageType::AuthRes,
                            header.seq,
                            AuthRes {
                                ok: false,
                                player_id: String::new(),
                                error_code: error_code.to_string(),
                            },
                        )?;
                        mysql_store
                            .append_connection_event(
                                session.id,
                                None,
                                Some(&peer_addr),
                                "auth_failed",
                                Some(json!({
                                    "seq": header.seq,
                                    "errorCode": error_code
                                })),
                            )
                            .await;
                    }
                }
            }
            MessageType::PingReq => {
                queue_message(
                    &tx,
                    MessageType::PingRes,
                    header.seq,
                    PingRes {
                        server_time: current_unix_ms(),
                    },
                )?;
            }
            MessageType::GetRoomDataReq => {
                let Some(_player_id) = ensure_authenticated(&session, &tx, header.seq)? else {
                    continue;
                };

                let request =
                    match packet.decode_body::<GetRoomDataReq>("INVALID_GET_ROOM_DATA_BODY") {
                        Ok(value) => value,
                        Err(error_code) => {
                            queue_error(
                                &tx,
                                header.seq,
                                error_code,
                                "invalid get room data body",
                            )?;
                            continue;
                        }
                    };

                if request.id_start > request.id_end {
                    queue_message(
                        &tx,
                        MessageType::GetRoomDataRes,
                        header.seq,
                        GetRoomDataRes {
                            ok: false,
                            field_0_list: Vec::new(),
                            error_code: "INVALID_ID_RANGE".to_string(),
                        },
                    )?;
                    continue;
                }

                let tables = config_tables.snapshot().await;
                let table = &tables.testtable_100;
                let mut field_0_list = Vec::new();

                for id in request.id_start..=request.id_end {
                    if let Some(row) = table.get(id) {
                        for key in &row.field_0 {
                            field_0_list
                                .push(table.resolve_string(*key).unwrap_or_default().to_string());
                        }
                    }
                }

                if field_0_list.is_empty() {
                    queue_message(
                        &tx,
                        MessageType::GetRoomDataRes,
                        header.seq,
                        GetRoomDataRes {
                            ok: false,
                            field_0_list,
                            error_code: "CONFIG_NOT_FOUND".to_string(),
                        },
                    )?;
                } else {
                    queue_message(
                        &tx,
                        MessageType::GetRoomDataRes,
                        header.seq,
                        GetRoomDataRes {
                            ok: true,
                            field_0_list,
                            error_code: String::new(),
                        },
                    )?;
                }
            }
            MessageType::RoomJoinReq => {
                let Some(player_id) = ensure_authenticated(&session, &tx, header.seq)? else {
                    continue;
                };

                let request = match packet.decode_body::<RoomJoinReq>("INVALID_ROOM_JOIN_BODY") {
                    Ok(value) => value,
                    Err(error_code) => {
                        queue_error(&tx, header.seq, error_code, "invalid room join body")?;
                        continue;
                    }
                };

                let room_id = if request.room_id.is_empty() {
                    "room-default".to_string()
                } else {
                    request.room_id
                };

                if let Some(current_room_id) = &session.room_id {
                    if current_room_id != &room_id {
                        queue_message(
                            &tx,
                            MessageType::RoomJoinRes,
                            header.seq,
                            RoomJoinRes {
                                ok: false,
                                room_id: current_room_id.clone(),
                                error_code: "ALREADY_IN_OTHER_ROOM".to_string(),
                            },
                        )?;
                        continue;
                    }

                    queue_message(
                        &tx,
                        MessageType::RoomJoinRes,
                        header.seq,
                        RoomJoinRes {
                            ok: true,
                            room_id: room_id.clone(),
                            error_code: String::new(),
                        },
                    )?;
                    continue;
                }

                let join_result = room_manager.join_room(&room_id, &player_id, tx.clone()).await;

                match join_result {
                    Ok(snapshot) => {
                        session.room_id = Some(room_id.clone());
                        queue_message(
                            &tx,
                            MessageType::RoomJoinRes,
                            header.seq,
                            RoomJoinRes {
                                ok: true,
                                room_id: room_id.clone(),
                                error_code: String::new(),
                            },
                        )?;
                        mysql_store
                            .append_room_event(
                                &room_id,
                                Some(&player_id),
                                Some(&snapshot.owner_player_id),
                                "room_joined",
                                Some(&snapshot.state),
                                snapshot.members.len(),
                                Some(json!({
                                    "seq": header.seq,
                                    "members": snapshot.members.iter().map(|member| json!({
                                        "playerId": member.player_id,
                                        "ready": member.ready,
                                        "isOwner": member.is_owner
                                    })).collect::<Vec<_>>()
                                })),
                            )
                            .await;
                        room_manager
                            .broadcast_snapshot(&room_id, "member_joined", snapshot)
                            .await?;
                    }
                    Err(error_code) => {
                        queue_message(
                            &tx,
                            MessageType::RoomJoinRes,
                            header.seq,
                            RoomJoinRes {
                                ok: false,
                                room_id: room_id.clone(),
                                error_code: error_code.to_string(),
                            },
                        )?;
                        mysql_store
                            .append_room_event(
                                &room_id,
                                Some(&player_id),
                                None,
                                "room_join_failed",
                                None,
                                0,
                                Some(json!({ "errorCode": error_code, "seq": header.seq })),
                            )
                            .await;
                    }
                }
            }
            MessageType::RoomLeaveReq => {
                let Some(room_id) = session.room_id.clone() else {
                    queue_message(
                        &tx,
                        MessageType::RoomLeaveRes,
                        header.seq,
                        RoomLeaveRes {
                            ok: false,
                            room_id: String::new(),
                            error_code: "ROOM_NOT_JOINED".to_string(),
                        },
                    )?;
                    continue;
                };

                let Some(player_id) = session.player_id.clone() else {
                    queue_error(&tx, header.seq, "NOT_AUTHENTICATED", "authenticate before leaving a room")?;
                    continue;
                };

                let leave_result = room_manager.leave_room(&room_id, &player_id).await;
                session.room_id = None;

                queue_message(
                    &tx,
                    MessageType::RoomLeaveRes,
                    header.seq,
                    RoomLeaveRes {
                        ok: true,
                        room_id: room_id.clone(),
                        error_code: String::new(),
                    },
                )?;

                if let Some(snapshot) = leave_result.snapshot {
                    mysql_store
                        .append_room_event(
                            &room_id,
                            Some(&player_id),
                            Some(&snapshot.owner_player_id),
                            "room_left",
                            Some(&snapshot.state),
                            snapshot.members.len(),
                            None,
                        )
                        .await;
                    room_manager
                        .broadcast_snapshot(&room_id, "member_left", snapshot)
                        .await?;
                } else if leave_result.room_removed {
                    mysql_store
                        .append_room_event(
                            &room_id,
                            Some(&player_id),
                            None,
                            "room_disbanded",
                            None,
                            0,
                            None,
                        )
                        .await;
                }
            }
            MessageType::RoomReadyReq => {
                let Some(player_id) = ensure_authenticated(&session, &tx, header.seq)? else {
                    continue;
                };
                let Some(room_id) = session.room_id.clone() else {
                    queue_message(
                        &tx,
                        MessageType::RoomReadyRes,
                        header.seq,
                        RoomReadyRes {
                            ok: false,
                            room_id: String::new(),
                            ready: false,
                            error_code: "ROOM_NOT_JOINED".to_string(),
                        },
                    )?;
                    continue;
                };

                let request = match packet.decode_body::<RoomReadyReq>("INVALID_ROOM_READY_BODY") {
                    Ok(value) => value,
                    Err(error_code) => {
                        queue_error(&tx, header.seq, error_code, "invalid room ready body")?;
                        continue;
                    }
                };

                let ready_result = room_manager
                    .set_ready_state(&room_id, &player_id, request.ready)
                    .await;

                match ready_result {
                    Ok(snapshot) => {
                        queue_message(
                            &tx,
                            MessageType::RoomReadyRes,
                            header.seq,
                            RoomReadyRes {
                                ok: true,
                                room_id: room_id.clone(),
                                ready: request.ready,
                                error_code: String::new(),
                            },
                        )?;
                        mysql_store
                            .append_room_event(
                                &room_id,
                                Some(&player_id),
                                Some(&snapshot.owner_player_id),
                                "room_ready_changed",
                                Some(&snapshot.state),
                                snapshot.members.len(),
                                Some(json!({ "ready": request.ready, "seq": header.seq })),
                            )
                            .await;
                        room_manager
                            .broadcast_snapshot(&room_id, "ready_changed", snapshot)
                            .await?;
                    }
                    Err(error_code) => {
                        queue_message(
                            &tx,
                            MessageType::RoomReadyRes,
                            header.seq,
                            RoomReadyRes {
                                ok: false,
                                room_id,
                                ready: request.ready,
                                error_code: error_code.to_string(),
                            },
                        )?;
                    }
                }
            }
            MessageType::RoomStartReq => {
                let Some(player_id) = ensure_authenticated(&session, &tx, header.seq)? else {
                    continue;
                };
                let Some(room_id) = session.room_id.clone() else {
                    queue_message(
                        &tx,
                        MessageType::RoomStartRes,
                        header.seq,
                        RoomStartRes {
                            ok: false,
                            room_id: String::new(),
                            error_code: "ROOM_NOT_JOINED".to_string(),
                        },
                    )?;
                    continue;
                };

                let start_result = room_manager.start_game(&room_id, &player_id).await;

                match start_result {
                    Ok(snapshot) => {
                        queue_message(
                            &tx,
                            MessageType::RoomStartRes,
                            header.seq,
                            RoomStartRes {
                                ok: true,
                                room_id: room_id.clone(),
                                error_code: String::new(),
                            },
                        )?;
                        mysql_store
                            .append_room_event(
                                &room_id,
                                Some(&player_id),
                                Some(&snapshot.owner_player_id),
                                "game_started",
                                Some(&snapshot.state),
                                snapshot.members.len(),
                                Some(json!({ "seq": header.seq })),
                            )
                            .await;
                        room_manager
                            .broadcast_snapshot(&room_id, "game_started", snapshot)
                            .await?;
                    }
                    Err(error_code) => {
                        queue_message(
                            &tx,
                            MessageType::RoomStartRes,
                            header.seq,
                            RoomStartRes {
                                ok: false,
                                room_id,
                                error_code: error_code.to_string(),
                            },
                        )?;
                    }
                }
            }
            MessageType::PlayerInputReq => {
                let Some(player_id) = ensure_authenticated(&session, &tx, header.seq)? else {
                    continue;
                };
                let Some(room_id) = session.room_id.clone() else {
                    queue_message(
                        &tx,
                        MessageType::PlayerInputRes,
                        header.seq,
                        PlayerInputRes {
                            ok: false,
                            room_id: String::new(),
                            error_code: "ROOM_NOT_JOINED".to_string(),
                        },
                    )?;
                    continue;
                };

                let request = match packet.decode_body::<PlayerInputReq>("INVALID_PLAYER_INPUT_BODY") {
                    Ok(value) => value,
                    Err(error_code) => {
                        queue_error(&tx, header.seq, error_code, "invalid player input body")?;
                        continue;
                    }
                };

                let input_result = room_manager
                    .accept_player_input(&room_id, &player_id, request.frame_id, &request.action, &request.payload_json)
                    .await;

                match input_result {
                    Ok(_) => {
                        queue_message(
                            &tx,
                            MessageType::PlayerInputRes,
                            header.seq,
                            PlayerInputRes {
                                ok: true,
                                room_id: room_id.clone(),
                                error_code: String::new(),
                            },
                        )?;
                        mysql_store
                            .append_room_event(
                                &room_id,
                                Some(&player_id),
                                None,
                                "player_input",
                                Some("in_game"),
                                0,
                                Some(json!({
                                    "seq": header.seq,
                                    "action": request.action,
                                    "payloadJson": request.payload_json
                                })),
                            )
                            .await;                    }
                    Err(error_code) => {
                        queue_message(
                            &tx,
                            MessageType::PlayerInputRes,
                            header.seq,
                            PlayerInputRes {
                                ok: false,
                                room_id,
                                error_code: error_code.to_string(),
                            },
                        )?;
                    }
                }
            }
            MessageType::RoomEndReq => {
                let Some(player_id) = ensure_authenticated(&session, &tx, header.seq)? else {
                    continue;
                };
                let Some(room_id) = session.room_id.clone() else {
                    queue_message(
                        &tx,
                        MessageType::RoomEndRes,
                        header.seq,
                        RoomEndRes {
                            ok: false,
                            room_id: String::new(),
                            error_code: "ROOM_NOT_JOINED".to_string(),
                        },
                    )?;
                    continue;
                };

                let request = match packet.decode_body::<RoomEndReq>("INVALID_ROOM_END_BODY") {
                    Ok(value) => value,
                    Err(error_code) => {
                        queue_error(&tx, header.seq, error_code, "invalid room end body")?;
                        continue;
                    }
                };

                let end_result = room_manager.end_game(&room_id, &player_id).await;

                match end_result {
                    Ok(snapshot) => {
                        queue_message(
                            &tx,
                            MessageType::RoomEndRes,
                            header.seq,
                            RoomEndRes {
                                ok: true,
                                room_id: room_id.clone(),
                                error_code: String::new(),
                            },
                        )?;
                        mysql_store
                            .append_room_event(
                                &room_id,
                                Some(&player_id),
                                Some(&snapshot.owner_player_id),
                                "game_ended",
                                Some(&snapshot.state),
                                snapshot.members.len(),
                                Some(json!({
                                    "seq": header.seq,
                                    "reason": request.reason
                                })),
                            )
                            .await;
                        room_manager
                            .broadcast_snapshot(&room_id, "game_ended", snapshot)
                            .await?;
                    }
                    Err(error_code) => {
                        queue_message(
                            &tx,
                            MessageType::RoomEndRes,
                            header.seq,
                            RoomEndRes {
                                ok: false,
                                room_id,
                                error_code: error_code.to_string(),
                            },
                        )?;
                    }
                }
            }
            _ => {
                queue_error(&tx, header.seq, "MESSAGE_NOT_SUPPORTED", "message not supported in this phase")?;
            }
        }
    }

    if let (Some(room_id), Some(player_id)) = (session.room_id.clone(), session.player_id.clone()) {
        let leave_result = room_manager.leave_room(&room_id, &player_id).await;

        if let Some(snapshot) = leave_result.snapshot {
            mysql_store
                .append_room_event(
                    &room_id,
                    Some(&player_id),
                    Some(&snapshot.owner_player_id),
                    "member_disconnected",
                    Some(&snapshot.state),
                    snapshot.members.len(),
                    None,
                )
                .await;
            let _ = room_manager
                .broadcast_snapshot(&room_id, "member_disconnected", snapshot)
                .await;
        }
    }

    drop(tx);
    writer_task.await??;
    Ok(())
}

fn ensure_authenticated(
    session: &Session,
    tx: &mpsc::UnboundedSender<OutboundMessage>,
    seq: u32,
) -> Result<Option<String>, std::io::Error> {
    if session.state != SessionState::Authenticated {
        queue_error(tx, seq, "NOT_AUTHENTICATED", "authenticate first")?;
        return Ok(None);
    }

    Ok(session.player_id.clone())
}

fn queue_error(
    tx: &mpsc::UnboundedSender<OutboundMessage>,
    seq: u32,
    error_code: &str,
    message: &str,
) -> Result<(), std::io::Error> {
    queue_message(
        tx,
        MessageType::ErrorRes,
        seq,
        ErrorRes {
            error_code: error_code.to_string(),
            message: message.to_string(),
        },
    )
}

fn queue_message<M: prost::Message>(
    tx: &mpsc::UnboundedSender<OutboundMessage>,
    message_type: MessageType,
    seq: u32,
    message: M,
) -> Result<(), std::io::Error> {
    let body = encode_body(&message);
    tx.send(OutboundMessage {
        message_type,
        seq,
        body,
    })
    .map_err(|_| std::io::Error::other("failed to queue outbound"))
}

fn current_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default()
}


