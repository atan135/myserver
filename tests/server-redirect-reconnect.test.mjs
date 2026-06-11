import assert from "node:assert/strict";
import test from "node:test";

import {
  buildRedirectReconnectOptions,
  shouldFallbackToJoin,
  summarizeRedirectReconnectResult,
  validateServerRedirectPush
} from "../tools/mock-client/src/server-redirect-reconnect.js";

const redirect = {
  reason: "rollout_redirect",
  roomId: "room-a",
  rolloutEpoch: "epoch-1",
  reconnectRequired: true,
  retryAfterMs: 300,
  targetHost: "127.0.0.1",
  targetPort: 4000,
  targetServerId: "game-server-new",
  transport: "tcp"
};

test("server redirect reconnect options use push target", () => {
  const options = buildRedirectReconnectOptions(
    { host: "old-host", gameHost: "", port: 7000, roomId: "room-a", timeoutMs: 5000 },
    redirect
  );

  assert.equal(options.host, "127.0.0.1");
  assert.equal(options.gameHost, "127.0.0.1");
  assert.equal(options.port, 4000);
  assert.equal(options.timeoutMs, 5000);
});

test("server redirect validation rejects wrong room and missing target", () => {
  assert.throws(
    () => validateServerRedirectPush({ ...redirect, roomId: "room-b" }, "room-a"),
    /redirect room mismatch/
  );
  assert.throws(
    () => validateServerRedirectPush({ ...redirect, targetHost: "" }, "room-a"),
    /missing target host or port/
  );
});

test("server redirect join fallback requires explicit opt-in and known error", () => {
  assert.equal(
    shouldFallbackToJoin({ ok: false, errorCode: "ROOM_NOT_FOUND" }, {}),
    false
  );
  assert.equal(
    shouldFallbackToJoin({ ok: false, errorCode: "ROOM_NOT_FOUND" }, { allowRedirectJoinFallback: true }),
    true
  );
  assert.equal(
    shouldFallbackToJoin({ ok: false, errorCode: "AUTH_FAILED" }, { allowRedirectJoinFallback: true }),
    false
  );
  assert.equal(
    shouldFallbackToJoin({ ok: true, roomId: "room-a" }, { allowRedirectJoinFallback: true }),
    false
  );
});

test("server redirect reconnect summary exposes redirect and final room", () => {
  const summary = summarizeRedirectReconnectResult({
    login: { playerId: "player-a" },
    redirect,
    reconnectRes: { ok: false, errorCode: "ROOM_NOT_FOUND" },
    joinRes: { ok: true, roomId: "room-a" },
    finalMode: "join"
  });

  assert.equal(summary.ok, true);
  assert.equal(summary.playerId, "player-a");
  assert.equal(summary.finalMode, "join");
  assert.equal(summary.finalRoomId, "room-a");
  assert.deepEqual(summary.redirect, redirect);
});
