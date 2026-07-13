import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

import { DbMailStore } from "../db-store.js";
import { MailOperationsController } from "./mail-operations.controller.js";
import { MailOperationsService } from "./mail-operations.service.js";
import { MetricsCollector } from "../metrics.js";

const fingerprint = `sha256:${"a".repeat(64)}`;

function seedClaim(store: any, mailId: string, overrides: any = {}) {
  const id = overrides.id || store.memoryClaimWorkflowNextId++;
  const now = overrides.updated_at || new Date();
  store.memory.set(mailId, {
    id,
    mail_id: mailId,
    to_player_id: overrides.player_id || "player-1",
    status: overrides.mail_status || "claiming",
    title: "must stay private",
    content: "secret mail body",
    attachments: [{ type: "item", id: 1001, count: 2 }],
    created_at: now
  });
  store.memoryClaimWorkflows.set(mailId, {
    id,
    mail_id: mailId,
    player_id: overrides.player_id || "player-1",
    claim_request_id: `mail_claim:${mailId}`,
    character_id: overrides.character_id || "chr_1",
    attachments_snapshot: [{ itemId: 1001, count: 2, binded: false }],
    attachments_fingerprint: fingerprint,
    status: overrides.status || "retryable_failure",
    attempts: 1,
    lease_owner: null,
    lease_token: null,
    lease_expires_at: null,
    last_error_code: overrides.last_error_code || "ROUTE_UNAVAILABLE",
    last_error_category: "ROUTE_UNAVAILABLE",
    last_result_state: "not_applied",
    last_error_retryable: true,
    last_error_message: "bounded error",
    recovery_attempts: overrides.recovery_attempts || 0,
    recovery_mode: null,
    recovery_lease_owner: null,
    recovery_lease_token: null,
    recovery_lease_expires_at: null,
    next_recovery_at: now,
    last_query_instance_ids: [],
    created_at: now,
    updated_at: now,
    completed_at: null,
    manual_review_at: overrides.manual_review_at || null
  });
}

function createService(store: any, queryGrant: any = async (requestId: string) => ({
  queryStatus: "succeeded",
  requestId,
  requestFingerprint: fingerprint,
  resultState: "applied",
  resultSummary: { characterId: "chr_1", itemCount: 1 },
  instanceIds: ["game-a"]
})) {
  return new MailOperationsService(store, { queryMailAttachmentGrant: queryGrant }, {
    mailRetentionDays: 400,
    claimWorkflowRetentionDays: 400,
    gameGrantRetentionDays: 400,
    outboxSentRetentionDays: 7,
    outboxTerminalRetentionDays: 30
  });
}

function operation(overrides: any = {}) {
  return {
    operation_request_id: overrides.operation_request_id || "op_001",
    actor: overrides.actor || "admin_1",
    reason: overrides.reason || "verify and recover claim"
  };
}

test("claim query requires exact filter, bounds pagination, and strips mail payload", async () => {
  const store = new DbMailStore(null);
  seedClaim(store, "mail-1", { id: 1 });
  seedClaim(store, "mail-2", { id: 2 });
  const service = createService(store);

  await assert.rejects(() => service.queryClaims({}), (error: any) =>
    error?.getResponse?.()?.error === "CLAIM_QUERY_FILTER_REQUIRED");
  await assert.rejects(() => service.queryClaims({ mail_id: "mail-1", limit: "1x" }), (error: any) =>
    error?.getResponse?.()?.error === "INVALID_LIMIT");
  const first = await service.queryClaims({ player_id: "player-1", limit: "1" });
  assert.equal(first.items.length, 1);
  assert.equal(first.items[0].mail_id, "mail-2");
  assert.equal(first.next_before_id, 2);
  assert.equal(first.items[0].game_result.status, "succeeded");
  assert.equal("attachments_snapshot" in first.items[0], false);
  assert.equal("content" in first.items[0], false);
  const second = await service.queryClaims({ player_id: "player-1", limit: "1", before_id: first.next_before_id });
  assert.equal(second.items[0].mail_id, "mail-1");
  assert.equal(second.next_before_id, null);
});

test("claim query supports every exact locator and fails closed when game query is unavailable", async () => {
  const store = new DbMailStore(null);
  seedClaim(store, "mail-1", { status: "reconciliation_pending" });
  const service = createService(store, async () => {
    const error: any = new Error("bad game response containing internal details");
    error.code = "GAME_ADMIN_RESPONSE_INVALID";
    throw error;
  });

  for (const filter of [
    { mail_id: "mail-1" },
    { request_id: "mail_claim:mail-1" },
    { player_id: "player-1" },
    { character_id: "chr_1" },
    { status: "reconciliation_pending" }
  ]) {
    const result = await service.queryClaims(filter);
    assert.equal(result.items.length, 1);
    assert.equal(result.items[0].game_result.status, "result_unavailable");
    assert.equal(result.items[0].game_result.error_code, "GAME_ADMIN_RESPONSE_INVALID");
    assert.equal(JSON.stringify(result), JSON.stringify(result).replace("bad game response containing internal details", "bad game response containing internal details"));
    assert.doesNotMatch(JSON.stringify(result), /internal details/);
  }
});

test("ordinary retry schedules query-first recovery with frozen identity and idempotent operation audit", async () => {
  const store = new DbMailStore(null);
  seedClaim(store, "mail-1");
  const service = createService(store);
  const before = await store.getMailClaimWorkflow("mail-1");

  const first = await service.scheduleClaim("mail-1", "retry_original", operation());
  const duplicate = await service.scheduleClaim("mail-1", "retry_original", operation());
  const after = await store.getMailClaimWorkflow("mail-1");

  assert.equal(first.scheduled, true);
  assert.equal(duplicate.idempotent_replay, true);
  assert.equal(after.status, "reconciliation_pending");
  assert.equal(after.claim_request_id, before.claim_request_id);
  assert.equal(after.character_id, before.character_id);
  assert.equal(after.attachments_fingerprint, before.attachments_fingerprint);
  assert.deepEqual(after.attachments_snapshot, before.attachments_snapshot);
  assert.equal(store.memoryAdminAudit.length, 1);
  assert.doesNotMatch(JSON.stringify(store.memoryAdminAudit[0]), /secret mail body|attachments_snapshot|GAME_ADMIN_TOKEN/);

  await assert.rejects(
    () => service.scheduleClaim("mail-1", "reconcile", operation()),
    (error: any) => error?.getResponse?.()?.error === "ADMIN_OPERATION_CONFLICT"
  );
  await assert.rejects(
    () => service.scheduleClaim("mail-1", "retry_original", operation({ reason: "changed reason" })),
    (error: any) => error?.getResponse?.()?.error === "ADMIN_OPERATION_CONFLICT"
  );
});

test("hard-deleted mail keeps its frozen workflow queryable and recoverable", async () => {
  const store = new DbMailStore(null);
  seedClaim(store, "mail-deleted");
  assert.equal(await store.deleteMail("mail-deleted"), true);
  const service = createService(store);

  const queried = await service.queryClaims({ mail_id: "mail-deleted" });
  assert.equal(queried.items.length, 1);
  assert.equal(queried.items[0].mail_status, null);
  assert.equal(queried.items[0].request_id, "mail_claim:mail-deleted");

  const scheduled = await service.scheduleClaim(
    "mail-deleted",
    "retry_original",
    operation({ operation_request_id: "op_deleted_1" })
  );
  assert.equal(scheduled.workflow_status, "reconciliation_pending");
  assert.equal((await store.getMailClaimWorkflow("mail-deleted")).status, "reconciliation_pending");
  assert.equal(store.memoryAdminAudit[0].before_snapshot.mail_status, null);
});

test("PostgreSQL query and recovery lock workflow without requiring a mail row", async () => {
  const row = {
    id: "41",
    mail_id: "mail-deleted",
    player_id: "player-1",
    claim_request_id: "mail_claim:mail-deleted",
    character_id: "chr_1",
    attachments_snapshot: [{ itemId: 1001, count: 2, binded: false }],
    attachments_fingerprint: fingerprint,
    status: "retryable_failure",
    attempts: 1,
    lease_owner: null,
    lease_token: null,
    lease_expires_at: null,
    recovery_attempts: 1,
    recovery_mode: null,
    recovery_lease_owner: null,
    recovery_lease_token: null,
    recovery_lease_expires_at: null,
    next_recovery_at: new Date(),
    last_result_state: "not_applied",
    last_query_instance_ids: [],
    created_at: new Date(),
    updated_at: new Date(),
    completed_at: null,
    mail_status: null,
    outbox_event_id: null,
    outbox_status: null,
    outbox_attempts: null,
    outbox_terminal_at: null
  };
  const poolQueries: string[] = [];
  const clientQueries: string[] = [];
  const client = {
    async query(sql: string) {
      clientQueries.push(sql);
      if (/FROM mail_admin_operation_audit WHERE operation_request_id/.test(sql)) return { rows: [] };
      if (/FROM mail_claim_workflows workflow/.test(sql)) return { rows: [row] };
      if (/UPDATE mail_claim_workflows/.test(sql)) {
        return { rows: [{ ...row, status: "reconciliation_pending", updated_at: new Date() }] };
      }
      return { rows: [], rowCount: 1 };
    },
    release() {}
  };
  const pool = {
    async query(sql: string) {
      poolQueries.push(sql);
      return { rows: [row] };
    },
    async connect() { return client; }
  };
  const store = new DbMailStore(pool);

  const page = await store.queryMailClaimWorkflows({ mailId: "mail-deleted" }, { limit: 20 });
  assert.equal(page.items[0].mail_status, null);
  assert.match(poolQueries[0], /LEFT JOIN mails mail/);
  assert.doesNotMatch(poolQueries[0], /\n\s+JOIN mails mail/);

  const scheduled = await store.scheduleMailClaimAdminRecovery({
    operationRequestId: "op_pg_deleted_1",
    action: "retry_original",
    mailId: "mail-deleted",
    actor: "admin-1",
    reason: "recover frozen workflow",
    highRisk: false
  });
  assert.equal(scheduled.workflow_status, "reconciliation_pending");
  const lockSql = clientQueries.find((sql) => /FROM mail_claim_workflows workflow/.test(sql)) || "";
  assert.match(lockSql, /LEFT JOIN mails mail/);
  assert.match(lockSql, /FOR UPDATE OF workflow/);
  assert.doesNotMatch(lockSql, /FOR UPDATE OF workflow, mail/);
});

test("manual review requires high-risk action and resets only recovery scheduling state", async () => {
  const store = new DbMailStore(null);
  seedClaim(store, "mail-1", { status: "manual_review", recovery_attempts: 12, manual_review_at: new Date() });
  const service = createService(store);

  await assert.rejects(
    () => service.scheduleClaim("mail-1", "retry_original", operation()),
    (error: any) => error?.getResponse?.()?.error === "MAIL_CLAIM_OPERATION_NOT_ALLOWED"
  );
  const result = await service.scheduleClaim("mail-1", "manual_recover", operation(), true);
  const workflow = await store.getMailClaimWorkflow("mail-1");
  assert.equal(result.workflow_status, "reconciliation_pending");
  assert.equal(workflow.recovery_attempts, 0);
  assert.equal(workflow.manual_review_at, null);
  assert.equal(store.memoryAdminAudit[0].high_risk, true);
  assert.equal(store.memoryAdminAudit[0].actor, "admin_1");
  assert.equal(store.memoryAdminAudit[0].reason, "verify and recover claim");
});

test("controller isolates operations and high-risk credentials", async () => {
  const calls: any[] = [];
  const controller = new MailOperationsController({
    async scheduleClaim(...args: any[]) { calls.push(args); return { ok: true }; }
  } as any, {
    mailOperationsToken: "operations-secret-123456",
    mailHighRiskToken: "high-risk-secret-123456"
  });
  const body = operation();

  assert.throws(
    () => controller.manualRecover("mail-1", { "x-mail-operations-token": "operations-secret-123456" }, body),
    (error: any) => error?.getResponse?.()?.error === "MAIL_HIGH_RISK_TOKEN_REQUIRED"
  );
  await controller.manualRecover("mail-1", {
    "x-mail-operations-token": "operations-secret-123456",
    "x-mail-high-risk-token": "high-risk-secret-123456"
  }, body);
  assert.equal(calls[0][1], "manual_recover");
  assert.equal(calls[0][3], true);
});

test("terminal outbox replay preserves event payload and does not change mail or claim state", async () => {
  const store = new DbMailStore(null);
  seedClaim(store, "mail-1");
  const payload = { event_id: "mail-notify:mail-1", player_id: "player-1", mail: { mail_id: "mail-1" } };
  const outbox = store.enqueueMailNotificationOutboxMemory({
    mail_id: "mail-1",
    to_player_id: "player-1",
    event_id: "mail-notify:mail-1",
    event_version: 1,
    trace_id: "1".repeat(32),
    occurred_at: Date.now(),
    payload,
    max_attempts: 8
  });
  const persisted = store.memoryOutbox.get(outbox.id);
  persisted.status = "terminal";
  persisted.attempts = 8;
  persisted.terminal_at = new Date();
  const service = createService(store);
  const mailBefore = structuredClone(store.memory.get("mail-1"));
  const workflowBefore = await store.getMailClaimWorkflow("mail-1");

  const first = await service.replayOutbox("mail-notify:mail-1", operation({ operation_request_id: "op_replay_1" }));
  const duplicate = await service.replayOutbox("mail-notify:mail-1", operation({ operation_request_id: "op_replay_1" }));

  assert.equal(first.status, "pending");
  assert.equal(duplicate.idempotent_replay, true);
  assert.deepEqual(store.memoryOutbox.get(outbox.id).payload, payload);
  assert.equal(store.memoryOutbox.get(outbox.id).event_id, "mail-notify:mail-1");
  assert.deepEqual(store.memory.get("mail-1"), mailBefore);
  assert.deepEqual(await store.getMailClaimWorkflow("mail-1"), workflowBefore);
  assert.equal(store.memoryAdminAudit.length, 1);
});

test("database schema enforces append-only operation audit", () => {
  for (const path of [
    new URL("../../db/init.sql", import.meta.url),
    new URL("../../../../db/init.sql", import.meta.url)
  ]) {
    const sql = fs.readFileSync(path, "utf8");
    assert.match(sql, /CREATE TABLE IF NOT EXISTS mail_admin_operation_audit/);
    assert.match(sql, /BEFORE UPDATE OR DELETE ON mail_admin_operation_audit/);
    assert.match(sql, /BEFORE TRUNCATE ON mail_admin_operation_audit/);
    assert.match(sql, /REVOKE UPDATE, DELETE, TRUNCATE ON mail_admin_operation_audit FROM PUBLIC/);
    assert.match(sql, /UNIQUE \(operation_request_id\)/);
  }
  const runtimeSchema = fs.readFileSync(new URL("../db-client.js", import.meta.url), "utf8");
  assert.match(runtimeSchema, /CREATE TABLE IF NOT EXISTS mail_admin_operation_audit/);
  assert.match(runtimeSchema, /CREATE TRIGGER trg_mail_admin_operation_audit_immutable/);
  assert.match(runtimeSchema, /BEFORE TRUNCATE ON mail_admin_operation_audit/);
  assert.match(runtimeSchema, /REVOKE UPDATE, DELETE, TRUNCATE ON mail_admin_operation_audit FROM PUBLIC/);
});

test("claim operational snapshots and rate counters are emitted without high-cardinality labels", async () => {
  const store = new DbMailStore(null);
  const old = new Date(Date.now() - 20 * 60_000);
  seedClaim(store, "mail-1", { status: "reconciliation_pending", updated_at: old });
  seedClaim(store, "mail-2", {
    status: "manual_review",
    last_error_code: "REQUEST_FINGERPRINT_CONFLICT",
    manual_review_at: old
  });
  seedClaim(store, "mail-3", { status: "retryable_failure", updated_at: old });
  seedClaim(store, "mail-4", { status: "permanent_failure", updated_at: old });
  const snapshot = await store.getMailClaimOperationalStats({ longRunningMs: 15 * 60_000 });
  assert.deepEqual(snapshot, { longRunning: 3, fingerprintConflicts: 1, manualReview: 1 });

  const messages: any[] = [];
  const metrics = new MetricsCollector({
    async publishJson(subject: string, payload: any) { messages.push({ subject, payload }); }
  }, "mail-service", "mail-1");
  metrics.recordMailClaimAttempt();
  metrics.recordMailClaimSucceeded();
  metrics.setMailClaimOperationalSnapshot(snapshot);
  await metrics.flush();

  const fields = messages[0].payload.metrics;
  assert.equal(fields.mail_claim_attempts, 1);
  assert.equal(fields.mail_claim_succeeded, 1);
  assert.equal(fields.mail_claim_long_running, 3);
  assert.equal(fields.mail_claim_fingerprint_conflicts, 1);
  assert.equal(fields.mail_claim_manual_review_backlog, 1);
  assert.equal("mail_id" in fields, false);
  assert.equal("player_id" in fields, false);
});
