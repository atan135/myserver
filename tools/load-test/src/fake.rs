use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    active_connections: Arc<AtomicUsize>,
}
impl FakeKcpService {
    pub fn scripted(outcomes: impl IntoIterator<Item = FakeOutcome>) -> Self {
        Self {
            outcomes: outcomes.into_iter().collect(),
            active_connections: Arc::new(AtomicUsize::new(0)),
        }
    }
    pub fn connect(&self) -> FakeKcpConnection {
        self.active_connections.fetch_add(1, Ordering::SeqCst);
        FakeKcpConnection {
            active_connections: self.active_connections.clone(),
        }
    }
    pub fn next_outcome(&mut self) -> FakeOutcome {
        self.outcomes.pop_front().unwrap_or(FakeOutcome::Success)
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
}
