import assert from "node:assert/strict";
import test from "node:test";

import {
  buildTransferCliFatalErrorEnvelope,
  buildTransferCliParseErrorEnvelope,
  buildTransferCliExecutionEnvelope,
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
    "--proxy-admin-actor", "ops@example.com",
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
  assert.equal(plan.plan.endpoints.gameProxyAdmin.actor, "ops@example.com");
  assert.equal(plan.plan.endpoints.gameProxyAdmin.actorState, "set");
  assert.equal(plan.plan.routeCas.proxyExpectedRoomVersion, "auto-from-existing-route");
  assert.equal(plan.plan.routeCas.proxyRoomVersion, "auto-next-version");
  assert.equal(plan.plan.routeMetadata.requiredExistingRoute, false);
  assert.equal(plan.plan.routeMetadata.actionOnMissing, "allow_first_route_create");
  assert.equal(plan.plan.timeoutMs, 6000);
});

test("rollout transfer cli dry-run can require existing route metadata", () => {
  const options = parseArgs([
    "--dry-run",
    "--rollout-epoch", "rollout-test",
    "--room-id", "room-test",
    "--old-server-id", "game-server-old",
    "--new-server-id", "game-server-new",
    "--old-admin-port", "7500",
    "--new-admin-port", "7501",
    "--proxy-admin-url", "http://127.0.0.1:7101",
    "--proxy-admin-actor", "ops@example.com",
    "--require-existing-route-metadata"
  ]);

  const plan = buildTransferCliDryRunPlan(options);

  assert.equal(plan.ok, true);
  assert.equal(plan.plan.routeMetadata.requiredExistingRoute, true);
  assert.equal(plan.plan.routeMetadata.actionOnMissing, "fail_before_proxy_route_upsert");
});

test("rollout transfer cli rejects invalid proxy admin actor before service calls", () => {
  const options = parseArgs([
    "--dry-run",
    "--rollout-epoch", "rollout-test",
    "--room-id", "room-test",
    "--old-server-id", "game-server-old",
    "--new-server-id", "game-server-new",
    "--old-admin-port", "7500",
    "--new-admin-port", "7501",
    "--proxy-admin-url", "http://127.0.0.1:7101",
    "--proxy-admin-actor", "bad actor"
  ]);

  const validation = validateTransferCliOptions(options);
  const plan = buildTransferCliDryRunPlan(options);

  assert.equal(validation.ok, false);
  assert.equal(plan.ok, false);
  assert.equal(plan.safety.callsControlPlane, false);
  assert(validation.errors.includes("invalid option --proxy-admin-actor: expected 1-128 chars matching [A-Za-z0-9_.@-]"));
});

test("rollout transfer cli parse error envelope is machine-readable and safe", () => {
  const envelope = buildTransferCliParseErrorEnvelope(new Error("unknown option --bad"));

  assert.equal(envelope.ok, false);
  assert.equal(envelope.mode, "argument-error");
  assert.equal(envelope.safety.callsControlPlane, false);
  assert.equal(envelope.summary.stage, "argument_parse");
  assert.equal(envelope.summary.errorCode, "INVALID_OPTIONS");
  assert.deepEqual(envelope.validation.errors, ["unknown option --bad"]);
});

test("rollout transfer cli fatal error envelope is machine-readable and safe", () => {
  const error = Object.assign(new Error("unexpected failure"), { code: "UNEXPECTED_FAILURE" });
  const envelope = buildTransferCliFatalErrorEnvelope(error);

  assert.equal(envelope.ok, false);
  assert.equal(envelope.mode, "fatal-error");
  assert.equal(envelope.safety.startsServices, false);
  assert.equal(envelope.safety.callsControlPlane, false);
  assert.equal(envelope.safety.requestsShutdown, false);
  assert.equal(envelope.summary.stage, "fatal");
  assert.equal(envelope.summary.errorCode, "UNEXPECTED_FAILURE");
  assert.deepEqual(envelope.validation.errors, ["unexpected failure"]);
});

test("rollout transfer cli execution envelope summarizes transfer result", () => {
  const options = parseArgs([
    "--rollout-epoch", "rollout-test",
    "--room-id", "room-test",
    "--old-server-id", "game-server-old",
    "--new-server-id", "game-server-new",
    "--old-admin-port", "7500",
    "--new-admin-port", "7501",
    "--proxy-admin-url", "http://127.0.0.1:7101",
    "--proxy-admin-actor", "rollout-drill"
  ]);

  const envelope = buildTransferCliExecutionEnvelope(options, {
    ok: true,
    stage: "complete",
    completedStages: [
      ROOM_TRANSFER_STAGE.OLD_FREEZE,
      ROOM_TRANSFER_STAGE.OLD_EXPORT,
      ROOM_TRANSFER_STAGE.NEW_IMPORT,
      ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP,
      ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
      ROOM_TRANSFER_STAGE.OLD_RETIRE
    ],
    exported: { checksum: "checksum-1", roomVersion: 2 },
    imported: { checksum: "checksum-1", roomVersion: 3 },
    confirmed: { checksum: "checksum-1", roomVersion: 3 },
    proxyRoute: { roomVersion: 2, importedRoomVersion: 3 }
  });

  assert.equal(envelope.ok, true);
  assert.equal(envelope.mode, "transfer-execute");
  assert.equal(envelope.dryRun, false);
  assert.equal(envelope.safety.callsControlPlane, true);
  assert.equal(envelope.safety.requestsShutdown, false);
  assert.equal(envelope.validation.ok, true);
  assert.equal(envelope.summary.stage, "complete");
  assert.equal(envelope.summary.checksum, "checksum-1");
  assert.equal(envelope.summary.importedRoomVersion, 3);
  assert.equal(envelope.summary.proxyRoomVersion, 2);
});

test("rollout transfer cli execution envelope reports missing route metadata", () => {
  const options = parseArgs([
    "--rollout-epoch", "rollout-test",
    "--room-id", "room-test",
    "--old-server-id", "game-server-old",
    "--new-server-id", "game-server-new",
    "--old-admin-port", "7500",
    "--new-admin-port", "7501",
    "--proxy-admin-url", "http://127.0.0.1:7101",
    "--proxy-admin-actor", "rollout-drill",
    "--require-existing-route-metadata"
  ]);

  const envelope = buildTransferCliExecutionEnvelope(options, {
    ok: false,
    stage: ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
    errorCode: "ROOM_ROUTE_METADATA_MISSING",
    error: "ROOM_ROUTE_METADATA_MISSING",
    completedStages: [
      ROOM_TRANSFER_STAGE.OLD_FREEZE,
      ROOM_TRANSFER_STAGE.OLD_EXPORT,
      ROOM_TRANSFER_STAGE.NEW_IMPORT,
      ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP
    ],
    routeMetadata: {
      requiredExistingRoute: true,
      found: false,
      checkedVia: "proxy.getRoomRoute",
      actionOnMissing: "fail_before_proxy_route_upsert"
    }
  });

  assert.equal(envelope.ok, false);
  assert.equal(envelope.summary.stage, ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT);
  assert.equal(envelope.summary.errorCode, "ROOM_ROUTE_METADATA_MISSING");
  assert.deepEqual(envelope.summary.completedStages, [
    ROOM_TRANSFER_STAGE.OLD_FREEZE,
    ROOM_TRANSFER_STAGE.OLD_EXPORT,
    ROOM_TRANSFER_STAGE.NEW_IMPORT,
    ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP
  ]);
  assert.deepEqual(envelope.summary.routeMetadata, {
    requiredExistingRoute: true,
    found: false,
    checkedVia: "proxy.getRoomRoute",
    actionOnMissing: "fail_before_proxy_route_upsert"
  });
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

test("rollout transfer cli dry-run allows display placeholders but execution validation rejects them", () => {
  const options = parseArgs([
    "--dry-run",
    "--rollout-epoch", "<ROLLOUT_EPOCH>",
    "--room-id", "<ROOM_ID>",
    "--old-server-id", "game-server-old",
    "--new-server-id", "game-server-new"
  ]);

  const plan = buildTransferCliDryRunPlan(options);
  const executeValidation = validateTransferCliOptions(options);

  assert.equal(plan.ok, true);
  assert.equal(plan.plan.rolloutEpoch, "<ROLLOUT_EPOCH>");
  assert.equal(plan.plan.roomId, "<ROOM_ID>");
  assert.equal(executeValidation.ok, false);
  assert(executeValidation.errors.includes("invalid option --rollout-epoch: placeholder values are only allowed in --dry-run"));
  assert(executeValidation.errors.includes("invalid option --room-id: placeholder values are only allowed in --dry-run"));
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
