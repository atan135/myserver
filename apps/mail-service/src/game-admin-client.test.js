import assert from "node:assert/strict";
import net from "node:net";
import test from "node:test";

import {
  MESSAGE_TYPE,
  buildAdminAuthBody,
  buildGrantMailAttachmentsPayload,
  getDefaultGameAdminActor,
  normalizeGameAdminActor,
  normalizeServiceActorCandidate,
  sendRequest
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

test("mail admin client rejects response larger than configured limit", async () => {
  const server = net.createServer((socket) => {
    socket.once("data", () => {
      const header = Buffer.alloc(14);
      header.writeUInt16BE(0xcafe, 0);
      header.writeUInt8(1, 2);
      header.writeUInt8(0, 3);
      header.writeUInt16BE(MESSAGE_TYPE.GM_SEND_ITEM_RES, 4);
      header.writeUInt32BE(1, 6);
      header.writeUInt32BE(64, 10);
      socket.write(header);
    });
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;

  try {
    await assert.rejects(
      sendRequest(
        {
          gameServerAdminHost: "127.0.0.1",
          gameServerAdminPort: port,
          gameAdminToken: "secret-admin-token",
          gameAdminConnectTimeoutMs: 1000,
          gameAdminWriteTimeoutMs: 1000,
          gameAdminReadTimeoutMs: 1000,
          gameAdminMaxResponseBytes: 32
        },
        MESSAGE_TYPE.GM_SEND_ITEM_REQ,
        Buffer.from("{}"),
        MESSAGE_TYPE.GM_SEND_ITEM_RES
      ),
      { code: "GAME_ADMIN_RESPONSE_TOO_LARGE" }
    );
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});
