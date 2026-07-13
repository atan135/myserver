import assert from "node:assert/strict";
import test from "node:test";

import { DbMailStore } from "../db-store.js";
import { computeGrantRequestFingerprint } from "../game-admin-client.js";
import { configureLogger } from "../logger.js";
import { MailsController } from "./mails.controller.js";
import { MailsService } from "./mails.service.js";

configureLogger({
  appName: "mail-service-test",
  logEnableConsole: false,
  logEnableFile: false,
  logLevel: "fatal"
});

function createMail(overrides: Record<string, any> = {}) {
  return {
    id: 1,
    mail_id: "mail-1",
    sender_type: "system",
    sender_id: "system",
    sender_name: "系统",
    from_player_id: "system",
    to_player_id: "player-1",
    title: "Reward",
    content: "",
    status: "unread",
    attachments: [{ type: "item", id: 1001, count: 2, binded: true }],
    mail_type: "system",
    created_by_type: "system",
    created_by_id: "system",
    created_by_name: "系统",
    created_at: new Date(),
    read_at: null,
    claimed_at: null,
    expires_at: null,
    ...overrides
  };
}

function createClaimMetrics() {
  const counts = { route: 0, grant: 0, unknown: 0, retryable: 0, permanent: 0 };
  return {
    counts,
    metrics: {
      recordMailClaimRouteUnavailable() { counts.route += 1; },
      recordMailClaimGrantFailure() { counts.grant += 1; },
      recordMailClaimResultUnknown() { counts.unknown += 1; },
      recordMailClaimRetryableFailure() { counts.retryable += 1; },
      recordMailClaimPermanentFailure() { counts.permanent += 1; }
    }
  };
}

function createService({
  mail = createMail(),
  grant,
  config = {},
  metrics = null,
  storeOptions = {}
}: {
  mail?: any;
  grant?: (...args: any[]) => Promise<any>;
  config?: any;
  metrics?: any;
  storeOptions?: any;
} = {}) {
  const calls: any[][] = [];
  const mailStore = new DbMailStore(null, storeOptions);
  if (mail) {
    mailStore.memory.set(mail.mail_id, mail);
  }
  const gameAdminClient = {
    async grantMailAttachments(...args: any[]) {
      calls.push(args);
      if (grant) {
        return grant(...args);
      }
      const [characterId, , attachments, , options] = args;
      return {
        ok: true,
        applied: true,
        traceId: options.traceId,
        instanceId: "game-1",
        resultSummary: { characterId, source: "mail-claim", items: attachments }
      };
    }
  };
  return {
    calls,
    mailStore,
    service: new MailsService(
      mailStore,
      {},
      gameAdminClient,
      { claimLeaseMs: 30_000, serviceInstanceId: "mail-test", ...config },
      metrics
    )
  };
}

async function reserveWorkflow(mailStore: any, overrides: Record<string, any> = {}, options: any = {}) {
  const attachments = [{ itemId: 1001, count: 2, binded: true }];
  return mailStore.reserveMailClaimWorkflow({
    mailId: "mail-1",
    playerId: "player-1",
    characterId: "chr_1",
    requestId: "mail_claim:mail-1",
    attachmentsSnapshot: attachments,
    attachmentsFingerprint: computeGrantRequestFingerprint("mail-1", "chr_1", attachments),
    expectedAttachments: createMail().attachments,
    traceId: "0123456789abcdef0123456789abcdef",
    ...overrides
  }, {
    leaseMs: 30_000,
    leaseOwner: "mail-a",
    ...options
  });
}

test("claim returns claimed compatibility fields without downstream grant", async () => {
  const { service, calls } = createService({
    mail: createMail({ status: "claimed", claimed_at: new Date("2026-06-01T00:00:00.000Z") })
  });

  const result = await service.claim("mail-1", "player-1", "chr_1");

  assert.equal(result.claim_status, "claimed");
  assert.equal(result.already_claimed, true);
  assert.equal(result.claimed, false);
  assert.equal(result._http_status, 200);
  assert.equal(calls.length, 0);
});

test("active claim lease returns processing instead of issuing a concurrent grant", async () => {
  const { service, calls, mailStore } = createService();
  await reserveWorkflow(mailStore);

  const result = await service.claim("mail-1", "player-1", "chr_1");

  assert.equal(result.claim_status, "processing");
  assert.equal(result._http_status, 202);
  assert.equal(result.request_id, "mail_claim:mail-1");
  assert.equal(calls.length, 0);
});

test("claim requires authenticated character id before workflow reservation", async () => {
  const { service, calls, mailStore } = createService();

  await assert.rejects(
    () => service.claim("mail-1", "player-1", ""),
    (error: any) => error?.getResponse?.()?.error === "MISSING_CHARACTER_ID"
  );
  assert.equal(calls.length, 0);
  assert.equal(await mailStore.getMailClaimWorkflow("mail-1"), null);
});

test("claim freezes canonical request identity and completes the workflow", async () => {
  const { service, calls, mailStore } = createService();

  const result = await service.claim("mail-1", "player-1", " chr_1 ", {
    character_id: "chr_attacker",
    mail_id: "mail_attacker",
    request_id: "attacker-request",
    source: "gm",
    attachments: [{ type: "item", id: 9999, count: 9999 }]
  });

  assert.equal(calls.length, 1);
  assert.deepEqual(calls[0].slice(0, 4), [
    "chr_1",
    "mail_claim:mail-1",
    [{ itemId: 1001, count: 2, binded: true }],
    "claim mail mail-1"
  ]);
  assert.equal(calls[0][4].targetInstanceId, "");
  assert.match(calls[0][4].traceId, /^[0-9a-f]{32}$/);
  assert.match(calls[0][4].requestFingerprint, /^sha256:[0-9a-f]{64}$/);
  assert.equal(result.claim_status, "claimed");
  assert.equal(result.claimed, true);
  const workflow = await mailStore.getMailClaimWorkflow("mail-1");
  assert.equal(workflow.status, "claimed");
  assert.equal(workflow.attempts, 1);
  assert.ok(workflow.completed_at);
});

test("claim ignores client attachment, request id, source, and fingerprint overrides", async () => {
  const { service, calls } = createService();

  await service.claim("mail-1", "player-1", "chr_1", {
    request_id: "attacker-request",
    source: "gm",
    attachments: [{ type: "item", id: 9999, count: 9999 }],
    requestFingerprint: "sha256:attacker"
  });

  assert.equal(calls[0][1], "mail_claim:mail-1");
  assert.deepEqual(calls[0][2], [{ itemId: 1001, count: 2, binded: true }]);
  assert.match(calls[0][4].requestFingerprint, /^sha256:[0-9a-f]{64}$/);
});

test("claim passes explicit local target and rejects it in strict discovery", async () => {
  const local = createService({
    config: { localDiscoveryFallbackEnabled: true, registryDiscoveryRequired: false }
  });
  await local.service.claim("mail-1", "player-1", "chr_1", { target_instance_id: "game-server-c" });
  assert.equal(local.calls[0][4].targetInstanceId, "game-server-c");

  const strict = createService({
    config: { localDiscoveryFallbackEnabled: false, registryDiscoveryRequired: true }
  });
  await assert.rejects(
    () => strict.service.claim("mail-1", "player-1", "chr_1", { targetInstanceId: "game-server-b" }),
    (error: any) => error?.getResponse?.()?.error === "CLIENT_TARGET_INSTANCE_FORBIDDEN"
  );
  assert.equal(strict.calls.length, 0);
});

test("claim accepts camelCase local targetInstanceId", async () => {
  const { service, calls } = createService({
    config: { localDiscoveryFallbackEnabled: true, registryDiscoveryRequired: false }
  });

  await service.claim("mail-1", "player-1", "chr_1", { targetInstanceId: "game-server-b" });

  assert.equal(calls[0][4].targetInstanceId, "game-server-b");
});

test("explicit not_applied route failure persists retryable state and reuses request identity", async () => {
  let attempt = 0;
  const claimMetrics = createClaimMetrics();
  const { service, calls, mailStore } = createService({
    metrics: claimMetrics.metrics,
    grant: async (...args: any[]) => {
      attempt += 1;
      if (attempt === 1) {
        const error: any = new Error("route missing");
        error.code = "MAIL_CLAIM_ROUTE_UNAVAILABLE";
        error.errorCategory = "ROUTE_UNAVAILABLE";
        error.resultState = "not_applied";
        error.retryable = true;
        error.requestWritten = false;
        error.traceId = args[4].traceId;
        throw error;
      }
      return {
        ok: true,
        traceId: args[4].traceId,
        resultSummary: { characterId: args[0], source: "mail-claim", items: args[2] }
      };
    }
  });

  const failed = await service.claim("mail-1", "player-1", "chr_1");
  assert.equal(failed.claim_status, "retryable_failure");
  assert.equal(failed._http_status, 503);
  assert.equal(failed.retryable, true);
  assert.equal((await mailStore.getMailById("mail-1")).status, "claiming");

  const retried = await service.claim("mail-1", "player-1", "chr_1");
  assert.equal(retried.claim_status, "claimed");
  assert.equal(calls.length, 2);
  assert.equal(calls[0][1], calls[1][1]);
  assert.deepEqual(calls[0][2], calls[1][2]);
  assert.equal((await mailStore.getMailClaimWorkflow("mail-1")).attempts, 2);
  assert.deepEqual(claimMetrics.counts, { route: 1, grant: 0, unknown: 0, retryable: 1, permanent: 0 });
});

test("connect refusal before request write is a retryable route failure", async () => {
  const claimMetrics = createClaimMetrics();
  const { service, mailStore } = createService({
    metrics: claimMetrics.metrics,
    grant: async () => {
      const error: any = new Error("connect ECONNREFUSED");
      error.code = "ECONNREFUSED";
      error.errorCategory = "ROUTE_UNAVAILABLE";
      error.resultState = "not_applied";
      error.retryable = true;
      error.requestWritten = false;
      throw error;
    }
  });

  const result = await service.claim("mail-1", "player-1", "chr_1");

  assert.equal(result.claim_status, "retryable_failure");
  assert.equal(result._http_status, 503);
  assert.equal(result.error, "MAIL_CLAIM_ROUTE_UNAVAILABLE");
  assert.equal(result.message, "Mail attachment claim could not be completed yet; retry later");
  assert.equal((await mailStore.getMailClaimWorkflow("mail-1")).last_error_code, "ECONNREFUSED");
  assert.deepEqual(claimMetrics.counts, { route: 1, grant: 0, unknown: 0, retryable: 1, permanent: 0 });
});

test("unclassified pre-write failure is persisted as retryable instead of releasing the mail", async () => {
  const { service, mailStore } = createService({
    grant: async () => {
      const error: any = new Error("target selection unavailable");
      error.code = "GAME_SERVER_ADMIN_TARGET_REQUIRED";
      error.requestWritten = false;
      throw error;
    }
  });

  const result = await service.claim("mail-1", "player-1", "chr_1");

  assert.equal(result.claim_status, "retryable_failure");
  assert.equal(result.error, "MAIL_CLAIM_RETRYABLE_FAILURE");
  assert.equal((await mailStore.getMailById("mail-1")).status, "claiming");
  assert.equal((await mailStore.getMailClaimWorkflow("mail-1")).last_error_code, "GAME_SERVER_ADMIN_TARGET_REQUIRED");
});

test("response timeout remains reconciliation_pending and player retries do not grant again", async () => {
  const claimMetrics = createClaimMetrics();
  const { service, calls, mailStore } = createService({
    metrics: claimMetrics.metrics,
    grant: async (...args: any[]) => {
      const error: any = new Error("game-server admin read timeout");
      error.code = "GAME_ADMIN_READ_TIMEOUT";
      error.errorCategory = "RESULT_UNKNOWN";
      error.resultState = "unknown";
      error.retryable = true;
      error.requestWritten = true;
      error.traceId = args[4].traceId;
      throw error;
    }
  });

  const first = await service.claim("mail-1", "player-1", "chr_1");
  const second = await service.claim("mail-1", "player-1", "chr_1");

  assert.equal(first.claim_status, "reconciliation_pending");
  assert.equal(first._http_status, 202);
  assert.equal(first.retryable, false);
  assert.equal(second.claim_status, "reconciliation_pending");
  assert.equal(calls.length, 1);
  const workflow = await mailStore.getMailClaimWorkflow("mail-1");
  assert.equal(workflow.status, "reconciliation_pending");
  assert.equal(workflow.last_result_state, "unknown");
  assert.equal(workflow.lease_token, null);
  assert.deepEqual(claimMetrics.counts, { route: 0, grant: 1, unknown: 1, retryable: 0, permanent: 0 });
});

test("permanent business failure remains explainable and retries the frozen snapshot", async () => {
  let attempt = 0;
  const { service, calls, mailStore } = createService({
    grant: async (...args: any[]) => {
      attempt += 1;
      if (attempt === 1) {
        const error: any = new Error("item not found");
        error.code = "ITEM_NOT_FOUND";
        error.errorCategory = "PERMANENT_FAILURE";
        error.resultState = "not_applied";
        error.retryable = false;
        error.requestWritten = true;
        throw error;
      }
      return {
        ok: true,
        traceId: args[4].traceId,
        resultSummary: { characterId: args[0], source: "mail-claim", items: args[2] }
      };
    }
  });

  const failed = await service.claim("mail-1", "player-1", "chr_1");
  assert.equal(failed.claim_status, "permanent_failure");
  assert.equal(failed._http_status, 422);
  assert.equal(failed.error, "ITEM_NOT_FOUND");

  mailStore.memory.get("mail-1").attachments = [{ type: "item", id: 9999, count: 99 }];
  const retried = await service.claim("mail-1", "player-1", "chr_1");
  assert.equal(retried.claim_status, "claimed");
  assert.deepEqual(calls[1][2], [{ itemId: 1001, count: 2, binded: true }]);
  assert.equal(calls[1][1], "mail_claim:mail-1");
});

test("started workflow can finish from its frozen snapshot after hard mail deletion", async () => {
  let attempt = 0;
  const { service, mailStore } = createService({
    grant: async (...args: any[]) => {
      attempt += 1;
      if (attempt === 1) {
        const error: any = new Error("inventory busy");
        error.code = "INVENTORY_BUSY";
        error.errorCategory = "RETRYABLE_FAILURE";
        error.resultState = "not_applied";
        error.retryable = true;
        throw error;
      }
      return {
        ok: true,
        traceId: args[4].traceId,
        resultSummary: { characterId: args[0], source: "mail-claim", items: args[2] }
      };
    }
  });

  await service.claim("mail-1", "player-1", "chr_1");
  assert.equal(await mailStore.deleteMail("mail-1"), true);
  const result = await service.claim("mail-1", "player-1", "chr_1");

  assert.equal(result.claim_status, "claimed");
  assert.equal(result.status, "claimed");
  assert.equal((await mailStore.getMailClaimWorkflow("mail-1")).status, "claimed");
  assert.equal(await mailStore.getMailById("mail-1"), null);
});

test("existing workflow rejects a different authenticated character", async () => {
  const { service, mailStore, calls } = createService();
  await reserveWorkflow(mailStore, {}, { leaseMs: 1 });
  mailStore.memoryClaimWorkflows.get("mail-1").lease_expires_at = new Date(Date.now() - 1);

  const result = await service.claim("mail-1", "player-1", "chr_2");

  assert.equal(result.claim_status, "permanent_failure");
  assert.equal(result.error, "MAIL_CLAIM_CHARACTER_MISMATCH");
  assert.equal(result._http_status, 409);
  assert.equal(calls.length, 0);
});

test("manual review workflow remains claiming and cannot be reacquired by the player", async () => {
  const { service, mailStore, calls } = createService();
  await reserveWorkflow(mailStore, {}, { leaseMs: 1 });
  mailStore.memoryClaimWorkflows.get("mail-1").lease_expires_at = new Date(Date.now() - 1);
  const recovery = await mailStore.reserveMailClaimRecoveries(1, {
    leaseOwner: "recovery-a",
    leaseMs: 30_000,
    maxAttempts: 5
  });
  await mailStore.markMailClaimRecoveryManualReview(
    "mail-1",
    recovery.workflows[0].recovery_lease_token,
    {
      errorCode: "REQUEST_FINGERPRINT_CONFLICT",
      errorCategory: "PERMANENT_FAILURE",
      resultState: "not_applied",
      message: "query conflict"
    }
  );

  const result = await service.claim("mail-1", "player-1", "chr_1");

  assert.equal(result.claim_status, "manual_review");
  assert.equal(result._http_status, 409);
  assert.equal(result.error, "MAIL_CLAIM_MANUAL_REVIEW_REQUIRED");
  assert.equal(result.retryable, false);
  assert.equal((await mailStore.getMailById("mail-1")).status, "claiming");
  assert.equal(calls.length, 0);
});

test("mail completion persistence failure never releases a successfully granted workflow", async () => {
  const { service, mailStore, calls } = createService();
  mailStore.completeMailClaimWorkflow = async () => {
    throw new Error("database commit unavailable");
  };

  await assert.rejects(
    () => service.claim("mail-1", "player-1", "chr_1"),
    /database commit unavailable/
  );

  assert.equal(calls.length, 1);
  const workflow = await mailStore.getMailClaimWorkflow("mail-1");
  assert.equal(workflow.status, "processing");
  assert.ok(workflow.lease_token);
  assert.equal((await mailStore.getMailById("mail-1")).status, "claiming");
});

test("late failure from a stale attempt reports the current claimed result", async () => {
  const routeError: any = new Error("late route error");
  routeError.code = "ROUTE_UNAVAILABLE";
  routeError.errorCategory = "ROUTE_UNAVAILABLE";
  routeError.resultState = "not_applied";
  routeError.retryable = true;
  routeError.requestWritten = false;
  const { service, mailStore } = createService({
    grant: async () => { throw routeError; }
  });
  const originalRecordFailure = mailStore.recordMailClaimWorkflowFailure.bind(mailStore);
  mailStore.recordMailClaimWorkflowFailure = async (mailId: string, leaseToken: string, failure: any) => {
    await mailStore.completeMailClaimWorkflow(mailId, leaseToken, {
      resultSummary: { characterId: "chr_1", source: "mail-claim", items: [] }
    });
    return originalRecordFailure(mailId, leaseToken, failure);
  };

  const result = await service.claim("mail-1", "player-1", "chr_1");

  assert.equal(result.claim_status, "claimed");
  assert.equal(result._http_status, 200);
  assert.equal(result.ok, true);
  assert.equal(result.already_claimed, true);
  assert.equal(result.error, undefined);
});

test("claim controller applies workflow HTTP status without leaking internal metadata", async () => {
  const controller = new MailsController(
    {
      async claim() {
        return { _http_status: 202, ok: true, claim_status: "reconciliation_pending" };
      }
    } as any,
    { mailPlayerAuthRequired: false },
    null
  );
  const response = {
    statusCode: 0,
    status(value: number) { this.statusCode = value; }
  };

  const result = await controller.claim(
    "mail-1",
    {},
    { player_id: "player-1", character_id: "chr_1" },
    response
  );

  assert.equal(response.statusCode, 202);
  assert.equal(result.claim_status, "reconciliation_pending");
  assert.equal("_http_status" in result, false);
});

test("claim responses never expose persisted endpoint, Redis, or token diagnostics", async () => {
  const sensitiveMessage =
    `connect ECONNREFUSED 127.0.0.1:7500 redis://admin:secret@127.0.0.1:6379 ` +
    `GAME_ADMIN_TOKEN=token-like-secret ${"x".repeat(600)}`;
  const { service, mailStore, calls } = createService({
    grant: async () => {
      const error: any = new Error(sensitiveMessage);
      error.code = "ECONNRESET";
      error.errorCategory = "RESULT_UNKNOWN";
      error.resultState = "unknown";
      error.retryable = true;
      error.requestWritten = true;
      throw error;
    }
  });

  const first = await service.claim("mail-1", "player-1", "chr_1");
  const workflow = await mailStore.getMailClaimWorkflow("mail-1");
  const controller = new MailsController(
    service,
    { mailPlayerAuthRequired: false },
    null
  );
  const response = {
    statusCode: 0,
    status(value: number) { this.statusCode = value; }
  };
  const second = await controller.claim(
    "mail-1",
    {},
    { player_id: "player-1", character_id: "chr_1" },
    response
  );

  assert.equal(workflow.last_error_message, sensitiveMessage.slice(0, 512));
  assert.equal(first.error, "MAIL_CLAIM_RECONCILIATION_PENDING");
  assert.equal(first.claim_status, "reconciliation_pending");
  assert.equal(first._http_status, 202);
  assert.equal(second.error, "MAIL_CLAIM_RECONCILIATION_PENDING");
  assert.equal(second.claim_status, "reconciliation_pending");
  assert.equal(response.statusCode, 202);
  assert.equal(calls.length, 1);
  for (const playerResponse of [first, second]) {
    const serialized = JSON.stringify(playerResponse);
    assert.doesNotMatch(serialized, /127\.0\.0\.1|redis:\/\/|GAME_ADMIN_TOKEN|token-like-secret|ECONNREFUSED/);
    assert.equal(playerResponse.message, "Mail attachment claim result is being verified");
  }
});
