//! Guarded game-session runner boundary.
//!
//! This module deliberately defines the transport contract before adding a
//! live KCP implementation. The deterministic implementation below never
//! opens a socket, but exercises the same virtual-player lifecycle that a
//! future KCP transport must drive.

use std::time::{Duration, Instant};

use game_protocol::{MessageType, Packet, read_packet};
use thiserror::Error;
use tokio::time::timeout;
use tokio_kcp::KcpStream;

use crate::accounts::{AccountLease, AccountLeasePool};
use crate::config::{LoadTestConfig, RunAccess};
use crate::fake::{FakeKcpEvent, FakeKcpService};
use crate::game_kcp::{GameProxyEndpoint, KcpConnector, OutboundPacket, ReconnectPolicy};
use crate::virtual_player::{
    PlayerConnection, VirtualPlayerError, VirtualPlayerEvent, VirtualPlayerSession,
    VirtualPlayerSessionState,
};

/// Explicit live-game execution admission. The CLI will construct this only
/// after it has loaded and validated its profile, manifest, and private config.
#[derive(Debug, Clone, Copy)]
pub struct GameExecutionGate<'a> {
    pub execute_game: bool,
    pub confirm_game: Option<&'a str>,
    pub environment: &'a str,
    pub account_manifest_supplied: bool,
    pub private_config_supplied: bool,
}

impl GameExecutionGate<'_> {
    pub fn validate(self) -> Result<(), GameLiveError> {
        if !self.execute_game {
            return Err(GameLiveError::Gate(
                "real game execution requires --execute-game",
            ));
        }
        if self.confirm_game != Some(self.environment) {
            return Err(GameLiveError::Gate(
                "real game execution requires --confirm-game <environment>",
            ));
        }
        if !self.account_manifest_supplied {
            return Err(GameLiveError::Gate(
                "real game execution requires --account-manifest <credential-free manifest>",
            ));
        }
        if !self.private_config_supplied {
            return Err(GameLiveError::Gate(
                "real game execution requires --private-config with secret references",
            ));
        }
        Ok(())
    }
}

/// Transport ownership remains in the runner so a real KCP stream can perform
/// framed reads and writes. `VirtualPlayerSession` receives the connection only
/// as a resource-release guard and never exposes a ticket or transport body.
pub trait GameTransport {
    type Connection: PlayerConnection + 'static;

    fn connect(&mut self) -> Result<Self::Connection, GameLiveError>;
    fn send(&mut self, packet: &OutboundPacket) -> Result<(), GameLiveError>;
    fn receive_for(&mut self, outbound: &OutboundPacket) -> Result<Packet, GameLiveError>;
}

#[derive(Debug, Error)]
pub enum GameLiveError {
    #[error("game execution gate rejected: {0}")]
    Gate(&'static str),
    #[error("game transport failed: {0}")]
    Transport(&'static str),
    #[error("game session lifecycle failed")]
    Session(#[source] VirtualPlayerError),
    #[error("game transport returned an unexpected lifecycle event")]
    UnexpectedLifecycleEvent,
}

impl From<VirtualPlayerError> for GameLiveError {
    fn from(error: VirtualPlayerError) -> Self {
        Self::Session(error)
    }
}

/// Publicly reportable runner trace. It intentionally has no credential,
/// packet-body, account ID, or character ID field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameRunnerStep {
    AccountLeased,
    LoggedIn,
    CharacterSelected,
    TicketIssued,
    AuthRequestSent,
    GameAuthenticated,
    Active,
    HeartbeatSent,
    HeartbeatAcknowledged,
    Leaving,
    Closed,
    Failed,
}

/// A live-runner safety boundary. Only outbound player requests consume the
/// business-message quota; read-side checks still enforce cancellation and
/// target protection without charging a message that was never sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameRunnerCheckpoint {
    Control,
    OutboundMessage,
}

/// The guarded minimal KCP flow has three safety-only checkpoints and exactly
/// two outbound player messages. Keeping this sequence in one place lets the
/// offline admission test prove reads do not consume message quota.
const MINIMAL_LIVE_KCP_CHECKPOINTS: [GameRunnerCheckpoint; 5] = [
    GameRunnerCheckpoint::Control,
    GameRunnerCheckpoint::OutboundMessage,
    GameRunnerCheckpoint::Control,
    GameRunnerCheckpoint::OutboundMessage,
    GameRunnerCheckpoint::Control,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameRunResult {
    pub steps: Vec<GameRunnerStep>,
    pub terminal_state: VirtualPlayerSessionState,
    pub connection_released: bool,
    pub lease_released: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct GameSessionRunner {
    pub max_body_len: usize,
    pub reconnect_policy: ReconnectPolicy,
}

impl GameSessionRunner {
    /// Runs the smallest protected online sequence after auth has acquired a
    /// ticket: `AuthReq -> AuthRes -> heartbeat -> close`. The ticket only
    /// exists in the local request bytes required for `AuthReq` construction.
    pub fn run_guarded<T: GameTransport>(
        &self,
        gate: GameExecutionGate<'_>,
        transport: &mut T,
        pool: &mut AccountLeasePool,
        lease: AccountLease,
        ticket: &str,
    ) -> Result<GameRunResult, GameLiveError> {
        let mut steps = vec![GameRunnerStep::AccountLeased];
        let mut session =
            VirtualPlayerSession::new(lease, self.max_body_len, self.reconnect_policy)?;
        if let Err(error) = gate.validate() {
            // The caller already leased the account before it could choose a
            // runner. Rejecting a CLI gate must therefore release that lease
            // even though no transport has been constructed.
            session.close(pool);
            return Err(error);
        }
        self.advance_authenticated_state(&mut session, pool, &mut steps)?;

        let connection = transport.connect()?;
        let auth = session.connect_and_begin_auth(pool, connection, ticket)?;
        if let Err(error) = transport.send(&auth) {
            self.close_after_write_failure(&mut session, pool, &auth);
            return Err(error);
        }
        steps.push(GameRunnerStep::AuthRequestSent);

        let auth_packet = match transport.receive_for(&auth) {
            Ok(packet) => packet,
            Err(error) => {
                self.close_after_timeout(&mut session, pool, &auth);
                return Err(error);
            }
        };
        match session.handle_packet(pool, auth_packet)? {
            VirtualPlayerEvent::GameAuthenticated => steps.push(GameRunnerStep::GameAuthenticated),
            VirtualPlayerEvent::Failed => {
                steps.push(GameRunnerStep::Failed);
                return Ok(result(session, steps));
            }
            _ => {
                session.close(pool);
                return Err(GameLiveError::UnexpectedLifecycleEvent);
            }
        }

        session.activate(pool)?;
        steps.push(GameRunnerStep::Active);
        let heartbeat = session.begin_heartbeat(pool, 0)?;
        if let Err(error) = transport.send(&heartbeat) {
            self.close_after_write_failure(&mut session, pool, &heartbeat);
            return Err(error);
        }
        steps.push(GameRunnerStep::HeartbeatSent);
        let heartbeat_packet = match transport.receive_for(&heartbeat) {
            Ok(packet) => packet,
            Err(error) => {
                self.close_after_timeout(&mut session, pool, &heartbeat);
                return Err(error);
            }
        };
        match session.handle_packet(pool, heartbeat_packet)? {
            VirtualPlayerEvent::HeartbeatAcknowledged => {
                steps.push(GameRunnerStep::HeartbeatAcknowledged)
            }
            VirtualPlayerEvent::Failed => {
                steps.push(GameRunnerStep::Failed);
                return Ok(result(session, steps));
            }
            _ => {
                session.close(pool);
                return Err(GameLiveError::UnexpectedLifecycleEvent);
            }
        }
        session.begin_leaving(pool)?;
        steps.push(GameRunnerStep::Leaving);
        session.close(pool);
        steps.push(GameRunnerStep::Closed);
        Ok(result(session, steps))
    }

    fn advance_authenticated_state(
        &self,
        session: &mut VirtualPlayerSession,
        pool: &mut AccountLeasePool,
        steps: &mut Vec<GameRunnerStep>,
    ) -> Result<(), GameLiveError> {
        session.mark_logged_in(pool)?;
        steps.push(GameRunnerStep::LoggedIn);
        session.mark_character_selected(pool)?;
        steps.push(GameRunnerStep::CharacterSelected);
        session.mark_ticket_issued(pool)?;
        steps.push(GameRunnerStep::TicketIssued);
        Ok(())
    }

    fn close_after_write_failure(
        &self,
        session: &mut VirtualPlayerSession,
        pool: &mut AccountLeasePool,
        outbound: &OutboundPacket,
    ) {
        let _ = session.handle_outbound_write_failure(pool, outbound, 0);
        session.close(pool);
    }

    fn close_after_timeout(
        &self,
        session: &mut VirtualPlayerSession,
        pool: &mut AccountLeasePool,
        outbound: &OutboundPacket,
    ) {
        let _ = session.handle_request_timeout(pool, outbound, 0);
        session.close(pool);
    }

    /// Uses only the formal KCP player ingress connector. The live adapter
    /// owns the stream while the virtual player owns a release guard, which
    /// keeps transport I/O explicit and ticket-free outside `AuthReq` bytes.
    pub async fn run_live_kcp(
        &self,
        gate: GameExecutionGate<'_>,
        transport: &mut LiveKcpTransport,
        config: &LoadTestConfig,
        access: RunAccess<'_>,
        endpoint: &GameProxyEndpoint,
        pool: &mut AccountLeasePool,
        lease: AccountLease,
        ticket: &str,
        mut checkpoint: impl FnMut(GameRunnerCheckpoint) -> Result<(), GameLiveError>,
    ) -> Result<GameRunResult, GameLiveError> {
        let mut steps = vec![GameRunnerStep::AccountLeased];
        let mut session =
            VirtualPlayerSession::new(lease, self.max_body_len, self.reconnect_policy)?;
        if let Err(error) = gate.validate() {
            session.close(pool);
            return Err(error);
        }
        if let Err(error) = self.advance_authenticated_state(&mut session, pool, &mut steps) {
            session.close(pool);
            return Err(error);
        }
        if let Err(error) = run_checkpoint(&mut checkpoint, MINIMAL_LIVE_KCP_CHECKPOINTS[0]) {
            session.close(pool);
            return Err(error);
        }

        let connection = match transport.connect(config, access, endpoint).await {
            Ok(connection) => connection,
            Err(error) => {
                session.close(pool);
                return Err(error);
            }
        };
        let auth = match session.connect_and_begin_auth(pool, connection, ticket) {
            Ok(outbound) => outbound,
            Err(error) => {
                transport.close();
                session.close(pool);
                return Err(error.into());
            }
        };
        if let Err(error) = run_checkpoint(&mut checkpoint, MINIMAL_LIVE_KCP_CHECKPOINTS[1]) {
            transport.close();
            session.close(pool);
            return Err(error);
        }
        if let Err(error) = transport.send(&auth).await {
            self.close_after_write_failure(&mut session, pool, &auth);
            transport.close();
            return Err(error);
        }
        steps.push(GameRunnerStep::AuthRequestSent);
        if let Err(error) = run_checkpoint(&mut checkpoint, MINIMAL_LIVE_KCP_CHECKPOINTS[2]) {
            self.close_after_timeout(&mut session, pool, &auth);
            transport.close();
            return Err(error);
        }
        let auth_packet = match transport.receive().await {
            Ok(packet) => packet,
            Err(error) => {
                self.close_after_timeout(&mut session, pool, &auth);
                transport.close();
                return Err(error);
            }
        };
        match session.handle_packet(pool, auth_packet) {
            Ok(VirtualPlayerEvent::GameAuthenticated) => {
                steps.push(GameRunnerStep::GameAuthenticated)
            }
            Ok(VirtualPlayerEvent::Failed) => {
                steps.push(GameRunnerStep::Failed);
                transport.close();
                return Ok(result(session, steps));
            }
            Ok(_) => {
                transport.close();
                session.close(pool);
                return Err(GameLiveError::UnexpectedLifecycleEvent);
            }
            Err(error) => {
                transport.close();
                session.close(pool);
                return Err(error.into());
            }
        }

        if let Err(error) = session.activate(pool) {
            transport.close();
            session.close(pool);
            return Err(error.into());
        }
        steps.push(GameRunnerStep::Active);
        let heartbeat = match session.begin_heartbeat(pool, 0) {
            Ok(outbound) => outbound,
            Err(error) => {
                transport.close();
                session.close(pool);
                return Err(error.into());
            }
        };
        if let Err(error) = run_checkpoint(&mut checkpoint, MINIMAL_LIVE_KCP_CHECKPOINTS[3]) {
            self.close_after_write_failure(&mut session, pool, &heartbeat);
            transport.close();
            return Err(error);
        }
        if let Err(error) = transport.send(&heartbeat).await {
            self.close_after_write_failure(&mut session, pool, &heartbeat);
            transport.close();
            return Err(error);
        }
        steps.push(GameRunnerStep::HeartbeatSent);
        if let Err(error) = run_checkpoint(&mut checkpoint, MINIMAL_LIVE_KCP_CHECKPOINTS[4]) {
            self.close_after_timeout(&mut session, pool, &heartbeat);
            transport.close();
            return Err(error);
        }
        let heartbeat_packet = match transport.receive().await {
            Ok(packet) => packet,
            Err(error) => {
                self.close_after_timeout(&mut session, pool, &heartbeat);
                transport.close();
                return Err(error);
            }
        };
        match session.handle_packet(pool, heartbeat_packet) {
            Ok(VirtualPlayerEvent::HeartbeatAcknowledged) => {
                steps.push(GameRunnerStep::HeartbeatAcknowledged)
            }
            Ok(VirtualPlayerEvent::Failed) => {
                steps.push(GameRunnerStep::Failed);
                transport.close();
                return Ok(result(session, steps));
            }
            Ok(_) => {
                transport.close();
                session.close(pool);
                return Err(GameLiveError::UnexpectedLifecycleEvent);
            }
            Err(error) => {
                transport.close();
                session.close(pool);
                return Err(error.into());
            }
        }
        if let Err(error) = session.begin_leaving(pool) {
            transport.close();
            session.close(pool);
            return Err(error.into());
        }
        steps.push(GameRunnerStep::Leaving);
        transport.close();
        session.close(pool);
        steps.push(GameRunnerStep::Closed);
        Ok(result(session, steps))
    }
}

fn run_checkpoint(
    checkpoint: &mut impl FnMut(GameRunnerCheckpoint) -> Result<(), GameLiveError>,
    kind: GameRunnerCheckpoint,
) -> Result<(), GameLiveError> {
    checkpoint(kind)
}

fn result(session: VirtualPlayerSession, steps: Vec<GameRunnerStep>) -> GameRunResult {
    GameRunResult {
        terminal_state: session.state(),
        connection_released: !session.connection_attached(),
        lease_released: !session.lease_held(),
        steps,
    }
}

/// Deterministic no-network transport for dry-run and lifecycle tests.
#[derive(Debug, Clone)]
pub struct FakeGameTransport {
    service: FakeKcpService,
    sent: Vec<(MessageType, u32)>,
}

/// The actual KCP transport adapter. Construction makes no network request;
/// `connect` calls the existing guarded `KcpConnector` only after all CLI and
/// profile gates have completed. There is intentionally no TCP implementation.
pub struct LiveKcpTransport {
    connector: KcpConnector,
    stream: Option<KcpStream>,
    deadline: Instant,
    max_body_len: usize,
}

/// Resource-release guard held by `VirtualPlayerSession`; the adapter retains
/// stream ownership so it can execute shared framed reads and writes.
pub struct LiveKcpConnection;

impl PlayerConnection for LiveKcpConnection {}

impl LiveKcpTransport {
    pub fn new(deadline: Instant, max_body_len: usize) -> Result<Self, GameLiveError> {
        if max_body_len == 0 {
            return Err(GameLiveError::Transport("invalid KCP body limit"));
        }
        Ok(Self {
            connector: KcpConnector,
            stream: None,
            deadline,
            max_body_len,
        })
    }

    pub async fn connect(
        &mut self,
        config: &LoadTestConfig,
        access: RunAccess<'_>,
        endpoint: &GameProxyEndpoint,
    ) -> Result<LiveKcpConnection, GameLiveError> {
        if self.stream.is_some() {
            return Err(GameLiveError::Transport(
                "KCP transport is already connected",
            ));
        }
        // KcpConnector repeats structural/access checks and validates every
        // resolved DNS address before calling KcpStream::connect.
        let remaining = self.remaining()?;
        let stream = timeout(remaining, self.connector.connect(config, access, endpoint))
            .await
            .map_err(|_| GameLiveError::Transport("KCP connection deadline elapsed"))?
            .map_err(|_| GameLiveError::Transport("KCP connection failed"))?;
        self.stream = Some(stream);
        Ok(LiveKcpConnection)
    }

    pub async fn send(&mut self, outbound: &OutboundPacket) -> Result<(), GameLiveError> {
        let remaining = self.remaining()?;
        let stream = self
            .stream
            .as_mut()
            .ok_or(GameLiveError::Transport("KCP transport is not connected"))?;
        timeout(remaining, outbound.write_to(stream))
            .await
            .map_err(|_| GameLiveError::Transport("KCP write deadline elapsed"))?
            .map_err(|_| GameLiveError::Transport("KCP write failed"))
    }

    pub async fn receive(&mut self) -> Result<Packet, GameLiveError> {
        let remaining = self.remaining()?;
        let stream = self
            .stream
            .as_mut()
            .ok_or(GameLiveError::Transport("KCP transport is not connected"))?;
        timeout(remaining, read_packet(stream, self.max_body_len))
            .await
            .map_err(|_| GameLiveError::Transport("KCP read deadline elapsed"))?
            .map_err(|_| GameLiveError::Transport("KCP read failed"))?
            .ok_or(GameLiveError::Transport("KCP peer disconnected"))
    }

    pub fn close(&mut self) {
        self.stream.take();
    }

    fn remaining(&self) -> Result<Duration, GameLiveError> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(GameLiveError::Transport("KCP session deadline elapsed"));
        }
        Ok(remaining)
    }
}

impl FakeGameTransport {
    pub fn scripted(events: impl IntoIterator<Item = FakeKcpEvent>) -> Self {
        Self {
            service: FakeKcpService::scripted_events(events),
            sent: Vec::new(),
        }
    }

    pub fn active_connections(&self) -> usize {
        self.service.active_connections()
    }

    pub fn sent(&self) -> &[(MessageType, u32)] {
        &self.sent
    }
}

impl GameTransport for FakeGameTransport {
    type Connection = crate::fake::FakeKcpConnection;

    fn connect(&mut self) -> Result<Self::Connection, GameLiveError> {
        Ok(self.service.connect())
    }

    fn send(&mut self, packet: &OutboundPacket) -> Result<(), GameLiveError> {
        self.sent.push((packet.message_type(), packet.seq()));
        Ok(())
    }

    fn receive_for(&mut self, outbound: &OutboundPacket) -> Result<Packet, GameLiveError> {
        self.service
            .next_event()
            .packet_for(outbound.seq())
            .ok_or(GameLiveError::Transport(
                "fake transport has no response packet",
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_http::AuthDispatchAdmission;
    use crate::config::HardBudget;

    fn gate() -> GameExecutionGate<'static> {
        GameExecutionGate {
            execute_game: true,
            confirm_game: Some("local"),
            environment: "local",
            account_manifest_supplied: true,
            private_config_supplied: true,
        }
    }

    fn runner() -> GameSessionRunner {
        GameSessionRunner {
            max_body_len: 1_024,
            reconnect_policy: ReconnectPolicy {
                max_attempts: 1,
                base_delay_ms: 10,
                max_delay_ms: 10,
                max_jitter_ms: 0,
            },
        }
    }

    fn lease(pool: &mut AccountLeasePool) -> AccountLease {
        pool.acquire("account-a", "player-a", 0, 1_000).unwrap()
    }

    #[test]
    fn minimal_live_flow_admits_only_its_two_outbound_messages() {
        let budget = HardBudget {
            max_virtual_players: 1,
            max_login_qps: 1.0,
            max_new_connections_per_second: 1.0,
            max_business_messages_per_second: 10.0,
            max_messages_per_connection_per_second: 10.0,
            max_duration_secs: 10,
            max_total_operations: 3,
            max_error_rate: 0.1,
            max_connection_failure_rate: 0.1,
            max_p99_ms: 1_000,
            max_data_writes: 0,
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut admission = AuthDispatchAdmission::new(&budget).unwrap();
        admission
            .admit_game_connection(deadline, || Ok(()))
            .unwrap();

        for checkpoint in MINIMAL_LIVE_KCP_CHECKPOINTS {
            run_checkpoint(
                &mut |kind| {
                    if kind == GameRunnerCheckpoint::OutboundMessage {
                        admission
                            .admit_game_message(deadline, || Ok(()))
                            .map_err(|_| GameLiveError::Transport("game admission failed"))?;
                    }
                    Ok(())
                },
                checkpoint,
            )
            .unwrap();
        }

        assert_eq!(admission.used_operations(), 3);
        assert_eq!(admission.used_data_writes(), 0);
    }

    #[test]
    fn fake_runner_drives_auth_heartbeat_and_close_without_retaining_ticket() {
        let mut pool = AccountLeasePool::default();
        let mut transport = FakeGameTransport::scripted([
            FakeKcpEvent::Authenticated,
            FakeKcpEvent::HeartbeatAcknowledged,
        ]);
        let account_lease = lease(&mut pool);
        let result = runner()
            .run_guarded(
                gate(),
                &mut transport,
                &mut pool,
                account_lease,
                "private-ticket",
            )
            .unwrap();

        assert_eq!(
            result.steps,
            vec![
                GameRunnerStep::AccountLeased,
                GameRunnerStep::LoggedIn,
                GameRunnerStep::CharacterSelected,
                GameRunnerStep::TicketIssued,
                GameRunnerStep::AuthRequestSent,
                GameRunnerStep::GameAuthenticated,
                GameRunnerStep::Active,
                GameRunnerStep::HeartbeatSent,
                GameRunnerStep::HeartbeatAcknowledged,
                GameRunnerStep::Leaving,
                GameRunnerStep::Closed,
            ]
        );
        assert_eq!(
            transport.sent(),
            &[(MessageType::AuthReq, 1), (MessageType::PingReq, 2),]
        );
        assert_eq!(result.terminal_state, VirtualPlayerSessionState::Closed);
        assert!(result.connection_released);
        assert!(result.lease_released);
        assert_eq!(transport.active_connections(), 0);
        assert!(!format!("{result:?}").contains("private-ticket"));
        assert!(pool.acquire("account-a", "replacement", 1, 1_000).is_ok());
    }

    #[test]
    fn auth_rejection_releases_fake_connection_and_account_lease() {
        let mut pool = AccountLeasePool::default();
        let mut transport = FakeGameTransport::scripted([FakeKcpEvent::AuthRejected {
            error_code: "INVALID_TICKET_SIGNATURE".into(),
        }]);
        let account_lease = lease(&mut pool);
        let result = runner()
            .run_guarded(
                gate(),
                &mut transport,
                &mut pool,
                account_lease,
                "private-ticket",
            )
            .unwrap();

        assert_eq!(result.steps.last(), Some(&GameRunnerStep::Failed));
        assert_eq!(result.terminal_state, VirtualPlayerSessionState::Failed);
        assert!(result.connection_released);
        assert!(result.lease_released);
        assert_eq!(transport.active_connections(), 0);
        assert!(pool.acquire("account-a", "replacement", 1, 1_000).is_ok());
    }

    #[test]
    fn gate_rejects_before_creating_a_transport_connection() {
        let mut pool = AccountLeasePool::default();
        let mut transport = FakeGameTransport::scripted([]);
        let mut rejected = gate();
        rejected.execute_game = false;
        let account_lease = lease(&mut pool);
        let error = runner()
            .run_guarded(
                rejected,
                &mut transport,
                &mut pool,
                account_lease,
                "private-ticket",
            )
            .unwrap_err();

        assert!(error.to_string().contains("--execute-game"));
        assert_eq!(transport.active_connections(), 0);
        assert!(pool.acquire("account-a", "replacement", 1, 1_000).is_ok());
    }
}
