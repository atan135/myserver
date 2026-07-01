import assert from "node:assert/strict";
import { register } from "node:module";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT = fileURLToPath(new URL("../../apps/auth-http/tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY = "true";
register("ts-node/esm", pathToFileURL("./"));

import { decodeRolloutDrainStatusRes } from "../../apps/auth-http/src/game-admin-client.js";

const { InternalController } = await import("../../apps/auth-http/src/internal/internal.controller.ts");

function encodeVarint(value) {
  let current = BigInt(value);
  const bytes = [];
  while (current >= 0x80n) {
    bytes.push(Number((current & 0x7fn) | 0x80n));
    current >>= 7n;
  }
  bytes.push(Number(current));
  return Buffer.from(bytes);
}

function encodeBoolField(fieldNumber, value) {
  return Buffer.concat([encodeVarint(fieldNumber << 3), encodeVarint(value ? 1 : 0)]);
}

function encodeUInt64Field(fieldNumber, value) {
  return Buffer.concat([encodeVarint(fieldNumber << 3), encodeVarint(value)]);
}

function encodeStringField(fieldNumber, value) {
  const body = Buffer.from(value, "utf8");
  return Buffer.concat([encodeVarint((fieldNumber << 3) | 2), encodeVarint(body.length), body]);
}

function encodeMessageField(fieldNumber, body) {
  return Buffer.concat([encodeVarint((fieldNumber << 3) | 2), encodeVarint(body.length), body]);
}

function encodeRoute(fields) {
  return Buffer.concat([
    encodeStringField(1, fields.roomId),
    encodeStringField(2, fields.ownerServerId),
    encodeUInt64Field(3, fields.migrationState),
    encodeUInt64Field(4, fields.memberCount),
    encodeUInt64Field(5, fields.onlineMemberCount),
    encodeUInt64Field(6, fields.emptySinceMs),
    encodeUInt64Field(7, fields.roomVersion)
  ]);
}

test("decodeRolloutDrainStatusRes decodes counters and route samples", () => {
  const body = Buffer.concat([
    encodeBoolField(1, true),
    encodeStringField(2, ""),
    encodeStringField(3, "epoch-1"),
    encodeStringField(4, "old-1"),
    encodeUInt64Field(5, 2),
    encodeUInt64Field(6, 1),
    encodeUInt64Field(7, 3),
    encodeMessageField(8, encodeRoute({
      roomId: "room-a",
      ownerServerId: "old-1",
      migrationState: 2,
      memberCount: 4,
      onlineMemberCount: 0,
      emptySinceMs: 1234,
      roomVersion: 9
    })),
    encodeMessageField(8, encodeRoute({
      roomId: "room-b",
      ownerServerId: "old-1",
      migrationState: 4,
      memberCount: 0,
      onlineMemberCount: 0,
      emptySinceMs: 0,
      roomVersion: 10
    }))
  ]);

  assert.deepEqual(decodeRolloutDrainStatusRes(body), {
    ok: true,
    errorCode: "",
    rolloutEpoch: "epoch-1",
    ownerServerId: "old-1",
    ownedRoomCount: 2,
    migratingRoomCount: 1,
    connectionCount: 3,
    routes: [
      {
        roomId: "room-a",
        ownerServerId: "old-1",
        migrationState: "FrozenForTransfer",
        memberCount: 4,
        onlineMemberCount: 0,
        emptySinceMs: 1234,
        roomVersion: 9
      },
      {
        roomId: "room-b",
        ownerServerId: "old-1",
        migrationState: "OwnedByNew",
        memberCount: 0,
        onlineMemberCount: 0,
        emptySinceMs: 0,
        roomVersion: 10
      }
    ],
    drainModeEnabled: false,
    drainModeEnteredAtMs: 0,
    transferableEmptyRoomCount: 0,
    transferableEmptyRoomSamples: [],
    drainModeReason: "",
    drainModeSource: "",
    retiredRoomCount: 0
  });
});

test("decodeRolloutDrainStatusRes rejects truncated length-delimited fields", () => {
  const truncated = Buffer.from([
    (2 << 3) | 2,
    5,
    0x6f,
    0x6f
  ]);

  assert.throws(
    () => decodeRolloutDrainStatusRes(truncated),
    /UNEXPECTED_END_OF_LENGTH_DELIMITED_FIELD/
  );
});

test("InternalController rolloutDrainStatus returns protected game-server status", async () => {
  const expected = {
    ok: true,
    errorCode: "",
    rolloutEpoch: "epoch-1",
    ownerServerId: "old-1",
    ownedRoomCount: 0,
    migratingRoomCount: 0,
    connectionCount: 0,
    routes: []
  };
  const controller = new InternalController(
    { internalApiToken: "token", strictSecurity: true },
    { getRolloutDrainStatus: async () => expected }
  );

  const result = await controller.rolloutDrainStatus({ headers: { "x-service-token": "token" } });
  assert.deepEqual(result, expected);

  await assert.rejects(
    () => controller.rolloutDrainStatus({ headers: { "x-service-token": "bad" } }),
    (error) => {
      assert.equal(error.getStatus(), 401);
      assert.equal(error.getResponse().error, "INVALID_SERVICE_TOKEN");
      return true;
    }
  );
});
