use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::config::AgentLimits;
use crate::preflight::{Capabilities, ForgeRootSummary};
use crate::protocol::{
    JsonValue, MAX_SAFE_INTEGER, PROTOCOL_VERSION, ProtocolError, RESULT_FIXED_RESERVE_BYTES,
    deserialize, strict_base64url,
};

const PROTOCOL_ERROR_CODES: &[&str] = &[
    "MYFORGE_AGENT_AUTH_FAILED",
    "MYFORGE_AGENT_UNKNOWN",
    "MYFORGE_IDENTITY_MISMATCH",
    "MYFORGE_SERVER_SIGNATURE_INVALID",
    "MYFORGE_AGENT_SIGNATURE_INVALID",
    "MYFORGE_MESSAGE_EXPIRED",
    "MYFORGE_REPLAY_DETECTED",
    "MYFORGE_LIMIT_MISMATCH",
    "MYFORGE_MESSAGE_IJSON_INVALID",
    "MYFORGE_MESSAGE_SCHEMA_INVALID",
    "MYFORGE_PROTOCOL_VERSION_UNSUPPORTED",
    "MYFORGE_PROTOCOL_STATE_INVALID",
    "MYFORGE_DUPLICATE_REQUEST_CONFLICT",
    "MYFORGE_DUPLICATE_RESULT_CONFLICT",
    "MYFORGE_AGENT_BUSY",
    "MYFORGE_AGENT_DISCONNECTED",
    "MYFORGE_SERVER_RESTARTED",
    "MYFORGE_OUTPUT_TOO_LARGE",
];
const COMMAND_ERROR_CODES: &[&str] = &[
    "MYFORGE_ROOT_MISSING",
    "MYFORGE_ROOT_INVALID",
    "MYFORGE_TARGET_PATH_INVALID",
    "MYFORGE_RULES_FILE_MISSING",
    "MYFORGE_CODEX_UNAVAILABLE",
    "MYFORGE_PROFILE_UNSUPPORTED",
    "MYFORGE_COMMAND_EXPIRED",
    "MYFORGE_COMMAND_SPAWN_FAILED",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerLimits {
    pub auth_ttl_ms: u64,
    pub command_ttl_ms: u64,
    pub clock_skew_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub heartbeat_timeout_ms: u64,
    pub command_timeout_ms: u64,
    pub cancel_timeout_ms: u64,
    pub max_output_bytes: u64,
    pub ws_max_message_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveLimits {
    pub auth_ttl_ms: u64,
    pub command_ttl_ms: u64,
    pub server_clock_skew_ms: u64,
    pub agent_clock_skew_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub heartbeat_timeout_ms: u64,
    pub command_timeout_ms: u64,
    pub cancel_timeout_ms: u64,
    pub max_output_bytes: u64,
    pub ws_max_message_bytes: u64,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerChallenge {
    pub protocol_version: u8,
    #[serde(rename = "type")]
    pub message_type: String,
    pub challenge_id: String,
    pub challenge: String,
    pub agent_id: String,
    pub project_id: String,
    pub limits: ServerLimits,
    pub timestamp_ms: u64,
    pub expires_at_ms: u64,
    pub nonce: String,
    pub signature: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandExecute {
    pub protocol_version: u8,
    #[serde(rename = "type")]
    pub message_type: String,
    pub connection_id: String,
    pub request_id: String,
    pub task_type: String,
    pub agent_id: String,
    pub project_id: String,
    pub profile: String,
    pub input: CommandInput,
    pub timeout_ms: u64,
    pub max_output_bytes: u64,
    pub timestamp_ms: u64,
    pub expires_at_ms: u64,
    pub nonce: String,
    pub signature: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandInput {
    pub artifact_file: String,
    pub consumer_target_file: Option<String>,
    pub rules_file: String,
    pub prompt: BlueprintPrompt,
    pub rendered_prompt: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlueprintPrompt {
    pub theme: String,
    pub primitive_limit: u64,
    pub bounds: BlueprintBounds,
    pub requirements: Vec<String>,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BlueprintBounds {
    pub width: u64,
    pub depth: u64,
    pub height: u64,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandCancel {
    pub protocol_version: u8,
    #[serde(rename = "type")]
    pub message_type: String,
    pub connection_id: String,
    pub request_id: String,
    pub agent_id: String,
    pub project_id: String,
    pub reason_code: String,
    pub cancel_requested_at_ms: u64,
    pub cancel_deadline_at_ms: u64,
    pub timestamp_ms: u64,
    pub expires_at_ms: u64,
    pub nonce: String,
    pub signature: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PeerProtocolError {
    pub protocol_version: u8,
    #[serde(rename = "type")]
    pub message_type: String,
    pub connection_id: Option<String>,
    pub agent_id: String,
    pub project_id: String,
    pub request_id: Option<String>,
    pub error_code: String,
    pub error_message: String,
    pub fatal: bool,
    pub timestamp_ms: u64,
    pub expires_at_ms: u64,
    pub nonce: String,
    pub signature: String,
}

#[derive(Clone, Eq, PartialEq)]
pub enum ServerMessage {
    Challenge(ServerChallenge),
    Execute(CommandExecute),
    Cancel(CommandCancel),
    ProtocolError(PeerProtocolError),
}

impl ServerMessage {
    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::Challenge(_) => None,
            Self::Execute(message) => Some(&message.request_id),
            Self::Cancel(message) => Some(&message.request_id),
            Self::ProtocolError(message) => message.request_id.as_deref(),
        }
    }

    pub fn connection_id(&self) -> Option<&str> {
        match self {
            Self::Challenge(message) => Some(&message.challenge_id),
            Self::Execute(message) => Some(&message.connection_id),
            Self::Cancel(message) => Some(&message.connection_id),
            Self::ProtocolError(message) => message.connection_id.as_deref(),
        }
    }

    pub fn nonce(&self) -> &str {
        match self {
            Self::Challenge(message) => &message.nonce,
            Self::Execute(message) => &message.nonce,
            Self::Cancel(message) => &message.nonce,
            Self::ProtocolError(message) => &message.nonce,
        }
    }

    pub const fn timestamp_ms(&self) -> u64 {
        match self {
            Self::Challenge(message) => message.timestamp_ms,
            Self::Execute(message) => message.timestamp_ms,
            Self::Cancel(message) => message.timestamp_ms,
            Self::ProtocolError(message) => message.timestamp_ms,
        }
    }

    pub const fn expires_at_ms(&self) -> u64 {
        match self {
            Self::Challenge(message) => message.expires_at_ms,
            Self::Execute(message) => message.expires_at_ms,
            Self::Cancel(message) => message.expires_at_ms,
            Self::ProtocolError(message) => message.expires_at_ms,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHello<'a> {
    pub protocol_version: u8,
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub challenge_id: &'a str,
    pub challenge: &'a str,
    pub agent_id: &'a str,
    pub project_id: &'a str,
    pub timestamp_ms: u64,
    pub expires_at_ms: u64,
    pub nonce: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRegister<'a> {
    pub protocol_version: u8,
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub connection_id: &'a str,
    pub agent_id: &'a str,
    pub project_id: &'a str,
    pub hostname: &'a str,
    pub platform: &'a str,
    pub agent_version: &'a str,
    pub forge_root_summary: &'a ForgeRootSummary,
    pub capabilities: &'a Capabilities,
    pub limits: AgentLimits,
    pub timestamp_ms: u64,
    pub expires_at_ms: u64,
    pub nonce: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHeartbeat<'a> {
    pub protocol_version: u8,
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub connection_id: &'a str,
    pub agent_id: &'a str,
    pub project_id: &'a str,
    pub sequence: u32,
    pub state: &'static str,
    pub active_request_id: Option<&'a str>,
    pub timestamp_ms: u64,
    pub expires_at_ms: u64,
    pub nonce: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandStarted<'a> {
    pub protocol_version: u8,
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub connection_id: &'a str,
    pub request_id: &'a str,
    pub agent_id: &'a str,
    pub project_id: &'a str,
    pub execution_mode: &'static str,
    pub started_at_ms: u64,
    pub timestamp_ms: u64,
    pub expires_at_ms: u64,
    pub nonce: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandErrorMessage<'a> {
    pub protocol_version: u8,
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub connection_id: &'a str,
    pub request_id: &'a str,
    pub agent_id: &'a str,
    pub project_id: &'a str,
    pub error_code: &'a str,
    pub error_message: &'a str,
    pub retryable: bool,
    pub timestamp_ms: u64,
    pub expires_at_ms: u64,
    pub nonce: String,
}

#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactSummary {
    pub exists: bool,
    pub sha256: Option<String>,
    pub bytes: Option<u64>,
    pub modified_at_ms: Option<u64>,
}

impl ArtifactSummary {
    pub const fn missing() -> Self {
        Self {
            exists: false,
            sha256: None,
            bytes: None,
            modified_at_ms: None,
        }
    }
}

#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditFinding {
    pub severity: String,
    pub code: String,
    pub field_path: String,
    pub message: String,
}

#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditSummary {
    pub status: String,
    pub errors: Option<u64>,
    pub warnings: Option<u64>,
    pub primitive_count: Option<u64>,
    pub main_code: Option<String>,
    pub reason_code: Option<String>,
    pub findings_preview: Vec<AuditFinding>,
}

impl AuditSummary {
    pub fn skipped(reason_code: &str) -> Self {
        Self {
            status: "skipped".to_string(),
            errors: None,
            warnings: None,
            primitive_count: None,
            main_code: None,
            reason_code: Some(reason_code.to_string()),
            findings_preview: Vec::new(),
        }
    }

    pub fn unavailable() -> Self {
        Self {
            status: "unavailable".to_string(),
            errors: None,
            warnings: None,
            primitive_count: None,
            main_code: None,
            reason_code: Some("auditor_not_configured".to_string()),
            findings_preview: Vec::new(),
        }
    }
}

#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResultSemantic {
    pub execution_mode: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout_preview: String,
    pub stderr_preview: String,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub artifact_file: String,
    pub consumer_target_file: Option<String>,
    pub artifact: ArtifactSummary,
    pub audit: AuditSummary,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub started_at_ms: Option<u64>,
    pub completed_at_ms: u64,
}

impl std::fmt::Debug for CommandResultSemantic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommandResultSemantic")
            .field("execution_mode", &self.execution_mode)
            .field("status", &self.status)
            .field("exit_code", &self.exit_code)
            .field("stdout_bytes", &self.stdout_bytes)
            .field("stderr_bytes", &self.stderr_bytes)
            .field("stdout_truncated", &self.stdout_truncated)
            .field("stderr_truncated", &self.stderr_truncated)
            .field("artifact_exists", &self.artifact.exists)
            .field("audit_status", &self.audit.status)
            .field("error_code", &self.error_code)
            .field("started_at_ms", &self.started_at_ms)
            .field("completed_at_ms", &self.completed_at_ms)
            .finish_non_exhaustive()
    }
}

impl CommandResultSemantic {
    pub fn output_too_large_fallback(&self) -> Self {
        let mut fallback = self.clone();
        fallback.status = "failed".to_string();
        fallback.stdout_preview.clear();
        fallback.stderr_preview.clear();
        fallback.stdout_truncated = fallback.stdout_bytes > 0;
        fallback.stderr_truncated = fallback.stderr_bytes > 0;
        fallback.audit = AuditSummary::skipped("execution_failed");
        fallback.error_code = Some("MYFORGE_OUTPUT_TOO_LARGE".to_string());
        fallback.error_message = Some("result exceeds the negotiated size limit".to_string());
        fallback
    }

    pub fn validate(&self, max_output_bytes: u64) -> Result<(), ProtocolError> {
        validate_command_result(self, max_output_bytes)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResultMessage<'a> {
    pub protocol_version: u8,
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub connection_id: &'a str,
    pub request_id: &'a str,
    pub agent_id: &'a str,
    pub project_id: &'a str,
    #[serde(flatten)]
    pub result: &'a CommandResultSemantic,
    pub timestamp_ms: u64,
    pub expires_at_ms: u64,
    pub nonce: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolErrorMessage<'a> {
    pub protocol_version: u8,
    #[serde(rename = "type")]
    pub message_type: &'static str,
    pub connection_id: Option<&'a str>,
    pub agent_id: &'a str,
    pub project_id: &'a str,
    pub request_id: Option<&'a str>,
    pub error_code: &'a str,
    pub error_message: &'a str,
    pub fatal: bool,
    pub timestamp_ms: u64,
    pub expires_at_ms: u64,
    pub nonce: String,
}

pub fn parse_server_message(value: &JsonValue) -> Result<ServerMessage, ProtocolError> {
    let message_type = value.string_field("type").ok_or_else(|| {
        ProtocolError::new("MYFORGE_MESSAGE_SCHEMA_INVALID", "message.type is required")
    })?;
    let message = match message_type {
        "server.challenge" => {
            exact_fields(
                value,
                &[
                    "protocolVersion",
                    "type",
                    "challengeId",
                    "challenge",
                    "agentId",
                    "projectId",
                    "limits",
                    "timestampMs",
                    "expiresAtMs",
                    "nonce",
                    "signature",
                ],
                "message",
            )?;
            exact_fields(
                required_object_field(value, "limits")?,
                &[
                    "authTtlMs",
                    "commandTtlMs",
                    "clockSkewMs",
                    "heartbeatIntervalMs",
                    "heartbeatTimeoutMs",
                    "commandTimeoutMs",
                    "cancelTimeoutMs",
                    "maxOutputBytes",
                    "wsMaxMessageBytes",
                ],
                "limits",
            )?;
            let message: ServerChallenge = deserialize(value)?;
            validate_challenge(&message)?;
            ServerMessage::Challenge(message)
        }
        "command.execute" => {
            exact_fields(
                value,
                &[
                    "protocolVersion",
                    "type",
                    "connectionId",
                    "requestId",
                    "taskType",
                    "agentId",
                    "projectId",
                    "profile",
                    "input",
                    "timeoutMs",
                    "maxOutputBytes",
                    "timestampMs",
                    "expiresAtMs",
                    "nonce",
                    "signature",
                ],
                "message",
            )?;
            let input = required_object_field(value, "input")?;
            exact_fields(
                input,
                &[
                    "artifactFile",
                    "consumerTargetFile",
                    "rulesFile",
                    "prompt",
                    "renderedPrompt",
                ],
                "input",
            )?;
            let prompt = required_object_field(input, "prompt")?;
            exact_fields(
                prompt,
                &["theme", "primitiveLimit", "bounds", "requirements"],
                "input.prompt",
            )?;
            exact_fields(
                required_object_field(prompt, "bounds")?,
                &["width", "depth", "height"],
                "input.prompt.bounds",
            )?;
            let message: CommandExecute = deserialize(value)?;
            validate_execute_structure(&message)?;
            ServerMessage::Execute(message)
        }
        "command.cancel" => {
            exact_fields(
                value,
                &[
                    "protocolVersion",
                    "type",
                    "connectionId",
                    "requestId",
                    "agentId",
                    "projectId",
                    "reasonCode",
                    "cancelRequestedAtMs",
                    "cancelDeadlineAtMs",
                    "timestampMs",
                    "expiresAtMs",
                    "nonce",
                    "signature",
                ],
                "message",
            )?;
            let message: CommandCancel = deserialize(value)?;
            validate_cancel_structure(&message)?;
            ServerMessage::Cancel(message)
        }
        "protocol.error" => {
            exact_fields(
                value,
                &[
                    "protocolVersion",
                    "type",
                    "connectionId",
                    "agentId",
                    "projectId",
                    "requestId",
                    "errorCode",
                    "errorMessage",
                    "fatal",
                    "timestampMs",
                    "expiresAtMs",
                    "nonce",
                    "signature",
                ],
                "message",
            )?;
            let message: PeerProtocolError = deserialize(value)?;
            validate_protocol_error(&message)?;
            ServerMessage::ProtocolError(message)
        }
        _ => {
            return Err(ProtocolError::new(
                "MYFORGE_MESSAGE_SCHEMA_INVALID",
                "message.type is unsupported",
            ));
        }
    };
    Ok(message)
}

fn required_object_field<'a>(
    value: &'a JsonValue,
    name: &str,
) -> Result<&'a JsonValue, ProtocolError> {
    value.object_field(name).ok_or_else(|| {
        ProtocolError::new(
            "MYFORGE_MESSAGE_SCHEMA_INVALID",
            format!("{name} must be an object"),
        )
    })
}

fn exact_fields(value: &JsonValue, expected: &[&str], label: &str) -> Result<(), ProtocolError> {
    if !value.has_exact_object_fields(expected) {
        return schema_error(&format!("{label} has an invalid field set"));
    }
    Ok(())
}

fn validate_challenge(message: &ServerChallenge) -> Result<(), ProtocolError> {
    validate_envelope(
        message.protocol_version,
        &message.message_type,
        "server.challenge",
        message.timestamp_ms,
        message.expires_at_ms,
        &message.nonce,
        &message.signature,
    )?;
    validate_uuid(&message.challenge_id, "challengeId")?;
    strict_base64url(&message.challenge, 32, "challenge")?;
    validate_identity(&message.agent_id, &message.project_id)?;
    validate_server_limits(message.limits)
}

fn validate_execute_structure(message: &CommandExecute) -> Result<(), ProtocolError> {
    validate_envelope(
        message.protocol_version,
        &message.message_type,
        "command.execute",
        message.timestamp_ms,
        message.expires_at_ms,
        &message.nonce,
        &message.signature,
    )?;
    validate_uuid(&message.connection_id, "connectionId")?;
    validate_uuid(&message.request_id, "requestId")?;
    validate_identity(&message.agent_id, &message.project_id)?;
    validate_text(&message.task_type, "taskType", 1, 64, false)?;
    validate_text(&message.profile, "profile", 1, 64, false)?;
    validate_prompt(&message.input.prompt)?;
    validate_text(
        &message.input.rendered_prompt,
        "input.renderedPrompt",
        1,
        16_384,
        true,
    )?;
    integer_range(message.timeout_ms, "timeoutMs", 1_000, 1_800_000)?;
    integer_range(message.max_output_bytes, "maxOutputBytes", 4_096, 4_194_304)
}

fn validate_cancel_structure(message: &CommandCancel) -> Result<(), ProtocolError> {
    validate_envelope(
        message.protocol_version,
        &message.message_type,
        "command.cancel",
        message.timestamp_ms,
        message.expires_at_ms,
        &message.nonce,
        &message.signature,
    )?;
    validate_uuid(&message.connection_id, "connectionId")?;
    validate_uuid(&message.request_id, "requestId")?;
    validate_identity(&message.agent_id, &message.project_id)?;
    if message.reason_code != "ADMIN_CANCELLED" {
        return schema_error("reasonCode is invalid");
    }
    if message.cancel_deadline_at_ms <= message.cancel_requested_at_ms
        || message.timestamp_ms < message.cancel_requested_at_ms
        || message.timestamp_ms >= message.cancel_deadline_at_ms
        || message.expires_at_ms > message.cancel_deadline_at_ms
    {
        return schema_error("command.cancel timing fields are inconsistent");
    }
    Ok(())
}

fn validate_protocol_error(message: &PeerProtocolError) -> Result<(), ProtocolError> {
    validate_envelope(
        message.protocol_version,
        &message.message_type,
        "protocol.error",
        message.timestamp_ms,
        message.expires_at_ms,
        &message.nonce,
        &message.signature,
    )?;
    if let Some(connection_id) = &message.connection_id {
        validate_uuid(connection_id, "connectionId")?;
    }
    if let Some(request_id) = &message.request_id {
        validate_uuid(request_id, "requestId")?;
    }
    validate_identity(&message.agent_id, &message.project_id)?;
    if !PROTOCOL_ERROR_CODES.contains(&message.error_code.as_str()) {
        return schema_error("protocol.error errorCode is not allowed");
    }
    validate_text(&message.error_message, "errorMessage", 1, 512, false)?;
    if !message.fatal {
        return schema_error("P0 protocol.error must be fatal");
    }
    Ok(())
}

fn validate_envelope(
    protocol_version: u8,
    message_type: &str,
    expected_type: &str,
    timestamp_ms: u64,
    expires_at_ms: u64,
    nonce: &str,
    signature: &str,
) -> Result<(), ProtocolError> {
    if i64::from(protocol_version) != PROTOCOL_VERSION {
        return Err(ProtocolError::new(
            "MYFORGE_PROTOCOL_VERSION_UNSUPPORTED",
            "protocolVersion is unsupported",
        ));
    }
    if message_type != expected_type {
        return schema_error("message type is invalid");
    }
    integer_range(timestamp_ms, "timestampMs", 0, MAX_SAFE_INTEGER as u64)?;
    integer_range(expires_at_ms, "expiresAtMs", 0, MAX_SAFE_INTEGER as u64)?;
    strict_base64url(nonce, 16, "nonce")?;
    strict_base64url(signature, 64, "signature")?;
    Ok(())
}

pub fn negotiate_limits(
    server: ServerLimits,
    agent: AgentLimits,
) -> Result<EffectiveLimits, ProtocolError> {
    if agent.heartbeat_interval_ms != server.heartbeat_interval_ms {
        return limit_error("heartbeatIntervalMs differs between server and agent");
    }
    if agent.auth_ttl_ms < server.auth_ttl_ms {
        return limit_error("agent authTtlMs is below the challenge lifetime");
    }
    let ws_max_message_bytes = server.ws_max_message_bytes.min(agent.ws_max_message_bytes);
    let frame_output_budget = ws_max_message_bytes.saturating_sub(RESULT_FIXED_RESERVE_BYTES) / 12;
    let max_output_bytes = server
        .max_output_bytes
        .min(agent.max_output_bytes)
        .min(frame_output_budget);
    if max_output_bytes < 4_096 {
        return limit_error("negotiated output budget is below 4096 bytes");
    }
    Ok(EffectiveLimits {
        auth_ttl_ms: server.auth_ttl_ms.min(agent.auth_ttl_ms),
        command_ttl_ms: server.command_ttl_ms.min(agent.command_ttl_ms),
        server_clock_skew_ms: server.clock_skew_ms,
        agent_clock_skew_ms: agent.clock_skew_ms,
        heartbeat_interval_ms: server.heartbeat_interval_ms,
        heartbeat_timeout_ms: server.heartbeat_timeout_ms,
        command_timeout_ms: server.command_timeout_ms.min(agent.max_command_timeout_ms),
        cancel_timeout_ms: server.cancel_timeout_ms.min(agent.cancel_timeout_ms),
        max_output_bytes,
        ws_max_message_bytes,
    })
}

pub fn validate_challenge_compatibility(
    challenge: &ServerChallenge,
    agent: AgentLimits,
) -> Result<EffectiveLimits, ProtocolError> {
    let lifetime = challenge
        .expires_at_ms
        .checked_sub(challenge.timestamp_ms)
        .ok_or_else(|| {
            ProtocolError::new(
                "MYFORGE_MESSAGE_EXPIRED",
                "message has an invalid validity window",
            )
        })?;
    if lifetime != challenge.limits.auth_ttl_ms || lifetime > agent.auth_ttl_ms {
        return limit_error("challenge lifetime does not match compatible auth limits");
    }
    negotiate_limits(challenge.limits, agent)
}

pub fn validate_execute_business(
    message: &CommandExecute,
    effective: EffectiveLimits,
) -> Result<(), CommandRejection> {
    let paths_valid = validate_path(
        &message.input.artifact_file,
        "input.artifactFile",
        "artifacts/fangyuan/",
        ".ron",
    )
    .is_ok()
        && message
            .input
            .consumer_target_file
            .as_ref()
            .is_none_or(|path| {
                validate_path(
                    path,
                    "input.consumerTargetFile",
                    "project/assets/fangyuan/",
                    ".ron",
                )
                .is_ok()
            })
        && validate_path(
            &message.input.rules_file,
            "input.rulesFile",
            "rules/fangyuan/",
            ".md",
        )
        .is_ok();
    if !paths_valid {
        return Err(CommandRejection::new(
            "MYFORGE_TARGET_PATH_INVALID",
            "command path is outside the allowed workspace layout",
            false,
        ));
    }
    if message.task_type != "fangyuan.blueprint.generate" {
        return Err(CommandRejection::new(
            "MYFORGE_PROFILE_UNSUPPORTED",
            "task type is unsupported",
            false,
        ));
    }
    if message.profile != "codex_exec" {
        return Err(CommandRejection::new(
            "MYFORGE_PROFILE_UNSUPPORTED",
            "execution profile is unsupported",
            false,
        ));
    }
    if message.timeout_ms != effective.command_timeout_ms
        || message.max_output_bytes != effective.max_output_bytes
    {
        return Err(CommandRejection::protocol_limit());
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandRejection {
    pub error_code: &'static str,
    pub error_message: &'static str,
    pub retryable: bool,
    pub protocol_fatal: bool,
}

impl CommandRejection {
    pub const fn new(
        error_code: &'static str,
        error_message: &'static str,
        retryable: bool,
    ) -> Self {
        Self {
            error_code,
            error_message,
            retryable,
            protocol_fatal: false,
        }
    }

    pub const fn protocol_limit() -> Self {
        Self {
            error_code: "MYFORGE_LIMIT_MISMATCH",
            error_message: "command limits do not match the connection",
            retryable: false,
            protocol_fatal: true,
        }
    }

    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.protocol_fatal {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "protocol failure cannot be sent as command.error",
            ));
        }
        if !COMMAND_ERROR_CODES.contains(&self.error_code) {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "command handler returned an unsupported error code",
            ));
        }
        validate_text(self.error_message, "errorMessage", 1, 512, false)
    }
}

pub fn validate_message_time(
    timestamp_ms: u64,
    expires_at_ms: u64,
    now_ms: u64,
    clock_skew_ms: u64,
    ttl_ms: u64,
    exact_lifetime_ms: Option<u64>,
) -> Result<(), ProtocolError> {
    let lifetime = expires_at_ms.checked_sub(timestamp_ms).ok_or_else(|| {
        ProtocolError::new(
            "MYFORGE_MESSAGE_EXPIRED",
            "message has an invalid validity window",
        )
    })?;
    if lifetime == 0 || lifetime > ttl_ms || exact_lifetime_ms.is_some_and(|ttl| ttl != lifetime) {
        return limit_error("message lifetime does not match negotiated limits");
    }
    if timestamp_ms > now_ms.saturating_add(clock_skew_ms)
        || expires_at_ms.saturating_add(clock_skew_ms) < now_ms
    {
        return Err(ProtocolError::new(
            "MYFORGE_MESSAGE_EXPIRED",
            "message is outside the accepted time window",
        ));
    }
    Ok(())
}

fn validate_command_result(
    result: &CommandResultSemantic,
    max_output_bytes: u64,
) -> Result<(), ProtocolError> {
    let safe_max = MAX_SAFE_INTEGER as u64;
    if !matches!(result.execution_mode.as_str(), "codex_exec" | "dry_run") {
        return schema_error("command.result executionMode is invalid");
    }
    if !matches!(
        result.status.as_str(),
        "completed" | "completed_with_errors" | "failed" | "cancelled"
    ) {
        return schema_error("command.result status is invalid");
    }
    if result.stdout_preview.len() as u64 > max_output_bytes
        || result.stderr_preview.len() as u64 > max_output_bytes
        || result.stdout_bytes > safe_max
        || result.stderr_bytes > safe_max
        || result.stdout_bytes < result.stdout_preview.len() as u64
        || result.stderr_bytes < result.stderr_preview.len() as u64
    {
        return schema_error("command.result output fields are invalid");
    }
    validate_path(
        &result.artifact_file,
        "artifactFile",
        "artifacts/fangyuan/",
        ".ron",
    )?;
    if let Some(path) = &result.consumer_target_file {
        validate_path(
            path,
            "consumerTargetFile",
            "project/assets/fangyuan/",
            ".ron",
        )?;
    }
    validate_artifact_summary(&result.artifact)?;
    validate_audit_summary(&result.audit)?;
    if let Some(code) = &result.error_code
        && !valid_error_code(code)
    {
        return schema_error("command.result errorCode is invalid");
    }
    if let Some(message) = &result.error_message {
        validate_text(message, "errorMessage", 1, 512, false)?;
    }
    if result.completed_at_ms > safe_max
        || result
            .started_at_ms
            .is_some_and(|started| started > safe_max || started > result.completed_at_ms)
    {
        return schema_error("command.result timing fields are invalid");
    }
    if result.status == "completed" {
        if result.error_code.is_some() || result.error_message.is_some() {
            return schema_error("completed result must not contain an error");
        }
    } else if result.error_code.is_none() || result.error_message.is_none() {
        return schema_error("non-completed result requires an error");
    }

    if result.execution_mode == "dry_run" {
        if result.status == "completed"
            && (result.exit_code.is_some()
                || result.started_at_ms.is_none()
                || result.audit.status != "skipped"
                || result.audit.reason_code.as_deref() != Some("dry_run"))
        {
            return schema_error("dry_run completed result fields are inconsistent");
        }
        if !matches!(result.status.as_str(), "completed" | "cancelled") {
            return schema_error("dry_run result status is invalid");
        }
    }
    if result.execution_mode == "codex_exec"
        && result.status == "completed"
        && (result.exit_code != Some(0)
            || result.started_at_ms.is_none()
            || !result.artifact.exists
            || !matches!(result.audit.status.as_str(), "passed" | "unavailable"))
    {
        return schema_error("completed codex_exec result fields are inconsistent");
    }
    if result.status == "completed_with_errors" {
        let expected = match result.audit.status.as_str() {
            "warning" => Some("FANGYUAN_BLUEPRINT_AUDIT_WARNING"),
            "failed" => Some("FANGYUAN_BLUEPRINT_AUDIT_FAILED"),
            _ => None,
        };
        if result.execution_mode != "codex_exec"
            || result.exit_code != Some(0)
            || result.started_at_ms.is_none()
            || !result.artifact.exists
            || expected != result.error_code.as_deref()
        {
            return schema_error("completed_with_errors result fields are inconsistent");
        }
    }
    if result.status == "failed" {
        if result.started_at_ms.is_none()
            || result.audit.status != "skipped"
            || !matches!(
                result.audit.reason_code.as_deref(),
                Some("execution_failed" | "artifact_missing")
            )
        {
            return schema_error("failed result fields are inconsistent");
        }
        if result.audit.reason_code.as_deref() == Some("artifact_missing") {
            if result.error_code.as_deref() != Some("MYFORGE_TARGET_FILE_MISSING")
                || result.exit_code != Some(0)
                || result.artifact.exists
            {
                return schema_error("artifact-missing result fields are inconsistent");
            }
        } else if !matches!(
            result.error_code.as_deref(),
            Some("MYFORGE_COMMAND_TIMEOUT" | "MYFORGE_COMMAND_FAILED" | "MYFORGE_OUTPUT_TOO_LARGE")
        ) || (result.error_code.as_deref() == Some("MYFORGE_COMMAND_TIMEOUT")
            && result.exit_code.is_some())
            || (result.error_code.as_deref() == Some("MYFORGE_COMMAND_FAILED")
                && result.exit_code == Some(0))
        {
            return schema_error("execution-failed result fields are inconsistent");
        }
    }
    if result.status == "cancelled"
        && (result.audit.status != "skipped"
            || result.audit.reason_code.as_deref() != Some("cancelled")
            || result.error_code.as_deref() != Some("MYFORGE_COMMAND_CANCELLED")
            || (result.started_at_ms.is_none() && result.exit_code.is_some()))
    {
        return schema_error("cancelled result fields are inconsistent");
    }
    Ok(())
}

fn validate_artifact_summary(artifact: &ArtifactSummary) -> Result<(), ProtocolError> {
    if artifact.exists {
        let Some(sha256) = artifact.sha256.as_deref() else {
            return schema_error("artifact sha256 is required");
        };
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            || artifact
                .bytes
                .is_none_or(|value| value > MAX_SAFE_INTEGER as u64)
            || artifact
                .modified_at_ms
                .is_none_or(|value| value > MAX_SAFE_INTEGER as u64)
        {
            return schema_error("artifact summary is invalid");
        }
    } else if artifact.sha256.is_some()
        || artifact.bytes.is_some()
        || artifact.modified_at_ms.is_some()
    {
        return schema_error("missing artifact fields must be null");
    }
    Ok(())
}

fn validate_audit_summary(audit: &AuditSummary) -> Result<(), ProtocolError> {
    if audit.findings_preview.len() > 20 {
        return schema_error("audit findings exceed the allowed count");
    }
    for finding in &audit.findings_preview {
        if !matches!(finding.severity.as_str(), "info" | "warning" | "error")
            || !valid_lower_code(&finding.code)
        {
            return schema_error("audit finding is invalid");
        }
        validate_text(&finding.field_path, "finding.fieldPath", 1, 256, false)?;
        validate_text(&finding.message, "finding.message", 1, 512, false)?;
    }
    match audit.status.as_str() {
        "passed" | "warning" | "failed" => {
            if audit
                .errors
                .is_none_or(|value| value > MAX_SAFE_INTEGER as u64)
                || audit
                    .warnings
                    .is_none_or(|value| value > MAX_SAFE_INTEGER as u64)
                || audit
                    .primitive_count
                    .is_some_and(|value| value > MAX_SAFE_INTEGER as u64)
                || audit.reason_code.is_some()
            {
                return schema_error("audit counters are invalid");
            }
            if audit.status == "passed" {
                if audit.main_code.is_some() {
                    return schema_error("passed audit has invalid codes");
                }
            } else if audit
                .main_code
                .as_deref()
                .is_none_or(|code| !valid_lower_code(code))
                || audit.findings_preview.is_empty()
            {
                return schema_error("warning or failed audit is incomplete");
            }
        }
        "skipped" | "unavailable" => {
            if audit.errors.is_some()
                || audit.warnings.is_some()
                || audit.primitive_count.is_some()
                || audit.main_code.is_some()
                || !audit.findings_preview.is_empty()
            {
                return schema_error("skipped or unavailable audit fields are invalid");
            }
            let valid_reason = if audit.status == "unavailable" {
                audit.reason_code.as_deref() == Some("auditor_not_configured")
            } else {
                matches!(
                    audit.reason_code.as_deref(),
                    Some("dry_run" | "execution_failed" | "artifact_missing" | "cancelled")
                )
            };
            if !valid_reason {
                return schema_error("audit reasonCode is invalid");
            }
        }
        _ => return schema_error("audit status is invalid"),
    }
    Ok(())
}

fn valid_error_code(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_uppercase()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_lower_code(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && (value.as_bytes()[0].is_ascii_lowercase() || value.as_bytes()[0].is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
        })
}

fn validate_server_limits(limits: ServerLimits) -> Result<(), ProtocolError> {
    integer_range(limits.auth_ttl_ms, "limits.authTtlMs", 5_000, 300_000)?;
    integer_range(limits.command_ttl_ms, "limits.commandTtlMs", 5_000, 300_000)?;
    integer_range(limits.clock_skew_ms, "limits.clockSkewMs", 0, 30_000)?;
    integer_range(
        limits.heartbeat_interval_ms,
        "limits.heartbeatIntervalMs",
        1_000,
        60_000,
    )?;
    integer_range(
        limits.heartbeat_timeout_ms,
        "limits.heartbeatTimeoutMs",
        3_000,
        180_000,
    )?;
    integer_range(
        limits.command_timeout_ms,
        "limits.commandTimeoutMs",
        1_000,
        1_800_000,
    )?;
    integer_range(
        limits.cancel_timeout_ms,
        "limits.cancelTimeoutMs",
        1_000,
        30_000,
    )?;
    integer_range(
        limits.max_output_bytes,
        "limits.maxOutputBytes",
        4_096,
        4_194_304,
    )?;
    integer_range(
        limits.ws_max_message_bytes,
        "limits.wsMaxMessageBytes",
        524_288,
        33_554_432,
    )?;
    let double_skew = limits.clock_skew_ms.saturating_mul(2);
    if double_skew >= limits.auth_ttl_ms || double_skew >= limits.command_ttl_ms {
        return limit_error("server TTL and clock skew invariants are invalid");
    }
    if limits.heartbeat_timeout_ms
        < limits
            .heartbeat_interval_ms
            .saturating_mul(2)
            .saturating_add(limits.clock_skew_ms)
    {
        return limit_error("server heartbeat timeout invariant is invalid");
    }
    if limits.cancel_timeout_ms > limits.command_timeout_ms {
        return limit_error("server cancel timeout invariant is invalid");
    }
    Ok(())
}

fn validate_prompt(prompt: &BlueprintPrompt) -> Result<(), ProtocolError> {
    validate_text(&prompt.theme, "input.prompt.theme", 1, 200, false)?;
    if prompt.theme.trim() != prompt.theme {
        return schema_error("input.prompt.theme must be normalized");
    }
    integer_range(
        prompt.primitive_limit,
        "input.prompt.primitiveLimit",
        1,
        1_000,
    )?;
    for (name, value) in [
        ("width", prompt.bounds.width),
        ("depth", prompt.bounds.depth),
        ("height", prompt.bounds.height),
    ] {
        integer_range(value, &format!("input.prompt.bounds.{name}"), 1, 1_000)?;
    }
    if !(1..=32).contains(&prompt.requirements.len()) {
        return schema_error("input.prompt.requirements must contain 1 to 32 items");
    }
    let mut total = 0_usize;
    let mut seen = HashSet::new();
    for requirement in &prompt.requirements {
        validate_text(requirement, "input.prompt.requirements[]", 1, 500, false)?;
        if requirement.trim() != requirement {
            return schema_error("input.prompt.requirements must be normalized");
        }
        total += requirement.len();
        if !seen.insert(requirement) {
            return schema_error("input.prompt.requirements contains a duplicate");
        }
    }
    if total > 8_192 {
        return schema_error("input.prompt.requirements exceeds 8192 UTF-8 bytes");
    }
    Ok(())
}

fn validate_identity(agent_id: &str, project_id: &str) -> Result<(), ProtocolError> {
    validate_identifier(agent_id, "agentId")?;
    validate_identifier(project_id, "projectId")
}

fn validate_identifier(value: &str, label: &str) -> Result<(), ProtocolError> {
    let bytes = value.as_bytes();
    if !(1..=128).contains(&bytes.len())
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return schema_error(&format!("{label} has an invalid format"));
    }
    Ok(())
}

fn validate_uuid(value: &str, label: &str) -> Result<(), ProtocolError> {
    let parsed = uuid::Uuid::parse_str(value)
        .map_err(|_| schema_error_value(&format!("{label} must be a lowercase UUID v4")))?;
    if parsed.get_version_num() != 4
        || parsed.get_variant() != uuid::Variant::RFC4122
        || parsed.hyphenated().to_string() != value
    {
        return schema_error(&format!("{label} must be a lowercase UUID v4"));
    }
    Ok(())
}

fn validate_path(
    value: &str,
    label: &str,
    prefix: &str,
    suffix: &str,
) -> Result<(), ProtocolError> {
    validate_text(value, label, 1, 512, false)?;
    if value.starts_with('/')
        || value.ends_with('/')
        || value.contains("//")
        || value.contains('\\')
        || value
            .chars()
            .any(|character| matches!(character, ':' | '"' | '<' | '>' | '|' | '?' | '*'))
        || value.split('/').any(|part| {
            part.is_empty()
                || matches!(part, "." | "..")
                || part.ends_with(' ')
                || part.ends_with('.')
        })
        || !value.starts_with(prefix)
        || !value.ends_with(suffix)
    {
        return schema_error(&format!("{label} is not a valid allowed relative path"));
    }
    Ok(())
}

fn validate_text(
    value: &str,
    label: &str,
    minimum: usize,
    maximum: usize,
    controls: bool,
) -> Result<(), ProtocolError> {
    if !(minimum..=maximum).contains(&value.len()) {
        return schema_error(&format!(
            "{label} must be {minimum} to {maximum} UTF-8 bytes"
        ));
    }
    if !controls
        && value
            .chars()
            .any(|character| character <= '\u{001f}' || character == '\u{007f}')
    {
        return schema_error(&format!("{label} contains a control character"));
    }
    Ok(())
}

fn integer_range(value: u64, label: &str, minimum: u64, maximum: u64) -> Result<(), ProtocolError> {
    if !(minimum..=maximum).contains(&value) {
        return schema_error(&format!(
            "{label} must be an integer between {minimum} and {maximum}"
        ));
    }
    Ok(())
}

fn schema_error<T>(message: &str) -> Result<T, ProtocolError> {
    Err(schema_error_value(message))
}

fn schema_error_value(message: &str) -> ProtocolError {
    ProtocolError::new("MYFORGE_MESSAGE_SCHEMA_INVALID", message)
}

fn limit_error<T>(message: &str) -> Result<T, ProtocolError> {
    Err(ProtocolError::new("MYFORGE_LIMIT_MISMATCH", message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_limits() -> ServerLimits {
        ServerLimits {
            auth_ttl_ms: 60_000,
            command_ttl_ms: 60_000,
            clock_skew_ms: 5_000,
            heartbeat_interval_ms: 15_000,
            heartbeat_timeout_ms: 45_000,
            command_timeout_ms: 120_000,
            cancel_timeout_ms: 10_000,
            max_output_bytes: 1_048_576,
            ws_max_message_bytes: 16_777_216,
        }
    }

    fn agent_limits() -> AgentLimits {
        AgentLimits {
            auth_ttl_ms: 60_000,
            command_ttl_ms: 60_000,
            clock_skew_ms: 5_000,
            heartbeat_interval_ms: 15_000,
            max_command_timeout_ms: 120_000,
            cancel_timeout_ms: 10_000,
            max_output_bytes: 1_048_576,
            ws_max_message_bytes: 16_777_216,
        }
    }

    #[test]
    fn negotiates_defaults_and_rejects_static_mismatches() {
        let effective = negotiate_limits(server_limits(), agent_limits()).unwrap();
        assert_eq!(effective.command_timeout_ms, 120_000);
        assert_eq!(effective.max_output_bytes, 1_048_576);

        let mut agent = agent_limits();
        agent.heartbeat_interval_ms = 14_000;
        assert_eq!(
            negotiate_limits(server_limits(), agent).unwrap_err().code(),
            "MYFORGE_LIMIT_MISMATCH"
        );

        let mut server = server_limits();
        server.ws_max_message_bytes = RESULT_FIXED_RESERVE_BYTES + 12 * 4_095;
        assert!(negotiate_limits(server, agent_limits()).is_err());
    }

    #[test]
    fn output_budget_is_reverse_constrained_by_frame_size() {
        let mut server = server_limits();
        server.ws_max_message_bytes = RESULT_FIXED_RESERVE_BYTES + 12 * 8_192;
        assert_eq!(
            negotiate_limits(server, agent_limits())
                .unwrap()
                .max_output_bytes,
            8_192
        );
    }

    #[test]
    fn rejects_invalid_server_limit_invariants() {
        let mut limits = server_limits();
        limits.heartbeat_timeout_ms = 20_000;
        assert_eq!(
            validate_server_limits(limits).unwrap_err().code(),
            "MYFORGE_LIMIT_MISMATCH"
        );

        let mut limits = server_limits();
        limits.clock_skew_ms = 30_000;
        assert!(validate_server_limits(limits).is_err());

        let mut limits = server_limits();
        limits.cancel_timeout_ms = 30_000;
        limits.command_timeout_ms = 20_000;
        assert!(validate_server_limits(limits).is_err());
    }

    #[test]
    fn validates_lowercase_uuid_v4_only() {
        validate_uuid("2d0465b1-dc92-46d2-bc45-c90ed9724f5a", "requestId").unwrap();
        assert!(validate_uuid("2D0465B1-DC92-46D2-BC45-C90ED9724F5A", "requestId").is_err());
        assert!(validate_uuid("2d0465b1-dc92-36d2-bc45-c90ed9724f5a", "requestId").is_err());
    }
}
