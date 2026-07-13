import assert from "node:assert/strict";
import test from "node:test";

import { ClaimRecoveryWorker } from "./claim-recovery.worker.js";
import { DbMailStore } from "./db-store.js";

const fingerprint = `sha256:${"a".repeat(64)}`;
const frozenItems = [{ itemId: 1001, count: 2, binded: true }];

function seedMail(store: any, mailId = "mail-1") {
  store.memory.set(mailId, {
    id: 1,
    mail_id: mailId,
    sender_type: "system",
    sender_id: "system",
    sender_name: "system",
    from_player_id: "system",
    to_player_id: "player-1",
    title: "Reward",
    content: "",
    attachments: [{ type: "item", id: 1001, count: 2, binded: true }],
    mail_type: "system",
    created_by_type: "system",
    created_by_id: "system",
    created_by_name: "system",
    status: "unread",
    created_at: new Date(),
    read_at: null,
    claimed_at: null,
    expires_at: null
  });
}

async function createWorkflow(store: any, status = "reconciliation_pending", mailId = "mail-1") {
  seedMail(store, mailId);
  const reservation = await store.reserveMailClaimWorkflow({
    mailId,
    playerId: "player-1",
    characterId: "chr_1",
    requestId: `mail_claim:${mailId}`,
    attachmentsSnapshot: frozenItems,
    attachmentsFingerprint: fingerprint,
    expectedAttachments: store.memory.get(mailId).attachments,
    traceId: "1".repeat(32)
  }, { leaseMs: 60_000, leaseOwner: "player-worker" });
  if (status === "processing") {
    store.memoryClaimWorkflows.get(mailId).lease_expires_at = new Date(Date.now() - 1);
  } else {
    await store.recordMailClaimWorkflowFailure(mailId, reservation.workflow.lease_token, {
      status,
      traceId: "1".repeat(32),
      errorCode: status === "retryable_failure" ? "ROUTE_UNAVAILABLE" : "GAME_ADMIN_READ_TIMEOUT",
      errorCategory: status === "retryable_failure" ? "ROUTE_UNAVAILABLE" : "RESULT_UNKNOWN",
      resultState: status === "retryable_failure" ? "not_applied" : "unknown",
      retryable: status === "retryable_failure",
      message: "seed failure"
    });
  }
  return store.getMailClaimWorkflow(mailId);
}

function workerConfig(overrides: any = {}) {
  return {
    claimRecoveryEnabled: true,
    claimRecoveryPollIntervalMs: 60_000,
    claimRecoveryBatchSize: 20,
    claimRecoveryLeaseMs: 60_000,
    claimRecoveryBackoffBaseMs: 1000,
    claimRecoveryBackoffMaxMs: 10_000,
    claimRecoveryMaxAttempts: 5,
    claimRecoveryShutdownTimeoutMs: 50,
    serviceInstanceId: "mail-worker-a",
    ...overrides
  };
}

function succeededQuery() {
  return {
    ok: true,
    queryStatus: "succeeded",
    requestId: "mail_claim:mail-1",
    requestFingerprint: fingerprint,
    resultState: "applied",
    resultSummary: { characterId: "chr_1", source: "mail-claim", items: frozenItems },
    traceId: "2".repeat(32),
    createdAtMs: Date.now(),
    instanceIds: ["game-a", "game-b"]
  };
}

test("startup scan recovers an expired processing workflow after querying", async () => {
  const store = new DbMailStore(null);
  await createWorkflow(store, "processing");
  let grants = 0;
  const worker = new ClaimRecoveryWorker(store, {
    async queryMailAttachmentGrant() { return succeededQuery(); },
    async grantMailAttachments() { grants += 1; }
  }, workerConfig(), null);

  await worker.onModuleInit();
  await worker.onModuleDestroy();

  assert.equal((await store.getMailClaimWorkflow("mail-1")).status, "claimed");
  assert.equal((await store.getMailById("mail-1")).status, "claimed");
  assert.equal(grants, 0);
});

test("concurrent scans do not reenter the same worker", async () => {
  const store = new DbMailStore(null);
  await createWorkflow(store);
  let releaseQuery: (value: any) => void;
  const query = new Promise((resolve) => { releaseQuery = resolve; });
  const worker = new ClaimRecoveryWorker(store, {
    async queryMailAttachmentGrant() { return query; },
    async grantMailAttachments() { throw new Error("must not grant"); }
  }, workerConfig(), null);

  const first = worker.processRecoveries("periodic");
  const second = await worker.processRecoveries("periodic");
  assert.equal(second.skipped, true);
  releaseQuery!(succeededQuery());
  assert.equal((await first).recovered, 1);
});

test("periodic lifecycle scan does not overlap a slow query", async () => {
  const store = new DbMailStore(null);
  let releaseQuery: (value: any) => void;
  let queryCalls = 0;
  const query = new Promise((resolve) => { releaseQuery = resolve; });
  const worker = new ClaimRecoveryWorker(store, {
    async queryMailAttachmentGrant() {
      queryCalls += 1;
      return query;
    },
    async grantMailAttachments() { throw new Error("must not grant"); }
  }, workerConfig({ claimRecoveryPollIntervalMs: 5 }), null);

  await worker.onModuleInit();
  await createWorkflow(store);
  await new Promise((resolve) => setTimeout(resolve, 25));
  assert.equal(queryCalls, 1);
  releaseQuery!(succeededQuery());
  await new Promise((resolve) => setTimeout(resolve, 5));
  await worker.onModuleDestroy();
  assert.equal((await store.getMailClaimWorkflow("mail-1")).status, "claimed");
});

test("multiple workers compete for one recovery lease", async () => {
  const store = new DbMailStore(null);
  await createWorkflow(store);
  const [first, second] = await Promise.all([
    store.reserveMailClaimRecoveries(10, { leaseOwner: "a", leaseMs: 60_000, maxAttempts: 5 }),
    store.reserveMailClaimRecoveries(10, { leaseOwner: "b", leaseMs: 60_000, maxAttempts: 5 })
  ]);

  assert.equal(first.workflows.length + second.workflows.length, 1);
  assert.equal((await store.getMailClaimWorkflow("mail-1")).recovery_attempts, 1);
});

test("expired recovery lease is taken over and stale owner is fenced", async () => {
  const store = new DbMailStore(null);
  await createWorkflow(store);
  const first = await store.reserveMailClaimRecoveries(1, { leaseOwner: "a", leaseMs: 60_000, maxAttempts: 5 });
  const firstToken = first.workflows[0].recovery_lease_token;
  store.memoryClaimWorkflows.get("mail-1").recovery_lease_expires_at = new Date(Date.now() - 1);
  const second = await store.reserveMailClaimRecoveries(1, { leaseOwner: "b", leaseMs: 60_000, maxAttempts: 5 });

  assert.equal(second.workflows[0].recovery_lease_taken_over, true);
  assert.notEqual(second.workflows[0].recovery_lease_token, firstToken);
  assert.equal(await store.rescheduleMailClaimRecovery("mail-1", firstToken, {
    status: "reconciliation_pending",
    delayMs: 0
  }), null);
});

test("succeeded reconciliation completes mail with zero grant", async () => {
  const store = new DbMailStore(null);
  await createWorkflow(store);
  let grants = 0;
  const worker = new ClaimRecoveryWorker(store, {
    async queryMailAttachmentGrant() { return succeededQuery(); },
    async grantMailAttachments() { grants += 1; }
  }, workerConfig(), null);

  const result = await worker.processRecoveries();

  assert.equal(result.recovered, 1);
  assert.equal(grants, 0);
  assert.deepEqual((await store.getMailClaimWorkflow("mail-1")).result_summary, succeededQuery().resultSummary);
});

test("first recovery scan reports age from the pre-lease unknown state", async () => {
  const store = new DbMailStore(null);
  await createWorkflow(store);
  const stuckSince = new Date(Date.now() - 120_000);
  const persisted = store.memoryClaimWorkflows.get("mail-1");
  persisted.updated_at = stuckSince;
  persisted.next_recovery_at = stuckSince;
  const unknownAges: number[] = [];
  const recoveryDurations: number[] = [];
  const worker = new ClaimRecoveryWorker(store, {
    async queryMailAttachmentGrant() { return succeededQuery(); },
    async grantMailAttachments() { throw new Error("must not grant"); }
  }, workerConfig(), {
    recordMailClaimRecoveryUnknownAge(ageMs: number) { unknownAges.push(ageMs); },
    recordMailClaimRecoveryDuration(durationMs: number) { recoveryDurations.push(durationMs); }
  });

  await worker.processRecoveries();
  const workflow = await store.getMailClaimWorkflow("mail-1");

  assert.equal(unknownAges.length, 1);
  assert.ok(unknownAges[0] >= 119_000 && unknownAges[0] < 125_000);
  assert.equal(recoveryDurations.length, 1);
  assert.ok(recoveryDurations[0] >= 119_000 && recoveryDurations[0] < 125_000);
  assert.equal(new Date(workflow.recovery_started_at).getTime(), stuckSince.getTime());
});

test("not_seen retries the original frozen request", async () => {
  const store = new DbMailStore(null);
  await createWorkflow(store);
  const grants: any[] = [];
  const worker = new ClaimRecoveryWorker(store, {
    async queryMailAttachmentGrant(requestId: string, requestFingerprint: string) {
      return {
        ok: true,
        queryStatus: "not_seen",
        requestId,
        requestFingerprint,
        resultState: "not_applied",
        traceId: "2".repeat(32),
        instanceIds: ["game-a"]
      };
    },
    async grantMailAttachments(...args: any[]) {
      grants.push(args);
      return {
        traceId: args[4].traceId,
        instanceId: "game-a",
        resultSummary: { characterId: args[0], source: "mail-claim", items: args[2] }
      };
    }
  }, workerConfig(), null);

  await worker.processRecoveries();

  assert.equal(grants.length, 1);
  assert.equal(grants[0][0], "chr_1");
  assert.equal(grants[0][1], "mail_claim:mail-1");
  assert.deepEqual(grants[0][2], frozenItems);
  assert.equal(grants[0][4].requestFingerprint, fingerprint);
  assert.equal((await store.getMailClaimWorkflow("mail-1")).attempts, 2);
});

test("query unavailable backs off without grant", async () => {
  const store = new DbMailStore(null);
  await createWorkflow(store);
  let grants = 0;
  const worker = new ClaimRecoveryWorker(store, {
    async queryMailAttachmentGrant() {
      return {
        queryStatus: "result_unavailable",
        requestId: "mail_claim:mail-1",
        requestFingerprint: fingerprint,
        resultState: "unknown",
        errorCode: "GAME_ADMIN_READ_TIMEOUT",
        traceId: "2".repeat(32),
        instanceIds: ["game-a"]
      };
    },
    async grantMailAttachments() { grants += 1; }
  }, workerConfig(), null);

  const before = Date.now();
  await worker.processRecoveries();
  const workflow = await store.getMailClaimWorkflow("mail-1");

  assert.equal(grants, 0);
  assert.equal(workflow.status, "reconciliation_pending");
  assert.ok(new Date(workflow.next_recovery_at).getTime() >= before + 900);
  assert.equal(workflow.last_query_status, "result_unavailable");
});

test("unknown grant result returns to reconciliation instead of direct retry", async () => {
  const store = new DbMailStore(null);
  await createWorkflow(store, "retryable_failure");
  const worker = new ClaimRecoveryWorker(store, {
    async queryMailAttachmentGrant() { throw new Error("direct retry must not query first"); },
    async grantMailAttachments() {
      const error: any = new Error("response lost");
      error.code = "GAME_ADMIN_READ_TIMEOUT";
      error.errorCategory = "RESULT_UNKNOWN";
      error.resultState = "unknown";
      error.requestWritten = true;
      throw error;
    }
  }, workerConfig(), null);

  await worker.processRecoveries();

  const workflow = await store.getMailClaimWorkflow("mail-1");
  assert.equal(workflow.status, "reconciliation_pending");
  assert.equal(workflow.last_result_state, "unknown");
});

test("recovery attempt limit moves workflow to manual review", async () => {
  const store = new DbMailStore(null);
  await createWorkflow(store);
  store.memoryClaimWorkflows.get("mail-1").recovery_attempts = 5;
  const worker = new ClaimRecoveryWorker(store, {
    async queryMailAttachmentGrant() { throw new Error("must not query after exhaustion"); },
    async grantMailAttachments() { throw new Error("must not grant after exhaustion"); }
  }, workerConfig({ claimRecoveryMaxAttempts: 5 }), null);

  const result = await worker.processRecoveries();
  const workflow = await store.getMailClaimWorkflow("mail-1");

  assert.equal(result.manual, 1);
  assert.equal(workflow.status, "manual_review");
  assert.equal(workflow.last_error_code, "GAME_ADMIN_READ_TIMEOUT");
  assert.deepEqual(workflow.attachments_snapshot, frozenItems);
  assert.equal((await store.getMailById("mail-1")).status, "claiming");
});

test("graceful stop waits only up to its configured bound", async () => {
  const store = new DbMailStore(null);
  await createWorkflow(store);
  let releaseQuery: (value: any) => void;
  const query = new Promise((resolve) => { releaseQuery = resolve; });
  const worker = new ClaimRecoveryWorker(store, {
    async queryMailAttachmentGrant() { return query; },
    async grantMailAttachments() { throw new Error("must not grant"); }
  }, workerConfig({ claimRecoveryShutdownTimeoutMs: 10 }), null);

  const active = worker.processRecoveries();
  const startedAt = Date.now();
  await worker.onModuleDestroy();
  assert.ok(Date.now() - startedAt < 200);
  releaseQuery!({
    queryStatus: "result_unavailable",
    requestId: "mail_claim:mail-1",
    requestFingerprint: fingerprint,
    resultState: "unknown",
    traceId: "2".repeat(32),
    instanceIds: ["game-a"]
  });
  await active;
});
