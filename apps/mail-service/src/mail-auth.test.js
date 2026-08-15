import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";

import {
  MailPlayerAuthService,
  ticketKey,
  ticketVersionKey,
  validateMailLoadTestToken,
  verifyTicketSignature
} from "./mail-auth.js";

const ticketSecret = "mail-ticket-secret-for-character-tests";

function createTicket(payload) {
  const payloadB64 = Buffer.from(JSON.stringify(payload)).toString("base64url");
  const signatureB64 = crypto
    .createHmac("sha256", ticketSecret)
    .update(payloadB64)
    .digest("base64url");
  return `${payloadB64}.${signatureB64}`;
}

test("verifyTicketSignature returns account playerId and characterId", () => {
  const ticket = createTicket({
    playerId: "player-1",
    characterId: "chr_1",
    ver: 3,
    exp: "2099-01-01T00:00:00.000Z"
  });

  const payload = verifyTicketSignature(ticketSecret, ticket, Date.parse("2026-01-01T00:00:00.000Z"));

  assert.equal(payload.playerId, "player-1");
  assert.equal(payload.characterId, "chr_1");
  assert.equal(payload.ver, 3);
});

test("verifyTicketSignature rejects tickets missing characterId", () => {
  const ticket = createTicket({
    playerId: "player-1",
    ver: 3,
    exp: "2099-01-01T00:00:00.000Z"
  });

  assert.throws(
    () => verifyTicketSignature(ticketSecret, ticket, Date.parse("2026-01-01T00:00:00.000Z")),
    { code: "INVALID_TICKET_PAYLOAD" }
  );
});

test("MailPlayerAuthService rejects a signed ticket owned by another player", async () => {
  const ticket = createTicket({
    playerId: "player-1",
    characterId: "chr_1",
    ver: 3,
    exp: "2099-01-01T00:00:00.000Z"
  });
  const redis = {
    async get(key) {
      if (key === ticketKey("", ticket)) return "player-attacker";
      if (key === ticketVersionKey("", "player-1")) return "3";
      return null;
    }
  };
  const auth = new MailPlayerAuthService({ ticketSecret, redisKeyPrefix: "" }, redis);

  await assert.rejects(() => auth.authenticateTicket(ticket), { code: "TICKET_REVOKED" });
});

test("MailPlayerAuthService requires an unmodified ticket with the active version", async () => {
  const ticket = createTicket({
    playerId: "player-1",
    characterId: "chr_1",
    ver: 3,
    exp: "2099-01-01T00:00:00.000Z"
  });
  const redis = {
    async get(key) {
      if (key === ticketKey("", ticket)) return "player-1";
      if (key === ticketVersionKey("", "player-1")) return "3";
      return null;
    }
  };
  const auth = new MailPlayerAuthService({ ticketSecret, redisKeyPrefix: "" }, redis);

  assert.deepEqual(await auth.authenticateTicket(ticket), {
    playerId: "player-1",
    characterId: "chr_1",
    ticketVersion: 3
  });
  await assert.rejects(
    () => auth.authenticateTicket(`${ticket}tampered`),
    { code: "INVALID_TICKET_SIGNATURE" }
  );
});

test("load-test mail notification token is isolated from general service auth", () => {
  const config = { mailLoadTestNotificationToken: "separate-load-test-token" };
  assert.doesNotThrow(() => validateMailLoadTestToken(
    { "x-mail-load-test-token": "separate-load-test-token" },
    config
  ));
  assert.throws(
    () => validateMailLoadTestToken({ "x-service-token": "separate-load-test-token" }, config),
    { code: "MAIL_LOAD_TEST_NOTIFICATION_TOKEN_REQUIRED" }
  );
  assert.throws(
    () => validateMailLoadTestToken({ "x-mail-load-test-token": "wrong" }, config),
    { code: "MAIL_LOAD_TEST_NOTIFICATION_TOKEN_INVALID" }
  );
});
