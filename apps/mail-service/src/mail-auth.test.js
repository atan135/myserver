import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";

import { verifyTicketSignature } from "./mail-auth.js";

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
