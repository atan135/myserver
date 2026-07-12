use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Message, WebSocketConfig};
use tokio_util::sync::CancellationToken;

use crate::command::{
    CommandControl, CommandHandler, CommandHandlerOutcome, StartedExecutionOutcome,
};
use crate::config::AgentConfig;
use crate::error::{AgentError, ErrorCode};
use crate::preflight::PreflightReport;
use crate::protocol::{
    JsonValue, MAX_SAFE_INTEGER, ProtocolError, QUEUE_CAPACITY, SUBPROTOCOL, parse_canonical_frame,
    random_base64url, semantic_digest, sign_message, verify_message_signature,
};
use crate::schemas::{
    AgentHeartbeat, AgentHello, AgentRegister, ArtifactSummary, AuditSummary, CommandErrorMessage,
    CommandExecute, CommandRejection, CommandResultMessage, CommandResultSemantic, CommandStarted,
    EffectiveLimits, ProtocolErrorMessage, ServerMessage, parse_server_message,
    validate_challenge_compatibility, validate_execute_business, validate_message_time,
};
use crate::state::{
    CachedResponse, CancelDecision, CompletionDecision, DeliveryGeneration, DeliveryLease,
    ExecuteDecision, ReplayCache, RequestRegistry, StartedDeliveryCandidate,
};

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
                        let execution_mode = if config.dry_run() {
                            "dry_run"
                        } else {
                            "codex_exec"
                        };
                        self.handle_execute(
                            ConnectionIdentity::new(
                                config,
                                state.connection_id().unwrap_or_default().to_string(),
                            ),
                            execution_mode,
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
                        let identity = ConnectionIdentity::new(
                            config,
                            state.connection_id().unwrap_or_default().to_string(),
                        );
                        cancel_request_owned(
                            &identity,
                            effective,
                            &cancel.request_id,
                            cancel.cancel_deadline_at_ms,
                            outbound,
                            self.hooks.clock.as_ref(),
                            &self.request_registry,
                        )
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
        identity: ConnectionIdentity,
        execution_mode: &'static str,
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
        let cancellation_result =
            cancelled_semantic_result(&command, execution_mode, None, self.hooks.clock.now_ms());
        let decision = self
            .request_registry
            .begin(
                &identity.connection_id,
                &command.request_id,
                &digest,
                cancellation_result,
                self.hooks.clock.now_ms(),
            )
            .await?;
        match decision {
            ExecuteDecision::DuplicateActive { started, .. } => {
                if let Some(started) = started {
                    let identity = identity.clone();
                    let request_id = command.request_id.clone();
                    let outbound = outbound.clone();
                    let clock = self.hooks.clock.clone();
                    let terminal_tx = terminal_tx.clone();
                    workers.spawn(async move {
                        if let Err(error) = send_started_owned(
                            &identity,
                            effective,
                            StartedDelivery {
                                request_id: &request_id,
                                execution_mode,
                            },
                            started,
                            &outbound,
                            clock.as_ref(),
                        )
                        .await
                        {
                            let _ = terminal_tx.send(TerminalFailure::protocol(error));
                        }
                    });
                }
                Ok(())
            }
            ExecuteDecision::DuplicateCompleted { .. } => {
                let request_id = command.request_id.clone();
                let outbound = outbound.clone();
                let clock = self.hooks.clock.clone();
                let registry = self.request_registry.clone();
                let terminal_tx = terminal_tx.clone();
                workers.spawn(async move {
                    if let Err(error) = replay_completed_response(
                        &identity,
                        effective,
                        &request_id,
                        &outbound,
                        clock.as_ref(),
                        &registry,
                    )
                    .await
                    {
                        let _ = terminal_tx.send(TerminalFailure::protocol(error));
                    }
                });
                Ok(())
            }
            ExecuteDecision::New { cancellation } => {
                let handler = self.handler.clone();
                let registry = self.request_registry.clone();
                let outbound = outbound.clone();
                let clock = self.hooks.clock.clone();
                let request_id = command.request_id.clone();
                let command_for_result = command.clone();
                let terminal_tx = terminal_tx.clone();
                workers.spawn(async move {
                    if let Err(rejection) = validate_execute_business(&command, effective) {
                        if rejection.protocol_fatal {
                            let error =
                                ProtocolError::new(rejection.error_code, rejection.error_message)
                                    .with_request_id(Some(request_id.clone()));
                            let _ = terminal_tx.send(TerminalFailure::protocol(error));
                        } else if let Err(error) = send_and_cache_error(
                            &identity,
                            effective,
                            &request_id,
                            &command_for_result,
                            execution_mode,
                            rejection,
                            &outbound,
                            clock.as_ref(),
                            &registry,
                        )
                        .await
                        {
                            let _ = terminal_tx.send(TerminalFailure::protocol(error));
                        }
                        return;
                    }
                    let received_at_ms = clock.now_ms();
                    let outcome = handler
                        .execute(
                            command,
                            CommandControl::from_cancellation(cancellation.clone(), received_at_ms),
                        )
                        .await;
                    match outcome {
                        CommandHandlerOutcome::PreStartError(rejection) => {
                            if let Err(error) = send_and_cache_error(
                                &identity,
                                effective,
                                &request_id,
                                &command_for_result,
                                execution_mode,
                                rejection,
                                &outbound,
                                clock.as_ref(),
                                &registry,
                            )
                            .await
                            {
                                let _ = terminal_tx.send(TerminalFailure::protocol(error));
                            }
                        }
                        CommandHandlerOutcome::CancelledBeforeStart => {
                            let result = cancelled_semantic_result(
                                &command_for_result,
                                execution_mode,
                                None,
                                clock.now_ms(),
                            );
                            if let Err(error) = send_and_cache_result(
                                &identity,
                                effective,
                                &request_id,
                                &command_for_result,
                                execution_mode,
                                result,
                                &outbound,
                                clock.as_ref(),
                                &registry,
                            )
                            .await
                            {
                                let _ = terminal_tx.send(TerminalFailure::protocol(error));
                            }
                        }
                        CommandHandlerOutcome::CompletedBeforeStart(result) => {
                            let mut result = *result;
                            if cancellation.is_cancelled() && result.status != "cancelled" {
                                force_cancelled(&mut result, None, clock.now_ms());
                            }
                            if let Err(error) = send_and_cache_result(
                                &identity,
                                effective,
                                &request_id,
                                &command_for_result,
                                execution_mode,
                                result,
                                &outbound,
                                clock.as_ref(),
                                &registry,
                            )
                            .await
                            {
                                let _ = terminal_tx.send(TerminalFailure::protocol(error));
                            }
                        }
                        CommandHandlerOutcome::Started(execution) => {
                            let started_at_ms = execution.started_at_ms();
                            let started =
                                match registry.mark_started(&request_id, started_at_ms).await {
                                    Ok(started) => started,
                                    Err(error) => {
                                        cancellation.cancel();
                                        let _ = execution.finish().await;
                                        let _ = terminal_tx.send(TerminalFailure::protocol(error));
                                        return;
                                    }
                                };
                            if let Some(started) = started
                                && let Err(error) = send_started_owned(
                                    &identity,
                                    effective,
                                    StartedDelivery {
                                        request_id: &request_id,
                                        execution_mode,
                                    },
                                    started,
                                    &outbound,
                                    clock.as_ref(),
                                )
                                .await
                            {
                                cancellation.cancel();
                                let _ = execution.finish().await;
                                let _ = terminal_tx.send(TerminalFailure::protocol(error));
                                return;
                            }
                            let mut result = match execution.finish().await {
                                StartedExecutionOutcome::Result(result) => *result,
                                StartedExecutionOutcome::FailClosed { reason } => {
                                    let _ = terminal_tx.send(TerminalFailure::transport(reason));
                                    return;
                                }
                            };
                            let committed_started_at_ms =
                                match registry.committed_started_at_ms(&request_id).await {
                                    Ok(started_at_ms) => started_at_ms,
                                    Err(error) => {
                                        let _ = terminal_tx.send(TerminalFailure::protocol(error));
                                        return;
                                    }
                                };
                            if cancellation.is_cancelled() {
                                force_cancelled(
                                    &mut result,
                                    committed_started_at_ms,
                                    clock.now_ms(),
                                );
                            }
                            if result.started_at_ms != committed_started_at_ms {
                                let error = ProtocolError::new(
                                    "MYFORGE_PROTOCOL_STATE_INVALID",
                                    "command result start time is inconsistent",
                                )
                                .with_request_id(Some(request_id.clone()));
                                let _ = terminal_tx.send(TerminalFailure::protocol(error));
                                return;
                            }
                            if let Err(error) = send_and_cache_result(
                                &identity,
                                effective,
                                &request_id,
                                &command_for_result,
                                execution_mode,
                                result,
                                &outbound,
                                clock.as_ref(),
                                &registry,
                            )
                            .await
                            {
                                let _ = terminal_tx.send(TerminalFailure::protocol(error));
                            }
                        }
                    }
                });
                Ok(())
            }
        }
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

struct StartedDelivery<'a> {
    request_id: &'a str,
    execution_mode: &'static str,
}

async fn send_started_owned(
    identity: &ConnectionIdentity,
    effective: EffectiveLimits,
    started: StartedDelivery<'_>,
    candidate: StartedDeliveryCandidate,
    outbound: &OutboundHandle,
    clock: &dyn Clock,
) -> Result<DeliveryResult, ProtocolError> {
    let delivery = candidate.delivery;
    let lease = candidate.lease;
    let timestamp_ms = clock.now_ms();
    let expires_at_ms = timestamp_ms.saturating_add(effective.auth_ttl_ms);
    let Some(reservation) = outbound.reserve_delivery(expires_at_ms, &lease).await? else {
        return Ok(DeliveryResult::Superseded);
    };
    let delivery_guard = delivery.lock().await;
    if !lease.is_current() {
        drop(delivery_guard);
        drop(reservation);
        return Ok(DeliveryResult::Superseded);
    }
    let frame = sign_message(
        &CommandStarted {
            protocol_version: 1,
            message_type: "command.started",
            connection_id: &identity.connection_id,
            request_id: started.request_id,
            agent_id: &identity.agent_id,
            project_id: &identity.project_id,
            execution_mode: started.execution_mode,
            started_at_ms: candidate.started_at_ms,
            timestamp_ms,
            expires_at_ms,
            nonce: random_base64url::<16>(),
        },
        &outbound.signing_key,
    )?;
    let pending =
        outbound.commit_reserved_frame(reservation, frame, DeliveryBoundary::Started(lease))?;
    drop(delivery_guard);
    pending.wait().await
}

async fn reserve_current_delivery(
    outbound: &OutboundHandle,
    registry: &RequestRegistry,
    request_id: &str,
    base_expires_at_ms: u64,
) -> Result<(ReservedOutbound, Arc<DeliveryGeneration>, DeliveryLease), ProtocolError> {
    loop {
        let delivery = registry.delivery(request_id).await?;
        let lease = delivery.lease();
        let expires_at_ms = registry
            .cancel_deadline_at_ms(request_id)
            .await?
            .map_or(base_expires_at_ms, |deadline| {
                deadline.min(base_expires_at_ms)
            });
        if let Some(reservation) = outbound.reserve_delivery(expires_at_ms, &lease).await? {
            return Ok((reservation, delivery, lease));
        }
    }
}

async fn cancel_request_owned(
    identity: &ConnectionIdentity,
    effective: EffectiveLimits,
    request_id: &str,
    cancel_deadline_at_ms: u64,
    outbound: &OutboundHandle,
    clock: &dyn Clock,
    registry: &RequestRegistry,
) -> Result<(), ProtocolError> {
    let delivery = registry.delivery(request_id).await?;
    let delivery_guard = delivery.lock().await;
    let decision = registry.cancel(request_id, cancel_deadline_at_ms).await?;
    match decision {
        CancelDecision::CompletedNeedsCancellation {
            response: CachedResponse::CommandResult(result),
            started_at_ms,
        } => {
            let mut result = *result;
            force_cancelled(&mut result, started_at_ms, clock.now_ms());
            let result =
                fit_command_result(identity, effective, request_id, result, outbound, clock)?;
            registry
                .replace_completed_with_cancelled(request_id, result.clone())
                .await?;
        }
        CancelDecision::DuplicateCompleted {
            response: CachedResponse::CommandResult(_),
        } => {}
        CancelDecision::CompletedNeedsCancellation { .. }
        | CancelDecision::DuplicateCompleted { .. } => {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "completed request has no replayable cancellation result",
            )
            .with_request_id(Some(request_id.to_string())));
        }
        CancelDecision::First | CancelDecision::DuplicateActive => {
            drop(delivery_guard);
            return Ok(());
        }
    }
    drop(delivery_guard);

    let base_expires_at_ms =
        cancel_deadline_at_ms.min(clock.now_ms().saturating_add(effective.auth_ttl_ms));
    loop {
        let reserved =
            reserve_current_delivery(outbound, registry, request_id, base_expires_at_ms).await;
        let (reservation, delivery, lease) = match reserved {
            Ok(reserved) => reserved,
            Err(error) => {
                registry.mark_no_replay(request_id).await?;
                return Err(error);
            }
        };
        let delivery_guard = delivery.lock().await;
        if !lease.is_current() {
            drop(delivery_guard);
            drop(reservation);
            continue;
        }
        let (response, stored_deadline) = registry.completed_response(request_id).await?;
        let CachedResponse::CommandResult(result) = response else {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "completed request has no replayable cancellation result",
            )
            .with_request_id(Some(request_id.to_string())));
        };
        if stored_deadline != Some(cancel_deadline_at_ms) || result.status != "cancelled" {
            return Err(ProtocolError::new(
                "MYFORGE_DUPLICATE_REQUEST_CONFLICT",
                "cancel deadline conflicts with the completed request",
            )
            .with_request_id(Some(request_id.to_string())));
        }
        let prepared = prepare_fitted_command_result(
            identity,
            effective,
            request_id,
            *result,
            Some(cancel_deadline_at_ms),
            outbound,
            clock,
        )?;
        let pending = match outbound.commit_reserved_frame(
            reservation,
            prepared.frame,
            DeliveryBoundary::Terminal(lease),
        ) {
            Ok(pending) => pending,
            Err(error) => {
                registry.mark_no_replay(request_id).await?;
                return Err(error);
            }
        };
        drop(delivery_guard);
        match pending.wait().await? {
            DeliveryResult::Written | DeliveryResult::Superseded => return Ok(()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_and_cache_result(
    identity: &ConnectionIdentity,
    effective: EffectiveLimits,
    request_id: &str,
    expected: &CommandExecute,
    expected_execution_mode: &str,
    result: CommandResultSemantic,
    outbound: &OutboundHandle,
    clock: &dyn Clock,
    registry: &RequestRegistry,
) -> Result<(), ProtocolError> {
    if result.artifact_file != expected.input.artifact_file
        || result.consumer_target_file != expected.input.consumer_target_file
        || result.execution_mode != expected_execution_mode
    {
        return Err(ProtocolError::new(
            "MYFORGE_PROTOCOL_STATE_INVALID",
            "command result does not match the request",
        )
        .with_request_id(Some(request_id.to_string())));
    }
    let proposed_result =
        fit_command_result(identity, effective, request_id, result, outbound, clock)?;
    let retention_ms = effective
        .command_timeout_ms
        .saturating_add(effective.command_ttl_ms);
    let base_expires_at_ms = clock.now_ms().saturating_add(effective.auth_ttl_ms);
    loop {
        let (reservation, delivery, lease) =
            reserve_current_delivery(outbound, registry, request_id, base_expires_at_ms).await?;
        let delivery_guard = delivery.lock().await;
        if !lease.is_current() {
            drop(delivery_guard);
            drop(reservation);
            continue;
        }
        let mut final_result = proposed_result.clone();
        match registry
            .complete_response(
                request_id,
                CachedResponse::CommandResult(Box::new(final_result.clone())),
                clock.now_ms(),
                retention_ms,
            )
            .await?
        {
            CompletionDecision::Stored => {}
            CompletionDecision::CancellationRequired {
                response,
                started_at_ms,
            } => {
                final_result = match response {
                    CachedResponse::CommandResult(result) => *result,
                    _ => cancelled_semantic_result(
                        expected,
                        expected_execution_mode,
                        started_at_ms,
                        clock.now_ms(),
                    ),
                };
                force_cancelled(&mut final_result, started_at_ms, clock.now_ms());
                final_result = fit_command_result(
                    identity,
                    effective,
                    request_id,
                    final_result,
                    outbound,
                    clock,
                )?;
                if registry
                    .complete_response(
                        request_id,
                        CachedResponse::CommandResult(Box::new(final_result.clone())),
                        clock.now_ms(),
                        retention_ms,
                    )
                    .await?
                    != CompletionDecision::Stored
                {
                    return Err(ProtocolError::new(
                        "MYFORGE_PROTOCOL_STATE_INVALID",
                        "cancelled result could not be finalized",
                    )
                    .with_request_id(Some(request_id.to_string())));
                }
            }
        }
        let delivery_deadline_at_ms = if final_result.status == "cancelled" {
            registry.cancel_deadline_at_ms(request_id).await?
        } else {
            None
        };
        let prepared = prepare_fitted_command_result(
            identity,
            effective,
            request_id,
            final_result.clone(),
            delivery_deadline_at_ms,
            outbound,
            clock,
        )?;
        if prepared.result != final_result {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "cached result differs from the transmitted result",
            )
            .with_request_id(Some(request_id.to_string())));
        }
        let pending = match outbound.commit_reserved_frame(
            reservation,
            prepared.frame,
            DeliveryBoundary::Terminal(lease),
        ) {
            Ok(pending) => pending,
            Err(error) => {
                registry.mark_no_replay(request_id).await?;
                return Err(error);
            }
        };
        drop(delivery_guard);
        return match pending.wait().await? {
            DeliveryResult::Written | DeliveryResult::Superseded => Ok(()),
        };
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_and_cache_error(
    identity: &ConnectionIdentity,
    effective: EffectiveLimits,
    request_id: &str,
    expected: &CommandExecute,
    expected_execution_mode: &str,
    rejection: CommandRejection,
    outbound: &OutboundHandle,
    clock: &dyn Clock,
    registry: &RequestRegistry,
) -> Result<(), ProtocolError> {
    rejection.validate()?;
    let retention_ms = effective
        .command_timeout_ms
        .saturating_add(effective.command_ttl_ms);
    let base_expires_at_ms = clock.now_ms().saturating_add(effective.auth_ttl_ms);
    loop {
        let (reservation, delivery, lease) =
            reserve_current_delivery(outbound, registry, request_id, base_expires_at_ms).await?;
        let delivery_guard = delivery.lock().await;
        if !lease.is_current() {
            drop(delivery_guard);
            drop(reservation);
            continue;
        }
        let frame = match registry
            .complete_response(
                request_id,
                CachedResponse::CommandError(rejection.clone()),
                clock.now_ms(),
                retention_ms,
            )
            .await?
        {
            CompletionDecision::Stored => {
                prepare_command_error_frame(
                    identity, effective, request_id, &rejection, outbound, clock,
                )?
                .0
            }
            CompletionDecision::CancellationRequired { started_at_ms, .. } => {
                let result = fit_command_result(
                    identity,
                    effective,
                    request_id,
                    cancelled_semantic_result(
                        expected,
                        expected_execution_mode,
                        started_at_ms,
                        clock.now_ms(),
                    ),
                    outbound,
                    clock,
                )?;
                if registry
                    .complete_response(
                        request_id,
                        CachedResponse::CommandResult(Box::new(result.clone())),
                        clock.now_ms(),
                        retention_ms,
                    )
                    .await?
                    != CompletionDecision::Stored
                {
                    return Err(ProtocolError::new(
                        "MYFORGE_PROTOCOL_STATE_INVALID",
                        "cancelled result could not be finalized",
                    )
                    .with_request_id(Some(request_id.to_string())));
                }
                prepare_fitted_command_result(
                    identity,
                    effective,
                    request_id,
                    result,
                    registry.cancel_deadline_at_ms(request_id).await?,
                    outbound,
                    clock,
                )?
                .frame
            }
        };
        let pending = match outbound.commit_reserved_frame(
            reservation,
            frame,
            DeliveryBoundary::Terminal(lease),
        ) {
            Ok(pending) => pending,
            Err(error) => {
                registry.mark_no_replay(request_id).await?;
                return Err(error);
            }
        };
        drop(delivery_guard);
        return match pending.wait().await? {
            DeliveryResult::Written | DeliveryResult::Superseded => Ok(()),
        };
    }
}

async fn replay_completed_response(
    identity: &ConnectionIdentity,
    effective: EffectiveLimits,
    request_id: &str,
    outbound: &OutboundHandle,
    clock: &dyn Clock,
    registry: &RequestRegistry,
) -> Result<(), ProtocolError> {
    let base_expires_at_ms = clock.now_ms().saturating_add(effective.auth_ttl_ms);
    loop {
        let (reservation, delivery, lease) =
            reserve_current_delivery(outbound, registry, request_id, base_expires_at_ms).await?;
        let delivery_guard = delivery.lock().await;
        if !lease.is_current() {
            drop(delivery_guard);
            drop(reservation);
            continue;
        }
        let (response, cancel_deadline_at_ms) = registry.completed_response(request_id).await?;
        let frame = match response {
            CachedResponse::CommandError(rejection) => {
                prepare_command_error_frame(
                    identity, effective, request_id, &rejection, outbound, clock,
                )?
                .0
            }
            CachedResponse::CommandResult(result) => {
                prepare_fitted_command_result(
                    identity,
                    effective,
                    request_id,
                    *result,
                    cancel_deadline_at_ms,
                    outbound,
                    clock,
                )?
                .frame
            }
            CachedResponse::NoReplay => {
                return Err(ProtocolError::new(
                    "MYFORGE_DUPLICATE_REQUEST_CONFLICT",
                    "request belongs to a closed connection",
                )
                .with_request_id(Some(request_id.to_string())));
            }
        };
        let pending = match outbound.commit_reserved_frame(
            reservation,
            frame,
            DeliveryBoundary::Terminal(lease),
        ) {
            Ok(pending) => pending,
            Err(error) => {
                registry.mark_no_replay(request_id).await?;
                return Err(error);
            }
        };
        drop(delivery_guard);
        return match pending.wait().await? {
            DeliveryResult::Written | DeliveryResult::Superseded => Ok(()),
        };
    }
}

struct PreparedCommandResult {
    result: CommandResultSemantic,
    frame: String,
    #[cfg(test)]
    send_deadline_at_ms: u64,
}

fn prepare_fitted_command_result(
    identity: &ConnectionIdentity,
    effective: EffectiveLimits,
    request_id: &str,
    result: CommandResultSemantic,
    _delivery_deadline_at_ms: Option<u64>,
    outbound: &OutboundHandle,
    clock: &dyn Clock,
) -> Result<PreparedCommandResult, ProtocolError> {
    let timestamp_ms = clock.now_ms().min(MAX_SAFE_INTEGER as u64);
    let expires_at_ms = timestamp_ms
        .saturating_add(effective.auth_ttl_ms)
        .min(MAX_SAFE_INTEGER as u64);
    let frame = sign_result(
        identity,
        request_id,
        &result,
        timestamp_ms,
        expires_at_ms,
        outbound,
    )?;
    Ok(PreparedCommandResult {
        result,
        frame,
        #[cfg(test)]
        send_deadline_at_ms: _delivery_deadline_at_ms
            .map_or(expires_at_ms, |deadline| deadline.min(expires_at_ms)),
    })
}

#[cfg(test)]
async fn send_command_result_owned(
    identity: &ConnectionIdentity,
    effective: EffectiveLimits,
    request_id: &str,
    result: CommandResultSemantic,
    delivery_deadline_at_ms: Option<u64>,
    outbound: &OutboundHandle,
    clock: &dyn Clock,
) -> Result<CommandResultSemantic, ProtocolError> {
    let result = fit_command_result(identity, effective, request_id, result, outbound, clock)?;
    let prepared = prepare_fitted_command_result(
        identity,
        effective,
        request_id,
        result,
        delivery_deadline_at_ms,
        outbound,
        clock,
    )?;
    outbound
        .send_signed_frame(prepared.frame, prepared.send_deadline_at_ms)
        .await?;
    Ok(prepared.result)
}

fn fit_command_result(
    identity: &ConnectionIdentity,
    effective: EffectiveLimits,
    request_id: &str,
    mut result: CommandResultSemantic,
    outbound: &OutboundHandle,
    clock: &dyn Clock,
) -> Result<CommandResultSemantic, ProtocolError> {
    result.validate(effective.max_output_bytes)?;
    let timestamp_ms = clock.now_ms().min(MAX_SAFE_INTEGER as u64);
    let expires_at_ms = timestamp_ms
        .saturating_add(effective.auth_ttl_ms)
        .min(MAX_SAFE_INTEGER as u64);
    let frame = sign_result(
        identity,
        request_id,
        &result,
        timestamp_ms,
        expires_at_ms,
        outbound,
    )?;
    if frame.len() as u64 <= effective.ws_max_message_bytes {
        return Ok(result);
    }
    tracing::warn!(
        serialized_result_bytes = frame.len(),
        negotiated_frame_bytes = effective.ws_max_message_bytes,
        "command result exceeded the negotiated frame budget"
    );
    result = result.output_too_large_fallback();
    result.validate(effective.max_output_bytes)?;
    let fallback = sign_result(
        identity,
        request_id,
        &result,
        timestamp_ms,
        expires_at_ms,
        outbound,
    )?;
    if fallback.len() as u64 > effective.ws_max_message_bytes {
        return Err(ProtocolError::new(
            "MYFORGE_OUTPUT_TOO_LARGE",
            "minimal command result exceeds the negotiated limit",
        ));
    }
    Ok(result)
}

fn sign_result(
    identity: &ConnectionIdentity,
    request_id: &str,
    result: &CommandResultSemantic,
    timestamp_ms: u64,
    expires_at_ms: u64,
    outbound: &OutboundHandle,
) -> Result<String, ProtocolError> {
    sign_message(
        &CommandResultMessage {
            protocol_version: 1,
            message_type: "command.result",
            connection_id: &identity.connection_id,
            request_id,
            agent_id: &identity.agent_id,
            project_id: &identity.project_id,
            result,
            timestamp_ms,
            expires_at_ms,
            nonce: random_base64url::<16>(),
        },
        &outbound.signing_key,
    )
}

fn cancelled_semantic_result(
    command: &CommandExecute,
    execution_mode: &str,
    started_at_ms: Option<u64>,
    completed_at_ms: u64,
) -> CommandResultSemantic {
    CommandResultSemantic {
        execution_mode: execution_mode.to_string(),
        status: "cancelled".to_string(),
        exit_code: None,
        stdout_preview: String::new(),
        stderr_preview: String::new(),
        stdout_bytes: 0,
        stderr_bytes: 0,
        stdout_truncated: false,
        stderr_truncated: false,
        artifact_file: command.input.artifact_file.clone(),
        consumer_target_file: command.input.consumer_target_file.clone(),
        artifact: ArtifactSummary::missing(),
        audit: AuditSummary::skipped("cancelled"),
        error_code: Some("MYFORGE_COMMAND_CANCELLED".to_string()),
        error_message: Some("command was cancelled".to_string()),
        started_at_ms,
        completed_at_ms: started_at_ms
            .map_or(completed_at_ms, |started| completed_at_ms.max(started)),
    }
}

fn force_cancelled(
    result: &mut CommandResultSemantic,
    started_at_ms: Option<u64>,
    completed_at_ms: u64,
) {
    result.status = "cancelled".to_string();
    if started_at_ms.is_none() {
        result.exit_code = None;
    }
    result.audit = AuditSummary::skipped("cancelled");
    result.error_code = Some("MYFORGE_COMMAND_CANCELLED".to_string());
    result.error_message = Some("command was cancelled".to_string());
    result.started_at_ms = started_at_ms;
    result.completed_at_ms =
        started_at_ms.map_or(completed_at_ms, |started| completed_at_ms.max(started));
}

fn prepare_command_error_frame(
    identity: &ConnectionIdentity,
    effective: EffectiveLimits,
    request_id: &str,
    rejection: &CommandRejection,
    outbound: &OutboundHandle,
    clock: &dyn Clock,
) -> Result<(String, u64), ProtocolError> {
    rejection.validate()?;
    let timestamp_ms = clock.now_ms();
    let expires_at_ms = timestamp_ms.saturating_add(effective.auth_ttl_ms);
    let frame = sign_message(
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
            expires_at_ms,
            nonce: random_base64url::<16>(),
        },
        &outbound.signing_key,
    )?;
    Ok((frame, expires_at_ms))
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeliveryResult {
    Written,
    Superseded,
}

enum OutboundPayload {
    Message(Message),
    Close(u16, &'static str),
}

struct OutboundItem {
    payload: OutboundPayload,
    deadline: tokio::time::Instant,
    delivery_boundary: Option<DeliveryBoundary>,
    completion: oneshot::Sender<Result<DeliveryResult, TransportError>>,
}

enum DeliveryBoundary {
    Terminal(DeliveryLease),
    Started(DeliveryLease),
}

impl DeliveryBoundary {
    fn lease(&self) -> &DeliveryLease {
        match self {
            Self::Terminal(lease) | Self::Started(lease) => lease,
        }
    }

    fn try_commit(&self) -> bool {
        match self {
            Self::Terminal(lease) => lease.try_commit_current(),
            Self::Started(lease) => lease.try_commit_started(),
        }
    }
}

struct ReservedOutbound {
    permit: mpsc::OwnedPermit<OutboundItem>,
    deadline: tokio::time::Instant,
}

impl ReservedOutbound {
    fn commit(
        self,
        payload: OutboundPayload,
        delivery_boundary: DeliveryBoundary,
    ) -> PendingOutbound {
        let (completion_tx, completion_rx) = oneshot::channel();
        self.permit.send(OutboundItem {
            payload,
            deadline: self.deadline,
            delivery_boundary: Some(delivery_boundary),
            completion: completion_tx,
        });
        PendingOutbound {
            completion: completion_rx,
            deadline: self.deadline,
        }
    }
}

struct PendingOutbound {
    completion: oneshot::Receiver<Result<DeliveryResult, TransportError>>,
    deadline: tokio::time::Instant,
}

impl PendingOutbound {
    async fn wait(self) -> Result<DeliveryResult, ProtocolError> {
        let remaining = self
            .deadline
            .saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(writer_error());
        }
        tokio::time::timeout(remaining, self.completion)
            .await
            .map_err(|_| writer_error())?
            .map_err(|_| writer_error())?
            .map_err(|_| writer_error())
    }
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

    #[cfg(test)]
    async fn send_signed_frame(
        &self,
        frame: String,
        expires_at_ms: u64,
    ) -> Result<(), ProtocolError> {
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
            delivery_boundary: None,
            completion: completion_tx,
        };
        tokio::time::timeout_at(deadline, self.sender.send(item))
            .await
            .map_err(|_| writer_error())?
            .map_err(|_| writer_error())?;
        PendingOutbound {
            completion: completion_rx,
            deadline,
        }
        .wait()
        .await
        .map(|_| ())
    }

    async fn reserve_delivery(
        &self,
        expires_at_ms: u64,
        lease: &DeliveryLease,
    ) -> Result<Option<ReservedOutbound>, ProtocolError> {
        let now_ms = self.clock.now_ms();
        let expiry_budget = Duration::from_millis(expires_at_ms.saturating_sub(now_ms));
        let timeout = self.write_timeout.min(expiry_budget);
        if timeout.is_zero() {
            return Err(ProtocolError::new(
                "MYFORGE_MESSAGE_EXPIRED",
                "outbound message expired before send",
            ));
        }
        let deadline = tokio::time::Instant::now() + timeout;
        let reserve = self.sender.clone().reserve_owned();
        let permit = tokio::select! {
            biased;
            () = lease.superseded() => return Ok(None),
            result = tokio::time::timeout_at(deadline, reserve) => {
                result.map_err(|_| writer_error())?.map_err(|_| writer_error())?
            }
        };
        if !lease.is_current() {
            drop(permit);
            return Ok(None);
        }
        Ok(Some(ReservedOutbound { permit, deadline }))
    }

    fn commit_reserved_frame(
        &self,
        reservation: ReservedOutbound,
        frame: String,
        boundary: DeliveryBoundary,
    ) -> Result<PendingOutbound, ProtocolError> {
        if frame.len() as u64 > self.max_frame_bytes.load(Ordering::Acquire) {
            return Err(ProtocolError::new(
                "MYFORGE_OUTPUT_TOO_LARGE",
                "outbound frame exceeds the negotiated limit",
            ));
        }
        Ok(reservation.commit(
            OutboundPayload::Message(Message::Text(frame.into())),
            boundary,
        ))
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
            delivery_boundary,
            completion,
        } = item;
        if delivery_boundary
            .as_ref()
            .is_some_and(|boundary| !boundary.lease().is_current())
        {
            let _ = completion.send(Ok(DeliveryResult::Superseded));
            continue;
        }
        if tokio::time::Instant::now() >= deadline {
            let _ = completion.send(Err(TransportError));
            let _ = terminal.send(TerminalFailure::transport("writer_deadline"));
            break;
        }
        let close = matches!(payload, OutboundPayload::Close(_, _));
        let message = match payload {
            OutboundPayload::Message(message) => message,
            OutboundPayload::Close(code, reason) => Message::Close(Some(CloseFrame {
                code: code.into(),
                reason: reason.into(),
            })),
        };
        let superseded = async {
            match delivery_boundary.as_ref() {
                Some(boundary) => boundary.lease().superseded().await,
                None => std::future::pending::<()>().await,
            }
        };
        let ready = tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                let _ = completion.send(Err(TransportError));
                break;
            }
            () = superseded => {
                let _ = completion.send(Ok(DeliveryResult::Superseded));
                continue;
            }
            result = tokio::time::timeout_at(
                deadline,
                std::future::poll_fn(|context| Pin::new(&mut sink).poll_ready(context)),
            ) => result,
        };
        match ready {
            Ok(Ok(())) => {}
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
        if delivery_boundary
            .as_ref()
            .is_some_and(|boundary| !boundary.try_commit())
        {
            let _ = completion.send(Ok(DeliveryResult::Superseded));
            continue;
        }
        if Pin::new(&mut sink).start_send(message).is_err() {
            let _ = completion.send(Err(TransportError));
            let _ = terminal.send(TerminalFailure::transport("writer_failure"));
            break;
        }
        let flushed = tokio::select! {
            biased;
            () = shutdown.cancelled() => {
                let _ = completion.send(Err(TransportError));
                break;
            }
            result = tokio::time::timeout_at(
                deadline,
                std::future::poll_fn(|context| Pin::new(&mut sink).poll_flush(context)),
            ) => result,
        };
        match flushed {
            Ok(Ok(())) => {
                let _ = completion.send(Ok(DeliveryResult::Written));
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

    use crate::schemas::{BlueprintBounds, BlueprintPrompt, CommandInput};

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

    #[tokio::test]
    async fn oversized_result_falls_back_to_a_signed_minimal_failure_frame() {
        let (sender, mut receiver) = mpsc::channel(QUEUE_CAPACITY);
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let verifying_key = signing_key.verifying_key();
        let outbound = OutboundHandle {
            sender,
            signing_key: Arc::new(signing_key),
            clock: Arc::new(SystemClock),
            max_frame_bytes: Arc::new(AtomicU64::new(524_288)),
            write_timeout: Duration::from_secs(1),
        };
        let receiver_task = tokio::spawn(async move {
            let item = receiver.recv().await.unwrap();
            let OutboundPayload::Message(Message::Text(frame)) = item.payload else {
                panic!("expected text result frame");
            };
            assert!(frame.len() <= 524_288);
            let _ = item.completion.send(Ok(DeliveryResult::Written));
            frame.to_string()
        });
        let now = SystemClock.now_ms();
        let result = CommandResultSemantic {
            execution_mode: "codex_exec".to_string(),
            status: "completed".to_string(),
            exit_code: Some(0),
            stdout_preview: "\0".repeat(100_000),
            stderr_preview: "\0".repeat(100_000),
            stdout_bytes: 100_000,
            stderr_bytes: 100_000,
            stdout_truncated: false,
            stderr_truncated: false,
            artifact_file: "artifacts/fangyuan/result.ron".to_string(),
            consumer_target_file: None,
            artifact: ArtifactSummary {
                exists: true,
                sha256: Some("a".repeat(64)),
                bytes: Some(1),
                modified_at_ms: Some(now),
            },
            audit: AuditSummary {
                status: "passed".to_string(),
                errors: Some(0),
                warnings: Some(0),
                primitive_count: Some(1),
                main_code: None,
                reason_code: None,
                findings_preview: Vec::new(),
            },
            error_code: None,
            error_message: None,
            started_at_ms: Some(now),
            completed_at_ms: now,
        };
        let effective = EffectiveLimits {
            auth_ttl_ms: 5_000,
            command_ttl_ms: 5_000,
            server_clock_skew_ms: 1_000,
            agent_clock_skew_ms: 1_000,
            heartbeat_interval_ms: 1_000,
            heartbeat_timeout_ms: 5_000,
            command_timeout_ms: 5_000,
            cancel_timeout_ms: 1_000,
            max_output_bytes: 100_000,
            ws_max_message_bytes: 524_288,
        };
        let sent = send_command_result_owned(
            &ConnectionIdentity {
                connection_id: "67da7da9-a653-4d6e-9e81-f5f8baf874bb".to_string(),
                agent_id: "dev-pc-001".to_string(),
                project_id: "myforge-local".to_string(),
            },
            effective,
            "2d0465b1-dc92-46d2-bc45-c90ed9724f5a",
            result,
            None,
            &outbound,
            &SystemClock,
        )
        .await
        .unwrap();
        assert_eq!(sent.status, "failed");
        assert_eq!(sent.error_code.as_deref(), Some("MYFORGE_OUTPUT_TOO_LARGE"));
        assert!(sent.stdout_preview.is_empty());
        assert!(sent.stderr_preview.is_empty());
        let frame = receiver_task.await.unwrap();
        let value = parse_canonical_frame(frame.as_bytes(), 524_288).unwrap();
        verify_message_signature(&value, &verifying_key).unwrap();
        assert_eq!(value.string_field("type"), Some("command.result"));
        assert_eq!(
            value.string_field("errorCode"),
            Some("MYFORGE_OUTPUT_TOO_LARGE")
        );
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

    #[derive(Default)]
    struct FlushGateState {
        flush_permits: AtomicUsize,
        started: Mutex<Vec<String>>,
        pending: Mutex<Vec<String>>,
        flushed: Mutex<Vec<String>>,
        waker: Mutex<Option<Waker>>,
    }

    impl FlushGateState {
        fn grant_flush(&self) {
            self.flush_permits.fetch_add(1, Ordering::SeqCst);
            if let Some(waker) = self.waker.lock().unwrap().take() {
                waker.wake();
            }
        }
    }

    struct FlushGateSink(Arc<FlushGateState>);

    impl Sink<Message> for FlushGateSink {
        type Error = ();

        fn poll_ready(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, item: Message) -> Result<(), Self::Error> {
            let label = match item {
                Message::Text(text) => text.to_string(),
                _ => "control".to_string(),
            };
            self.0.started.lock().unwrap().push(label.clone());
            self.0.pending.lock().unwrap().push(label);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            let mut permits = self.0.flush_permits.load(Ordering::SeqCst);
            while permits > 0 {
                match self.0.flush_permits.compare_exchange(
                    permits,
                    permits - 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => {
                        let pending = std::mem::take(&mut *self.0.pending.lock().unwrap());
                        self.0.flushed.lock().unwrap().extend(pending);
                        return Poll::Ready(Ok(()));
                    }
                    Err(actual) => permits = actual,
                }
            }
            *self.0.waker.lock().unwrap() = Some(context.waker().clone());
            Poll::Pending
        }

        fn poll_close(
            self: Pin<&mut Self>,
            context: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            self.poll_flush(context)
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
            max_frame_bytes: Arc::new(AtomicU64::new(524_288)),
            write_timeout,
        }
    }

    const TEST_CONNECTION_ID: &str = "67da7da9-a653-4d6e-9e81-f5f8baf874bb";
    const TEST_REQUEST_ID: &str = "2d0465b1-dc92-46d2-bc45-c90ed9724f5a";

    fn test_identity() -> ConnectionIdentity {
        ConnectionIdentity {
            connection_id: TEST_CONNECTION_ID.to_string(),
            agent_id: "dev-pc-001".to_string(),
            project_id: "myforge-local".to_string(),
        }
    }

    fn test_command(profile: &str) -> CommandExecute {
        let now_ms = SystemClock.now_ms();
        CommandExecute {
            protocol_version: 1,
            message_type: "command.execute".to_string(),
            connection_id: TEST_CONNECTION_ID.to_string(),
            request_id: TEST_REQUEST_ID.to_string(),
            task_type: "fangyuan.blueprint.generate".to_string(),
            agent_id: "dev-pc-001".to_string(),
            project_id: "myforge-local".to_string(),
            profile: profile.to_string(),
            input: CommandInput {
                artifact_file: "artifacts/fangyuan/result.ron".to_string(),
                consumer_target_file: None,
                rules_file: Some("rules/fangyuan/rules.md".to_string()),
                prompt: BlueprintPrompt {
                    theme: "test".to_string(),
                    primitive_limit: 10,
                    bounds: BlueprintBounds {
                        width: 10,
                        depth: 10,
                        height: 10,
                    },
                    requirements: vec!["safe".to_string()],
                },
                rendered_prompt: "fixed test prompt".to_string(),
            },
            timeout_ms: 5_000,
            max_output_bytes: 4_096,
            timestamp_ms: now_ms,
            expires_at_ms: now_ms + 5_000,
            nonce: "nonce".to_string(),
            signature: "signature".to_string(),
        }
    }

    fn completed_test_result() -> CommandResultSemantic {
        CommandResultSemantic {
            execution_mode: "codex_exec".to_string(),
            status: "completed".to_string(),
            exit_code: Some(0),
            stdout_preview: "done".to_string(),
            stderr_preview: String::new(),
            stdout_bytes: 4,
            stderr_bytes: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            artifact_file: "artifacts/fangyuan/result.ron".to_string(),
            consumer_target_file: None,
            artifact: ArtifactSummary {
                exists: true,
                sha256: Some("a".repeat(64)),
                bytes: Some(1),
                modified_at_ms: Some(100),
            },
            audit: AuditSummary::unavailable(),
            error_code: None,
            error_message: None,
            started_at_ms: Some(100),
            completed_at_ms: 100,
        }
    }

    async fn wait_for_gate_polls(gate: &GateState, minimum: usize) {
        for _ in 0..1_000 {
            if gate.polls.load(Ordering::SeqCst) >= minimum {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("writer did not reach gate poll {minimum}");
    }

    async fn wait_for_started_frames(gate: &FlushGateState, minimum: usize) {
        for _ in 0..1_000 {
            if gate.started.lock().unwrap().len() >= minimum {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("writer did not start frame {minimum}");
    }

    fn parse_written_frame(gate: &GateState, index: usize) -> JsonValue {
        let frame = gate.writes.lock().unwrap()[index].clone();
        parse_canonical_frame(frame.as_bytes(), 524_288).unwrap()
    }

    fn parse_flushed_frame(gate: &FlushGateState, index: usize) -> JsonValue {
        let frame = gate.flushed.lock().unwrap()[index].clone();
        parse_canonical_frame(frame.as_bytes(), 524_288).unwrap()
    }

    fn filler_item(label: &str, deadline: tokio::time::Instant) -> OutboundItem {
        let (completion, _receiver) = oneshot::channel();
        OutboundItem {
            payload: OutboundPayload::Message(Message::Text(label.to_string().into())),
            deadline,
            delivery_boundary: None,
            completion,
        }
    }

    fn complete_queued_text(item: OutboundItem) -> JsonValue {
        let OutboundItem {
            payload: OutboundPayload::Message(Message::Text(frame)),
            completion,
            ..
        } = item
        else {
            panic!("expected queued text frame");
        };
        let value = parse_canonical_frame(frame.as_bytes(), 524_288).unwrap();
        let _ = completion.send(Ok(DeliveryResult::Written));
        value
    }

    async fn register_start_candidate(
        registry: &RequestRegistry,
        command: &CommandExecute,
    ) -> StartedDeliveryCandidate {
        registry
            .begin(
                TEST_CONNECTION_ID,
                TEST_REQUEST_ID,
                "digest",
                cancelled_semantic_result(command, "codex_exec", None, 100),
                100,
            )
            .await
            .unwrap();
        registry
            .mark_started(TEST_REQUEST_ID, 100)
            .await
            .unwrap()
            .expect("uncancelled start must capture a delivery candidate")
    }

    async fn register_started_request(registry: &RequestRegistry, command: &CommandExecute) {
        let candidate = register_start_candidate(registry, command).await;
        assert!(candidate.lease.try_commit_started());
    }

    fn spawn_started_delivery(
        outbound: OutboundHandle,
        candidate: StartedDeliveryCandidate,
    ) -> JoinHandle<Result<DeliveryResult, ProtocolError>> {
        tokio::spawn(async move {
            send_started_owned(
                &test_identity(),
                test_effective_limits(),
                StartedDelivery {
                    request_id: TEST_REQUEST_ID,
                    execution_mode: "codex_exec",
                },
                candidate,
                &outbound,
                &SystemClock,
            )
            .await
        })
    }

    fn spawn_completed_result(
        outbound: OutboundHandle,
        registry: Arc<RequestRegistry>,
        command: CommandExecute,
    ) -> JoinHandle<Result<(), ProtocolError>> {
        tokio::spawn(async move {
            send_and_cache_result(
                &test_identity(),
                test_effective_limits(),
                TEST_REQUEST_ID,
                &command,
                "codex_exec",
                completed_test_result(),
                &outbound,
                &SystemClock,
                &registry,
            )
            .await
        })
    }

    #[tokio::test(start_paused = true)]
    async fn generation_cannot_supersede_a_frame_after_start_send() {
        let gate = Arc::new(FlushGateState::default());
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let writer = tokio::spawn(writer_task(
            FlushGateSink(gate.clone()),
            receiver,
            terminal_tx,
            shutdown.child_token(),
        ));
        let outbound = test_outbound(sender, Duration::from_secs(30));
        let registry = Arc::new(RequestRegistry::new(8));
        let command = test_command("codex_exec");
        registry
            .begin(
                TEST_CONNECTION_ID,
                TEST_REQUEST_ID,
                "digest",
                cancelled_semantic_result(&command, "codex_exec", None, 100),
                100,
            )
            .await
            .unwrap();
        let delivery = registry.delivery(TEST_REQUEST_ID).await.unwrap();
        let first_lease = delivery.lease();
        let first_reservation = outbound
            .reserve_delivery(SystemClock.now_ms() + 5_000, &first_lease)
            .await
            .unwrap()
            .unwrap();
        let first = first_reservation.commit(
            OutboundPayload::Message(Message::Text("first".into())),
            DeliveryBoundary::Terminal(first_lease),
        );
        wait_for_started_frames(&gate, 1).await;

        let delivery_guard = delivery.lock().await;
        assert!(matches!(
            registry
                .cancel(TEST_REQUEST_ID, SystemClock.now_ms() + 1_000)
                .await
                .unwrap(),
            CancelDecision::First
        ));
        drop(delivery_guard);
        let second_lease = delivery.lease();
        let second_reservation = outbound
            .reserve_delivery(SystemClock.now_ms() + 5_000, &second_lease)
            .await
            .unwrap()
            .unwrap();
        let second = second_reservation.commit(
            OutboundPayload::Message(Message::Text("second".into())),
            DeliveryBoundary::Terminal(second_lease),
        );
        let first_wait = tokio::spawn(first.wait());
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(!first_wait.is_finished());
        assert_eq!(gate.started.lock().unwrap().as_slice(), ["first"]);
        assert!(gate.flushed.lock().unwrap().is_empty());

        gate.grant_flush();
        assert_eq!(first_wait.await.unwrap().unwrap(), DeliveryResult::Written);
        assert_eq!(gate.flushed.lock().unwrap().as_slice(), ["first"]);
        assert_eq!(
            gate.started
                .lock()
                .unwrap()
                .iter()
                .filter(|frame| frame.as_str() == "first")
                .count(),
            1
        );

        wait_for_started_frames(&gate, 2).await;
        gate.grant_flush();
        assert_eq!(second.wait().await.unwrap(), DeliveryResult::Written);
        assert_eq!(gate.flushed.lock().unwrap().as_slice(), ["first", "second"]);
        assert!(terminal_rx.try_recv().is_err());
        shutdown.cancel();
        await_task(writer).await;
    }

    #[tokio::test]
    async fn terminal_waits_for_queue_capacity_within_the_shared_deadline() {
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .try_send(filler_item(
                "occupied",
                tokio::time::Instant::now() + Duration::from_secs(30),
            ))
            .unwrap();
        let outbound = test_outbound(sender, Duration::from_secs(30));
        let registry = Arc::new(RequestRegistry::new(8));
        let command = test_command("codex_exec");
        register_started_request(&registry, &command).await;
        let send_outbound = outbound.clone();
        let send_registry = registry.clone();
        let send_command = command.clone();
        let send = tokio::spawn(async move {
            send_and_cache_result(
                &test_identity(),
                test_effective_limits(),
                TEST_REQUEST_ID,
                &send_command,
                "codex_exec",
                completed_test_result(),
                &send_outbound,
                &SystemClock,
                &send_registry,
            )
            .await
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(!send.is_finished());

        let occupied = receiver.recv().await.unwrap();
        drop(occupied);
        let terminal = receiver.recv().await.unwrap();
        let value = complete_queued_text(terminal);
        assert_eq!(value.string_field("type"), Some("command.result"));
        assert_eq!(value.string_field("status"), Some("completed"));
        send.await.unwrap().unwrap();
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_queue_wait_timeout_never_late_enqueues() {
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .try_send(filler_item(
                "occupied",
                tokio::time::Instant::now() + Duration::from_secs(30),
            ))
            .unwrap();
        let outbound = test_outbound(sender, Duration::from_millis(100));
        let registry = Arc::new(RequestRegistry::new(8));
        let command = test_command("codex_exec");
        register_started_request(&registry, &command).await;
        let send_outbound = outbound.clone();
        let send_registry = registry.clone();
        let send_command = command.clone();
        let send = tokio::spawn(async move {
            send_and_cache_result(
                &test_identity(),
                test_effective_limits(),
                TEST_REQUEST_ID,
                &send_command,
                "codex_exec",
                completed_test_result(),
                &send_outbound,
                &SystemClock,
                &send_registry,
            )
            .await
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(101)).await;
        assert_eq!(
            send.await.unwrap().unwrap_err().code(),
            "MYFORGE_AGENT_DISCONNECTED"
        );

        drop(receiver.recv().await.unwrap());
        tokio::task::yield_now().await;
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn cancel_wins_while_terminal_waits_for_queue_capacity() {
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .try_send(filler_item(
                "occupied",
                tokio::time::Instant::now() + Duration::from_secs(30),
            ))
            .unwrap();
        let outbound = test_outbound(sender, Duration::from_secs(30));
        let registry = Arc::new(RequestRegistry::new(8));
        let command = test_command("codex_exec");
        registry
            .begin(
                TEST_CONNECTION_ID,
                TEST_REQUEST_ID,
                "digest",
                cancelled_semantic_result(&command, "codex_exec", None, 100),
                100,
            )
            .await
            .unwrap();
        let send_outbound = outbound.clone();
        let send_registry = registry.clone();
        let send_command = command.clone();
        let send = tokio::spawn(async move {
            send_and_cache_result(
                &test_identity(),
                test_effective_limits(),
                TEST_REQUEST_ID,
                &send_command,
                "codex_exec",
                completed_test_result(),
                &send_outbound,
                &SystemClock,
                &send_registry,
            )
            .await
        });
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        assert!(!send.is_finished());
        cancel_request_owned(
            &test_identity(),
            test_effective_limits(),
            TEST_REQUEST_ID,
            SystemClock.now_ms() + 1_000,
            &outbound,
            &SystemClock,
            &registry,
        )
        .await
        .unwrap();

        drop(receiver.recv().await.unwrap());
        let terminal = receiver.recv().await.unwrap();
        let value = complete_queued_text(terminal);
        assert_eq!(value.string_field("type"), Some("command.result"));
        assert_eq!(value.string_field("status"), Some("cancelled"));
        assert_eq!(value.object_field("startedAtMs"), Some(&JsonValue::Null));
        send.await.unwrap().unwrap();
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn initial_cancel_before_candidate_capture_suppresses_started_and_uses_null_start() {
        let (sender, mut receiver) = mpsc::channel(QUEUE_CAPACITY);
        let outbound = test_outbound(sender, Duration::from_secs(30));
        let registry = Arc::new(RequestRegistry::new(8));
        let command = test_command("codex_exec");
        registry
            .begin(
                TEST_CONNECTION_ID,
                TEST_REQUEST_ID,
                "digest",
                cancelled_semantic_result(&command, "codex_exec", None, 100),
                100,
            )
            .await
            .unwrap();
        let cancel_deadline_at_ms = SystemClock.now_ms() + 1_000;
        assert_eq!(
            registry
                .cancel(TEST_REQUEST_ID, cancel_deadline_at_ms)
                .await
                .unwrap(),
            CancelDecision::First
        );

        assert!(
            registry
                .mark_started(TEST_REQUEST_ID, 100)
                .await
                .unwrap()
                .is_none(),
            "cancelled request must not mint a started delivery candidate"
        );
        assert_eq!(
            registry
                .committed_started_at_ms(TEST_REQUEST_ID)
                .await
                .unwrap(),
            None
        );
        assert!(matches!(
            registry
                .begin(
                    TEST_CONNECTION_ID,
                    TEST_REQUEST_ID,
                    "digest",
                    cancelled_semantic_result(&command, "codex_exec", None, 100),
                    101,
                )
                .await
                .unwrap(),
            ExecuteDecision::DuplicateActive { started: None, .. }
        ));
        assert!(receiver.try_recv().is_err());

        let result = spawn_completed_result(outbound, registry, command);
        let terminal = receiver.recv().await.unwrap();
        let value = complete_queued_text(terminal);
        result.await.unwrap().unwrap();
        assert_eq!(value.string_field("type"), Some("command.result"));
        assert_eq!(value.string_field("status"), Some("cancelled"));
        assert_eq!(value.object_field("startedAtMs"), Some(&JsonValue::Null));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn duplicate_cancel_before_worker_invalidates_the_captured_started_candidate() {
        let (sender, mut receiver) = mpsc::channel(QUEUE_CAPACITY);
        let outbound = test_outbound(sender, Duration::from_secs(30));
        let registry = Arc::new(RequestRegistry::new(8));
        let command = test_command("codex_exec");
        register_started_request(&registry, &command).await;
        let replay_candidate = match registry
            .begin(
                TEST_CONNECTION_ID,
                TEST_REQUEST_ID,
                "digest",
                cancelled_semantic_result(&command, "codex_exec", None, 100),
                101,
            )
            .await
            .unwrap()
        {
            ExecuteDecision::DuplicateActive {
                started: Some(candidate),
                ..
            } if candidate.started_at_ms == 100 => candidate,
            other => panic!("expected replayable started candidate, got {other:?}"),
        };

        let cancel_deadline_at_ms = SystemClock.now_ms() + 1_000;
        assert_eq!(
            registry
                .cancel(TEST_REQUEST_ID, cancel_deadline_at_ms)
                .await
                .unwrap(),
            CancelDecision::First
        );
        let replay = spawn_started_delivery(outbound.clone(), replay_candidate);
        assert_eq!(replay.await.unwrap().unwrap(), DeliveryResult::Superseded);
        assert!(receiver.try_recv().is_err());
        assert!(matches!(
            registry
                .begin(
                    TEST_CONNECTION_ID,
                    TEST_REQUEST_ID,
                    "digest",
                    cancelled_semantic_result(&command, "codex_exec", None, 100),
                    102,
                )
                .await
                .unwrap(),
            ExecuteDecision::DuplicateActive { started: None, .. }
        ));

        let result = spawn_completed_result(outbound, registry, command);
        let terminal = receiver.recv().await.unwrap();
        let value = complete_queued_text(terminal);
        result.await.unwrap().unwrap();
        assert_eq!(value.string_field("type"), Some("command.result"));
        assert_eq!(value.string_field("status"), Some("cancelled"));
        assert_eq!(
            value.object_field("startedAtMs"),
            Some(&JsonValue::Integer(100))
        );
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn initial_started_is_suppressed_when_cancel_wins_before_start_send() {
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
        let outbound = test_outbound(sender, Duration::from_secs(30));
        let registry = Arc::new(RequestRegistry::new(8));
        let command = test_command("codex_exec");
        let candidate = register_start_candidate(&registry, &command).await;

        let started = spawn_started_delivery(outbound.clone(), candidate);
        wait_for_gate_polls(&gate, 1).await;
        cancel_request_owned(
            &test_identity(),
            test_effective_limits(),
            TEST_REQUEST_ID,
            SystemClock.now_ms() + 1_000,
            &outbound,
            &SystemClock,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(started.await.unwrap().unwrap(), DeliveryResult::Superseded);
        assert_eq!(
            registry
                .committed_started_at_ms(TEST_REQUEST_ID)
                .await
                .unwrap(),
            None
        );
        assert!(matches!(
            registry
                .begin(
                    TEST_CONNECTION_ID,
                    TEST_REQUEST_ID,
                    "digest",
                    cancelled_semantic_result(&command, "codex_exec", None, 100),
                    101,
                )
                .await
                .unwrap(),
            ExecuteDecision::DuplicateActive { started: None, .. }
        ));
        assert!(gate.writes.lock().unwrap().is_empty());

        let next_poll = gate.polls.load(Ordering::SeqCst) + 1;
        let result = spawn_completed_result(outbound.clone(), registry.clone(), command);
        wait_for_gate_polls(&gate, next_poll).await;
        gate.grant();
        result.await.unwrap().unwrap();
        let written = parse_written_frame(&gate, 0);
        assert_eq!(written.string_field("type"), Some("command.result"));
        assert_eq!(written.string_field("status"), Some("cancelled"));
        assert_eq!(written.object_field("startedAtMs"), Some(&JsonValue::Null));
        assert!(terminal_rx.try_recv().is_err());
        shutdown.cancel();
        await_task(writer).await;
    }

    #[tokio::test(start_paused = true)]
    async fn initial_started_finishes_before_cancelled_result_after_start_send() {
        let gate = Arc::new(FlushGateState::default());
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let writer = tokio::spawn(writer_task(
            FlushGateSink(gate.clone()),
            receiver,
            terminal_tx,
            shutdown.child_token(),
        ));
        let outbound = test_outbound(sender, Duration::from_secs(30));
        let registry = Arc::new(RequestRegistry::new(8));
        let command = test_command("codex_exec");
        let candidate = register_start_candidate(&registry, &command).await;

        let started = spawn_started_delivery(outbound.clone(), candidate);
        wait_for_started_frames(&gate, 1).await;
        assert_eq!(
            registry
                .committed_started_at_ms(TEST_REQUEST_ID)
                .await
                .unwrap(),
            Some(100)
        );
        cancel_request_owned(
            &test_identity(),
            test_effective_limits(),
            TEST_REQUEST_ID,
            SystemClock.now_ms() + 1_000,
            &outbound,
            &SystemClock,
            &registry,
        )
        .await
        .unwrap();
        assert!(!started.is_finished());
        gate.grant_flush();
        assert_eq!(started.await.unwrap().unwrap(), DeliveryResult::Written);

        let result = spawn_completed_result(outbound.clone(), registry.clone(), command);
        wait_for_started_frames(&gate, 2).await;
        gate.grant_flush();
        result.await.unwrap().unwrap();
        let first = parse_flushed_frame(&gate, 0);
        let second = parse_flushed_frame(&gate, 1);
        assert_eq!(first.string_field("type"), Some("command.started"));
        assert_eq!(second.string_field("type"), Some("command.result"));
        assert_eq!(second.string_field("status"), Some("cancelled"));
        assert_eq!(
            second.object_field("startedAtMs"),
            Some(&JsonValue::Integer(100))
        );
        assert!(terminal_rx.try_recv().is_err());
        shutdown.cancel();
        await_task(writer).await;
    }

    #[tokio::test(start_paused = true)]
    async fn duplicate_started_is_suppressed_when_cancel_wins_before_start_send() {
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
        let outbound = test_outbound(sender, Duration::from_secs(30));
        let registry = Arc::new(RequestRegistry::new(8));
        let command = test_command("codex_exec");
        register_started_request(&registry, &command).await;
        let replay_candidate = match registry
            .begin(
                TEST_CONNECTION_ID,
                TEST_REQUEST_ID,
                "digest",
                cancelled_semantic_result(&command, "codex_exec", None, 100),
                101,
            )
            .await
            .unwrap()
        {
            ExecuteDecision::DuplicateActive {
                started: Some(candidate),
                ..
            } if candidate.started_at_ms == 100 => candidate,
            other => panic!("expected replayable started candidate, got {other:?}"),
        };

        let replay = spawn_started_delivery(outbound.clone(), replay_candidate);
        wait_for_gate_polls(&gate, 1).await;
        cancel_request_owned(
            &test_identity(),
            test_effective_limits(),
            TEST_REQUEST_ID,
            SystemClock.now_ms() + 1_000,
            &outbound,
            &SystemClock,
            &registry,
        )
        .await
        .unwrap();
        assert_eq!(replay.await.unwrap().unwrap(), DeliveryResult::Superseded);

        let next_poll = gate.polls.load(Ordering::SeqCst) + 1;
        let result = spawn_completed_result(outbound.clone(), registry.clone(), command);
        wait_for_gate_polls(&gate, next_poll).await;
        gate.grant();
        result.await.unwrap().unwrap();
        let written = parse_written_frame(&gate, 0);
        assert_eq!(written.string_field("type"), Some("command.result"));
        assert_eq!(written.string_field("status"), Some("cancelled"));
        assert_eq!(
            written.object_field("startedAtMs"),
            Some(&JsonValue::Integer(100))
        );
        gate.grant();
        tokio::task::yield_now().await;
        assert_eq!(gate.writes.lock().unwrap().len(), 1);
        assert!(terminal_rx.try_recv().is_err());
        shutdown.cancel();
        await_task(writer).await;
    }

    #[tokio::test(start_paused = true)]
    async fn duplicate_started_finishes_before_cancelled_result_after_start_send() {
        let gate = Arc::new(FlushGateState::default());
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let writer = tokio::spawn(writer_task(
            FlushGateSink(gate.clone()),
            receiver,
            terminal_tx,
            shutdown.child_token(),
        ));
        let outbound = test_outbound(sender, Duration::from_secs(30));
        let registry = Arc::new(RequestRegistry::new(8));
        let command = test_command("codex_exec");
        register_started_request(&registry, &command).await;
        let replay_candidate = match registry
            .begin(
                TEST_CONNECTION_ID,
                TEST_REQUEST_ID,
                "digest",
                cancelled_semantic_result(&command, "codex_exec", None, 100),
                101,
            )
            .await
            .unwrap()
        {
            ExecuteDecision::DuplicateActive {
                started: Some(candidate),
                ..
            } if candidate.started_at_ms == 100 => candidate,
            other => panic!("expected replayable started candidate, got {other:?}"),
        };

        let replay = spawn_started_delivery(outbound.clone(), replay_candidate);
        wait_for_started_frames(&gate, 1).await;
        cancel_request_owned(
            &test_identity(),
            test_effective_limits(),
            TEST_REQUEST_ID,
            SystemClock.now_ms() + 1_000,
            &outbound,
            &SystemClock,
            &registry,
        )
        .await
        .unwrap();
        assert!(!replay.is_finished());
        gate.grant_flush();
        assert_eq!(replay.await.unwrap().unwrap(), DeliveryResult::Written);

        let result = spawn_completed_result(outbound.clone(), registry.clone(), command);
        wait_for_started_frames(&gate, 2).await;
        gate.grant_flush();
        result.await.unwrap().unwrap();
        let first = parse_flushed_frame(&gate, 0);
        let second = parse_flushed_frame(&gate, 1);
        assert_eq!(first.string_field("type"), Some("command.started"));
        assert_eq!(second.string_field("type"), Some("command.result"));
        assert_eq!(second.string_field("status"), Some("cancelled"));
        assert_eq!(
            second.object_field("startedAtMs"),
            Some(&JsonValue::Integer(100))
        );
        assert!(terminal_rx.try_recv().is_err());
        shutdown.cancel();
        await_task(writer).await;
    }

    #[tokio::test]
    async fn cancellation_supersedes_an_inflight_natural_result_before_any_write() {
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
        let outbound = test_outbound(sender, Duration::from_secs(30));
        let registry = Arc::new(RequestRegistry::new(8));
        let command = test_command("codex_exec");
        let decision = registry
            .begin(
                TEST_CONNECTION_ID,
                TEST_REQUEST_ID,
                "digest",
                cancelled_semantic_result(&command, "codex_exec", None, 100),
                100,
            )
            .await
            .unwrap();
        assert!(matches!(decision, ExecuteDecision::New { .. }));
        assert!(
            registry
                .mark_started(TEST_REQUEST_ID, 100)
                .await
                .unwrap()
                .is_some()
        );

        let result_outbound = outbound.clone();
        let result_registry = registry.clone();
        let result_command = command.clone();
        let result_send = tokio::spawn(async move {
            send_and_cache_result(
                &test_identity(),
                test_effective_limits(),
                TEST_REQUEST_ID,
                &result_command,
                "codex_exec",
                completed_test_result(),
                &result_outbound,
                &SystemClock,
                &result_registry,
            )
            .await
        });
        wait_for_gate_polls(&gate, 1).await;
        assert!(gate.writes.lock().unwrap().is_empty());

        let cancel_deadline_at_ms = SystemClock.now_ms() + 1_000;
        let cancel_outbound = outbound.clone();
        let cancel_registry = registry.clone();
        let cancel = tokio::spawn(async move {
            cancel_request_owned(
                &test_identity(),
                test_effective_limits(),
                TEST_REQUEST_ID,
                cancel_deadline_at_ms,
                &cancel_outbound,
                &SystemClock,
                &cancel_registry,
            )
            .await
        });
        result_send.await.unwrap().unwrap();
        wait_for_gate_polls(&gate, 2).await;
        assert!(gate.writes.lock().unwrap().is_empty());

        gate.grant();
        cancel.await.unwrap().unwrap();
        assert_eq!(gate.writes.lock().unwrap().len(), 1);
        let first = parse_written_frame(&gate, 0);
        assert_eq!(first.string_field("type"), Some("command.result"));
        assert_eq!(first.string_field("status"), Some("cancelled"));

        let replay_polls = gate.polls.load(Ordering::SeqCst) + 1;
        let replay_outbound = outbound.clone();
        let replay_registry = registry.clone();
        let replay = tokio::spawn(async move {
            cancel_request_owned(
                &test_identity(),
                test_effective_limits(),
                TEST_REQUEST_ID,
                cancel_deadline_at_ms,
                &replay_outbound,
                &SystemClock,
                &replay_registry,
            )
            .await
        });
        wait_for_gate_polls(&gate, replay_polls).await;
        gate.grant();
        replay.await.unwrap().unwrap();
        let replayed = parse_written_frame(&gate, 1);
        assert_eq!(
            semantic_digest(&replayed).unwrap(),
            semantic_digest(&first).unwrap()
        );

        assert_eq!(
            cancel_request_owned(
                &test_identity(),
                test_effective_limits(),
                TEST_REQUEST_ID,
                cancel_deadline_at_ms + 1,
                &outbound,
                &SystemClock,
                &registry,
            )
            .await
            .unwrap_err()
            .code(),
            "MYFORGE_DUPLICATE_REQUEST_CONFLICT"
        );
        assert!(terminal_rx.try_recv().is_err());
        shutdown.cancel();
        await_task(writer).await;
    }

    #[derive(Default)]
    struct BusinessValidationHandler {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl CommandHandler for BusinessValidationHandler {
        async fn execute(
            &self,
            _command: CommandExecute,
            _control: CommandControl,
        ) -> CommandHandlerOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            CommandHandlerOutcome::PreStartError(CommandRejection::new(
                "MYFORGE_CODEX_UNAVAILABLE",
                "unexpected handler call",
                false,
            ))
        }
    }

    #[tokio::test]
    async fn dispatcher_processes_cancel_while_business_error_delivery_is_stalled() {
        let handler = Arc::new(BusinessValidationHandler::default());
        let runtime = ClientRuntime::new(handler.clone());
        let gate = Arc::new(GateState::default());
        let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
        let (terminal_tx, mut terminal_rx) = mpsc::unbounded_channel();
        let shutdown = CancellationToken::new();
        let writer = tokio::spawn(writer_task(
            GateSink(gate.clone()),
            receiver,
            terminal_tx.clone(),
            shutdown.child_token(),
        ));
        let outbound = test_outbound(sender, Duration::from_secs(30));
        let command = test_command("unknown_profile");
        let mut workers = JoinSet::new();

        runtime
            .handle_execute(
                test_identity(),
                "codex_exec",
                test_effective_limits(),
                JsonValue::Object(vec![(
                    "requestId".to_string(),
                    JsonValue::String(TEST_REQUEST_ID.to_string()),
                )]),
                command,
                &outbound,
                &terminal_tx,
                &mut workers,
            )
            .await
            .unwrap();
        wait_for_gate_polls(&gate, 1).await;
        assert_eq!(handler.calls.load(Ordering::SeqCst), 0);
        assert!(gate.writes.lock().unwrap().is_empty());

        let cancel_outbound = outbound.clone();
        let cancel_registry = runtime.request_registry.clone();
        let cancel_deadline_at_ms = SystemClock.now_ms() + 1_000;
        let cancel = tokio::spawn(async move {
            cancel_request_owned(
                &test_identity(),
                test_effective_limits(),
                TEST_REQUEST_ID,
                cancel_deadline_at_ms,
                &cancel_outbound,
                &SystemClock,
                &cancel_registry,
            )
            .await
        });
        wait_for_gate_polls(&gate, 2).await;
        assert!(gate.writes.lock().unwrap().is_empty());

        gate.grant();
        cancel.await.unwrap().unwrap();
        assert_eq!(gate.writes.lock().unwrap().len(), 1);
        let written = parse_written_frame(&gate, 0);
        assert_eq!(written.string_field("type"), Some("command.result"));
        assert_eq!(written.string_field("status"), Some("cancelled"));
        assert!(terminal_rx.try_recv().is_err());

        shutdown.cancel();
        drain_workers(&mut workers).await;
        await_task(writer).await;
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_result_never_writes_after_the_original_cancel_deadline() {
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
        let outbound = OutboundHandle {
            sender,
            signing_key: Arc::new(SigningKey::from_bytes(&[7; 32])),
            clock: Arc::new(SystemClock),
            max_frame_bytes: Arc::new(AtomicU64::new(524_288)),
            write_timeout: Duration::from_secs(30),
        };
        let now_ms = SystemClock.now_ms();
        let cancel_deadline_at_ms = now_ms + 100;
        let send_outbound = outbound.clone();
        let send = tokio::spawn(async move {
            send_command_result_owned(
                &ConnectionIdentity {
                    connection_id: "67da7da9-a653-4d6e-9e81-f5f8baf874bb".to_string(),
                    agent_id: "dev-pc-001".to_string(),
                    project_id: "myforge-local".to_string(),
                },
                test_effective_limits(),
                "2d0465b1-dc92-46d2-bc45-c90ed9724f5a",
                CommandResultSemantic {
                    completed_at_ms: now_ms,
                    ..cancelled_test_result()
                },
                Some(cancel_deadline_at_ms),
                &send_outbound,
                &SystemClock,
            )
            .await
        });

        while gate.polls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(101)).await;
        assert_eq!(
            send.await.unwrap().unwrap_err().code(),
            "MYFORGE_AGENT_DISCONNECTED"
        );
        assert_eq!(terminal_rx.recv().await.unwrap().reason, "writer_deadline");
        gate.grant();
        tokio::task::yield_now().await;
        assert!(gate.writes.lock().unwrap().is_empty());

        shutdown.cancel();
        await_task(writer).await;
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

    fn cancelled_test_result() -> CommandResultSemantic {
        CommandResultSemantic {
            execution_mode: "codex_exec".to_string(),
            status: "cancelled".to_string(),
            exit_code: None,
            stdout_preview: String::new(),
            stderr_preview: String::new(),
            stdout_bytes: 0,
            stderr_bytes: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            artifact_file: "artifacts/fangyuan/result.ron".to_string(),
            consumer_target_file: None,
            artifact: ArtifactSummary::missing(),
            audit: AuditSummary::skipped("cancelled"),
            error_code: Some("MYFORGE_COMMAND_CANCELLED".to_string()),
            error_message: Some("command was cancelled".to_string()),
            started_at_ms: None,
            completed_at_ms: 100,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn termination_cancels_active_request_before_waiting_on_stalled_writer() {
        let registry = Arc::new(RequestRegistry::new(8));
        let decision = registry
            .begin(
                "connection",
                "request",
                "digest",
                cancelled_test_result(),
                100,
            )
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
                .begin(
                    "connection",
                    "request",
                    "digest",
                    cancelled_test_result(),
                    101,
                )
                .await
                .unwrap(),
            ExecuteDecision::DuplicateCompleted {
                response: CachedResponse::NoReplay,
                cancel_deadline_at_ms: None,
            }
        ));
    }
}
