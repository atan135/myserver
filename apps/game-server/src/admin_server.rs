use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, timeout};
use tracing::warn;

use crate::admin_pb::{ServerStatusReq, ServerStatusRes, UpdateConfigReq, UpdateConfigRes};
use crate::pb::ErrorRes;
use crate::protocol::{HEADER_LEN, MessageType, Packet, encode_body, encode_packet, parse_header};
use crate::server::{RuntimeConfig, SharedRooms, SharedRuntimeConfig};

const ADMIN_MAX_BODY_LEN: usize = 64 * 1024;

pub async fn run_listener(
    listener: TcpListener,
    rooms: SharedRooms,
    runtime_config: SharedRuntimeConfig,
    connection_count: Arc<AtomicU64>,
) -> Result<(), std::io::Error> {
    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let rooms = rooms.clone();
        let runtime_config = runtime_config.clone();
        let connection_count = connection_count.clone();

        tokio::spawn(async move {
            if let Err(error) = handle_admin_connection(socket, rooms, runtime_config, connection_count).await {
                warn!(peer = %peer_addr, error = %error, "admin connection failed");
            }
        });
    }
}

async fn handle_admin_connection(
    socket: TcpStream,
    rooms: SharedRooms,
    runtime_config: SharedRuntimeConfig,
    connection_count: Arc<AtomicU64>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut reader, mut writer) = socket.into_split();

    loop {
        let Some(packet) = read_packet(&mut reader).await? else {
            break;
        };

        match packet.message_type() {
            Some(MessageType::AdminServerStatusReq) => {
                packet
                    .decode_body::<ServerStatusReq>("INVALID_ADMIN_STATUS_BODY")
                    .map_err(std::io::Error::other)?;

                let room_count = {
                    let rooms = rooms.lock().await;
                    rooms.len() as u64
                };
                let RuntimeConfig {
                    heartbeat_timeout_secs,
                    max_body_len,
                } = *runtime_config.read().await;

                write_message(
                    &mut writer,
                    MessageType::AdminServerStatusRes,
                    packet.header.seq,
                    &ServerStatusRes {
                        connection_count: connection_count.load(Ordering::Relaxed),
                        room_count,
                        status: "ok".to_string(),
                        max_body_len: max_body_len as u64,
                        heartbeat_timeout_secs,
                    },
                )
                .await?;
            }
            Some(MessageType::AdminUpdateConfigReq) => {
                let request = packet
                    .decode_body::<UpdateConfigReq>("INVALID_ADMIN_UPDATE_CONFIG_BODY")
                    .map_err(std::io::Error::other)?;
                let result = apply_runtime_config(&runtime_config, &request.key, &request.value).await;

                write_message(
                    &mut writer,
                    MessageType::AdminUpdateConfigRes,
                    packet.header.seq,
                    &UpdateConfigRes {
                        ok: result.is_ok(),
                        error_code: result.err().unwrap_or_default().to_string(),
                    },
                )
                .await?;
            }
            Some(_) => {
                write_error(&mut writer, packet.header.seq, "MESSAGE_NOT_SUPPORTED", "message not supported on admin channel").await?;
            }
            None => {
                write_error(&mut writer, packet.header.seq, "UNKNOWN_MESSAGE_TYPE", "unknown message type").await?;
            }
        }
    }

    Ok(())
}

async fn apply_runtime_config(
    runtime_config: &SharedRuntimeConfig,
    key: &str,
    value: &str,
) -> Result<(), &'static str> {
    let mut runtime = runtime_config.write().await;

    match key {
        "max_body_len" => {
            let parsed = value.parse::<usize>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if !(1..=1024 * 1024).contains(&parsed) {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.max_body_len = parsed;
            Ok(())
        }
        "heartbeat_timeout_secs" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if !(1..=3600).contains(&parsed) {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.heartbeat_timeout_secs = parsed;
            Ok(())
        }
        _ => Err("UNSUPPORTED_CONFIG_KEY"),
    }
}

async fn read_packet(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
) -> Result<Option<Packet>, Box<dyn std::error::Error>> {
    let read_header = timeout(Duration::from_secs(10), read_header_bytes(reader)).await;
    let header_buf = match read_header {
        Ok(Ok(Some(header_buf))) => header_buf,
        Ok(Ok(None)) => return Ok(None),
        Ok(Err(error)) => return Err(Box::new(error)),
        Err(_) => return Err(Box::new(std::io::Error::new(std::io::ErrorKind::TimedOut, "read timeout"))),
    };

    let header = parse_header(header_buf).map_err(std::io::Error::other)?;
    if header.body_len as usize > ADMIN_MAX_BODY_LEN {
        return Err(Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, "body too large")));
    }

    let mut body = vec![0u8; header.body_len as usize];
    reader.read_exact(&mut body).await?;
    Ok(Some(Packet::new(header, body)))
}

async fn read_header_bytes(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
) -> Result<Option<[u8; HEADER_LEN]>, std::io::Error> {
    let mut header_buf = [0u8; HEADER_LEN];
    match reader.read_exact(&mut header_buf).await {
        Ok(_) => Ok(Some(header_buf)),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error),
    }
}

async fn write_message<M: prost::Message>(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    message_type: MessageType,
    seq: u32,
    message: &M,
) -> Result<(), std::io::Error> {
    let body = encode_body(message);
    let packet = encode_packet(message_type, seq, &body);
    writer.write_all(&packet).await
}

async fn write_error(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
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
