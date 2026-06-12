import assert from "node:assert/strict";
import test from "node:test";

import {
  buildTransferCliDryRunPlan,
  parseArgs,
  validateTransferCliOptions
} from "../tools/mock-client/src/rollout-transfer-cli.js";
import { ROOM_TRANSFER_STAGE } from "../tools/mock-client/src/rollout-transfer.js";

test("rollout transfer cli dry-run builds a safe three-process plan", () => {
  const options = parseArgs([
    "--dry-run",
    "--rollout-epoch", "rollout-test",
    "--room-id", "room-test",
    "--old-server-id", "game-server-old",
    "--new-server-id", "game-server-new",
    "--old-admin-port", "7500",
    "--new-admin-port", "7501",
    "--proxy-admin-url", "http://127.0.0.1:7101",
    "--timeout-ms", "6000"
  ]);

  const plan = buildTransferCliDryRunPlan(options);

  assert.equal(plan.ok, true);
  assert.equal(plan.mode, "transfer-dry-run");
  assert.equal(plan.dryRun, true);
  assert.equal(plan.safety.startsServices, false);
  assert.equal(plan.safety.callsControlPlane, false);
  assert.equal(plan.safety.requestsShutdown, false);
  assert.deepEqual(plan.plan.plannedStages, [
    ROOM_TRANSFER_STAGE.OLD_FREEZE,
    ROOM_TRANSFER_STAGE.OLD_EXPORT,
    ROOM_TRANSFER_STAGE.NEW_IMPORT,
    ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP,
    ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
    ROOM_TRANSFER_STAGE.OLD_RETIRE
  ]);
  assert.equal(plan.plan.endpoints.oldGameServerAdmin.endpoint, "127.0.0.1:7500");
  assert.equal(plan.plan.endpoints.newGameServerAdmin.endpoint, "127.0.0.1:7501");
  assert.equal(plan.plan.endpoints.gameProxyAdmin.url, "http://127.0.0.1:7101");
  assert.equal(plan.plan.routeCas.proxyExpectedRoomVersion, "auto-from-existing-route");
  assert.equal(plan.plan.routeCas.proxyRoomVersion, "auto-next-version");
  assert.equal(plan.plan.timeoutMs, 6000);
});

test("rollout transfer cli dry-run reports missing required arguments without service calls", () => {
  const options = parseArgs(["--dry-run"]);

  const validation = validateTransferCliOptions(options);
  const plan = buildTransferCliDryRunPlan(options);

  assert.equal(validation.ok, false);
  assert.equal(plan.ok, false);
  assert.equal(plan.safety.callsControlPlane, false);
  assert.deepEqual(validation.errors, [
    "missing required option --rollout-epoch",
    "missing required option --room-id",
    "missing required option --old-server-id",
    "missing required option --new-server-id"
  ]);
  assert.deepEqual(
    plan.validation.requiredOptions.map((option) => [option.name, option.present]),
    [
      ["--rollout-epoch", false],
      ["--room-id", false],
      ["--old-server-id", false],
      ["--new-server-id", false]
    ]
  );
});

test("rollout transfer cli redirect dry-run builds redirect-only plan", () => {
  const options = parseArgs([
    "--dry-run",
    "--trigger-redirect-only",
    "--rollout-epoch", "rollout-test",
    "--room-id", "room-test",
    "--old-admin-port", "7500",
    "--redirect-target-host", "127.0.0.1",
    "--redirect-target-port", "4000",
    "--redirect-target-server-id", "game-proxy",
    "--redirect-retry-after-ms", "250"
  ]);

  const plan = buildTransferCliDryRunPlan(options);

  assert.equal(plan.ok, true);
  assert.equal(plan.mode, "redirect-dry-run");
  assert.deepEqual(plan.plan.plannedCalls, ["old.triggerServerRedirect"]);
  assert.equal(plan.safety.callsControlPlane, false);
  assert.equal(plan.plan.redirectTarget.host, "127.0.0.1");
  assert.equal(plan.plan.redirectTarget.port, 4000);
  assert.equal(plan.plan.redirectTarget.serverId, "game-proxy");
  assert.equal(plan.plan.redirectTarget.retryAfterMs, 250);
});

test("rollout transfer cli rejects unsafe same-server transfer plan", () => {
  const options = parseArgs([
    "--dry-run",
    "--rollout-epoch", "rollout-test",
    "--room-id", "room-test",
    "--old-server-id", "game-server",
    "--new-server-id", "game-server",
    "--old-admin-port", "7500",
    "--new-admin-port", "7500"
  ]);

  const plan = buildTransferCliDryRunPlan(options);

  assert.equal(plan.ok, false);
  assert(plan.validation.errors.includes("--old-server-id and --new-server-id must be different for a transfer drill"));
  assert(plan.validation.errors.includes("old and new game-server admin endpoints must be different for a three-process transfer drill"));
});
