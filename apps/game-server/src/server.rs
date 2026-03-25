use std::time::{SystemTime, UNIX_EPOCH};

use prost::Message;
use redis::AsyncCommands;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use crate::config::Config;
use crate::pb::{AuthReq, AuthRes, ErrorRes, PingRes, RoomJoinReq, RoomJoinRes};
use crate::protocol::{HEADER_LEN, MessageType, encode_packet, parse_header};
use crate::session::{Session, SessionState};
use crate::ticket::verify_ticket;

pub async fn run(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(config.bind_addr()).await?;
    let redis_client = redis::Client::open(config.redis_url.clone())?;
    info!(addr = %config.bind_addr(), redis = %config.redis_url, "game server listening");

    let mut next_session_id: u64 = 1;

    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let session_id = next_session_id;
        next_session_id += 1;

        info!(session_id = session_id, peer = %peer_addr, "accepted tcp connection");

        let connection_config = config.clone();
        let redis_client = redis_client.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(socket, session_id, &connection_config, redis_client).await {
                warn!(session_id = session_id, error = %error, "connection task failed");
            }
        });
    }
}

async fn handle_connection(
    mut socket: TcpStream,
    session_id: u64,
    config: &Config,
    redis_client: redis::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut session = Session::new(session_id);
    let mut redis = redis_client.get_multiplexed_async_connection().await?;

    loop {
        let mut header_buf = [0u8; HEADER_LEN];
        let read_header = timeout(
            Duration::from_secs(config.heartbeat_timeout_secs),
            socket.read_exact(&mut header_buf),
        )
        .await;

        match read_header {
            Ok(Ok(_)) => {}
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                info!(session_id = session.id, "peer closed connection");
                return Ok(());
            }
            Ok(Err(error)) => return Err(Box::new(error)),
            Err(_) => {
                send_error(&mut socket, 0, "HEARTBEAT_TIMEOUT", "connection timed out").await?;
                return Ok(());
            }
        }

        let header = match parse_header(header_buf) {
            Ok(value) => value,
            Err(error_code) => {
                send_error(&mut socket, 0, error_code, "invalid header").await?;
                return Ok(());
            }
        };

        if header.body_len as usize > config.max_body_len {
            send_error(&mut socket, header.seq, "BODY_TOO_LARGE", "body too large").await?;
            return Ok(());
        }

        let mut body = vec![0u8; header.body_len as usize];
        socket.read_exact(&mut body).await?;

        let Some(message_type) = MessageType::from_u16(header.msg_type) else {
            send_error(&mut socket, header.seq, "UNKNOWN_MESSAGE_TYPE", "unknown message type").await?;
            continue;
        };

        match message_type {
            MessageType::AuthReq => {
                let request = match AuthReq::decode(body.as_slice()) {
                    Ok(value) => value,
                    Err(_) => {
                        send_error(&mut socket, header.seq, "INVALID_AUTH_BODY", "invalid auth body")
                            .await?;
                        continue;
                    }
                };

                match verify_ticket(&config.ticket_secret, &request.ticket) {
                    Ok(player_id) => {
                        let ticket_key = format!("ticket:{}", crate::ticket::hash_ticket(&request.ticket));
                        let ticket_owner: Option<String> = redis.get(ticket_key).await?;

                        if ticket_owner.as_deref() != Some(player_id.as_str()) {
                            let response = AuthRes {
                                ok: false,
                                player_id: String::new(),
                                error_code: "TICKET_NOT_FOUND".to_string(),
                            };
                            send_message(&mut socket, MessageType::AuthRes, header.seq, response).await?;
                            continue;
                        }

                        session.state = SessionState::Authenticated;
                        session.player_id = Some(player_id.clone());

                        let response = AuthRes {
                            ok: true,
                            player_id,
                            error_code: String::new(),
                        };
                        send_message(&mut socket, MessageType::AuthRes, header.seq, response).await?;
                    }
                    Err(error_code) => {
                        let response = AuthRes {
                            ok: false,
                            player_id: String::new(),
                            error_code: error_code.to_string(),
                        };
                        send_message(&mut socket, MessageType::AuthRes, header.seq, response).await?;
                    }
                }
            }
            MessageType::PingReq => {
                let response = PingRes {
                    server_time: current_unix_ms(),
                };
                send_message(&mut socket, MessageType::PingRes, header.seq, response).await?;
            }
            MessageType::RoomJoinReq => {
                if session.state != SessionState::Authenticated {
                    send_error(
                        &mut socket,
                        header.seq,
                        "NOT_AUTHENTICATED",
                        "authenticate before joining a room",
                    )
                    .await?;
                    continue;
                }

                let request = match RoomJoinReq::decode(body.as_slice()) {
                    Ok(value) => value,
                    Err(_) => {
                        send_error(
                            &mut socket,
                            header.seq,
                            "INVALID_ROOM_JOIN_BODY",
                            "invalid room join body",
                        )
                        .await?;
                        continue;
                    }
                };

                let response = RoomJoinRes {
                    ok: true,
                    room_id: if request.room_id.is_empty() {
                        "room-default".to_string()
                    } else {
                        request.room_id
                    },
                    error_code: String::new(),
                };
                send_message(&mut socket, MessageType::RoomJoinRes, header.seq, response).await?;
            }
            _ => {
                send_error(
                    &mut socket,
                    header.seq,
                    "MESSAGE_NOT_SUPPORTED",
                    "message not supported in this phase",
                )
                .await?;
            }
        }
    }
}

async fn send_error(
    socket: &mut TcpStream,
    seq: u32,
    error_code: &str,
    message: &str,
) -> Result<(), std::io::Error> {
    let payload = ErrorRes {
        error_code: error_code.to_string(),
        message: message.to_string(),
    };

    send_message(socket, MessageType::ErrorRes, seq, payload).await
}

async fn send_message<M: Message>(
    socket: &mut TcpStream,
    msg_type: MessageType,
    seq: u32,
    message: M,
) -> Result<(), std::io::Error> {
    let mut body = Vec::new();
    message.encode(&mut body).expect("protobuf encode failed");
    let packet = encode_packet(msg_type, seq, &body);
    socket.write_all(&packet).await
}

fn current_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default()
}
