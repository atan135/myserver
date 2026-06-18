import assert from "node:assert/strict";
import test from "node:test";

import {
  buildRedirectReconnectOptions,
  shouldFallbackToJoin,
  summarizeRedirectReconnectResult,
  validateServerRedirectPush
} from "../tools/mock-client/src/server-redirect-reconnect.js";
import { parseArgs } from "../tools/mock-client/src/args.js";

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
    {
      host: "old-host",
      gameHost: "",
      port: 7000,
      roomId: "room-a",
      timeoutMs: 5000,
      policyId: "movement_demo",
      redirectReconnectDelayMs: 1200
    },
    redirect
  );

  assert.equal(options.host, "127.0.0.1");
  assert.equal(options.gameHost, "127.0.0.1");
  assert.equal(options.port, 4000);
  assert.equal(options.timeoutMs, 5000);
  assert.equal(options.policyId, "movement_demo");
  assert.equal(options.redirectReconnectDelayMs, 1200);
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

test("server redirect reconnect cli options parse policy and delayed reconnect", () => {
  const options = parseArgs([
    "--scenario", "server-redirect-transfer-reconnect",
    "--room-id", "room-a",
    "--rollout-epoch", "rollout-1",
    "--old-server-id", "old",
    "--new-server-id", "new",
    "--policy-id", "movement_demo",
    "--redirect-reconnect-delay-ms", "1500",
    "--resolved-control-targets",
    "--old-admin-host", "127.0.0.10",
    "--old-admin-port", "7500",
    "--new-admin-host", "127.0.0.11",
    "--new-admin-port", "7501",
    "--proxy-admin-url", "http://127.0.0.1:7101",
    "--proxy-admin-actor", "rollout-drill",
    "--redirect-target-host", "127.0.0.1",
    "--redirect-target-port", "14000",
    "--redirect-target-server-id", "game-proxy",
    "--redirect-transport", "tcp",
    "--redirect-reason", "rollout_redirect",
    "--redirect-retry-after-ms", "250",
    "--allow-redirect-join-fallback"
  ]);

  assert.equal(options.scenario, "server-redirect-transfer-reconnect");
  assert.equal(options.roomId, "room-a");
  assert.equal(options.rolloutEpoch, "rollout-1");
  assert.equal(options.oldServerId, "old");
  assert.equal(options.newServerId, "new");
  assert.equal(options.policyId, "movement_demo");
  assert.equal(options.redirectReconnectDelayMs, 1500);
  assert.equal(options.resolvedControlTargetsInput, true);
  assert.equal(options.oldAdminHost, "127.0.0.10");
  assert.equal(options.oldAdminPort, 7500);
  assert.equal(options.newAdminHost, "127.0.0.11");
  assert.equal(options.newAdminPort, 7501);
  assert.equal(options.proxyAdminUrl, "http://127.0.0.1:7101");
  assert.equal(options.proxyAdminActor, "rollout-drill");
  assert.equal(options.redirectTargetHost, "127.0.0.1");
  assert.equal(options.redirectTargetPort, 14000);
  assert.equal(options.redirectTargetServerId, "game-proxy");
  assert.equal(options.redirectTransport, "tcp");
  assert.equal(options.redirectReason, "rollout_redirect");
  assert.equal(options.redirectRetryAfterMs, 250);
  assert.equal(options.allowRedirectJoinFallback, true);
});
