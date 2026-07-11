use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Message, WebSocketConfig};
use tokio_util::sync::CancellationToken;

use crate::command::{CommandControl, CommandHandler, CommandHandlerOutcome};
use crate::config::AgentConfig;
use crate::error::{AgentError, ErrorCode};
use crate::preflight::PreflightReport;
use crate::protocol::{
    JsonValue, ProtocolError, QUEUE_CAPACITY, SUBPROTOCOL, parse_canonical_frame, random_base64url,
    semantic_digest, sign_message, verify_message_signature,
};
use crate::schemas::{
    AgentHeartbeat, AgentHello, AgentRegister, CommandErrorMessage, CommandExecute,
    CommandRejection, CommandStarted, EffectiveLimits, ProtocolErrorMessage, ServerMessage,
    parse_server_message, validate_challenge_compatibility, validate_execute_business,
    validate_message_time,
};
use crate::state::{CachedResponse, ExecuteDecision, ReplayCache, RequestRegistry};

const INITIAL_BACKOFF_MS: u64 = 250;
const MAX_BACKOFF_MS: u64 = 30_000;
const DEFAULT_CACHE_CAPACITY: usize = 65_536;
const TASK_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

#[async_trait]
pub trait Sleeper: Send + Sync {
    async fn sleep(&self, duration: Duration);
}

#[derive(Debug, Default)]
pub struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

pub trait BackoffJitter: Send + Sync {
    fn apply(&self, base: Duration, attempt: u32) -> Duration;
}

#[derive(Debug, Default)]
pub struct RandomJitter;

impl BackoffJitter for RandomJitter {
    fn apply(&self, base: Duration, _attempt: u32) -> Duration {
        let percent = rand::random_range(80_u64..=120_u64);
        let milliseconds: u64 = base
            .as_millis()
            .saturating_mul(u128::from(percent))
            .saturating_div(100)
            .try_into()
            .unwrap_or(u64::MAX);
        Duration::from_millis(milliseconds.min(MAX_BACKOFF_MS))
    }
}

#[derive(Clone)]
pub struct RuntimeHooks {
    pub clock: Arc<dyn Clock>,
    pub sleeper: Arc<dyn Sleeper>,
    pub jitter: Arc<dyn BackoffJitter>,
}

impl Default for RuntimeHooks {
    fn default() -> Self {
        Self {
            clock: Arc::new(SystemClock),
            sleeper: Arc::new(TokioSleeper),
            jitter: Arc::new(RandomJitter),
        }
    }
}

pub struct ClientRuntime {
    handler: Arc<dyn CommandHandler>,
    hooks: RuntimeHooks,
    replay_cache: Arc<ReplayCache>,
    request_registry: Arc<RequestRegistry>,
}

impl ClientRuntime {
    pub fn new(handler: Arc<dyn CommandHandler>) -> Self {
        Self::with_options(
            handler,
            RuntimeHooks::default(),
            DEFAULT_CACHE_CAPACITY,
            DEFAULT_CACHE_CAPACITY,
        )
    }

    pub fn with_options(
        handler: Arc<dyn CommandHandler>,
        hooks: RuntimeHooks,
        replay_capacity: usize,
        request_capacity: usize,
    ) -> Self {
        Self {
            handler,
            hooks,
            replay_cache: Arc::new(ReplayCache::new(replay_capacity)),
            request_registry: Arc::new(RequestRegistry::new(request_capacity)),
        }
    }

    pub async fn run(
        &self,
        config: &AgentConfig,
        preflight: &PreflightReport,
        shutdown: CancellationToken,
    ) -> Result<(), AgentError> {
        let request = build_request(config)?;
        let mut attempt = 0_u32;
        while !shutdown.is_cancelled() {
            let outcome = self
                .connect_once(config, preflight, request.clone(), shutdown.child_token())
                .await;
            if shutdown.is_cancelled() {
                break;
            }
            match &outcome {
                Ok(summary) => tracing::warn!(
                    registered = summary.registered,
                    stable = summary.stable,
                    reason = summary.reason,
                    "myforge WebSocket disconnected"
                ),
                Err(error) => tracing::warn!(
                    error_code = error.code(),
                    "myforge WebSocket connection attempt failed"
                ),
            }
            if outcome.as_ref().is_ok_and(|summary| summary.stable) {
                attempt = 0;
            }
            let base = backoff_delay(attempt);
            let delay = self.hooks.jitter.apply(base, attempt);
            attempt = attempt.saturating_add(1);
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = self.hooks.sleeper.sleep(delay) => {}
            }
        }
        self.request_registry.cancel_all().await;
        Ok(())
    }

    async fn connect_once(
        &self,
        config: &AgentConfig,
        preflight: &PreflightReport,
        request: tokio_tungstenite::tungstenite::http::Request<()>,
        shutdown: CancellationToken,
    ) -> Result<DisconnectSummary, ConnectionFailure> {
        let (websocket, response) = tokio::select! {
            () = shutdown.cancelled() => {
                return Ok(DisconnectSummary {
                    registered: false,
                    stable: false,
                    reason: "shutdown",
                });
            }
            result = tokio_tungstenite::connect_async_with_config(
                request,
                Some(client_websocket_config(
                    config.limits().ws_max_message_bytes as usize,
                )),
                false,
            ) => {
                result.map_err(|_| ConnectionFailure::transport("WebSocket connection failed"))?
            }
        };
        let negotiated = response
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|value| value.to_str().ok());
        if negotiated != Some(SUBPROTOCOL) {
            return Err(ConnectionFailure::protocol(
                ProtocolError::new(
                    "MYFORGE_PROTOCOL_VERSION_UNSUPPORTED",
                    "required WebSocket subprotocol was not negotiated",
                )
                .unsafe_response(),
            ));
        }
        let connected_at = Instant::now();

        let connection_token = shutdown.child_token();
        let (sink, stream) = websocket.split();
        let (outbound_tx, outbound_rx) = mpsc::channel(QUEUE_CAPACITY);
        let (inbound_tx, mut inbound_rx) = mpsc::channel(QUEUE_CAPACITY);
        let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
        let max_frame_bytes = Arc::new(AtomicU64::new(config.limits().ws_max_message_bytes));
        let outbound = OutboundHandle {
            sender: outbound_tx,
            signing_key: Arc::new(config.keys().agent_signing_key().clone()),
            clock: self.hooks.clock.clone(),
            max_frame_bytes: max_frame_bytes.clone(),
            write_timeout: Duration::from_millis(config.ws_write_timeout_ms()),
        };
        let writer = tokio::spawn(writer_task(
            sink,
            outbound_rx,
            terminal_tx.clone(),
            connection_token.child_token(),
        ));
        let reader = tokio::spawn(reader_task(
            stream,
            inbound_tx,
            terminal_tx.clone(),
            connection_token.child_token(),
            config.limits().ws_max_message_bytes,
        ));

        let handshake_token = connection_token.child_token();
        let handshake_timer = spawn_handshake_timer(
            handshake_token.clone(),
            terminal_tx.clone(),
            config
                .limits()
                .auth_ttl_ms
                .saturating_add(config.limits().clock_skew_ms),
        );
        let mut state = ConnectionState::Connected;
        let mut heartbeat: Option<JoinHandle<()>> = None;
        let mut workers = JoinSet::new();
        let mut registered = false;

        let termination = loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => {
                    break TerminationAction::Shutdown;
                }
                Some(failure) = terminal_rx.recv() => {
                    break TerminationAction::from_terminal(failure);
                }
                Some(frame) = inbound_rx.recv() => {
                    let handled = tokio::select! {
                        biased;
                        () = shutdown.cancelled() => break TerminationAction::Shutdown,
                        Some(failure) = terminal_rx.recv() => {
                            break TerminationAction::from_terminal(failure);
                        }
                        handled = self.handle_inbound(
                            config,
                            preflight,
                            &mut state,
                            &outbound,
                            &max_frame_bytes,
                            &terminal_tx,
                            &mut workers,
                            frame,
                        ) => handled,
                    };
                    match handled {
                        Ok(HandleOutcome::Continue) => {}
                        Ok(HandleOutcome::Registered { effective, connection_id }) => {
                            registered = true;
                            handshake_token.cancel();
                            heartbeat = Some(spawn_heartbeat(
                                outbound.clone(),
                                self.request_registry.clone(),
                                self.hooks.clock.clone(),
                                terminal_tx.clone(),
                                connection_token.child_token(),
                                ConnectionIdentity::new(config, connection_id),
                                effective,
                            ));
                        }
                        Ok(HandleOutcome::PeerFatal) => {
                            break TerminationAction::PeerFatal;
                        }
                        Err(error) => {
                            break TerminationAction::Protocol(error);
                        }
                    }
                }
                Some(joined) = workers.join_next(), if !workers.is_empty() => {
                    if joined.is_err() {
                        break TerminationAction::WorkerFailure;
                    }
                }
                else => break TerminationAction::Transport("socket_closed"),
            }
        };

        handshake_token.cancel();
        cancel_connection_requests(
            &self.request_registry,
            &state,
            self.hooks.clock.now_ms(),
            config
                .limits()
                .max_command_timeout_ms
                .saturating_add(config.limits().command_ttl_ms),
        )
        .await;
        if let Some(task) = heartbeat.as_ref() {
            task.abort();
        }
        let write_budget = Duration::from_millis(config.ws_write_timeout_ms());
        let _ = tokio::time::timeout(write_budget, async {
            match &termination {
                TerminationAction::Shutdown => {
                    let _ = outbound.close(1001, "agent_shutdown").await;
                }
                TerminationAction::Protocol(error) => {
                    self.handle_protocol_failure(config, &state, &outbound, error)
                        .await;
                }
                TerminationAction::PeerFatal => {
                    let _ = outbound.close(1008, "peer_protocol_error").await;
                }
                TerminationAction::Transport(_) => {
                    let _ = outbound.close(1011, "websocket_transport_failure").await;
                }
                TerminationAction::WorkerFailure => {
                    let _ = outbound.close(1011, "command_worker_failed").await;
                }
            }
        })
        .await;
        connection_token.cancel();
        drain_workers(&mut workers).await;
        await_task(handshake_timer).await;
        if let Some(task) = heartbeat {
            await_task(task).await;
        }
        await_task(reader).await;
        await_task(writer).await;

        Ok(DisconnectSummary {
            registered,
            stable: registered
                && connected_at.elapsed()
                    >= Duration::from_millis(
                        config.limits().heartbeat_interval_ms.saturating_mul(2),
                    ),
            reason: termination.reason(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_inbound(
        &self,
        config: &AgentConfig,
        preflight: &PreflightReport,
        state: &mut ConnectionState,
        outbound: &OutboundHandle,
        max_frame_bytes: &Arc<AtomicU64>,
        terminal_tx: &mpsc::UnboundedSender<TerminalFailure>,
        workers: &mut JoinSet<()>,
        frame: InboundFrame,
    ) -> Result<HandleOutcome, ProtocolError> {
        match frame {
            InboundFrame::Ping(payload) => {
                outbound.send_control(Message::Pong(payload.into())).await?;
                Ok(HandleOutcome::Continue)
            }
            InboundFrame::Pong => Ok(HandleOutcome::Continue),
            InboundFrame::Close => Err(ProtocolError::new(
                "MYFORGE_AGENT_DISCONNECTED",
                "server closed the WebSocket connection",
            )
            .unsafe_response()),
            InboundFrame::Text(frame) => {
                let accepted_limit = state
                    .effective()
                    .map_or(config.limits().ws_max_message_bytes, |limits| {
                        limits.ws_max_message_bytes
                    });
                if frame.len() as u64 > accepted_limit {
                    let code = if state.effective().is_some() {
                        "MYFORGE_LIMIT_MISMATCH"
                    } else {
                        "MYFORGE_OUTPUT_TOO_LARGE"
                    };
                    return Err(
                        ProtocolError::new(code, "message exceeds the accepted limit")
                            .unsafe_response(),
                    );
                }
                let value = parse_canonical_frame(&frame, accepted_limit as usize)?;
                verify_message_signature(&value, config.keys().server_verifying_key())?;
                let message = parse_server_message(&value)?;
                if let ServerMessage::Challenge(challenge) = &message {
                    state.use_verified_challenge(
                        challenge.challenge_id.clone(),
                        config
                            .limits()
                            .auth_ttl_ms
                            .min(challenge.limits.auth_ttl_ms),
                    );
                }
                self.validate_server_message(config, state, &message)?;
                self.record_replay(config, &message)?;
                self.validate_state(state, &message)?;

                match message {
                    ServerMessage::Challenge(challenge) => {
                        let effective =
                            validate_challenge_compatibility(&challenge, config.limits())?;
                        *state = ConnectionState::Challenged {
                            connection_id: challenge.challenge_id.clone(),
                            effective,
                        };
                        let hello_timestamp = self.hooks.clock.now_ms();
                        outbound
                            .send_signed(
                                &AgentHello {
                                    protocol_version: 1,
                                    message_type: "agent.hello",
                                    challenge_id: &challenge.challenge_id,
                                    challenge: &challenge.challenge,
                                    agent_id: config.agent_id(),
                                    project_id: config.project_id(),
                                    timestamp_ms: hello_timestamp,
                                    expires_at_ms: hello_timestamp
                                        .saturating_add(effective.auth_ttl_ms),
                                    nonce: random_base64url::<16>(),
                                },
                                hello_timestamp.saturating_add(effective.auth_ttl_ms),
                            )
                            .await?;
                        *state = ConnectionState::Authenticated {
                            connection_id: challenge.challenge_id.clone(),
                            effective,
                        };
                        let register_timestamp = self.hooks.clock.now_ms();
                        outbound
                            .send_signed(
                                &AgentRegister {
                                    protocol_version: 1,
                                    message_type: "agent.register",
                                    connection_id: &challenge.challenge_id,
                                    agent_id: config.agent_id(),
                                    project_id: config.project_id(),
                                    hostname: preflight.hostname(),
                                    platform: preflight.platform(),
                                    agent_version: preflight.agent_version(),
                                    forge_root_summary: preflight.forge_root_summary(),
                                    capabilities: preflight.capabilities(),
                                    limits: config.limits(),
                                    timestamp_ms: register_timestamp,
                                    expires_at_ms: register_timestamp
                                        .saturating_add(effective.auth_ttl_ms),
                                    nonce: random_base64url::<16>(),
                                },
                                register_timestamp.saturating_add(effective.auth_ttl_ms),
                            )
                            .await?;
                        max_frame_bytes.store(effective.ws_max_message_bytes, Ordering::Release);
                        *state = ConnectionState::Registered {
                            connection_id: challenge.challenge_id.clone(),
                            effective,
                        };
                        Ok(HandleOutcome::Registered {
                            effective,
                            connection_id: challenge.challenge_id,
                        })
                    }
                    ServerMessage::Execute(command) => {
                        let effective = state.effective().ok_or_else(|| {
                            ProtocolError::new(
                                "MYFORGE_PROTOCOL_STATE_INVALID",
                                "command.execute requires a registered connection",
                            )
                        })?;
                        self.handle_execute(
                            config,
                            state.connection_id().unwrap_or_default(),
                            effective,
                            value,
                            command,
                            outbound,
                            terminal_tx,
                            workers,
                        )
                        .await?;
                        Ok(HandleOutcome::Continue)
                    }
                    ServerMessage::Cancel(cancel) => {
                        let effective = state.effective().ok_or_else(|| {
                            ProtocolError::new(
                                "MYFORGE_PROTOCOL_STATE_INVALID",
                                "command.cancel requires a registered connection",
                            )
                        })?;
                        if cancel.cancel_deadline_at_ms - cancel.cancel_requested_at_ms
                            != effective.cancel_timeout_ms
                        {
                            return Err(ProtocolError::new(
                                "MYFORGE_LIMIT_MISMATCH",
                                "cancel deadline does not match the connection",
                            )
                            .with_request_id(Some(cancel.request_id)));
                        }
                        if self.hooks.clock.now_ms()
                            > cancel
                                .cancel_deadline_at_ms
                                .saturating_add(config.limits().clock_skew_ms)
                        {
                            return Err(ProtocolError::new(
                                "MYFORGE_MESSAGE_EXPIRED",
                                "cancel deadline has passed",
                            )
                            .with_request_id(Some(cancel.request_id)));
                        }
                        self.request_registry
                            .cancel(&cancel.request_id, cancel.cancel_deadline_at_ms)
                            .await?;
                        Ok(HandleOutcome::Continue)
                    }
                    ServerMessage::ProtocolError(_) => Ok(HandleOutcome::PeerFatal),
                }
            }
        }
    }

    fn validate_server_message(
        &self,
        config: &AgentConfig,
        state: &ConnectionState,
        message: &ServerMessage,
    ) -> Result<(), ProtocolError> {
        let request_id = message.request_id().map(ToOwned::to_owned);
        let identity = match message {
            ServerMessage::Challenge(message) => (&message.agent_id, &message.project_id),
            ServerMessage::Execute(message) => (&message.agent_id, &message.project_id),
            ServerMessage::Cancel(message) => (&message.agent_id, &message.project_id),
            ServerMessage::ProtocolError(message) => (&message.agent_id, &message.project_id),
        };
        if identity.0 != config.agent_id() || identity.1 != config.project_id() {
            return Err(ProtocolError::new(
                "MYFORGE_IDENTITY_MISMATCH",
                "message identity does not match this agent",
            )
            .with_request_id(request_id));
        }

        let now = self.hooks.clock.now_ms();
        let (ttl_ms, exact_lifetime_ms) = match message {
            ServerMessage::Challenge(challenge) => (
                config.limits().auth_ttl_ms,
                Some(challenge.limits.auth_ttl_ms),
            ),
            ServerMessage::Execute(_) => (
                config.limits().command_ttl_ms,
                state.effective().map(|limits| limits.command_ttl_ms),
            ),
            ServerMessage::Cancel(_) => (config.limits().command_ttl_ms, None),
            ServerMessage::ProtocolError(_) => (config.limits().auth_ttl_ms, None),
        };
        validate_message_time(
            message.timestamp_ms(),
            message.expires_at_ms(),
            now,
            config.limits().clock_skew_ms,
            ttl_ms,
            exact_lifetime_ms,
        )
        .map_err(|error| error.with_request_id(request_id.clone()))?;

        match message {
            ServerMessage::Challenge(_)
                if !matches!(
                    state,
                    ConnectionState::Connected | ConnectionState::VerifiedChallenge { .. }
                ) =>
            {
                return Err(ProtocolError::new(
                    "MYFORGE_PROTOCOL_STATE_INVALID",
                    "server.challenge is not valid in the current state",
                ));
            }
            ServerMessage::Challenge(_) => {}
            _ if message.connection_id() != state.connection_id() => {
                return Err(ProtocolError::new(
                    "MYFORGE_IDENTITY_MISMATCH",
                    "connectionId does not match this socket",
                )
                .with_request_id(request_id));
            }
            _ => {}
        }
        Ok(())
    }

    fn validate_state(
        &self,
        state: &ConnectionState,
        message: &ServerMessage,
    ) -> Result<(), ProtocolError> {
        let valid = matches!(
            (state, message),
            (ConnectionState::Connected, ServerMessage::Challenge(_))
                | (
                    ConnectionState::VerifiedChallenge { .. },
                    ServerMessage::Challenge(_)
                )
                | (
                    ConnectionState::Registered { .. },
                    ServerMessage::Execute(_)
                )
                | (ConnectionState::Registered { .. }, ServerMessage::Cancel(_))
                | (_, ServerMessage::ProtocolError(_))
        );
        if valid {
            Ok(())
        } else {
            Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "message is not valid in the current connection state",
            )
            .with_request_id(message.request_id().map(ToOwned::to_owned)))
        }
    }

    fn record_replay(
        &self,
        config: &AgentConfig,
        message: &ServerMessage,
    ) -> Result<(), ProtocolError> {
        let connection_id = message.connection_id().unwrap_or("untrusted");
        let key = format!(
            "{}\0{}\0{}",
            connection_id,
            config.keys().server_public_key_fingerprint(),
            message.nonce()
        );
        self.replay_cache.check_and_insert(
            key,
            message
                .expires_at_ms()
                .saturating_add(config.limits().clock_skew_ms),
            self.hooks.clock.now_ms(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_execute(
        &self,
        config: &AgentConfig,
        connection_id: &str,
        effective: EffectiveLimits,
        wire_value: JsonValue,
        command: CommandExecute,
        outbound: &OutboundHandle,
        terminal_tx: &mpsc::UnboundedSender<TerminalFailure>,
        workers: &mut JoinSet<()>,
    ) -> Result<(), ProtocolError> {
        if command.timeout_ms != effective.command_timeout_ms
            || command.max_output_bytes != effective.max_output_bytes
        {
            return Err(ProtocolError::new(
                "MYFORGE_LIMIT_MISMATCH",
                "command limits do not match the connection",
            )
            .with_request_id(Some(command.request_id)));
        }
        let digest = semantic_digest(&wire_value)?;
        let decision = self
            .request_registry
            .begin(
                connection_id,
                &command.request_id,
                &digest,
                self.hooks.clock.now_ms(),
            )
            .await?;
        match decision {
            ExecuteDecision::DuplicateActive { started_at_ms, .. } => {
                if let Some(started_at_ms) = started_at_ms {
                    self.send_started(
                        config,
                        connection_id,
                        effective,
                        &command.request_id,
                        started_at_ms,
                        outbound,
                    )
                    .await?;
                }
                Ok(())
            }
            ExecuteDecision::DuplicateCompleted { response } => match response {
                CachedResponse::CommandError(rejection) => {
                    self.send_command_error(
                        config,
                        connection_id,
                        effective,
                        &command.request_id,
                        &rejection,
                        outbound,
                    )
                    .await
                }
                CachedResponse::NoReplay => Err(ProtocolError::new(
                    "MYFORGE_DUPLICATE_REQUEST_CONFLICT",
                    "request belongs to a closed connection",
                )
                .with_request_id(Some(command.request_id))),
            },
            ExecuteDecision::New { cancellation } => {
                if let Err(rejection) = validate_execute_business(&command, effective) {
                    if rejection.protocol_fatal {
                        return Err(ProtocolError::new(
                            rejection.error_code,
                            rejection.error_message,
                        )
                        .with_request_id(Some(command.request_id)));
                    }
                    self.send_command_error(
                        config,
                        connection_id,
                        effective,
                        &command.request_id,
                        &rejection,
                        outbound,
                    )
                    .await?;
                    self.request_registry
                        .complete_error(
                            &command.request_id,
                            rejection,
                            self.hooks.clock.now_ms(),
                            effective
                                .command_timeout_ms
                                .saturating_add(effective.command_ttl_ms),
                        )
                        .await?;
                    return Ok(());
                }

                let handler = self.handler.clone();
                let registry = self.request_registry.clone();
                let outbound = outbound.clone();
                let clock = self.hooks.clock.clone();
                let identity = ConnectionIdentity::new(config, connection_id.to_string());
                let request_id = command.request_id.clone();
                let terminal_tx = terminal_tx.clone();
                workers.spawn(async move {
                    let received_at_ms = clock.now_ms();
                    let outcome = handler
                        .execute(
                            command,
                            CommandControl::new(cancellation.clone(), received_at_ms),
                        )
                        .await;
                    match outcome {
                        CommandHandlerOutcome::PreStartError(rejection)
                            if !cancellation.is_cancelled() =>
                        {
                            let send = send_command_error_owned(
                                &identity,
                                effective,
                                &request_id,
                                &rejection,
                                &outbound,
                                clock.as_ref(),
                            )
                            .await;
                            if let Err(error) = send {
                                let _ = terminal_tx.send(TerminalFailure::protocol(error));
                                return;
                            }
                            if let Err(error) = registry
                                .complete_error(
                                    &request_id,
                                    rejection,
                                    clock.now_ms(),
                                    effective
                                        .command_timeout_ms
                                        .saturating_add(effective.command_ttl_ms),
                                )
                                .await
                            {
                                let _ = terminal_tx.send(TerminalFailure::protocol(error));
                            }
                        }
                        CommandHandlerOutcome::PreStartError(_)
                        | CommandHandlerOutcome::CancelledBeforeStart => {
                            // Stage 7 owns construction of the signed cancelled result. Keeping the
                            // request active prevents a false command.error after cancellation.
                        }
                    }
                });
                Ok(())
            }
        }
    }

    async fn send_started(
        &self,
        config: &AgentConfig,
        connection_id: &str,
        effective: EffectiveLimits,
        request_id: &str,
        started_at_ms: u64,
        outbound: &OutboundHandle,
    ) -> Result<(), ProtocolError> {
        let timestamp_ms = self.hooks.clock.now_ms();
        outbound
            .send_signed(
                &CommandStarted {
                    protocol_version: 1,
                    message_type: "command.started",
                    connection_id,
                    request_id,
                    agent_id: config.agent_id(),
                    project_id: config.project_id(),
                    execution_mode: if config.dry_run() {
                        "dry_run"
                    } else {
                        "codex_exec"
                    },
                    started_at_ms,
                    timestamp_ms,
                    expires_at_ms: timestamp_ms.saturating_add(effective.auth_ttl_ms),
                    nonce: random_base64url::<16>(),
                },
                timestamp_ms.saturating_add(effective.auth_ttl_ms),
            )
            .await
    }

    async fn send_command_error(
        &self,
        config: &AgentConfig,
        connection_id: &str,
        effective: EffectiveLimits,
        request_id: &str,
        rejection: &CommandRejection,
        outbound: &OutboundHandle,
    ) -> Result<(), ProtocolError> {
        send_command_error_owned(
            &ConnectionIdentity::new(config, connection_id.to_string()),
            effective,
            request_id,
            rejection,
            outbound,
            self.hooks.clock.as_ref(),
        )
        .await
    }

    async fn handle_protocol_failure(
        &self,
        config: &AgentConfig,
        state: &ConnectionState,
        outbound: &OutboundHandle,
        error: &ProtocolError,
    ) {
        tracing::warn!(
            error_code = error.code(),
            "myforge protocol message rejected"
        );
        if error.safe_to_respond() {
            let timestamp_ms = self.hooks.clock.now_ms();
            let ttl_ms = state
                .protocol_error_auth_ttl_ms()
                .unwrap_or(config.limits().auth_ttl_ms);
            let message = ProtocolErrorMessage {
                protocol_version: 1,
                message_type: "protocol.error",
                connection_id: state.connection_id(),
                agent_id: config.agent_id(),
                project_id: config.project_id(),
                request_id: error.request_id(),
                error_code: error.code(),
                error_message: safe_protocol_error_message(error.code()),
                fatal: true,
                timestamp_ms,
                expires_at_ms: timestamp_ms.saturating_add(ttl_ms),
                nonce: random_base64url::<16>(),
            };
            let _ = outbound
                .send_signed(&message, timestamp_ms.saturating_add(ttl_ms))
                .await;
        }
        let _ = outbound.close(1008, "policy_violation").await;
    }
}

async fn send_command_error_owned(
    identity: &ConnectionIdentity,
    effective: EffectiveLimits,
    request_id: &str,
    rejection: &CommandRejection,
    outbound: &OutboundHandle,
    clock: &dyn Clock,
) -> Result<(), ProtocolError> {
    rejection.validate()?;
    let timestamp_ms = clock.now_ms();
    outbound
        .send_signed(
            &CommandErrorMessage {
                protocol_version: 1,
                message_type: "command.error",
                connection_id: &identity.connection_id,
                request_id,
                agent_id: &identity.agent_id,
                project_id: &identity.project_id,
                error_code: rejection.error_code,
                error_message: rejection.error_message,
                retryable: rejection.retryable,
                timestamp_ms,
                expires_at_ms: timestamp_ms.saturating_add(effective.auth_ttl_ms),
                nonce: random_base64url::<16>(),
            },
            timestamp_ms.saturating_add(effective.auth_ttl_ms),
        )
        .await
}

fn build_request(
    config: &AgentConfig,
) -> Result<tokio_tungstenite::tungstenite::http::Request<()>, AgentError> {
    let mut url = config.admin_api_ws_url().clone();
    url.query_pairs_mut()
        .append_pair("agentId", config.agent_id())
        .append_pair("projectId", config.project_id());
    let mut request = url.as_str().into_client_request().map_err(|_| {
        AgentError::new(
            ErrorCode::ConfigInvalid,
            "configured WebSocket endpoint cannot form a request",
        )
    })?;
    request.headers_mut().insert(
        "sec-websocket-protocol",
        HeaderValue::from_static(SUBPROTOCOL),
    );
    Ok(request)
}

fn backoff_delay(attempt: u32) -> Duration {
    let multiplier = 1_u64 << attempt.min(7);
    Duration::from_millis(
        INITIAL_BACKOFF_MS
            .saturating_mul(multiplier)
            .min(MAX_BACKOFF_MS),
    )
}

fn client_websocket_config(max_bytes: usize) -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(max_bytes))
        .max_frame_size(Some(max_bytes))
}

#[derive(Clone, Debug)]
struct ConnectionIdentity {
    connection_id: String,
    agent_id: String,
    project_id: String,
}

impl ConnectionIdentity {
    fn new(config: &AgentConfig, connection_id: String) -> Self {
        Self {
            connection_id,
            agent_id: config.agent_id().to_string(),
            project_id: config.project_id().to_string(),
        }
    }
}

#[derive(Debug)]
enum ConnectionState {
    Connected,
    VerifiedChallenge {
        connection_id: String,
        error_auth_ttl_ms: u64,
    },
    Challenged {
        connection_id: String,
        effective: EffectiveLimits,
    },
    Authenticated {
        connection_id: String,
        effective: EffectiveLimits,
    },
    Registered {
        connection_id: String,
        effective: EffectiveLimits,
    },
}

impl ConnectionState {
    fn connection_id(&self) -> Option<&str> {
        match self {
            Self::Connected => None,
            Self::VerifiedChallenge { connection_id, .. }
            | Self::Challenged { connection_id, .. }
            | Self::Authenticated { connection_id, .. }
            | Self::Registered { connection_id, .. } => Some(connection_id),
        }
    }

    const fn effective(&self) -> Option<EffectiveLimits> {
        match self {
            Self::Connected | Self::VerifiedChallenge { .. } => None,
            Self::Challenged { effective, .. }
            | Self::Authenticated { effective, .. }
            | Self::Registered { effective, .. } => Some(*effective),
        }
    }

    fn use_verified_challenge(&mut self, connection_id: String, error_auth_ttl_ms: u64) {
        if matches!(self, Self::Connected) {
            *self = Self::VerifiedChallenge {
                connection_id,
                error_auth_ttl_ms,
            };
        }
    }

    const fn protocol_error_auth_ttl_ms(&self) -> Option<u64> {
        match self {
            Self::Connected => None,
            Self::VerifiedChallenge {
                error_auth_ttl_ms, ..
            } => Some(*error_auth_ttl_ms),
            Self::Challenged { effective, .. }
            | Self::Authenticated { effective, .. }
            | Self::Registered { effective, .. } => Some(effective.auth_ttl_ms),
        }
    }
}

enum HandleOutcome {
    Continue,
    Registered {
        effective: EffectiveLimits,
        connection_id: String,
    },
    PeerFatal,
}

enum TerminationAction {
    Shutdown,
    Protocol(ProtocolError),
    PeerFatal,
    Transport(&'static str),
    WorkerFailure,
}

impl TerminationAction {
    fn from_terminal(failure: TerminalFailure) -> Self {
        failure
            .protocol
            .map_or(Self::Transport(failure.reason), Self::Protocol)
    }

    const fn reason(&self) -> &'static str {
        match self {
            Self::Shutdown => "shutdown",
            Self::Protocol(_) => "protocol_error",
            Self::PeerFatal => "peer_protocol_error",
            Self::Transport(reason) => reason,
            Self::WorkerFailure => "command_worker_failed",
        }
    }
}

async fn cancel_connection_requests(
    registry: &RequestRegistry,
    state: &ConnectionState,
    now_ms: u64,
    default_retention_ms: u64,
) {
    let Some(connection_id) = state.connection_id() else {
        return;
    };
    let retention_ms = state.effective().map_or(default_retention_ms, |limits| {
        limits
            .command_timeout_ms
            .saturating_add(limits.command_ttl_ms)
    });
    registry
        .disconnect_connection(connection_id, now_ms, retention_ms)
        .await;
}

struct DisconnectSummary {
    registered: bool,
    stable: bool,
    reason: &'static str,
}

#[derive(Debug)]
struct ConnectionFailure {
    code: &'static str,
}

impl ConnectionFailure {
    fn transport(_reason: &'static str) -> Self {
        Self {
            code: "MYFORGE_AGENT_DISCONNECTED",
        }
    }

    fn protocol(error: ProtocolError) -> Self {
        Self { code: error.code() }
    }

    const fn code(&self) -> &'static str {
        self.code
    }
}

#[derive(Debug)]
struct TerminalFailure {
    reason: &'static str,
    protocol: Option<ProtocolError>,
}

impl TerminalFailure {
    fn transport(reason: &'static str) -> Self {
        Self {
            reason,
            protocol: None,
        }
    }

    fn protocol(error: ProtocolError) -> Self {
        Self {
            reason: "protocol_error",
            protocol: Some(error),
        }
    }
}

enum InboundFrame {
    Text(Vec<u8>),
    Ping(Vec<u8>),
    Pong,
    Close,
}

#[derive(Clone, Debug)]
struct TransportError;

enum OutboundPayload {
    Message(Message),
    Close(u16, &'static str),
}

struct OutboundItem {
    payload: OutboundPayload,
    deadline: tokio::time::Instant,
    completion: oneshot::Sender<Result<(), TransportError>>,
}

#[derive(Clone)]
struct OutboundHandle {
    sender: mpsc::Sender<OutboundItem>,
    signing_key: Arc<ed25519_dalek::SigningKey>,
    clock: Arc<dyn Clock>,
    max_frame_bytes: Arc<AtomicU64>,
    write_timeout: Duration,
}

impl OutboundHandle {
    async fn send_signed(
        &self,
        message: &impl serde::Serialize,
        expires_at_ms: u64,
    ) -> Result<(), ProtocolError> {
        let frame = sign_message(message, &self.signing_key)?;
        if frame.len() as u64 > self.max_frame_bytes.load(Ordering::Acquire) {
            return Err(ProtocolError::new(
                "MYFORGE_OUTPUT_TOO_LARGE",
                "outbound frame exceeds the negotiated limit",
            ));
        }
        self.send_payload(
            OutboundPayload::Message(Message::Text(frame.into())),
            Some(expires_at_ms),
        )
        .await
    }

    async fn send_control(&self, message: Message) -> Result<(), ProtocolError> {
        self.send_payload(OutboundPayload::Message(message), None)
            .await
    }

    async fn close(&self, code: u16, reason: &'static str) -> Result<(), ProtocolError> {
        self.send_payload(OutboundPayload::Close(code, reason), None)
            .await
    }

    async fn send_payload(
        &self,
        payload: OutboundPayload,
        expires_at_ms: Option<u64>,
    ) -> Result<(), ProtocolError> {
        let now_ms = self.clock.now_ms();
        let expiry_budget = expires_at_ms
            .map(|expiry| Duration::from_millis(expiry.saturating_sub(now_ms)))
            .unwrap_or(self.write_timeout);
        let timeout = self.write_timeout.min(expiry_budget);
        if timeout.is_zero() {
            return Err(ProtocolError::new(
                "MYFORGE_MESSAGE_EXPIRED",
                "outbound message expired before send",
            ));
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let (completion_tx, completion_rx) = oneshot::channel();
        let item = OutboundItem {
            payload,
            deadline,
            completion: completion_tx,
        };
        tokio::time::timeout_at(deadline, self.sender.send(item))
            .await
            .map_err(|_| writer_error())?
            .map_err(|_| writer_error())?;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(writer_error());
        }
        tokio::time::timeout(remaining, completion_rx)
            .await
            .map_err(|_| writer_error())?
            .map_err(|_| writer_error())?
            .map_err(|_| writer_error())
    }
}

fn writer_error() -> ProtocolError {
    ProtocolError::new(
        "MYFORGE_AGENT_DISCONNECTED",
        "WebSocket writer is unavailable",
    )
    .unsafe_response()
}

async fn writer_task<S>(
    mut sink: S,
    mut receiver: mpsc::Receiver<OutboundItem>,
    terminal: mpsc::UnboundedSender<TerminalFailure>,
    shutdown: CancellationToken,
) where
    S: futures_util::Sink<Message> + Unpin,
{
    loop {
        let item = tokio::select! {
            () = shutdown.cancelled() => break,
            item = receiver.recv() => match item {
                Some(item) => item,
                None => break,
            }
        };
        let OutboundItem {
            payload,
            deadline,
            completion,
        } = item;
        if tokio::time::Instant::now() >= deadline {
            let _ = completion.send(Err(TransportError));
            let _ = terminal.send(TerminalFailure::transport("writer_deadline"));
            break;
        }
        let close = matches!(payload, OutboundPayload::Close(_, _));
        let send = async {
            match payload {
                OutboundPayload::Message(message) => sink.send(message).await,
                OutboundPayload::Close(code, reason) => {
                    sink.send(Message::Close(Some(CloseFrame {
                        code: code.into(),
                        reason: reason.into(),
                    })))
                    .await
                }
            }
        };
        let result = tokio::select! {
            () = shutdown.cancelled() => {
                let _ = completion.send(Err(TransportError));
                break;
            }
            result = tokio::time::timeout_at(deadline, send) => result,
        };
        match result {
            Ok(Ok(())) => {
                let _ = completion.send(Ok(()));
                if close {
                    break;
                }
            }
            Ok(Err(_)) => {
                let _ = completion.send(Err(TransportError));
                let _ = terminal.send(TerminalFailure::transport("writer_failure"));
                break;
            }
            Err(_) => {
                let _ = completion.send(Err(TransportError));
                let _ = terminal.send(TerminalFailure::transport("writer_deadline"));
                break;
            }
        }
    }
    while let Ok(item) = receiver.try_recv() {
        let _ = item.completion.send(Err(TransportError));
    }
}

async fn reader_task<S, E>(
    mut stream: S,
    sender: mpsc::Sender<InboundFrame>,
    terminal: mpsc::UnboundedSender<TerminalFailure>,
    shutdown: CancellationToken,
    max_frame_bytes: u64,
) where
    S: futures_util::Stream<Item = Result<Message, E>> + Unpin,
{
    loop {
        let message = tokio::select! {
            () = shutdown.cancelled() => break,
            message = stream.next() => match message {
                Some(Ok(message)) => message,
                Some(Err(_)) => {
                    let _ = terminal.send(TerminalFailure::transport("reader_failure"));
                    break;
                }
                None => {
                    let _ = terminal.send(TerminalFailure::transport("socket_closed"));
                    break;
                }
            }
        };
        let frame = match message {
            Message::Text(text) => {
                if text.len() as u64 > max_frame_bytes {
                    let _ = terminal.send(TerminalFailure::protocol(
                        ProtocolError::new(
                            "MYFORGE_OUTPUT_TOO_LARGE",
                            "WebSocket frame exceeds the configured limit",
                        )
                        .unsafe_response(),
                    ));
                    break;
                }
                InboundFrame::Text(text.as_bytes().to_vec())
            }
            Message::Binary(_) => {
                let _ = terminal.send(TerminalFailure::protocol(
                    ProtocolError::new(
                        "MYFORGE_MESSAGE_IJSON_INVALID",
                        "binary WebSocket frames are not accepted",
                    )
                    .unsafe_response(),
                ));
                break;
            }
            Message::Ping(payload) => InboundFrame::Ping(payload.to_vec()),
            Message::Pong(_) => InboundFrame::Pong,
            Message::Close(_) => InboundFrame::Close,
            Message::Frame(_) => continue,
        };
        let sent = tokio::select! {
            () = shutdown.cancelled() => break,
            sent = sender.send(frame) => sent,
        };
        if sent.is_err() {
            break;
        }
    }
}

fn spawn_handshake_timer(
    shutdown: CancellationToken,
    terminal: mpsc::UnboundedSender<TerminalFailure>,
    delay_ms: u64,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        tokio::select! {
            () = shutdown.cancelled() => {}
            () = tokio::time::sleep(Duration::from_millis(delay_ms)) => {
                let _ = terminal.send(TerminalFailure::protocol(ProtocolError::new(
                    "MYFORGE_PROTOCOL_STATE_INVALID",
                    "server challenge timed out",
                )));
            }
        }
    })
}

fn spawn_heartbeat(
    outbound: OutboundHandle,
    registry: Arc<RequestRegistry>,
    clock: Arc<dyn Clock>,
    terminal: mpsc::UnboundedSender<TerminalFailure>,
    shutdown: CancellationToken,
    identity: ConnectionIdentity,
    effective: EffectiveLimits,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut sequence = 0_u32;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = tokio::time::sleep(Duration::from_millis(effective.heartbeat_interval_ms)) => {}
            }
            let active = registry
                .active_request_for_connection(&identity.connection_id)
                .await;
            let timestamp_ms = clock.now_ms();
            let send = outbound
                .send_signed(
                    &AgentHeartbeat {
                        protocol_version: 1,
                        message_type: "agent.heartbeat",
                        connection_id: &identity.connection_id,
                        agent_id: &identity.agent_id,
                        project_id: &identity.project_id,
                        sequence,
                        state: if active.is_some() { "running" } else { "idle" },
                        active_request_id: active.as_deref(),
                        timestamp_ms,
                        expires_at_ms: timestamp_ms.saturating_add(effective.auth_ttl_ms),
                        nonce: random_base64url::<16>(),
                    },
                    timestamp_ms.saturating_add(effective.auth_ttl_ms),
                )
                .await;
            if let Err(error) = send {
                let _ = terminal.send(TerminalFailure::protocol(error));
                break;
            }
            sequence = if sequence == 2_147_483_647 {
                0
            } else {
                sequence + 1
            };
        }
    })
}

async fn await_task(task: JoinHandle<()>) {
    let mut task = task;
    if tokio::time::timeout(TASK_SHUTDOWN_TIMEOUT, &mut task)
        .await
        .is_err()
    {
        task.abort();
        let _ = task.await;
    }
}

async fn drain_workers(workers: &mut JoinSet<()>) {
    let deadline = tokio::time::Instant::now() + TASK_SHUTDOWN_TIMEOUT;
    while !workers.is_empty() {
        if tokio::time::timeout_at(deadline, workers.join_next())
            .await
            .is_err()
        {
            workers.abort_all();
            break;
        }
    }
    while workers.join_next().await.is_some() {}
}

fn safe_protocol_error_message(code: &str) -> &'static str {
    match code {
        "MYFORGE_SERVER_SIGNATURE_INVALID" => "server message signature is invalid",
        "MYFORGE_IDENTITY_MISMATCH" => "message identity does not match this agent",
        "MYFORGE_MESSAGE_EXPIRED" => "message is outside the accepted time window",
        "MYFORGE_REPLAY_DETECTED" => "message nonce was already used",
        "MYFORGE_LIMIT_MISMATCH" => "message limits do not match the connection",
        "MYFORGE_MESSAGE_IJSON_INVALID" => "message is not valid interoperable JSON",
        "MYFORGE_MESSAGE_SCHEMA_INVALID" => "message schema is invalid",
        "MYFORGE_PROTOCOL_VERSION_UNSUPPORTED" => "protocol version is unsupported",
        "MYFORGE_DUPLICATE_REQUEST_CONFLICT" => "request conflicts with an existing request",
        "MYFORGE_AGENT_BUSY" => "agent protocol capacity is exhausted",
        "MYFORGE_OUTPUT_TOO_LARGE" => "message exceeds the negotiated size limit",
        _ => "message is not valid in the current connection state",
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Context, Poll, Waker};

    use ed25519_dalek::SigningKey;
    use futures_util::{Sink, StreamExt};

    use super::*;

    struct NoJitter;

    impl BackoffJitter for NoJitter {
        fn apply(&self, base: Duration, _attempt: u32) -> Duration {
            base
        }
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(backoff_delay(0), Duration::from_millis(250));
        assert_eq!(backoff_delay(1), Duration::from_millis(500));
        assert_eq!(backoff_delay(7), Duration::from_millis(30_000));
        assert_eq!(backoff_delay(100), Duration::from_millis(30_000));
        assert_eq!(NoJitter.apply(backoff_delay(3), 3), Duration::from_secs(2));
    }

    #[test]
    fn websocket_parser_limits_match_local_configuration() {
        let config = client_websocket_config(33_554_432);
        assert_eq!(config.max_message_size, Some(33_554_432));
        assert_eq!(config.max_frame_size, Some(33_554_432));
    }

    struct FailingSink;

    impl Sink<Message> for FailingSink {
        type Error = ();

        fn poll_ready(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, _item: Message) -> Result<(), Self::Error> {
            Err(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn writer_failure_completes_waiter_and_reports_terminal_failure() {
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let writer = tokio::spawn(writer_task(
            FailingSink,
            receiver,
            terminal_tx,
            shutdown.child_token(),
        ));
        let outbound = OutboundHandle {
            sender,
            signing_key: Arc::new(SigningKey::from_bytes(&[7; 32])),
            clock: Arc::new(SystemClock),
            max_frame_bytes: Arc::new(AtomicU64::new(1_024)),
            write_timeout: Duration::from_secs(1),
        };

        assert_eq!(
            outbound
                .send_control(Message::Pong(Vec::new().into()))
                .await
                .unwrap_err()
                .code(),
            "MYFORGE_AGENT_DISCONNECTED"
        );
        assert_eq!(terminal_rx.recv().await.unwrap().reason, "writer_failure");
        shutdown.cancel();
        await_task(writer).await;
    }

    #[derive(Default)]
    struct GateState {
        permits: AtomicUsize,
        polls: AtomicUsize,
        writes: Mutex<Vec<String>>,
        waker: Mutex<Option<Waker>>,
    }

    impl GateState {
        fn grant(&self) {
            self.permits.fetch_add(1, Ordering::SeqCst);
            if let Some(waker) = self.waker.lock().unwrap().take() {
                waker.wake();
            }
        }
    }

    struct GateSink(Arc<GateState>);

    impl Sink<Message> for GateSink {
        type Error = ();

        fn poll_ready(
            self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            self.0.polls.fetch_add(1, Ordering::SeqCst);
            let mut permits = self.0.permits.load(Ordering::SeqCst);
            while permits > 0 {
                match self.0.permits.compare_exchange(
                    permits,
                    permits - 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => return Poll::Ready(Ok(())),
                    Err(actual) => permits = actual,
                }
            }
            *self.0.waker.lock().unwrap() = Some(context.waker().clone());
            Poll::Pending
        }

        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            let label = match item {
                Message::Text(text) => text.to_string(),
                _ => "control".to_string(),
            };
            self.0.writes.lock().unwrap().push(label);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    fn test_outbound(
        sender: mpsc::Sender<OutboundItem>,
        write_timeout: Duration,
    ) -> OutboundHandle {
        OutboundHandle {
            sender,
            signing_key: Arc::new(SigningKey::from_bytes(&[7; 32])),
            clock: Arc::new(SystemClock),
            max_frame_bytes: Arc::new(AtomicU64::new(1_024)),
            write_timeout,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn queued_and_inflight_writes_share_one_deadline_and_never_write_late() {
        let gate = Arc::new(GateState::default());
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let writer = tokio::spawn(writer_task(
            GateSink(gate.clone()),
            receiver,
            terminal_tx,
            shutdown.child_token(),
        ));
        let outbound = test_outbound(sender, Duration::from_millis(100));

        let first_outbound = outbound.clone();
        let first = tokio::spawn(async move {
            first_outbound
                .send_control(Message::Text("first".into()))
                .await
        });
        while gate.polls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        let second_outbound = outbound.clone();
        let second = tokio::spawn(async move {
            second_outbound
                .send_control(Message::Text("second".into()))
                .await
        });
        let third_outbound = outbound.clone();
        let third = tokio::spawn(async move {
            third_outbound
                .send_control(Message::Text("third".into()))
                .await
        });
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_millis(60)).await;
        gate.grant();
        first.await.unwrap().unwrap();
        assert_eq!(gate.writes.lock().unwrap().as_slice(), ["first"]);

        tokio::time::advance(Duration::from_millis(41)).await;
        assert_eq!(
            second.await.unwrap().unwrap_err().code(),
            "MYFORGE_AGENT_DISCONNECTED"
        );
        assert_eq!(
            third.await.unwrap().unwrap_err().code(),
            "MYFORGE_AGENT_DISCONNECTED"
        );
        assert_eq!(terminal_rx.recv().await.unwrap().reason, "writer_deadline");
        gate.grant();
        tokio::task::yield_now().await;
        assert_eq!(gate.writes.lock().unwrap().as_slice(), ["first"]);
        await_task(writer).await;
    }

    #[tokio::test]
    async fn inbound_fifo_backpressures_a_burst_and_resumes_in_order() {
        const FRAMES: usize = QUEUE_CAPACITY + 6;
        let polled = Arc::new(AtomicUsize::new(0));
        let stream_polled = polled.clone();
        let stream = futures_util::stream::iter(
            (0..FRAMES).map(|index| Ok::<_, ()>(Message::Text(index.to_string().into()))),
        )
        .inspect(move |_| {
            stream_polled.fetch_add(1, Ordering::SeqCst);
        });
        let (sender, mut receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let reader = tokio::spawn(reader_task(
            stream,
            sender,
            terminal_tx,
            shutdown.child_token(),
            1_024,
        ));

        for _ in 0..100 {
            if polled.load(Ordering::SeqCst) == QUEUE_CAPACITY + 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(polled.load(Ordering::SeqCst), QUEUE_CAPACITY + 1);

        for expected in 0..FRAMES {
            let frame = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
                .await
                .unwrap()
                .unwrap();
            let InboundFrame::Text(bytes) = frame else {
                panic!("expected text frame");
            };
            assert_eq!(String::from_utf8(bytes).unwrap(), expected.to_string());
        }
        await_task(reader).await;
        assert_eq!(polled.load(Ordering::SeqCst), FRAMES);
        assert_eq!(terminal_rx.recv().await.unwrap().reason, "socket_closed");
    }

    fn test_effective_limits() -> EffectiveLimits {
        EffectiveLimits {
            auth_ttl_ms: 5_000,
            command_ttl_ms: 5_000,
            server_clock_skew_ms: 1_000,
            agent_clock_skew_ms: 1_000,
            heartbeat_interval_ms: 1_000,
            heartbeat_timeout_ms: 5_000,
            command_timeout_ms: 5_000,
            cancel_timeout_ms: 1_000,
            max_output_bytes: 4_096,
            ws_max_message_bytes: 524_288,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn termination_cancels_active_request_before_waiting_on_stalled_writer() {
        let registry = Arc::new(RequestRegistry::new(8));
        let decision = registry
            .begin("connection", "request", "digest", 100)
            .await
            .unwrap();
        let ExecuteDecision::New { cancellation } = decision else {
            panic!("expected active request");
        };
        let state = ConnectionState::Registered {
            connection_id: "connection".to_string(),
            effective: test_effective_limits(),
        };
        let gate = Arc::new(GateState::default());
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (terminal_tx, _terminal_rx) = mpsc::unbounded_channel();
        let writer_shutdown = CancellationToken::new();
        let writer = tokio::spawn(writer_task(
            GateSink(gate.clone()),
            receiver,
            terminal_tx,
            writer_shutdown.child_token(),
        ));
        let outbound = test_outbound(sender, Duration::from_secs(30));
        let termination_registry = registry.clone();
        let termination = tokio::spawn(async move {
            cancel_connection_requests(&termination_registry, &state, 100, 10_000).await;
            let _ = outbound.close(1001, "agent_shutdown").await;
        });

        while gate.polls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        assert!(cancellation.is_cancelled());
        assert!(!termination.is_finished());

        writer_shutdown.cancel();
        termination.await.unwrap();
        await_task(writer).await;
        assert!(matches!(
            registry
                .begin("connection", "request", "digest", 101)
                .await
                .unwrap(),
            ExecuteDecision::DuplicateCompleted {
                response: CachedResponse::NoReplay
            }
        ));
    }
}
