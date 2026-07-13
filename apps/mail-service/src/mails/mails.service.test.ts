import assert from "node:assert/strict";
import test from "node:test";

import { configureLogger } from "../logger.js";
import { MailsService } from "./mails.service.js";

configureLogger({
  appName: "mail-service-test",
  logEnableConsole: false,
  logEnableFile: false,
  logLevel: "fatal"
});

function createMail(overrides: Record<string, any> = {}) {
  return {
    mail_id: "mail-1",
    to_player_id: "player-1",
    status: "unread",
    attachments: [{ type: "item", id: 1001, count: 2, binded: true }],
    read_at: null,
    claimed_at: null,
    expires_at: null,
    ...overrides
  };
}

function getErrorCode(error: any) {
  return error?.response?.error || error?.getResponse?.()?.error || error?.code;
}

function createService({
  mail = createMail(),
  beginResult,
  completeResult,
  grantError,
  config = {},
  metrics = null
}: {
  mail?: any;
  beginResult?: any;
  completeResult?: any;
  grantError?: Error;
  config?: any;
  metrics?: any;
} = {}) {
  const calls: Record<string, any[]> = {
    grant: [],
    complete: [],
    release: []
  };

  const mailStore = {
    async getMailById(mailId: string) {
      assert.equal(mailId, mail.mail_id);
      return mail;
    },
    async beginClaimAttachments(mailId: string) {
      assert.equal(mailId, mail.mail_id);
      return beginResult ?? {
        reserved: true,
        alreadyClaimed: false,
        inProgress: false,
        mail
      };
    },
    async completeClaimAttachments(mailId: string) {
      calls.complete.push(mailId);
      return completeResult ?? {
        claimed: true,
        mail: createMail({
          status: "claimed",
          read_at: new Date("2026-06-01T00:00:00.000Z"),
          claimed_at: new Date("2026-06-01T00:00:00.000Z")
        })
      };
    },
    async releaseClaimAttachments(mailId: string) {
      calls.release.push(mailId);
      return true;
    }
  };

  const gameAdminClient = {
    async grantMailAttachments(...args: any[]) {
      calls.grant.push(args);
      if (grantError) {
        throw grantError;
      }
      return { ok: true };
    }
  };

  return {
    calls,
    service: new MailsService(mailStore, {}, gameAdminClient, config, metrics)
  };
}

function createClaimMetrics() {
  const counts = { route: 0, grant: 0 };
  return {
    counts,
    metrics: {
      recordMailClaimRouteUnavailable() { counts.route += 1; },
      recordMailClaimGrantFailure() { counts.grant += 1; }
    }
  };
}

test("claim returns already_claimed without downstream grant when mail is already claimed", async () => {
  const mail = createMail({
    status: "claimed",
    claimed_at: new Date("2026-06-01T00:00:00.000Z")
  });
  const { service, calls } = createService({
    mail,
    beginResult: {
      reserved: false,
      alreadyClaimed: true,
      inProgress: false,
      mail
    }
  });

  const result = await service.claim("mail-1", "player-1", "chr_1");

  assert.equal(result.already_claimed, true);
  assert.equal(result.claimed, false);
  assert.equal(calls.grant.length, 0);
  assert.equal(calls.complete.length, 0);
  assert.equal(calls.release.length, 0);
});

test("claim rejects in-progress reservations without downstream grant", async () => {
  const { service, calls } = createService({
    beginResult: {
      reserved: false,
      alreadyClaimed: false,
      inProgress: true,
      mail: createMail()
    }
  });

  await assert.rejects(
    () => service.claim("mail-1", "player-1", "chr_1"),
    (error: any) => {
      assert.equal(getErrorCode(error), "MAIL_CLAIM_IN_PROGRESS");
      assert.equal(error.getStatus?.(), 409);
      return true;
    }
  );
  assert.equal(calls.grant.length, 0);
});

test("claim rejects unreserved begin result as in-progress", async () => {
  const { service, calls } = createService({
    beginResult: {
      reserved: false,
      alreadyClaimed: false,
      inProgress: false,
      mail: createMail()
    }
  });

  await assert.rejects(
    () => service.claim("mail-1", "player-1", "chr_1"),
    (error: any) => getErrorCode(error) === "MAIL_CLAIM_IN_PROGRESS"
  );
  assert.equal(calls.grant.length, 0);
});

test("claim requires authenticated character id before downstream grant", async () => {
  const { service, calls } = createService();

  await assert.rejects(
    () => service.claim("mail-1", "player-1", ""),
    (error: any) => {
      assert.equal(getErrorCode(error), "MISSING_CHARACTER_ID");
      assert.equal(error.getStatus?.(), 400);
      return true;
    }
  );

  assert.equal(calls.grant.length, 0);
  assert.equal(calls.complete.length, 0);
  assert.equal(calls.release.length, 0);
});

test("claim grants once with stable request id, source semantics, and completes claimed state", async () => {
  const { service, calls } = createService();

  const result = await service.claim("mail-1", "player-1", " chr_1 ");

  assert.equal(calls.grant.length, 1);
  assert.deepEqual(calls.grant[0], [
    "chr_1",
    "mail_claim:mail-1",
    [{ itemId: 1001, count: 2, binded: true }],
    "claim mail mail-1",
    { targetInstanceId: "" }
  ]);
  assert.deepEqual(calls.complete, ["mail-1"]);
  assert.equal(result.claimed, true);
  assert.equal(result.already_claimed, false);
  assert.equal(result.status, "claimed");
});

test("claim ignores client attempts to override the authoritative grant payload", async () => {
  const { service, calls } = createService();

  await service.claim("mail-1", "player-1", "chr_1", {
    character_id: "chr_attacker",
    mail_id: "mail_attacker",
    request_id: "attacker-request",
    source: "gm",
    attachments: [{ type: "item", id: 9999, count: 9999 }],
    requestFingerprint: "sha256:attacker"
  });

  assert.deepEqual(calls.grant[0], [
    "chr_1",
    "mail_claim:mail-1",
    [{ itemId: 1001, count: 2, binded: true }],
    "claim mail mail-1",
    { targetInstanceId: "" }
  ]);
});

test("claim passes explicit targetInstanceId to downstream grant", async () => {
  const { service, calls } = createService({
    config: { localDiscoveryFallbackEnabled: true, registryDiscoveryRequired: false }
  });

  await service.claim("mail-1", "player-1", "chr_1", { targetInstanceId: "game-server-b" });

  assert.equal(calls.grant.length, 1);
  assert.equal(calls.grant[0][4].targetInstanceId, "game-server-b");
  assert.deepEqual(calls.complete, ["mail-1"]);
  assert.equal(calls.release.length, 0);
});

test("claim accepts snake_case target_instance_id for downstream grant", async () => {
  const { service, calls } = createService({
    config: { localDiscoveryFallbackEnabled: true, registryDiscoveryRequired: false }
  });

  await service.claim("mail-1", "player-1", "chr_1", { target_instance_id: "game-server-c" });

  assert.equal(calls.grant[0][4].targetInstanceId, "game-server-c");
});

test("claim rejects client targetInstanceId before reservation in strict discovery", async () => {
  const { service, calls } = createService({
    config: { localDiscoveryFallbackEnabled: false, registryDiscoveryRequired: true }
  });

  await assert.rejects(
    () => service.claim("mail-1", "player-1", "chr_1", { targetInstanceId: "game-server-b" }),
    (error: any) => {
      assert.equal(getErrorCode(error), "CLIENT_TARGET_INSTANCE_FORBIDDEN");
      assert.equal(error.getStatus?.(), 403);
      return true;
    }
  );

  assert.equal(calls.grant.length, 0);
  assert.equal(calls.release.length, 0);
  assert.equal(calls.complete.length, 0);
});

test("claim maps route unavailable separately and records route metric", async () => {
  const grantError = new Error("route missing");
  (grantError as any).code = "MAIL_CLAIM_ROUTE_UNAVAILABLE";
  (grantError as any).errorCategory = "ROUTE_UNAVAILABLE";
  (grantError as any).requestId = "mail_claim:mail-1";
  (grantError as any).traceId = "0123456789abcdef0123456789abcdef";
  let routeFailures = 0;
  let grantFailures = 0;
  const { service, calls } = createService({
    grantError,
    metrics: {
      recordMailClaimRouteUnavailable() { routeFailures += 1; },
      recordMailClaimGrantFailure() { grantFailures += 1; }
    }
  });

  await assert.rejects(
    () => service.claim("mail-1", "player-1", "chr_1"),
    (error: any) => {
      assert.equal(getErrorCode(error), "MAIL_CLAIM_ROUTE_UNAVAILABLE");
      assert.equal(error.getStatus?.(), 503);
      return true;
    }
  );

  assert.equal(routeFailures, 1);
  assert.equal(grantFailures, 0);
  assert.deepEqual(calls.release, ["mail-1"]);
});

test("claim records registry discovery failure as route unavailable", async () => {
  const grantError = new Error("registry scan failed");
  (grantError as any).code = "REDIS_SCAN_FAILED";
  (grantError as any).errorCategory = "ROUTE_UNAVAILABLE";
  (grantError as any).resultState = "not_applied";
  (grantError as any).retryable = true;
  (grantError as any).requestPhase = "discovery";
  (grantError as any).requestWritten = false;
  const claimMetrics = createClaimMetrics();
  const { service, calls } = createService({
    grantError,
    metrics: claimMetrics.metrics
  });

  await assert.rejects(
    () => service.claim("mail-1", "player-1", "chr_1"),
    (error: any) => {
      assert.equal(getErrorCode(error), "MAIL_CLAIM_ROUTE_UNAVAILABLE");
      assert.equal(error.getStatus?.(), 503);
      return true;
    }
  );

  assert.deepEqual(claimMetrics.counts, { route: 1, grant: 0 });
  assert.deepEqual(calls.release, ["mail-1"]);
});

test("claim records connect refusal before request write as route unavailable", async () => {
  const grantError = new Error("connect ECONNREFUSED");
  (grantError as any).code = "ECONNREFUSED";
  (grantError as any).errorCategory = "ROUTE_UNAVAILABLE";
  (grantError as any).resultState = "not_applied";
  (grantError as any).retryable = true;
  (grantError as any).requestPhase = "connect";
  (grantError as any).requestWritten = false;
  const claimMetrics = createClaimMetrics();
  const { service, calls } = createService({
    grantError,
    metrics: claimMetrics.metrics
  });

  await assert.rejects(
    () => service.claim("mail-1", "player-1", "chr_1"),
    (error: any) => {
      assert.equal(getErrorCode(error), "MAIL_CLAIM_ROUTE_UNAVAILABLE");
      assert.equal(error.getStatus?.(), 503);
      return true;
    }
  );

  assert.deepEqual(claimMetrics.counts, { route: 1, grant: 0 });
  assert.deepEqual(calls.release, ["mail-1"]);
});

test("claim records response timeout after request write as grant result failure", async () => {
  const grantError = new Error("game-server admin read timeout");
  (grantError as any).code = "GAME_ADMIN_READ_TIMEOUT";
  (grantError as any).errorCategory = "RESULT_UNKNOWN";
  (grantError as any).resultState = "unknown";
  (grantError as any).retryable = true;
  (grantError as any).requestPhase = "response_read";
  (grantError as any).requestWritten = true;
  const claimMetrics = createClaimMetrics();
  const { service, calls } = createService({
    grantError,
    metrics: claimMetrics.metrics
  });

  await assert.rejects(
    () => service.claim("mail-1", "player-1", "chr_1"),
    (error: any) => {
      assert.equal(getErrorCode(error), "GAME_SERVER_GRANT_FAILED");
      assert.equal(error.getStatus?.(), 502);
      return true;
    }
  );

  assert.deepEqual(claimMetrics.counts, { route: 0, grant: 1 });
  assert.deepEqual(calls.release, ["mail-1"]);
});

test("claim records ITEM_NOT_FOUND business failure as grant failure", async () => {
  const grantError = new Error("item not found");
  (grantError as any).code = "ITEM_NOT_FOUND";
  (grantError as any).errorCategory = "PERMANENT_FAILURE";
  (grantError as any).resultState = "not_applied";
  (grantError as any).retryable = false;
  (grantError as any).requestWritten = true;
  const claimMetrics = createClaimMetrics();
  const { service, calls } = createService({
    grantError,
    metrics: claimMetrics.metrics
  });

  await assert.rejects(
    () => service.claim("mail-1", "player-1", "chr_1"),
    (error: any) => {
      assert.equal(getErrorCode(error), "GAME_SERVER_GRANT_FAILED");
      assert.equal(error.getStatus?.(), 502);
      return true;
    }
  );

  assert.equal(calls.grant.length, 1);
  assert.deepEqual(claimMetrics.counts, { route: 0, grant: 1 });
  assert.deepEqual(calls.release, ["mail-1"]);
  assert.equal(calls.complete.length, 0);
});

test("claim releases reservation when registry has multiple targets but targetInstanceId is missing", async () => {
  const grantError = new Error("multiple game-server admin endpoints are available; targetInstanceId is required");
  (grantError as any).code = "GAME_SERVER_ADMIN_TARGET_REQUIRED";
  const { service, calls } = createService({ grantError });

  await assert.rejects(
    () => service.claim("mail-1", "player-1", "chr_1"),
    (error: any) => {
      assert.equal(getErrorCode(error), "GAME_SERVER_GRANT_FAILED");
      assert.equal(error.getStatus?.(), 502);
      return true;
    }
  );

  assert.equal(calls.grant.length, 1);
  assert.equal(calls.grant[0][4].targetInstanceId, "");
  assert.deepEqual(calls.release, ["mail-1"]);
  assert.equal(calls.complete.length, 0);
});
