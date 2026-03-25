use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use prost::Message;
use redis::AsyncCommands;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use crate::config::Config;
use crate::pb::{
    AuthReq, AuthRes, ErrorRes, PingRes, RoomJoinReq, RoomJoinRes, RoomLeaveRes, RoomReadyReq,
    RoomReadyRes, RoomSnapshot, RoomStatePush,
};
use crate::protocol::{HEADER_LEN, MessageType, encode_packet, parse_header};
use crate::room::{OutboundMessage, Room, RoomMemberState};
use crate::session::{Session, SessionState};
use crate::ticket::verify_ticket;

type SharedRooms = Arc<Mutex<HashMap<String, Room>>>;

pub async fn run(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(config.bind_addr()).await?;
    let redis_client = redis::Client::open(config.redis_url.clone())?;
    let rooms: SharedRooms = Arc::new(Mutex::new(HashMap::new()));
    info!(addr = %config.bind_addr(), redis = %config.redis_url, "game server listening");

    let mut next_session_id: u64 = 1;

    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let session_id = next_session_id;
        next_session_id += 1;

        info!(session_id = session_id, peer = %peer_addr, "accepted tcp connection");

        let connection_config = config.clone();
        let redis_client = redis_client.clone();
        let rooms = rooms.clone();
        tokio::spawn(async move {
            if let Err(error) =
                handle_connection(socket, session_id, &connection_config, redis_client, rooms).await
            {
                warn!(session_id = session_id, error = %error, "connection task failed");
            }
        });
    }
}

async fn handle_connection(
    socket: TcpStream,
    session_id: u64,
    config: &Config,
    redis_client: redis::Client,
    rooms: SharedRooms,
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
        let mut header_buf = [0u8; HEADER_LEN];
        let read_header = timeout(
            Duration::from_secs(config.heartbeat_timeout_secs),
            reader.read_exact(&mut header_buf),
        )
        .await;

        match read_header {
            Ok(Ok(_)) => {}
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                info!(session_id = session.id, "peer closed connection");
                break;
            }
            Ok(Err(error)) => return Err(Box::new(error)),
            Err(_) => {
                queue_error(&tx, 0, "HEARTBEAT_TIMEOUT", "connection timed out")?;
                break;
            }
        }

        let header = match parse_header(header_buf) {
            Ok(value) => value,
            Err(error_code) => {
                queue_error(&tx, 0, error_code, "invalid header")?;
                break;
            }
        };

        if header.body_len as usize > config.max_body_len {
            queue_error(&tx, header.seq, "BODY_TOO_LARGE", "body too large")?;
            break;
        }

        let mut body = vec![0u8; header.body_len as usize];
        reader.read_exact(&mut body).await?;

        let Some(message_type) = MessageType::from_u16(header.msg_type) else {
            queue_error(&tx, header.seq, "UNKNOWN_MESSAGE_TYPE", "unknown message type")?;
            continue;
        };

        match message_type {
            MessageType::AuthReq => {
                let request = match AuthReq::decode(body.as_slice()) {
                    Ok(value) => value,
                    Err(_) => {
                        queue_error(&tx, header.seq, "INVALID_AUTH_BODY", "invalid auth body")?;
                        continue;
                    }
                };

                match verify_ticket(&config.ticket_secret, &request.ticket) {
                    Ok(player_id) => {
                        let ticket_key = format!("{}ticket:{}", config.redis_key_prefix, crate::ticket::hash_ticket(&request.ticket));
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
                                player_id,
                                error_code: String::new(),
                            },
                        )?;
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
            MessageType::RoomJoinReq => {
                let Some(player_id) = ensure_authenticated(&session, &tx, header.seq)? else {
                    continue;
                };

                let request = match RoomJoinReq::decode(body.as_slice()) {
                    Ok(value) => value,
                    Err(_) => {
                        queue_error(&tx, header.seq, "INVALID_ROOM_JOIN_BODY", "invalid room join body")?;
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

                let join_result = {
                    let mut rooms_guard = rooms.lock().await;
                    join_room(&mut rooms_guard, &room_id, &player_id, tx.clone())
                };

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
                        broadcast_snapshot(&rooms, &room_id, "member_joined", snapshot).await?;
                    }
                    Err(error_code) => {
                        queue_message(
                            &tx,
                            MessageType::RoomJoinRes,
                            header.seq,
                            RoomJoinRes {
                                ok: false,
                                room_id,
                                error_code: error_code.to_string(),
                            },
                        )?;
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

                let snapshot = {
                    let mut rooms_guard = rooms.lock().await;
                    leave_room(&mut rooms_guard, &room_id, &player_id)
                };

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

                if let Some(snapshot) = snapshot {
                    broadcast_snapshot(&rooms, &room_id, "member_left", snapshot).await?;
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

                let request = match RoomReadyReq::decode(body.as_slice()) {
                    Ok(value) => value,
                    Err(_) => {
                        queue_error(&tx, header.seq, "INVALID_ROOM_READY_BODY", "invalid room ready body")?;
                        continue;
                    }
                };

                let ready_result = {
                    let mut rooms_guard = rooms.lock().await;
                    set_ready_state(&mut rooms_guard, &room_id, &player_id, request.ready)
                };

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
                        broadcast_snapshot(&rooms, &room_id, "ready_changed", snapshot).await?;
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
            _ => {
                queue_error(&tx, header.seq, "MESSAGE_NOT_SUPPORTED", "message not supported in this phase")?;
            }
        }
    }

    if let (Some(room_id), Some(player_id)) = (session.room_id.clone(), session.player_id.clone()) {
        let snapshot = {
            let mut rooms_guard = rooms.lock().await;
            leave_room(&mut rooms_guard, &room_id, &player_id)
        };

        if let Some(snapshot) = snapshot {
            let _ = broadcast_snapshot(&rooms, &room_id, "member_disconnected", snapshot).await;
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

fn join_room(
    rooms: &mut HashMap<String, Room>,
    room_id: &str,
    player_id: &str,
    sender: mpsc::UnboundedSender<OutboundMessage>,
) -> Result<RoomSnapshot, &'static str> {
    let room = rooms.entry(room_id.to_string()).or_insert_with(|| Room {
        room_id: room_id.to_string(),
        owner_player_id: player_id.to_string(),
        members: HashMap::new(),
    });

    if room.members.len() >= 10 && !room.members.contains_key(player_id) {
        return Err("ROOM_FULL");
    }

    room.members.insert(
        player_id.to_string(),
        RoomMemberState {
            player_id: player_id.to_string(),
            ready: false,
            sender,
        },
    );

    Ok(room.snapshot())
}

fn leave_room(
    rooms: &mut HashMap<String, Room>,
    room_id: &str,
    player_id: &str,
) -> Option<RoomSnapshot> {
    let room = rooms.get_mut(room_id)?;
    room.members.remove(player_id)?;

    if room.members.is_empty() {
        rooms.remove(room_id);
        return None;
    }

    if room.owner_player_id == player_id {
        if let Some(next_owner) = room.members.keys().next() {
            room.owner_player_id = next_owner.clone();
        }
    }

    Some(room.snapshot())
}

fn set_ready_state(
    rooms: &mut HashMap<String, Room>,
    room_id: &str,
    player_id: &str,
    ready: bool,
) -> Result<RoomSnapshot, &'static str> {
    let room = rooms.get_mut(room_id).ok_or("ROOM_NOT_FOUND")?;
    let member = room
        .members
        .get_mut(player_id)
        .ok_or("ROOM_MEMBER_NOT_FOUND")?;
    member.ready = ready;
    Ok(room.snapshot())
}

async fn broadcast_snapshot(
    rooms: &SharedRooms,
    room_id: &str,
    event: &str,
    snapshot: RoomSnapshot,
) -> Result<(), std::io::Error> {
    let body = encode_body(RoomStatePush {
        event: event.to_string(),
        snapshot: Some(snapshot.clone()),
    });

    let senders = {
        let rooms_guard = rooms.lock().await;
        let Some(room) = rooms_guard.get(room_id) else {
            return Ok(());
        };

        room.members
            .values()
            .map(|member| member.sender.clone())
            .collect::<Vec<_>>()
    };

    for sender in senders {
        let _ = sender.send(OutboundMessage {
            message_type: MessageType::RoomStatePush,
            seq: 0,
            body: body.clone(),
        });
    }

    Ok(())
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

fn queue_message<M: Message>(
    tx: &mpsc::UnboundedSender<OutboundMessage>,
    message_type: MessageType,
    seq: u32,
    message: M,
) -> Result<(), std::io::Error> {
    let body = encode_body(message);
    tx.send(OutboundMessage {
        message_type,
        seq,
        body,
    })
    .map_err(|_| std::io::Error::other("failed to queue outbound"))
}

fn encode_body<M: Message>(message: M) -> Vec<u8> {
    let mut body = Vec::new();
    message.encode(&mut body).expect("protobuf encode failed");
    body
}

fn current_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default()
}

