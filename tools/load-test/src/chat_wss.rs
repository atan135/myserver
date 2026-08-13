use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use prost::Message;
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use crate::chat_pb::{ChatAuthReq, ChatAuthRes, ChatGroupReq, ChatHistoryReq, ChatPrivateReq};
use crate::config::EnvironmentKind;
use crate::game_kcp::ReconnectPolicy;
use crate::side_services::{
    PlannedSideServiceStep, ServiceDescriptor, SideOutcomeCategory, SideServiceKind,
    SideServiceOperation,
};

pub const CHAT_PACKET_HEADER_LEN: usize = 14;
pub const CHAT_MAX_BODY_BYTES: usize = 4 * 1024;
pub const CHAT_MAX_OUTBOUND_QUEUE: usize = 16;

const CHAT_AUTH_REQ: u16 = 20_001;
const CHAT_AUTH_RES: u16 = 20_002;
const CHAT_PRIVATE_REQ: u16 = 20_101;
const CHAT_PRIVATE_RES: u16 = 20_102;
const CHAT_GROUP_REQ: u16 = 20_103;
const CHAT_GROUP_RES: u16 = 20_104;
const CHAT_PUSH: u16 = 20_105;
const CHAT_HISTORY_REQ: u16 = 20_211;
const CHAT_HISTORY_RES: u16 = 20_212;
const MAIL_NOTIFY_PUSH: u16 = 20_301;
const ERROR_RES: u16 = 9_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatConnectionState {
    Disconnected,
    Connected,
    Authenticated,
    Backpressured,
    Closed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatOperation {
    Authenticate,
    PrivateMessage,
    GroupMessage,
    History,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatPushKind {
    Chat,
    MailNotification,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatWssError {
    LiveTransportForbidden,
    LiveTransportNotEnabled,
    DescriptorRejected,
    InvalidState,
    PacketMalformed,
    PacketTooLarge,
    BinaryFrameRequired,
    ResponseTypeMismatch,
    SequenceMismatch,
    AuthenticationRejected,
    SlowConsumer,
    Transport(String),
}

impl std::fmt::Display for ChatWssError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatWssMetrics {
    pub handshake_attempts: u64,
    pub handshake_successes: u64,
    pub auth_attempts: u64,
    pub active_connections: u64,
    pub messages_sent: u64,
    pub pushes_received: u64,
    pub push_duplicates: u64,
    pub push_out_of_order: u64,
    pub queue_backlog: u64,
    pub slow_consumer_disconnects: u64,
    pub disconnects: u64,
    pub reconnects: u64,
    pub reconnect_backoff_ms: u64,
    pub outcomes: BTreeMap<SideOutcomeCategory, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatPacket {
    pub message_type: u16,
    pub sequence: u32,
    pub body: Vec<u8>,
}

impl ChatPacket {
    pub fn encode(&self) -> Result<Vec<u8>, ChatWssError> {
        if self.body.len() > CHAT_MAX_BODY_BYTES {
            return Err(ChatWssError::PacketTooLarge);
        }
        let mut bytes = Vec::with_capacity(CHAT_PACKET_HEADER_LEN + self.body.len());
        bytes.extend_from_slice(&0xCAFE_u16.to_be_bytes());
        bytes.extend_from_slice(&[1, 0]);
        bytes.extend_from_slice(&self.message_type.to_be_bytes());
        bytes.extend_from_slice(&self.sequence.to_be_bytes());
        bytes.extend_from_slice(&(self.body.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&self.body);
        Ok(bytes)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ChatWssError> {
        if bytes.len() < CHAT_PACKET_HEADER_LEN {
            return Err(ChatWssError::PacketMalformed);
        }
        if bytes[..2] != 0xCAFE_u16.to_be_bytes() || bytes[2] != 1 || bytes[3] != 0 {
            return Err(ChatWssError::PacketMalformed);
        }
        let message_type = u16::from_be_bytes([bytes[4], bytes[5]]);
        let sequence = u32::from_be_bytes([bytes[6], bytes[7], bytes[8], bytes[9]]);
        let body_len = u32::from_be_bytes([bytes[10], bytes[11], bytes[12], bytes[13]]) as usize;
        if body_len > CHAT_MAX_BODY_BYTES || bytes.len() != CHAT_PACKET_HEADER_LEN + body_len {
            return Err(ChatWssError::PacketTooLarge);
        }
        Ok(Self {
            message_type,
            sequence,
            body: bytes[CHAT_PACKET_HEADER_LEN..].to_vec(),
        })
    }
}

#[derive(Debug)]
pub struct ChatWssSession {
    state: ChatConnectionState,
    next_sequence: u32,
    pending: BTreeMap<u32, u16>,
    outbound: VecDeque<ChatPacket>,
    last_push_sequence: Option<u32>,
    pub metrics: ChatWssMetrics,
    reconnect_policy: ReconnectPolicy,
}

impl ChatWssSession {
    pub fn new(reconnect_policy: ReconnectPolicy) -> Result<Self, ChatWssError> {
        reconnect_policy
            .validate()
            .map_err(|_| ChatWssError::InvalidState)?;
        Ok(Self {
            state: ChatConnectionState::Disconnected,
            next_sequence: 1,
            pending: BTreeMap::new(),
            outbound: VecDeque::new(),
            last_push_sequence: None,
            metrics: ChatWssMetrics::default(),
            reconnect_policy,
        })
    }
    pub fn state(&self) -> ChatConnectionState {
        self.state
    }
    pub fn connected(&mut self) {
        self.state = ChatConnectionState::Connected;
        self.metrics.handshake_attempts += 1;
        self.metrics.handshake_successes += 1;
        self.metrics.active_connections = 1;
    }
    pub fn disconnected(&mut self, attempt: u32, jitter: u64) -> Result<u64, ChatWssError> {
        self.state = ChatConnectionState::Disconnected;
        self.metrics.active_connections = 0;
        self.metrics.disconnects += 1;
        let delay = self
            .reconnect_policy
            .delay_for(attempt, jitter)
            .map_err(|_| ChatWssError::InvalidState)?;
        self.metrics.reconnects += 1;
        self.metrics.reconnect_backoff_ms += delay;
        Ok(delay)
    }
    fn prepare_reconnect(&mut self) {
        self.state = ChatConnectionState::Disconnected;
        self.pending.clear();
        self.outbound.clear();
        self.last_push_sequence = None;
    }
    /// Chat-server derives the authenticated player exclusively from the
    /// ticket. `ChatAuthReq.player_id` remains in the shared compatibility
    /// proto but must never be populated by the load generator.
    pub fn queue_auth(&mut self, token: String) -> Result<u32, ChatWssError> {
        self.queue(
            CHAT_AUTH_REQ,
            ChatAuthReq {
                player_id: String::new(),
                token,
            }
            .encode_to_vec(),
            Some(CHAT_AUTH_RES),
        )
    }
    pub fn queue_private(
        &mut self,
        target_id: String,
        content: String,
    ) -> Result<u32, ChatWssError> {
        self.require_authenticated()?;
        self.queue(
            CHAT_PRIVATE_REQ,
            ChatPrivateReq { target_id, content }.encode_to_vec(),
            Some(CHAT_PRIVATE_RES),
        )
    }
    pub fn queue_group(&mut self, group_id: String, content: String) -> Result<u32, ChatWssError> {
        self.require_authenticated()?;
        self.queue(
            CHAT_GROUP_REQ,
            ChatGroupReq { group_id, content }.encode_to_vec(),
            Some(CHAT_GROUP_RES),
        )
    }
    pub fn queue_history(
        &mut self,
        chat_type: i32,
        target_id: String,
        before_time: i64,
        limit: i32,
    ) -> Result<u32, ChatWssError> {
        self.require_authenticated()?;
        self.queue(
            CHAT_HISTORY_REQ,
            ChatHistoryReq {
                chat_type,
                target_id,
                before_time,
                limit,
            }
            .encode_to_vec(),
            Some(CHAT_HISTORY_RES),
        )
    }
    fn require_authenticated(&self) -> Result<(), ChatWssError> {
        (self.state == ChatConnectionState::Authenticated)
            .then_some(())
            .ok_or(ChatWssError::InvalidState)
    }
    fn queue(
        &mut self,
        request_type: u16,
        body: Vec<u8>,
        expected: Option<u16>,
    ) -> Result<u32, ChatWssError> {
        if self.outbound.len() >= CHAT_MAX_OUTBOUND_QUEUE {
            self.state = ChatConnectionState::Backpressured;
            self.metrics.queue_backlog += 1;
            self.metrics.slow_consumer_disconnects += 1;
            return Err(ChatWssError::SlowConsumer);
        }
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.outbound.push_back(ChatPacket {
            message_type: request_type,
            sequence,
            body,
        });
        if let Some(expected) = expected {
            self.pending.insert(sequence, expected);
        }
        self.metrics.messages_sent += 1;
        if request_type == CHAT_AUTH_REQ {
            self.metrics.auth_attempts += 1;
        }
        Ok(sequence)
    }
    pub fn pop_outbound(&mut self) -> Option<ChatPacket> {
        self.outbound.pop_front()
    }
    pub fn handle_inbound(
        &mut self,
        packet: ChatPacket,
    ) -> Result<Option<ChatPushKind>, ChatWssError> {
        if matches!(packet.message_type, CHAT_PUSH | MAIL_NOTIFY_PUSH) {
            if self
                .last_push_sequence
                .is_some_and(|last| packet.sequence == last)
            {
                self.metrics.push_duplicates += 1;
                return Ok(None);
            }
            if self
                .last_push_sequence
                .is_some_and(|last| packet.sequence < last)
            {
                self.metrics.push_out_of_order += 1;
                return Ok(None);
            }
            self.last_push_sequence = Some(packet.sequence);
            self.metrics.pushes_received += 1;
            return Ok(Some(if packet.message_type == CHAT_PUSH {
                ChatPushKind::Chat
            } else {
                ChatPushKind::MailNotification
            }));
        }
        let expected = self
            .pending
            .remove(&packet.sequence)
            .ok_or(ChatWssError::SequenceMismatch)?;
        if packet.message_type == ERROR_RES {
            *self
                .metrics
                .outcomes
                .entry(SideOutcomeCategory::BusinessError)
                .or_default() += 1;
            return Ok(None);
        }
        if packet.message_type != expected {
            return Err(ChatWssError::ResponseTypeMismatch);
        }
        if packet.message_type == CHAT_AUTH_RES {
            let response = ChatAuthRes::decode(packet.body.as_slice())
                .map_err(|_| ChatWssError::PacketMalformed)?;
            if !response.ok {
                return Err(ChatWssError::AuthenticationRejected);
            }
            self.state = ChatConnectionState::Authenticated;
        }
        *self
            .metrics
            .outcomes
            .entry(SideOutcomeCategory::Success)
            .or_default() += 1;
        Ok(None)
    }

    pub fn merge_into_metrics(&self, metrics: &mut crate::metrics::Metrics) {
        self.metrics.merge_into_metrics(metrics);
    }
}

impl ChatWssMetrics {
    pub fn merge_into_metrics(&self, metrics: &mut crate::metrics::Metrics) {
        for (key, value) in [
            ("chat_wss_handshakes", self.handshake_attempts),
            ("chat_wss_handshake_successes", self.handshake_successes),
            ("chat_wss_auth_attempts", self.auth_attempts),
            ("chat_wss_active_connections", self.active_connections),
            ("chat_wss_messages_sent", self.messages_sent),
            ("chat_wss_pushes_received", self.pushes_received),
            ("chat_wss_push_duplicates", self.push_duplicates),
            ("chat_wss_push_out_of_order", self.push_out_of_order),
            ("chat_wss_queue_backlog", self.queue_backlog),
            (
                "chat_wss_slow_consumer_disconnects",
                self.slow_consumer_disconnects,
            ),
            ("chat_wss_disconnects", self.disconnects),
            ("chat_wss_reconnects", self.reconnects),
            ("chat_wss_reconnect_backoff_ms", self.reconnect_backoff_ms),
        ] {
            metrics.increment(key, value);
        }
    }
}

pub struct LiveChatWssTransport {
    websocket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

/// Runs a bounded live chat flow for one authenticated account. The caller
/// owns admission, deadlines, and the ticket lifecycle; this function only
/// performs the explicitly planned WebSocket operations.
pub async fn execute_live_chat_steps(
    descriptor: &ServiceDescriptor,
    environment: EnvironmentKind,
    live_websocket: bool,
    ticket: String,
    player_id: &str,
    steps: &[PlannedSideServiceStep],
    timeout_ms: u64,
    reconnect_policy: ReconnectPolicy,
) -> Result<ChatWssMetrics, ChatWssError> {
    if steps.is_empty() {
        return Ok(ChatWssMetrics::default());
    }
    if steps
        .iter()
        .any(|step| step.service != SideServiceKind::Chat)
    {
        return Err(ChatWssError::InvalidState);
    }
    let mut session = ChatWssSession::new(reconnect_policy)?;
    let mut transport =
        LiveChatWssTransport::connect(descriptor, environment, live_websocket, timeout_ms).await?;
    session.connected();
    let auth = session.queue_auth(ticket.clone())?;
    drive_chat_request(&mut session, &mut transport, auth, timeout_ms).await?;

    for step in steps {
        let mut request = match step.operation {
            SideServiceOperation::ChatAuth => continue,
            SideServiceOperation::ChatPrivate => session.queue_private(
                format!("{player_id}-loadtest-peer"),
                "loadtest private message".into(),
            )?,
            SideServiceOperation::ChatGroup => session.queue_group(
                format!("{player_id}-loadtest-group"),
                "loadtest group message".into(),
            )?,
            SideServiceOperation::ChatHistory => {
                session.queue_history(1, format!("{player_id}-loadtest-peer"), 0, 20)?
            }
            _ => return Err(ChatWssError::InvalidState),
        };
        let mut attempt = 0;
        loop {
            match drive_chat_request(&mut session, &mut transport, request, timeout_ms).await {
                Ok(()) => break,
                Err(error)
                    if is_reconnectable(&error) && attempt < reconnect_policy.max_attempts =>
                {
                    attempt += 1;
                    let delay = session.disconnected(attempt, attempt as u64)?;
                    session.prepare_reconnect();
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    transport = LiveChatWssTransport::connect(
                        descriptor,
                        environment,
                        live_websocket,
                        timeout_ms,
                    )
                    .await?;
                    session.connected();
                    let auth = session.queue_auth(ticket.clone())?;
                    drive_chat_request(&mut session, &mut transport, auth, timeout_ms).await?;
                    request = match step.operation {
                        SideServiceOperation::ChatPrivate => session.queue_private(
                            format!("{player_id}-loadtest-peer"),
                            "loadtest private message".into(),
                        )?,
                        SideServiceOperation::ChatGroup => session.queue_group(
                            format!("{player_id}-loadtest-group"),
                            "loadtest group message".into(),
                        )?,
                        SideServiceOperation::ChatHistory => {
                            session.queue_history(1, format!("{player_id}-loadtest-peer"), 0, 20)?
                        }
                        SideServiceOperation::ChatAuth => continue,
                        _ => return Err(ChatWssError::InvalidState),
                    };
                }
                Err(error) => return Err(error),
            }
        }
    }
    Ok(session.metrics)
}

fn is_reconnectable(error: &ChatWssError) -> bool {
    matches!(
        error,
        ChatWssError::Transport(reason)
            if matches!(reason.as_str(), "closed" | "read_failed" | "write_failed")
    )
}

async fn drive_chat_request(
    session: &mut ChatWssSession,
    transport: &mut LiveChatWssTransport,
    sequence: u32,
    timeout_ms: u64,
) -> Result<(), ChatWssError> {
    let packet = session.pop_outbound().ok_or(ChatWssError::InvalidState)?;
    if packet.sequence != sequence {
        return Err(ChatWssError::SequenceMismatch);
    }
    transport.send(packet, timeout_ms).await?;
    loop {
        let response = transport.receive(timeout_ms).await?;
        let is_response = response.sequence == sequence
            && !matches!(response.message_type, CHAT_PUSH | MAIL_NOTIFY_PUSH);
        session.handle_inbound(response)?;
        if is_response {
            return Ok(());
        }
    }
}

impl LiveChatWssTransport {
    pub async fn connect(
        descriptor: &ServiceDescriptor,
        environment: EnvironmentKind,
        live_websocket: bool,
        timeout_ms: u64,
    ) -> Result<Self, ChatWssError> {
        if !matches!(environment, EnvironmentKind::Local | EnvironmentKind::Test) {
            return Err(ChatWssError::LiveTransportForbidden);
        }
        if !live_websocket {
            return Err(ChatWssError::LiveTransportNotEnabled);
        }
        descriptor
            .validate(SideServiceKind::Chat)
            .map_err(|_| ChatWssError::DescriptorRejected)?;
        let url = chat_websocket_url(descriptor)?;
        let (websocket, _) = timeout(Duration::from_millis(timeout_ms.max(1)), connect_async(url))
            .await
            .map_err(|_| ChatWssError::Transport("handshake_timeout".into()))?
            .map_err(|_| ChatWssError::Transport("handshake_failed".into()))?;
        Ok(Self { websocket })
    }
    pub async fn send(&mut self, packet: ChatPacket, timeout_ms: u64) -> Result<(), ChatWssError> {
        let bytes = packet.encode()?;
        timeout(
            Duration::from_millis(timeout_ms.max(1)),
            self.websocket.send(WsMessage::Binary(bytes.into())),
        )
        .await
        .map_err(|_| ChatWssError::Transport("write_timeout".into()))?
        .map_err(|_| ChatWssError::Transport("write_failed".into()))
    }
    pub async fn receive(&mut self, timeout_ms: u64) -> Result<ChatPacket, ChatWssError> {
        let message = timeout(
            Duration::from_millis(timeout_ms.max(1)),
            self.websocket.next(),
        )
        .await
        .map_err(|_| ChatWssError::Transport("read_timeout".into()))?
        .ok_or_else(|| ChatWssError::Transport("closed".into()))?
        .map_err(|_| ChatWssError::Transport("read_failed".into()))?;
        match message {
            WsMessage::Binary(bytes) => ChatPacket::decode(&bytes),
            _ => Err(ChatWssError::BinaryFrameRequired),
        }
    }
}

fn chat_websocket_url(descriptor: &ServiceDescriptor) -> Result<String, ChatWssError> {
    let scheme = match descriptor.protocol {
        crate::side_services::SideTransportKind::Ws => "ws",
        crate::side_services::SideTransportKind::Wss => "wss",
        _ => return Err(ChatWssError::DescriptorRejected),
    };
    Ok(format!(
        "{scheme}://{}:{}",
        descriptor.host, descriptor.port
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> ReconnectPolicy {
        ReconnectPolicy {
            max_attempts: 2,
            base_delay_ms: 10,
            max_delay_ms: 50,
            max_jitter_ms: 0,
        }
    }
    #[test]
    fn packet_framing_and_auth_chat_history_flow_are_bounded() {
        let mut session = ChatWssSession::new(policy()).unwrap();
        session.connected();
        let auth = session.queue_auth("secret".into()).unwrap();
        let packet = session.pop_outbound().unwrap();
        assert_eq!(
            ChatPacket::decode(&packet.encode().unwrap())
                .unwrap()
                .message_type,
            CHAT_AUTH_REQ
        );
        assert_eq!(
            ChatAuthReq::decode(packet.body.as_slice()).unwrap(),
            ChatAuthReq {
                player_id: String::new(),
                token: "secret".into(),
            }
        );
        session
            .handle_inbound(ChatPacket {
                message_type: CHAT_AUTH_RES,
                sequence: auth,
                body: ChatAuthRes {
                    ok: true,
                    error_code: String::new(),
                }
                .encode_to_vec(),
            })
            .unwrap();
        assert_eq!(session.state(), ChatConnectionState::Authenticated);
        session.queue_private("p2".into(), "hello".into()).unwrap();
        session.queue_group("g".into(), "hello".into()).unwrap();
        session.queue_history(1, "p2".into(), 0, 10).unwrap();
    }
    #[test]
    fn pushes_backpressure_and_reconnect_are_classified() {
        let mut session = ChatWssSession::new(policy()).unwrap();
        session.connected();
        session
            .handle_inbound(ChatPacket {
                message_type: CHAT_PUSH,
                sequence: 2,
                body: vec![],
            })
            .unwrap();
        session
            .handle_inbound(ChatPacket {
                message_type: CHAT_PUSH,
                sequence: 2,
                body: vec![],
            })
            .unwrap();
        assert_eq!(session.metrics.push_duplicates, 1);
        for _ in 0..CHAT_MAX_OUTBOUND_QUEUE {
            session.queue_auth("t".into()).unwrap();
        }
        assert_eq!(
            session.queue_auth("t".into()).unwrap_err(),
            ChatWssError::SlowConsumer
        );
        assert_eq!(session.disconnected(1, 0).unwrap(), 10);
    }
    #[test]
    fn live_wss_is_rejected_outside_local_or_test() {
        let descriptor = ServiceDescriptor {
            host: "chat.example".into(),
            port: 443,
            protocol: crate::side_services::SideTransportKind::Wss,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let result = runtime.block_on(LiveChatWssTransport::connect(
            &descriptor,
            EnvironmentKind::Production,
            true,
            1,
        ));
        assert!(matches!(result, Err(ChatWssError::LiveTransportForbidden)));
    }

    #[test]
    fn live_websocket_requires_the_explicit_diagnostic_gate() {
        let descriptor = ServiceDescriptor {
            host: "127.0.0.1".into(),
            port: 9011,
            protocol: crate::side_services::SideTransportKind::Ws,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let result = runtime.block_on(LiveChatWssTransport::connect(
            &descriptor,
            EnvironmentKind::Local,
            false,
            1,
        ));
        assert!(matches!(result, Err(ChatWssError::LiveTransportNotEnabled)));
    }

    #[test]
    fn local_plain_ws_descriptor_builds_a_plain_websocket_url() {
        let descriptor = ServiceDescriptor {
            host: "127.0.0.1".into(),
            port: 9011,
            protocol: crate::side_services::SideTransportKind::Ws,
        };
        descriptor.validate(SideServiceKind::Chat).unwrap();
        assert_eq!(
            chat_websocket_url(&descriptor).unwrap(),
            "ws://127.0.0.1:9011"
        );
    }
}
