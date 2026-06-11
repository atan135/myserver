import assert from "node:assert/strict";
import test from "node:test";

import { decodeRolloutDrainStatusRes } from "./game-admin-client.js";

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

test("decodeRolloutDrainStatusRes exposes drain mode fields", () => {
  const body = Buffer.concat([
    fieldVarint(1, 1),
    fieldString(3, "epoch-7"),
    fieldString(4, "game-server-old"),
    fieldVarint(5, 2),
    fieldVarint(6, 1),
    fieldVarint(7, 9),
    fieldVarint(9, 1),
    fieldVarint(10, 1_717_000_000_123n)
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
    drainModeEnteredAtMs: 1_717_000_000_123
  });
});
