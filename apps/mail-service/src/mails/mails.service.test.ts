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
  grantError
}: {
  mail?: any;
  beginResult?: any;
  completeResult?: any;
  grantError?: Error;
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
    service: new MailsService(mailStore, {}, gameAdminClient)
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

test("claim passes explicit targetInstanceId to downstream grant", async () => {
  const { service, calls } = createService();

  await service.claim("mail-1", "player-1", "chr_1", { targetInstanceId: "game-server-b" });

  assert.equal(calls.grant.length, 1);
  assert.equal(calls.grant[0][4].targetInstanceId, "game-server-b");
  assert.deepEqual(calls.complete, ["mail-1"]);
  assert.equal(calls.release.length, 0);
});

test("claim accepts snake_case target_instance_id for downstream grant", async () => {
  const { service, calls } = createService();

  await service.claim("mail-1", "player-1", "chr_1", { target_instance_id: "game-server-c" });

  assert.equal(calls.grant[0][4].targetInstanceId, "game-server-c");
});

test("claim releases reservation and maps downstream grant failure", async () => {
  const grantError = new Error("grant failed");
  (grantError as any).code = "DOWNSTREAM_ERROR";
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
