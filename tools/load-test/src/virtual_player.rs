//! Virtual-player ownership around auth, KCP lifecycle, and account leases.
//!
//! This layer deliberately receives tickets only at the moment it builds an
//! `AuthReq`; it stores neither tickets nor auth credentials. It owns the
//! account lease and transport handle so every terminal transition follows one
//! release path.

use game_protocol::{MessageType, Packet};
use thiserror::Error;

use crate::accounts::{AccountLease, AccountLeasePool};
use crate::fake::{FakeKcpConnection, FakeKcpEvent};
use crate::game_kcp::{
    GameConnectionLifecycle, GameKcpError, GameLifecycleEvent, OutboundPacket, ReconnectPolicy,
};

pub trait PlayerConnection: Send {}

impl PlayerConnection for FakeKcpConnection {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualPlayerSessionState {
    AccountLeased,
    LoggedIn,
    CharacterSelected,
    TicketIssued,
    ProxyConnected,
    GameAuthenticated,
    Active,
    Leaving,
    Reconnecting,
    Closed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtualPlayerEvent {
    LoggedIn,
    CharacterSelected,
    TicketIssued,
    ProxyConnected,
    GameAuthenticated,
    Active,
    HeartbeatAcknowledged,
    Response { message_type: MessageType, seq: u32 },
    Push { message_type: MessageType, seq: u32 },
    ReconnectScheduled { attempt: u32, delay_ms: u64 },
    LateResponseDropped { message_type: MessageType, seq: u32 },
    Closed,
    Failed,
}

#[derive(Debug, Error)]
pub enum VirtualPlayerError {
    #[error("virtual player transition {operation} is invalid in state {state:?}")]
    InvalidState {
        operation: &'static str,
        state: VirtualPlayerSessionState,
    },
    #[error("virtual player transport event requires the matching outbound request")]
    MissingOutboundRequest,
    #[error("virtual player received an unsupported fake event in this state")]
    UnsupportedFakeEvent,
    #[error(transparent)]
    Game(#[from] GameKcpError),
}

pub struct VirtualPlayerSession {
    lease: Option<AccountLease>,
    state: VirtualPlayerSessionState,
    game: GameConnectionLifecycle,
    connection: Option<Box<dyn PlayerConnection>>,
}

impl std::fmt::Debug for VirtualPlayerSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VirtualPlayerSession")
            .field("state", &self.state)
            .field("lease_held", &self.lease.is_some())
            .field("connection_attached", &self.connection.is_some())
            .field("game_state", &self.game.state())
            .finish()
    }
}

impl VirtualPlayerSession {
    pub fn new(
        lease: AccountLease,
        max_body_len: usize,
        reconnect_policy: ReconnectPolicy,
    ) -> Result<Self, VirtualPlayerError> {
        Ok(Self {
            lease: Some(lease),
            state: VirtualPlayerSessionState::AccountLeased,
            game: GameConnectionLifecycle::new(max_body_len, reconnect_policy)?,
            connection: None,
        })
    }

    pub fn state(&self) -> VirtualPlayerSessionState {
        self.state
    }

    pub fn lease_held(&self) -> bool {
        self.lease.is_some()
    }

    pub fn connection_attached(&self) -> bool {
        self.connection.is_some()
    }

    pub fn pending_requests(&self) -> usize {
        self.game.pending_requests()
    }

    pub fn mark_logged_in(
        &mut self,
        pool: &mut AccountLeasePool,
    ) -> Result<VirtualPlayerEvent, VirtualPlayerError> {
        self.transition(
            pool,
            "mark_logged_in",
            VirtualPlayerSessionState::AccountLeased,
            |this| {
                this.state = VirtualPlayerSessionState::LoggedIn;
                VirtualPlayerEvent::LoggedIn
            },
        )
    }

    pub fn mark_character_selected(
        &mut self,
        pool: &mut AccountLeasePool,
    ) -> Result<VirtualPlayerEvent, VirtualPlayerError> {
        self.transition(
            pool,
            "mark_character_selected",
            VirtualPlayerSessionState::LoggedIn,
            |this| {
                this.state = VirtualPlayerSessionState::CharacterSelected;
                VirtualPlayerEvent::CharacterSelected
            },
        )
    }

    pub fn mark_ticket_issued(
        &mut self,
        pool: &mut AccountLeasePool,
    ) -> Result<VirtualPlayerEvent, VirtualPlayerError> {
        self.transition(
            pool,
            "mark_ticket_issued",
            VirtualPlayerSessionState::CharacterSelected,
            |this| {
                this.state = VirtualPlayerSessionState::TicketIssued;
                VirtualPlayerEvent::TicketIssued
            },
        )
    }

    /// Attaches the already-created KCP transport and immediately creates the
    /// shared-proto authentication packet. The session does not retain the
    /// ticket; the caller writes and then drops the returned packet.
    pub fn connect_and_begin_auth(
        &mut self,
        pool: &mut AccountLeasePool,
        connection: impl PlayerConnection + 'static,
        ticket: &str,
    ) -> Result<OutboundPacket, VirtualPlayerError> {
        if !matches!(
            self.state,
            VirtualPlayerSessionState::TicketIssued | VirtualPlayerSessionState::Reconnecting
        ) {
            return self.invalid_state(pool, "connect_and_begin_auth");
        }
        if self.connection.is_some() {
            return self.invalid_state(pool, "connect_and_begin_auth");
        }
        let result = (|| {
            self.game.begin_connect()?;
            let outbound = self.game.begin_auth(ticket)?;
            self.connection = Some(Box::new(connection));
            self.state = VirtualPlayerSessionState::ProxyConnected;
            Ok(outbound)
        })();
        result.map_err(|error: GameKcpError| self.fail(pool, error))
    }

    pub fn handle_packet(
        &mut self,
        pool: &mut AccountLeasePool,
        packet: Packet,
    ) -> Result<VirtualPlayerEvent, VirtualPlayerError> {
        let event = self
            .game
            .handle_packet(packet)
            .map_err(|error| self.fail(pool, error))?;
        self.after_game_event(pool, event)
    }

    pub fn activate(
        &mut self,
        pool: &mut AccountLeasePool,
    ) -> Result<VirtualPlayerEvent, VirtualPlayerError> {
        self.transition(
            pool,
            "activate",
            VirtualPlayerSessionState::GameAuthenticated,
            |this| {
                this.state = VirtualPlayerSessionState::Active;
                VirtualPlayerEvent::Active
            },
        )
    }

    pub fn begin_heartbeat(
        &mut self,
        pool: &mut AccountLeasePool,
        client_time: i64,
    ) -> Result<OutboundPacket, VirtualPlayerError> {
        if self.state != VirtualPlayerSessionState::Active {
            return self.invalid_state(pool, "begin_heartbeat");
        }
        self.game
            .begin_heartbeat(client_time)
            .map_err(|error| self.fail(pool, error))
    }

    pub fn begin_gameplay_request(
        &mut self,
        pool: &mut AccountLeasePool,
        request_type: MessageType,
        expected_response: MessageType,
        body: &[u8],
    ) -> Result<OutboundPacket, VirtualPlayerError> {
        if self.state != VirtualPlayerSessionState::Active {
            return self.invalid_state(pool, "begin_gameplay_request");
        }
        self.game
            .begin_gameplay_request(request_type, expected_response, body)
            .map_err(|error| self.fail(pool, error))
    }

    pub fn handle_request_timeout(
        &mut self,
        pool: &mut AccountLeasePool,
        outbound: &OutboundPacket,
        jitter_sample: u64,
    ) -> Result<VirtualPlayerEvent, VirtualPlayerError> {
        let event = self
            .game
            .handle_request_timeout(outbound, jitter_sample)
            .map_err(|error| self.fail(pool, error))?;
        self.after_game_event(pool, event)
    }

    pub fn handle_outbound_write_failure(
        &mut self,
        pool: &mut AccountLeasePool,
        outbound: &OutboundPacket,
        jitter_sample: u64,
    ) -> Result<VirtualPlayerEvent, VirtualPlayerError> {
        let event = self
            .game
            .handle_outbound_write_failure(outbound, jitter_sample)
            .map_err(|error| self.fail(pool, error))?;
        self.after_game_event(pool, event)
    }

    pub fn handle_disconnect(
        &mut self,
        pool: &mut AccountLeasePool,
        jitter_sample: u64,
    ) -> Result<VirtualPlayerEvent, VirtualPlayerError> {
        let event = self
            .game
            .handle_disconnected(jitter_sample)
            .map_err(|error| self.fail(pool, error))?;
        self.after_game_event(pool, event)
    }

    pub fn begin_leaving(&mut self, pool: &mut AccountLeasePool) -> Result<(), VirtualPlayerError> {
        if self.state != VirtualPlayerSessionState::Active {
            return self.invalid_state::<()>(pool, "begin_leaving");
        }
        self.state = VirtualPlayerSessionState::Leaving;
        Ok(())
    }

    pub fn close(&mut self, pool: &mut AccountLeasePool) -> VirtualPlayerEvent {
        self.release_resources(pool);
        self.state = VirtualPlayerSessionState::Closed;
        VirtualPlayerEvent::Closed
    }

    /// Drives the deterministic KCP fake while preserving the same packet and
    /// lifecycle behavior used by a real transport loop.
    pub fn handle_fake_event(
        &mut self,
        pool: &mut AccountLeasePool,
        event: FakeKcpEvent,
        outbound: Option<&OutboundPacket>,
        jitter_sample: u64,
    ) -> Result<VirtualPlayerEvent, VirtualPlayerError> {
        match event {
            FakeKcpEvent::SlowResponse | FakeKcpEvent::HalfOpen => self.handle_request_timeout(
                pool,
                outbound.ok_or(VirtualPlayerError::MissingOutboundRequest)?,
                jitter_sample,
            ),
            FakeKcpEvent::ForcedDisconnect => self.handle_disconnect(pool, jitter_sample),
            FakeKcpEvent::Push { .. } => {
                let packet = event
                    .packet_for(0)
                    .ok_or(VirtualPlayerError::UnsupportedFakeEvent)?;
                self.handle_packet(pool, packet)
            }
            event => {
                let outbound = outbound.ok_or(VirtualPlayerError::MissingOutboundRequest)?;
                let packet = event
                    .packet_for(outbound.seq())
                    .ok_or(VirtualPlayerError::UnsupportedFakeEvent)?;
                self.handle_packet(pool, packet)
            }
        }
    }

    fn after_game_event(
        &mut self,
        pool: &mut AccountLeasePool,
        event: GameLifecycleEvent,
    ) -> Result<VirtualPlayerEvent, VirtualPlayerError> {
        match event {
            GameLifecycleEvent::Authenticated => {
                self.state = VirtualPlayerSessionState::GameAuthenticated;
                Ok(VirtualPlayerEvent::GameAuthenticated)
            }
            GameLifecycleEvent::HeartbeatAcknowledged => {
                Ok(VirtualPlayerEvent::HeartbeatAcknowledged)
            }
            GameLifecycleEvent::Response { message_type, seq } => {
                Ok(VirtualPlayerEvent::Response { message_type, seq })
            }
            GameLifecycleEvent::Push { message_type, seq } => {
                Ok(VirtualPlayerEvent::Push { message_type, seq })
            }
            GameLifecycleEvent::ReconnectScheduled { attempt, delay_ms } => {
                self.connection.take();
                self.state = VirtualPlayerSessionState::Reconnecting;
                Ok(VirtualPlayerEvent::ReconnectScheduled { attempt, delay_ms })
            }
            GameLifecycleEvent::LateResponseDropped { message_type, seq } => {
                Ok(VirtualPlayerEvent::LateResponseDropped { message_type, seq })
            }
            GameLifecycleEvent::AuthRejected { .. } | GameLifecycleEvent::Failed => {
                self.release_resources(pool);
                self.state = VirtualPlayerSessionState::Failed;
                Ok(VirtualPlayerEvent::Failed)
            }
            GameLifecycleEvent::Closed => Ok(self.close(pool)),
            GameLifecycleEvent::Connecting => Err(self.fail(
                pool,
                GameKcpError::InvalidLifecycleState {
                    operation: "unexpected_connecting_event",
                    state: self.game.state(),
                },
            )),
        }
    }

    fn transition(
        &mut self,
        pool: &mut AccountLeasePool,
        operation: &'static str,
        expected: VirtualPlayerSessionState,
        apply: impl FnOnce(&mut Self) -> VirtualPlayerEvent,
    ) -> Result<VirtualPlayerEvent, VirtualPlayerError> {
        if self.state == expected {
            Ok(apply(self))
        } else {
            self.invalid_state(pool, operation)
        }
    }

    fn invalid_state<T>(
        &mut self,
        pool: &mut AccountLeasePool,
        operation: &'static str,
    ) -> Result<T, VirtualPlayerError> {
        let error = VirtualPlayerError::InvalidState {
            operation,
            state: self.state,
        };
        self.release_resources(pool);
        self.state = VirtualPlayerSessionState::Failed;
        Err(error)
    }

    fn fail(&mut self, pool: &mut AccountLeasePool, error: GameKcpError) -> VirtualPlayerError {
        self.release_resources(pool);
        self.state = VirtualPlayerSessionState::Failed;
        VirtualPlayerError::Game(error)
    }

    fn release_resources(&mut self, pool: &mut AccountLeasePool) {
        self.game.close();
        self.connection.take();
        if let Some(lease) = self.lease.take() {
            let _ = pool.release(&lease);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeKcpService;

    fn policy(max_attempts: u32) -> ReconnectPolicy {
        ReconnectPolicy {
            max_attempts,
            base_delay_ms: 100,
            max_delay_ms: 500,
            max_jitter_ms: 10,
        }
    }

    fn leased_session(pool: &mut AccountLeasePool) -> VirtualPlayerSession {
        let lease = pool.acquire("account-a", "player-a", 0, 1_000).unwrap();
        VirtualPlayerSession::new(lease, 1_024, policy(2)).unwrap()
    }

    fn advance_to_ticket(session: &mut VirtualPlayerSession, pool: &mut AccountLeasePool) {
        session.mark_logged_in(pool).unwrap();
        session.mark_character_selected(pool).unwrap();
        session.mark_ticket_issued(pool).unwrap();
    }

    #[test]
    fn virtual_player_runs_the_required_state_path_and_releases_every_owned_resource() {
        let mut pool = AccountLeasePool::default();
        let mut session = leased_session(&mut pool);
        advance_to_ticket(&mut session, &mut pool);
        let fake = FakeKcpService::scripted_events([
            FakeKcpEvent::Authenticated,
            FakeKcpEvent::ForwardedResponse {
                message_type: MessageType::PingRes,
            },
            FakeKcpEvent::Push {
                message_type: MessageType::FrameBundlePush,
                seq: 88,
            },
        ]);
        let auth = session
            .connect_and_begin_auth(&mut pool, fake.connect(), "ephemeral-ticket")
            .unwrap();
        assert!(!format!("{session:?}").contains("ephemeral-ticket"));
        assert_eq!(session.state(), VirtualPlayerSessionState::ProxyConnected);
        assert_eq!(fake.active_connections(), 1);
        assert_eq!(
            session
                .handle_fake_event(&mut pool, fake.next_event(), Some(&auth), 0,)
                .unwrap(),
            VirtualPlayerEvent::GameAuthenticated
        );
        assert_eq!(
            session.state(),
            VirtualPlayerSessionState::GameAuthenticated
        );
        assert_eq!(
            session.activate(&mut pool).unwrap(),
            VirtualPlayerEvent::Active
        );
        let heartbeat = session.begin_heartbeat(&mut pool, 42).unwrap();
        assert_eq!(
            session
                .handle_fake_event(&mut pool, fake.next_event(), Some(&heartbeat), 0,)
                .unwrap(),
            VirtualPlayerEvent::HeartbeatAcknowledged
        );
        assert_eq!(
            session
                .handle_fake_event(&mut pool, fake.next_event(), None, 0,)
                .unwrap(),
            VirtualPlayerEvent::Push {
                message_type: MessageType::FrameBundlePush,
                seq: 88,
            }
        );
        session.begin_leaving(&mut pool).unwrap();
        assert_eq!(session.close(&mut pool), VirtualPlayerEvent::Closed);
        assert_eq!(session.state(), VirtualPlayerSessionState::Closed);
        assert!(!session.lease_held());
        assert!(!session.connection_attached());
        assert_eq!(fake.active_connections(), 0);
        assert!(pool.acquire("account-a", "replacement", 1, 1_000).is_ok());
    }

    #[test]
    fn auth_rejection_fails_closed_and_returns_the_account_lease() {
        let mut pool = AccountLeasePool::default();
        let mut session = leased_session(&mut pool);
        advance_to_ticket(&mut session, &mut pool);
        let fake = FakeKcpService::scripted_events([FakeKcpEvent::AuthRejected {
            error_code: "INVALID_TICKET_SIGNATURE".into(),
        }]);
        let auth = session
            .connect_and_begin_auth(&mut pool, fake.connect(), "ephemeral-ticket")
            .unwrap();
        assert_eq!(
            session
                .handle_fake_event(&mut pool, fake.next_event(), Some(&auth), 0,)
                .unwrap(),
            VirtualPlayerEvent::Failed
        );
        assert_eq!(session.state(), VirtualPlayerSessionState::Failed);
        assert_eq!(session.pending_requests(), 0);
        assert_eq!(fake.active_connections(), 0);
        assert!(pool.acquire("account-a", "replacement", 1, 1_000).is_ok());
    }

    #[test]
    fn slow_half_open_and_forced_disconnect_use_bounded_reconnect_then_release_at_exhaustion() {
        let mut pool = AccountLeasePool::default();
        let mut session = leased_session(&mut pool);
        advance_to_ticket(&mut session, &mut pool);
        let fake = FakeKcpService::scripted_events([FakeKcpEvent::SlowResponse]);
        let auth = session
            .connect_and_begin_auth(&mut pool, fake.connect(), "ephemeral-ticket")
            .unwrap();
        assert_eq!(
            session
                .handle_fake_event(&mut pool, fake.next_event(), Some(&auth), 0)
                .unwrap(),
            VirtualPlayerEvent::ReconnectScheduled {
                attempt: 1,
                delay_ms: 90,
            }
        );
        assert_eq!(session.state(), VirtualPlayerSessionState::Reconnecting);
        assert_eq!(fake.active_connections(), 0);
        assert!(session.lease_held());

        let retry_fake = FakeKcpService::scripted_events([FakeKcpEvent::HalfOpen]);
        let retry = session
            .connect_and_begin_auth(&mut pool, retry_fake.connect(), "replacement-ticket")
            .unwrap();
        assert_eq!(
            session
                .handle_fake_event(&mut pool, retry_fake.next_event(), Some(&retry), 0)
                .unwrap(),
            VirtualPlayerEvent::ReconnectScheduled {
                attempt: 2,
                delay_ms: 190,
            }
        );
        let final_fake = FakeKcpService::scripted_events([FakeKcpEvent::ForcedDisconnect]);
        let retry = session
            .connect_and_begin_auth(&mut pool, final_fake.connect(), "replacement-ticket")
            .unwrap();
        assert_eq!(
            session
                .handle_fake_event(&mut pool, final_fake.next_event(), Some(&retry), 0)
                .unwrap(),
            VirtualPlayerEvent::Failed
        );
        assert_eq!(session.state(), VirtualPlayerSessionState::Failed);
        assert_eq!(session.pending_requests(), 0);
        assert_eq!(fake.active_connections(), 0);
        assert!(pool.acquire("account-a", "replacement", 1, 1_000).is_ok());
    }

    #[test]
    fn failed_outbound_write_clears_request_and_drops_transport_before_reconnect() {
        let mut pool = AccountLeasePool::default();
        let mut session = leased_session(&mut pool);
        advance_to_ticket(&mut session, &mut pool);
        let fake = FakeKcpService::scripted_events([]);
        let auth = session
            .connect_and_begin_auth(&mut pool, fake.connect(), "ephemeral-ticket")
            .unwrap();
        assert_eq!(
            session
                .handle_outbound_write_failure(&mut pool, &auth, 0)
                .unwrap(),
            VirtualPlayerEvent::ReconnectScheduled {
                attempt: 1,
                delay_ms: 90,
            }
        );
        assert_eq!(session.pending_requests(), 0);
        assert_eq!(session.state(), VirtualPlayerSessionState::Reconnecting);
        assert_eq!(fake.active_connections(), 0);
        assert!(session.lease_held());
    }

    #[test]
    fn invalid_transitions_and_protocol_errors_release_the_lease_and_transport() {
        let mut pool = AccountLeasePool::default();
        let mut session = leased_session(&mut pool);
        assert!(matches!(
            session.activate(&mut pool),
            Err(VirtualPlayerError::InvalidState {
                operation: "activate",
                state: VirtualPlayerSessionState::AccountLeased,
            })
        ));
        assert_eq!(session.state(), VirtualPlayerSessionState::Failed);
        assert!(pool.acquire("account-a", "replacement", 1, 1_000).is_ok());

        let mut session = VirtualPlayerSession::new(
            pool.acquire("account-b", "player-b", 1, 1_000).unwrap(),
            1_024,
            policy(1),
        )
        .unwrap();
        advance_to_ticket(&mut session, &mut pool);
        let fake = FakeKcpService::scripted_events([]);
        let auth = session
            .connect_and_begin_auth(&mut pool, fake.connect(), "ephemeral-ticket")
            .unwrap();
        assert!(matches!(
            session.handle_packet(
                &mut pool,
                Packet::new(
                    game_protocol::PacketHeader {
                        msg_type: MessageType::AuthRes as u16,
                        seq: auth.seq(),
                        body_len: 1,
                    },
                    vec![0],
                ),
            ),
            Err(VirtualPlayerError::Game(GameKcpError::InvalidAuthResponse))
        ));
        assert_eq!(session.state(), VirtualPlayerSessionState::Failed);
        assert_eq!(fake.active_connections(), 0);
        assert!(pool.acquire("account-b", "replacement", 2, 1_000).is_ok());
    }
}
