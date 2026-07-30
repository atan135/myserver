use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::error::Error as WebSocketError;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Message, WebSocketConfig};
use tokio_tungstenite::{WebSocketStream, accept_async_with_config};

use crate::protocol::{HEADER_LEN, parse_header};

const OUTBOUND_BRIDGE_QUEUE_CAPACITY: usize = 1;
const WEBSOCKET_FRAME_OVERHEAD_BUDGET: usize = 256;

#[derive(Clone, Copy, Debug)]
pub struct AdapterConfig {
    pub handshake_timeout: Duration,
    pub handshake_max_bytes: usize,
    pub max_frame_len: usize,
    pub max_body_len: usize,
    pub bridge_capacity: usize,
    pub io_timeout: Duration,
}

#[derive(Debug)]
pub struct AdapterError {
    category: &'static str,
}

impl AdapterError {
    fn new(category: &'static str) -> Self {
        Self { category }
    }

    pub fn category(&self) -> &'static str {
        self.category
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.category)
    }
}

impl std::error::Error for AdapterError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CloseSpec {
    code: CloseCode,
    reason: &'static str,
}

impl CloseSpec {
    const fn protocol(reason: &'static str) -> Self {
        Self {
            code: CloseCode::Protocol,
            reason,
        }
    }

    const fn too_large() -> Self {
        Self {
            code: CloseCode::Size,
            reason: "message_too_big",
        }
    }

    const fn internal(reason: &'static str) -> Self {
        Self {
            code: CloseCode::Error,
            reason,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MessageAction {
    ForwardBinary,
    FlushAutomaticPong,
    Ignore,
    PeerClose,
    Reject(CloseSpec),
}

enum OutboundBridgeItem {
    Packet(Vec<u8>),
    Failed,
}

struct HandshakeLimitedStream<S> {
    inner: S,
    remaining_handshake_bytes: usize,
    handshake_complete: bool,
}

impl<S> HandshakeLimitedStream<S> {
    fn new(inner: S, handshake_max_bytes: usize) -> Self {
        Self {
            inner,
            remaining_handshake_bytes: handshake_max_bytes,
            handshake_complete: false,
        }
    }

    fn complete_handshake(&mut self) {
        self.handshake_complete = true;
    }
}

impl<S> AsyncRead for HandshakeLimitedStream<S>
where
    S: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.handshake_complete {
            return Pin::new(&mut this.inner).poll_read(context, buffer);
        }
        if buffer.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if this.remaining_handshake_bytes == 0 {
            return Poll::Ready(Err(std::io::Error::other(
                "WebSocket handshake exceeds byte limit",
            )));
        }

        let max_read = buffer.remaining().min(this.remaining_handshake_bytes);
        let unfilled = buffer.initialize_unfilled_to(max_read);
        let mut limited_buffer = ReadBuf::new(&mut unfilled[..max_read]);
        match Pin::new(&mut this.inner).poll_read(context, &mut limited_buffer) {
            Poll::Ready(Ok(())) => {
                let bytes_read = limited_buffer.filled().len();
                buffer.advance(bytes_read);
                this.remaining_handshake_bytes -= bytes_read;
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(error)) => Poll::Ready(Err(error)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S> AsyncWrite for HandshakeLimitedStream<S>
where
    S: AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(context, buffer)
    }

    fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(context)
    }

    fn poll_shutdown(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(context)
    }
}

pub async fn serve<F, Fut>(
    socket: TcpStream,
    config: AdapterConfig,
    handler: F,
) -> Result<(), AdapterError>
where
    F: FnOnce(DuplexStream) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let websocket = upgrade(socket, config).await?;
    let (handler_stream, adapter_stream) = tokio::io::duplex(config.bridge_capacity);
    let mut handler_task = tokio::spawn(handler(handler_stream));
    let (adapter_reader, mut adapter_writer) = tokio::io::split(adapter_stream);
    let (outbound_tx, mut outbound_rx) = mpsc::channel(OUTBOUND_BRIDGE_QUEUE_CAPACITY);
    let outbound_task = tokio::spawn(pump_outbound_packets(
        adapter_reader,
        outbound_tx,
        config.max_body_len,
        config.max_frame_len,
    ));

    let adapter_result =
        run_adapter_loop(websocket, &mut adapter_writer, &mut outbound_rx, config).await;

    let _ = adapter_writer.shutdown().await;
    outbound_task.abort();
    let _ = outbound_task.await;

    match timeout(config.io_timeout, &mut handler_task).await {
        Ok(Ok(())) => {}
        Ok(Err(_)) => return Err(AdapterError::new("handler_task_failed")),
        Err(_) => {
            handler_task.abort();
            let _ = handler_task.await;
            return Err(AdapterError::new("handler_shutdown_timeout"));
        }
    }

    adapter_result
}

async fn upgrade(
    socket: TcpStream,
    config: AdapterConfig,
) -> Result<WebSocketStream<HandshakeLimitedStream<TcpStream>>, AdapterError> {
    let limited_socket = HandshakeLimitedStream::new(socket, config.handshake_max_bytes);
    let upgraded = timeout(
        config.handshake_timeout,
        accept_async_with_config(limited_socket, Some(websocket_config(config))),
    )
    .await
    .map_err(|_| AdapterError::new("handshake_timeout"))?
    .map_err(|_| AdapterError::new("handshake_rejected"))?;

    let mut upgraded = upgraded;
    upgraded.get_mut().complete_handshake();
    Ok(upgraded)
}

fn websocket_config(config: AdapterConfig) -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(config.max_frame_len)
        .write_buffer_size(0)
        .max_write_buffer_size(
            config
                .max_frame_len
                .saturating_add(WEBSOCKET_FRAME_OVERHEAD_BUDGET),
        )
        .max_message_size(Some(config.max_frame_len))
        .max_frame_size(Some(config.max_frame_len))
}

async fn run_adapter_loop<S>(
    mut websocket: WebSocketStream<S>,
    bridge_writer: &mut tokio::io::WriteHalf<DuplexStream>,
    outbound_rx: &mut mpsc::Receiver<OutboundBridgeItem>,
    config: AdapterConfig,
) -> Result<(), AdapterError>
where
    S: AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        tokio::select! {
            incoming = websocket.next() => {
                let Some(incoming) = incoming else {
                    return Ok(());
                };
                let message = match incoming {
                    Ok(message) => message,
                    Err(WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed) => {
                        return Ok(());
                    }
                    Err(error) => {
                        let close = close_for_websocket_error(&error);
                        let _ = send_close(&mut websocket, close, config.io_timeout).await;
                        return Err(AdapterError::new("websocket_read_failed"));
                    }
                };

                match classify_message(&message, config.max_frame_len, config.max_body_len) {
                    MessageAction::ForwardBinary => {
                        let Message::Binary(frame) = message else {
                            unreachable!("binary action must contain a binary message");
                        };
                        timeout(config.io_timeout, bridge_writer.write_all(&frame))
                            .await
                            .map_err(|_| AdapterError::new("bridge_write_timeout"))?
                            .map_err(|_| AdapterError::new("bridge_write_failed"))?;
                    }
                    MessageAction::FlushAutomaticPong => {
                        timeout(config.io_timeout, websocket.flush())
                            .await
                            .map_err(|_| AdapterError::new("pong_write_timeout"))?
                            .map_err(|_| AdapterError::new("pong_write_failed"))?;
                    }
                    MessageAction::Ignore => {}
                    MessageAction::PeerClose => {
                        let _ = timeout(config.io_timeout, websocket.flush()).await;
                        return Ok(());
                    }
                    MessageAction::Reject(close) => {
                        let _ = send_close(&mut websocket, close, config.io_timeout).await;
                        return Ok(());
                    }
                }
            }
            outbound = outbound_rx.recv() => {
                match outbound {
                    Some(OutboundBridgeItem::Packet(packet)) => {
                        timeout(config.io_timeout, websocket.send(Message::Binary(packet.into())))
                            .await
                            .map_err(|_| AdapterError::new("websocket_write_timeout"))?
                            .map_err(|_| AdapterError::new("websocket_write_failed"))?;
                    }
                    Some(OutboundBridgeItem::Failed) => {
                        let _ = send_close(
                            &mut websocket,
                            CloseSpec::internal("invalid_outbound_packet"),
                            config.io_timeout,
                        )
                        .await;
                        return Err(AdapterError::new("outbound_bridge_failed"));
                    }
                    None => {
                        let _ = send_close(
                            &mut websocket,
                            CloseSpec {
                                code: CloseCode::Normal,
                                reason: "handler_exit",
                            },
                            config.io_timeout,
                        )
                        .await;
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn classify_message(message: &Message, max_frame_len: usize, max_body_len: usize) -> MessageAction {
    match message {
        Message::Binary(frame) => match validate_binary_frame(frame, max_frame_len, max_body_len) {
            Ok(()) => MessageAction::ForwardBinary,
            Err(close) => MessageAction::Reject(close),
        },
        Message::Text(_) => MessageAction::Reject(CloseSpec {
            code: CloseCode::Unsupported,
            reason: "unsupported_data",
        }),
        Message::Ping(_) => MessageAction::FlushAutomaticPong,
        Message::Pong(_) => MessageAction::Ignore,
        Message::Close(_) => MessageAction::PeerClose,
        Message::Frame(_) => MessageAction::Reject(CloseSpec::protocol("invalid_control_frame")),
    }
}

fn validate_binary_frame(
    frame: &[u8],
    max_frame_len: usize,
    max_body_len: usize,
) -> Result<(), CloseSpec> {
    if frame.len() > max_frame_len {
        return Err(CloseSpec::too_large());
    }
    if frame.len() < HEADER_LEN {
        return Err(CloseSpec::protocol("invalid_packet_length"));
    }

    let header_bytes: [u8; HEADER_LEN] = frame[..HEADER_LEN]
        .try_into()
        .expect("header length was checked");
    let header =
        parse_header(header_bytes).map_err(|_| CloseSpec::protocol("invalid_packet_header"))?;
    if header.version != 1 || header.flags != 0 {
        return Err(CloseSpec::protocol("invalid_packet_header"));
    }

    let body_len = header.body_len as usize;
    if body_len > max_body_len {
        return Err(CloseSpec::too_large());
    }
    let expected_len = HEADER_LEN
        .checked_add(body_len)
        .ok_or_else(CloseSpec::too_large)?;
    if expected_len != frame.len() {
        return Err(CloseSpec::protocol("invalid_packet_length"));
    }

    Ok(())
}

fn close_for_websocket_error(error: &WebSocketError) -> CloseSpec {
    match error {
        WebSocketError::Capacity(_) => CloseSpec::too_large(),
        WebSocketError::Protocol(_) => CloseSpec::protocol("websocket_protocol_error"),
        _ => CloseSpec::internal("websocket_transport_error"),
    }
}

async fn send_close<S>(
    websocket: &mut WebSocketStream<S>,
    close: CloseSpec,
    io_timeout: Duration,
) -> Result<(), AdapterError>
where
    S: AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = CloseFrame {
        code: close.code,
        reason: close.reason.into(),
    };
    timeout(io_timeout, websocket.send(Message::Close(Some(frame))))
        .await
        .map_err(|_| AdapterError::new("close_write_timeout"))?
        .map_err(|_| AdapterError::new("close_write_failed"))
}

async fn pump_outbound_packets<R>(
    mut reader: R,
    sender: mpsc::Sender<OutboundBridgeItem>,
    max_body_len: usize,
    max_frame_len: usize,
) where
    R: AsyncRead + Unpin,
{
    loop {
        match read_outbound_packet(&mut reader, max_body_len, max_frame_len).await {
            Ok(Some(packet)) => {
                if sender
                    .send(OutboundBridgeItem::Packet(packet))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Ok(None) => return,
            Err(()) => {
                let _ = sender.send(OutboundBridgeItem::Failed).await;
                return;
            }
        }
    }
}

async fn read_outbound_packet<R>(
    reader: &mut R,
    max_body_len: usize,
    max_frame_len: usize,
) -> Result<Option<Vec<u8>>, ()>
where
    R: AsyncRead + Unpin,
{
    let mut header_bytes = [0u8; HEADER_LEN];
    match reader.read_exact(&mut header_bytes[..1]).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(_) => return Err(()),
    }
    reader
        .read_exact(&mut header_bytes[1..])
        .await
        .map_err(|_| ())?;

    let header = parse_header(header_bytes).map_err(|_| ())?;
    if header.version != 1 || header.flags != 0 {
        return Err(());
    }
    let body_len = header.body_len as usize;
    let packet_len = HEADER_LEN.checked_add(body_len).ok_or(())?;
    if body_len > max_body_len || packet_len > max_frame_len {
        return Err(());
    }

    let mut packet = vec![0u8; packet_len];
    packet[..HEADER_LEN].copy_from_slice(&header_bytes);
    reader
        .read_exact(&mut packet[HEADER_LEN..])
        .await
        .map_err(|_| ())?;
    Ok(Some(packet))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::encode_packet;
    use tokio_tungstenite::tungstenite::protocol::Role;

    const MAX_BODY_LEN: usize = 32;
    const MAX_FRAME_LEN: usize = HEADER_LEN + MAX_BODY_LEN;

    #[test]
    fn accepts_one_complete_binary_packet() {
        let packet = encode_packet(20001, 7, &[1, 2, 3]);
        assert_eq!(
            classify_message(&Message::Binary(packet.into()), MAX_FRAME_LEN, MAX_BODY_LEN),
            MessageAction::ForwardBinary
        );
    }

    #[test]
    fn accepts_complete_header_with_empty_body() {
        let packet = encode_packet(20209, 8, &[]);
        assert_eq!(
            validate_binary_frame(&packet, MAX_FRAME_LEN, MAX_BODY_LEN),
            Ok(())
        );
    }

    #[test]
    fn rejects_truncated_binary_packet() {
        let mut packet = encode_packet(20001, 7, &[1, 2, 3]);
        packet.pop();
        assert_eq!(
            validate_binary_frame(&packet, MAX_FRAME_LEN, MAX_BODY_LEN),
            Err(CloseSpec::protocol("invalid_packet_length"))
        );
    }

    #[test]
    fn rejects_multiple_packets_in_one_binary_message() {
        let mut packets = encode_packet(20001, 7, &[1]);
        packets.extend_from_slice(&encode_packet(20209, 8, &[]));
        assert_eq!(
            validate_binary_frame(&packets, MAX_FRAME_LEN, MAX_BODY_LEN),
            Err(CloseSpec::protocol("invalid_packet_length"))
        );
    }

    #[test]
    fn rejects_text_message_as_unsupported_data() {
        assert_eq!(
            classify_message(&Message::Text("hello".into()), MAX_FRAME_LEN, MAX_BODY_LEN),
            MessageAction::Reject(CloseSpec {
                code: CloseCode::Unsupported,
                reason: "unsupported_data",
            })
        );
    }

    #[test]
    fn rejects_oversized_frame_and_declared_body() {
        let oversized = vec![0; MAX_FRAME_LEN + 1];
        assert_eq!(
            validate_binary_frame(&oversized, MAX_FRAME_LEN, MAX_BODY_LEN),
            Err(CloseSpec::too_large())
        );

        let declared_too_large = encode_packet(20001, 7, &[0; MAX_BODY_LEN + 1]);
        assert_eq!(
            validate_binary_frame(&declared_too_large, declared_too_large.len(), MAX_BODY_LEN),
            Err(CloseSpec::too_large())
        );
    }

    #[test]
    fn handles_ping_pong_and_close_as_control_messages() {
        assert_eq!(
            classify_message(
                &Message::Ping(vec![1, 2].into()),
                MAX_FRAME_LEN,
                MAX_BODY_LEN
            ),
            MessageAction::FlushAutomaticPong
        );
        assert_eq!(
            classify_message(
                &Message::Pong(vec![1, 2].into()),
                MAX_FRAME_LEN,
                MAX_BODY_LEN
            ),
            MessageAction::Ignore
        );
        assert_eq!(
            classify_message(&Message::Close(None), MAX_FRAME_LEN, MAX_BODY_LEN),
            MessageAction::PeerClose
        );
    }

    #[test]
    fn websocket_library_limits_frame_message_and_write_buffers() {
        let config = AdapterConfig {
            handshake_timeout: Duration::from_secs(5),
            handshake_max_bytes: 1024,
            max_frame_len: MAX_FRAME_LEN,
            max_body_len: MAX_BODY_LEN,
            bridge_capacity: 128,
            io_timeout: Duration::from_secs(5),
        };
        let websocket = websocket_config(config);
        assert_eq!(websocket.read_buffer_size, MAX_FRAME_LEN);
        assert_eq!(websocket.max_frame_size, Some(MAX_FRAME_LEN));
        assert_eq!(websocket.max_message_size, Some(MAX_FRAME_LEN));
        assert_eq!(
            websocket.max_write_buffer_size,
            MAX_FRAME_LEN + WEBSOCKET_FRAME_OVERHEAD_BUDGET
        );
    }

    #[tokio::test]
    async fn outbound_packets_remain_separate_binary_messages() {
        let first = encode_packet(20002, 7, &[1]);
        let second = encode_packet(20210, 8, &[2, 3]);
        let (mut writer, reader) = tokio::io::duplex(256);
        writer.write_all(&first).await.unwrap();
        writer.write_all(&second).await.unwrap();
        writer.shutdown().await.unwrap();

        let mut reader = reader;
        assert_eq!(
            read_outbound_packet(&mut reader, MAX_BODY_LEN, MAX_FRAME_LEN)
                .await
                .unwrap(),
            Some(first)
        );
        assert_eq!(
            read_outbound_packet(&mut reader, MAX_BODY_LEN, MAX_FRAME_LEN)
                .await
                .unwrap(),
            Some(second)
        );
        assert_eq!(
            read_outbound_packet(&mut reader, MAX_BODY_LEN, MAX_FRAME_LEN)
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn handshake_stream_enforces_limit_until_upgrade_completes() {
        let (mut peer, stream) = tokio::io::duplex(16);
        peer.write_all(&[1, 2, 3, 4, 5, 6]).await.unwrap();
        let mut limited = HandshakeLimitedStream::new(stream, 4);
        let mut first = [0; 4];

        limited.read_exact(&mut first).await.unwrap();
        assert_eq!(first, [1, 2, 3, 4]);
        assert!(limited.read_u8().await.is_err());

        limited.complete_handshake();
        assert_eq!(limited.read_u8().await.unwrap(), 5);
        assert_eq!(limited.read_u8().await.unwrap(), 6);
    }

    #[tokio::test]
    async fn adapter_bridges_one_packet_per_binary_message_and_flushes_pong() {
        let config = AdapterConfig {
            handshake_timeout: Duration::from_secs(1),
            handshake_max_bytes: 1024,
            max_frame_len: MAX_FRAME_LEN,
            max_body_len: MAX_BODY_LEN,
            bridge_capacity: 256,
            io_timeout: Duration::from_secs(1),
        };
        let (client_io, server_io) = tokio::io::duplex(1024);
        let server_websocket = WebSocketStream::from_raw_socket(
            server_io,
            Role::Server,
            Some(websocket_config(config)),
        )
        .await;
        let mut client_websocket =
            WebSocketStream::from_raw_socket(client_io, Role::Client, None).await;
        let (mut handler_stream, adapter_stream) = tokio::io::duplex(config.bridge_capacity);
        let (adapter_reader, mut adapter_writer) = tokio::io::split(adapter_stream);
        let (outbound_tx, mut outbound_rx) = mpsc::channel(1);
        let adapter_task = tokio::spawn(async move {
            let _adapter_reader = adapter_reader;
            run_adapter_loop(
                server_websocket,
                &mut adapter_writer,
                &mut outbound_rx,
                config,
            )
            .await
        });

        let ping_payload = vec![1, 2, 3];
        client_websocket
            .send(Message::Ping(ping_payload.clone().into()))
            .await
            .unwrap();
        let pong = timeout(Duration::from_secs(1), client_websocket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(pong, Message::Pong(ping_payload.into()));

        let inbound = encode_packet(20001, 7, &[1, 2]);
        client_websocket
            .send(Message::Binary(inbound.clone().into()))
            .await
            .unwrap();
        let mut forwarded = vec![0; inbound.len()];
        timeout(
            Duration::from_secs(1),
            handler_stream.read_exact(&mut forwarded),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(forwarded, inbound);

        let outbound = encode_packet(20002, 7, &[3, 4]);
        outbound_tx
            .send(OutboundBridgeItem::Packet(outbound.clone()))
            .await
            .unwrap();
        let delivered = timeout(Duration::from_secs(1), client_websocket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(delivered, Message::Binary(outbound.into()));

        client_websocket.send(Message::Close(None)).await.unwrap();
        assert!(
            timeout(Duration::from_secs(1), adapter_task)
                .await
                .unwrap()
                .unwrap()
                .is_ok()
        );
    }
}
