import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

import { MyforgeStore } from "./myforge-store.js";

class ScriptedClient {
  constructor(steps) {
    this.steps = [...steps];
    this.calls = [];
    this.released = false;
  }

  async query(sql, params = []) {
    const normalized = String(sql).replace(/\s+/g, " ").trim();
    this.calls.push({ sql: normalized, params });
    const step = this.steps.shift();
    assert.ok(step, `Unexpected query: ${normalized}`);
    assert.match(normalized, step.pattern);
    return typeof step.result === "function"
      ? step.result({ sql: normalized, params, calls: this.calls })
      : step.result ?? { rows: [], rowCount: 0 };
  }

  release() {
    this.released = true;
  }

  assertDone({ released = true } = {}) {
    assert.equal(this.steps.length, 0, `Expected ${this.steps.length} more queries`);
    assert.equal(this.released, released);
  }
}

function transactionClient(steps) {
  const client = new ScriptedClient([
    { pattern: /^BEGIN$/ },
    ...steps,
    { pattern: /^COMMIT$/ }
  ]);
  return {
    client,
    pool: {
      async connect() { return client; },
      async query(sql, params) { return client.query(sql, params); }
    }
  };
}

function rollbackTransactionClient(steps) {
  const client = new ScriptedClient([
    { pattern: /^BEGIN$/ },
    ...steps,
    { pattern: /^ROLLBACK$/ }
  ]);
  return {
    client,
    pool: {
      async connect() { return client; },
      async query(sql, params) { return client.query(sql, params); }
    }
  };
}

const NOW = new Date("2026-07-11T08:00:00.000Z");
const REQUEST_ID = "11111111-1111-4111-8111-111111111111";
const CONNECTION_ID = "22222222-2222-4222-8222-222222222222";
const CONNECTION_B_ID = "44444444-4444-4444-8444-444444444444";
const DIGEST_A = "a".repeat(64);
const DIGEST_B = "b".repeat(64);

function taskRow(overrides = {}) {
  return {
    request_id: REQUEST_ID,
    task_type: "fangyuan.blueprint.generate",
    project_id: "myforge-local",
    agent_id: "dev-pc-001",
    status: "queued",
    queue_reason: "agent_offline",
    execution_mode: null,
    connection_id: null,
    artifact_file: "artifacts/fangyuan/home.ron",
    consumer_target_file: "project/assets/fangyuan/home.ron",
    rules_file: "rules/fangyuan/rules.md",
    prompt_json: { theme: "home" },
    rendered_prompt: "sensitive rendered prompt",
    command_preview: "codex exec [prompt]",
    command_digest: null,
    command_expires_at: null,
    timeout_ms: 120000,
    max_output_bytes: 1048576,
    stdout_preview: null,
    stderr_preview: null,
    stdout_bytes: null,
    stderr_bytes: null,
    stdout_truncated: false,
    stderr_truncated: false,
    exit_code: null,
    artifact_json: null,
    audit_json: null,
    result_digest: null,
    error_code: null,
    error_message: null,
    created_by_admin_id: 7,
    created_by_admin_username: "operator",
    created_at: NOW,
    queue_expires_at: new Date(NOW.getTime() + 900000),
    dispatched_at: null,
    started_at: null,
    cancel_requested_at: null,
    cancel_deadline_at: null,
    completed_at: null,
    updated_at: NOW,
    ...overrides
  };
}

function agentRow(overrides = {}) {
  return {
    agent_id: "dev-pc-001",
    project_id: "myforge-local",
    label: "Development PC",
    public_key_fingerprint: "c".repeat(64),
    configured: true,
    status: "offline",
    hostname: null,
    platform: null,
    agent_version: null,
    forge_root_summary_json: null,
    capabilities_json: null,
    limits_json: null,
    effective_limits_json: null,
    connection_id: null,
    last_registered_at: null,
    connected_at: null,
    last_seen_at: new Date("2026-07-10T08:00:00.000Z"),
    disconnected_at: null,
    created_at: NOW,
    updated_at: NOW,
    ...overrides
  };
}

test("myforge startup sync is transactional and fails orphaned work with audit records", async () => {
  const restarted = taskRow({
    status: "failed",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_A,
    command_expires_at: NOW,
    dispatched_at: NOW,
    completed_at: NOW,
    error_code: "MYFORGE_SERVER_RESTARTED"
  });
  const cancelUnconfirmed = taskRow({
    request_id: "33333333-3333-4333-8333-333333333333",
    status: "failed",
    queue_reason: null,
    execution_mode: "dry_run",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_B,
    command_expires_at: NOW,
    dispatched_at: NOW,
    cancel_requested_at: NOW,
    cancel_deadline_at: new Date(NOW.getTime() + 10000),
    completed_at: NOW,
    error_code: "MYFORGE_CANCEL_UNCONFIRMED"
  });
  const { client, pool } = transactionClient([
    { pattern: /^UPDATE myforge_agents SET configured = false/ },
    {
      pattern: /^INSERT INTO myforge_agents/,
      result: ({ params }) => {
        assert.deepEqual(params, ["dev-pc-001", "myforge-local", "Development PC", "c".repeat(64)]);
        return { rows: [], rowCount: 1 };
      }
    },
    {
      pattern: /^UPDATE myforge_task_runs SET status = 'failed'/,
      result: { rows: [restarted, cancelUnconfirmed], rowCount: 2 }
    },
    { pattern: /^INSERT INTO admin_audit_logs/ },
    { pattern: /^INSERT INTO admin_audit_logs/ }
  ]);
  const store = new MyforgeStore(pool);
  const result = await store.initializeKnownAgents([{
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    label: "Development PC",
    publicKeyFingerprint: "c".repeat(64),
    publicKey: { mustNotReachSql: true }
  }]);

  assert.deepEqual(result, { configuredAgents: 1, failedTasks: 2 });
  assert.match(client.calls[1].sql, /connection_id = NULL/);
  assert.match(client.calls[2].sql, /connection_id = NULL/);
  assert.doesNotMatch(client.calls[1].sql, /last_seen_at\s*=/);
  assert.match(client.calls[3].sql, /MYFORGE_SERVER_RESTARTED/);
  assert.match(client.calls[3].sql, /MYFORGE_CANCEL_UNCONFIRMED/);
  client.assertDone();
});

test("myforge createTask derives offline queueing and writes a redacted audit in one transaction", async () => {
  let insertedRequestId;
  let auditJson;
  const { client, pool } = transactionClient([
    {
      pattern: /^SELECT agent_id, project_id, status FROM myforge_agents/,
      result: { rows: [{ agent_id: "dev-pc-001", project_id: "myforge-local", status: "offline" }] }
    },
    {
      pattern: /^INSERT INTO myforge_task_runs/,
      result: ({ params }) => {
        insertedRequestId = params[0];
        assert.match(insertedRequestId, /^[0-9a-f-]{36}$/);
        assert.equal(params[4], "agent_offline");
        assert.equal(params[9], "sensitive rendered prompt");
        return {
          rows: [taskRow({ request_id: insertedRequestId })],
          rowCount: 1
        };
      }
    },
    {
      pattern: /^INSERT INTO admin_audit_logs/,
      result: ({ params }) => {
        auditJson = params[4];
        assert.equal(params[0], 7);
        assert.equal(params[1], "operator");
        assert.equal(params[2], "myforge_task_create");
        assert.equal(params[3], insertedRequestId);
        return { rows: [], rowCount: 1 };
      }
    }
  ]);
  const store = new MyforgeStore(pool, {
    commandTimeoutMs: 120000,
    maxOutputBytes: 1048576,
    queueTtlMs: 900000
  });
  const task = await store.createTask({
    projectId: "myforge-local",
    agentId: "dev-pc-001",
    artifactFile: "artifacts/fangyuan/home.ron",
    consumerTargetFile: "project/assets/fangyuan/home.ron",
    rulesFile: "rules/fangyuan/rules.md",
    prompt: { theme: "home" },
    renderedPrompt: "sensitive rendered prompt",
    commandPreview: "codex exec [prompt]",
    createdByAdminId: 7,
    createdByAdminUsername: "operator",
    now: NOW
  });

  assert.equal(task.requestId, insertedRequestId);
  assert.equal(task.queueReason, "agent_offline");
  assert.doesNotMatch(auditJson, /sensitive rendered prompt/);
  assert.doesNotMatch(auditJson, /stdout|stderr/i);
  const details = JSON.parse(auditJson);
  assert.equal(details.paths.artifact.name, "home.ron");
  assert.match(details.paths.artifact.sha256, /^[0-9a-f]{64}$/);
  client.assertDone();
});

test("myforge agent registration verifies configured identity before updating runtime fields", async () => {
  const online = agentRow({
    status: "online",
    connection_id: CONNECTION_ID,
    hostname: "devbox",
    platform: "windows",
    agent_version: "0.1.0",
    capabilities_json: { dryRun: true },
    limits_json: { authTtlMs: 60000 },
    effective_limits_json: { authTtlMs: 60000 },
    last_registered_at: NOW,
    connected_at: NOW,
    last_seen_at: NOW
  });
  const { client, pool } = transactionClient([
    { pattern: /^SELECT \* FROM myforge_agents/, result: { rows: [agentRow()] } },
    {
      pattern: /^UPDATE myforge_agents SET status = 'online'/,
      result: ({ params }) => {
        assert.equal(params[0], "dev-pc-001");
        assert.equal(params[4], JSON.stringify({ rootName: "myforge" }));
        assert.equal(params[5], JSON.stringify({ dryRun: true }));
        assert.equal(params[6], JSON.stringify({ authTtlMs: 60000 }));
        assert.equal(params[9], CONNECTION_ID);
        return { rows: [online], rowCount: 1 };
      }
    }
  ]);
  const store = new MyforgeStore(pool);
  const registration = await store.registerAgent({
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    publicKeyFingerprint: "c".repeat(64),
    hostname: "devbox",
    platform: "windows",
    agentVersion: "0.1.0",
    forgeRootSummary: { rootName: "myforge" },
    capabilities: { dryRun: true },
    limits: { authTtlMs: 60000 },
    effectiveLimits: { authTtlMs: 60000 },
    connectionId: CONNECTION_ID,
    registeredAt: NOW
  });
  assert.equal(registration.agent.status, "online");
  assert.deepEqual(registration.agent.capabilities, { dryRun: true });
  assert.equal(registration.replacedConnectionId, null);
  assert.equal("connectionId" in registration.agent, false);
  client.assertDone();

  const mismatch = rollbackTransactionClient([
    { pattern: /^SELECT \* FROM myforge_agents/, result: { rows: [agentRow()] } }
  ]);
  await assert.rejects(
    new MyforgeStore(mismatch.pool).registerAgent({
      agentId: "dev-pc-001",
      projectId: "another-project",
      publicKeyFingerprint: "c".repeat(64),
      connectionId: CONNECTION_ID
    }),
    (error) => error.code === "MYFORGE_IDENTITY_MISMATCH"
  );
  mismatch.client.assertDone();
});

test("myforge replaced connection rejects stale heartbeat, dispatch and offline state overwrite", async () => {
  const offline = agentRow();
  const onlineA = agentRow({
    status: "online",
    connection_id: CONNECTION_ID,
    last_registered_at: NOW,
    connected_at: NOW,
    last_seen_at: NOW
  });
  const onlineB = agentRow({
    status: "online",
    connection_id: CONNECTION_B_ID,
    last_registered_at: new Date(NOW.getTime() + 1000),
    connected_at: new Date(NOW.getTime() + 1000),
    last_seen_at: new Date(NOW.getTime() + 1000)
  });
  const failedA = taskRow({
    status: "failed",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_A,
    command_expires_at: NOW,
    dispatched_at: NOW,
    error_code: "MYFORGE_AGENT_DISCONNECTED",
    completed_at: new Date(NOW.getTime() + 2000)
  });
  const client = new ScriptedClient([
    { pattern: /^BEGIN$/ },
    { pattern: /^SELECT \* FROM myforge_agents/, result: { rows: [offline] } },
    {
      pattern: /^UPDATE myforge_agents SET status = 'online'/,
      result: ({ sql, params }) => {
        assert.match(sql, /connection_id = \$10/);
        assert.equal(params[9], CONNECTION_ID);
        return { rows: [onlineA], rowCount: 1 };
      }
    },
    { pattern: /^COMMIT$/ },
    { pattern: /^BEGIN$/ },
    { pattern: /^SELECT \* FROM myforge_agents/, result: { rows: [onlineA] } },
    {
      pattern: /^UPDATE myforge_agents SET status = 'online'/,
      result: ({ params }) => {
        assert.equal(params[9], CONNECTION_B_ID);
        return { rows: [onlineB], rowCount: 1 };
      }
    },
    { pattern: /^COMMIT$/ },
    {
      pattern: /^UPDATE myforge_agents SET last_seen_at = \$4/,
      result: ({ sql, params }) => {
        assert.match(sql, /connection_id = \$3/);
        assert.deepEqual(params.slice(0, 3), ["dev-pc-001", "myforge-local", CONNECTION_ID]);
        return { rows: [], rowCount: 0 };
      }
    },
    { pattern: /^BEGIN$/ },
    {
      pattern: /^SELECT agent_id FROM myforge_agents/,
      result: ({ sql, params }) => {
        assert.match(sql, /connection_id = \$3/);
        assert.deepEqual(params, ["dev-pc-001", "myforge-local", CONNECTION_ID]);
        return { rows: [], rowCount: 0 };
      }
    },
    { pattern: /^COMMIT$/ },
    { pattern: /^BEGIN$/ },
    {
      pattern: /^UPDATE myforge_agents SET status = 'offline'/,
      result: ({ sql, params }) => {
        assert.match(sql, /WHERE agent_id = \$1 AND connection_id = \$3/);
        assert.equal(params[2], CONNECTION_ID);
        return { rows: [], rowCount: 0 };
      }
    },
    {
      pattern: /^UPDATE myforge_task_runs SET status = 'failed'/,
      result: ({ sql, params }) => {
        assert.match(sql, /AND connection_id = \$3/);
        assert.equal(params[2], CONNECTION_ID);
        return { rows: [failedA], rowCount: 1 };
      }
    },
    { pattern: /^INSERT INTO admin_audit_logs/ },
    { pattern: /^COMMIT$/ },
    { pattern: /^SELECT \* FROM myforge_agents/, result: { rows: [onlineB] } }
  ]);
  const pool = {
    async connect() { return client; },
    async query(sql, params) { return client.query(sql, params); }
  };
  const store = new MyforgeStore(pool);

  const registrationA = await store.registerAgent({
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    publicKeyFingerprint: "c".repeat(64),
    connectionId: CONNECTION_ID,
    registeredAt: NOW
  });
  assert.equal(registrationA.replacedConnectionId, null);

  const registrationB = await store.registerAgent({
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    publicKeyFingerprint: "c".repeat(64),
    connectionId: CONNECTION_B_ID,
    registeredAt: new Date(NOW.getTime() + 1000)
  });
  assert.equal(registrationB.replacedConnectionId, CONNECTION_ID);
  assert.equal("connectionId" in registrationB.agent, false);

  const staleHeartbeat = await store.heartbeatAgent({
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    connectionId: CONNECTION_ID,
    seenAt: new Date(NOW.getTime() + 1500)
  });
  assert.deepEqual(staleHeartbeat, { agent: null, staleConnection: true });

  const staleClaim = await store.claimTaskDispatched({
    requestId: REQUEST_ID,
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    connectionId: CONNECTION_ID,
    executionMode: "codex_exec",
    commandDigest: DIGEST_A,
    commandExpiresAt: new Date(NOW.getTime() + 60000),
    timeoutMs: 120000,
    maxOutputBytes: 1048576,
    dispatchedAt: new Date(NOW.getTime() + 1600)
  });
  assert.equal(staleClaim, null);

  const staleClose = await store.markAgentOffline({
    agentId: "dev-pc-001",
    connectionId: CONNECTION_ID,
    disconnectedAt: new Date(NOW.getTime() + 2000)
  });
  assert.equal(staleClose.agent, null);
  assert.equal(staleClose.staleConnection, true);
  assert.equal(staleClose.failedTasks.length, 1);
  assert.equal(staleClose.failedTasks[0].connectionId, CONNECTION_ID);

  const current = await store.getAgent("dev-pc-001");
  assert.equal(current.status, "online");
  assert.equal("connectionId" in current, false);
  client.assertDone();
});

test("myforge dispatch claim is conditional, records negotiated fields and audits atomically", async () => {
  const dispatched = taskRow({
    status: "dispatched",
    queue_reason: null,
    execution_mode: "dry_run",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_A,
    command_expires_at: new Date(NOW.getTime() + 60000),
    dispatched_at: NOW
  });
  const { client, pool } = transactionClient([
    {
      pattern: /^SELECT agent_id FROM myforge_agents/,
      result: ({ sql, params }) => {
        assert.match(sql, /connection_id = \$3/);
        assert.match(sql, /configured = true/);
        assert.match(sql, /status = 'online'/);
        assert.match(sql, /FOR SHARE/);
        assert.deepEqual(params, ["dev-pc-001", "myforge-local", CONNECTION_ID]);
        return { rows: [{ agent_id: "dev-pc-001" }] };
      }
    },
    {
      pattern: /^UPDATE myforge_task_runs task SET status = 'dispatched'/,
      result: ({ sql, params }) => {
        assert.match(sql, /status = 'queued'/);
        assert.match(sql, /current_agent\.connection_id = \$4/);
        assert.match(sql, /NOT EXISTS/);
        assert.equal(params[3], CONNECTION_ID);
        assert.equal(params[4], "dry_run");
        assert.equal(params[5], DIGEST_A);
        return { rows: [dispatched], rowCount: 1 };
      }
    },
    {
      pattern: /^INSERT INTO admin_audit_logs/,
      result: ({ params }) => {
        assert.equal(params[2], "myforge_task_dispatch");
        return { rows: [], rowCount: 1 };
      }
    }
  ]);
  const store = new MyforgeStore(pool);
  const task = await store.claimTaskDispatched({
    requestId: REQUEST_ID,
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    connectionId: CONNECTION_ID,
    executionMode: "dry_run",
    commandDigest: DIGEST_A,
    commandExpiresAt: new Date(NOW.getTime() + 60000),
    timeoutMs: 120000,
    maxOutputBytes: 1048576,
    dispatchedAt: NOW
  });
  assert.equal(task.status, "dispatched");
  assert.equal(task.executionMode, "dry_run");
  client.assertDone();
});

test("myforge duplicate started message is idempotent and does not duplicate audit", async () => {
  const running = taskRow({
    status: "running",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_A,
    command_expires_at: new Date(NOW.getTime() + 60000),
    dispatched_at: NOW,
    started_at: new Date(NOW.getTime() + 100)
  });
  const { client, pool } = transactionClient([
    { pattern: /^SELECT \* FROM myforge_task_runs/, result: { rows: [running] } }
  ]);
  const store = new MyforgeStore(pool);
  const result = await store.markTaskStarted({
    requestId: REQUEST_ID,
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    connectionId: CONNECTION_ID,
    startedAt: new Date(NOW.getTime() + 100)
  });
  assert.equal(result.outcome, "duplicate");
  client.assertDone();
});

test("myforge started linearizes after cancellation request and preserves cancellation fields", async () => {
  const cancelRequestedAt = new Date(NOW.getTime() + 50);
  const cancelDeadlineAt = new Date(NOW.getTime() + 10050);
  const dispatched = taskRow({
    status: "dispatched",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_A,
    command_expires_at: new Date(NOW.getTime() + 60000),
    dispatched_at: NOW,
    started_at: null,
    cancel_requested_at: cancelRequestedAt,
    cancel_deadline_at: cancelDeadlineAt
  });
  const running = {
    ...dispatched,
    status: "running",
    started_at: new Date(NOW.getTime() + 100)
  };
  const { client, pool } = transactionClient([
    { pattern: /^SELECT \* FROM myforge_task_runs/, result: { rows: [dispatched] } },
    {
      pattern: /^UPDATE myforge_task_runs SET status = 'running'/,
      result: ({ sql, params }) => {
        assert.match(sql, /WHERE request_id = \$1 AND status = 'dispatched'/);
        assert.doesNotMatch(sql, /cancel_requested_at\s*=/);
        assert.doesNotMatch(sql, /cancel_deadline_at\s*=/);
        assert.deepEqual(params, [REQUEST_ID, new Date(NOW.getTime() + 100)]);
        return { rows: [running], rowCount: 1 };
      }
    },
    { pattern: /^INSERT INTO admin_audit_logs/ }
  ]);
  const store = new MyforgeStore(pool);
  const result = await store.markTaskStarted({
    requestId: REQUEST_ID,
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    connectionId: CONNECTION_ID,
    executionMode: "codex_exec",
    startedAt: new Date(NOW.getTime() + 100)
  });
  assert.equal(result.outcome, "updated");
  assert.equal(result.task.status, "running");
  assert.equal(result.task.cancelRequestedAt, cancelRequestedAt.toISOString());
  assert.equal(result.task.cancelDeadlineAt, cancelDeadlineAt.toISOString());
  client.assertDone();
});

test("myforge started remains invalid after terminal transition", async () => {
  const terminal = taskRow({
    status: "cancelled",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_A,
    command_expires_at: new Date(NOW.getTime() + 60000),
    dispatched_at: NOW,
    started_at: null,
    cancel_requested_at: new Date(NOW.getTime() + 50),
    cancel_deadline_at: new Date(NOW.getTime() + 10050),
    completed_at: new Date(NOW.getTime() + 200)
  });
  const { client, pool } = transactionClient([
    { pattern: /^SELECT \* FROM myforge_task_runs/, result: { rows: [terminal] } }
  ]);
  const store = new MyforgeStore(pool);
  await assert.rejects(
    store.markTaskStarted({
      requestId: REQUEST_ID,
      agentId: "dev-pc-001",
      projectId: "myforge-local",
      connectionId: CONNECTION_ID,
      executionMode: "codex_exec",
      startedAt: new Date(NOW.getTime() + 100)
    }),
    (error) => error.code === "MYFORGE_PROTOCOL_STATE_INVALID"
  );
  client.assertDone();
});

test("myforge first terminal result wins and lifecycle audit excludes output content", async () => {
  const running = taskRow({
    status: "running",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_A,
    command_expires_at: new Date(NOW.getTime() + 60000),
    dispatched_at: NOW,
    started_at: NOW
  });
  const completed = {
    ...running,
    status: "completed",
    stdout_preview: "secret stdout",
    stderr_preview: "secret stderr",
    stdout_bytes: 13,
    stderr_bytes: 13,
    result_digest: DIGEST_B,
    completed_at: new Date(NOW.getTime() + 1000)
  };
  let auditJson;
  const { client, pool } = transactionClient([
    { pattern: /^SELECT \* FROM myforge_task_runs/, result: { rows: [running] } },
    { pattern: /^UPDATE myforge_task_runs SET status = \$2/, result: { rows: [completed], rowCount: 1 } },
    {
      pattern: /^INSERT INTO admin_audit_logs/,
      result: ({ params }) => {
        auditJson = params[4];
        assert.equal(params[2], "myforge_task_complete");
        return { rows: [], rowCount: 1 };
      }
    }
  ]);
  const store = new MyforgeStore(pool);
  const result = await store.recordTaskResult({
    requestId: REQUEST_ID,
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    connectionId: CONNECTION_ID,
    executionMode: "codex_exec",
    status: "completed",
    resultDigest: DIGEST_B,
    stdoutPreview: "secret stdout",
    stderrPreview: "secret stderr",
    stdoutBytes: 13,
    stderrBytes: 13,
    completedAt: new Date(NOW.getTime() + 1000),
    artifactFile: "artifacts/fangyuan/home.ron",
    consumerTargetFile: "project/assets/fangyuan/home.ron"
  });
  assert.equal(result.outcome, "updated");
  assert.doesNotMatch(auditJson, /secret stdout|secret stderr|sensitive rendered prompt/);
  client.assertDone();

  const conflict = rollbackTransactionClient([
    { pattern: /^SELECT \* FROM myforge_task_runs/, result: { rows: [completed] } }
  ]);
  const conflictStore = new MyforgeStore(conflict.pool);
  await assert.rejects(
    conflictStore.recordTaskResult({
      requestId: REQUEST_ID,
      status: "completed",
      resultDigest: DIGEST_A
    }),
    (error) => error.code === "MYFORGE_DUPLICATE_RESULT_CONFLICT"
  );
  conflict.client.assertDone();
});

test("myforge command error and queue expiry write failed state with lifecycle audit", async () => {
  const dispatched = taskRow({
    status: "dispatched",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_A,
    command_expires_at: new Date(NOW.getTime() + 60000),
    dispatched_at: NOW
  });
  const commandFailed = {
    ...dispatched,
    status: "failed",
    error_code: "MYFORGE_TARGET_PATH_INVALID",
    error_message: "Target path is invalid",
    completed_at: new Date(NOW.getTime() + 1000)
  };
  const errorTx = transactionClient([
    { pattern: /^SELECT \* FROM myforge_task_runs/, result: { rows: [dispatched] } },
    {
      pattern: /^UPDATE myforge_task_runs SET status = 'failed'/,
      result: ({ params }) => {
        assert.equal(params[1], "MYFORGE_TARGET_PATH_INVALID");
        return { rows: [commandFailed], rowCount: 1 };
      }
    },
    {
      pattern: /^INSERT INTO admin_audit_logs/,
      result: ({ params }) => {
        assert.equal(params[2], "myforge_task_fail");
        return { rows: [], rowCount: 1 };
      }
    }
  ]);
  const errorResult = await new MyforgeStore(errorTx.pool).recordTaskError({
    requestId: REQUEST_ID,
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    connectionId: CONNECTION_ID,
    errorCode: "MYFORGE_TARGET_PATH_INVALID",
    errorMessage: "Target path is invalid",
    completedAt: new Date(NOW.getTime() + 1000)
  });
  assert.equal(errorResult.outcome, "updated");
  assert.equal(errorResult.task.errorCode, "MYFORGE_TARGET_PATH_INVALID");
  errorTx.client.assertDone();

  const expired = taskRow({
    status: "failed",
    queue_reason: null,
    error_code: "MYFORGE_QUEUE_EXPIRED",
    error_message: "Task expired while waiting for an agent",
    completed_at: NOW
  });
  const expiryTx = transactionClient([
    {
      pattern: /^UPDATE myforge_task_runs SET status = 'failed'/,
      result: ({ sql, params }) => {
        assert.match(sql, /status = 'queued' AND queue_expires_at <= \$1/);
        assert.equal(params[0], NOW);
        return { rows: [expired], rowCount: 1 };
      }
    },
    { pattern: /^INSERT INTO admin_audit_logs/ }
  ]);
  const expiredTasks = await new MyforgeStore(expiryTx.pool).failExpiredQueuedTasks(NOW);
  assert.equal(expiredTasks.length, 1);
  assert.equal(expiredTasks[0].errorCode, "MYFORGE_QUEUE_EXPIRED");
  expiryTx.client.assertDone();
});

test("myforge active cancellation persists one deadline and reserves later failures for cancel paths", async () => {
  const dispatched = taskRow({
    status: "dispatched",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_A,
    command_expires_at: new Date(NOW.getTime() + 60000),
    dispatched_at: NOW
  });
  const pending = {
    ...dispatched,
    cancel_requested_at: NOW,
    cancel_deadline_at: new Date(NOW.getTime() + 10000)
  };
  const requested = transactionClient([
    { pattern: /^SELECT \* FROM myforge_task_runs/, result: { rows: [dispatched] } },
    {
      pattern: /^UPDATE myforge_task_runs SET cancel_requested_at = \$2/,
      result: ({ params }) => {
        assert.equal(params[1], NOW);
        assert.equal(params[2].toISOString(), "2026-07-11T08:00:10.000Z");
        return { rows: [pending], rowCount: 1 };
      }
    },
    { pattern: /^INSERT INTO admin_audit_logs/ }
  ]);
  const store = new MyforgeStore(requested.pool, { cancelTimeoutMs: 10000 });
  const requestResult = await store.requestTaskCancellation({
    requestId: REQUEST_ID,
    adminId: 8,
    adminUsername: "canceller",
    requestedAt: NOW
  });
  assert.equal(requestResult.outcome, "requested");
  assert.equal(requestResult.sendCancel, true);
  assert.equal(requestResult.task.cancelDeadlineAt, "2026-07-11T08:00:10.000Z");
  requested.client.assertDone();

  const invalidFailure = rollbackTransactionClient([
    { pattern: /^SELECT \* FROM myforge_task_runs/, result: { rows: [pending] } }
  ]);
  await assert.rejects(
    new MyforgeStore(invalidFailure.pool).failTask({
      requestId: REQUEST_ID,
      expectedStatuses: ["dispatched"],
      errorCode: "MYFORGE_COMMAND_TIMEOUT",
      errorMessage: "ordinary timeout"
    }),
    (error) => error.code === "MYFORGE_PROTOCOL_STATE_INVALID"
  );
  invalidFailure.client.assertDone();

  const cancelledFailure = {
    ...pending,
    status: "failed",
    error_code: "MYFORGE_CANCEL_TIMEOUT",
    error_message: "Cancellation timed out",
    completed_at: new Date(NOW.getTime() + 15000)
  };
  const validFailure = transactionClient([
    { pattern: /^SELECT \* FROM myforge_task_runs/, result: { rows: [pending] } },
    { pattern: /^UPDATE myforge_task_runs SET status = 'failed'/, result: { rows: [cancelledFailure] } },
    { pattern: /^INSERT INTO admin_audit_logs/ }
  ]);
  const failResult = await new MyforgeStore(validFailure.pool).failTask({
    requestId: REQUEST_ID,
    expectedStatuses: ["dispatched"],
    errorCode: "MYFORGE_CANCEL_TIMEOUT",
    errorMessage: "Cancellation timed out",
    completedAt: new Date(NOW.getTime() + 15000)
  });
  assert.equal(failResult.outcome, "updated");
  assert.equal(failResult.task.errorCode, "MYFORGE_CANCEL_TIMEOUT");
  validFailure.client.assertDone();
});

test("myforge agent offline transition fails only the selected connection and audits each task", async () => {
  const failed = taskRow({
    status: "failed",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_A,
    command_expires_at: NOW,
    dispatched_at: NOW,
    error_code: "MYFORGE_AGENT_DISCONNECTED",
    completed_at: NOW
  });
  const { client, pool } = transactionClient([
    {
      pattern: /^UPDATE myforge_agents SET status = 'offline'/,
      result: ({ sql, params }) => {
        assert.match(sql, /connection_id = NULL/);
        assert.match(sql, /WHERE agent_id = \$1 AND connection_id = \$3/);
        assert.equal(params[2], CONNECTION_ID);
        return { rows: [agentRow()], rowCount: 1 };
      }
    },
    {
      pattern: /^UPDATE myforge_task_runs SET status = 'failed'/,
      result: ({ sql, params }) => {
        assert.match(sql, /connection_id = \$3/);
        assert.equal(params[2], CONNECTION_ID);
        assert.equal(params[3], "agent_disconnected");
        return { rows: [failed], rowCount: 1 };
      }
    },
    { pattern: /^INSERT INTO admin_audit_logs/ }
  ]);
  const result = await new MyforgeStore(pool).markAgentOffline({
    agentId: "dev-pc-001",
    connectionId: CONNECTION_ID,
    disconnectedAt: NOW
  });
  assert.equal(result.failedTasks.length, 1);
  assert.equal(result.failedTasks[0].errorCode, "MYFORGE_AGENT_DISCONNECTED");
  assert.equal(result.staleConnection, false);
  client.assertDone();
});

test("myforge server shutdown preserves CAS and marks active work as server-restarted", async () => {
  const failed = taskRow({
    status: "failed",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_A,
    command_expires_at: new Date(NOW.getTime() + 60000),
    dispatched_at: NOW,
    error_code: "MYFORGE_SERVER_RESTARTED",
    error_message: "Admin API stopped before the task completed",
    completed_at: NOW
  });
  let auditDetails;
  const { client, pool } = transactionClient([
    {
      pattern: /^UPDATE myforge_agents SET status = 'offline'/,
      result: { rows: [agentRow()], rowCount: 1 }
    },
    {
      pattern: /^UPDATE myforge_task_runs SET status = 'failed'/,
      result: ({ sql, params }) => {
        assert.match(sql, /MYFORGE_SERVER_RESTARTED/);
        assert.equal(params[2], CONNECTION_ID);
        assert.equal(params[3], "server_shutdown");
        return { rows: [failed], rowCount: 1 };
      }
    },
    {
      pattern: /^INSERT INTO admin_audit_logs/,
      result: ({ params }) => {
        auditDetails = JSON.parse(params[4]);
        return { rows: [], rowCount: 1 };
      }
    }
  ]);
  const result = await new MyforgeStore(pool).markAgentOffline({
    agentId: "dev-pc-001",
    connectionId: CONNECTION_ID,
    disconnectedAt: NOW,
    failureReason: "server_shutdown"
  });
  assert.equal(result.failedTasks[0].errorCode, "MYFORGE_SERVER_RESTARTED");
  assert.equal(auditDetails.reason, "server_shutdown");
  client.assertDone();
});

test("myforge queued cancellation records request and terminal audit without a cancel deadline", async () => {
  const queued = taskRow();
  const cancelled = taskRow({
    status: "cancelled",
    queue_reason: null,
    error_code: "MYFORGE_COMMAND_CANCELLED",
    error_message: "Task was cancelled before dispatch",
    completed_at: NOW
  });
  const actions = [];
  const { client, pool } = transactionClient([
    { pattern: /^SELECT \* FROM myforge_task_runs/, result: { rows: [queued] } },
    { pattern: /^UPDATE myforge_task_runs SET status = 'cancelled'/, result: { rows: [cancelled] } },
    {
      pattern: /^INSERT INTO admin_audit_logs/,
      result: ({ params }) => { actions.push(params[2]); return { rows: [] }; }
    },
    {
      pattern: /^INSERT INTO admin_audit_logs/,
      result: ({ params }) => { actions.push(params[2]); return { rows: [] }; }
    }
  ]);
  const store = new MyforgeStore(pool, { cancelTimeoutMs: 10000 });
  const result = await store.requestTaskCancellation({
    requestId: REQUEST_ID,
    adminId: 8,
    adminUsername: "canceller",
    requestedAt: NOW
  });
  assert.equal(result.outcome, "cancelled");
  assert.equal(result.sendCancel, false);
  assert.equal(result.task.cancelRequestedAt, null);
  assert.equal(result.task.cancelDeadlineAt, null);
  assert.deepEqual(actions, ["myforge_task_cancel_request", "myforge_task_cancelled"]);
  client.assertDone();
});

test("myforge queued-only cancel probe never mutates a task that dispatch already claimed", async () => {
  const dispatched = taskRow({
    status: "dispatched",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_digest: DIGEST_A,
    command_expires_at: new Date(NOW.getTime() + 60000),
    dispatched_at: NOW
  });
  const tx = transactionClient([
    { pattern: /^SELECT \* FROM myforge_task_runs/, result: { rows: [dispatched] } }
  ]);
  const result = await new MyforgeStore(tx.pool).requestTaskCancellation({
    requestId: REQUEST_ID,
    requestedAt: NOW,
    queuedOnly: true
  });
  assert.equal(result.outcome, "requires_connection");
  assert.equal(result.task.cancelRequestedAt, null);
  tx.client.assertDone();
});

test("myforge task get, filtered list and count return mapped persisted records", async () => {
  const failed = taskRow({
    status: "failed",
    queue_reason: null,
    error_code: "MYFORGE_QUEUE_EXPIRED",
    error_message: "Task expired while waiting for an agent",
    completed_at: NOW
  });
  const client = new ScriptedClient([
    {
      pattern: /^SELECT \* FROM myforge_task_runs WHERE request_id = \$1 LIMIT 1$/,
      result: ({ params }) => {
        assert.deepEqual(params, [REQUEST_ID]);
        return { rows: [failed] };
      }
    },
    {
      pattern: /^SELECT \* FROM myforge_task_runs WHERE agent_id = \$1 AND status = ANY\(\$2::varchar\[\]\)/,
      result: ({ sql, params }) => {
        assert.match(sql, /ORDER BY created_at DESC, request_id DESC LIMIT \$3 OFFSET \$4$/);
        assert.deepEqual(params, ["dev-pc-001", ["failed"], 25, 5]);
        return { rows: [failed] };
      }
    },
    {
      pattern: /^SELECT COUNT\(\*\) AS total FROM myforge_task_runs WHERE project_id = \$1$/,
      result: ({ params }) => {
        assert.deepEqual(params, ["myforge-local"]);
        return { rows: [{ total: "3" }] };
      }
    }
  ]);
  const pool = { async query(sql, params) { return client.query(sql, params); } };
  const store = new MyforgeStore(pool);
  const task = await store.getTask(REQUEST_ID);
  const tasks = await store.listTasks({ agentId: "dev-pc-001", status: "failed", limit: 25, offset: 5 });
  const count = await store.countTasks({ projectId: "myforge-local" });

  assert.equal(task.requestId, REQUEST_ID);
  assert.equal(task.errorCode, "MYFORGE_QUEUE_EXPIRED");
  assert.equal(tasks.length, 1);
  assert.equal(tasks[0].status, "failed");
  assert.equal(count, 3);
  client.assertDone({ released: false });
});

test("myforge FIFO lookup and bulk queue reason updates use stable agent-local ordering", async () => {
  const oldest = taskRow({ queue_reason: null });
  const client = new ScriptedClient([
    {
      pattern: /^SELECT \* FROM myforge_task_runs WHERE agent_id = \$1 AND project_id = \$2 AND status = 'queued'/,
      result: ({ sql, params }) => {
        assert.match(sql, /queue_expires_at > \$3/);
        assert.match(sql, /ORDER BY created_at ASC, request_id ASC LIMIT 1$/);
        assert.deepEqual(params, ["dev-pc-001", "myforge-local", NOW]);
        return { rows: [oldest] };
      }
    },
    {
      pattern: /^UPDATE myforge_task_runs SET queue_reason = \$3/,
      result: ({ sql, params }) => {
        assert.match(sql, /status = 'queued'/);
        assert.deepEqual(params, ["dev-pc-001", "myforge-local", "agent_busy"]);
        return { rows: [{ ...oldest, queue_reason: "agent_busy" }] };
      }
    },
    {
      pattern: /^SELECT DISTINCT task\.agent_id, task\.project_id FROM myforge_task_runs task/,
      result: ({ sql, params }) => {
        assert.match(sql, /agent\.configured = true/);
        assert.match(sql, /agent\.status = 'online'/);
        assert.match(sql, /task\.status = 'queued' AND task\.queue_expires_at > \$1/);
        assert.match(sql, /NOT EXISTS/);
        assert.deepEqual(params, [NOW]);
        return { rows: [{ agent_id: "dev-pc-001", project_id: "myforge-local" }] };
      }
    }
  ]);
  const store = new MyforgeStore({ async query(sql, params) { return client.query(sql, params); } });
  const next = await store.findNextQueuedTask({ agentId: "dev-pc-001", projectId: "myforge-local", now: NOW });
  const updated = await store.setQueuedTasksReasonForAgent({
    agentId: "dev-pc-001",
    projectId: "myforge-local",
    queueReason: "agent_busy"
  });
  const identities = await store.listQueuedAgentIdentities(NOW);
  assert.equal(next.requestId, REQUEST_ID);
  assert.equal(updated[0].queueReason, "agent_busy");
  assert.deepEqual(identities, [{ agentId: "dev-pc-001", projectId: "myforge-local" }]);
  client.assertDone({ released: false });
});

test("myforge command and cancellation watchdog SQL keeps cancel priority deterministic", async () => {
  const dispatchedFailed = taskRow({
    status: "failed",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_expires_at: new Date(NOW.getTime() - 6000),
    dispatched_at: new Date(NOW.getTime() - 66000),
    error_code: "MYFORGE_COMMAND_EXPIRED",
    completed_at: NOW
  });
  const dispatchedTx = transactionClient([
    {
      pattern: /^UPDATE myforge_task_runs SET status = 'failed'/,
      result: ({ sql, params }) => {
        assert.match(sql, /status = 'dispatched'/);
        assert.match(sql, /cancel_requested_at IS NULL/);
        assert.match(sql, /command_expires_at \+ \(\$2::bigint \* interval '1 millisecond'\) <= \$1/);
        assert.deepEqual(params, [NOW, 5000]);
        return { rows: [dispatchedFailed] };
      }
    },
    { pattern: /^INSERT INTO admin_audit_logs/ }
  ]);
  const commandExpired = await new MyforgeStore(dispatchedTx.pool).failExpiredDispatchedTasks({ now: NOW, clockSkewMs: 5000 });
  assert.equal(commandExpired[0].errorCode, "MYFORGE_COMMAND_EXPIRED");
  dispatchedTx.client.assertDone();

  const runningFailed = taskRow({
    status: "failed",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_expires_at: new Date(NOW.getTime() - 120000),
    dispatched_at: new Date(NOW.getTime() - 130000),
    started_at: new Date(NOW.getTime() - 126000),
    error_code: "MYFORGE_COMMAND_TIMEOUT",
    completed_at: NOW
  });
  const runningTx = transactionClient([
    {
      pattern: /^UPDATE myforge_task_runs SET status = 'failed'/,
      result: ({ sql }) => {
        assert.match(sql, /status = 'running'/);
        assert.match(sql, /cancel_requested_at IS NULL/);
        assert.match(sql, /started_at \+ \(\(timeout_ms \+ \$2::bigint\) \* interval '1 millisecond'\) <= \$1/);
        return { rows: [runningFailed] };
      }
    },
    { pattern: /^INSERT INTO admin_audit_logs/ }
  ]);
  const commandTimedOut = await new MyforgeStore(runningTx.pool).failTimedOutRunningTasks({ now: NOW, clockSkewMs: 5000 });
  assert.equal(commandTimedOut[0].errorCode, "MYFORGE_COMMAND_TIMEOUT");
  runningTx.client.assertDone();

  const cancelFailed = taskRow({
    status: "failed",
    queue_reason: null,
    execution_mode: "codex_exec",
    connection_id: CONNECTION_ID,
    command_expires_at: new Date(NOW.getTime() - 120000),
    dispatched_at: new Date(NOW.getTime() - 130000),
    cancel_requested_at: new Date(NOW.getTime() - 16000),
    cancel_deadline_at: new Date(NOW.getTime() - 6000),
    error_code: "MYFORGE_CANCEL_TIMEOUT",
    completed_at: NOW
  });
  const cancelTx = transactionClient([
    {
      pattern: /^UPDATE myforge_task_runs SET status = 'failed'/,
      result: ({ sql }) => {
        assert.match(sql, /status IN \('dispatched', 'running'\)/);
        assert.match(sql, /cancel_requested_at IS NOT NULL/);
        assert.match(sql, /cancel_deadline_at \+ \(\$2::bigint \* interval '1 millisecond'\) <= \$1/);
        return { rows: [cancelFailed] };
      }
    },
    { pattern: /^INSERT INTO admin_audit_logs/ }
  ]);
  const cancelTimedOut = await new MyforgeStore(cancelTx.pool).failExpiredCancellationTasks({ now: NOW, clockSkewMs: 5000 });
  assert.equal(cancelTimedOut[0].errorCode, "MYFORGE_CANCEL_TIMEOUT");
  cancelTx.client.assertDone();
});

test("myforge connection close preserves execute and cancel writer failure classifications", async () => {
  for (const [kind, errorCode, pendingCancel, expectedAuditReason] of [
    ["command.execute", "MYFORGE_DISPATCH_FAILED", false, "dispatch_delivery_failed"],
    ["command.cancel", "MYFORGE_CANCEL_DELIVERY_FAILED", true, "cancel_delivery_failed"],
    ["command.cancel", "MYFORGE_AGENT_DISCONNECTED", false, "agent_disconnected"]
  ]) {
    const failed = taskRow({
      status: "failed",
      queue_reason: null,
      execution_mode: "codex_exec",
      connection_id: CONNECTION_ID,
      command_digest: DIGEST_A,
      command_expires_at: new Date(NOW.getTime() + 60000),
      dispatched_at: NOW,
      cancel_requested_at: pendingCancel ? NOW : null,
      cancel_deadline_at: pendingCancel ? new Date(NOW.getTime() + 10000) : null,
      error_code: errorCode,
      completed_at: NOW
    });
    const tx = transactionClient([
      { pattern: /^UPDATE myforge_agents SET status = 'offline'/, result: { rows: [agentRow()] } },
      {
        pattern: /^UPDATE myforge_task_runs SET status = 'failed'/,
        result: ({ sql, params }) => {
          assert.match(sql, /request_id = \$5::uuid AND \$6 = 'command.execute'/);
          assert.match(sql, /request_id = \$5::uuid AND \$6 = 'command.cancel'/);
          assert.equal(params[4], REQUEST_ID);
          assert.equal(params[5], kind);
          return { rows: [failed] };
        }
      },
      {
        pattern: /^INSERT INTO admin_audit_logs/,
        result: ({ params }) => {
          assert.equal(JSON.parse(params[4]).reason, expectedAuditReason);
          return { rows: [] };
        }
      }
    ]);
    const result = await new MyforgeStore(tx.pool).markAgentOffline({
      agentId: "dev-pc-001",
      connectionId: CONNECTION_ID,
      disconnectedAt: NOW,
      deliveryFailure: { requestId: REQUEST_ID, kind }
    });
    assert.equal(result.failedTasks[0].errorCode, errorCode);
    tx.client.assertDone();
  }
});

test("myforge schema is present in both bootstrap paths with state and active-task constraints", () => {
  const dbClient = fs.readFileSync(new URL("../db-client.js", import.meta.url), "utf8");
  const initSql = fs.readFileSync(new URL("../../../../db/init.sql", import.meta.url), "utf8");
  for (const source of [dbClient, initSql]) {
    assert.match(source, /CREATE TABLE IF NOT EXISTS myforge_agents/);
    assert.match(source, /CREATE TABLE IF NOT EXISTS myforge_task_runs/);
    assert.match(source, /ck_myforge_tasks_status/);
    assert.match(source, /ck_myforge_tasks_cancel_pair/);
    assert.match(source, /ck_myforge_tasks_dispatch_fields/);
    assert.match(source, /ck_myforge_agents_connection_status/);
    assert.match(source, /uk_myforge_agents_connection_id/);
    assert.match(source, /ck_myforge_tasks_prompt_json/);
    assert.match(source, /ck_myforge_tasks_artifact_json/);
    assert.match(source, /ck_myforge_tasks_audit_json/);
    assert.match(source, /uk_myforge_tasks_agent_active/);
    assert.match(source, /idx_myforge_tasks_agent_status_created/);
    assert.match(source, /idx_myforge_tasks_project_created/);
  }
});
