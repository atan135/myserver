import assert from "node:assert/strict";
import http from "node:http";
import test from "node:test";

import {
  ProxyAdminClient,
  ROOM_TRANSFER_FAILURE_INJECTION,
  ROOM_TRANSFER_STAGE,
  encodeRoomTransferPayloadForTest,
  orchestrateRoomTransfer
} from "../../tools/mock-client/src/rollout-transfer.js";

function createClients(overrides = {}) {
  const calls = [];
  const payloadRaw = encodeRoomTransferPayloadForTest({
    rolloutEpoch: "rollout-1",
    roomId: "room-1",
    roomVersion: 2,
    checksum: "checksum-1"
  });

  const oldServer = {
    async freezeRoomForTransfer() {
      calls.push("old.freeze");
      return overrides.freeze ?? { ok: true, roomId: "room-1", roomVersion: 1 };
    },
    async exportRoomTransfer() {
      calls.push("old.export");
      return overrides.export ?? {
        ok: true,
        roomId: "room-1",
        checksum: "checksum-1",
        payload: { raw: payloadRaw, roomVersion: 2, checksum: "checksum-1" }
      };
    },
    async retireTransferredRoom() {
      calls.push("old.retire");
      if (overrides.retireError) throw overrides.retireError;
      return overrides.retire ?? { ok: true, roomId: "room-1" };
    }
  };

  const newServer = {
    async importRoomTransfer(request) {
      calls.push("new.import");
      if (overrides.importHandler) {
        return overrides.importHandler(request, payloadRaw);
      }
      return overrides.import ?? {
        ok: true,
        roomId: "room-1",
        checksum: "checksum-1",
        roomVersion: 3
      };
    },
    async confirmRoomOwnership(request) {
      calls.push(`new.confirm:${request.checksum}:${request.roomVersion}`);
      if (overrides.confirmError) throw overrides.confirmError;
      return overrides.confirm ?? {
        ok: true,
        roomId: "room-1",
        checksum: request.checksum,
        roomVersion: request.roomVersion
      };
    }
  };

  const proxy = {
    async getRoomRoute() {
      calls.push("proxy.getRoomRoute");
      if (Object.hasOwn(overrides, "existingRoute")) {
        return overrides.existingRoute;
      }
      return {
        room_id: "room-1",
        room_version: 1,
        last_transfer_checksum: ""
      };
    },
    async upsertRoomRoute(route) {
      calls.push(`proxy.upsert:${route.roomVersion}:${route.expectedRoomVersion}:${route.lastTransferChecksum}`);
      if (overrides.proxyError) throw overrides.proxyError;
      return { ok: true };
    }
  };

  return { calls, clients: { oldServer, newServer, proxy } };
}

const request = {
  rolloutEpoch: "rollout-1",
  roomId: "room-1",
  oldServerId: "old",
  newServerId: "new"
};

test("room transfer orchestrator runs conservative success order", async () => {
  const { calls, clients } = createClients();

  const result = await orchestrateRoomTransfer(request, clients);

  assert.equal(result.ok, true);
  assert.deepEqual(calls, [
    "old.freeze",
    "old.export",
    "new.import",
    "new.confirm:checksum-1:3",
    "proxy.getRoomRoute",
    "proxy.upsert:2:1:checksum-1",
    "old.retire"
  ]);
  assert.deepEqual(result.completedStages, [
    ROOM_TRANSFER_STAGE.OLD_FREEZE,
    ROOM_TRANSFER_STAGE.OLD_EXPORT,
    ROOM_TRANSFER_STAGE.NEW_IMPORT,
    ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP,
    ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
    ROOM_TRANSFER_STAGE.OLD_RETIRE
  ]);
});

test("room transfer creates first proxy route with version one when route is absent", async () => {
  const { calls, clients } = createClients({ existingRoute: null });

  const result = await orchestrateRoomTransfer(request, clients);

  assert.equal(result.ok, true);
  assert.deepEqual(calls, [
    "old.freeze",
    "old.export",
    "new.import",
    "new.confirm:checksum-1:3",
    "proxy.getRoomRoute",
    "proxy.upsert:1:0:checksum-1",
    "old.retire"
  ]);
  assert.equal(result.proxyRoute.importedRoomVersion, 3);
});

test("room transfer requires existing route metadata when requested", async () => {
  const { calls, clients } = createClients({ existingRoute: null });

  const result = await orchestrateRoomTransfer(
    {
      ...request,
      requireExistingRouteMetadata: true
    },
    clients
  );

  assert.equal(result.ok, false);
  assert.equal(result.stage, ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT);
  assert.equal(result.errorCode, "ROOM_ROUTE_METADATA_MISSING");
  assert.deepEqual(result.completedStages, [
    ROOM_TRANSFER_STAGE.OLD_FREEZE,
    ROOM_TRANSFER_STAGE.OLD_EXPORT,
    ROOM_TRANSFER_STAGE.NEW_IMPORT,
    ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP
  ]);
  assert.deepEqual(calls, [
    "old.freeze",
    "old.export",
    "new.import",
    "new.confirm:checksum-1:3",
    "proxy.getRoomRoute"
  ]);
  assert.deepEqual(result.routeMetadata, {
    requiredExistingRoute: true,
    found: false,
    checkedVia: "proxy.getRoomRoute",
    actionOnMissing: "fail_before_proxy_route_upsert"
  });
});

test("room transfer stops when import checksum mismatches export checksum", async () => {
  const { calls, clients } = createClients({
    import: { ok: true, roomId: "room-1", checksum: "checksum-mismatch", roomVersion: 3 }
  });

  const result = await orchestrateRoomTransfer(request, clients);

  assert.equal(result.ok, false);
  assert.equal(result.stage, ROOM_TRANSFER_STAGE.NEW_IMPORT);
  assert.equal(result.errorCode, "ROOM_TRANSFER_IMPORT_CHECKSUM_MISMATCH");
  assert.deepEqual(calls, ["old.freeze", "old.export", "new.import"]);
});

test("room transfer stops before proxy route when ownership confirm fails", async () => {
  const { calls, clients } = createClients({
    confirm: {
      ok: false,
      roomId: "room-1",
      errorCode: "ROOM_TRANSFER_VERSION_MISMATCH",
      checksum: "",
      roomVersion: 0
    }
  });

  const result = await orchestrateRoomTransfer(request, clients);

  assert.equal(result.ok, false);
  assert.equal(result.stage, ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP);
  assert.equal(result.errorCode, "ROOM_TRANSFER_VERSION_MISMATCH");
  assert.deepEqual(calls, [
    "old.freeze",
    "old.export",
    "new.import",
    "new.confirm:checksum-1:3"
  ]);
});

test("room transfer does not retire old room when proxy upsert fails", async () => {
  const { calls, clients } = createClients({
    proxyError: Object.assign(new Error("ROOM_ROUTE_VERSION_MISMATCH"), {
      code: "ROOM_ROUTE_VERSION_MISMATCH"
    })
  });

  const result = await orchestrateRoomTransfer(request, clients);

  assert.equal(result.ok, false);
  assert.equal(result.stage, ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT);
  assert.equal(result.errorCode, "ROOM_ROUTE_VERSION_MISMATCH");
  assert.deepEqual(calls, [
    "old.freeze",
    "old.export",
    "new.import",
    "new.confirm:checksum-1:3",
    "proxy.getRoomRoute",
    "proxy.upsert:2:1:checksum-1"
  ]);
});

test("room transfer reports old retire failures at retire stage", async () => {
  const { calls, clients } = createClients({
    retireError: Object.assign(new Error("ROOM_TRANSFER_CHECKSUM_MISMATCH"), {
      code: "ROOM_TRANSFER_CHECKSUM_MISMATCH"
    })
  });

  const result = await orchestrateRoomTransfer(request, clients);

  assert.equal(result.ok, false);
  assert.equal(result.stage, ROOM_TRANSFER_STAGE.OLD_RETIRE);
  assert.equal(result.errorCode, "ROOM_TRANSFER_CHECKSUM_MISMATCH");
  assert.deepEqual(calls, [
    "old.freeze",
    "old.export",
    "new.import",
    "new.confirm:checksum-1:3",
    "proxy.getRoomRoute",
    "proxy.upsert:2:1:checksum-1",
    "old.retire"
  ]);
});

test("room transfer fault injection stops at import failure before confirm or route", async () => {
  const { calls, clients } = createClients({
    importHandler(request, payloadRaw) {
      if (!Buffer.from(request.payloadRaw).equals(payloadRaw)) {
        throw Object.assign(new Error("ROOM_TRANSFER_CHECKSUM_MISMATCH"), {
          code: "ROOM_TRANSFER_CHECKSUM_MISMATCH"
        });
      }
      return {
        ok: true,
        roomId: "room-1",
        checksum: "checksum-1",
        roomVersion: 3
      };
    }
  });

  const result = await orchestrateRoomTransfer(
    {
      ...request,
      failureInjection: {
        stage: ROOM_TRANSFER_STAGE.NEW_IMPORT,
        mode: ROOM_TRANSFER_FAILURE_INJECTION.IMPORT_CORRUPT_PAYLOAD
      }
    },
    clients
  );

  assert.equal(result.ok, false);
  assert.equal(result.stage, ROOM_TRANSFER_STAGE.NEW_IMPORT);
  assert.equal(result.expectedFailure, true);
  assert.equal(result.errorCode, "ROOM_TRANSFER_CHECKSUM_MISMATCH");
  assert.deepEqual(calls, ["old.freeze", "old.export", "new.import"]);
});

test("room transfer fault injection stops at proxy upsert before old retire", async () => {
  const { calls, clients } = createClients({
    proxyError: Object.assign(new Error("ROOM_ROUTE_VERSION_MISMATCH"), {
      code: "ROOM_ROUTE_VERSION_MISMATCH"
    })
  });

  const result = await orchestrateRoomTransfer(
    {
      ...request,
      failureInjection: {
        stage: ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
        mode: ROOM_TRANSFER_FAILURE_INJECTION.PROXY_BAD_EXPECTED_ROOM_VERSION
      }
    },
    clients
  );

  assert.equal(result.ok, false);
  assert.equal(result.stage, ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT);
  assert.equal(result.expectedFailure, true);
  assert.equal(result.errorCode, "ROOM_ROUTE_VERSION_MISMATCH");
  assert.deepEqual(calls, [
    "old.freeze",
    "old.export",
    "new.import",
    "new.confirm:checksum-1:3",
    "proxy.getRoomRoute",
    "proxy.upsert:2:1000004:checksum-1"
  ]);
});

test("proxy admin client sends actor header for auditable writes", async () => {
  const requests = [];
  const server = http.createServer((req, res) => {
    requests.push({
      method: req.method,
      url: req.url,
      authorization: req.headers.authorization,
      actor: req.headers["x-admin-actor"]
    });
    res.writeHead(200, { "content-type": "text/plain" });
    res.end(req.url === "/room-routes" ? '{"routes":[]}' : "ok");
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();

  try {
    const proxy = new ProxyAdminClient({
      baseUrl: `http://127.0.0.1:${port}`,
      token: "proxy-token",
      actor: "ops@example.com",
      timeoutMs: 500
    });

    await proxy.getRoomRoute("room-1");
    await proxy.upsertRoomRoute({
      roomId: "room-1",
      ownerServerId: "game-server-new",
      migrationState: "OwnedByNew",
      memberCount: 0,
      onlineMemberCount: 0,
      roomVersion: 1,
      rolloutEpoch: "rollout-1",
      lastTransferChecksum: "checksum-1",
      expectedRoomVersion: 0,
      expectedLastTransferChecksum: "",
      importedRoomVersion: 3
    });
    await proxy.upsertCharacterRoute({
      characterId: "chr-1",
      currentRoomId: "room-1",
      preferredServerId: "game-server-new",
      rolloutEpoch: "rollout-1"
    });

    assert.equal(requests.length, 3);
    assert.equal(requests[0].url, "/room-routes");
    assert.equal(requests[0].authorization, "Bearer proxy-token");
    assert.equal(requests[0].actor, "ops@example.com");
    assert(requests[1].url.startsWith("/room-route/upsert?"));
    assert.equal(requests[1].actor, "ops@example.com");
    assert(requests[2].url.startsWith("/character-route/upsert?"));
    assert(requests[2].url.includes("character_id=chr-1"));
    assert(requests[2].url.includes("current_room_id=room-1"));
    assert(requests[2].url.includes("preferred_server_id=game-server-new"));
    assert.equal(requests[2].actor, "ops@example.com");
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});

test("proxy admin client missing route plus required metadata stops before upsert", async () => {
  const requests = [];
  const server = http.createServer((req, res) => {
    requests.push({ method: req.method, url: req.url });
    res.writeHead(200, { "content-type": "application/json" });
    res.end(JSON.stringify({ routes: [] }));
  });

  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  const { clients } = createClients({
    existingRoute: {
      room_id: "room-1",
      room_version: 1,
      last_transfer_checksum: ""
    }
  });
  clients.proxy = new ProxyAdminClient({
    baseUrl: `http://127.0.0.1:${port}`,
    token: "proxy-token",
    actor: "ops@example.com",
    timeoutMs: 500
  });

  try {
    const result = await orchestrateRoomTransfer(
      {
        ...request,
        requireExistingRouteMetadata: true
      },
      clients
    );

    assert.equal(result.ok, false);
    assert.equal(result.stage, ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT);
    assert.equal(result.errorCode, "ROOM_ROUTE_METADATA_MISSING");
    assert.equal(result.routeMetadata.found, false);
    assert.deepEqual(requests, [{ method: "GET", url: "/room-routes" }]);
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
});
