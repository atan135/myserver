//! Guarded game-session runner boundary.
//!
//! This module deliberately defines the transport contract before adding a
//! live KCP implementation. The deterministic implementation below never
//! opens a socket, but exercises the same virtual-player lifecycle that a
//! future KCP transport must drive.

use std::fmt::Display;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use game_protocol::{MessageType, Packet, read_packet};
use prost::Message;
use thiserror::Error;
use tokio::time::{sleep, timeout};
use tokio_kcp::KcpStream;

use crate::accounts::{AccountLease, AccountLeasePool};
use crate::config::{LiveGameplayCoordination, LiveGameplayScenario, LoadTestConfig, RunAccess};
use crate::fake::{FakeKcpEvent, FakeKcpService};
use crate::game_kcp::{GameProxyEndpoint, KcpConnector, OutboundPacket, ReconnectPolicy};
use crate::gameplay::{
    CoordinatedGameplayPacket, GameplayError, GameplayProfilePlan, PlannedPacket, RoomFlowTracker,
    room_reconnect_step,
};
use crate::pb::{
    PlayerInputReq, PlayerInputRes, RoomJoinRes, RoomLeaveRes, RoomReadyRes, RoomReconnectReq,
    RoomReconnectRes, RoomStartRes,
};
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
    #[error("system clock is unavailable for live gameplay timestamps")]
    Clock,
    #[error("game session lifecycle failed")]
    Session(#[source] VirtualPlayerError),
    #[error("gameplay flow failed: {0}")]
    Gameplay(#[from] GameplayError),
    #[error("live gameplay flow failed: {message}")]
    GameplayFailed {
        message: String,
        metrics: crate::metrics::MetricsSnapshot,
        failure_category: Option<&'static str>,
    },
    #[error("game transport returned an unexpected lifecycle event")]
    UnexpectedLifecycleEvent,
}

impl From<VirtualPlayerError> for GameLiveError {
    fn from(error: VirtualPlayerError) -> Self {
        Self::Session(error)
    }
}

impl GameLiveError {
    /// Partial gameplay telemetry stays low-cardinality and is retained for a
    /// failed flow, while the caller still reports the session as failed.
    pub fn gameplay_metrics(&self) -> Option<&crate::metrics::MetricsSnapshot> {
        match self {
            Self::GameplayFailed { metrics, .. } => Some(metrics),
            _ => None,
        }
    }

    /// A deliberately small, public-safe set of gameplay failure categories.
    /// Arbitrary server error codes must never become report dimensions.
    pub fn reportable_failure_category(&self) -> Option<&'static str> {
        match self {
            Self::GameplayFailed {
                failure_category, ..
            } => *failure_category,
            _ => None,
        }
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
    RoomJoined,
    RoomReady,
    RoomStarted,
    FrameInputAcknowledged,
    FrameBundleReceived,
    KcpReconnected,
    RoomReconnected,
    RoomLeft,
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
    GameplayOutboundMessage,
    ReconnectConnection,
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

#[derive(Debug, Clone, PartialEq)]
pub struct GameRunResult {
    pub steps: Vec<GameRunnerStep>,
    pub terminal_state: VirtualPlayerSessionState,
    pub connection_released: bool,
    pub lease_released: bool,
    pub gameplay_metrics: Option<crate::metrics::MetricsSnapshot>,
}

/// A controlled multiplayer run owns both player sessions for its entire
/// lifetime. It has no identity or ticket fields; the caller retains those
/// only until each session builds its AuthReq.
#[derive(Debug, Clone, PartialEq)]
pub struct TwoPlayerGameRunResult {
    pub players: Vec<GameRunResult>,
}

#[derive(Debug, Clone)]
struct PreparedLiveGameplay {
    approved_room_id: String,
    before_reconnect: Vec<PlannedPacket>,
    leave: PlannedPacket,
    reconnect_cursor: Option<u64>,
}

#[derive(Debug, Clone)]
struct PreparedTwoPlayerGameplay {
    approved_room_id: String,
    packets: Vec<CoordinatedGameplayPacket>,
}

const DEFAULT_MATCH_INPUT_DELAY_FRAMES: u32 = 2;
const DEFAULT_MATCH_MAX_FRAME_DRAIN_PACKETS: usize = 64;
const DEFAULT_MATCH_FRAME_DRAIN_IDLE_MS: u64 = 1;

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
                return Ok(result(session, steps, None));
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
                return Ok(result(session, steps, None));
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
        Ok(result(session, steps, None))
    }

    /// Deterministic no-network analogue of the two-player coordinator. It
    /// exists only to exercise generated protobuf sequencing and the shared
    /// two-lease cleanup contract; the production path below owns real KCP
    /// transports and independently enforces deadlines/checkpoints.
    #[cfg(test)]
    fn run_guarded_two_player_default_match<T: GameTransport>(
        &self,
        gate: GameExecutionGate<'_>,
        transports: [&mut T; 2],
        pool: &mut AccountLeasePool,
        leases: [AccountLease; 2],
        tickets: [&str; 2],
        gameplay: &LiveGameplayScenario,
    ) -> Result<TwoPlayerGameRunResult, GameLiveError> {
        let prepared = prepare_two_player_live_gameplay(gameplay)?;
        let mut sessions = [
            VirtualPlayerSession::new(leases[0].clone(), self.max_body_len, self.reconnect_policy)?,
            VirtualPlayerSession::new(leases[1].clone(), self.max_body_len, self.reconnect_policy)?,
        ];
        let mut steps = [
            vec![GameRunnerStep::AccountLeased],
            vec![GameRunnerStep::AccountLeased],
        ];
        let mut trackers = [RoomFlowTracker::default(), RoomFlowTracker::default()];
        if let Err(error) = gate.validate() {
            close_guarded_two_player_sessions(&mut sessions, pool);
            return Err(error);
        }
        for player_index in 0..2 {
            if let Err(error) = self.advance_authenticated_state(
                &mut sessions[player_index],
                pool,
                &mut steps[player_index],
            ) {
                close_guarded_two_player_sessions(&mut sessions, pool);
                return Err(error);
            }
            let connection = match transports[player_index].connect() {
                Ok(connection) => connection,
                Err(error) => {
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(error);
                }
            };
            let auth = match sessions[player_index].connect_and_begin_auth(
                pool,
                connection,
                tickets[player_index],
            ) {
                Ok(packet) => packet,
                Err(error) => {
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(error.into());
                }
            };
            if let Err(error) = transports[player_index].send(&auth) {
                self.close_after_write_failure(&mut sessions[player_index], pool, &auth);
                close_guarded_two_player_sessions(&mut sessions, pool);
                return Err(error);
            }
            steps[player_index].push(GameRunnerStep::AuthRequestSent);
            let auth_packet = match transports[player_index].receive_for(&auth) {
                Ok(packet) => packet,
                Err(error) => {
                    self.close_after_timeout(&mut sessions[player_index], pool, &auth);
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(error);
                }
            };
            match sessions[player_index].handle_packet(pool, auth_packet) {
                Ok(VirtualPlayerEvent::GameAuthenticated) => {
                    steps[player_index].push(GameRunnerStep::GameAuthenticated)
                }
                Ok(_) => {
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(GameLiveError::UnexpectedLifecycleEvent);
                }
                Err(error) => {
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(error.into());
                }
            }
            if let Err(error) = sessions[player_index].activate(pool) {
                close_guarded_two_player_sessions(&mut sessions, pool);
                return Err(error.into());
            }
            steps[player_index].push(GameRunnerStep::Active);
            let heartbeat = match sessions[player_index].begin_heartbeat(pool, 0) {
                Ok(packet) => packet,
                Err(error) => {
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(error.into());
                }
            };
            if let Err(error) = transports[player_index].send(&heartbeat) {
                self.close_after_write_failure(&mut sessions[player_index], pool, &heartbeat);
                close_guarded_two_player_sessions(&mut sessions, pool);
                return Err(error);
            }
            steps[player_index].push(GameRunnerStep::HeartbeatSent);
            let heartbeat_packet = match transports[player_index].receive_for(&heartbeat) {
                Ok(packet) => packet,
                Err(error) => {
                    self.close_after_timeout(&mut sessions[player_index], pool, &heartbeat);
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(error);
                }
            };
            match sessions[player_index].handle_packet(pool, heartbeat_packet) {
                Ok(VirtualPlayerEvent::HeartbeatAcknowledged) => {
                    steps[player_index].push(GameRunnerStep::HeartbeatAcknowledged)
                }
                Ok(_) => {
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(GameLiveError::UnexpectedLifecycleEvent);
                }
                Err(error) => {
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(error.into());
                }
            }
        }
        for coordinated in &prepared.packets {
            let player_index = coordinated.player_index;
            let step = coordinated.packet.step.clone();
            let outbound = match prepare_outbound_gameplay_packet_with_clock(
                &mut sessions[player_index],
                pool,
                &coordinated.packet,
                || Ok(1_700_000_000_000),
            ) {
                Ok(packet) => packet,
                Err(error) => {
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(error);
                }
            };
            if let Err(error) = transports[player_index].send(&outbound) {
                self.close_after_write_failure(&mut sessions[player_index], pool, &outbound);
                close_guarded_two_player_sessions(&mut sessions, pool);
                return Err(error);
            }
            let planned = match planned_packet_with_live_sequence(&outbound, step.clone()) {
                Ok(packet) => packet,
                Err(error) => {
                    self.close_after_timeout(&mut sessions[player_index], pool, &outbound);
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(gameplay_failure(error, &trackers[player_index]));
                }
            };
            if let Err(error) =
                trackers[player_index].begin_planned_action(&planned, monotonic_ms())
            {
                self.close_after_timeout(&mut sessions[player_index], pool, &outbound);
                close_guarded_two_player_sessions(&mut sessions, pool);
                return Err(gameplay_failure(error, &trackers[player_index]));
            }
            let packet = match transports[player_index].receive_for(&outbound) {
                Ok(packet) => packet,
                Err(error) => {
                    self.close_after_timeout(&mut sessions[player_index], pool, &outbound);
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(gameplay_failure(error, &trackers[player_index]));
                }
            };
            if let Err(error) = ensure_approved_room_packet(&packet, &prepared.approved_room_id) {
                let failure_category = reportable_room_failure_category(&packet);
                close_guarded_two_player_sessions(&mut sessions, pool);
                return Err(gameplay_failure_with_category(
                    error,
                    &trackers[player_index],
                    failure_category,
                ));
            }
            match sessions[player_index].handle_packet(pool, packet.clone()) {
                Ok(VirtualPlayerEvent::Response { message_type, .. })
                    if message_type == step.response_type.expect("validated gameplay step") => {}
                Ok(_) => {
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(GameLiveError::UnexpectedLifecycleEvent);
                }
                Err(error) => {
                    close_guarded_two_player_sessions(&mut sessions, pool);
                    return Err(gameplay_failure(error, &trackers[player_index]));
                }
            }
            if let Err(error) = trackers[player_index].ingest(packet, monotonic_ms()) {
                close_guarded_two_player_sessions(&mut sessions, pool);
                return Err(gameplay_failure(error, &trackers[player_index]));
            }
            match step.request_type {
                MessageType::RoomJoinReq => steps[player_index].push(GameRunnerStep::RoomJoined),
                MessageType::RoomReadyReq => steps[player_index].push(GameRunnerStep::RoomReady),
                MessageType::RoomStartReq => steps[player_index].push(GameRunnerStep::RoomStarted),
                MessageType::PlayerInputReq => {
                    steps[player_index].push(GameRunnerStep::FrameInputAcknowledged)
                }
                MessageType::RoomLeaveReq => steps[player_index].push(GameRunnerStep::RoomLeft),
                _ => unreachable!("two-player packet plan is closed"),
            }
        }
        for player_index in 0..2 {
            if let Err(error) = sessions[player_index].begin_leaving(pool) {
                close_guarded_two_player_sessions(&mut sessions, pool);
                return Err(error.into());
            }
            steps[player_index].push(GameRunnerStep::Leaving);
            sessions[player_index].close(pool);
            steps[player_index].push(GameRunnerStep::Closed);
        }
        let [session_one, session_two] = sessions;
        let [steps_one, steps_two] = steps;
        let [tracker_one, tracker_two] = trackers;
        Ok(TwoPlayerGameRunResult {
            players: vec![
                result(session_one, steps_one, Some(tracker_one.telemetry())),
                result(session_two, steps_two, Some(tracker_two.telemetry())),
            ],
        })
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
        let gameplay = config
            .scenario
            .live_gameplay
            .as_ref()
            .map(prepare_live_gameplay)
            .transpose()?;
        let mut steps = vec![GameRunnerStep::AccountLeased];
        let reconnect_policy = config
            .scenario
            .live_gameplay
            .as_ref()
            .and_then(|gameplay| gameplay.reconnect)
            .map(|reconnect| reconnect.reconnect_policy.into())
            .unwrap_or(self.reconnect_policy);
        let mut session = VirtualPlayerSession::new(lease, self.max_body_len, reconnect_policy)?;
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
                return Ok(result(session, steps, None));
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
                return Ok(result(session, steps, None));
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
        let gameplay_metrics = if let Some(gameplay) = gameplay.as_ref() {
            let mut tracker = RoomFlowTracker::default();
            for packet in &gameplay.before_reconnect {
                let step = packet.step.clone();
                let step_timeout_ms = step.timeout_ms;
                let frame_bundles_before = tracker.metrics().frame_bundles_received;
                let packet = match prepare_admitted_outbound_gameplay_packet(
                    &mut session,
                    pool,
                    packet,
                    &mut checkpoint,
                ) {
                    Ok(packet) => packet,
                    Err(error) => {
                        transport.close();
                        session.close(pool);
                        return Err(error);
                    }
                };
                if let Err(error) = transport.send(&packet).await {
                    self.close_after_write_failure(&mut session, pool, &packet);
                    transport.close();
                    return Err(error);
                }
                let now_ms = monotonic_ms();
                let planned = match planned_packet_with_live_sequence(&packet, step) {
                    Ok(planned) => planned,
                    Err(error) => {
                        self.close_after_timeout(&mut session, pool, &packet);
                        transport.close();
                        return Err(gameplay_failure(error, &tracker));
                    }
                };
                if let Err(error) = tracker.begin_planned_action(&planned, now_ms) {
                    self.close_after_timeout(&mut session, pool, &packet);
                    transport.close();
                    return Err(gameplay_failure(error, &tracker));
                }
                let response = match receive_gameplay_response(
                    transport,
                    &mut session,
                    pool,
                    &mut tracker,
                    &packet,
                    step_timeout_ms,
                    Some(&gameplay.approved_room_id),
                    &mut checkpoint,
                )
                .await
                {
                    Ok(response) => response,
                    Err(error) => {
                        transport.close();
                        return Err(error);
                    }
                };
                match response {
                    MessageType::RoomJoinRes => {
                        steps.push(GameRunnerStep::RoomJoined);
                    }
                    MessageType::RoomReadyRes => steps.push(GameRunnerStep::RoomReady),
                    MessageType::RoomStartRes => steps.push(GameRunnerStep::RoomStarted),
                    MessageType::PlayerInputRes => {
                        steps.push(GameRunnerStep::FrameInputAcknowledged);
                        if tracker.metrics().frame_bundles_received == frame_bundles_before {
                            receive_frame_bundle(
                                transport,
                                &mut session,
                                pool,
                                &mut tracker,
                                step_timeout_ms,
                                &mut checkpoint,
                            )
                            .await?;
                        }
                        steps.push(GameRunnerStep::FrameBundleReceived);
                    }
                    MessageType::RoomLeaveRes => steps.push(GameRunnerStep::RoomLeft),
                    _ => {}
                }
            }
            if let Some(cursor) = gameplay.reconnect_cursor {
                transport.close();
                let delay_ms = match session.handle_disconnect(pool, 0) {
                    Ok(VirtualPlayerEvent::ReconnectScheduled { delay_ms, .. }) => delay_ms,
                    Ok(VirtualPlayerEvent::Failed) => {
                        steps.push(GameRunnerStep::Failed);
                        return Ok(result(session, steps, Some(tracker.telemetry())));
                    }
                    Ok(_) => {
                        session.close(pool);
                        return Err(GameLiveError::UnexpectedLifecycleEvent);
                    }
                    Err(error) => {
                        session.close(pool);
                        return Err(error.into());
                    }
                };
                if let Err(error) = run_checkpoint(&mut checkpoint, GameRunnerCheckpoint::Control) {
                    session.close(pool);
                    return Err(gameplay_failure(error, &tracker));
                }
                let remaining = match transport.remaining() {
                    Ok(remaining) => remaining,
                    Err(error) => {
                        transport.close();
                        session.close(pool);
                        return Err(gameplay_failure(error, &tracker));
                    }
                };
                if timeout(remaining, sleep(Duration::from_millis(delay_ms)))
                    .await
                    .is_err()
                {
                    transport.close();
                    session.close(pool);
                    return Err(gameplay_failure(
                        "KCP reconnect backoff exceeded action deadline",
                        &tracker,
                    ));
                }
                if let Err(error) = run_checkpoint(&mut checkpoint, GameRunnerCheckpoint::Control) {
                    transport.close();
                    session.close(pool);
                    return Err(gameplay_failure(error, &tracker));
                }
                let connection = match reconnect_live_transport(
                    transport,
                    config,
                    access,
                    endpoint,
                    &mut checkpoint,
                )
                .await
                {
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
                if let Err(error) =
                    run_checkpoint(&mut checkpoint, GameRunnerCheckpoint::OutboundMessage)
                {
                    self.close_after_write_failure(&mut session, pool, &auth);
                    transport.close();
                    return Err(error);
                }
                if let Err(error) = transport.send(&auth).await {
                    self.close_after_write_failure(&mut session, pool, &auth);
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
                        steps.push(GameRunnerStep::KcpReconnected)
                    }
                    Ok(VirtualPlayerEvent::Failed) => {
                        steps.push(GameRunnerStep::Failed);
                        transport.close();
                        return Ok(result(session, steps, Some(tracker.telemetry())));
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
                let reconnect_profile = match reconnect_profile_packet(cursor) {
                    Ok(packet) => packet,
                    Err(error) => {
                        transport.close();
                        session.close(pool);
                        return Err(error);
                    }
                };
                let reconnect = match prepare_admitted_outbound_gameplay_packet(
                    &mut session,
                    pool,
                    &reconnect_profile,
                    &mut checkpoint,
                ) {
                    Ok(packet) => packet,
                    Err(error) => {
                        transport.close();
                        session.close(pool);
                        return Err(error);
                    }
                };
                if let Err(error) = transport.send(&reconnect).await {
                    self.close_after_write_failure(&mut session, pool, &reconnect);
                    transport.close();
                    return Err(error);
                }
                let reconnect_step = match reconnect_profile_packet(cursor) {
                    Ok(packet) => packet.step,
                    Err(error) => {
                        transport.close();
                        session.close(pool);
                        return Err(gameplay_failure(error, &tracker));
                    }
                };
                let reconnect_planned =
                    match planned_packet_with_live_sequence(&reconnect, reconnect_step) {
                        Ok(packet) => packet,
                        Err(error) => {
                            transport.close();
                            session.close(pool);
                            return Err(gameplay_failure(error, &tracker));
                        }
                    };
                if let Err(error) = tracker.begin_planned_action(&reconnect_planned, monotonic_ms())
                {
                    self.close_after_timeout(&mut session, pool, &reconnect);
                    transport.close();
                    return Err(error.into());
                }
                let response = receive_gameplay_response(
                    transport,
                    &mut session,
                    pool,
                    &mut tracker,
                    &reconnect,
                    reconnect_planned.step.timeout_ms,
                    Some(&gameplay.approved_room_id),
                    &mut checkpoint,
                )
                .await?;
                if response != MessageType::RoomReconnectRes {
                    transport.close();
                    session.close(pool);
                    return Err(GameLiveError::UnexpectedLifecycleEvent);
                }
                steps.push(GameRunnerStep::RoomReconnected);
            }
            let leave = match prepare_admitted_outbound_gameplay_packet(
                &mut session,
                pool,
                &gameplay.leave,
                &mut checkpoint,
            ) {
                Ok(leave) => leave,
                Err(error) => {
                    transport.close();
                    session.close(pool);
                    return Err(gameplay_failure(error, &tracker));
                }
            };
            if let Err(error) = transport.send(&leave).await {
                self.close_after_write_failure(&mut session, pool, &leave);
                transport.close();
                return Err(error);
            }
            let leave_planned =
                match planned_packet_with_live_sequence(&leave, gameplay.leave.step.clone()) {
                    Ok(packet) => packet,
                    Err(error) => {
                        transport.close();
                        session.close(pool);
                        return Err(gameplay_failure(error, &tracker));
                    }
                };
            if let Err(error) = tracker.begin_planned_action(&leave_planned, monotonic_ms()) {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure(error, &tracker));
            }
            let response = receive_gameplay_response(
                transport,
                &mut session,
                pool,
                &mut tracker,
                &leave,
                gameplay.leave.step.timeout_ms,
                Some(&gameplay.approved_room_id),
                &mut checkpoint,
            )
            .await?;
            if response != MessageType::RoomLeaveRes {
                transport.close();
                session.close(pool);
                return Err(GameLiveError::UnexpectedLifecycleEvent);
            }
            steps.push(GameRunnerStep::RoomLeft);
            Some(tracker.telemetry())
        } else {
            None
        };
        if let Err(error) = session.begin_leaving(pool) {
            transport.close();
            session.close(pool);
            return Err(error.into());
        }
        steps.push(GameRunnerStep::Leaving);
        transport.close();
        session.close(pool);
        steps.push(GameRunnerStep::Closed);
        Ok(result(session, steps, gameplay_metrics))
    }

    /// Runs the explicitly opt-in two-account `default_match` smoke. Both
    /// sessions stay leased until the whole room lifecycle has reached a
    /// terminal state, so a failure in either leg always closes and releases
    /// both players before returning to the auth/logout owner.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_live_two_player_default_match_kcp(
        &self,
        gate: GameExecutionGate<'_>,
        mut transports: [&mut LiveKcpTransport; 2],
        config: &LoadTestConfig,
        access: RunAccess<'_>,
        endpoint: &GameProxyEndpoint,
        pool: &mut AccountLeasePool,
        leases: [AccountLease; 2],
        tickets: [&str; 2],
        mut checkpoint: impl FnMut(GameRunnerCheckpoint) -> Result<(), GameLiveError>,
    ) -> Result<TwoPlayerGameRunResult, GameLiveError> {
        let gameplay = config
            .scenario
            .live_gameplay
            .as_ref()
            .filter(|gameplay| {
                gameplay.coordination == LiveGameplayCoordination::TwoPlayerDefaultMatch
            })
            .ok_or(GameLiveError::Transport(
                "two-player runner requires explicit default_match coordination",
            ))?;
        let prepared = prepare_two_player_live_gameplay(gameplay)?;
        let mut sessions = [
            VirtualPlayerSession::new(leases[0].clone(), self.max_body_len, self.reconnect_policy)?,
            VirtualPlayerSession::new(leases[1].clone(), self.max_body_len, self.reconnect_policy)?,
        ];
        let mut steps = [
            vec![GameRunnerStep::AccountLeased],
            vec![GameRunnerStep::AccountLeased],
        ];
        let mut trackers = [RoomFlowTracker::default(), RoomFlowTracker::default()];

        if let Err(error) = gate.validate() {
            close_two_player_sessions(&mut sessions, &mut transports, pool);
            return Err(error);
        }
        for player_index in 0..2 {
            if let Err(error) = self.advance_authenticated_state(
                &mut sessions[player_index],
                pool,
                &mut steps[player_index],
            ) {
                close_two_player_sessions(&mut sessions, &mut transports, pool);
                return Err(error);
            }
            if let Err(error) = run_checkpoint(&mut checkpoint, GameRunnerCheckpoint::Control) {
                close_two_player_sessions(&mut sessions, &mut transports, pool);
                return Err(error);
            }
            let connection = match transports[player_index]
                .connect(config, access, endpoint)
                .await
            {
                Ok(connection) => connection,
                Err(error) => {
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(error);
                }
            };
            let auth = match sessions[player_index].connect_and_begin_auth(
                pool,
                connection,
                tickets[player_index],
            ) {
                Ok(packet) => packet,
                Err(error) => {
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(error.into());
                }
            };
            if let Err(error) =
                run_checkpoint(&mut checkpoint, GameRunnerCheckpoint::OutboundMessage)
            {
                self.close_after_write_failure(&mut sessions[player_index], pool, &auth);
                close_two_player_sessions(&mut sessions, &mut transports, pool);
                return Err(error);
            }
            if let Err(error) = transports[player_index].send(&auth).await {
                self.close_after_write_failure(&mut sessions[player_index], pool, &auth);
                close_two_player_sessions(&mut sessions, &mut transports, pool);
                return Err(error);
            }
            steps[player_index].push(GameRunnerStep::AuthRequestSent);
            let auth_packet = match transports[player_index].receive().await {
                Ok(packet) => packet,
                Err(error) => {
                    self.close_after_timeout(&mut sessions[player_index], pool, &auth);
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(error);
                }
            };
            match sessions[player_index].handle_packet(pool, auth_packet) {
                Ok(VirtualPlayerEvent::GameAuthenticated) => {
                    steps[player_index].push(GameRunnerStep::GameAuthenticated)
                }
                Ok(VirtualPlayerEvent::Failed) => {
                    steps[player_index].push(GameRunnerStep::Failed);
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(GameLiveError::Transport(
                        "two-player KCP authentication failed",
                    ));
                }
                Ok(_) => {
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(GameLiveError::UnexpectedLifecycleEvent);
                }
                Err(error) => {
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(error.into());
                }
            }
            if let Err(error) = sessions[player_index].activate(pool) {
                close_two_player_sessions(&mut sessions, &mut transports, pool);
                return Err(error.into());
            }
            steps[player_index].push(GameRunnerStep::Active);
            let heartbeat = match sessions[player_index].begin_heartbeat(pool, 0) {
                Ok(packet) => packet,
                Err(error) => {
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(error.into());
                }
            };
            if let Err(error) =
                run_checkpoint(&mut checkpoint, GameRunnerCheckpoint::OutboundMessage)
            {
                self.close_after_write_failure(&mut sessions[player_index], pool, &heartbeat);
                close_two_player_sessions(&mut sessions, &mut transports, pool);
                return Err(error);
            }
            if let Err(error) = transports[player_index].send(&heartbeat).await {
                self.close_after_write_failure(&mut sessions[player_index], pool, &heartbeat);
                close_two_player_sessions(&mut sessions, &mut transports, pool);
                return Err(error);
            }
            steps[player_index].push(GameRunnerStep::HeartbeatSent);
            let heartbeat_packet = match transports[player_index].receive().await {
                Ok(packet) => packet,
                Err(error) => {
                    self.close_after_timeout(&mut sessions[player_index], pool, &heartbeat);
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(error);
                }
            };
            match sessions[player_index].handle_packet(pool, heartbeat_packet) {
                Ok(VirtualPlayerEvent::HeartbeatAcknowledged) => {
                    steps[player_index].push(GameRunnerStep::HeartbeatAcknowledged)
                }
                Ok(_) => {
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(GameLiveError::UnexpectedLifecycleEvent);
                }
                Err(error) => {
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(error.into());
                }
            }
        }

        let mut packet_index = 0;
        let mut frame_bundles_before_inputs = None;
        while packet_index < prepared.packets.len() {
            let coordinated = &prepared.packets[packet_index];
            if coordinated.packet.step.request_type == MessageType::PlayerInputReq {
                let input_packets = prepared
                    .packets
                    .get(packet_index..packet_index + 2)
                    .filter(|packets| {
                        packets[0].player_index == 0
                            && packets[1].player_index == 1
                            && packets.iter().all(|packet| {
                                packet.packet.step.request_type == MessageType::PlayerInputReq
                            })
                    })
                    .ok_or(GameLiveError::Transport(
                        "two-player default_match plan has an invalid input phase",
                    ))?;

                // Admission can wait behind the global live-run limiter. Do
                // it for both players before observing the current room frame
                // so the target does not expire while the second input waits.
                for _ in 0..2 {
                    if let Err(error) = run_checkpoint(
                        &mut checkpoint,
                        GameRunnerCheckpoint::GameplayOutboundMessage,
                    ) {
                        close_two_player_sessions(&mut sessions, &mut transports, pool);
                        return Err(error);
                    }
                }
                for player_index in 0..2 {
                    let error = drain_default_match_frame_bundles(
                        transports[player_index],
                        &mut sessions[player_index],
                        pool,
                        &mut trackers[player_index],
                        &mut checkpoint,
                    )
                    .await;
                    if let Err(error) = error {
                        close_two_player_sessions(&mut sessions, &mut transports, pool);
                        return Err(error);
                    }
                    steps[player_index].push(GameRunnerStep::FrameBundleReceived);
                }
                let input_frame_id = default_match_shared_input_frame_id(&trackers)?;
                frame_bundles_before_inputs = Some([
                    trackers[0].metrics().frame_bundles_received,
                    trackers[1].metrics().frame_bundles_received,
                ]);
                let first_step = input_packets[0].packet.step.clone();
                let second_step = input_packets[1].packet.step.clone();
                let first_outbound = match prepare_outbound_gameplay_packet_with_clock_and_frame(
                    &mut sessions[0],
                    pool,
                    &input_packets[0].packet,
                    Some(input_frame_id),
                    current_unix_ms,
                ) {
                    Ok(packet) => packet,
                    Err(error) => {
                        close_two_player_sessions(&mut sessions, &mut transports, pool);
                        return Err(error);
                    }
                };
                let second_outbound = match prepare_outbound_gameplay_packet_with_clock_and_frame(
                    &mut sessions[1],
                    pool,
                    &input_packets[1].packet,
                    Some(input_frame_id),
                    current_unix_ms,
                ) {
                    Ok(packet) => packet,
                    Err(error) => {
                        self.close_after_timeout(&mut sessions[0], pool, &first_outbound);
                        close_two_player_sessions(&mut sessions, &mut transports, pool);
                        return Err(error);
                    }
                };
                let outbounds = [first_outbound, second_outbound];
                let steps_for_inputs = [first_step, second_step];

                // Both inputs share a frame and are sent before either
                // response is awaited. Waiting for the first acknowledgement
                // here can advance the strict room past the second input.
                for player_index in 0..2 {
                    if let Err(error) = transports[player_index]
                        .send(&outbounds[player_index])
                        .await
                    {
                        self.close_after_write_failure(
                            &mut sessions[player_index],
                            pool,
                            &outbounds[player_index],
                        );
                        close_two_player_sessions(&mut sessions, &mut transports, pool);
                        return Err(error);
                    }
                    let planned = match planned_packet_with_live_sequence(
                        &outbounds[player_index],
                        steps_for_inputs[player_index].clone(),
                    ) {
                        Ok(packet) => packet,
                        Err(error) => {
                            self.close_after_timeout(
                                &mut sessions[player_index],
                                pool,
                                &outbounds[player_index],
                            );
                            close_two_player_sessions(&mut sessions, &mut transports, pool);
                            return Err(gameplay_failure(error, &trackers[player_index]));
                        }
                    };
                    if let Err(error) =
                        trackers[player_index].begin_planned_action(&planned, monotonic_ms())
                    {
                        self.close_after_timeout(
                            &mut sessions[player_index],
                            pool,
                            &outbounds[player_index],
                        );
                        close_two_player_sessions(&mut sessions, &mut transports, pool);
                        return Err(gameplay_failure(error, &trackers[player_index]));
                    }
                }
                for player_index in 0..2 {
                    let response = receive_gameplay_response(
                        transports[player_index],
                        &mut sessions[player_index],
                        pool,
                        &mut trackers[player_index],
                        &outbounds[player_index],
                        steps_for_inputs[player_index].timeout_ms,
                        Some(&prepared.approved_room_id),
                        &mut checkpoint,
                    )
                    .await;
                    match response {
                        Ok(MessageType::PlayerInputRes) => {
                            steps[player_index].push(GameRunnerStep::FrameInputAcknowledged);
                        }
                        Ok(_) => {
                            close_two_player_sessions(&mut sessions, &mut transports, pool);
                            return Err(GameLiveError::UnexpectedLifecycleEvent);
                        }
                        Err(error) => {
                            close_two_player_sessions(&mut sessions, &mut transports, pool);
                            return Err(error);
                        }
                    }
                }
                packet_index += 2;
                continue;
            }
            // The final two planned packets are the leaves. Both inputs must
            // have been acknowledged and observed in a frame bundle before a
            // participant is allowed to leave the shared match.
            if packet_index == prepared.packets.len() - 2 {
                let frame_bundles_before_inputs =
                    frame_bundles_before_inputs.ok_or(GameLiveError::Transport(
                        "two-player default_match reached leave before the input phase",
                    ))?;
                for player_index in 0..2 {
                    if !received_frame_after_input(
                        &trackers[player_index],
                        frame_bundles_before_inputs[player_index],
                    ) {
                        let error = receive_frame_bundle(
                            transports[player_index],
                            &mut sessions[player_index],
                            pool,
                            &mut trackers[player_index],
                            crate::gameplay::DEFAULT_STEP_TIMEOUT_MS,
                            &mut checkpoint,
                        )
                        .await;
                        if let Err(error) = error {
                            close_two_player_sessions(&mut sessions, &mut transports, pool);
                            return Err(error);
                        }
                        if !received_frame_after_input(
                            &trackers[player_index],
                            frame_bundles_before_inputs[player_index],
                        ) {
                            close_two_player_sessions(&mut sessions, &mut transports, pool);
                            return Err(GameLiveError::Transport(
                                "two-player default_match did not receive a post-input frame",
                            ));
                        }
                        steps[player_index].push(GameRunnerStep::FrameBundleReceived);
                    }
                }
            }
            let player_index = coordinated.player_index;
            let step = coordinated.packet.step.clone();
            let outbound = match prepare_admitted_outbound_gameplay_packet(
                &mut sessions[player_index],
                pool,
                &coordinated.packet,
                &mut checkpoint,
            ) {
                Ok(packet) => packet,
                Err(error) => {
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(error);
                }
            };
            if let Err(error) = transports[player_index].send(&outbound).await {
                self.close_after_write_failure(&mut sessions[player_index], pool, &outbound);
                close_two_player_sessions(&mut sessions, &mut transports, pool);
                return Err(error);
            }
            let planned = match planned_packet_with_live_sequence(&outbound, step.clone()) {
                Ok(packet) => packet,
                Err(error) => {
                    self.close_after_timeout(&mut sessions[player_index], pool, &outbound);
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(gameplay_failure(error, &trackers[player_index]));
                }
            };
            if let Err(error) =
                trackers[player_index].begin_planned_action(&planned, monotonic_ms())
            {
                self.close_after_timeout(&mut sessions[player_index], pool, &outbound);
                close_two_player_sessions(&mut sessions, &mut transports, pool);
                return Err(gameplay_failure(error, &trackers[player_index]));
            }
            let response = receive_gameplay_response(
                transports[player_index],
                &mut sessions[player_index],
                pool,
                &mut trackers[player_index],
                &outbound,
                step.timeout_ms,
                Some(&prepared.approved_room_id),
                &mut checkpoint,
            )
            .await;
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(error);
                }
            };
            match response {
                MessageType::RoomJoinRes => steps[player_index].push(GameRunnerStep::RoomJoined),
                MessageType::RoomReadyRes => steps[player_index].push(GameRunnerStep::RoomReady),
                MessageType::RoomStartRes => steps[player_index].push(GameRunnerStep::RoomStarted),
                MessageType::PlayerInputRes => {
                    steps[player_index].push(GameRunnerStep::FrameInputAcknowledged);
                }
                MessageType::RoomLeaveRes => steps[player_index].push(GameRunnerStep::RoomLeft),
                _ => {
                    close_two_player_sessions(&mut sessions, &mut transports, pool);
                    return Err(GameLiveError::UnexpectedLifecycleEvent);
                }
            }
            packet_index += 1;
        }
        for player_index in 0..2 {
            if let Err(error) = sessions[player_index].begin_leaving(pool) {
                close_two_player_sessions(&mut sessions, &mut transports, pool);
                return Err(error.into());
            }
            steps[player_index].push(GameRunnerStep::Leaving);
        }
        close_two_player_transports(&mut transports);
        for session in &mut sessions {
            session.close(pool);
        }
        steps[0].push(GameRunnerStep::Closed);
        steps[1].push(GameRunnerStep::Closed);
        let [session_one, session_two] = sessions;
        let [steps_one, steps_two] = steps;
        let [tracker_one, tracker_two] = trackers;
        Ok(TwoPlayerGameRunResult {
            players: vec![
                result(session_one, steps_one, Some(tracker_one.telemetry())),
                result(session_two, steps_two, Some(tracker_two.telemetry())),
            ],
        })
    }
}

#[cfg(test)]
fn close_guarded_two_player_sessions(
    sessions: &mut [VirtualPlayerSession; 2],
    pool: &mut AccountLeasePool,
) {
    for session in sessions {
        session.close(pool);
    }
}

fn run_checkpoint(
    checkpoint: &mut impl FnMut(GameRunnerCheckpoint) -> Result<(), GameLiveError>,
    kind: GameRunnerCheckpoint,
) -> Result<(), GameLiveError> {
    checkpoint(kind)
}

fn prepare_live_gameplay(
    gameplay: &LiveGameplayScenario,
) -> Result<PreparedLiveGameplay, GameLiveError> {
    let profile = GameplayProfilePlan::from_lockstep_scenario_json(
        gameplay.profile,
        &gameplay.lockstep_scenario_json,
    )?;
    let mut packets = profile.packet_plan_with_input_limit(
        &gameplay.room_id,
        &gameplay.policy_id,
        gameplay.max_frame_inputs,
    )?;
    let leave = packets
        .pop()
        .filter(|packet| packet.step.request_type == MessageType::RoomLeaveReq)
        .ok_or(GameLiveError::Transport(
            "live gameplay plan has no room leave",
        ))?;
    Ok(PreparedLiveGameplay {
        approved_room_id: gameplay.room_id.clone(),
        before_reconnect: packets,
        leave,
        reconnect_cursor: gameplay
            .reconnect
            .as_ref()
            .map(|reconnect| reconnect.last_character_push_sequence),
    })
}

fn prepare_two_player_live_gameplay(
    gameplay: &LiveGameplayScenario,
) -> Result<PreparedTwoPlayerGameplay, GameLiveError> {
    if gameplay.coordination != LiveGameplayCoordination::TwoPlayerDefaultMatch
        || gameplay.policy_id != "default_match"
        || gameplay.reconnect.is_some()
    {
        return Err(GameLiveError::Transport(
            "invalid two-player default_match gameplay profile",
        ));
    }
    let profile = GameplayProfilePlan::from_lockstep_scenario_json(
        gameplay.profile,
        &gameplay.lockstep_scenario_json,
    )?;
    Ok(PreparedTwoPlayerGameplay {
        approved_room_id: gameplay.room_id.clone(),
        packets: profile.two_player_default_match_packet_plan(
            &gameplay.room_id,
            &gameplay.policy_id,
            gameplay.max_frame_inputs,
        )?,
    })
}

fn close_two_player_transports(transports: &mut [&mut LiveKcpTransport; 2]) {
    for transport in transports {
        transport.close();
    }
}

fn close_two_player_sessions(
    sessions: &mut [VirtualPlayerSession; 2],
    transports: &mut [&mut LiveKcpTransport; 2],
    pool: &mut AccountLeasePool,
) {
    close_two_player_transports(transports);
    for session in sessions {
        session.close(pool);
    }
}

fn prepare_outbound_gameplay_packet_with_clock(
    session: &mut VirtualPlayerSession,
    pool: &mut AccountLeasePool,
    packet: &PlannedPacket,
    current_unix_ms: impl FnOnce() -> Result<i64, GameLiveError>,
) -> Result<OutboundPacket, GameLiveError> {
    prepare_outbound_gameplay_packet_with_clock_and_frame(
        session,
        pool,
        packet,
        None,
        current_unix_ms,
    )
}

fn prepare_outbound_gameplay_packet_with_clock_and_frame(
    session: &mut VirtualPlayerSession,
    pool: &mut AccountLeasePool,
    packet: &PlannedPacket,
    input_frame_id: Option<u32>,
    current_unix_ms: impl FnOnce() -> Result<i64, GameLiveError>,
) -> Result<OutboundPacket, GameLiveError> {
    let packet = if packet.step.request_type == MessageType::PlayerInputReq {
        materialize_live_gameplay_packet(packet, current_unix_ms()?, input_frame_id)?
    } else {
        packet.clone()
    };
    let expected_response = packet
        .step
        .response_type
        .ok_or(GameplayError::MissingExpectedResponse(packet.step.name))?;
    Ok(session.begin_gameplay_request(
        pool,
        packet.step.request_type,
        expected_response,
        packet.body()?,
    )?)
}

fn default_match_shared_input_frame_id(
    trackers: &[RoomFlowTracker; 2],
) -> Result<u32, GameLiveError> {
    let shared_frame_id = trackers
        .iter()
        .filter_map(RoomFlowTracker::latest_frame_id)
        .max()
        .ok_or(GameLiveError::Transport(
            "two-player default_match has no shared frame observation",
        ))?;
    shared_frame_id
        .checked_add(DEFAULT_MATCH_INPUT_DELAY_FRAMES)
        .ok_or(GameLiveError::Transport(
            "two-player default_match shared frame overflowed",
        ))
}

fn received_frame_after_input(tracker: &RoomFlowTracker, frame_bundles_before_input: u64) -> bool {
    tracker.metrics().frame_bundles_received > frame_bundles_before_input
}

/// Admits a gameplay request before it can reserve an in-flight sequence or
/// materialize a wall-clock-sensitive frame input. This keeps rate-limit
/// waits outside the timestamp-to-send interval.
fn prepare_admitted_outbound_gameplay_packet(
    session: &mut VirtualPlayerSession,
    pool: &mut AccountLeasePool,
    packet: &PlannedPacket,
    checkpoint: &mut impl FnMut(GameRunnerCheckpoint) -> Result<(), GameLiveError>,
) -> Result<OutboundPacket, GameLiveError> {
    prepare_admitted_outbound_gameplay_packet_with_clock(
        session,
        pool,
        packet,
        checkpoint,
        current_unix_ms,
    )
}

fn prepare_admitted_outbound_gameplay_packet_with_clock(
    session: &mut VirtualPlayerSession,
    pool: &mut AccountLeasePool,
    packet: &PlannedPacket,
    checkpoint: &mut impl FnMut(GameRunnerCheckpoint) -> Result<(), GameLiveError>,
    current_unix_ms: impl FnOnce() -> Result<i64, GameLiveError>,
) -> Result<OutboundPacket, GameLiveError> {
    run_checkpoint(checkpoint, GameRunnerCheckpoint::GameplayOutboundMessage)?;
    prepare_outbound_gameplay_packet_with_clock(session, pool, packet, current_unix_ms)
}

/// Keeps profile packet generation deterministic while filling live-only
/// fields immediately before send.
fn materialize_live_gameplay_packet(
    packet: &PlannedPacket,
    unix_ms: i64,
    input_frame_id: Option<u32>,
) -> Result<PlannedPacket, GameLiveError> {
    if packet.step.request_type != MessageType::PlayerInputReq {
        return Ok(packet.clone());
    }
    if unix_ms <= 0 {
        return Err(GameLiveError::Clock);
    }
    let header = packet.packet_header()?;
    if header.msg_type != MessageType::PlayerInputReq as u16 {
        return Err(GameLiveError::Transport(
            "planned player input packet has an unexpected message type",
        ));
    }
    let mut input =
        PlayerInputReq::decode(packet.body()?).map_err(|_| GameplayError::InvalidBody)?;
    input.client_timestamp_ms = unix_ms;
    if let Some(input_frame_id) = input_frame_id {
        input.frame_id = input_frame_id;
    }
    Ok(PlannedPacket {
        step: packet.step.clone(),
        packet: game_protocol::encode_packet(
            MessageType::PlayerInputReq,
            header.seq,
            &game_protocol::encode_body(&input),
        ),
        sequence: packet.sequence,
    })
}

fn current_unix_ms() -> Result<i64, GameLiveError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| GameLiveError::Clock)?
        .as_millis()
        .try_into()
        .map_err(|_| GameLiveError::Clock)
}

fn planned_packet_with_live_sequence(
    outbound: &OutboundPacket,
    step: crate::gameplay::GameplayStep,
) -> Result<PlannedPacket, GameLiveError> {
    let packet = PlannedPacket {
        step,
        packet: game_protocol::encode_packet(
            outbound.message_type(),
            outbound.seq(),
            outbound.body(),
        ),
        sequence: outbound.seq(),
    };
    packet.packet_header()?;
    Ok(packet)
}

fn reconnect_profile_packet(cursor: u64) -> Result<PlannedPacket, GameLiveError> {
    let step = room_reconnect_step(crate::gameplay::DEFAULT_MAX_MESSAGES_PER_CONNECTION_PER_SECOND);
    let packet = PlannedPacket {
        packet: game_protocol::encode_packet(
            MessageType::RoomReconnectReq,
            1,
            &game_protocol::encode_body(&RoomReconnectReq {
                last_character_push_sequence: cursor,
            }),
        ),
        step,
        sequence: 1,
    };
    packet.packet_header()?;
    Ok(packet)
}

async fn receive_gameplay_response(
    transport: &mut LiveKcpTransport,
    session: &mut VirtualPlayerSession,
    pool: &mut AccountLeasePool,
    tracker: &mut RoomFlowTracker,
    outbound: &OutboundPacket,
    step_timeout_ms: u64,
    approved_room_id: Option<&str>,
    checkpoint: &mut impl FnMut(GameRunnerCheckpoint) -> Result<(), GameLiveError>,
) -> Result<MessageType, GameLiveError> {
    let step_deadline = Instant::now() + Duration::from_millis(step_timeout_ms);
    loop {
        if let Err(error) = run_checkpoint(checkpoint, GameRunnerCheckpoint::Control) {
            transport.close();
            session.close(pool);
            return Err(gameplay_failure(error, tracker));
        }
        let packet = match transport.receive_until(step_deadline).await {
            Ok(packet) => packet,
            Err(error) => {
                let _ = session.handle_request_timeout(pool, outbound, 0);
                transport.close();
                session.close(pool);
                return Err(gameplay_failure(error, tracker));
            }
        };
        let message_type = match packet.message_type() {
            Some(message_type) => message_type,
            None => {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure(
                    "KCP gameplay message type is invalid",
                    tracker,
                ));
            }
        };
        if let Some(approved_room_id) = approved_room_id {
            if let Err(error) = ensure_approved_room_packet(&packet, approved_room_id) {
                let failure_category = reportable_room_failure_category(&packet);
                transport.close();
                session.close(pool);
                return Err(gameplay_failure_with_category(
                    error,
                    tracker,
                    failure_category,
                ));
            }
        }
        match session.handle_packet(pool, packet.clone()) {
            Ok(VirtualPlayerEvent::Response {
                message_type: response_type,
                ..
            }) => {
                if response_type != message_type {
                    transport.close();
                    session.close(pool);
                    return Err(gameplay_failure(
                        GameLiveError::UnexpectedLifecycleEvent,
                        tracker,
                    ));
                }
                if let Err(error) = tracker.ingest(packet, monotonic_ms()) {
                    transport.close();
                    session.close(pool);
                    return Err(gameplay_failure(error, tracker));
                }
                return Ok(response_type);
            }
            Ok(VirtualPlayerEvent::Push { .. }) => {
                if let Err(error) = tracker.ingest(packet, monotonic_ms()) {
                    transport.close();
                    session.close(pool);
                    return Err(gameplay_failure(error, tracker));
                }
            }
            Ok(VirtualPlayerEvent::Failed) => {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure("KCP gameplay session failed", tracker));
            }
            Ok(_) => {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure(
                    GameLiveError::UnexpectedLifecycleEvent,
                    tracker,
                ));
            }
            Err(error) => {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure(error, tracker));
            }
        }
    }
}

fn gameplay_failure(message: impl Display, tracker: &RoomFlowTracker) -> GameLiveError {
    gameplay_failure_with_category(message, tracker, None)
}

fn gameplay_failure_with_category(
    message: impl Display,
    tracker: &RoomFlowTracker,
    failure_category: Option<&'static str>,
) -> GameLiveError {
    GameLiveError::GameplayFailed {
        message: message.to_string(),
        metrics: tracker.telemetry(),
        failure_category,
    }
}

async fn receive_frame_bundle(
    transport: &mut LiveKcpTransport,
    session: &mut VirtualPlayerSession,
    pool: &mut AccountLeasePool,
    tracker: &mut RoomFlowTracker,
    step_timeout_ms: u64,
    checkpoint: &mut impl FnMut(GameRunnerCheckpoint) -> Result<(), GameLiveError>,
) -> Result<(), GameLiveError> {
    let step_deadline = Instant::now() + Duration::from_millis(step_timeout_ms);
    loop {
        if let Err(error) = run_checkpoint(checkpoint, GameRunnerCheckpoint::Control) {
            transport.close();
            session.close(pool);
            return Err(gameplay_failure(error, tracker));
        }
        let packet = match transport.receive_until(step_deadline).await {
            Ok(packet) => packet,
            Err(error) => {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure(error, tracker));
            }
        };
        if packet.message_type().is_none() {
            transport.close();
            session.close(pool);
            return Err(gameplay_failure(
                "KCP frame message type is invalid",
                tracker,
            ));
        }
        match session.handle_packet(pool, packet.clone()) {
            Ok(VirtualPlayerEvent::Push {
                message_type: MessageType::FrameBundlePush,
                ..
            }) => {
                if let Err(error) = tracker.ingest(packet, monotonic_ms()) {
                    transport.close();
                    session.close(pool);
                    return Err(gameplay_failure(error, tracker));
                }
                return Ok(());
            }
            Ok(VirtualPlayerEvent::Push { .. }) => {
                if let Err(error) = tracker.ingest(packet, monotonic_ms()) {
                    transport.close();
                    session.close(pool);
                    return Err(gameplay_failure(error, tracker));
                }
            }
            Ok(VirtualPlayerEvent::Failed) => {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure("KCP frame session failed", tracker));
            }
            Ok(_) => {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure(
                    GameLiveError::UnexpectedLifecycleEvent,
                    tracker,
                ));
            }
            Err(error) => {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure(error, tracker));
            }
        }
    }
}

/// Drains the bounded backlog of frame pushes immediately after the two
/// default-match input admissions. An input target derived from just the
/// first queued frame can already be expired by the time it reaches a strict
/// room, so this ends only once the connection has been idle briefly.
async fn drain_default_match_frame_bundles(
    transport: &mut LiveKcpTransport,
    session: &mut VirtualPlayerSession,
    pool: &mut AccountLeasePool,
    tracker: &mut RoomFlowTracker,
    checkpoint: &mut impl FnMut(GameRunnerCheckpoint) -> Result<(), GameLiveError>,
) -> Result<(), GameLiveError> {
    let mut drained_packets = 0;
    loop {
        if let Err(error) = run_checkpoint(checkpoint, GameRunnerCheckpoint::Control) {
            transport.close();
            session.close(pool);
            return Err(gameplay_failure(error, tracker));
        }
        if drained_packets == DEFAULT_MATCH_MAX_FRAME_DRAIN_PACKETS {
            transport.close();
            session.close(pool);
            return Err(gameplay_failure(
                "two-player default_match frame backlog exceeded the drain limit",
                tracker,
            ));
        }
        let packet_deadline =
            Instant::now() + Duration::from_millis(DEFAULT_MATCH_FRAME_DRAIN_IDLE_MS);
        let packet = match transport.receive_until_or_idle(packet_deadline).await {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(error) => {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure(error, tracker));
            }
        };
        drained_packets += 1;
        if packet.message_type().is_none() {
            transport.close();
            session.close(pool);
            return Err(gameplay_failure(
                "KCP default_match frame message type is invalid",
                tracker,
            ));
        }
        match session.handle_packet(pool, packet.clone()) {
            Ok(VirtualPlayerEvent::Push {
                message_type: MessageType::FrameBundlePush,
                ..
            }) => {
                if let Err(error) = tracker.ingest(packet, monotonic_ms()) {
                    transport.close();
                    session.close(pool);
                    return Err(gameplay_failure(error, tracker));
                }
            }
            Ok(VirtualPlayerEvent::Push { .. }) => {
                if let Err(error) = tracker.ingest(packet, monotonic_ms()) {
                    transport.close();
                    session.close(pool);
                    return Err(gameplay_failure(error, tracker));
                }
            }
            Ok(VirtualPlayerEvent::Failed) => {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure(
                    "KCP default_match frame session failed",
                    tracker,
                ));
            }
            Ok(_) => {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure(
                    GameLiveError::UnexpectedLifecycleEvent,
                    tracker,
                ));
            }
            Err(error) => {
                transport.close();
                session.close(pool);
                return Err(gameplay_failure(error, tracker));
            }
        }
    }
    if tracker.latest_frame_id().is_none() {
        transport.close();
        session.close(pool);
        return Err(gameplay_failure(
            "two-player default_match did not receive a frame before input",
            tracker,
        ));
    }
    Ok(())
}

fn ensure_approved_room_packet(
    packet: &Packet,
    approved_room_id: &str,
) -> Result<(), GameplayError> {
    let body = packet.body.as_slice();
    let response_type = packet
        .message_type()
        .ok_or(GameplayError::UnknownMessageType(packet.header.msg_type))?;
    let room_id = match response_type {
        MessageType::RoomJoinRes => {
            let response = RoomJoinRes::decode(body).map_err(|_| GameplayError::InvalidBody)?;
            if !response.ok {
                return Err(GameplayError::BusinessRejected(response_type));
            }
            response.room_id
        }
        MessageType::RoomReconnectRes => {
            let response =
                RoomReconnectRes::decode(body).map_err(|_| GameplayError::InvalidBody)?;
            if !response.ok {
                return Err(GameplayError::BusinessRejected(response_type));
            }
            response.room_id
        }
        MessageType::RoomLeaveRes => {
            let response = RoomLeaveRes::decode(body).map_err(|_| GameplayError::InvalidBody)?;
            if !response.ok {
                return Err(GameplayError::BusinessRejected(response_type));
            }
            response.room_id
        }
        MessageType::PlayerInputRes => {
            let response = PlayerInputRes::decode(body).map_err(|_| GameplayError::InvalidBody)?;
            if !response.ok {
                return Err(GameplayError::BusinessRejected(response_type));
            }
            response.room_id
        }
        MessageType::RoomReadyRes => {
            let response = RoomReadyRes::decode(body).map_err(|_| GameplayError::InvalidBody)?;
            if !response.ok || !response.ready {
                return Err(GameplayError::BusinessRejected(response_type));
            }
            response.room_id
        }
        MessageType::RoomStartRes => {
            let response = RoomStartRes::decode(body).map_err(|_| GameplayError::InvalidBody)?;
            if !response.ok {
                return Err(GameplayError::BusinessRejected(response_type));
            }
            response.room_id
        }
        _ => return Ok(()),
    };
    if room_id != approved_room_id {
        return Err(GameplayError::RoomMismatch);
    }
    Ok(())
}

fn reportable_room_failure_category(packet: &Packet) -> Option<&'static str> {
    if packet.message_type()? != MessageType::PlayerInputRes {
        return None;
    }
    let response = PlayerInputRes::decode(packet.body.as_slice()).ok()?;
    if response.ok {
        return None;
    }
    match response.error_code.as_str() {
        "INPUT_TIMESTAMP_SKEW" => Some("gameplay_input_timestamp_skew"),
        "INPUT_FRAME_EXPIRED" => Some("gameplay_input_frame_expired"),
        _ => None,
    }
}

async fn reconnect_live_transport(
    transport: &mut LiveKcpTransport,
    config: &LoadTestConfig,
    access: RunAccess<'_>,
    endpoint: &GameProxyEndpoint,
    checkpoint: &mut impl FnMut(GameRunnerCheckpoint) -> Result<(), GameLiveError>,
) -> Result<LiveKcpConnection, GameLiveError> {
    run_checkpoint(checkpoint, GameRunnerCheckpoint::ReconnectConnection)?;
    transport.connect(config, access, endpoint).await
}

fn monotonic_ms() -> u64 {
    static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    STARTED.get_or_init(Instant::now).elapsed().as_millis() as u64
}

fn result(
    session: VirtualPlayerSession,
    steps: Vec<GameRunnerStep>,
    gameplay_metrics: Option<crate::metrics::MetricsSnapshot>,
) -> GameRunResult {
    GameRunResult {
        terminal_state: session.state(),
        connection_released: !session.connection_attached(),
        lease_released: !session.lease_held(),
        steps,
        gameplay_metrics,
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
        self.receive_until(self.deadline).await
    }

    pub async fn receive_until(&mut self, deadline: Instant) -> Result<Packet, GameLiveError> {
        let remaining = self.remaining_until(deadline)?;
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

    async fn receive_until_or_idle(
        &mut self,
        deadline: Instant,
    ) -> Result<Option<Packet>, GameLiveError> {
        let remaining = self.remaining_until(deadline)?;
        let stream = self
            .stream
            .as_mut()
            .ok_or(GameLiveError::Transport("KCP transport is not connected"))?;
        match timeout(remaining, read_packet(stream, self.max_body_len)).await {
            Ok(Ok(Some(packet))) => Ok(Some(packet)),
            Ok(Ok(None)) => Err(GameLiveError::Transport("KCP peer disconnected")),
            Ok(Err(_)) => Err(GameLiveError::Transport("KCP read failed")),
            Err(_) => Ok(None),
        }
    }

    pub fn close(&mut self) {
        self.stream.take();
    }

    fn remaining(&self) -> Result<Duration, GameLiveError> {
        self.remaining_until(self.deadline)
    }

    fn remaining_until(&self, deadline: Instant) -> Result<Duration, GameLiveError> {
        let effective_deadline = self.deadline.min(deadline);
        let remaining = effective_deadline.saturating_duration_since(Instant::now());
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
    use crate::config::{
        HardBudget, LiveGameplayCoordination, LiveGameplayReconnect, LiveGameplayScenario,
        ReconnectPolicyConfig,
    };
    use crate::pb::{
        FrameBundlePush, FrameInput, PlayerInputRes, RoomJoinRes, RoomLeaveRes, RoomReadyRes,
        RoomStartRes,
    };
    use prost::Message;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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

    fn gameplay() -> LiveGameplayScenario {
        LiveGameplayScenario {
            room_id: "approved-room".into(),
            policy_id: "approved-policy".into(),
            profile: crate::gameplay::PlayerProfile::Normal,
            lockstep_scenario_json: include_str!("../../lockstep-client/scenarios/move_stop.json")
                .into(),
            max_frame_inputs: 1,
            coordination: LiveGameplayCoordination::SinglePlayer,
            reconnect: Some(LiveGameplayReconnect {
                last_character_push_sequence: 0,
                reconnect_policy: ReconnectPolicyConfig {
                    max_attempts: 1,
                    base_delay_ms: 1,
                    max_delay_ms: 1,
                    max_jitter_ms: 0,
                },
            }),
        }
    }

    fn two_player_gameplay() -> LiveGameplayScenario {
        let mut gameplay = gameplay();
        gameplay.policy_id = "default_match".into();
        gameplay.coordination = LiveGameplayCoordination::TwoPlayerDefaultMatch;
        gameplay.reconnect = None;
        gameplay
    }

    #[derive(Debug)]
    struct ScriptedTwoPlayerConnection {
        active_connections: Arc<AtomicUsize>,
    }

    impl Drop for ScriptedTwoPlayerConnection {
        fn drop(&mut self) {
            self.active_connections.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl PlayerConnection for ScriptedTwoPlayerConnection {}

    #[derive(Debug)]
    struct ScriptedTwoPlayerTransport {
        responses: VecDeque<Result<Packet, GameLiveError>>,
        sent: Vec<(MessageType, u32)>,
        active_connections: Arc<AtomicUsize>,
    }

    impl ScriptedTwoPlayerTransport {
        fn scripted(responses: impl IntoIterator<Item = Result<Packet, GameLiveError>>) -> Self {
            Self {
                responses: responses.into_iter().collect(),
                sent: Vec::new(),
                active_connections: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn active_connections(&self) -> usize {
            self.active_connections.load(Ordering::SeqCst)
        }
    }

    impl GameTransport for ScriptedTwoPlayerTransport {
        type Connection = ScriptedTwoPlayerConnection;

        fn connect(&mut self) -> Result<Self::Connection, GameLiveError> {
            self.active_connections.fetch_add(1, Ordering::SeqCst);
            Ok(ScriptedTwoPlayerConnection {
                active_connections: self.active_connections.clone(),
            })
        }

        fn send(&mut self, packet: &OutboundPacket) -> Result<(), GameLiveError> {
            self.sent.push((packet.message_type(), packet.seq()));
            Ok(())
        }

        fn receive_for(&mut self, _outbound: &OutboundPacket) -> Result<Packet, GameLiveError> {
            self.responses
                .pop_front()
                .unwrap_or(Err(GameLiveError::Transport("scripted two-player timeout")))
        }
    }

    fn authenticated_response(sequence: u32) -> Packet {
        response(
            MessageType::AuthRes,
            sequence,
            &crate::pb::AuthRes {
                ok: true,
                player_id: "fake".into(),
                error_code: String::new(),
                server_protocol_version: 1,
                minimum_client_protocol_version: 1,
                upgrade_message: String::new(),
                upgrade_url: String::new(),
            },
        )
    }

    fn heartbeat_response(sequence: u32) -> Packet {
        response(
            MessageType::PingRes,
            sequence,
            &crate::pb::PingRes { server_time: 1 },
        )
    }

    fn room_join_response(sequence: u32, ok: bool) -> Packet {
        response(
            MessageType::RoomJoinRes,
            sequence,
            &RoomJoinRes {
                ok,
                room_id: "approved-room".into(),
                error_code: String::new(),
            },
        )
    }

    fn room_ready_response(sequence: u32, ok: bool) -> Packet {
        response(
            MessageType::RoomReadyRes,
            sequence,
            &RoomReadyRes {
                ok,
                room_id: "approved-room".into(),
                ready: ok,
                error_code: String::new(),
            },
        )
    }

    fn room_start_response(sequence: u32, ok: bool) -> Packet {
        response(
            MessageType::RoomStartRes,
            sequence,
            &RoomStartRes {
                ok,
                room_id: "approved-room".into(),
                error_code: String::new(),
            },
        )
    }

    fn input_response(sequence: u32, ok: bool) -> Packet {
        input_response_with_error(sequence, ok, "")
    }

    fn input_response_with_error(sequence: u32, ok: bool, error_code: &str) -> Packet {
        response(
            MessageType::PlayerInputRes,
            sequence,
            &PlayerInputRes {
                ok,
                room_id: "approved-room".into(),
                error_code: error_code.into(),
            },
        )
    }

    fn leave_response(sequence: u32) -> Packet {
        response(
            MessageType::RoomLeaveRes,
            sequence,
            &RoomLeaveRes {
                ok: true,
                room_id: "approved-room".into(),
                error_code: String::new(),
            },
        )
    }

    fn response<M: Message>(message_type: MessageType, seq: u32, body: &M) -> Packet {
        let body = game_protocol::encode_body(body);
        Packet::new(
            game_protocol::PacketHeader {
                msg_type: message_type as u16,
                seq,
                body_len: body.len() as u32,
            },
            body,
        )
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

    #[test]
    fn prepared_live_gameplay_is_bounded_and_carries_no_identity_or_ticket() {
        let prepared = prepare_live_gameplay(&gameplay()).unwrap();
        assert_eq!(prepared.before_reconnect.len(), 4);
        assert_eq!(
            prepared.before_reconnect[0].step.request_type,
            MessageType::RoomJoinReq
        );
        assert_eq!(
            prepared.before_reconnect[1].step.request_type,
            MessageType::RoomReadyReq
        );
        assert_eq!(
            prepared.before_reconnect[2].step.request_type,
            MessageType::RoomStartReq
        );
        assert_eq!(
            prepared.before_reconnect[3].step.request_type,
            MessageType::PlayerInputReq
        );
        assert_eq!(prepared.leave.step.request_type, MessageType::RoomLeaveReq);
        assert_eq!(prepared.reconnect_cursor, Some(0));
        let debug = format!("{prepared:?}");
        assert!(!debug.contains("private-ticket"));
        assert!(!debug.contains("account-a"));
    }

    #[test]
    fn room_tracker_handles_a_minimal_shared_packet_flow_and_frame_bundle() {
        let prepared = prepare_live_gameplay(&gameplay()).unwrap();
        let mut tracker = RoomFlowTracker::default();
        let join = prepared.before_reconnect[0].with_sequence(3).unwrap();
        tracker.begin_planned_action(&join, 1).unwrap();
        tracker
            .ingest(
                response(
                    MessageType::RoomJoinRes,
                    3,
                    &RoomJoinRes {
                        ok: true,
                        room_id: "approved-room".into(),
                        error_code: String::new(),
                    },
                ),
                2,
            )
            .unwrap();
        let ready = prepared.before_reconnect[1].with_sequence(4).unwrap();
        tracker.begin_planned_action(&ready, 3).unwrap();
        tracker.ingest(room_ready_response(4, true), 4).unwrap();
        let start = prepared.before_reconnect[2].with_sequence(5).unwrap();
        tracker.begin_planned_action(&start, 5).unwrap();
        tracker.ingest(room_start_response(5, true), 6).unwrap();
        let input = prepared.before_reconnect[3].with_sequence(6).unwrap();
        tracker.begin_planned_action(&input, 7).unwrap();
        tracker
            .ingest(
                response(
                    MessageType::PlayerInputRes,
                    6,
                    &PlayerInputRes {
                        ok: true,
                        room_id: "approved-room".into(),
                        error_code: String::new(),
                    },
                ),
                8,
            )
            .unwrap();
        tracker
            .ingest(
                response(
                    MessageType::FrameBundlePush,
                    0,
                    &FrameBundlePush {
                        room_id: "approved-room".into(),
                        frame_id: 1,
                        fps: 20,
                        inputs: vec![FrameInput {
                            character_id: "ignored-by-metrics".into(),
                            action: "sim_input".into(),
                            payload_json: "{}".into(),
                            frame_id: 1,
                        }],
                        is_silent_frame: false,
                        snapshot: None,
                    },
                ),
                9,
            )
            .unwrap();
        let leave = prepared.leave.with_sequence(7).unwrap();
        tracker.begin_planned_action(&leave, 10).unwrap();
        tracker
            .ingest(
                response(
                    MessageType::RoomLeaveRes,
                    7,
                    &RoomLeaveRes {
                        ok: true,
                        room_id: "approved-room".into(),
                        error_code: String::new(),
                    },
                ),
                11,
            )
            .unwrap();
        let metrics = tracker.metrics();
        assert_eq!(metrics.room_create_or_join, 1);
        assert_eq!(metrics.frame_inputs_sent, 1);
        assert_eq!(metrics.frame_bundles_received, 1);
        assert_eq!(metrics.room_leave, 1);
        assert!(!format!("{:?}", tracker.telemetry()).contains("ignored-by-metrics"));
    }

    #[test]
    fn approved_room_guard_rejects_server_canonicalization_to_another_room() {
        let packet = response(
            MessageType::RoomJoinRes,
            3,
            &RoomJoinRes {
                ok: true,
                room_id: "unexpected-room".into(),
                error_code: String::new(),
            },
        );
        assert!(matches!(
            ensure_approved_room_packet(&packet, "approved-room"),
            Err(GameplayError::RoomMismatch)
        ));
    }

    #[test]
    fn approved_room_guard_rejects_every_unsuccessful_room_response() {
        let packets = [
            room_join_response(3, false),
            response(
                MessageType::RoomLeaveRes,
                3,
                &RoomLeaveRes {
                    ok: false,
                    room_id: "approved-room".into(),
                    error_code: "ignored".into(),
                },
            ),
            room_ready_response(3, false),
            room_start_response(3, false),
            input_response(3, false),
        ];
        for packet in packets {
            assert!(matches!(
                ensure_approved_room_packet(&packet, "approved-room"),
                Err(GameplayError::BusinessRejected(_))
            ));
        }
    }

    #[test]
    fn only_known_input_failures_are_reportable_room_failure_categories() {
        assert_eq!(
            reportable_room_failure_category(&input_response_with_error(
                3,
                false,
                "INPUT_TIMESTAMP_SKEW",
            )),
            Some("gameplay_input_timestamp_skew")
        );
        assert_eq!(
            reportable_room_failure_category(&input_response_with_error(
                3,
                false,
                "INPUT_FRAME_EXPIRED",
            )),
            Some("gameplay_input_frame_expired")
        );
        assert_eq!(
            reportable_room_failure_category(&input_response_with_error(3, false, "UNEXPECTED")),
            None
        );
        assert_eq!(
            reportable_room_failure_category(&input_response(3, true)),
            None
        );
    }

    #[test]
    fn live_packet_materialization_replaces_only_live_input_fields() {
        let profile = GameplayProfilePlan::from_lockstep_scenario_json(
            crate::gameplay::PlayerProfile::Normal,
            include_str!("../../lockstep-client/scenarios/move_stop.json"),
        )
        .unwrap();
        let plan = profile
            .packet_plan_with_input_limit("approved-room", "approved-policy", 1)
            .unwrap();
        let input = &plan[3];
        let materialized =
            materialize_live_gameplay_packet(input, 1_700_000_000_000, Some(8)).unwrap();

        assert_eq!(materialized.step, input.step);
        assert_eq!(materialized.sequence, input.sequence);
        assert_eq!(
            materialized.packet_header().unwrap().msg_type,
            MessageType::PlayerInputReq as u16
        );
        assert_eq!(
            materialized.packet_header().unwrap().seq,
            input.packet_header().unwrap().seq
        );
        assert_eq!(
            PlayerInputReq::decode(materialized.body().unwrap())
                .unwrap()
                .client_timestamp_ms,
            1_700_000_000_000
        );
        assert_eq!(
            PlayerInputReq::decode(materialized.body().unwrap())
                .unwrap()
                .frame_id,
            8
        );
        assert_eq!(
            PlayerInputReq::decode(input.body().unwrap())
                .unwrap()
                .client_timestamp_ms,
            1
        );
        assert_eq!(
            PlayerInputReq::decode(input.body().unwrap())
                .unwrap()
                .frame_id,
            1
        );
        assert_eq!(
            materialize_live_gameplay_packet(&plan[0], 1_700_000_000_000, Some(8)).unwrap(),
            plan[0]
        );
    }

    #[test]
    fn default_match_uses_the_latest_shared_observation_plus_input_delay() {
        let mut first = RoomFlowTracker::default();
        let mut second = RoomFlowTracker::default();
        for (tracker, frame_id) in [(&mut first, 4), (&mut second, 5)] {
            tracker.begin_join(1, 1, 1);
            tracker
                .ingest(
                    response(
                        MessageType::RoomJoinRes,
                        1,
                        &RoomJoinRes {
                            ok: true,
                            room_id: "approved-room".into(),
                            error_code: String::new(),
                        },
                    ),
                    2,
                )
                .unwrap();
            tracker
                .ingest(
                    response(
                        MessageType::FrameBundlePush,
                        0,
                        &FrameBundlePush {
                            room_id: "approved-room".into(),
                            frame_id,
                            fps: 20,
                            inputs: Vec::new(),
                            is_silent_frame: false,
                            snapshot: None,
                        },
                    ),
                    3,
                )
                .unwrap();
        }

        assert_eq!(
            default_match_shared_input_frame_id(&[first, second]).unwrap(),
            7
        );
    }

    #[test]
    fn default_match_frame_target_advances_when_backlog_contains_newer_frames() {
        let mut first = RoomFlowTracker::default();
        let mut second = RoomFlowTracker::default();
        for tracker in [&mut first, &mut second] {
            tracker.begin_join(1, 1, 1);
            tracker
                .ingest(
                    response(
                        MessageType::RoomJoinRes,
                        1,
                        &RoomJoinRes {
                            ok: true,
                            room_id: "approved-room".into(),
                            error_code: String::new(),
                        },
                    ),
                    2,
                )
                .unwrap();
        }
        for (tracker, frame_ids) in [(&mut first, &[70, 97][..]), (&mut second, &[71, 99][..])] {
            for &frame_id in frame_ids {
                tracker
                    .ingest(
                        response(
                            MessageType::FrameBundlePush,
                            0,
                            &FrameBundlePush {
                                room_id: "approved-room".into(),
                                frame_id,
                                fps: 5,
                                inputs: Vec::new(),
                                is_silent_frame: true,
                                snapshot: None,
                            },
                        ),
                        3,
                    )
                    .unwrap();
            }
        }

        assert_eq!(first.latest_frame_id(), Some(97));
        assert_eq!(second.latest_frame_id(), Some(99));
        assert_eq!(
            default_match_shared_input_frame_id(&[first, second]).unwrap(),
            101
        );
    }

    #[test]
    fn gameplay_admission_precedes_input_timestamp_materialization() {
        let profile = GameplayProfilePlan::from_lockstep_scenario_json(
            crate::gameplay::PlayerProfile::Normal,
            include_str!("../../lockstep-client/scenarios/move_stop.json"),
        )
        .unwrap();
        let input = profile
            .packet_plan_with_input_limit("approved-room", "approved-policy", 1)
            .unwrap()
            .remove(3);
        let mut pool = AccountLeasePool::default();
        let mut session = active_session(&mut pool);
        let order = std::cell::RefCell::new(Vec::new());
        let outbound = prepare_admitted_outbound_gameplay_packet_with_clock(
            &mut session,
            &mut pool,
            &input,
            &mut |checkpoint| {
                assert_eq!(checkpoint, GameRunnerCheckpoint::GameplayOutboundMessage);
                order.borrow_mut().push("admit");
                Ok(())
            },
            || {
                assert_eq!(order.borrow().as_slice(), ["admit"]);
                order.borrow_mut().push("clock");
                Ok(1_700_000_000_000)
            },
        )
        .unwrap();

        assert_eq!(order.into_inner(), vec!["admit", "clock"]);
        assert_eq!(
            PlayerInputReq::decode(outbound.body())
                .unwrap()
                .client_timestamp_ms,
            1_700_000_000_000
        );
        session.close(&mut pool);
    }

    #[test]
    fn rejected_gameplay_admission_does_not_materialize_or_reserve_input() {
        let profile = GameplayProfilePlan::from_lockstep_scenario_json(
            crate::gameplay::PlayerProfile::Normal,
            include_str!("../../lockstep-client/scenarios/move_stop.json"),
        )
        .unwrap();
        let input = profile
            .packet_plan_with_input_limit("approved-room", "approved-policy", 1)
            .unwrap()
            .remove(3);
        let mut pool = AccountLeasePool::default();
        let mut session = active_session(&mut pool);
        let clock_called = std::cell::Cell::new(false);
        let error = prepare_admitted_outbound_gameplay_packet_with_clock(
            &mut session,
            &mut pool,
            &input,
            &mut |_| Err(GameLiveError::Transport("test admission rejection")),
            || {
                clock_called.set(true);
                Ok(1_700_000_000_000)
            },
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "game transport failed: test admission rejection"
        );
        assert!(!clock_called.get());
        let retry =
            prepare_outbound_gameplay_packet_with_clock(&mut session, &mut pool, &input, || {
                Ok(1_700_000_000_000)
            })
            .unwrap();
        session
            .handle_outbound_write_failure(&mut pool, &retry, 0)
            .unwrap();
        session.close(&mut pool);
        assert!(pool.acquire("account-a", "replacement", 1, 1_000).is_ok());
    }

    #[test]
    fn two_player_preparation_and_room_echo_cover_ready_and_start() {
        let mut multiplayer = gameplay();
        multiplayer.policy_id = "default_match".into();
        multiplayer.coordination = LiveGameplayCoordination::TwoPlayerDefaultMatch;
        multiplayer.reconnect = None;
        let prepared = prepare_two_player_live_gameplay(&multiplayer).unwrap();
        assert_eq!(prepared.packets.len(), 9);
        assert_eq!(
            prepared.packets[2].packet.step.request_type,
            MessageType::RoomReadyReq
        );
        assert_eq!(
            prepared.packets[4].packet.step.request_type,
            MessageType::RoomStartReq
        );

        let ready = response(
            MessageType::RoomReadyRes,
            4,
            &RoomReadyRes {
                ok: true,
                room_id: "approved-room".into(),
                ready: true,
                error_code: String::new(),
            },
        );
        ensure_approved_room_packet(&ready, "approved-room").unwrap();
        let start = response(
            MessageType::RoomStartRes,
            5,
            &RoomStartRes {
                ok: true,
                room_id: "approved-room".into(),
                error_code: String::new(),
            },
        );
        ensure_approved_room_packet(&start, "approved-room").unwrap();
        let mismatched = response(
            MessageType::RoomReadyRes,
            4,
            &RoomReadyRes {
                ok: true,
                room_id: "other-room".into(),
                ready: true,
                error_code: String::new(),
            },
        );
        assert!(matches!(
            ensure_approved_room_packet(&mismatched, "approved-room"),
            Err(GameplayError::RoomMismatch)
        ));
    }

    #[test]
    fn ready_and_start_sequence_correlation_rejects_wrong_response_type_or_sequence() {
        let mut pool = AccountLeasePool::default();
        let mut session = active_session(&mut pool);
        let ready = session
            .begin_gameplay_request(
                &mut pool,
                MessageType::RoomReadyReq,
                MessageType::RoomReadyRes,
                &game_protocol::encode_body(&crate::pb::RoomReadyReq { ready: true }),
            )
            .unwrap();
        assert!(
            session
                .handle_packet(
                    &mut pool,
                    response(
                        MessageType::RoomStartRes,
                        ready.seq(),
                        &RoomStartRes {
                            ok: true,
                            room_id: "approved-room".into(),
                            error_code: String::new(),
                        },
                    ),
                )
                .is_err()
        );
        assert!(!session.lease_held());

        let mut session = active_session(&mut pool);
        let start = session
            .begin_gameplay_request(
                &mut pool,
                MessageType::RoomStartReq,
                MessageType::RoomStartRes,
                &game_protocol::encode_body(&crate::pb::RoomStartReq {}),
            )
            .unwrap();
        assert!(
            session
                .handle_packet(
                    &mut pool,
                    response(
                        MessageType::RoomStartRes,
                        start.seq().saturating_add(1),
                        &RoomStartRes {
                            ok: true,
                            room_id: "approved-room".into(),
                            error_code: String::new(),
                        },
                    ),
                )
                .is_err()
        );
        assert!(!session.lease_held());
    }

    #[test]
    fn two_player_terminal_cleanup_releases_both_fake_connections_and_leases() {
        let mut pool = AccountLeasePool::default();
        let fake = FakeKcpService::scripted_events([]);
        let leases = [
            pool.acquire("account-a", "player-a", 0, 1_000).unwrap(),
            pool.acquire("account-b", "player-b", 0, 1_000).unwrap(),
        ];
        let mut sessions = [
            VirtualPlayerSession::new(leases[0].clone(), 1_024, runner().reconnect_policy).unwrap(),
            VirtualPlayerSession::new(leases[1].clone(), 1_024, runner().reconnect_policy).unwrap(),
        ];
        for session in &mut sessions {
            session.mark_logged_in(&mut pool).unwrap();
            session.mark_character_selected(&mut pool).unwrap();
            session.mark_ticket_issued(&mut pool).unwrap();
            let auth = session
                .connect_and_begin_auth(&mut pool, fake.connect(), "private-ticket")
                .unwrap();
            session
                .handle_packet(
                    &mut pool,
                    response(
                        MessageType::AuthRes,
                        auth.seq(),
                        &crate::pb::AuthRes {
                            ok: true,
                            player_id: "fake".into(),
                            error_code: String::new(),
                            server_protocol_version: 1,
                            minimum_client_protocol_version: 1,
                            upgrade_message: String::new(),
                            upgrade_url: String::new(),
                        },
                    ),
                )
                .unwrap();
            session.activate(&mut pool).unwrap();
        }
        assert_eq!(fake.active_connections(), 2);
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut first = LiveKcpTransport::new(deadline, 1_024).unwrap();
        let mut second = LiveKcpTransport::new(deadline, 1_024).unwrap();
        close_two_player_sessions(&mut sessions, &mut [&mut first, &mut second], &mut pool);
        assert_eq!(fake.active_connections(), 0);
        assert!(pool.acquire("account-a", "replacement-a", 1, 1_000).is_ok());
        assert!(pool.acquire("account-b", "replacement-b", 1, 1_000).is_ok());
    }

    #[test]
    fn guarded_two_player_default_match_completes_in_the_global_plan_order() {
        let mut pool = AccountLeasePool::default();
        let leases = [
            pool.acquire("account-a", "player-a", 0, 1_000).unwrap(),
            pool.acquire("account-b", "player-b", 0, 1_000).unwrap(),
        ];
        // Per-player KCP sequences are independent: auth=1, heartbeat=2,
        // then the globally ordered gameplay requests use 3 onward.
        let mut first = ScriptedTwoPlayerTransport::scripted([
            Ok(authenticated_response(1)),
            Ok(heartbeat_response(2)),
            Ok(room_join_response(3, true)),
            Ok(room_ready_response(4, true)),
            Ok(room_start_response(5, true)),
            Ok(input_response(6, true)),
            Ok(leave_response(7)),
        ]);
        let mut second = ScriptedTwoPlayerTransport::scripted([
            Ok(authenticated_response(1)),
            Ok(heartbeat_response(2)),
            Ok(room_join_response(3, true)),
            Ok(room_ready_response(4, true)),
            Ok(input_response(5, true)),
            Ok(leave_response(6)),
        ]);
        let result = runner()
            .run_guarded_two_player_default_match(
                gate(),
                [&mut first, &mut second],
                &mut pool,
                leases,
                ["private-ticket-a", "private-ticket-b"],
                &two_player_gameplay(),
            )
            .unwrap();
        assert!(result.players.iter().all(|player| {
            player.terminal_state == VirtualPlayerSessionState::Closed
                && player.connection_released
                && player.lease_released
        }));
        assert_eq!(
            first
                .sent
                .iter()
                .map(|(message, _)| *message)
                .collect::<Vec<_>>(),
            vec![
                MessageType::AuthReq,
                MessageType::PingReq,
                MessageType::RoomJoinReq,
                MessageType::RoomReadyReq,
                MessageType::RoomStartReq,
                MessageType::PlayerInputReq,
                MessageType::RoomLeaveReq,
            ]
        );
        assert_eq!(
            second
                .sent
                .iter()
                .map(|(message, _)| *message)
                .collect::<Vec<_>>(),
            vec![
                MessageType::AuthReq,
                MessageType::PingReq,
                MessageType::RoomJoinReq,
                MessageType::RoomReadyReq,
                MessageType::PlayerInputReq,
                MessageType::RoomLeaveReq,
            ]
        );
        assert_eq!(first.active_connections(), 0);
        assert_eq!(second.active_connections(), 0);
    }

    #[test]
    fn guarded_two_player_ready_start_rejection_and_timeout_release_both_players() {
        for (ready_ok, start_response) in [
            (false, Some(room_start_response(5, true))),
            (true, Some(room_start_response(5, false))),
            (true, None),
        ] {
            let mut pool = AccountLeasePool::default();
            let leases = [
                pool.acquire("account-a", "player-a", 0, 1_000).unwrap(),
                pool.acquire("account-b", "player-b", 0, 1_000).unwrap(),
            ];
            let mut first_responses = vec![
                Ok(authenticated_response(1)),
                Ok(heartbeat_response(2)),
                Ok(room_join_response(3, true)),
                Ok(room_ready_response(4, ready_ok)),
            ];
            if let Some(response) = start_response {
                first_responses.push(Ok(response));
                first_responses.push(Err(GameLiveError::Transport("scripted start timeout")));
            }
            let mut first = ScriptedTwoPlayerTransport::scripted(first_responses);
            let mut second = ScriptedTwoPlayerTransport::scripted([
                Ok(authenticated_response(1)),
                Ok(heartbeat_response(2)),
                Ok(room_join_response(3, true)),
                Ok(room_ready_response(4, true)),
            ]);
            assert!(
                runner()
                    .run_guarded_two_player_default_match(
                        gate(),
                        [&mut first, &mut second],
                        &mut pool,
                        leases,
                        ["private-ticket-a", "private-ticket-b"],
                        &two_player_gameplay(),
                    )
                    .is_err()
            );
            assert_eq!(first.active_connections(), 0);
            assert_eq!(second.active_connections(), 0);
            assert!(pool.acquire("account-a", "replacement-a", 1, 1_000).is_ok());
            assert!(pool.acquire("account-b", "replacement-b", 1, 1_000).is_ok());
        }
    }

    #[test]
    fn guarded_two_player_input_rejection_releases_both_players_before_progress() {
        let mut pool = AccountLeasePool::default();
        let leases = [
            pool.acquire("account-a", "player-a", 0, 1_000).unwrap(),
            pool.acquire("account-b", "player-b", 0, 1_000).unwrap(),
        ];
        let mut first = ScriptedTwoPlayerTransport::scripted([
            Ok(authenticated_response(1)),
            Ok(heartbeat_response(2)),
            Ok(room_join_response(3, true)),
            Ok(room_ready_response(4, true)),
            Ok(room_start_response(5, true)),
            Ok(input_response_with_error(6, false, "INPUT_TIMESTAMP_SKEW")),
        ]);
        let mut second = ScriptedTwoPlayerTransport::scripted([
            Ok(authenticated_response(1)),
            Ok(heartbeat_response(2)),
            Ok(room_join_response(3, true)),
            Ok(room_ready_response(4, true)),
        ]);
        let error = runner()
            .run_guarded_two_player_default_match(
                gate(),
                [&mut first, &mut second],
                &mut pool,
                leases,
                ["private-ticket-a", "private-ticket-b"],
                &two_player_gameplay(),
            )
            .unwrap_err();
        assert_eq!(
            error.reportable_failure_category(),
            Some("gameplay_input_timestamp_skew")
        );
        assert_eq!(first.active_connections(), 0);
        assert_eq!(second.active_connections(), 0);
        assert!(pool.acquire("account-a", "replacement-a", 1, 1_000).is_ok());
        assert!(pool.acquire("account-b", "replacement-b", 1, 1_000).is_ok());
        assert!(
            !first
                .sent
                .iter()
                .any(|(message, _)| *message == MessageType::RoomLeaveReq)
        );
    }

    #[test]
    fn guarded_two_player_frame_expiry_is_reportable_and_stops_before_leave() {
        let mut pool = AccountLeasePool::default();
        let leases = [
            pool.acquire("account-a", "player-a", 0, 1_000).unwrap(),
            pool.acquire("account-b", "player-b", 0, 1_000).unwrap(),
        ];
        let mut first = ScriptedTwoPlayerTransport::scripted([
            Ok(authenticated_response(1)),
            Ok(heartbeat_response(2)),
            Ok(room_join_response(3, true)),
            Ok(room_ready_response(4, true)),
            Ok(room_start_response(5, true)),
            Ok(input_response_with_error(6, false, "INPUT_FRAME_EXPIRED")),
        ]);
        let mut second = ScriptedTwoPlayerTransport::scripted([
            Ok(authenticated_response(1)),
            Ok(heartbeat_response(2)),
            Ok(room_join_response(3, true)),
            Ok(room_ready_response(4, true)),
        ]);
        let error = runner()
            .run_guarded_two_player_default_match(
                gate(),
                [&mut first, &mut second],
                &mut pool,
                leases,
                ["private-ticket-a", "private-ticket-b"],
                &two_player_gameplay(),
            )
            .unwrap_err();

        assert_eq!(
            error.reportable_failure_category(),
            Some("gameplay_input_frame_expired")
        );
        assert!(
            !first
                .sent
                .iter()
                .any(|(message, _)| *message == MessageType::RoomLeaveReq)
        );
        assert_eq!(first.active_connections(), 0);
        assert_eq!(second.active_connections(), 0);
    }

    fn active_session(pool: &mut AccountLeasePool) -> VirtualPlayerSession {
        let account_lease = lease(pool);
        let mut session = VirtualPlayerSession::new(
            account_lease,
            1_024,
            ReconnectPolicy {
                max_attempts: 1,
                base_delay_ms: 1,
                max_delay_ms: 1,
                max_jitter_ms: 0,
            },
        )
        .unwrap();
        session.mark_logged_in(pool).unwrap();
        session.mark_character_selected(pool).unwrap();
        session.mark_ticket_issued(pool).unwrap();
        let connection = LiveKcpConnection;
        let auth = session
            .connect_and_begin_auth(pool, connection, "private-ticket")
            .unwrap();
        session
            .handle_packet(
                pool,
                response(
                    MessageType::AuthRes,
                    auth.seq(),
                    &crate::pb::AuthRes {
                        ok: true,
                        player_id: "fake-player".into(),
                        error_code: String::new(),
                        server_protocol_version: 1,
                        minimum_client_protocol_version: 1,
                        upgrade_message: String::new(),
                        upgrade_url: String::new(),
                    },
                ),
            )
            .unwrap();
        session.activate(pool).unwrap();
        session
    }

    #[tokio::test]
    async fn frame_bundle_timeout_releases_the_session_lease() {
        let mut pool = AccountLeasePool::default();
        let mut session = active_session(&mut pool);

        let mut transport = LiveKcpTransport::new(Instant::now(), 1_024).unwrap();
        let mut tracker = RoomFlowTracker::default();
        let error = receive_frame_bundle(
            &mut transport,
            &mut session,
            &mut pool,
            &mut tracker,
            1,
            &mut |_| Ok(()),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("deadline"));
        assert!(!session.connection_attached());
        assert!(!session.lease_held());
        assert!(pool.acquire("account-a", "replacement", 1, 1_000).is_ok());
    }

    #[tokio::test]
    async fn frame_bundle_abort_releases_the_session_lease() {
        let mut pool = AccountLeasePool::default();
        let mut session = active_session(&mut pool);
        let mut transport =
            LiveKcpTransport::new(Instant::now() + Duration::from_secs(1), 1_024).unwrap();
        let mut tracker = RoomFlowTracker::default();
        let error = receive_frame_bundle(
            &mut transport,
            &mut session,
            &mut pool,
            &mut tracker,
            1,
            &mut |_| Err(GameLiveError::Transport("test checkpoint abort")),
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("test checkpoint abort"));
        assert!(!session.connection_attached());
        assert!(!session.lease_held());
        assert!(pool.acquire("account-a", "replacement", 1, 1_000).is_ok());
    }
}
