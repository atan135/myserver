import assert from "node:assert/strict";
import test from "node:test";

import { validateMessageSchema } from "./schemas.js";

const CONNECTION_ID = "67da7da9-a653-4d6e-9e81-f5f8baf874bb";
const REQUEST_ID = "2d0465b1-dc92-46d2-bc45-c90ed9724f5a";
const TIMESTAMP_MS = 1783694421000;
const NONCE = Buffer.alloc(16, 1).toString("base64url");
const SIGNATURE = Buffer.alloc(64, 2).toString("base64url");

const commandErrorCodes = [
  "MYFORGE_ROOT_MISSING",
  "MYFORGE_ROOT_INVALID",
  "MYFORGE_TARGET_PATH_INVALID",
  "MYFORGE_RULES_FILE_MISSING",
  "MYFORGE_CODEX_UNAVAILABLE",
  "MYFORGE_PROFILE_UNSUPPORTED",
  "MYFORGE_COMMAND_EXPIRED",
  "MYFORGE_COMMAND_SPAWN_FAILED"
];

const protocolErrorCodes = [
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
];

function envelope(type) {
  return {
    protocolVersion: 1,
    type,
    timestampMs: TIMESTAMP_MS,
    expiresAtMs: TIMESTAMP_MS + 60000,
    nonce: NONCE,
    signature: SIGNATURE
  };
}

function artifact(exists = true) {
  return exists
    ? { exists: true, sha256: "a".repeat(64), bytes: 42, modifiedAtMs: TIMESTAMP_MS + 1000 }
    : { exists: false, sha256: null, bytes: null, modifiedAtMs: null };
}

function audit(status, overrides = {}) {
  const values = {
    passed: {
      status: "passed", errors: 0, warnings: 0, primitiveCount: 3,
      mainCode: null, reasonCode: null, findingsPreview: []
    },
    unavailable: {
      status: "unavailable", errors: null, warnings: null, primitiveCount: null,
      mainCode: null, reasonCode: "auditor_not_configured", findingsPreview: []
    },
    warning: {
      status: "warning", errors: 0, warnings: 1, primitiveCount: 3,
      mainCode: "audit.warning", reasonCode: null,
      findingsPreview: [{ severity: "warning", code: "audit.warning", fieldPath: "root", message: "warning" }]
    },
    failed: {
      status: "failed", errors: 1, warnings: 0, primitiveCount: 3,
      mainCode: "audit.failed", reasonCode: null,
      findingsPreview: [{ severity: "error", code: "audit.failed", fieldPath: "root", message: "failed" }]
    },
    execution_failed: {
      status: "skipped", errors: null, warnings: null, primitiveCount: null,
      mainCode: null, reasonCode: "execution_failed", findingsPreview: []
    },
    artifact_missing: {
      status: "skipped", errors: null, warnings: null, primitiveCount: null,
      mainCode: null, reasonCode: "artifact_missing", findingsPreview: []
    },
    dry_run: {
      status: "skipped", errors: null, warnings: null, primitiveCount: null,
      mainCode: null, reasonCode: "dry_run", findingsPreview: []
    },
    cancelled: {
      status: "skipped", errors: null, warnings: null, primitiveCount: null,
      mainCode: null, reasonCode: "cancelled", findingsPreview: []
    }
  }[status];
  return { ...values, ...overrides };
}

function result(overrides = {}) {
  return {
    ...envelope("command.result"),
    connectionId: CONNECTION_ID,
    requestId: REQUEST_ID,
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    executionMode: "codex_exec",
    status: "completed",
    exitCode: 0,
    stdoutPreview: "ok",
    stderrPreview: "",
    stdoutBytes: 2,
    stderrBytes: 0,
    stdoutTruncated: false,
    stderrTruncated: false,
    artifactFile: "artifacts/fangyuan/home.ron",
    consumerTargetFile: null,
    artifact: artifact(true),
    audit: audit("passed"),
    errorCode: null,
    errorMessage: null,
    startedAtMs: TIMESTAMP_MS - 1000,
    completedAtMs: TIMESTAMP_MS,
    ...overrides
  };
}

test("command.result accepts every frozen result mapping", () => {
  const cases = [
    ["dry-run completed", result({
      executionMode: "dry_run", exitCode: null, artifact: artifact(false), audit: audit("dry_run")
    })],
    ["codex audit passed", result()],
    ["codex audit unavailable", result({ audit: audit("unavailable") })],
    ["codex audit warning", result({
      status: "completed_with_errors", audit: audit("warning"),
      errorCode: "FANGYUAN_BLUEPRINT_AUDIT_WARNING", errorMessage: "audit warning"
    })],
    ["codex audit failed", result({
      status: "completed_with_errors", audit: audit("failed"),
      errorCode: "FANGYUAN_BLUEPRINT_AUDIT_FAILED", errorMessage: "audit failed"
    })],
    ["command timeout", result({
      status: "failed", exitCode: null, audit: audit("execution_failed"),
      errorCode: "MYFORGE_COMMAND_TIMEOUT", errorMessage: "timed out"
    })],
    ["non-zero command exit", result({
      status: "failed", exitCode: 2, audit: audit("execution_failed"),
      errorCode: "MYFORGE_COMMAND_FAILED", errorMessage: "command failed"
    })],
    ["runtime error without exit", result({
      status: "failed", exitCode: null, audit: audit("execution_failed"),
      errorCode: "MYFORGE_COMMAND_FAILED", errorMessage: "runtime failed"
    })],
    ["oversized serialized result", result({
      status: "failed", exitCode: 0, audit: audit("execution_failed"),
      errorCode: "MYFORGE_OUTPUT_TOO_LARGE", errorMessage: "result too large"
    })],
    ["artifact missing", result({
      status: "failed", exitCode: 0, artifact: artifact(false), audit: audit("artifact_missing"),
      errorCode: "MYFORGE_TARGET_FILE_MISSING", errorMessage: "artifact missing"
    })],
    ["pre-start cancellation", result({
      status: "cancelled", exitCode: null, startedAtMs: null, artifact: artifact(false), audit: audit("cancelled"),
      errorCode: "MYFORGE_COMMAND_CANCELLED", errorMessage: "cancelled"
    })],
    ["post-start cancellation", result({
      status: "cancelled", exitCode: -1, artifact: artifact(false), audit: audit("cancelled"),
      errorCode: "MYFORGE_COMMAND_CANCELLED", errorMessage: "cancelled"
    })]
  ];

  for (const [name, message] of cases) {
    assert.doesNotThrow(() => validateMessageSchema(message), name);
  }
});

test("command.result rejects invalid dry-run, failure, and pre-start cancellation combinations", () => {
  const invalid = [
    result({ executionMode: "dry_run", exitCode: null, startedAtMs: null, artifact: artifact(false), audit: audit("dry_run") }),
    result({
      status: "failed", exitCode: 1, audit: audit("execution_failed"),
      errorCode: "MYFORGE_COMMAND_TIMEOUT", errorMessage: "bad timeout"
    }),
    result({
      status: "failed", exitCode: 0, audit: audit("execution_failed"),
      errorCode: "MYFORGE_COMMAND_FAILED", errorMessage: "bad exit"
    }),
    result({
      status: "failed", exitCode: null, audit: audit("execution_failed"),
      errorCode: "MYFORGE_ROOT_INVALID", errorMessage: "bad result code"
    }),
    result({
      status: "cancelled", exitCode: 1, startedAtMs: null, artifact: artifact(false), audit: audit("cancelled"),
      errorCode: "MYFORGE_COMMAND_CANCELLED", errorMessage: "bad cancellation"
    })
  ];
  for (const message of invalid) {
    assert.throws(() => validateMessageSchema(message), { code: "MYFORGE_MESSAGE_SCHEMA_INVALID" });
  }
});

test("command.error accepts only frozen pre-start local execution codes", () => {
  for (const errorCode of commandErrorCodes) {
    assert.doesNotThrow(() => validateMessageSchema({
      ...envelope("command.error"),
      connectionId: CONNECTION_ID,
      requestId: REQUEST_ID,
      agentId: "dev-pc-001",
      projectId: "myforge-local",
      errorCode,
      errorMessage: "pre-start failure",
      retryable: false
    }), errorCode);
  }
  assert.throws(() => validateMessageSchema({
    ...envelope("command.error"),
    connectionId: CONNECTION_ID,
    requestId: REQUEST_ID,
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    errorCode: "MYFORGE_COMMAND_TIMEOUT",
    errorMessage: "not a pre-start failure",
    retryable: false
  }), { code: "MYFORGE_MESSAGE_SCHEMA_INVALID" });
});

test("protocol.error accepts only frozen WebSocket protocol codes", () => {
  for (const errorCode of protocolErrorCodes) {
    assert.doesNotThrow(() => validateMessageSchema({
      ...envelope("protocol.error"),
      connectionId: CONNECTION_ID,
      agentId: "dev-pc-001",
      projectId: "myforge-local",
      requestId: null,
      errorCode,
      errorMessage: "protocol failure",
      fatal: true
    }), errorCode);
  }
  assert.throws(() => validateMessageSchema({
    ...envelope("protocol.error"),
    connectionId: CONNECTION_ID,
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    requestId: null,
    errorCode: "MYFORGE_CONFIG_INVALID",
    errorMessage: "not a protocol code",
    fatal: true
  }), { code: "MYFORGE_MESSAGE_SCHEMA_INVALID" });
});

test("agent.register requires a preflight-confirmed forge root", () => {
  const registration = {
    ...envelope("agent.register"),
    connectionId: CONNECTION_ID,
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    hostname: "DESKTOP-TEST",
    platform: "windows",
    agentVersion: "0.1.0",
    forgeRootSummary: { name: "myforge", configured: true },
    capabilities: {
      profiles: ["codex_exec"], codexExec: true, fangyuanBlueprint: true,
      audit: "unavailable", dryRun: false, maxConcurrentTasks: 1
    },
    limits: {
      authTtlMs: 60000, commandTtlMs: 60000, clockSkewMs: 5000,
      heartbeatIntervalMs: 15000, maxCommandTimeoutMs: 120000,
      cancelTimeoutMs: 10000, maxOutputBytes: 1048576, wsMaxMessageBytes: 16777216
    }
  };
  assert.doesNotThrow(() => validateMessageSchema(registration));
  assert.throws(() => validateMessageSchema({
    ...registration,
    forgeRootSummary: { name: "myforge", configured: false }
  }), { code: "MYFORGE_MESSAGE_SCHEMA_INVALID" });
});
