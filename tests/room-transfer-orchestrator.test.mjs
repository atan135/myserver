import assert from "node:assert/strict";
import test from "node:test";

import {
  ROOM_TRANSFER_STAGE,
  encodeRoomTransferPayloadForTest,
  orchestrateRoomTransfer
} from "../tools/mock-client/src/rollout-transfer.js";

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
    async importRoomTransfer() {
      calls.push("new.import");
      return overrides.import ?? {
        ok: true,
        roomId: "room-1",
        checksum: "checksum-1",
        roomVersion: 3
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
    "proxy.getRoomRoute",
    "proxy.upsert:2:1:checksum-1",
    "old.retire"
  ]);
  assert.deepEqual(result.completedStages, [
    ROOM_TRANSFER_STAGE.OLD_FREEZE,
    ROOM_TRANSFER_STAGE.OLD_EXPORT,
    ROOM_TRANSFER_STAGE.NEW_IMPORT,
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
    "proxy.getRoomRoute",
    "proxy.upsert:1:0:checksum-1",
    "old.retire"
  ]);
  assert.equal(result.proxyRoute.importedRoomVersion, 3);
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
    "proxy.getRoomRoute",
    "proxy.upsert:2:1:checksum-1",
    "old.retire"
  ]);
});
