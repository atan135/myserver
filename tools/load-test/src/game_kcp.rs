//! KCP player-ingress endpoint guards.
//!
//! There is no TCP fallback. The connector repeats the configuration access
//! gate immediately before DNS and KCP setup, and player frames are read only
//! through the shared protocol crate.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use game_protocol::{HEADER_LEN, MessageType, Packet, player_kcp_config, read_packet};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio_kcp::KcpStream;

use crate::config::{ConfigError, Endpoint, LoadTestConfig, RunAccess};
use crate::pb::{
    AuthReq, AuthRes, MatchCancelReq, MatchEventStreamReq, MatchStartReq, MatchStatusReq, PingReq,
    PingRes,
};
use crate::protocol_version_policy::{
    CLIENT_PROTOCOL_VERSION_TOO_NEW, CLIENT_PROTOCOL_VERSION_TOO_OLD,
    CURRENT_CLIENT_PROTOCOL_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameProxyEndpoint {
    endpoint: Endpoint,
}

impl GameProxyEndpoint {
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        let endpoint = Endpoint::parse(input)?;
        if endpoint.scheme != "kcp" {
            return Err(ConfigError::Rejected(
                "game_proxy must use kcp; TCP fallback is diagnostic-only".into(),
            ));
        }
        if endpoint.port == 7000
            || endpoint.host == "game-server"
            || endpoint.host.ends_with(".game-server")
        {
            return Err(ConfigError::Rejected(
                "player load must not target game-server directly".into(),
            ));
        }

        Ok(Self { endpoint })
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }
}

#[derive(Debug, Default)]
pub struct KcpConnector;

impl KcpConnector {
    /// Connect only to the configured KCP player ingress after repeating its
    /// access gate. This method intentionally has no TCP alternative.
    pub async fn connect(
        &self,
        config: &LoadTestConfig,
        access: RunAccess<'_>,
        endpoint: &GameProxyEndpoint,
    ) -> Result<KcpStream, GameKcpError> {
        config.validate_structural()?;
        config.validate_access(access)?;
        if GameProxyEndpoint::parse(&config.targets.game_proxy)? != *endpoint {
            return Err(GameKcpError::EndpointMismatch);
        }

        let addresses: Vec<SocketAddr> =
            tokio::net::lookup_host((endpoint.endpoint.host.as_str(), endpoint.endpoint.port))
                .await
                .map_err(|source| GameKcpError::Resolve {
                    host: endpoint.endpoint.host.clone(),
                    source,
                })?
                .collect();
        let address = validate_resolved_address(config, endpoint, &addresses)?;
        KcpStream::connect(&player_kcp_config(), address)
            .await
            .map_err(|source| GameKcpError::Connect(std::io::Error::other(source)))
    }
}

fn validate_resolved_address(
    config: &LoadTestConfig,
    endpoint: &GameProxyEndpoint,
    addresses: &[SocketAddr],
) -> Result<SocketAddr, GameKcpError> {
    let permitted = |address: &SocketAddr| {
        if config.environment.kind.is_remote() {
            config.environment.allowed_ips.contains(&address.ip())
        } else {
            address.ip().is_loopback()
        }
    };
    if addresses.is_empty() || addresses.iter().any(|address| !permitted(address)) {
        return Err(GameKcpError::ResolvedAddressRejected {
            host: endpoint.endpoint.host.clone(),
        });
    }
    Ok(addresses[0])
}

pub const PLAYER_PACKET_HEADER_LEN: usize = HEADER_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KcpSessionEvent {
    Response { message_type: MessageType, seq: u32 },
    Push { message_type: MessageType, seq: u32 },
    Disconnected,
}

#[derive(Debug)]
pub struct KcpSession {
    max_body_len: usize,
    expected_responses: BTreeMap<(u16, u32), ()>,
    push_counts: BTreeMap<u16, u64>,
}

impl KcpSession {
    pub fn new(max_body_len: usize) -> Result<Self, GameKcpError> {
        if max_body_len == 0 {
            return Err(GameKcpError::InvalidBodyLimit);
        }
        Ok(Self {
            max_body_len,
            expected_responses: BTreeMap::new(),
            push_counts: BTreeMap::new(),
        })
    }

    /// Register the exact `(expected response message type, sequence)` pair.
    /// Callers must remove the entry if an outbound KCP write subsequently
    /// fails before the request has left the generator.
    pub fn begin_request(
        &mut self,
        expected_response: MessageType,
        seq: u32,
    ) -> Result<(), GameKcpError> {
        let key = (expected_response as u16, seq);
        if self.expected_responses.insert(key, ()).is_some() {
            return Err(GameKcpError::DuplicateRequest {
                message_type: expected_response as u16,
                seq,
            });
        }
        Ok(())
    }

    pub fn cancel_request(&mut self, expected_response: MessageType, seq: u32) -> bool {
        self.expected_responses
            .remove(&(expected_response as u16, seq))
            .is_some()
    }

    pub fn pending_requests(&self) -> usize {
        self.expected_responses.len()
    }

    pub fn clear_requests(&mut self) {
        self.expected_responses.clear();
    }

    pub fn push_count(&self, message_type: MessageType) -> u64 {
        self.push_counts
            .get(&(message_type as u16))
            .copied()
            .unwrap_or_default()
    }

    pub fn ingest(&mut self, packet: Packet) -> Result<KcpSessionEvent, GameKcpError> {
        let message_type = validate_packet(&packet, self.max_body_len)?;
        if is_push(message_type) {
            *self.push_counts.entry(message_type as u16).or_default() += 1;
            return Ok(KcpSessionEvent::Push {
                message_type,
                seq: packet.header.seq,
            });
        }

        let key = (message_type as u16, packet.header.seq);
        if self.expected_responses.remove(&key).is_none() {
            return Err(GameKcpError::UnexpectedResponse {
                message_type: message_type as u16,
                seq: packet.header.seq,
            });
        }
        Ok(KcpSessionEvent::Response {
            message_type,
            seq: packet.header.seq,
        })
    }

    /// Reads the shared 14-byte player frame, retaining its framing checks and
    /// mapping clean EOF to a disconnected session.
    pub async fn read_event<R>(&mut self, reader: &mut R) -> Result<KcpSessionEvent, GameKcpError>
    where
        R: AsyncRead + Unpin,
    {
        match read_packet(reader, self.max_body_len).await? {
            Some(packet) => self.ingest(packet),
            None => Ok(KcpSessionEvent::Disconnected),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameConnectionState {
    Disconnected,
    Connecting,
    AwaitingAuth,
    Authenticated,
    Reconnecting,
    Closed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuthRejectReason {
    InvalidTicket,
    ExpiredTicket,
    ProtocolVersion,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub max_jitter_ms: u64,
}

impl ReconnectPolicy {
    pub fn validate(self) -> Result<(), GameKcpError> {
        if self.max_attempts == 0
            || self.base_delay_ms == 0
            || self.max_delay_ms < self.base_delay_ms
        {
            return Err(GameKcpError::InvalidReconnectPolicy);
        }
        Ok(())
    }

    pub fn delay_for(self, attempt: u32, jitter_sample: u64) -> Result<u64, GameKcpError> {
        self.validate()?;
        if attempt == 0 || attempt > self.max_attempts {
            return Err(GameKcpError::ReconnectAttemptOutOfRange { attempt });
        }
        let exponent = attempt.saturating_sub(1).min(63);
        let exponential = self.base_delay_ms.saturating_mul(1_u64 << exponent);
        let base = exponential.min(self.max_delay_ms);
        let spread = self.max_jitter_ms.saturating_mul(2).saturating_add(1);
        let jitter = if spread == 0 {
            0
        } else {
            jitter_sample % spread
        };
        Ok(base
            .saturating_sub(self.max_jitter_ms)
            .saturating_add(jitter)
            .min(self.max_delay_ms))
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OutboundPacket {
    message_type: MessageType,
    expected_response: MessageType,
    seq: u32,
    bytes: Vec<u8>,
}

impl std::fmt::Debug for OutboundPacket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutboundPacket")
            .field("message_type", &self.message_type)
            .field("seq", &self.seq)
            .finish_non_exhaustive()
    }
}

impl OutboundPacket {
    fn new(
        message_type: MessageType,
        expected_response: MessageType,
        seq: u32,
        body: Vec<u8>,
    ) -> Self {
        Self {
            message_type,
            expected_response,
            seq,
            bytes: game_protocol::encode_packet(message_type, seq, &body),
        }
    }

    pub fn message_type(&self) -> MessageType {
        self.message_type
    }

    pub fn seq(&self) -> u32 {
        self.seq
    }

    pub fn body(&self) -> &[u8] {
        &self.bytes[game_protocol::HEADER_LEN..]
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    pub async fn write_to<W>(&self, writer: &mut W) -> Result<(), GameKcpError>
    where
        W: AsyncWrite + Unpin,
    {
        writer.write_all(&self.bytes).await?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameLifecycleEvent {
    Connecting,
    Authenticated,
    AuthRejected { reason: AuthRejectReason },
    HeartbeatAcknowledged,
    Response { message_type: MessageType, seq: u32 },
    Push { message_type: MessageType, seq: u32 },
    ReconnectScheduled { attempt: u32, delay_ms: u64 },
    LateResponseDropped { message_type: MessageType, seq: u32 },
    Closed,
    Failed,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GameConnectionMetrics {
    pub reconnect_attempts: u32,
    pub reconnect_delay_total_ms: u64,
    pub auth_invalid_ticket: u64,
    pub auth_expired_ticket: u64,
    pub auth_protocol_version: u64,
    pub auth_other_rejection: u64,
}

#[derive(Debug)]
pub struct GameConnectionLifecycle {
    session: KcpSession,
    state: GameConnectionState,
    next_seq: u32,
    reconnect_policy: ReconnectPolicy,
    metrics: GameConnectionMetrics,
}

impl GameConnectionLifecycle {
    pub fn new(
        max_body_len: usize,
        reconnect_policy: ReconnectPolicy,
    ) -> Result<Self, GameKcpError> {
        reconnect_policy.validate()?;
        Ok(Self {
            session: KcpSession::new(max_body_len)?,
            state: GameConnectionState::Disconnected,
            next_seq: 1,
            reconnect_policy,
            metrics: GameConnectionMetrics::default(),
        })
    }

    pub fn state(&self) -> GameConnectionState {
        self.state
    }

    pub fn metrics(&self) -> GameConnectionMetrics {
        self.metrics
    }

    pub fn pending_requests(&self) -> usize {
        self.session.pending_requests()
    }

    pub fn begin_connect(&mut self) -> Result<GameLifecycleEvent, GameKcpError> {
        if !matches!(
            self.state,
            GameConnectionState::Disconnected | GameConnectionState::Reconnecting
        ) {
            return Err(GameKcpError::InvalidLifecycleState {
                operation: "begin_connect",
                state: self.state,
            });
        }
        self.state = GameConnectionState::Connecting;
        Ok(GameLifecycleEvent::Connecting)
    }

    /// Builds an AuthReq from the ephemeral ticket input. The lifecycle never
    /// stores the ticket, its encoded bytes, or server rejection text.
    pub fn begin_auth(&mut self, ticket: &str) -> Result<OutboundPacket, GameKcpError> {
        self.begin_auth_with_protocol_version(ticket, CURRENT_CLIENT_PROTOCOL_VERSION)
    }

    /// An explicit version is reserved for the compatibility scenario. Normal
    /// player flows use `begin_auth`, which follows the shared current policy.
    pub fn begin_auth_with_protocol_version(
        &mut self,
        ticket: &str,
        client_protocol_version: u32,
    ) -> Result<OutboundPacket, GameKcpError> {
        self.require_state("begin_auth", GameConnectionState::Connecting)?;
        let seq = self.allocate_seq()?;
        self.session.begin_request(MessageType::AuthRes, seq)?;
        self.state = GameConnectionState::AwaitingAuth;
        Ok(OutboundPacket::new(
            MessageType::AuthReq,
            MessageType::AuthRes,
            seq,
            game_protocol::encode_body(&AuthReq {
                ticket: ticket.to_owned(),
                client_protocol_version,
            }),
        ))
    }

    pub fn begin_heartbeat(&mut self, client_time: i64) -> Result<OutboundPacket, GameKcpError> {
        self.require_state("begin_heartbeat", GameConnectionState::Authenticated)?;
        let seq = self.allocate_seq()?;
        self.session.begin_request(MessageType::PingRes, seq)?;
        Ok(OutboundPacket::new(
            MessageType::PingReq,
            MessageType::PingRes,
            seq,
            game_protocol::encode_body(&PingReq { client_time }),
        ))
    }

    /// Registers an already-validated player request after KCP auth. Sequence
    /// allocation and exact response correlation remain owned here rather
    /// than by a gameplay profile or the live runner.
    pub fn begin_gameplay_request(
        &mut self,
        request_type: MessageType,
        expected_response: MessageType,
        body: &[u8],
    ) -> Result<OutboundPacket, GameKcpError> {
        self.require_state("begin_gameplay_request", GameConnectionState::Authenticated)?;
        let seq = self.allocate_seq()?;
        self.session.begin_request(expected_response, seq)?;
        Ok(OutboundPacket::new(
            request_type,
            expected_response,
            seq,
            body.to_vec(),
        ))
    }

    pub fn begin_match_start(&mut self, mode: &str) -> Result<OutboundPacket, GameKcpError> {
        self.begin_gameplay_request(
            MessageType::MatchStartReq,
            MessageType::MatchStartRes,
            &game_protocol::encode_body(&MatchStartReq {
                mode: mode.to_string(),
                rank_tier: 0,
            }),
        )
    }

    pub fn begin_match_cancel(&mut self, match_id: &str) -> Result<OutboundPacket, GameKcpError> {
        self.begin_gameplay_request(
            MessageType::MatchCancelReq,
            MessageType::MatchCancelRes,
            &game_protocol::encode_body(&MatchCancelReq {
                match_id: match_id.to_string(),
            }),
        )
    }

    pub fn begin_match_status(&mut self) -> Result<OutboundPacket, GameKcpError> {
        self.begin_gameplay_request(
            MessageType::MatchStatusReq,
            MessageType::MatchStatusRes,
            &game_protocol::encode_body(&MatchStatusReq {}),
        )
    }

    pub fn begin_match_event_stream(&mut self) -> Result<OutboundPacket, GameKcpError> {
        self.begin_gameplay_request(
            MessageType::MatchEventStreamReq,
            MessageType::MatchEventStreamRes,
            &game_protocol::encode_body(&MatchEventStreamReq {}),
        )
    }

    /// Removes the response expectation created for a packet that could not
    /// be written, then handles the failed transport as a disconnect.
    pub fn handle_outbound_write_failure(
        &mut self,
        outbound: &OutboundPacket,
        jitter_sample: u64,
    ) -> Result<GameLifecycleEvent, GameKcpError> {
        self.cancel_outbound_request(outbound)?;
        self.handle_disconnected(jitter_sample)
    }

    /// Handles a deadline expiry after a request was successfully written but
    /// before its exact response arrived. The old expectation is removed so a
    /// delayed response cannot be correlated with a subsequent reconnect.
    pub fn handle_request_timeout(
        &mut self,
        outbound: &OutboundPacket,
        jitter_sample: u64,
    ) -> Result<GameLifecycleEvent, GameKcpError> {
        self.cancel_outbound_request(outbound)?;
        self.handle_disconnected(jitter_sample)
    }

    pub fn handle_packet(&mut self, packet: Packet) -> Result<GameLifecycleEvent, GameKcpError> {
        let message_type = validate_packet(&packet, self.session.max_body_len)?;
        match message_type {
            MessageType::AuthRes | MessageType::PingRes
                if matches!(
                    self.state,
                    GameConnectionState::Connecting | GameConnectionState::Reconnecting
                ) =>
            {
                Ok(GameLifecycleEvent::LateResponseDropped {
                    message_type,
                    seq: packet.header.seq,
                })
            }
            MessageType::AuthRes => self.handle_auth_response(packet),
            MessageType::PingRes => self.handle_ping_response(packet),
            _ if is_push(message_type) => match self.session.ingest(packet)? {
                KcpSessionEvent::Push { message_type, seq } => {
                    Ok(GameLifecycleEvent::Push { message_type, seq })
                }
                _ => unreachable!("known push must be counted as a push"),
            },
            _ => match self.session.ingest(packet)? {
                KcpSessionEvent::Response { message_type, seq } => {
                    Ok(GameLifecycleEvent::Response { message_type, seq })
                }
                _ => unreachable!("non-push packets must be exact responses"),
            },
        }
    }

    pub fn handle_disconnected(
        &mut self,
        jitter_sample: u64,
    ) -> Result<GameLifecycleEvent, GameKcpError> {
        self.session.clear_requests();
        if self.state == GameConnectionState::Closed {
            return Ok(GameLifecycleEvent::Closed);
        }
        if self.state == GameConnectionState::Failed {
            return Ok(GameLifecycleEvent::Failed);
        }
        let next_attempt = self.metrics.reconnect_attempts.saturating_add(1);
        if next_attempt > self.reconnect_policy.max_attempts {
            self.state = GameConnectionState::Failed;
            return Ok(GameLifecycleEvent::Failed);
        }
        let delay_ms = self
            .reconnect_policy
            .delay_for(next_attempt, jitter_sample)?;
        self.metrics.reconnect_attempts = next_attempt;
        self.metrics.reconnect_delay_total_ms = self
            .metrics
            .reconnect_delay_total_ms
            .saturating_add(delay_ms);
        self.state = GameConnectionState::Reconnecting;
        Ok(GameLifecycleEvent::ReconnectScheduled {
            attempt: next_attempt,
            delay_ms,
        })
    }

    pub fn close(&mut self) -> GameLifecycleEvent {
        self.session.clear_requests();
        self.state = GameConnectionState::Closed;
        GameLifecycleEvent::Closed
    }

    fn handle_auth_response(&mut self, packet: Packet) -> Result<GameLifecycleEvent, GameKcpError> {
        self.require_state("handle_auth_response", GameConnectionState::AwaitingAuth)?;
        let response = packet
            .decode_body::<AuthRes>("INVALID_AUTH_RESPONSE")
            .map_err(|_| GameKcpError::InvalidAuthResponse)?;
        self.session.ingest(packet)?;
        if response.ok {
            self.state = GameConnectionState::Authenticated;
            return Ok(GameLifecycleEvent::Authenticated);
        }

        let reason = classify_auth_rejection(&response.error_code);
        self.record_auth_rejection(reason);
        self.state = GameConnectionState::Failed;
        Ok(GameLifecycleEvent::AuthRejected { reason })
    }

    fn handle_ping_response(&mut self, packet: Packet) -> Result<GameLifecycleEvent, GameKcpError> {
        self.require_state("handle_ping_response", GameConnectionState::Authenticated)?;
        packet
            .decode_body::<PingRes>("INVALID_PING_RESPONSE")
            .map_err(|_| GameKcpError::InvalidPingResponse)?;
        self.session.ingest(packet)?;
        Ok(GameLifecycleEvent::HeartbeatAcknowledged)
    }

    fn require_state(
        &self,
        operation: &'static str,
        expected: GameConnectionState,
    ) -> Result<(), GameKcpError> {
        if self.state == expected {
            Ok(())
        } else {
            Err(GameKcpError::InvalidLifecycleState {
                operation,
                state: self.state,
            })
        }
    }

    fn allocate_seq(&mut self) -> Result<u32, GameKcpError> {
        let seq = self.next_seq;
        self.next_seq = self
            .next_seq
            .checked_add(1)
            .ok_or(GameKcpError::SequenceExhausted)?;
        Ok(seq)
    }

    fn cancel_outbound_request(&mut self, outbound: &OutboundPacket) -> Result<(), GameKcpError> {
        if self
            .session
            .cancel_request(outbound.expected_response, outbound.seq)
        {
            Ok(())
        } else {
            Err(GameKcpError::OutboundRequestNotPending {
                message_type: outbound.expected_response as u16,
                seq: outbound.seq,
            })
        }
    }

    fn record_auth_rejection(&mut self, reason: AuthRejectReason) {
        match reason {
            AuthRejectReason::InvalidTicket => self.metrics.auth_invalid_ticket += 1,
            AuthRejectReason::ExpiredTicket => self.metrics.auth_expired_ticket += 1,
            AuthRejectReason::ProtocolVersion => self.metrics.auth_protocol_version += 1,
            AuthRejectReason::Other => self.metrics.auth_other_rejection += 1,
        }
    }
}

fn classify_auth_rejection(error_code: &str) -> AuthRejectReason {
    match error_code {
        "TICKET_EXPIRED" | "INVALID_TICKET_EXP" => AuthRejectReason::ExpiredTicket,
        CLIENT_PROTOCOL_VERSION_TOO_OLD | CLIENT_PROTOCOL_VERSION_TOO_NEW => {
            AuthRejectReason::ProtocolVersion
        }
        "INVALID_TICKET_FORMAT"
        | "INVALID_TICKET_SIGNATURE"
        | "INVALID_TICKET_PAYLOAD"
        | "TICKET_NOT_FOUND"
        | "TICKET_REVOKED"
        | "MISSING_CHARACTER_ID"
        | "INVALID_CHARACTER_ID"
        | "ACCOUNT_PLAYER_ID_MISMATCH" => AuthRejectReason::InvalidTicket,
        _ => AuthRejectReason::Other,
    }
}

fn validate_packet(packet: &Packet, max_body_len: usize) -> Result<MessageType, GameKcpError> {
    if packet.header.body_len as usize != packet.body.len() {
        return Err(GameKcpError::BodyLengthMismatch {
            advertised: packet.header.body_len,
            actual: packet.body.len(),
        });
    }
    if packet.body.len() > max_body_len {
        return Err(GameKcpError::BodyTooLarge {
            body_len: packet.body.len(),
            max_body_len,
        });
    }
    MessageType::from_u16(packet.header.msg_type)
        .ok_or(GameKcpError::UnknownMessageType(packet.header.msg_type))
}

fn is_push(message_type: MessageType) -> bool {
    matches!(
        message_type,
        MessageType::RoomStatePush
            | MessageType::GameMessagePush
            | MessageType::FrameBundlePush
            | MessageType::RoomFrameRatePush
            | MessageType::RoomMemberOfflinePush
            | MessageType::MovementSnapshotPush
            | MessageType::MovementRejectPush
            | MessageType::ServerRedirectPush
            | MessageType::SessionKickPush
            | MessageType::AuthorityMigrationStartPush
            | MessageType::AuthorityMigrationCompletePush
            | MessageType::MatchEventPush
            | MessageType::InventoryUpdatePush
            | MessageType::AttrChangePush
            | MessageType::VisualChangePush
            | MessageType::ItemObtainPush
            | MessageType::CharacterElementsChangePush
            | MessageType::CharacterTitleChangePush
            | MessageType::CharacterDisciplineChangePush
    )
}

#[derive(Debug, Error)]
pub enum GameKcpError {
    #[error("game proxy configuration rejected: {0}")]
    Config(#[from] ConfigError),
    #[error("configured game_proxy target differs from the connection endpoint")]
    EndpointMismatch,
    #[error("could not resolve game proxy {host}: {source}")]
    Resolve {
        host: String,
        #[source]
        source: std::io::Error,
    },
    #[error("resolved game proxy {host} outside the validated access profile")]
    ResolvedAddressRejected { host: String },
    #[error("KCP connect failed: {0}")]
    Connect(#[source] std::io::Error),
    #[error("KCP session maximum body length must be positive")]
    InvalidBodyLimit,
    #[error("unknown player message type {0}")]
    UnknownMessageType(u16),
    #[error("packet body length mismatch: header={advertised}, actual={actual}")]
    BodyLengthMismatch { advertised: u32, actual: usize },
    #[error("packet body length {body_len} exceeds maximum {max_body_len}")]
    BodyTooLarge {
        body_len: usize,
        max_body_len: usize,
    },
    #[error("duplicate in-flight response expectation ({message_type}, {seq})")]
    DuplicateRequest { message_type: u16, seq: u32 },
    #[error("unexpected response ({message_type}, {seq})")]
    UnexpectedResponse { message_type: u16, seq: u32 },
    #[error("unsupported outbound request type {0}")]
    UnsupportedOutboundRequest(u16),
    #[error("outbound request expectation ({message_type}, {seq}) is not pending")]
    OutboundRequestNotPending { message_type: u16, seq: u32 },
    #[error("shared packet read failed: {0}")]
    Read(#[from] std::io::Error),
    #[error("invalid reconnect policy")]
    InvalidReconnectPolicy,
    #[error("reconnect attempt {attempt} is outside the configured range")]
    ReconnectAttemptOutOfRange { attempt: u32 },
    #[error("invalid lifecycle state for {operation}: {state:?}")]
    InvalidLifecycleState {
        operation: &'static str,
        state: GameConnectionState,
    },
    #[error("sequence space exhausted")]
    SequenceExhausted,
    #[error("invalid AuthRes body")]
    InvalidAuthResponse,
    #[error("invalid PingRes body")]
    InvalidPingResponse,
    #[error("unsupported lifecycle response type {0}")]
    UnsupportedLifecycleResponse(u16),
}

#[cfg(test)]
mod tests {
    use game_protocol::encode_packet;
    use prost::Message;
    use tokio::io::{AsyncWriteExt, duplex};

    use super::{
        AuthRejectReason, GameConnectionLifecycle, GameConnectionState, GameKcpError,
        GameLifecycleEvent, GameProxyEndpoint, KcpConnector, KcpSession, KcpSessionEvent,
        PLAYER_PACKET_HEADER_LEN, ReconnectPolicy,
    };
    use crate::protocol_version_policy::CURRENT_CLIENT_PROTOCOL_VERSION;

    fn packet(message_type: game_protocol::MessageType, seq: u32) -> game_protocol::Packet {
        game_protocol::Packet::new(
            game_protocol::PacketHeader {
                msg_type: message_type as u16,
                seq,
                body_len: 0,
            },
            Vec::new(),
        )
    }

    fn auth_response(seq: u32, ok: bool, error_code: &str) -> game_protocol::Packet {
        let body = game_protocol::encode_body(&crate::pb::AuthRes {
            ok,
            player_id: String::new(),
            error_code: error_code.to_owned(),
            server_protocol_version: 1,
            minimum_client_protocol_version: 1,
            upgrade_message: String::new(),
            upgrade_url: String::new(),
        });
        game_protocol::Packet::new(
            game_protocol::PacketHeader {
                msg_type: game_protocol::MessageType::AuthRes as u16,
                seq,
                body_len: body.len() as u32,
            },
            body,
        )
    }

    fn ping_response(seq: u32) -> game_protocol::Packet {
        let body = game_protocol::encode_body(&crate::pb::PingRes { server_time: 42 });
        game_protocol::Packet::new(
            game_protocol::PacketHeader {
                msg_type: game_protocol::MessageType::PingRes as u16,
                seq,
                body_len: body.len() as u32,
            },
            body,
        )
    }

    fn policy(max_attempts: u32) -> ReconnectPolicy {
        ReconnectPolicy {
            max_attempts,
            base_delay_ms: 100,
            max_delay_ms: 500,
            max_jitter_ms: 10,
        }
    }

    #[test]
    fn accepts_kcp_proxy_endpoints_without_reimplementing_environment_access_checks() {
        let endpoint = GameProxyEndpoint::parse("kcp://127.0.0.1:4000").unwrap();
        assert_eq!(endpoint.endpoint().host, "127.0.0.1");
        assert_eq!(endpoint.endpoint().port, 4000);
        assert_eq!(
            GameProxyEndpoint::parse("kcp://proxy.test.example:4000")
                .unwrap()
                .endpoint()
                .host,
            "proxy.test.example"
        );
    }

    #[test]
    fn rejects_tcp_and_direct_game_server_descriptors_for_every_profile() {
        for input in [
            "tcp://127.0.0.1:4000",
            "kcp://127.0.0.1:7000",
            "kcp://game-server:4000",
            "kcp://worker.game-server:4000",
            "kcp://10.0.0.5:7000",
        ] {
            assert!(GameProxyEndpoint::parse(input).is_err(), "accepted {input}");
        }
    }

    #[test]
    fn session_associates_the_exact_response_pair_and_counts_interleaved_pushes() {
        let mut session = KcpSession::new(32).unwrap();
        session
            .begin_request(game_protocol::MessageType::AuthRes, 7)
            .unwrap();
        session
            .begin_request(game_protocol::MessageType::PingRes, 8)
            .unwrap();

        assert_eq!(
            session
                .ingest(packet(game_protocol::MessageType::FrameBundlePush, 0))
                .unwrap(),
            KcpSessionEvent::Push {
                message_type: game_protocol::MessageType::FrameBundlePush,
                seq: 0,
            }
        );
        assert_eq!(
            session.push_count(game_protocol::MessageType::FrameBundlePush),
            1
        );
        assert_eq!(session.pending_requests(), 2);
        assert_eq!(
            session
                .ingest(packet(game_protocol::MessageType::AuthRes, 7))
                .unwrap(),
            KcpSessionEvent::Response {
                message_type: game_protocol::MessageType::AuthRes,
                seq: 7,
            }
        );
        assert_eq!(session.pending_requests(), 1);
    }

    #[test]
    fn session_rejects_unknown_wrong_sequence_invalid_and_oversize_packets() {
        let mut session = KcpSession::new(1).unwrap();
        assert_eq!(PLAYER_PACKET_HEADER_LEN, game_protocol::HEADER_LEN);
        session
            .begin_request(game_protocol::MessageType::AuthRes, 7)
            .unwrap();
        assert!(matches!(
            session.ingest(packet(game_protocol::MessageType::AuthRes, 8)),
            Err(GameKcpError::UnexpectedResponse {
                message_type: 1002,
                seq: 8,
            })
        ));
        assert_eq!(session.pending_requests(), 1);
        assert!(matches!(
            session.ingest(game_protocol::Packet::new(
                game_protocol::PacketHeader {
                    msg_type: 65535,
                    seq: 1,
                    body_len: 0,
                },
                Vec::new(),
            )),
            Err(GameKcpError::UnknownMessageType(65535))
        ));
        assert!(matches!(
            session.ingest(game_protocol::Packet::new(
                game_protocol::PacketHeader {
                    msg_type: game_protocol::MessageType::PingRes as u16,
                    seq: 1,
                    body_len: 1,
                },
                Vec::new(),
            )),
            Err(GameKcpError::BodyLengthMismatch {
                advertised: 1,
                actual: 0,
            })
        ));
        assert!(matches!(
            session.ingest(game_protocol::Packet::new(
                game_protocol::PacketHeader {
                    msg_type: game_protocol::MessageType::PingRes as u16,
                    seq: 1,
                    body_len: 2,
                },
                vec![1, 2],
            )),
            Err(GameKcpError::BodyTooLarge {
                body_len: 2,
                max_body_len: 1,
            })
        ));
    }

    #[tokio::test]
    async fn shared_reader_rejects_bad_magic_flags_and_oversize_then_maps_eof_to_disconnect() {
        let mut session = KcpSession::new(1).unwrap();
        let (mut writer, mut reader) = duplex(64);
        let mut bad_magic = encode_packet(game_protocol::MessageType::PingRes, 1, &[]);
        assert_eq!(bad_magic.len(), PLAYER_PACKET_HEADER_LEN);
        bad_magic[0] = 0;
        writer.write_all(&bad_magic).await.unwrap();
        assert!(matches!(
            session.read_event(&mut reader).await,
            Err(GameKcpError::Read(error)) if error.kind() == std::io::ErrorKind::Other
        ));

        let (mut writer, mut reader) = duplex(64);
        let mut bad_flags = encode_packet(game_protocol::MessageType::PingRes, 1, &[]);
        bad_flags[3] = 1;
        writer.write_all(&bad_flags).await.unwrap();
        assert!(matches!(
            session.read_event(&mut reader).await,
            Err(GameKcpError::Read(error)) if error.kind() == std::io::ErrorKind::Other
        ));

        let (mut writer, mut reader) = duplex(64);
        let oversize = encode_packet(game_protocol::MessageType::PingRes, 1, &[1, 2]);
        writer.write_all(&oversize).await.unwrap();
        assert!(matches!(
            session.read_event(&mut reader).await,
            Err(GameKcpError::Read(error)) if error.kind() == std::io::ErrorKind::InvalidData
        ));

        let (writer, mut reader) = duplex(64);
        drop(writer);
        assert_eq!(
            session.read_event(&mut reader).await.unwrap(),
            KcpSessionEvent::Disconnected
        );
    }

    #[test]
    fn connector_stays_on_the_kcp_transport_boundary() {
        let _connector = KcpConnector;
        let _connect = KcpConnector::connect;
        let _profile = game_protocol::player_kcp_config;
        let _stream: Option<tokio_kcp::KcpStream> = None;
    }

    #[test]
    fn reconnect_delay_remains_within_the_configured_upper_bound_after_jitter() {
        assert_eq!(policy(4).delay_for(4, 20).unwrap(), 500);
    }

    #[test]
    fn lifecycle_authenticates_heartbeats_and_uses_shared_proto_bodies_without_exposing_ticket() {
        let mut lifecycle = GameConnectionLifecycle::new(1024, policy(2)).unwrap();
        assert_eq!(
            lifecycle.begin_connect().unwrap(),
            GameLifecycleEvent::Connecting
        );
        let auth = lifecycle.begin_auth("secret-ticket-value").unwrap();
        assert_eq!(auth.message_type(), game_protocol::MessageType::AuthReq);
        assert_eq!(auth.seq(), 1);
        let debug = format!("{auth:?}");
        assert!(!debug.contains("secret-ticket-value"));
        let auth_packet = game_protocol::Packet::new(
            game_protocol::PacketHeader {
                msg_type: game_protocol::MessageType::AuthReq as u16,
                seq: auth.seq(),
                body_len: auth.clone().into_bytes().len() as u32 - PLAYER_PACKET_HEADER_LEN as u32,
            },
            auth.clone().into_bytes()[PLAYER_PACKET_HEADER_LEN..].to_vec(),
        );
        let decoded = auth_packet
            .decode_body::<crate::pb::AuthReq>("bad")
            .unwrap();
        assert_eq!(decoded.ticket, "secret-ticket-value");
        assert_eq!(
            decoded.client_protocol_version,
            CURRENT_CLIENT_PROTOCOL_VERSION
        );
        assert_eq!(
            lifecycle.handle_packet(auth_response(1, true, "")).unwrap(),
            GameLifecycleEvent::Authenticated
        );
        let ping = lifecycle.begin_heartbeat(123).unwrap();
        assert_eq!(ping.message_type(), game_protocol::MessageType::PingReq);
        let ping_bytes = ping.into_bytes();
        let ping_header =
            game_protocol::parse_header(ping_bytes[..PLAYER_PACKET_HEADER_LEN].try_into().unwrap())
                .unwrap();
        let ping_body =
            crate::pb::PingReq::decode(&ping_bytes[PLAYER_PACKET_HEADER_LEN..]).unwrap();
        assert_eq!(ping_header.seq, 2);
        assert_eq!(ping_body.client_time, 123);
        assert_eq!(
            lifecycle.handle_packet(ping_response(2)).unwrap(),
            GameLifecycleEvent::HeartbeatAcknowledged
        );
        assert_eq!(lifecycle.state(), GameConnectionState::Authenticated);
    }

    #[test]
    fn authenticated_gameplay_request_uses_shared_sequence_and_exact_response_type() {
        let mut lifecycle = GameConnectionLifecycle::new(1_024, policy(1)).unwrap();
        lifecycle.begin_connect().unwrap();
        lifecycle.begin_auth("private-ticket").unwrap();
        lifecycle.handle_packet(auth_response(1, true, "")).unwrap();

        let body = game_protocol::encode_body(&crate::pb::RoomJoinReq {
            room_id: "approved-room".into(),
            policy_id: "approved-policy".into(),
        });
        let join = lifecycle
            .begin_gameplay_request(
                game_protocol::MessageType::RoomJoinReq,
                game_protocol::MessageType::RoomJoinRes,
                &body,
            )
            .unwrap();
        assert_eq!(join.seq(), 2);
        assert_eq!(join.message_type(), game_protocol::MessageType::RoomJoinReq);
        assert_eq!(
            lifecycle
                .handle_packet(game_protocol::Packet::new(
                    game_protocol::PacketHeader {
                        msg_type: game_protocol::MessageType::RoomJoinRes as u16,
                        seq: join.seq(),
                        body_len: 0,
                    },
                    Vec::new(),
                ))
                .unwrap(),
            GameLifecycleEvent::Response {
                message_type: game_protocol::MessageType::RoomJoinRes,
                seq: 2,
            }
        );
        assert_eq!(lifecycle.pending_requests(), 0);
    }

    #[test]
    fn lifecycle_classifies_invalid_expired_and_version_rejections_without_retaining_sensitive_data()
     {
        for (error_code, expected) in [
            ("INVALID_TICKET_SIGNATURE", AuthRejectReason::InvalidTicket),
            ("TICKET_EXPIRED", AuthRejectReason::ExpiredTicket),
            (
                "CLIENT_PROTOCOL_VERSION_TOO_NEW",
                AuthRejectReason::ProtocolVersion,
            ),
        ] {
            let mut lifecycle = GameConnectionLifecycle::new(1024, policy(1)).unwrap();
            lifecycle.begin_connect().unwrap();
            if expected == AuthRejectReason::ProtocolVersion {
                lifecycle
                    .begin_auth_with_protocol_version("private-ticket", 99)
                    .unwrap();
            } else {
                lifecycle.begin_auth("private-ticket").unwrap();
            }
            assert_eq!(
                lifecycle
                    .handle_packet(auth_response(1, false, error_code))
                    .unwrap(),
                GameLifecycleEvent::AuthRejected { reason: expected }
            );
            assert_eq!(lifecycle.state(), GameConnectionState::Failed);
            assert_eq!(lifecycle.pending_requests(), 0);
            assert!(!format!("{:?}", lifecycle).contains("private-ticket"));
        }
    }

    #[test]
    fn lifecycle_normal_close_does_not_reconnect_but_disconnect_retries_with_bounded_jitter() {
        let mut closed = GameConnectionLifecycle::new(1024, policy(2)).unwrap();
        closed.begin_connect().unwrap();
        closed.begin_auth("ticket").unwrap();
        assert_eq!(closed.close(), GameLifecycleEvent::Closed);
        assert_eq!(
            closed.handle_disconnected(99).unwrap(),
            GameLifecycleEvent::Closed
        );

        let mut reconnecting = GameConnectionLifecycle::new(1024, policy(2)).unwrap();
        reconnecting.begin_connect().unwrap();
        reconnecting.begin_auth("ticket").unwrap();
        assert_eq!(reconnecting.pending_requests(), 1);
        assert_eq!(
            reconnecting.handle_disconnected(3).unwrap(),
            GameLifecycleEvent::ReconnectScheduled {
                attempt: 1,
                delay_ms: 93,
            }
        );
        assert_eq!(reconnecting.pending_requests(), 0);
        assert_eq!(
            reconnecting.handle_disconnected(20).unwrap(),
            GameLifecycleEvent::ReconnectScheduled {
                attempt: 2,
                delay_ms: 210,
            }
        );
        assert_eq!(
            reconnecting.handle_disconnected(20).unwrap(),
            GameLifecycleEvent::Failed
        );
        assert_eq!(reconnecting.metrics().reconnect_attempts, 2);
        assert_eq!(reconnecting.state(), GameConnectionState::Failed);
    }

    #[test]
    fn lifecycle_cancels_failed_outbound_writes_before_scheduling_reconnect() {
        let mut lifecycle = GameConnectionLifecycle::new(1024, policy(1)).unwrap();
        lifecycle.begin_connect().unwrap();
        let auth = lifecycle.begin_auth("private-ticket").unwrap();
        assert_eq!(lifecycle.pending_requests(), 1);

        assert_eq!(
            lifecycle.handle_outbound_write_failure(&auth, 0).unwrap(),
            GameLifecycleEvent::ReconnectScheduled {
                attempt: 1,
                delay_ms: 90,
            }
        );
        assert_eq!(lifecycle.pending_requests(), 0);
        assert_eq!(lifecycle.state(), GameConnectionState::Reconnecting);
        assert!(matches!(
            lifecycle.handle_outbound_write_failure(&auth, 0),
            Err(GameKcpError::OutboundRequestNotPending {
                message_type: 1002,
                seq: 1,
            })
        ));
    }

    #[test]
    fn lifecycle_timeout_clears_the_old_sequence_before_reconnect() {
        let mut lifecycle = GameConnectionLifecycle::new(1024, policy(1)).unwrap();
        lifecycle.begin_connect().unwrap();
        let auth = lifecycle.begin_auth("private-ticket").unwrap();

        assert_eq!(
            lifecycle.handle_request_timeout(&auth, 0).unwrap(),
            GameLifecycleEvent::ReconnectScheduled {
                attempt: 1,
                delay_ms: 90,
            }
        );
        assert_eq!(lifecycle.pending_requests(), 0);
        assert_eq!(
            lifecycle.handle_packet(auth_response(1, true, "")).unwrap(),
            GameLifecycleEvent::LateResponseDropped {
                message_type: game_protocol::MessageType::AuthRes,
                seq: 1,
            }
        );
    }
}
