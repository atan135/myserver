import assert from "node:assert/strict";
import test from "node:test";

import { decodeRequestServerShutdownRes, decodeRolloutDrainStatusRes } from "./game-admin-client.js";

function varint(value) {
  let remaining = BigInt(value);
  const bytes = [];
  while (remaining >= 0x80n) {
    bytes.push(Number((remaining & 0x7fn) | 0x80n));
    remaining >>= 7n;
  }
  bytes.push(Number(remaining));
  return Buffer.from(bytes);
}

function fieldVarint(fieldNumber, value) {
  return Buffer.concat([varint((fieldNumber << 3) | 0), varint(value)]);
}

function fieldString(fieldNumber, value) {
  const body = Buffer.from(value, "utf8");
  return Buffer.concat([varint((fieldNumber << 3) | 2), varint(body.length), body]);
}

function fieldMessage(fieldNumber, body) {
  return Buffer.concat([varint((fieldNumber << 3) | 2), varint(body.length), body]);
}

test("decodeRolloutDrainStatusRes exposes drain mode and transferable fields", () => {
  const transferableSample = Buffer.concat([
    fieldString(1, "room-empty"),
    fieldString(2, "game-server-old"),
    fieldVarint(3, 0),
    fieldVarint(4, 1),
    fieldVarint(5, 0),
    fieldVarint(6, 2500),
    fieldVarint(7, 3)
  ]);
  const body = Buffer.concat([
    fieldVarint(1, 1),
    fieldString(3, "epoch-7"),
    fieldString(4, "game-server-old"),
    fieldVarint(5, 2),
    fieldVarint(6, 1),
    fieldVarint(7, 9),
    fieldVarint(9, 1),
    fieldVarint(10, 1_717_000_000_123n),
    fieldVarint(11, 1),
    fieldMessage(12, transferableSample),
    fieldString(13, "rollout"),
    fieldString(14, "admin"),
    fieldVarint(15, 4)
  ]);

  assert.deepEqual(decodeRolloutDrainStatusRes(body), {
    ok: true,
    errorCode: "",
    rolloutEpoch: "epoch-7",
    ownerServerId: "game-server-old",
    ownedRoomCount: 2,
    migratingRoomCount: 1,
    connectionCount: 9,
    routes: [],
    drainModeEnabled: true,
    drainModeEnteredAtMs: 1_717_000_000_123,
    transferableEmptyRoomCount: 1,
    drainModeReason: "rollout",
    drainModeSource: "admin",
    retiredRoomCount: 4,
    transferableEmptyRoomSamples: [
      {
        roomId: "room-empty",
        ownerServerId: "game-server-old",
        migrationState: "OwnedByOld",
        memberCount: 1,
        onlineMemberCount: 0,
        emptySinceMs: 2500,
        roomVersion: 3
      }
    ]
  });
});

test("decodeRequestServerShutdownRes exposes shutdown safety gate fields", () => {
  const body = Buffer.concat([
    fieldVarint(1, 0),
    fieldString(2, "SHUTDOWN_CONNECTIONS_REMAIN"),
    fieldVarint(3, 2),
    fieldVarint(4, 0),
    fieldVarint(5, 0),
    fieldVarint(6, 1),
    fieldVarint(7, 3)
  ]);

  assert.deepEqual(decodeRequestServerShutdownRes(body), {
    ok: false,
    errorCode: "SHUTDOWN_CONNECTIONS_REMAIN",
    connectionCount: 2,
    ownedRoomCount: 0,
    migratingRoomCount: 0,
    drainModeEnabled: true,
    retiredRoomCount: 3
  });
});
