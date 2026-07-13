import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

import { DbMailStore } from "../../apps/mail-service/src/db-store.js";
import { computeGrantRequestFingerprint } from "../../apps/mail-service/src/game-admin-client.js";

async function createMail(store, overrides = {}) {
  const mail = {
    mail_id: "mail_001",
    sender_type: "system",
    sender_id: "system",
    sender_name: "系统",
    from_player_id: "system",
    to_player_id: "player_001",
    title: "Reward",
    content: "",
    attachments: [{ type: "item", id: 1001, count: 2 }],
    mail_type: "system",
    created_by_type: "system",
    created_by_id: "system",
    created_by_name: "系统",
    created_at: Date.now(),
    ...overrides
  };

  await store.createMail(mail);
  return mail;
}

function claimInput(overrides = {}) {
  const attachmentsSnapshot = [{ itemId: 1001, count: 2, binded: false }];
  return {
    mailId: "mail_001",
    playerId: "player_001",
    characterId: "chr_001",
    requestId: "mail_claim:mail_001",
    attachmentsSnapshot,
    attachmentsFingerprint: computeGrantRequestFingerprint("mail_001", "chr_001", attachmentsSnapshot),
    expectedAttachments: [{ type: "item", id: 1001, count: 2 }],
    traceId: "0123456789abcdef0123456789abcdef",
    ...overrides
  };
}

test("mail attachment workflow freezes request identity and only grants one active lease", async () => {
  const store = new DbMailStore(null);
  await createMail(store);

  const first = await store.reserveMailClaimWorkflow(claimInput(), {
    leaseOwner: "mail-a",
    leaseToken: "lease-a",
    leaseMs: 30_000
  });
  const second = await store.reserveMailClaimWorkflow(claimInput(), {
    leaseOwner: "mail-b",
    leaseToken: "lease-b",
    leaseMs: 30_000
  });

  assert.equal(first.acquired, true);
  assert.equal(first.inProgress, false);
  assert.equal(first.alreadyClaimed, false);
  assert.equal(first.mail.status, "claiming");
  assert.equal(first.workflow.claim_request_id, "mail_claim:mail_001");
  assert.equal(first.workflow.character_id, "chr_001");
  assert.match(first.workflow.attachments_fingerprint, /^sha256:[0-9a-f]{64}$/);
  assert.deepEqual(first.workflow.attachments_snapshot, [{ itemId: 1001, count: 2, binded: false }]);
  assert.equal(first.workflow.attempts, 1);

  assert.equal(second.acquired, false);
  assert.equal(second.inProgress, true);
  assert.equal(second.alreadyClaimed, false);
  assert.equal(second.workflow.lease_token, "lease-a");
});

test("completed workflow conditionally updates workflow and mail to claimed", async () => {
  const store = new DbMailStore(null);
  await createMail(store);

  const reserved = await store.reserveMailClaimWorkflow(claimInput(), {
    leaseToken: "lease-a"
  });
  assert.equal(reserved.acquired, true);

  const completed = await store.completeMailClaimWorkflow("mail_001", "lease-a", {
    traceId: "11111111111111111111111111111111",
    instanceId: "game-a",
    resultSummary: {
      characterId: "chr_001",
      source: "mail-claim",
      items: claimInput().attachmentsSnapshot
    }
  });
  const retry = await store.reserveMailClaimWorkflow(claimInput({ characterId: "chr_002" }), { leaseToken: "lease-b" });

  assert.equal(completed.claimed, true);
  assert.equal(completed.mail.status, "claimed");
  assert.ok(completed.mail.claimed_at);
  assert.equal(completed.workflow.status, "claimed");
  assert.ok(completed.workflow.completed_at);
  assert.equal(completed.workflow.game_instance_id, "game-a");

  assert.equal(retry.acquired, false);
  assert.equal(retry.inProgress, false);
  assert.equal(retry.alreadyClaimed, true);
  assert.equal(retry.workflow.status, "claimed");
});

test("retryable failure keeps mail claiming and reuses the frozen workflow", async () => {
  const store = new DbMailStore(null);
  await createMail(store);

  const reserved = await store.reserveMailClaimWorkflow(claimInput(), { leaseToken: "lease-a" });
  assert.equal(reserved.acquired, true);

  const failed = await store.recordMailClaimWorkflowFailure("mail_001", "lease-a", {
    status: "retryable_failure",
    errorCode: "ROUTE_UNAVAILABLE",
    errorCategory: "ROUTE_UNAVAILABLE",
    resultState: "not_applied",
    retryable: true,
    message: "route missing"
  });
  const afterFailure = await store.getMailById("mail_001");
  const retry = await store.reserveMailClaimWorkflow(claimInput({
    traceId: "11111111111111111111111111111111"
  }), { leaseToken: "lease-b" });

  assert.equal(failed.updated, true);
  assert.equal(failed.workflow.status, "retryable_failure");
  assert.equal(afterFailure.status, "claiming");
  assert.equal(afterFailure.claimed_at, null);

  assert.equal(retry.acquired, true);
  assert.equal(retry.workflow.claim_request_id, reserved.workflow.claim_request_id);
  assert.deepEqual(retry.workflow.attachments_snapshot, reserved.workflow.attachments_snapshot);
  assert.equal(retry.workflow.attempts, 2);
  assert.equal(retry.workflow.lease_token, "lease-b");
});

test("unknown result cannot be reacquired by a player request", async () => {
  const store = new DbMailStore(null);
  await createMail(store);
  await store.reserveMailClaimWorkflow(claimInput(), { leaseToken: "lease-a" });
  await store.recordMailClaimWorkflowFailure("mail_001", "lease-a", {
    status: "reconciliation_pending",
    errorCode: "GAME_ADMIN_READ_TIMEOUT",
    errorCategory: "RESULT_UNKNOWN",
    resultState: "unknown",
    retryable: false
  });

  const retry = await store.reserveMailClaimWorkflow(claimInput(), { leaseToken: "lease-b" });

  assert.equal(retry.acquired, false);
  assert.equal(retry.reconciliationPending, true);
  assert.equal(retry.workflow.status, "reconciliation_pending");
  assert.equal(retry.workflow.attempts, 1);
  assert.equal(retry.workflow.lease_token, null);
});

test("expired lease takeover fences stale completion and failure writes", async () => {
  const store = new DbMailStore(null);
  await createMail(store);
  await store.reserveMailClaimWorkflow(claimInput(), { leaseToken: "lease-old", leaseMs: 1 });
  store.memoryClaimWorkflows.get("mail_001").lease_expires_at = new Date(Date.now() - 1);

  const takeover = await store.reserveMailClaimWorkflow(claimInput(), { leaseToken: "lease-new" });
  const staleFailure = await store.recordMailClaimWorkflowFailure("mail_001", "lease-old", {
    status: "permanent_failure",
    errorCode: "STALE"
  });
  const staleComplete = await store.completeMailClaimWorkflow("mail_001", "lease-old");

  assert.equal(takeover.acquired, true);
  assert.equal(takeover.leaseTakenOver, true);
  assert.equal(takeover.workflow.attempts, 2);
  assert.equal(staleFailure.updated, false);
  assert.equal(staleComplete.claimed, false);
  const current = await store.getMailClaimWorkflow("mail_001");
  assert.equal(current.status, "processing");
  assert.equal(current.lease_token, "lease-new");
});

test("PostgreSQL rechecks workflow after mail row lock to serialize first claimants", async () => {
  const statements = [];
  let workflowSelects = 0;
  const activeWorkflow = {
    id: 41,
    mail_id: "mail_001",
    player_id: "player_001",
    claim_request_id: "mail_claim:mail_001",
    character_id: "chr_001",
    attachments_snapshot: claimInput().attachmentsSnapshot,
    attachments_fingerprint: claimInput().attachmentsFingerprint,
    status: "processing",
    attempts: 1,
    lease_owner: "mail-a",
    lease_token: "lease-a",
    lease_expires_at: new Date(Date.now() + 30_000)
  };
  const client = {
    async query(sql) {
      statements.push(sql.trim());
      if (sql.includes("FROM mail_claim_workflows") && sql.includes("FOR UPDATE")) {
        workflowSelects += 1;
        return { rows: workflowSelects === 1 ? [] : [activeWorkflow] };
      }
      if (sql.includes("FROM mails") && sql.includes("FOR UPDATE")) {
        return { rows: [{ ...mailRow(), status: "claiming" }] };
      }
      return { rows: [], rowCount: 0 };
    },
    release() {}
  };
  const store = new DbMailStore({ async connect() { return client; } });

  const result = await store.reserveMailClaimWorkflow(claimInput(), { leaseToken: "lease-b" });

  assert.equal(result.acquired, false);
  assert.equal(result.inProgress, true);
  assert.equal(result.workflow.lease_token, "lease-a");
  assert.equal(workflowSelects, 2);
  assert.equal(statements.some((sql) => sql.startsWith("INSERT INTO mail_claim_workflows")), false);
  assert.equal(statements.at(-1), "COMMIT");
});

test("claim workflow evidence survives hard mail deletion", async () => {
  const store = new DbMailStore(null);
  await createMail(store);
  await store.reserveMailClaimWorkflow(claimInput(), { leaseToken: "lease-a" });
  await store.recordMailClaimWorkflowFailure("mail_001", "lease-a", {
    status: "permanent_failure",
    errorCode: "ITEM_NOT_FOUND",
    errorCategory: "PERMANENT_FAILURE",
    resultState: "not_applied",
    retryable: false
  });

  await store.deleteMail("mail_001");
  const retry = await store.reserveMailClaimWorkflow(claimInput(), { leaseToken: "lease-b" });
  const completed = await store.completeMailClaimWorkflow("mail_001", "lease-b");

  assert.equal(retry.acquired, true);
  assert.equal(completed.claimed, true);
  assert.equal(completed.mail, null);
  assert.equal((await store.getMailClaimWorkflow("mail_001")).status, "claimed");
});

test("mail SQL defines durable claim workflow without cascading mail deletion", async () => {
  for (const path of ["db/init.sql", "apps/mail-service/db/init.sql", "apps/mail-service/src/db-client.js"]) {
    const source = await readFile(path, "utf8");
    assert.match(source, /CREATE TABLE IF NOT EXISTS mail_claim_workflows/);
    assert.match(source, /claim_request_id varchar\(128\) NOT NULL/);
    assert.match(source, /attachments_snapshot jsonb NOT NULL/);
    assert.match(source, /reconciliation_pending/);
    assert.match(source, /manual_review/);
    assert.match(source, /recovery_lease_token varchar\(64\) NULL/);
    assert.match(source, /next_recovery_at timestamptz NULL/);
    const claimTable = source.slice(
      source.indexOf("CREATE TABLE IF NOT EXISTS mail_claim_workflows"),
      source.indexOf("CREATE TABLE IF NOT EXISTS mail_notification_outbox")
    );
    assert.doesNotMatch(claimTable, /ON DELETE CASCADE/);
  }
  const storeSource = await readFile("apps/mail-service/src/db-store.js", "utf8");
  assert.match(storeSource, /FOR UPDATE SKIP LOCKED/);
  assert.match(storeSource, /recovery_lease_token = \$2/);
});

function mailRow() {
  return {
    mail_id: "mail_001",
    to_player_id: "player_001",
    attachments: [{ type: "item", id: 1001, count: 2 }],
    claimed_at: null,
    expires_at: null
  };
}

test("mail creation writes notification outbox in memory store", async () => {
  const store = new DbMailStore(null);
  await createMail(store);

  const outbox = await store.getMailNotificationOutboxByMailId("mail_001");

  assert.equal(outbox.mail_id, "mail_001");
  assert.equal(outbox.to_player_id, "player_001");
  assert.equal(outbox.status, "pending");
  assert.equal(outbox.attempts, 0);
  assert.equal(outbox.event_id, "mail.notify:mail_001");
  assert.equal(outbox.event_version, 1);
  assert.match(outbox.trace_id, /^[0-9a-f]{32}$/);
  assert.equal(outbox.payload.player_id, "player_001");
  assert.equal(outbox.payload.mail.mail_id, "mail_001");
});

test("mail insert uses provided executor instead of pool", async () => {
  const pool = {
    async query() {
      throw new Error("pool.query should not be used by insertMail");
    }
  };
  const executor = {
    calls: [],
    async query(sql, params) {
      this.calls.push({ sql, params });
      return { rows: [{ id: 123 }] };
    }
  };
  const store = new DbMailStore(pool);

  const mailId = await store.insertMail(executor, {
    mail_id: "mail_executor_001",
    sender_type: "system",
    sender_id: "system",
    sender_name: "系统",
    from_player_id: "system",
    to_player_id: "player_001",
    title: "Reward",
    content: "",
    attachments: [{ type: "item", id: 1001, count: 2 }],
    mail_type: "system",
    created_by_type: "system",
    created_by_id: "system",
    created_by_name: "系统",
    expires_at: null
  });

  assert.equal(mailId, 123);
  assert.equal(executor.calls.length, 1);
  assert.match(executor.calls[0].sql, /INSERT INTO mails/);
  assert.equal(executor.calls[0].params[0], "mail_executor_001");
});

test("PostgreSQL mail and notification outbox rollback as one transaction", async () => {
  const calls = [];
  const client = {
    async query(sql) {
      calls.push(sql.trim());
      if (sql.includes("INSERT INTO mails")) {
        return { rows: [{ id: 123 }] };
      }
      if (sql.includes("INSERT INTO mail_notification_outbox")) {
        throw new Error("outbox insert failed");
      }
      return { rows: [] };
    },
    release() {}
  };
  const store = new DbMailStore({ async connect() { return client; } });

  await assert.rejects(() => createMail(store), /outbox insert failed/);
  assert.equal(calls[0], "BEGIN");
  assert.match(calls[1], /INSERT INTO mails/);
  assert.match(calls[2], /INSERT INTO mail_notification_outbox/);
  assert.equal(calls.at(-1), "ROLLBACK");
  assert.equal(calls.includes("COMMIT"), false);
});

test("mail notification outbox can be reserved, failed, retried, and marked sent", async () => {
  const store = new DbMailStore(null);
  await createMail(store);

  const firstReserve = await store.reservePendingMailNotificationOutbox(10);
  assert.equal(firstReserve.length, 1);
  assert.equal(firstReserve[0].status, "sending");
  assert.equal(firstReserve[0].attempts, 1);

  await store.markMailNotificationOutboxFailed(firstReserve[0].id, "nats down", {
    leaseToken: firstReserve[0].lease_token
  });
  let outbox = await store.getMailNotificationOutboxByMailId("mail_001");
  assert.equal(outbox.status, "failed");
  assert.equal(outbox.attempts, 1);
  assert.equal(outbox.last_error, "nats down");

  outbox.next_attempt_at = new Date(Date.now() - 1);
  store.memoryOutbox.set(outbox.id, outbox);

  const retryReserve = await store.reservePendingMailNotificationOutbox(10);
  assert.equal(retryReserve.length, 1);
  assert.equal(retryReserve[0].attempts, 2);

  await store.markMailNotificationOutboxSent(retryReserve[0].id, retryReserve[0].lease_token);
  outbox = await store.getMailNotificationOutboxByMailId("mail_001");
  assert.equal(outbox.status, "sent");
  assert.ok(outbox.sent_at);
});
