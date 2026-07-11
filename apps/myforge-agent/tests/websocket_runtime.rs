use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
use ed25519_dalek::{SigningKey, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use myforge_agent::AgentError;
use myforge_agent::command::{CommandControl, CommandHandler, CommandHandlerOutcome};
use myforge_agent::config::{AgentConfig, Environment};
use myforge_agent::preflight::{CapabilityProbe, PreflightReport, run_preflight};
use myforge_agent::protocol::{
    JsonValue, SUBPROTOCOL, parse_canonical_frame, random_base64url, sign_message,
    verify_message_signature,
};
use myforge_agent::runtime::{BackoffJitter, ClientRuntime, RuntimeHooks, Sleeper, SystemClock};
use myforge_agent::schemas::{
    CommandExecute, CommandRejection, ServerMessage, parse_server_message, validate_message_time,
};
use pkcs8::LineEnding;
use serde_json::json;
use tempfile::TempDir;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const AGENT_ID: &str = "dev-pc-001";
const PROJECT_ID: &str = "myforge-local";
const AGENT_AUTH_TTL_MS: u64 = 10_000;
const AUTH_TTL_MS: u64 = 5_000;
const COMMAND_TTL_MS: u64 = 5_000;
const CLOCK_SKEW_MS: u64 = 1_000;
const HEARTBEAT_INTERVAL_MS: u64 = 1_000;
const COMMAND_TIMEOUT_MS: u64 = 5_000;
const CANCEL_TIMEOUT_MS: u64 = 1_000;
const MAX_OUTPUT_BYTES: u64 = 4_096;
const WS_MAX_MESSAGE_BYTES: u64 = 524_288;

struct MapEnvironment(HashMap<String, String>);

impl Environment for MapEnvironment {
    fn get(&self, name: &str) -> Result<Option<String>, AgentError> {
        Ok(self.0.get(name).cloned())
    }
}

struct FakeProbe;

impl CapabilityProbe for FakeProbe {
    fn hostname(&self) -> Result<String, AgentError> {
        Ok("safe-test-host".to_string())
    }

    fn codex_available(&self, _executable: &OsStr, _working_directory: &std::path::Path) -> bool {
        true
    }
}

struct Fixture {
    _directory: TempDir,
    config: AgentConfig,
    preflight: PreflightReport,
    agent_key: SigningKey,
    server_key: SigningKey,
}

impl Fixture {
    fn new(endpoint: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("external-myforge");
        fs::create_dir(&root).unwrap();
        let agent_key = SigningKey::from_bytes(&[41; 32]);
        let server_key = SigningKey::from_bytes(&[42; 32]);
        let agent_private = directory.path().join("agent-private.pem");
        let agent_public = directory.path().join("agent-public.pem");
        let server_public = directory.path().join("server-public.pem");
        fs::write(
            &agent_private,
            agent_key.to_pkcs8_pem(LineEnding::LF).unwrap().as_bytes(),
        )
        .unwrap();
        fs::write(
            &agent_public,
            agent_key
                .verifying_key()
                .to_public_key_pem(LineEnding::LF)
                .unwrap(),
        )
        .unwrap();
        fs::write(
            &server_public,
            server_key
                .verifying_key()
                .to_public_key_pem(LineEnding::LF)
                .unwrap(),
        )
        .unwrap();
        let environment = MapEnvironment(HashMap::from([
            ("ADMIN_API_WS_URL".to_string(), endpoint.to_string()),
            ("MYFORGE_AGENT_ID".to_string(), AGENT_ID.to_string()),
            ("MYFORGE_PROJECT_ID".to_string(), PROJECT_ID.to_string()),
            (
                "MYFORGE_AGENT_PRIVATE_KEY_PATH".to_string(),
                agent_private.to_string_lossy().into_owned(),
            ),
            (
                "MYFORGE_AGENT_PUBLIC_KEY_PATH".to_string(),
                agent_public.to_string_lossy().into_owned(),
            ),
            (
                "MYFORGE_SERVER_PUBLIC_KEY_PATH".to_string(),
                server_public.to_string_lossy().into_owned(),
            ),
            (
                "MYFORGE_ROOT".to_string(),
                root.to_string_lossy().into_owned(),
            ),
            (
                "MYFORGE_AUTH_TTL_MS".to_string(),
                AGENT_AUTH_TTL_MS.to_string(),
            ),
            (
                "MYFORGE_COMMAND_TTL_MS".to_string(),
                COMMAND_TTL_MS.to_string(),
            ),
            (
                "MYFORGE_CLOCK_SKEW_MS".to_string(),
                CLOCK_SKEW_MS.to_string(),
            ),
            (
                "MYFORGE_HEARTBEAT_INTERVAL_MS".to_string(),
                HEARTBEAT_INTERVAL_MS.to_string(),
            ),
            (
                "MYFORGE_MAX_COMMAND_TIMEOUT_MS".to_string(),
                COMMAND_TIMEOUT_MS.to_string(),
            ),
            (
                "MYFORGE_CANCEL_TIMEOUT_MS".to_string(),
                CANCEL_TIMEOUT_MS.to_string(),
            ),
            (
                "MYFORGE_MAX_OUTPUT_BYTES".to_string(),
                MAX_OUTPUT_BYTES.to_string(),
            ),
            (
                "MYFORGE_WS_MAX_MESSAGE_BYTES".to_string(),
                WS_MAX_MESSAGE_BYTES.to_string(),
            ),
            (
                "MYFORGE_WS_WRITE_TIMEOUT_MS".to_string(),
                "1000".to_string(),
            ),
            ("LOG_ENABLE_FILE".to_string(), "false".to_string()),
        ]));
        let config = AgentConfig::from_environment(&environment).unwrap();
        let preflight = run_preflight(&config, &FakeProbe).unwrap();
        Self {
            _directory: directory,
            config,
            preflight,
            agent_key,
            server_key,
        }
    }
}

#[derive(Default)]
struct CountingHandler {
    calls: AtomicUsize,
}

#[async_trait]
impl CommandHandler for CountingHandler {
    async fn execute(
        &self,
        _command: CommandExecute,
        _control: CommandControl,
    ) -> CommandHandlerOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        CommandHandlerOutcome::PreStartError(CommandRejection::new(
            "MYFORGE_CODEX_UNAVAILABLE",
            "test execution is unavailable",
            false,
        ))
    }
}

struct CancelAwareHandler {
    calls: AtomicUsize,
    cancelled: Notify,
}

impl CancelAwareHandler {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            cancelled: Notify::new(),
        }
    }
}

#[async_trait]
impl CommandHandler for CancelAwareHandler {
    async fn execute(
        &self,
        _command: CommandExecute,
        control: CommandControl,
    ) -> CommandHandlerOutcome {
        self.calls.fetch_add(1, Ordering::SeqCst);
        control.cancellation().cancelled().await;
        self.cancelled.notify_one();
        CommandHandlerOutcome::CancelledBeforeStart
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

async fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("ws://{}/api/v1/myforge/ws", listener.local_addr().unwrap());
    (listener, endpoint)
}

#[allow(clippy::result_large_err)]
async fn accept_agent(listener: &TcpListener) -> WebSocketStream<TcpStream> {
    let (stream, _) = listener.accept().await.unwrap();
    tokio_tungstenite::accept_hdr_async(stream, |request: &Request, mut response: Response| {
        let uri = request.uri().to_string();
        assert!(uri.starts_with("/api/v1/myforge/ws?"));
        assert!(uri.contains("agentId=dev-pc-001"));
        assert!(uri.contains("projectId=myforge-local"));
        assert_eq!(
            request
                .headers()
                .get("sec-websocket-protocol")
                .and_then(|value| value.to_str().ok()),
            Some(SUBPROTOCOL)
        );
        response.headers_mut().insert(
            "sec-websocket-protocol",
            HeaderValue::from_static(SUBPROTOCOL),
        );
        Ok(response)
    })
    .await
    .unwrap()
}

async fn send_signed(
    socket: &mut WebSocketStream<TcpStream>,
    message: serde_json::Value,
    key: &SigningKey,
) {
    let frame = sign_message(&message, key).unwrap();
    socket.send(Message::Text(frame.into())).await.unwrap();
}

async fn read_agent(socket: &mut WebSocketStream<TcpStream>, key: &VerifyingKey) -> JsonValue {
    loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Text(text) => {
                let value =
                    parse_canonical_frame(text.as_bytes(), WS_MAX_MESSAGE_BYTES as usize).unwrap();
                verify_message_signature(&value, key).unwrap();
                return value;
            }
            Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
            Message::Pong(_) => {}
            other => panic!("expected signed agent text frame, got {other:?}"),
        }
    }
}

async fn handshake(
    socket: &mut WebSocketStream<TcpStream>,
    server_key: &SigningKey,
    agent_key: &VerifyingKey,
) -> String {
    let connection_id = Uuid::new_v4().to_string();
    let timestamp_ms = now_ms();
    send_signed(
        socket,
        challenge_message(&connection_id, timestamp_ms),
        server_key,
    )
    .await;
    let hello = read_agent(socket, agent_key).await;
    assert_eq!(hello.string_field("type"), Some("agent.hello"));
    assert_eq!(
        hello.string_field("challengeId"),
        Some(connection_id.as_str())
    );
    let register = read_agent(socket, agent_key).await;
    assert_eq!(register.string_field("type"), Some("agent.register"));
    assert_eq!(
        register.string_field("connectionId"),
        Some(connection_id.as_str())
    );
    assert_eq!(
        register
            .object_field("limits")
            .and_then(|limits| limits.object_field("maxCommandTimeoutMs")),
        Some(&JsonValue::Integer(COMMAND_TIMEOUT_MS as i64))
    );
    connection_id
}

fn challenge_message(connection_id: &str, timestamp_ms: u64) -> serde_json::Value {
    json!({
        "protocolVersion": 1,
        "type": "server.challenge",
        "challengeId": connection_id,
        "challenge": random_base64url::<32>(),
        "agentId": AGENT_ID,
        "projectId": PROJECT_ID,
        "limits": {
            "authTtlMs": AUTH_TTL_MS,
            "commandTtlMs": COMMAND_TTL_MS,
            "clockSkewMs": CLOCK_SKEW_MS,
            "heartbeatIntervalMs": HEARTBEAT_INTERVAL_MS,
            "heartbeatTimeoutMs": 5_000,
            "commandTimeoutMs": COMMAND_TIMEOUT_MS,
            "cancelTimeoutMs": CANCEL_TIMEOUT_MS,
            "maxOutputBytes": MAX_OUTPUT_BYTES,
            "wsMaxMessageBytes": WS_MAX_MESSAGE_BYTES
        },
        "timestampMs": timestamp_ms,
        "expiresAtMs": timestamp_ms + AUTH_TTL_MS,
        "nonce": random_base64url::<16>()
    })
}

fn assert_valid_challenge_protocol_error(value: &JsonValue, connection_id: &str, error_code: &str) {
    let ServerMessage::ProtocolError(message) = parse_server_message(value).unwrap() else {
        panic!("expected protocol.error");
    };
    assert_eq!(message.connection_id.as_deref(), Some(connection_id));
    assert_eq!(message.request_id, None);
    assert_eq!(message.agent_id, AGENT_ID);
    assert_eq!(message.project_id, PROJECT_ID);
    assert_eq!(message.error_code, error_code);
    assert!(message.fatal);
    assert_eq!(message.expires_at_ms - message.timestamp_ms, AUTH_TTL_MS);
    validate_message_time(
        message.timestamp_ms,
        message.expires_at_ms,
        now_ms(),
        CLOCK_SKEW_MS,
        AUTH_TTL_MS,
        Some(AUTH_TTL_MS),
    )
    .unwrap();
}

fn execute_message(
    connection_id: &str,
    request_id: &str,
    profile: &str,
    timestamp_ms: u64,
) -> serde_json::Value {
    json!({
        "protocolVersion": 1,
        "type": "command.execute",
        "connectionId": connection_id,
        "requestId": request_id,
        "taskType": "fangyuan.blueprint.generate",
        "agentId": AGENT_ID,
        "projectId": PROJECT_ID,
        "profile": profile,
        "input": {
            "artifactFile": "artifacts/fangyuan/test.ron",
            "consumerTargetFile": null,
            "rulesFile": "rules/fangyuan/test.md",
            "prompt": {
                "theme": "test theme",
                "primitiveLimit": 20,
                "bounds": { "width": 10, "depth": 10, "height": 10 },
                "requirements": ["one requirement"]
            },
            "renderedPrompt": "fixed test prompt"
        },
        "timeoutMs": COMMAND_TIMEOUT_MS,
        "maxOutputBytes": MAX_OUTPUT_BYTES,
        "timestampMs": timestamp_ms,
        "expiresAtMs": timestamp_ms + COMMAND_TTL_MS,
        "nonce": random_base64url::<16>()
    })
}

fn cancel_message(connection_id: &str, request_id: &str, timestamp_ms: u64) -> serde_json::Value {
    json!({
        "protocolVersion": 1,
        "type": "command.cancel",
        "connectionId": connection_id,
        "requestId": request_id,
        "agentId": AGENT_ID,
        "projectId": PROJECT_ID,
        "reasonCode": "ADMIN_CANCELLED",
        "cancelRequestedAtMs": timestamp_ms,
        "cancelDeadlineAtMs": timestamp_ms + CANCEL_TIMEOUT_MS,
        "timestampMs": timestamp_ms,
        "expiresAtMs": timestamp_ms + CANCEL_TIMEOUT_MS,
        "nonce": random_base64url::<16>()
    })
}

#[tokio::test]
async fn completes_handshake_register_heartbeat_and_execute_idempotency() {
    let (listener, endpoint) = listener().await;
    let fixture = Fixture::new(&endpoint);
    let handler = Arc::new(CountingHandler::default());
    let runtime = ClientRuntime::new(handler.clone());
    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    let server = async {
        let mut socket = accept_agent(&listener).await;
        let connection_id = handshake(
            &mut socket,
            &fixture.server_key,
            &fixture.agent_key.verifying_key(),
        )
        .await;
        let unsupported_request_id = Uuid::new_v4().to_string();
        let unsupported_timestamp = now_ms();
        send_signed(
            &mut socket,
            execute_message(
                &connection_id,
                &unsupported_request_id,
                "unknown_profile",
                unsupported_timestamp,
            ),
            &fixture.server_key,
        )
        .await;
        let unsupported = read_agent(&mut socket, &fixture.agent_key.verifying_key()).await;
        assert_eq!(unsupported.string_field("type"), Some("command.error"));
        assert_eq!(
            unsupported.string_field("errorCode"),
            Some("MYFORGE_PROFILE_UNSUPPORTED")
        );
        assert_eq!(handler.calls.load(Ordering::SeqCst), 0);

        let request_id = Uuid::new_v4().to_string();
        let first_timestamp = now_ms();
        send_signed(
            &mut socket,
            execute_message(&connection_id, &request_id, "codex_exec", first_timestamp),
            &fixture.server_key,
        )
        .await;
        let first_error = read_agent(&mut socket, &fixture.agent_key.verifying_key()).await;
        assert_eq!(first_error.string_field("type"), Some("command.error"));
        assert_eq!(
            first_error.string_field("errorCode"),
            Some("MYFORGE_CODEX_UNAVAILABLE")
        );

        let second_timestamp = now_ms();
        send_signed(
            &mut socket,
            execute_message(&connection_id, &request_id, "codex_exec", second_timestamp),
            &fixture.server_key,
        )
        .await;
        let duplicate_error = read_agent(&mut socket, &fixture.agent_key.verifying_key()).await;
        assert_eq!(duplicate_error.string_field("type"), Some("command.error"));

        let heartbeat = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let message = read_agent(&mut socket, &fixture.agent_key.verifying_key()).await;
                if message.string_field("type") == Some("agent.heartbeat") {
                    break message;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(heartbeat.string_field("state"), Some("idle"));
        server_shutdown.cancel();
    };
    let client = runtime.run(&fixture.config, &fixture.preflight, shutdown.clone());
    let (client_result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
    client_result.unwrap();
    assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn delivers_cancel_to_active_worker_without_blocking_dispatcher() {
    let (listener, endpoint) = listener().await;
    let fixture = Fixture::new(&endpoint);
    let handler = Arc::new(CancelAwareHandler::new());
    let runtime = ClientRuntime::new(handler.clone());
    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    let server = async {
        let mut socket = accept_agent(&listener).await;
        let connection_id = handshake(
            &mut socket,
            &fixture.server_key,
            &fixture.agent_key.verifying_key(),
        )
        .await;
        let request_id = Uuid::new_v4().to_string();
        let timestamp = now_ms();
        send_signed(
            &mut socket,
            execute_message(&connection_id, &request_id, "codex_exec", timestamp),
            &fixture.server_key,
        )
        .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        let cancel_timestamp = now_ms();
        send_signed(
            &mut socket,
            cancel_message(&connection_id, &request_id, cancel_timestamp),
            &fixture.server_key,
        )
        .await;
        tokio::time::timeout(Duration::from_secs(1), handler.cancelled.notified())
            .await
            .unwrap();
        server_shutdown.cancel();
    };
    let client = runtime.run(&fixture.config, &fixture.preflight, shutdown.clone());
    let (client_result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
    client_result.unwrap();
    assert_eq!(handler.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn returns_signed_protocol_error_for_invalid_server_signature() {
    let (listener, endpoint) = listener().await;
    let fixture = Fixture::new(&endpoint);
    let runtime = ClientRuntime::new(Arc::new(CountingHandler::default()));
    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    let server = async {
        let mut socket = accept_agent(&listener).await;
        let wrong_key = SigningKey::from_bytes(&[99; 32]);
        let timestamp_ms = now_ms();
        let challenge_id = Uuid::new_v4().to_string();
        send_signed(
            &mut socket,
            challenge_message(&challenge_id, timestamp_ms),
            &wrong_key,
        )
        .await;
        let error = read_agent(&mut socket, &fixture.agent_key.verifying_key()).await;
        assert_eq!(error.string_field("type"), Some("protocol.error"));
        assert_eq!(
            error.string_field("errorCode"),
            Some("MYFORGE_SERVER_SIGNATURE_INVALID")
        );
        assert_eq!(error.object_field("connectionId"), Some(&JsonValue::Null));
        server_shutdown.cancel();
    };
    let client = runtime.run(&fixture.config, &fixture.preflight, shutdown.clone());
    let (client_result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
    client_result.unwrap();
}

#[tokio::test]
async fn signed_challenge_rejections_use_challenge_id_and_peer_auth_ttl() {
    let (listener, endpoint) = listener().await;
    let fixture = Fixture::new(&endpoint);
    let runtime = ClientRuntime::with_options(
        Arc::new(CountingHandler::default()),
        RuntimeHooks {
            clock: Arc::new(SystemClock),
            sleeper: Arc::new(ImmediateSleeper::default()),
            jitter: Arc::new(NoJitter),
        },
        64,
        64,
    );
    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    let server = async {
        let mut limit_socket = accept_agent(&listener).await;
        let limit_challenge_id = Uuid::new_v4().to_string();
        let mut incompatible = challenge_message(&limit_challenge_id, now_ms());
        incompatible["limits"]["heartbeatIntervalMs"] = json!(2_000);
        send_signed(&mut limit_socket, incompatible, &fixture.server_key).await;
        let limit_error = read_agent(&mut limit_socket, &fixture.agent_key.verifying_key()).await;
        assert_valid_challenge_protocol_error(
            &limit_error,
            &limit_challenge_id,
            "MYFORGE_LIMIT_MISMATCH",
        );

        let mut identity_socket = accept_agent(&listener).await;
        let identity_challenge_id = Uuid::new_v4().to_string();
        let mut wrong_identity = challenge_message(&identity_challenge_id, now_ms());
        wrong_identity["projectId"] = json!("different-project");
        send_signed(&mut identity_socket, wrong_identity, &fixture.server_key).await;
        let identity_error =
            read_agent(&mut identity_socket, &fixture.agent_key.verifying_key()).await;
        assert_valid_challenge_protocol_error(
            &identity_error,
            &identity_challenge_id,
            "MYFORGE_IDENTITY_MISMATCH",
        );
        server_shutdown.cancel();
    };
    let client = runtime.run(&fixture.config, &fixture.preflight, shutdown.clone());
    let (client_result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
    client_result.unwrap();
}

#[tokio::test]
async fn returns_signed_protocol_errors_for_expired_schema_and_identity_failures() {
    let (listener, endpoint) = listener().await;
    let fixture = Fixture::new(&endpoint);
    let sleeper = Arc::new(ImmediateSleeper::default());
    let runtime = ClientRuntime::with_options(
        Arc::new(CountingHandler::default()),
        RuntimeHooks {
            clock: Arc::new(SystemClock),
            sleeper,
            jitter: Arc::new(NoJitter),
        },
        64,
        64,
    );
    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    let server = async {
        let mut expired_socket = accept_agent(&listener).await;
        let expired_connection = handshake(
            &mut expired_socket,
            &fixture.server_key,
            &fixture.agent_key.verifying_key(),
        )
        .await;
        let old_timestamp = now_ms() - COMMAND_TTL_MS - CLOCK_SKEW_MS - 100;
        send_signed(
            &mut expired_socket,
            execute_message(
                &expired_connection,
                &Uuid::new_v4().to_string(),
                "codex_exec",
                old_timestamp,
            ),
            &fixture.server_key,
        )
        .await;
        let expired = read_agent(&mut expired_socket, &fixture.agent_key.verifying_key()).await;
        assert_eq!(
            expired.string_field("errorCode"),
            Some("MYFORGE_MESSAGE_EXPIRED")
        );

        let mut schema_socket = accept_agent(&listener).await;
        let schema_connection = handshake(
            &mut schema_socket,
            &fixture.server_key,
            &fixture.agent_key.verifying_key(),
        )
        .await;
        let mut invalid_schema = execute_message(
            &schema_connection,
            &Uuid::new_v4().to_string(),
            "codex_exec",
            now_ms(),
        );
        invalid_schema
            .as_object_mut()
            .unwrap()
            .insert("unexpected".to_string(), json!(true));
        send_signed(&mut schema_socket, invalid_schema, &fixture.server_key).await;
        let schema = read_agent(&mut schema_socket, &fixture.agent_key.verifying_key()).await;
        assert_eq!(
            schema.string_field("errorCode"),
            Some("MYFORGE_MESSAGE_SCHEMA_INVALID")
        );

        let mut missing_socket = accept_agent(&listener).await;
        let missing_connection = handshake(
            &mut missing_socket,
            &fixture.server_key,
            &fixture.agent_key.verifying_key(),
        )
        .await;
        let mut missing_nullable = execute_message(
            &missing_connection,
            &Uuid::new_v4().to_string(),
            "codex_exec",
            now_ms(),
        );
        missing_nullable
            .get_mut("input")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("consumerTargetFile");
        send_signed(&mut missing_socket, missing_nullable, &fixture.server_key).await;
        let missing = read_agent(&mut missing_socket, &fixture.agent_key.verifying_key()).await;
        assert_eq!(
            missing.string_field("errorCode"),
            Some("MYFORGE_MESSAGE_SCHEMA_INVALID")
        );

        let mut identity_socket = accept_agent(&listener).await;
        let identity_connection = handshake(
            &mut identity_socket,
            &fixture.server_key,
            &fixture.agent_key.verifying_key(),
        )
        .await;
        let mut invalid_identity = execute_message(
            &identity_connection,
            &Uuid::new_v4().to_string(),
            "codex_exec",
            now_ms(),
        );
        invalid_identity
            .as_object_mut()
            .unwrap()
            .insert("projectId".to_string(), json!("different-project"));
        send_signed(&mut identity_socket, invalid_identity, &fixture.server_key).await;
        let identity = read_agent(&mut identity_socket, &fixture.agent_key.verifying_key()).await;
        assert_eq!(
            identity.string_field("errorCode"),
            Some("MYFORGE_IDENTITY_MISMATCH")
        );

        let mut state_socket = accept_agent(&listener).await;
        handshake(
            &mut state_socket,
            &fixture.server_key,
            &fixture.agent_key.verifying_key(),
        )
        .await;
        let repeated_id = Uuid::new_v4().to_string();
        send_signed(
            &mut state_socket,
            challenge_message(&repeated_id, now_ms()),
            &fixture.server_key,
        )
        .await;
        let state_error = read_agent(&mut state_socket, &fixture.agent_key.verifying_key()).await;
        assert_eq!(
            state_error.string_field("errorCode"),
            Some("MYFORGE_PROTOCOL_STATE_INVALID")
        );
        server_shutdown.cancel();
    };
    let client = runtime.run(&fixture.config, &fixture.preflight, shutdown.clone());
    let (client_result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
    client_result.unwrap();
}

#[tokio::test]
async fn closes_without_echoing_noncanonical_json() {
    let (listener, endpoint) = listener().await;
    let fixture = Fixture::new(&endpoint);
    let runtime = ClientRuntime::new(Arc::new(CountingHandler::default()));
    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    let server = async {
        let mut socket = accept_agent(&listener).await;
        let connection_id = handshake(
            &mut socket,
            &fixture.server_key,
            &fixture.agent_key.verifying_key(),
        )
        .await;
        let frame = sign_message(
            &execute_message(
                &connection_id,
                &Uuid::new_v4().to_string(),
                "codex_exec",
                now_ms(),
            ),
            &fixture.server_key,
        )
        .unwrap();
        socket
            .send(Message::Text(format!(" {frame}").into()))
            .await
            .unwrap();
        let closed = tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .unwrap();
        assert!(matches!(closed, Some(Ok(Message::Close(_))) | None));
        server_shutdown.cancel();
    };
    let client = runtime.run(&fixture.config, &fixture.preflight, shutdown.clone());
    let (client_result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
    client_result.unwrap();
}

#[derive(Default)]
struct ImmediateSleeper {
    calls: AtomicUsize,
}

#[async_trait]
impl Sleeper for ImmediateSleeper {
    async fn sleep(&self, _duration: Duration) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::task::yield_now().await;
    }
}

struct NoJitter;

impl BackoffJitter for NoJitter {
    fn apply(&self, base: Duration, _attempt: u32) -> Duration {
        base
    }
}

#[tokio::test]
async fn reconnects_with_injected_backoff_and_shutdown_converges() {
    let (listener, endpoint) = listener().await;
    let fixture = Fixture::new(&endpoint);
    let sleeper = Arc::new(ImmediateSleeper::default());
    let runtime = ClientRuntime::with_options(
        Arc::new(CountingHandler::default()),
        RuntimeHooks {
            clock: Arc::new(SystemClock),
            sleeper: sleeper.clone(),
            jitter: Arc::new(NoJitter),
        },
        32,
        32,
    );
    let shutdown = CancellationToken::new();
    let server_shutdown = shutdown.clone();
    let server = async {
        let mut first = accept_agent(&listener).await;
        handshake(
            &mut first,
            &fixture.server_key,
            &fixture.agent_key.verifying_key(),
        )
        .await;
        first.close(None).await.unwrap();

        let mut second = accept_agent(&listener).await;
        handshake(
            &mut second,
            &fixture.server_key,
            &fixture.agent_key.verifying_key(),
        )
        .await;
        server_shutdown.cancel();
    };
    let client = runtime.run(&fixture.config, &fixture.preflight, shutdown.clone());
    let (client_result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(client, server)
    })
    .await
    .unwrap();
    client_result.unwrap();
    assert!(sleeper.calls.load(Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn shutdown_interrupts_an_incomplete_websocket_handshake() {
    let (listener, endpoint) = listener().await;
    let fixture = Fixture::new(&endpoint);
    let runtime = ClientRuntime::new(Arc::new(CountingHandler::default()));
    let shutdown = CancellationToken::new();
    let accepted = Arc::new(Notify::new());
    let server_shutdown = shutdown.clone();
    let server_accepted = accepted.clone();
    let server = async move {
        let (stream, _) = listener.accept().await.unwrap();
        server_accepted.notify_one();
        server_shutdown.cancelled().await;
        drop(stream);
    };
    let controller_shutdown = shutdown.clone();
    let controller = async move {
        accepted.notified().await;
        controller_shutdown.cancel();
    };
    let client = runtime.run(&fixture.config, &fixture.preflight, shutdown.clone());
    let (client_result, (), ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(client, server, controller)
    })
    .await
    .unwrap();
    client_result.unwrap();
}
