use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{Duration, timeout};

use crate::pb::ErrorRes;
use crate::protocol::{HEADER_LEN, MessageType, Packet, encode_body, encode_packet, parse_header};

const ADMIN_MAX_BODY_LEN: usize = 64 * 1024;

pub(super) async fn read_packet(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
) -> Result<Option<Packet>, Box<dyn std::error::Error>> {
    let read_header = timeout(Duration::from_secs(10), read_header_bytes(reader)).await;
    let header_buf = match read_header {
        Ok(Ok(Some(header_buf))) => header_buf,
        Ok(Ok(None)) => return Ok(None),
        Ok(Err(error)) => return Err(Box::new(error)),
        Err(_) => {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "read timeout",
            )));
        }
    };

    let header = parse_header(header_buf).map_err(std::io::Error::other)?;
    if header.body_len as usize > ADMIN_MAX_BODY_LEN {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "body too large",
        )));
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

pub(super) async fn write_message<M: prost::Message>(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    message_type: MessageType,
    seq: u32,
    message: &M,
) -> Result<(), std::io::Error> {
    let body = encode_body(message);
    let packet = encode_packet(message_type, seq, &body);
    writer.write_all(&packet).await
}

pub(super) async fn write_error(
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
