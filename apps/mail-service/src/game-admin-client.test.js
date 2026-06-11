import assert from "node:assert/strict";
import test from "node:test";

import {
  buildAdminAuthBody,
  buildGrantMailAttachmentsPayload,
  getDefaultGameAdminActor,
  normalizeGameAdminActor,
  normalizeServiceActorCandidate
} from "./game-admin-client.js";

const config = {
  gameAdminToken: "secret-admin-token",
  serviceInstanceId: "mail-001",
  serviceName: "mail-service"
};

test("admin auth body keeps legacy plain token when actor is missing", () => {
  const body = buildAdminAuthBody(config);

  assert.equal(body.toString("utf8"), "secret-admin-token");
});

test("admin auth body uses JSON envelope when actor is valid", () => {
  const body = buildAdminAuthBody(config, " mail-service ");

  assert.deepEqual(JSON.parse(body.toString("utf8")), {
    token: "secret-admin-token",
    actor: "mail-service"
  });
});

test("admin auth body falls back to plain token for invalid actor", () => {
  const body = buildAdminAuthBody(config, "mail/service");

  assert.equal(normalizeGameAdminActor("mail/service"), null);
  assert.equal(body.toString("utf8"), "secret-admin-token");
});

test("default service actor uses normalized service identity", () => {
  assert.equal(getDefaultGameAdminActor(config), "mail-001");
  assert.equal(
    getDefaultGameAdminActor({ ...config, serviceInstanceId: "mail/service 01" }),
    "mail-service-01"
  );
  assert.equal(
    getDefaultGameAdminActor({ ...config, serviceInstanceId: "mail/service", serviceName: "mail service" }),
    "mail-service"
  );
  assert.equal(normalizeServiceActorCandidate("mail/service 01"), "mail-service-01");
});

test("grant mail attachments payload keeps stable idempotency fields", () => {
  const body = buildGrantMailAttachmentsPayload(
    "player-1",
    "mail_claim:mail-1",
    [{ itemId: 1001, count: 2, binded: true }],
    "claim mail mail-1"
  );

  assert.deepEqual(JSON.parse(body.toString("utf8")), {
    requestId: "mail_claim:mail-1",
    playerId: "player-1",
    items: [{ itemId: 1001, count: 2, binded: true }],
    source: "mail-claim",
    reason: "claim mail mail-1"
  });
});
