use interprocess::local_socket::tokio::Listener;
use interprocess::local_socket::traits::tokio::Listener as _;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{Duration, timeout};
use tracing::warn;

use crate::core::context::ServiceContext;
use crate::core::service::room_service;
use crate::pb::{CreateMatchedRoomReq, ErrorRes};
use crate::protocol::{HEADER_LEN, MessageType, Packet, encode_body, encode_packet, parse_header};

const INTERNAL_MAX_BODY_LEN: usize = 64 * 1024;

pub async fn run_listener(
    listener: Listener,
    services: ServiceContext,
) -> Result<(), std::io::Error> {
    let mut next_connection_id = 2_000_000u64;

    loop {
        let socket = listener.accept().await?;
        let services = services.clone();
        let connection_id = next_connection_id;
        next_connection_id = next_connection_id.saturating_add(1);

        tokio::spawn(async move {
            if let Err(error) = handle_internal_connection(socket, services).await {
                warn!(
                    connection_id = connection_id,
                    error = %error,
                    "internal matched-room connection failed"
                );
            }
        });
    }
}

async fn handle_internal_connection<S>(
    socket: S,
    services: ServiceContext,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(socket);

    loop {
        let Some(packet) = read_packet(&mut reader).await? else {
            break;
        };

        match packet.message_type() {
            Some(MessageType::CreateMatchedRoomReq) => {
                let request = packet
                    .decode_body::<CreateMatchedRoomReq>("INVALID_CREATE_MATCHED_ROOM_BODY")
                    .map_err(std::io::Error::other)?;

                let response = room_service::handle_create_matched_room_internal(
                    &services,
                    request,
                )
                .await;

                write_message(
                    &mut writer,
                    MessageType::CreateMatchedRoomRes,
                    packet.header.seq,
                    &response,
                )
                .await?;
            }
            Some(_) => {
                write_error(
                    &mut writer,
                    packet.header.seq,
                    "MESSAGE_NOT_SUPPORTED",
                    "message not supported on internal channel",
                )
                .await?;
            }
            None => {
                write_error(
                    &mut writer,
                    packet.header.seq,
                    "UNKNOWN_MESSAGE_TYPE",
                    "unknown message type",
                )
                .await?;
            }
        }
    }

    Ok(())
}

async fn read_packet<R>(
    reader: &mut R,
) -> Result<Option<Packet>, Box<dyn std::error::Error>>
where
    R: AsyncRead + Unpin,
{
    let read_header = timeout(Duration::from_secs(10), read_header_bytes(reader)).await;
    let header_buf = match read_header {
        Ok(Ok(Some(header_buf))) => header_buf,
        Ok(Ok(None)) => return Ok(None),
        Ok(Err(error)) => return Err(Box::new(error)),
        Err(_) => {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "internal read timeout",
            )));
        }
    };

    let header = parse_header(header_buf).map_err(std::io::Error::other)?;
    if header.body_len as usize > INTERNAL_MAX_BODY_LEN {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "body too large",
        )));
    }

    let mut body = vec![0u8; header.body_len as usize];
    reader.read_exact(&mut body).await?;
    Ok(Some(Packet::new(header, body)))
}

async fn read_header_bytes<R>(
    reader: &mut R,
) -> Result<Option<[u8; HEADER_LEN]>, std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut header_buf = [0u8; HEADER_LEN];
    match reader.read_exact(&mut header_buf).await {
        Ok(_) => Ok(Some(header_buf)),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error),
    }
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
    let packet = encode_packet(message_type, seq, &body);
    writer.write_all(&packet).await
}

async fn write_error<W>(
    writer: &mut W,
    seq: u32,
    error_code: &str,
    message: &str,
) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
{
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
