use std::fmt;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::error::Error as WebSocketError;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Message, WebSocketConfig};
use tokio_tungstenite::{WebSocketStream, accept_hdr_async_with_config};

use crate::metrics::{METRICS, MetricTransport};
use crate::protocol::{HEADER_LEN, parse_header};

const OUTBOUND_BRIDGE_QUEUE_CAPACITY: usize = 1;
const WEBSOCKET_FRAME_OVERHEAD_BUDGET: usize = 256;

#[derive(Clone, Debug, Default)]
pub struct TrustedProxySet {
    entries: Vec<IpNetwork>,
}

impl TrustedProxySet {
    pub fn parse_csv(value: &str) -> Result<Self, String> {
        let entries = value
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(IpNetwork::parse)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { entries })
    }

    pub(crate) fn contains(&self, ip: IpAddr) -> bool {
        let ip = normalize_ip(ip);
        self.entries.iter().any(|entry| entry.contains(ip))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn contains_universal_network(&self) -> bool {
        self.entries.iter().any(|entry| {
            matches!(
                entry,
                IpNetwork::V4 { prefix: 0, .. } | IpNetwork::V6 { prefix: 0, .. }
            )
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IpNetwork {
    V4 { network: u32, prefix: u8 },
    V6 { network: u128, prefix: u8 },
}

impl IpNetwork {
    fn parse(value: &str) -> Result<Self, String> {
        let (address, prefix) = match value.split_once('/') {
            Some((address, prefix)) if !prefix.contains('/') => (address, Some(prefix)),
            Some(_) => return Err(format!("invalid trusted proxy CIDR: {value}")),
            None => (value, None),
        };
        let ip = address
            .parse::<IpAddr>()
            .map(normalize_ip)
            .map_err(|_| format!("invalid trusted proxy IP or CIDR: {value}"))?;

        match ip {
            IpAddr::V4(ip) => {
                let prefix = parse_prefix(prefix, 32, value)?;
                let mask = prefix_mask_v4(prefix);
                Ok(Self::V4 {
                    network: u32::from(ip) & mask,
                    prefix,
                })
            }
            IpAddr::V6(ip) => {
                let prefix = parse_prefix(prefix, 128, value)?;
                let mask = prefix_mask_v6(prefix);
                Ok(Self::V6 {
                    network: u128::from(ip) & mask,
                    prefix,
                })
            }
        }
    }

    fn contains(self, ip: IpAddr) -> bool {
        match (self, normalize_ip(ip)) {
            (Self::V4 { network, prefix }, IpAddr::V4(ip)) => {
                u32::from(ip) & prefix_mask_v4(prefix) == network
            }
            (Self::V6 { network, prefix }, IpAddr::V6(ip)) => {
                u128::from(ip) & prefix_mask_v6(prefix) == network
            }
            _ => false,
        }
    }
}

fn parse_prefix(prefix: Option<&str>, max: u8, value: &str) -> Result<u8, String> {
    let Some(prefix) = prefix else {
        return Ok(max);
    };
    let parsed = prefix
        .parse::<u8>()
        .map_err(|_| format!("invalid trusted proxy CIDR prefix: {value}"))?;
    if parsed > max {
        return Err(format!("invalid trusted proxy CIDR prefix: {value}"));
    }
    Ok(parsed)
}

fn prefix_mask_v4(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}

fn prefix_mask_v6(prefix: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        u128::MAX << (128 - prefix)
    }
}

fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        ip => ip,
    }
}

fn resolve_client_ip(
    peer_ip: IpAddr,
    trusted_proxies: &TrustedProxySet,
    forwarded_for: Option<&str>,
) -> IpAddr {
    let mut client_ip = normalize_ip(peer_ip);
    if !trusted_proxies.contains(client_ip) {
        return client_ip;
    }

    let Some(forwarded_for) = forwarded_for else {
        return client_ip;
    };
    let forwarded = forwarded_for
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::parse::<IpAddr>)
        .collect::<Result<Vec<_>, _>>();
    let Ok(forwarded) = forwarded else {
        return client_ip;
    };

    for forwarded_ip in forwarded.into_iter().rev().map(normalize_ip) {
        if !trusted_proxies.contains(client_ip) {
            break;
        }
        client_ip = forwarded_ip;
    }
    client_ip
}

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
pub struct ApplicationClose(CloseSpec);

impl ApplicationClose {
    pub const fn normal(reason: &'static str) -> Self {
        Self(CloseSpec {
            code: CloseCode::Normal,
            reason,
        })
    }

    pub const fn policy(reason: &'static str) -> Self {
        Self(CloseSpec {
            code: CloseCode::Policy,
            reason,
        })
    }

    pub const fn overloaded(reason: &'static str) -> Self {
        Self(CloseSpec {
            code: CloseCode::Again,
            reason,
        })
    }

    pub const fn internal(reason: &'static str) -> Self {
        Self(CloseSpec::internal(reason))
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AdapterEnd {
    Normal,
    Abnormal,
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
    peer_ip: IpAddr,
    trusted_proxies: TrustedProxySet,
    handshake_permit: OwnedSemaphorePermit,
    handler: F,
) -> Result<(), AdapterError>
where
    F: FnOnce(DuplexStream, IpAddr) -> Fut + Send + 'static,
    Fut: Future<Output = ApplicationClose> + Send + 'static,
{
    let upgraded = match upgrade(socket, config, peer_ip, trusted_proxies).await {
        Ok(upgraded) => {
            METRICS.record_websocket_handshake_success();
            upgraded
        }
        Err(error) => {
            METRICS.record_websocket_handshake_failure();
            return Err(error);
        }
    };
    drop(handshake_permit);
    let _connection_metric = METRICS.track_connection(MetricTransport::WebSocket);
    let (handler_stream, adapter_stream) = tokio::io::duplex(config.bridge_capacity);
    let (handler_close_tx, mut handler_close_rx) = oneshot::channel();
    let mut handler_task = tokio::spawn(async move {
        let close = handler(handler_stream, upgraded.client_ip).await;
        let _ = handler_close_tx.send(close);
    });
    let (adapter_reader, mut adapter_writer) = tokio::io::split(adapter_stream);
    let (outbound_tx, mut outbound_rx) = mpsc::channel(OUTBOUND_BRIDGE_QUEUE_CAPACITY);
    let outbound_task = tokio::spawn(pump_outbound_packets(
        adapter_reader,
        outbound_tx,
        config.max_body_len,
        config.max_frame_len,
    ));

    let adapter_result = run_adapter_loop(
        upgraded.websocket,
        &mut adapter_writer,
        &mut outbound_rx,
        &mut handler_close_rx,
        config,
    )
    .await;
    let mut abnormal_close = match &adapter_result {
        Ok(AdapterEnd::Normal) => false,
        Ok(AdapterEnd::Abnormal) | Err(_) => true,
    };

    let _ = adapter_writer.shutdown().await;
    outbound_task.abort();
    let _ = outbound_task.await;

    let handler_error = match timeout(config.io_timeout, &mut handler_task).await {
        Ok(Ok(())) => None,
        Ok(Err(_)) => {
            abnormal_close = true;
            Some(AdapterError::new("handler_task_failed"))
        }
        Err(_) => {
            handler_task.abort();
            let _ = handler_task.await;
            abnormal_close = true;
            Some(AdapterError::new("handler_shutdown_timeout"))
        }
    };

    if abnormal_close {
        METRICS.record_websocket_abnormal_close();
    }

    if let Some(error) = handler_error {
        return Err(error);
    }
    adapter_result.map(|_| ())
}

struct UpgradeResult {
    websocket: WebSocketStream<HandshakeLimitedStream<TcpStream>>,
    client_ip: IpAddr,
}

#[allow(clippy::result_large_err)]
async fn upgrade(
    socket: TcpStream,
    config: AdapterConfig,
    peer_ip: IpAddr,
    trusted_proxies: TrustedProxySet,
) -> Result<UpgradeResult, AdapterError> {
    let limited_socket = HandshakeLimitedStream::new(socket, config.handshake_max_bytes);
    let client_ip = Arc::new(Mutex::new(normalize_ip(peer_ip)));
    let resolved_client_ip = Arc::clone(&client_ip);
    let upgraded = timeout(
        config.handshake_timeout,
        accept_hdr_async_with_config(
            limited_socket,
            move |request: &Request, response: Response| {
                let forwarded_for = request
                    .headers()
                    .get("x-forwarded-for")
                    .and_then(|value| value.to_str().ok());
                let resolved = resolve_client_ip(peer_ip, &trusted_proxies, forwarded_for);
                *resolved_client_ip.lock().expect("client IP mutex poisoned") = resolved;
                Ok(response)
            },
            Some(websocket_config(config)),
        ),
    )
    .await
    .map_err(|_| AdapterError::new("handshake_timeout"))?
    .map_err(|_| AdapterError::new("handshake_rejected"))?;

    let mut upgraded = upgraded;
    upgraded.get_mut().complete_handshake();
    let client_ip = *client_ip.lock().expect("client IP mutex poisoned");
    Ok(UpgradeResult {
        websocket: upgraded,
        client_ip,
    })
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
    handler_close_rx: &mut oneshot::Receiver<ApplicationClose>,
    config: AdapterConfig,
) -> Result<AdapterEnd, AdapterError>
where
    S: AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    loop {
        tokio::select! {
            incoming = websocket.next() => {
                let Some(incoming) = incoming else {
                    return Ok(AdapterEnd::Abnormal);
                };
                let message = match incoming {
                    Ok(message) => message,
                    Err(WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed) => {
                        return Ok(AdapterEnd::Normal);
                    }
                    Err(error) => {
                        if matches!(error, WebSocketError::Capacity(_) | WebSocketError::Protocol(_)) {
                            METRICS.record_websocket_frame_rejected();
                        }
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
                        return Ok(AdapterEnd::Normal);
                    }
                    MessageAction::Reject(close) => {
                        METRICS.record_websocket_frame_rejected();
                        let _ = send_close(&mut websocket, close, config.io_timeout).await;
                        return Ok(AdapterEnd::Abnormal);
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
                        let close = timeout(config.io_timeout, handler_close_rx)
                            .await
                            .ok()
                            .and_then(Result::ok)
                            .unwrap_or_else(|| ApplicationClose::internal("handler_exit_failed"));
                        let end = if close.0.code == CloseCode::Normal {
                            AdapterEnd::Normal
                        } else {
                            AdapterEnd::Abnormal
                        };
                        let _ = send_close(
                            &mut websocket,
                            close.0,
                            config.io_timeout,
                        )
                        .await;
                        return Ok(end);
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
    use std::net::{Ipv4Addr, Ipv6Addr};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Semaphore;
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
    fn forwarded_client_ip_requires_a_trusted_socket_peer_and_valid_ip() {
        let trusted = TrustedProxySet::parse_csv("10.20.0.0/16,2001:db8::/32").unwrap();
        let caddy_peer = IpAddr::V4(Ipv4Addr::new(10, 20, 1, 4));
        let direct_peer = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 8));
        let client = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9));

        assert_eq!(
            resolve_client_ip(
                caddy_peer,
                &trusted,
                Some("192.0.2.200, 198.51.100.9, 10.20.1.4")
            ),
            client
        );
        assert_eq!(
            resolve_client_ip(direct_peer, &trusted, Some("198.51.100.9")),
            direct_peer
        );
        assert_eq!(
            resolve_client_ip(caddy_peer, &trusted, Some("not-an-ip")),
            caddy_peer
        );
    }

    #[test]
    fn trusted_proxy_parser_supports_exact_ipv4_ipv6_and_rejects_bad_cidr() {
        let trusted = TrustedProxySet::parse_csv("127.0.0.1,::1,192.0.2.0/24").unwrap();

        assert!(trusted.contains(IpAddr::V4(Ipv4Addr::LOCALHOST)));
        assert!(trusted.contains(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(trusted.contains(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 99))));
        assert!(!trusted.contains(IpAddr::V4(Ipv4Addr::new(192, 0, 3, 1))));
        assert!(TrustedProxySet::parse_csv("10.0.0.0/33").is_err());
        assert!(TrustedProxySet::parse_csv("garbage").is_err());
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
    async fn chat_and_mail_push_packets_remain_separate_binary_messages() {
        let first = encode_packet(20105, 0, &[1]);
        let second = encode_packet(20301, 0, &[2, 3]);
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
    async fn handshake_permit_is_released_after_upgrade_not_after_session_exit() {
        let config = AdapterConfig {
            handshake_timeout: Duration::from_secs(1),
            handshake_max_bytes: 1024,
            max_frame_len: MAX_FRAME_LEN,
            max_body_len: MAX_BODY_LEN,
            bridge_capacity: 256,
            io_timeout: Duration::from_secs(1),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let slots = Arc::new(Semaphore::new(1));
        let permit = Arc::clone(&slots).try_acquire_owned().unwrap();
        let server = tokio::spawn(async move {
            let (socket, peer) = listener.accept().await.unwrap();
            serve(
                socket,
                config,
                peer.ip(),
                TrustedProxySet::default(),
                permit,
                |mut stream, _client_ip| async move {
                    let mut byte = [0; 1];
                    let _ = stream.read(&mut byte).await;
                    ApplicationClose::normal("test_complete")
                },
            )
            .await
        });

        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(
                b"GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = [0; 1024];
        let response_len = timeout(Duration::from_secs(1), client.read(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(
            std::str::from_utf8(&response[..response_len])
                .unwrap()
                .starts_with("HTTP/1.1 101")
        );
        assert_eq!(slots.available_permits(), 1);

        drop(client);
        let _ = server.await.unwrap();
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
        let (_handler_close_tx, mut handler_close_rx) = oneshot::channel();
        let adapter_task = tokio::spawn(async move {
            let _adapter_reader = adapter_reader;
            run_adapter_loop(
                server_websocket,
                &mut adapter_writer,
                &mut outbound_rx,
                &mut handler_close_rx,
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
