import assert from "node:assert/strict";
import test from "node:test";

import { AdminStore } from "./admin-store.js";

const SCOPE = {
  world_ids: ["world-1"],
  service_names: ["game-server"],
  instance_ids: ["instance-1"],
  field_allowlist: ["*"],
  target_types: ["player"],
  target_ids: ["player-1"],
  max_targets: 1
};

const OPERATION_ID = "11111111-1111-4111-8111-111111111111";
const PREVIEW_ID = "22222222-2222-4222-8222-222222222222";
const HASH = "a".repeat(64);
const NEXT_HASH = "b".repeat(64);

class OperationTransactionPool {
  constructor() {
    this.calls = [];
    this.operations = new Map();
    this.previews = new Map();
    this.approvals = new Map();
    this.client = {
      query: async (sql, params = []) => this.query(sql, params),
      release: () => this.calls.push({ sql: "RELEASE", params: [] })
    };
  }

  operationRow(requestId, { withPreview = true, withApproval = false } = {}) {
    const operation = this.operations.get(requestId);
    if (!operation) return null;
    const preview = this.previews.get(operation.operation_id);
    return {
      ...operation,
      ...(withPreview && preview ? {
        preview_id: preview.preview_id,
        nonce_sha256: preview.nonce_sha256,
        summary_sha256: preview.summary_sha256,
        preview_expires_at: preview.expires_at,
        preview_consumed_at: preview.consumed_at
      } : {}),
      ...(withApproval ? { approval_record_status: this.approvals.get(operation.operation_id) } : {})
    };
  }

  async connect() {
    return this.client;
  }

  async query(sql, params = []) {
    this.calls.push({ sql, params });
    if (sql === "BEGIN" || sql === "COMMIT" || sql === "ROLLBACK") return { rows: [], rowCount: 0 };
    if (sql.includes("FROM admin_operation_requests r") && sql.includes("WHERE r.request_id = $1")) {
      const row = this.operationRow(params[0], { withPreview: true, withApproval: sql.includes("admin_operation_approvals") });
      return { rows: row ? [row] : [], rowCount: row ? 1 : 0 };
    }
    if (sql.includes("INSERT INTO admin_operation_requests")) {
      const row = {
        operation_id: params[0],
        request_id: params[1],
        actor_admin_id: params[2],
        actor_subject: params[3],
        permission_key: params[4],
        risk_level: params[5],
        authorization_scope_json: JSON.parse(params[6]),
        requested_scope_json: JSON.parse(params[7]),
        scope_sha256: params[8],
        target_summary_json: JSON.parse(params[9]),
        target_sha256: params[10],
        payload_sha256: params[11],
        semantic_sha256: params[12],
        reason: params[13],
        trace_id: params[14],
        status: "preflighted",
        approval_status: params[15],
        execution_claimed_at: null,
        completed_at: null,
        result_summary_json: null,
        error_summary_json: null,
        created_at: new Date("2026-07-19T10:00:00.000Z"),
        updated_at: new Date("2026-07-19T10:00:00.000Z")
      };
      this.operations.set(row.request_id, row);
      return { rows: [row], rowCount: 1 };
    }
    if (sql.includes("INSERT INTO admin_operation_previews")) {
      this.previews.set(params[1], {
        preview_id: params[0],
        operation_id: params[1],
        nonce_sha256: params[2],
        impact_summary_json: JSON.parse(params[3]),
        summary_sha256: params[4],
        target_sha256: params[5],
        payload_sha256: params[6],
        expires_at: params[7],
        consumed_at: null
      });
      return { rows: [], rowCount: 1 };
    }
    if (sql.includes("INSERT INTO admin_operation_approvals")) {
      this.approvals.set(params[0], params[1]);
      return { rows: [], rowCount: 1 };
    }
    if (sql.includes("INSERT INTO admin_operation_audit_events")) return { rows: [], rowCount: 1 };
    if (sql.includes("UPDATE admin_operation_previews")) {
      const preview = [...this.previews.values()].find((entry) => entry.preview_id === params[0]);
      if (!preview || preview.consumed_at) return { rows: [], rowCount: 0 };
      preview.consumed_at = params[1];
      return { rows: [], rowCount: 1 };
    }
    if (sql.includes("UPDATE admin_operation_requests") && sql.includes("SET status = 'executing'")) {
      const operation = [...this.operations.values()].find((entry) => entry.operation_id === params[0]);
      if (!operation || !["preflighted", "approved"].includes(operation.status)) return { rows: [], rowCount: 0 };
      operation.status = "executing";
      operation.execution_claimed_at = params[1];
      operation.updated_at = params[1];
      return { rows: [operation], rowCount: 1 };
    }
    throw new Error(`unexpected query: ${sql}`);
  }
}

function preflightInput(overrides = {}) {
  return {
    operationId: OPERATION_ID,
    requestId: "request-transaction-1",
    actorAdminId: 7,
    actorSubject: "admin:operator-7",
    permissionKey: "gm.broadcast",
    riskLevel: "high",
    authorizationScope: SCOPE,
    requestedScope: { worldId: "world-1", targetIds: ["player-1"], targetCount: 1 },
    scopeSha256: HASH,
    targetSummary: { worldId: "world-1", targetCount: 1 },
    targetSha256: HASH,
    payloadSha256: HASH,
    semanticSha256: HASH,
    reason: "incident response",
    traceId: "trace-transaction-1",
    approvalStatus: "not_required",
    preview: {
      previewId: PREVIEW_ID,
      nonceSha256: HASH,
      impactSummary: { affectedPlayers: 1 },
      summarySha256: HASH,
      expiresAt: "2026-07-19T11:00:00.000Z"
    },
    ...overrides
  };
}

test("AdminStore atomically reserves a request and stores only hashed preflight material", async () => {
  const pool = new OperationTransactionPool();
  const store = new AdminStore(pool);
  const reserved = await store.reserveAdminOperationPreflight(preflightInput());

  assert.equal(reserved.kind, "created");
  assert.equal(pool.operations.get("request-transaction-1").payload_sha256, HASH);
  assert.equal(pool.previews.get(OPERATION_ID).nonce_sha256, HASH);
  assert.equal(pool.previews.get(OPERATION_ID).nonce, undefined);
  assert.equal(pool.approvals.get(OPERATION_ID), "not_required");
  assert.equal(pool.calls.filter((call) => call.sql === "BEGIN").length, 1);
  assert.equal(pool.calls.filter((call) => call.sql === "COMMIT").length, 1);
  assert.ok(pool.calls.some((call) => call.sql.includes("INSERT INTO admin_operation_audit_events")));
  assert.equal(
    pool.calls.some((call) => /(?:UPDATE|DELETE)\s+admin_operation_audit_events/i.test(call.sql)),
    false,
    "operation writes append audit events and never mutate them"
  );
});
test("AdminStore execution claim locks request, preview and approval, consumes nonce once, and preserves idempotent in-progress state", async () => {
  const pool = new OperationTransactionPool();
  const store = new AdminStore(pool);
  await store.reserveAdminOperationPreflight(preflightInput());

  const claimed = await store.claimAdminOperationExecution({
    requestId: "request-transaction-1",
    semanticSha256: HASH,
    nonceSha256: HASH,
    summarySha256: HASH,
    now: new Date("2026-07-19T10:05:00.000Z")
  });
  assert.equal(claimed.kind, "claimed");
  assert.equal(pool.operations.get("request-transaction-1").status, "executing");
  assert.equal(pool.previews.get(OPERATION_ID).consumed_at.toISOString(), "2026-07-19T10:05:00.000Z");
  assert.ok(pool.calls.some((call) => call.sql.includes("FOR UPDATE OF r, p, a")));

  const retried = await store.claimAdminOperationExecution({
    requestId: "request-transaction-1",
    semanticSha256: HASH,
    nonceSha256: HASH,
    summarySha256: HASH
  });
  assert.equal(retried.kind, "in_progress");

  const conflict = await store.reserveAdminOperationPreflight(preflightInput({ semanticSha256: NEXT_HASH }));
  assert.equal(conflict.kind, "conflict");
});

test("AdminStore operation audit queries are parameterized, keyset ordered, and redact sensitive summaries", async () => {
  const calls = [];
  const pool = {
    async query(sql, params) {
      calls.push({ sql, params });
      return {
        rows: [{
          id: 9,
          operation_id: OPERATION_ID,
          breakglass_grant_id: null,
          event_type: "execution_succeeded",
          actor_admin_id: 7,
          actor_subject: "admin:operator-7",
          request_id: "request-transaction-1",
          permission_key: "gm.send_item",
          risk_level: "high",
          trace_id: "trace-transaction-1",
          reason: "Authorization: Bearer never-return-this-value",
          target_summary_json: { targetIds: ["player-1"], token: "must-not-leak" },
          result_summary_json: { itemDelta: 1, payload: { content: "must-not-leak" } },
          details_json: { nonce: "must-not-leak", businessRecord: "ledger-unavailable" },
          operation_status: "succeeded",
          created_at: new Date("2026-07-19T10:10:00.000Z")
        }]
      };
    }
  };
  const events = await new AdminStore(pool).listAdminOperationAuditEvents({
    from: "2026-07-19T10:00:00.000Z",
    to: "2026-07-19T11:00:00.000Z",
    actorAdminId: 7,
    permissionKey: "gm.send_item",
    eventType: "execution_succeeded",
    target: "player-1",
    requestId: "request-transaction-1",
    traceId: "trace-transaction-1",
    riskLevel: "high",
    result: "succeeded",
    cursor: { createdAt: "2026-07-19T10:11:00.000Z", id: 10 },
    limit: 10
  });

  assert.equal(events[0].targetSummary.token, "[REDACTED]");
  assert.equal(events[0].resultSummary.payload, "[REDACTED]");
  assert.equal(events[0].details.nonce, "[REDACTED]");
  assert.equal(events[0].reason, "[REDACTED: potential credential]");
  assert.match(calls[0].sql, /ORDER BY e\.created_at DESC, e\.id DESC/);
  assert.match(calls[0].sql, /LEFT JOIN admin_operation_requests/);
  assert.equal(calls[0].params.includes("player-1"), true);
  assert.equal(calls[0].params.includes("trace-transaction-1"), true);
});
