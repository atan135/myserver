import assert from "node:assert/strict";
import { EventEmitter } from "node:events";
import test from "node:test";

import { attachPoolErrorHandler } from "./db-client.js";
import { formatLogPayload } from "./logger.js";

test("database pool client errors are handled with a bounded stable code", () => {
  const pool = new EventEmitter();
  const client = new EventEmitter();
  const observed = [];
  attachPoolErrorHandler(pool, (errorCode) => observed.push(errorCode));

  pool.emit("error", Object.assign(new Error("postgres://secret@127.0.0.1/db"), { code: "ECONNRESET" }));
  pool.emit("error", new Error("free-form database failure"));
  pool.emit("connect", client);
  client.emit("error", Object.assign(new Error("checked-out client failed"), { code: "ECONNREFUSED" }));

  assert.deepEqual(observed, ["ECONNRESET", "DATABASE_POOL_ERROR", "ECONNREFUSED"]);
});

test("database pool client error handling never throws when reporting is unavailable", () => {
  const pool = new EventEmitter();
  attachPoolErrorHandler(pool, () => {
    throw new Error("logger is not configured");
  });

  assert.doesNotThrow(() => {
    pool.emit("error", Object.assign(new Error("database disconnected"), { code: "ECONNRESET" }));
  });
});

test("log formatting bounds nested data and removes credentials, mail bodies, attachments, and endpoints", () => {
  const secrets = {
    ticket: "test-only-ticket-value.DO_NOT_LOG",
    adminToken: "test-only-game-admin-token-DO_NOT_LOG",
    mailGrantPrivateKey: "ed25519-private-material-DO_NOT_LOG",
    content: "complete private mail body DO_NOT_LOG",
    endpoint: "redis://fixture-user:fixture-password@10.0.0.8:6379/0"
  };
  const attachments = Array.from({ length: 200 }, (_, index) => ({
    itemId: 1000 + index,
    count: index + 1,
    binded: index % 2 === 0,
    note: `attachment-private-${index}-DO_NOT_LOG`
  }));
  const error = new Error(JSON.stringify({
    ticket: secrets.ticket,
    GAME_ADMIN_TOKEN: secrets.adminToken,
    MAIL_GRANT_ASSERTION_PRIVATE_KEY: secrets.mailGrantPrivateKey,
    content: secrets.content,
    attachments,
    redisEndpoint: secrets.endpoint
  }));

  const payload = formatLogPayload("mail.claim_grant_failed", {
    requestId: "mail_claim:mail-fixture",
    traceId: "1".repeat(32),
    error: error.message,
    ticket: secrets.ticket,
    gameAdminToken: secrets.adminToken,
    mailGrantAssertionPrivateKey: secrets.mailGrantPrivateKey,
    content: secrets.content,
    attachments,
    redisEndpoint: secrets.endpoint
  });

  assert.match(payload, /mail\.claim_grant_failed/);
  assert.match(payload, /mail_claim:mail-fixture/);
  assert.match(payload, /REDACTED/);
  assert.ok(Buffer.byteLength(payload, "utf8") < 2048);
  for (const forbidden of [
    secrets.ticket,
    secrets.adminToken,
    secrets.mailGrantPrivateKey,
    secrets.content,
    secrets.endpoint,
    "attachment-private-199-DO_NOT_LOG",
    "10.0.0.8:6379"
  ]) {
    assert.doesNotMatch(payload, new RegExp(forbidden.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
  }
});

test("log formatting preserves bounded operational fields while hiding standalone endpoints", () => {
  const payload = formatLogPayload("mail.claim_recovery_scan_completed", {
    trigger: "periodic",
    recovered: 2,
    requestId: "mail_claim:mail-fixture",
    endpoint: "nats://127.0.0.1:4222"
  });

  assert.match(payload, /\"trigger\":\"periodic\"/);
  assert.match(payload, /\"recovered\":2/);
  assert.match(payload, /nats:\/\/\[REDACTED_ENDPOINT\]/);
  assert.doesNotMatch(payload, /127\.0\.0\.1:4222/);
});

test("free-form error details never expose opaque values while stable error identity remains", () => {
  const opaqueA = "A7x9Qp4Lm2Vz8Kc6Hn5Rw3Yd";
  const opaqueB = "Q6mZ1vR8pL4xN7cK2wF9hJ5s";
  const failure = Object.assign(new Error(`remote rejected ${opaqueB}`), {
    code: "GAME_ADMIN_READ_TIMEOUT"
  });
  const payload = formatLogPayload("mail.claim_grant_failed", {
    requestId: "mail_claim:mail-fixture",
    traceId: "2".repeat(32),
    errorCode: "GAME_ADMIN_TOKEN_REJECTED",
    error: `upstream rejected ${opaqueA}`,
    failure
  });

  assert.match(payload, /mail\.claim_grant_failed/);
  assert.match(payload, /mail_claim:mail-fixture/);
  assert.match(payload, /"traceId":"2{32}"/);
  assert.match(payload, /"errorCode":"GAME_ADMIN_TOKEN_REJECTED"/);
  assert.match(payload, /"code":"GAME_ADMIN_READ_TIMEOUT"/);
  assert.doesNotMatch(payload, new RegExp(opaqueA));
  assert.doesNotMatch(payload, new RegExp(opaqueB));
  assert.match(payload, /REDACTED_ERROR_DETAIL/);
});
