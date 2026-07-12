import {
  MYFORGE_PROTOCOL_VERSION,
  MyforgeProtocolError,
  isUuidV4,
  strictBase64UrlDecode
} from "./protocol.js";

const ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const ERROR_CODE_PATTERN = /^[A-Z][A-Z0-9_]{0,63}$/;
const LOWER_CODE_PATTERN = /^[a-z0-9][a-z0-9_.-]{0,63}$/;
const SHA256_PATTERN = /^[0-9a-f]{64}$/;
const CONTROL_PATTERN = /[\u0000-\u001f\u007f]/;
const EXECUTION_MODES = new Set(["codex_exec", "dry_run"]);
const RESULT_STATUSES = new Set(["completed", "completed_with_errors", "failed", "cancelled"]);
const AUDIT_STATUSES = new Set(["passed", "warning", "failed", "skipped", "unavailable"]);
const COMMAND_ERROR_CODES = new Set([
  "MYFORGE_ROOT_MISSING",
  "MYFORGE_ROOT_INVALID",
  "MYFORGE_TARGET_PATH_INVALID",
  "MYFORGE_RULES_FILE_MISSING",
  "MYFORGE_CODEX_UNAVAILABLE",
  "MYFORGE_PROFILE_UNSUPPORTED",
  "MYFORGE_COMMAND_EXPIRED",
  "MYFORGE_COMMAND_SPAWN_FAILED"
]);
const PROTOCOL_ERROR_CODES = new Set([
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
  "MYFORGE_OUTPUT_TOO_LARGE"
]);
const EXECUTION_FAILED_CODES = new Set([
  "MYFORGE_COMMAND_TIMEOUT",
  "MYFORGE_COMMAND_FAILED",
  "MYFORGE_OUTPUT_TOO_LARGE"
]);

function fail(message, code = "MYFORGE_MESSAGE_SCHEMA_INVALID", options = {}) {
  throw new MyforgeProtocolError(code, message, options);
}

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function exactObject(value, keys, label) {
  if (!isObject(value)) fail(`${label} must be an object`);
  const actual = Object.keys(value);
  const expected = new Set(keys);
  const missing = keys.find((key) => !Object.prototype.hasOwnProperty.call(value, key));
  if (missing) fail(`${label}.${missing} is required`);
  const unknown = actual.find((key) => !expected.has(key));
  if (unknown) fail(`${label}.${unknown} is not allowed`);
  if (actual.length !== keys.length) fail(`${label} has an invalid field set`);
  return value;
}

function integer(value, label, min = 0, max = Number.MAX_SAFE_INTEGER) {
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    fail(`${label} must be an integer between ${min} and ${max}`);
  }
}

function boolean(value, label) {
  if (typeof value !== "boolean") fail(`${label} must be a boolean`);
}

function string(value, label, { min = 0, max = Number.MAX_SAFE_INTEGER, controls = true, pattern = null } = {}) {
  if (typeof value !== "string") fail(`${label} must be a string`);
  const length = Buffer.byteLength(value, "utf8");
  if (length < min || length > max) fail(`${label} must be ${min} to ${max} UTF-8 bytes`);
  if (!controls && CONTROL_PATTERN.test(value)) fail(`${label} contains a control character`);
  if (pattern && !pattern.test(value)) fail(`${label} has an invalid format`);
}

function nullableString(value, label, options) {
  if (value !== null) string(value, label, options);
}

function uuid(value, label) {
  if (!isUuidV4(value)) fail(`${label} must be a lowercase UUID v4`);
}

function identity(message) {
  string(message.agentId, "agentId", { min: 1, max: 128, pattern: ID_PATTERN });
  string(message.projectId, "projectId", { min: 1, max: 128, pattern: ID_PATTERN });
}

function envelope(message, expectedType) {
  if (message.protocolVersion !== MYFORGE_PROTOCOL_VERSION) {
    fail("protocolVersion is unsupported", "MYFORGE_PROTOCOL_VERSION_UNSUPPORTED");
  }
  if (message.type !== expectedType) fail(`type must be ${expectedType}`);
  integer(message.timestampMs, "timestampMs");
  integer(message.expiresAtMs, "expiresAtMs");
  strictBase64UrlDecode(message.nonce, 16, "nonce");
  strictBase64UrlDecode(message.signature, 64, "signature");
}

function connectionIdentity(message) {
  uuid(message.connectionId, "connectionId");
  identity(message);
}

function requestIdentity(message) {
  connectionIdentity(message);
  uuid(message.requestId, "requestId");
}

function validateServerLimits(value, label = "limits") {
  exactObject(value, [
    "authTtlMs", "commandTtlMs", "clockSkewMs", "heartbeatIntervalMs",
    "heartbeatTimeoutMs", "commandTimeoutMs", "cancelTimeoutMs",
    "maxOutputBytes", "wsMaxMessageBytes"
  ], label);
  integer(value.authTtlMs, `${label}.authTtlMs`, 5000, 300000);
  integer(value.commandTtlMs, `${label}.commandTtlMs`, 5000, 300000);
  integer(value.clockSkewMs, `${label}.clockSkewMs`, 0, 30000);
  integer(value.heartbeatIntervalMs, `${label}.heartbeatIntervalMs`, 1000, 60000);
  integer(value.heartbeatTimeoutMs, `${label}.heartbeatTimeoutMs`, 3000, 180000);
  integer(value.commandTimeoutMs, `${label}.commandTimeoutMs`, 1000, 1800000);
  integer(value.cancelTimeoutMs, `${label}.cancelTimeoutMs`, 1000, 30000);
  integer(value.maxOutputBytes, `${label}.maxOutputBytes`, 4096, 4194304);
  integer(value.wsMaxMessageBytes, `${label}.wsMaxMessageBytes`, 524288, 33554432);
}

function validateAgentLimits(value, label = "limits") {
  exactObject(value, [
    "authTtlMs", "commandTtlMs", "clockSkewMs", "heartbeatIntervalMs",
    "maxCommandTimeoutMs", "cancelTimeoutMs", "maxOutputBytes", "wsMaxMessageBytes"
  ], label);
  integer(value.authTtlMs, `${label}.authTtlMs`, 5000, 300000);
  integer(value.commandTtlMs, `${label}.commandTtlMs`, 5000, 300000);
  integer(value.clockSkewMs, `${label}.clockSkewMs`, 0, 30000);
  integer(value.heartbeatIntervalMs, `${label}.heartbeatIntervalMs`, 1000, 60000);
  integer(value.maxCommandTimeoutMs, `${label}.maxCommandTimeoutMs`, 1000, 1800000);
  integer(value.cancelTimeoutMs, `${label}.cancelTimeoutMs`, 1000, 30000);
  integer(value.maxOutputBytes, `${label}.maxOutputBytes`, 4096, 4194304);
  integer(value.wsMaxMessageBytes, `${label}.wsMaxMessageBytes`, 524288, 33554432);
}

function validatePath(value, label, prefix, suffix, nullable = false) {
  if (nullable && value === null) return;
  string(value, label, { min: 1, max: 512, controls: false });
  if (value.startsWith("/") || value.endsWith("/") || value.includes("//") ||
      value.includes("\\") || /[:\"<>|?*]/.test(value)) {
    fail(`${label} is not a valid relative path`);
  }
  const segments = value.split("/");
  if (segments.some((part) => !part || part === "." || part === ".." || /[ .]$/.test(part))) {
    fail(`${label} is not a valid relative path`);
  }
  if (!value.startsWith(prefix) || !value.endsWith(suffix)) fail(`${label} is outside the allowed path`);
}

function validatePrompt(value) {
  exactObject(value, ["theme", "primitiveLimit", "bounds", "requirements"], "input.prompt");
  string(value.theme, "input.prompt.theme", { min: 1, max: 200, controls: false });
  if (value.theme.trim() !== value.theme) fail("input.prompt.theme must be normalized");
  integer(value.primitiveLimit, "input.prompt.primitiveLimit", 1, 1000);
  exactObject(value.bounds, ["width", "depth", "height"], "input.prompt.bounds");
  integer(value.bounds.width, "input.prompt.bounds.width", 1, 1000);
  integer(value.bounds.depth, "input.prompt.bounds.depth", 1, 1000);
  integer(value.bounds.height, "input.prompt.bounds.height", 1, 1000);
  if (!Array.isArray(value.requirements) || value.requirements.length < 1 || value.requirements.length > 32) {
    fail("input.prompt.requirements must contain 1 to 32 items");
  }
  let total = 0;
  const seen = new Set();
  for (const requirement of value.requirements) {
    string(requirement, "input.prompt.requirements[]", { min: 1, max: 500, controls: false });
    if (requirement.trim() !== requirement) fail("input.prompt.requirements must be normalized");
    total += Buffer.byteLength(requirement, "utf8");
    if (seen.has(requirement)) fail("input.prompt.requirements contains a duplicate");
    seen.add(requirement);
  }
  if (total > 8192) fail("input.prompt.requirements exceeds 8192 UTF-8 bytes");
}

function validateCapabilities(value) {
  exactObject(value, [
    "profiles", "codexExec", "fangyuanBlueprint", "audit", "dryRun", "dangerFullAccess",
    "maxConcurrentTasks"
  ], "capabilities");
  if (!Array.isArray(value.profiles) || value.profiles.length !== 1 || value.profiles[0] !== "codex_exec") {
    fail("capabilities.profiles must contain only codex_exec");
  }
  boolean(value.codexExec, "capabilities.codexExec");
  boolean(value.fangyuanBlueprint, "capabilities.fangyuanBlueprint");
  if (!value.fangyuanBlueprint) fail("capabilities.fangyuanBlueprint must be true");
  if (!new Set(["available", "unavailable"]).has(value.audit)) fail("capabilities.audit is invalid");
  boolean(value.dryRun, "capabilities.dryRun");
  boolean(value.dangerFullAccess, "capabilities.dangerFullAccess");
  if (!value.dryRun && !value.codexExec) fail("capabilities.codexExec must be true outside dry-run");
  if (value.maxConcurrentTasks !== 1) fail("capabilities.maxConcurrentTasks must be 1");
}

function validateArtifact(value) {
  exactObject(value, ["exists", "sha256", "bytes", "modifiedAtMs"], "artifact");
  boolean(value.exists, "artifact.exists");
  if (value.exists) {
    string(value.sha256, "artifact.sha256", { pattern: SHA256_PATTERN });
    integer(value.bytes, "artifact.bytes");
    integer(value.modifiedAtMs, "artifact.modifiedAtMs");
  } else if (value.sha256 !== null || value.bytes !== null || value.modifiedAtMs !== null) {
    fail("artifact fields must be null when exists is false");
  }
}

function validateAudit(value) {
  exactObject(value, [
    "status", "errors", "warnings", "primitiveCount", "mainCode", "reasonCode", "findingsPreview"
  ], "audit");
  if (!AUDIT_STATUSES.has(value.status)) fail("audit.status is invalid");
  if (!Array.isArray(value.findingsPreview) || value.findingsPreview.length > 20) {
    fail("audit.findingsPreview must have at most 20 entries");
  }
  for (const finding of value.findingsPreview) {
    exactObject(finding, ["severity", "code", "fieldPath", "message"], "audit.findingsPreview[]");
    if (!new Set(["info", "warning", "error"]).has(finding.severity)) fail("finding severity is invalid");
    string(finding.code, "finding.code", { pattern: LOWER_CODE_PATTERN });
    string(finding.fieldPath, "finding.fieldPath", { min: 1, max: 256, controls: false });
    string(finding.message, "finding.message", { min: 1, max: 512, controls: false });
  }
  if (new Set(["passed", "warning", "failed"]).has(value.status)) {
    integer(value.errors, "audit.errors");
    integer(value.warnings, "audit.warnings");
    if (value.primitiveCount !== null) integer(value.primitiveCount, "audit.primitiveCount");
    if (value.status === "passed") {
      if (value.mainCode !== null || value.reasonCode !== null) fail("passed audit has invalid codes");
    } else {
      string(value.mainCode, "audit.mainCode", { pattern: LOWER_CODE_PATTERN });
      if (value.reasonCode !== null || value.findingsPreview.length === 0) fail("warning/failed audit is incomplete");
    }
  } else {
    if (value.errors !== null || value.warnings !== null || value.primitiveCount !== null || value.mainCode !== null || value.findingsPreview.length !== 0) {
      fail("skipped/unavailable audit must use null counters and no findings");
    }
    const expected = value.status === "unavailable"
      ? "auditor_not_configured"
      : new Set(["dry_run", "execution_failed", "artifact_missing", "rules_not_provided", "cancelled"]);
    if (typeof expected === "string" ? value.reasonCode !== expected : !expected.has(value.reasonCode)) {
      fail("audit.reasonCode is invalid");
    }
  }
}

const COMMON = ["protocolVersion", "type", "timestampMs", "expiresAtMs", "nonce", "signature"];

const validators = {
  "server.challenge"(message) {
    exactObject(message, [...COMMON, "challengeId", "challenge", "agentId", "projectId", "limits"], "message");
    envelope(message, "server.challenge");
    uuid(message.challengeId, "challengeId");
    strictBase64UrlDecode(message.challenge, 32, "challenge");
    identity(message);
    validateServerLimits(message.limits);
  },

  "agent.hello"(message) {
    exactObject(message, [...COMMON, "challengeId", "challenge", "agentId", "projectId"], "message");
    envelope(message, "agent.hello");
    uuid(message.challengeId, "challengeId");
    strictBase64UrlDecode(message.challenge, 32, "challenge");
    identity(message);
  },

  "agent.register"(message) {
    exactObject(message, [
      ...COMMON, "connectionId", "agentId", "projectId", "hostname", "platform", "agentVersion",
      "forgeRootSummary", "capabilities", "limits"
    ], "message");
    envelope(message, "agent.register");
    connectionIdentity(message);
    string(message.hostname, "hostname", { min: 1, max: 255, controls: false });
    if (!new Set(["windows", "linux", "macos"]).has(message.platform)) fail("platform is invalid");
    string(message.agentVersion, "agentVersion", { min: 1, max: 64, controls: false });
    exactObject(message.forgeRootSummary, ["name", "configured"], "forgeRootSummary");
    string(message.forgeRootSummary.name, "forgeRootSummary.name", { min: 1, max: 128, controls: false });
    if (/[\\/:]/.test(message.forgeRootSummary.name)) fail("forgeRootSummary.name must not contain a path");
    boolean(message.forgeRootSummary.configured, "forgeRootSummary.configured");
    if (!message.forgeRootSummary.configured) fail("forgeRootSummary.configured must be true");
    validateCapabilities(message.capabilities);
    validateAgentLimits(message.limits);
  },

  "agent.heartbeat"(message) {
    exactObject(message, [...COMMON, "connectionId", "agentId", "projectId", "sequence", "state", "activeRequestId"], "message");
    envelope(message, "agent.heartbeat");
    connectionIdentity(message);
    integer(message.sequence, "sequence", 0, 2147483647);
    if (!new Set(["idle", "running"]).has(message.state)) fail("state is invalid");
    if (message.state === "idle" && message.activeRequestId !== null) fail("idle heartbeat must have a null activeRequestId");
    if (message.state === "running") uuid(message.activeRequestId, "activeRequestId");
  },

  "command.execute"(message) {
    exactObject(message, [
      ...COMMON, "connectionId", "requestId", "taskType", "agentId", "projectId", "profile", "input",
      "timeoutMs", "maxOutputBytes"
    ], "message");
    envelope(message, "command.execute");
    requestIdentity(message);
    if (message.taskType !== "fangyuan.blueprint.generate") fail("taskType is unsupported");
    if (message.profile !== "codex_exec") fail("profile is unsupported");
    exactObject(message.input, [
      "artifactFile", "consumerTargetFile", "rulesFile", "prompt", "renderedPrompt"
    ], "input");
    validatePath(message.input.artifactFile, "input.artifactFile", "artifacts/fangyuan/", ".ron");
    validatePath(message.input.consumerTargetFile, "input.consumerTargetFile", "project/assets/fangyuan/", ".ron", true);
    validatePath(message.input.rulesFile, "input.rulesFile", "rules/fangyuan/", ".md", true);
    validatePrompt(message.input.prompt);
    string(message.input.renderedPrompt, "input.renderedPrompt", { min: 1, max: 16384 });
    integer(message.timeoutMs, "timeoutMs", 1000, 1800000);
    integer(message.maxOutputBytes, "maxOutputBytes", 4096, 4194304);
  },

  "command.started"(message) {
    exactObject(message, [
      ...COMMON, "connectionId", "requestId", "agentId", "projectId", "executionMode", "startedAtMs"
    ], "message");
    envelope(message, "command.started");
    requestIdentity(message);
    if (!EXECUTION_MODES.has(message.executionMode)) fail("executionMode is invalid");
    integer(message.startedAtMs, "startedAtMs");
  },

  "command.cancel"(message) {
    exactObject(message, [
      ...COMMON, "connectionId", "requestId", "agentId", "projectId", "reasonCode",
      "cancelRequestedAtMs", "cancelDeadlineAtMs"
    ], "message");
    envelope(message, "command.cancel");
    requestIdentity(message);
    if (message.reasonCode !== "ADMIN_CANCELLED") fail("reasonCode is invalid");
    integer(message.cancelRequestedAtMs, "cancelRequestedAtMs");
    integer(message.cancelDeadlineAtMs, "cancelDeadlineAtMs");
    if (message.cancelDeadlineAtMs <= message.cancelRequestedAtMs ||
        message.timestampMs < message.cancelRequestedAtMs ||
        message.timestampMs >= message.cancelDeadlineAtMs ||
        message.expiresAtMs > message.cancelDeadlineAtMs) {
      fail("command.cancel timing fields are inconsistent");
    }
  },

  "command.result"(message) {
    exactObject(message, [
      ...COMMON, "connectionId", "requestId", "agentId", "projectId", "executionMode", "status", "exitCode",
      "stdoutPreview", "stderrPreview", "stdoutBytes", "stderrBytes", "stdoutTruncated", "stderrTruncated",
      "artifactFile", "consumerTargetFile", "artifact", "audit", "errorCode", "errorMessage", "startedAtMs",
      "completedAtMs"
    ], "message");
    envelope(message, "command.result");
    requestIdentity(message);
    if (!EXECUTION_MODES.has(message.executionMode)) fail("executionMode is invalid");
    if (!RESULT_STATUSES.has(message.status)) fail("status is invalid");
    if (message.exitCode !== null) integer(message.exitCode, "exitCode", -2147483648, 2147483647);
    string(message.stdoutPreview, "stdoutPreview");
    string(message.stderrPreview, "stderrPreview");
    integer(message.stdoutBytes, "stdoutBytes");
    integer(message.stderrBytes, "stderrBytes");
    if (message.stdoutBytes < Buffer.byteLength(message.stdoutPreview, "utf8") ||
        message.stderrBytes < Buffer.byteLength(message.stderrPreview, "utf8")) {
      fail("preview byte counts cannot exceed original output byte counts");
    }
    boolean(message.stdoutTruncated, "stdoutTruncated");
    boolean(message.stderrTruncated, "stderrTruncated");
    validatePath(message.artifactFile, "artifactFile", "artifacts/fangyuan/", ".ron");
    validatePath(message.consumerTargetFile, "consumerTargetFile", "project/assets/fangyuan/", ".ron", true);
    validateArtifact(message.artifact);
    validateAudit(message.audit);
    if (message.errorCode !== null) string(message.errorCode, "errorCode", { pattern: ERROR_CODE_PATTERN });
    nullableString(message.errorMessage, "errorMessage", { min: 1, max: 512, controls: false });
    if (message.startedAtMs !== null) integer(message.startedAtMs, "startedAtMs");
    integer(message.completedAtMs, "completedAtMs");
    if (message.status === "completed") {
      if (message.errorCode !== null || message.errorMessage !== null) fail("completed result must not contain an error");
    } else if (message.errorCode === null || message.errorMessage === null) {
      fail("non-completed result requires errorCode and errorMessage");
    }
    if (message.executionMode === "dry_run") {
      if (message.status === "completed" && (
        message.exitCode !== null || message.startedAtMs === null ||
        message.audit.status !== "skipped" || message.audit.reasonCode !== "dry_run"
      )) {
        fail("dry_run completed result fields are inconsistent");
      }
      if (!new Set(["completed", "cancelled"]).has(message.status)) {
        fail("dry_run result status is invalid");
      }
    }
    if (message.executionMode === "codex_exec" && message.status === "completed") {
      const auditAccepted = new Set(["passed", "unavailable"]).has(message.audit.status) ||
        (message.audit.status === "skipped" && message.audit.reasonCode === "rules_not_provided");
      if (message.exitCode !== 0 || message.startedAtMs === null || !message.artifact.exists ||
          !auditAccepted) {
        fail("completed codex_exec result fields are inconsistent");
      }
    }
    if (message.status === "completed_with_errors") {
      const expectedError = message.audit.status === "warning"
        ? "FANGYUAN_BLUEPRINT_AUDIT_WARNING"
        : message.audit.status === "failed"
          ? "FANGYUAN_BLUEPRINT_AUDIT_FAILED"
          : message.audit.status === "skipped" && message.audit.reasonCode === "artifact_missing"
            ? "MYFORGE_TARGET_FILE_MISSING"
            : null;
      const artifactConsistent = expectedError === "MYFORGE_TARGET_FILE_MISSING"
        ? !message.artifact.exists
        : message.artifact.exists;
      if (message.executionMode !== "codex_exec" || message.exitCode !== 0 || message.startedAtMs === null ||
          !artifactConsistent || expectedError === null || message.errorCode !== expectedError) {
        fail("completed_with_errors result fields are inconsistent");
      }
    }
    if (message.status === "failed") {
      if (message.startedAtMs === null || message.audit.status !== "skipped" ||
          message.audit.reasonCode !== "execution_failed") {
        fail("failed result fields are inconsistent");
      }
      if (!EXECUTION_FAILED_CODES.has(message.errorCode)) {
        fail("execution-failed result errorCode is invalid");
      }
      if (message.errorCode === "MYFORGE_COMMAND_TIMEOUT" && message.exitCode !== null) {
        fail("timeout result exitCode must be null");
      }
      if (message.errorCode === "MYFORGE_COMMAND_FAILED" && message.exitCode === 0) {
        fail("command-failed result exitCode must be null or non-zero");
      }
    }
    if (message.status === "cancelled" && (
      message.audit.status !== "skipped" || message.audit.reasonCode !== "cancelled" ||
      message.errorCode !== "MYFORGE_COMMAND_CANCELLED" ||
      (message.startedAtMs === null && message.exitCode !== null)
    )) {
      fail("cancelled result fields are inconsistent");
    }
  },

  "command.error"(message) {
    exactObject(message, [
      ...COMMON, "connectionId", "requestId", "agentId", "projectId", "errorCode", "errorMessage", "retryable"
    ], "message");
    envelope(message, "command.error");
    requestIdentity(message);
    string(message.errorCode, "errorCode", { pattern: ERROR_CODE_PATTERN });
    if (!COMMAND_ERROR_CODES.has(message.errorCode)) fail("command.error errorCode is not allowed");
    string(message.errorMessage, "errorMessage", { min: 1, max: 512, controls: false });
    boolean(message.retryable, "retryable");
  },

  "protocol.error"(message) {
    exactObject(message, [
      ...COMMON, "connectionId", "agentId", "projectId", "requestId", "errorCode", "errorMessage", "fatal"
    ], "message");
    envelope(message, "protocol.error");
    if (message.connectionId !== null) uuid(message.connectionId, "connectionId");
    identity(message);
    if (message.requestId !== null) uuid(message.requestId, "requestId");
    string(message.errorCode, "errorCode", { pattern: ERROR_CODE_PATTERN });
    if (!PROTOCOL_ERROR_CODES.has(message.errorCode)) fail("protocol.error errorCode is not allowed");
    string(message.errorMessage, "errorMessage", { min: 1, max: 512, controls: false });
    boolean(message.fatal, "fatal");
    if (!message.fatal) fail("P0 protocol.error must be fatal");
  }
};

export function validateMessageSchema(message, expectedType = null) {
  if (!isObject(message)) fail("message must be a JSON object");
  if (typeof message.type !== "string") fail("message.type is required");
  if (expectedType !== null && message.type !== expectedType) {
    fail(`expected ${expectedType}, received ${message.type}`, "MYFORGE_PROTOCOL_STATE_INVALID");
  }
  const validator = validators[message.type];
  if (!validator) fail("message.type is unsupported", "MYFORGE_MESSAGE_SCHEMA_INVALID");
  validator(message);
  return message;
}

export function validateServerLimitsSchema(limits) {
  validateServerLimits(limits);
  return limits;
}

export function validateAgentLimitsSchema(limits) {
  validateAgentLimits(limits);
  return limits;
}
