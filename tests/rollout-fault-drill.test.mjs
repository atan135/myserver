import assert from "node:assert/strict";
import test from "node:test";

import {
  ROLLOUT_FAULT_DRILL,
  runRolloutFaultDrills
} from "../tools/mock-client/src/rollout-fault-drill.js";
import { ROOM_TRANSFER_STAGE } from "../tools/mock-client/src/rollout-transfer.js";

test("rollout fault drill dry-run prints a safe plan without service calls", async () => {
  const report = await runRolloutFaultDrills({
    rolloutEpoch: "rollout-test",
    roomId: "room-test"
  });

  assert.equal(report.ok, true);
  assert.equal(report.mode, "dry-run");
  assert.equal(report.execute, false);
  assert.equal(report.safety.callsControlPlane, false);
  assert.equal(report.safety.requestsShutdown, false);
  assert.equal(report.safety.runsReconnectClient, false);
  assert.deepEqual(
    report.drills.map((drill) => drill.name),
    [
      ROLLOUT_FAULT_DRILL.IMPORT_FAILURE,
      ROLLOUT_FAULT_DRILL.ROUTE_UPSERT_FAILURE,
      ROLLOUT_FAULT_DRILL.REDIRECT_NO_RECONNECT
    ]
  );
});

test("rollout fault drill simulate validates expected stop stages", async () => {
  const report = await runRolloutFaultDrills({
    simulate: true,
    rolloutEpoch: "rollout-test",
    roomId: "room-test",
    redirectTargetHost: "127.0.0.1",
    redirectTargetPort: 4000
  });

  assert.equal(report.ok, true);
  assert.equal(report.mode, "simulate");
  assert.equal(report.safety.callsControlPlane, false);

  const importFailure = report.drills.find((drill) => drill.name === ROLLOUT_FAULT_DRILL.IMPORT_FAILURE);
  assert.equal(importFailure.validation.ok, true);
  assert.equal(importFailure.result.ok, false);
  assert.equal(importFailure.result.stage, ROOM_TRANSFER_STAGE.NEW_IMPORT);
  assert.equal(importFailure.result.expectedFailure, true);
  assert.deepEqual(importFailure.result.mockCalls, [
    "old.freezeRoomForTransfer",
    "old.exportRoomTransfer",
    "new.importRoomTransfer"
  ]);

  const routeFailure = report.drills.find((drill) => drill.name === ROLLOUT_FAULT_DRILL.ROUTE_UPSERT_FAILURE);
  assert.equal(routeFailure.validation.ok, true);
  assert.equal(routeFailure.result.ok, false);
  assert.equal(routeFailure.result.stage, ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT);
  assert.equal(routeFailure.result.expectedFailure, true);
  assert.deepEqual(routeFailure.result.mockCalls, [
    "old.freezeRoomForTransfer",
    "old.exportRoomTransfer",
    "new.importRoomTransfer",
    "new.confirmRoomOwnership",
    "proxy.getRoomRoute",
    "proxy.upsertRoomRoute"
  ]);

  const redirectFailure = report.drills.find((drill) => drill.name === ROLLOUT_FAULT_DRILL.REDIRECT_NO_RECONNECT);
  assert.equal(redirectFailure.validation.ok, true);
  assert.equal(redirectFailure.result.ok, false);
  assert.equal(redirectFailure.result.stage, "redirect_no_reconnect");
  assert.equal(redirectFailure.result.expectedFailure, true);
  assert.equal(redirectFailure.result.reconnectAttempted, false);
});
