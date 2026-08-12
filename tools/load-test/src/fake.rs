use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use game_protocol::{MessageType, Packet, PacketHeader};

use crate::step::ResponseClassification;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeOutcome {
    Success,
    Slow,
    BusinessError,
    Timeout,
    Disconnect,
    LateResponse,
    OutOfOrderPush,
}

/// Deterministic events used by the stage-three player-session tests. They
/// model only observable transport behavior; no socket is ever opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeKcpEvent {
    Authenticated,
    AuthRejected { error_code: String },
    HeartbeatAcknowledged,
    ForwardedResponse { message_type: MessageType },
    Push { message_type: MessageType, seq: u32 },
    SlowResponse,
    HalfOpen,
    ForcedDisconnect,
}

impl FakeKcpEvent {
    pub fn packet_for(&self, seq: u32) -> Option<Packet> {
        let (message_type, response_seq, body) = match self {
            Self::Authenticated => (
                MessageType::AuthRes,
                seq,
                game_protocol::encode_body(&crate::pb::AuthRes {
                    ok: true,
                    player_id: "fake-player".into(),
                    error_code: String::new(),
                    server_protocol_version: 1,
                    minimum_client_protocol_version: 1,
                    upgrade_message: String::new(),
                    upgrade_url: String::new(),
                }),
            ),
            Self::AuthRejected { error_code } => (
                MessageType::AuthRes,
                seq,
                game_protocol::encode_body(&crate::pb::AuthRes {
                    ok: false,
                    player_id: String::new(),
                    error_code: error_code.clone(),
                    server_protocol_version: 1,
                    minimum_client_protocol_version: 1,
                    upgrade_message: String::new(),
                    upgrade_url: String::new(),
                }),
            ),
            Self::HeartbeatAcknowledged => (
                MessageType::PingRes,
                seq,
                game_protocol::encode_body(&crate::pb::PingRes { server_time: 1 }),
            ),
            Self::ForwardedResponse { message_type } => (*message_type, seq, Vec::new()),
            Self::Push { message_type, seq } => (*message_type, *seq, Vec::new()),
            Self::SlowResponse | Self::HalfOpen | Self::ForcedDisconnect => return None,
        };
        Some(Packet::new(
            PacketHeader {
                msg_type: message_type as u16,
                seq: response_seq,
                body_len: body.len() as u32,
            },
            body,
        ))
    }
}

#[derive(Debug, Clone)]
pub struct FakeHttpService {
    outcomes: VecDeque<FakeOutcome>,
}
impl FakeHttpService {
    pub fn scripted(outcomes: impl IntoIterator<Item = FakeOutcome>) -> Self {
        Self {
            outcomes: outcomes.into_iter().collect(),
        }
    }
    pub fn request(&mut self) -> FakeOutcome {
        self.outcomes.pop_front().unwrap_or(FakeOutcome::Success)
    }
}

#[derive(Debug, Clone)]
pub struct FakeKcpService {
    outcomes: VecDeque<FakeOutcome>,
    events: Arc<Mutex<VecDeque<FakeKcpEvent>>>,
    active_connections: Arc<AtomicUsize>,
}
impl FakeKcpService {
    pub fn scripted(outcomes: impl IntoIterator<Item = FakeOutcome>) -> Self {
        Self {
            outcomes: outcomes.into_iter().collect(),
            events: Arc::new(Mutex::new(VecDeque::new())),
            active_connections: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn scripted_events(events: impl IntoIterator<Item = FakeKcpEvent>) -> Self {
        Self {
            outcomes: VecDeque::new(),
            events: Arc::new(Mutex::new(events.into_iter().collect())),
            active_connections: Arc::new(AtomicUsize::new(0)),
        }
    }
    pub fn connect(&self) -> FakeKcpConnection {
        self.active_connections.fetch_add(1, Ordering::SeqCst);
        FakeKcpConnection {
            active_connections: self.active_connections.clone(),
            events: self.events.clone(),
        }
    }
    pub fn next_outcome(&mut self) -> FakeOutcome {
        self.outcomes.pop_front().unwrap_or(FakeOutcome::Success)
    }
    pub fn next_event(&self) -> FakeKcpEvent {
        self.events
            .lock()
            .expect("fake KCP event queue mutex is not poisoned")
            .pop_front()
            .unwrap_or(FakeKcpEvent::SlowResponse)
    }
    pub fn active_connections(&self) -> usize {
        self.active_connections.load(Ordering::SeqCst)
    }
    pub fn response_packet(sequence: u32, outcome: &FakeOutcome) -> Packet {
        let message_type = match outcome {
            FakeOutcome::OutOfOrderPush => MessageType::FrameBundlePush,
            _ => MessageType::AuthRes,
        };
        Packet::new(
            PacketHeader {
                msg_type: message_type as u16,
                seq: if matches!(outcome, FakeOutcome::OutOfOrderPush) {
                    sequence.saturating_add(1)
                } else {
                    sequence
                },
                body_len: 0,
            },
            Vec::new(),
        )
    }
}

#[derive(Debug)]
pub struct FakeKcpConnection {
    active_connections: Arc<AtomicUsize>,
    events: Arc<Mutex<VecDeque<FakeKcpEvent>>>,
}
impl FakeKcpConnection {
    pub fn next_event(&self) -> FakeKcpEvent {
        self.events
            .lock()
            .expect("fake KCP event queue mutex is not poisoned")
            .pop_front()
            .unwrap_or(FakeKcpEvent::SlowResponse)
    }
}
impl Drop for FakeKcpConnection {
    fn drop(&mut self) {
        self.active_connections.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn classify(outcome: &FakeOutcome) -> ResponseClassification {
    match outcome {
        FakeOutcome::Success | FakeOutcome::Slow => ResponseClassification::Matched,
        FakeOutcome::BusinessError | FakeOutcome::Disconnect | FakeOutcome::OutOfOrderPush => {
            ResponseClassification::Unexpected
        }
        FakeOutcome::Timeout => ResponseClassification::Timeout,
        FakeOutcome::LateResponse => ResponseClassification::LateResponse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deterministic_fakes_cover_classification_and_release_connections() {
        let mut http = FakeHttpService::scripted([
            FakeOutcome::Success,
            FakeOutcome::Timeout,
            FakeOutcome::BusinessError,
        ]);
        assert_eq!(classify(&http.request()), ResponseClassification::Matched);
        assert_eq!(classify(&http.request()), ResponseClassification::Timeout);
        assert_eq!(
            classify(&http.request()),
            ResponseClassification::Unexpected
        );
        let kcp = FakeKcpService::scripted([
            FakeOutcome::OutOfOrderPush,
            FakeOutcome::LateResponse,
            FakeOutcome::Disconnect,
        ]);
        {
            let _connection = kcp.connect();
            assert_eq!(kcp.active_connections(), 1);
        }
        assert_eq!(kcp.active_connections(), 0);
        let packet = FakeKcpService::response_packet(7, &FakeOutcome::OutOfOrderPush);
        assert_eq!(packet.message_type(), Some(MessageType::FrameBundlePush));
        assert_eq!(packet.header.seq, 8);
        assert_eq!(
            classify(&FakeOutcome::LateResponse),
            ResponseClassification::LateResponse
        );
    }

    #[test]
    fn kcp_event_fake_models_protocol_packets_and_transport_failures_without_network() {
        let fake = FakeKcpService::scripted_events([
            FakeKcpEvent::Authenticated,
            FakeKcpEvent::HeartbeatAcknowledged,
            FakeKcpEvent::ForwardedResponse {
                message_type: MessageType::PingRes,
            },
            FakeKcpEvent::Push {
                message_type: MessageType::FrameBundlePush,
                seq: 99,
            },
            FakeKcpEvent::SlowResponse,
            FakeKcpEvent::HalfOpen,
            FakeKcpEvent::ForcedDisconnect,
        ]);
        let connection = fake.connect();
        assert_eq!(fake.active_connections(), 1);
        assert_eq!(
            fake.next_event().packet_for(1).unwrap().message_type(),
            Some(MessageType::AuthRes)
        );
        assert_eq!(
            fake.next_event().packet_for(2).unwrap().message_type(),
            Some(MessageType::PingRes)
        );
        assert_eq!(fake.next_event().packet_for(3).unwrap().header.seq, 3);
        assert_eq!(fake.next_event().packet_for(3).unwrap().header.seq, 99);
        assert!(fake.next_event().packet_for(4).is_none());
        assert!(fake.next_event().packet_for(4).is_none());
        assert!(fake.next_event().packet_for(4).is_none());
        drop(connection);
        assert_eq!(fake.active_connections(), 0);
    }
}
