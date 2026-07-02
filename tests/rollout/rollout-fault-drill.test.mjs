import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import test from "node:test";

import {
  createSimulatedTransferClients,
  ROLLOUT_FAULT_DRILL,
  runRolloutFaultDrills
} from "../../tools/rollout/rollout-fault-drill.js";
import {
  ROOM_TRANSFER_FAILURE_INJECTION,
  ROOM_TRANSFER_STAGE,
  encodeRoomTransferPayloadForTest,
  orchestrateRoomTransfer
} from "../../tools/rollout/rollout-transfer.js";

test("rollout fault drill cli help uses registry targets before direct endpoints", () => {
  const result = spawnSync(process.execPath, ["tools/rollout/rollout-fault-drill-cli.js", "--help"], {
    cwd: process.cwd(),
    encoding: "utf8"
  });

  assert.equal(result.status, 0);
  assert.match(result.stdout, /registry discovery/);
  assert.match(result.stdout, /game-server\.admin/);
  assert.match(result.stdout, /game-proxy\.admin/);
  assert.match(result.stdout, /pre-resolved or local debug fallback/);
  assert.doesNotMatch(result.stdout, /127\.0\.0\.1:7101/);
});

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
      ROLLOUT_FAULT_DRILL.ROUTE_METADATA_MISSING,
      ROLLOUT_FAULT_DRILL.REDIRECT_NO_RECONNECT
    ]
  );

  const routeMetadataMissing = report.drills.find((drill) => drill.name === ROLLOUT_FAULT_DRILL.ROUTE_METADATA_MISSING);
  assert.equal(routeMetadataMissing.plan.expectedErrorCode, "ROOM_ROUTE_METADATA_MISSING");
  assert.equal(routeMetadataMissing.plan.endpoints.oldGameServerAdmin.source, "registry");
  assert.equal(routeMetadataMissing.plan.endpoints.oldGameServerAdmin.target, "game-server.admin");
  assert.equal(routeMetadataMissing.plan.endpoints.oldGameServerAdmin.instanceId, "game-server-old");
  assert.equal(routeMetadataMissing.plan.endpoints.newGameServerAdmin.source, "registry");
  assert.equal(routeMetadataMissing.plan.endpoints.newGameServerAdmin.target, "game-server.admin");
  assert.equal(routeMetadataMissing.plan.endpoints.newGameServerAdmin.instanceId, "game-server-new");
  assert.equal(routeMetadataMissing.plan.endpoints.gameProxyAdmin.source, "registry");
  assert.equal(routeMetadataMissing.plan.endpoints.gameProxyAdmin.target, "game-proxy.admin");
  assert.deepEqual(routeMetadataMissing.plan.plannedCalls, [
    "old.freezeRoomForTransfer",
    "old.exportRoomTransfer",
    "new.importRoomTransfer",
    "new.confirmRoomOwnership",
    "proxy.getRoomRoute"
  ]);
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

  const routeMetadataMissing = report.drills.find((drill) => drill.name === ROLLOUT_FAULT_DRILL.ROUTE_METADATA_MISSING);
  assert.equal(routeMetadataMissing.validation.ok, true);
  assert.equal(routeMetadataMissing.result.ok, false);
  assert.equal(routeMetadataMissing.result.stage, ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT);
  assert.equal(routeMetadataMissing.result.expectedFailure, true);
  assert.equal(routeMetadataMissing.result.errorCode, "ROOM_ROUTE_METADATA_MISSING");
  assert.equal(routeMetadataMissing.validation.expectedErrorCodeObserved, true);
  assert.deepEqual(routeMetadataMissing.result.routeMetadata, {
    requiredExistingRoute: true,
    found: false,
    checkedVia: "proxy.getRoomRoute",
    actionOnMissing: "fail_before_proxy_route_upsert"
  });
  assert.deepEqual(routeMetadataMissing.result.completedStages, [
    ROOM_TRANSFER_STAGE.OLD_FREEZE,
    ROOM_TRANSFER_STAGE.OLD_EXPORT,
    ROOM_TRANSFER_STAGE.NEW_IMPORT,
    ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP
  ]);
  assert.deepEqual(routeMetadataMissing.result.mockCalls, [
    "old.freezeRoomForTransfer",
    "old.exportRoomTransfer",
    "new.importRoomTransfer",
    "new.confirmRoomOwnership",
    "proxy.getRoomRoute"
  ]);
  assert.equal(routeMetadataMissing.result.mockCalls.includes("proxy.upsertRoomRoute"), false);
  assert.equal(routeMetadataMissing.result.mockCalls.includes("old.retireTransferredRoom"), false);

  const redirectFailure = report.drills.find((drill) => drill.name === ROLLOUT_FAULT_DRILL.REDIRECT_NO_RECONNECT);
  assert.equal(redirectFailure.validation.ok, true);
  assert.equal(redirectFailure.result.ok, false);
  assert.equal(redirectFailure.result.stage, "redirect_no_reconnect");
  assert.equal(redirectFailure.result.expectedFailure, true);
  assert.equal(redirectFailure.result.reconnectAttempted, false);
});

test("route metadata missing fault does not rely on proxy upsert rejection", async () => {
  const calls = [];
  const payloadRaw = encodeRoomTransferPayloadForTest({
    rolloutEpoch: "rollout-test",
    roomId: "room-test",
    roomVersion: 2,
    checksum: "checksum-test"
  });
  const clients = {
    oldServer: {
      async freezeRoomForTransfer() {
        calls.push("old.freezeRoomForTransfer");
        return { ok: true, roomId: "room-test", roomVersion: 1 };
      },
      async exportRoomTransfer() {
        calls.push("old.exportRoomTransfer");
        return {
          ok: true,
          roomId: "room-test",
          checksum: "checksum-test",
          payload: { raw: payloadRaw, roomVersion: 2, checksum: "checksum-test" }
        };
      },
      async retireTransferredRoom() {
        calls.push("old.retireTransferredRoom");
        return { ok: true, roomId: "room-test" };
      }
    },
    newServer: {
      async importRoomTransfer() {
        calls.push("new.importRoomTransfer");
        return { ok: true, roomId: "room-test", checksum: "checksum-test", roomVersion: 3 };
      },
      async confirmRoomOwnership(request) {
        calls.push("new.confirmRoomOwnership");
        return {
          ok: true,
          roomId: "room-test",
          checksum: request.checksum,
          roomVersion: request.roomVersion
        };
      }
    },
    proxy: {
      async getRoomRoute() {
        calls.push("proxy.getRoomRoute");
        return null;
      },
      async upsertRoomRoute() {
        calls.push("proxy.upsertRoomRoute");
        return { ok: true };
      }
    }
  };

  const result = await orchestrateRoomTransfer(
    {
      rolloutEpoch: "rollout-test",
      roomId: "room-test",
      oldServerId: "game-server-old",
      newServerId: "game-server-new",
      failureInjection: {
        stage: ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
        mode: ROOM_TRANSFER_FAILURE_INJECTION.PROXY_MISSING_ROUTE_METADATA
      }
    },
    clients
  );

  assert.equal(result.ok, false);
  assert.equal(result.stage, ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT);
  assert.equal(result.expectedFailure, true);
  assert.equal(result.errorCode, "ROOM_ROUTE_METADATA_MISSING");
  assert.deepEqual(result.routeMetadata, {
    requiredExistingRoute: true,
    found: false,
    checkedVia: "proxy.getRoomRoute",
    actionOnMissing: "fail_before_proxy_route_upsert"
  });
  assert.deepEqual(result.completedStages, [
    ROOM_TRANSFER_STAGE.OLD_FREEZE,
    ROOM_TRANSFER_STAGE.OLD_EXPORT,
    ROOM_TRANSFER_STAGE.NEW_IMPORT,
    ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP
  ]);
  assert.deepEqual(calls, [
    "old.freezeRoomForTransfer",
    "old.exportRoomTransfer",
    "new.importRoomTransfer",
    "new.confirmRoomOwnership",
    "proxy.getRoomRoute"
  ]);
});

test("route metadata missing fault reports precondition when metadata is still present", async () => {
  const { calls, clients } = createSimulatedTransferClients({
    rolloutEpoch: "rollout-test",
    roomId: "room-test"
  });

  const result = await orchestrateRoomTransfer(
    {
      rolloutEpoch: "rollout-test",
      roomId: "room-test",
      oldServerId: "game-server-old",
      newServerId: "game-server-new",
      requireExistingRouteMetadata: true,
      expectMissingRouteMetadataFailure: true,
      failureInjection: {
        stage: ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
        mode: ROOM_TRANSFER_FAILURE_INJECTION.PROXY_MISSING_ROUTE_METADATA
      }
    },
    clients
  );

  assert.equal(result.ok, false);
  assert.equal(result.stage, ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT);
  assert.equal(result.expectedFailure, false);
  assert.equal(result.errorCode, "FAULT_DRILL_ROUTE_METADATA_PRESENT");
  assert.equal(result.routeMetadata.requiredExistingRoute, true);
  assert.equal(result.routeMetadata.found, true);
  assert.equal(calls.includes("proxy.upsertRoomRoute"), false);
  assert.equal(calls.includes("old.retireTransferredRoom"), false);
});
