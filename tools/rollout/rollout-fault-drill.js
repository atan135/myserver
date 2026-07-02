import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";

import {
  GameServerTransferClient,
  ProxyAdminClient,
  ROOM_TRANSFER_FAILURE_INJECTION,
  ROOM_TRANSFER_STAGE,
  encodeRoomTransferPayloadForTest,
  orchestrateRoomTransfer
} from "./rollout-transfer.js";
import {
  controlTargetPlan,
  createDefaultRolloutTargetOptions,
  resolveAndApplyRolloutControlTargets
} from "./rollout-targets.js";

export const ROLLOUT_FAULT_DRILL = {
  IMPORT_FAILURE: "import-failure",
  ROUTE_UPSERT_FAILURE: "route-upsert-failure",
  ROUTE_METADATA_MISSING: "route-metadata-missing",
  REDIRECT_NO_RECONNECT: "redirect-no-reconnect"
};

export const REDIRECT_NO_RECONNECT_STAGE = "redirect_no_reconnect";

export const ROLLOUT_FAULT_DRILL_DEFINITIONS = [
  {
    name: ROLLOUT_FAULT_DRILL.IMPORT_FAILURE,
    type: "transfer",
    title: "Import failure",
    expectedStage: ROOM_TRANSFER_STAGE.NEW_IMPORT,
    expectedFailure: true,
    failureInjection: {
      stage: ROOM_TRANSFER_STAGE.NEW_IMPORT,
      mode: ROOM_TRANSFER_FAILURE_INJECTION.IMPORT_CORRUPT_PAYLOAD
    },
    expectedCompletedStages: [
      ROOM_TRANSFER_STAGE.OLD_FREEZE,
      ROOM_TRANSFER_STAGE.OLD_EXPORT
    ],
    mustNotCompleteStages: [
      ROOM_TRANSFER_STAGE.NEW_IMPORT,
      ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP,
      ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
      ROOM_TRANSFER_STAGE.OLD_RETIRE
    ],
    safety: [
      "corrupts the transfer payload before new import",
      "stops at new_import when import/checksum validation rejects it",
      "does not confirm ownership, upsert proxy route, or retire the old room"
    ]
  },
  {
    name: ROLLOUT_FAULT_DRILL.ROUTE_UPSERT_FAILURE,
    type: "transfer",
    title: "Route upsert failure",
    expectedStage: ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
    expectedFailure: true,
    failureInjection: {
      stage: ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
      mode: ROOM_TRANSFER_FAILURE_INJECTION.PROXY_BAD_EXPECTED_ROOM_VERSION
    },
    expectedCompletedStages: [
      ROOM_TRANSFER_STAGE.OLD_FREEZE,
      ROOM_TRANSFER_STAGE.OLD_EXPORT,
      ROOM_TRANSFER_STAGE.NEW_IMPORT,
      ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP
    ],
    mustNotCompleteStages: [
      ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
      ROOM_TRANSFER_STAGE.OLD_RETIRE
    ],
    safety: [
      "uses an intentionally wrong expected_room_version for proxy CAS",
      "stops at proxy_route_upsert when game-proxy rejects the route update",
      "does not retire the old room after a failed proxy route switch"
    ]
  },
  {
    name: ROLLOUT_FAULT_DRILL.ROUTE_METADATA_MISSING,
    type: "transfer",
    title: "Route metadata missing",
    expectedStage: ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
    expectedFailure: true,
    expectedErrorCode: "ROOM_ROUTE_METADATA_MISSING",
    failureInjection: {
      stage: ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
      mode: ROOM_TRANSFER_FAILURE_INJECTION.PROXY_MISSING_ROUTE_METADATA
    },
    simulateExistingRoute: null,
    expectedCompletedStages: [
      ROOM_TRANSFER_STAGE.OLD_FREEZE,
      ROOM_TRANSFER_STAGE.OLD_EXPORT,
      ROOM_TRANSFER_STAGE.NEW_IMPORT,
      ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP
    ],
    mustNotCompleteStages: [
      ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
      ROOM_TRANSFER_STAGE.OLD_RETIRE
    ],
    safety: [
      "requires proxy getRoomRoute to observe that pre-existing room metadata is actually missing",
      "treats missing metadata as a proxy_route_upsert-stage failure before POST /room-route/upsert",
      "stops at proxy_route_upsert and does not retire the old room"
    ]
  },
  {
    name: ROLLOUT_FAULT_DRILL.REDIRECT_NO_RECONNECT,
    type: "redirect",
    title: "Redirect without client reconnect",
    expectedStage: REDIRECT_NO_RECONNECT_STAGE,
    expectedFailure: true,
    expectedCompletedStages: [],
    mustNotCompleteStages: [],
    safety: [
      "only triggers or plans ServerRedirectPush",
      "does not run server-redirect-reconnect",
      "does not claim mybevy has redirect/reconnect support"
    ]
  }
];

const DEFAULT_TIMEOUT_MS = 5000;

function nowIso() {
  return new Date().toISOString();
}

function normalizeError(error) {
  if (!error) {
    return { message: "unknown error", code: "UNKNOWN_ERROR" };
  }
  if (typeof error === "string") {
    return { message: error, code: error };
  }
  return {
    message: error.message || String(error),
    code: error.code || error.errorCode || "ERROR"
  };
}

function sanitizeTimestamp(value) {
  return value.replace(/[:.]/g, "-");
}

function drillByName(name) {
  return ROLLOUT_FAULT_DRILL_DEFINITIONS.find((definition) => definition.name === name);
}

export function listRolloutFaultDrills() {
  return ROLLOUT_FAULT_DRILL_DEFINITIONS.map((definition) => ({ ...definition }));
}

export function selectRolloutFaultDrills(names = []) {
  const normalized = names.length > 0 ? names : ["all"];
  if (normalized.includes("all")) {
    return listRolloutFaultDrills();
  }

  return normalized.map((name) => {
    const definition = drillByName(name);
    if (!definition) {
      throw new Error(`unknown fault drill ${name}`);
    }
    return { ...definition };
  });
}

function displayValue(value, fallback) {
  return value === undefined || value === null || value === "" ? fallback : value;
}

function buildTransferPlan(definition, options) {
  const reachesProxyRouteStage = definition.expectedCompletedStages.includes(
    ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP
  );
  const proxyRouteCalls = reachesProxyRouteStage
    ? [
      "new.confirmRoomOwnership",
      "proxy.getRoomRoute",
      ...(definition.name === ROLLOUT_FAULT_DRILL.ROUTE_METADATA_MISSING
        ? []
        : ["proxy.upsertRoomRoute"])
    ]
    : [];
  const common = {
    rolloutEpoch: displayValue(options.rolloutEpoch, "<ROLLOUT_EPOCH>"),
    roomId: displayValue(options.roomId, "<ROOM_ID>"),
    oldServerId: displayValue(options.oldServerId, "<OLD_SERVER_ID>"),
    newServerId: displayValue(options.newServerId, "<NEW_SERVER_ID>")
  };

  return {
    ...common,
    expectedFailure: true,
    expectedStage: definition.expectedStage,
    ...(definition.expectedErrorCode ? { expectedErrorCode: definition.expectedErrorCode } : {}),
    failureInjection: definition.failureInjection,
    plannedCalls: [
      "old.freezeRoomForTransfer",
      "old.exportRoomTransfer",
      "new.importRoomTransfer",
      ...proxyRouteCalls
    ],
    skippedAfterExpectedFailure: definition.mustNotCompleteStages,
    endpoints: {
      oldGameServerAdmin: controlTargetPlan(options, "oldGameServerAdmin"),
      newGameServerAdmin: controlTargetPlan(options, "newGameServerAdmin"),
      gameProxyAdmin: controlTargetPlan(options, "gameProxyAdmin")
    }
  };
}

function buildRedirectPlan(definition, options) {
  return {
    rolloutEpoch: displayValue(options.rolloutEpoch, "<ROLLOUT_EPOCH>"),
    roomId: displayValue(options.roomId, "<ROOM_ID>"),
    expectedFailure: true,
    expectedStage: definition.expectedStage,
    plannedCalls: ["old.triggerServerRedirect"],
    reconnectAttempted: false,
    skippedAfterExpectedFailure: ["server-redirect-reconnect", "mybevy reconnect"],
    redirectTarget: {
      host: displayValue(options.redirectTargetHost, "<REDIRECT_TARGET_HOST>"),
      port: displayValue(options.redirectTargetPort, "<REDIRECT_TARGET_PORT>"),
      serverId: displayValue(options.redirectTargetServerId || options.newServerId, "<TARGET_SERVER_ID>"),
      transport: options.redirectTransport || "kcp",
      retryAfterMs: options.redirectRetryAfterMs || 0
    }
  };
}

function buildDrillPlan(definition, options) {
  const plan = definition.type === "redirect"
    ? buildRedirectPlan(definition, options)
    : buildTransferPlan(definition, options);
  return {
    name: definition.name,
    title: definition.title,
    type: definition.type,
    safety: definition.safety,
    plan
  };
}

function buildDryRunReport(options, definitions) {
  return {
    ok: true,
    generatedAt: nowIso(),
    mode: "dry-run",
    execute: false,
    simulate: false,
    archive: { written: false },
    safety: {
      startsServices: false,
      callsControlPlane: false,
      requestsShutdown: false,
      runsReconnectClient: false
    },
    drills: definitions.map((definition) => buildDrillPlan(definition, options))
  };
}

function createRealTransferClients(options) {
  return {
    oldServer: new GameServerTransferClient({
      host: options.oldAdminHost,
      port: options.oldAdminPort,
      token: options.oldAdminToken || "",
      timeoutMs: options.timeoutMs || DEFAULT_TIMEOUT_MS
    }),
    newServer: new GameServerTransferClient({
      host: options.newAdminHost,
      port: options.newAdminPort,
      token: options.newAdminToken || "",
      timeoutMs: options.timeoutMs || DEFAULT_TIMEOUT_MS
    }),
    proxy: new ProxyAdminClient({
      baseUrl: options.proxyAdminUrl,
      token: options.proxyAdminToken || "",
      actor: options.proxyAdminActor || "rollout-fault-drill",
      timeoutMs: options.timeoutMs || DEFAULT_TIMEOUT_MS
    })
  };
}

export function createSimulatedTransferClients(options = {}) {
  const calls = [];
  const rolloutEpoch = options.rolloutEpoch || "rollout-fault-drill-sim";
  const roomId = options.roomId || "room-fault-drill";
  const checksum = options.checksum || "checksum-fault-drill";
  const payloadRaw = encodeRoomTransferPayloadForTest({
    rolloutEpoch,
    roomId,
    roomVersion: 2,
    checksum
  });
  const existingRoute = Object.hasOwn(options, "existingRoute")
    ? options.existingRoute
    : {
      room_id: roomId,
      room_version: 1,
      last_transfer_checksum: ""
    };

  const oldServer = {
    async freezeRoomForTransfer() {
      calls.push("old.freezeRoomForTransfer");
      return { ok: true, roomId, roomVersion: 1 };
    },
    async exportRoomTransfer() {
      calls.push("old.exportRoomTransfer");
      return {
        ok: true,
        roomId,
        checksum,
        payload: { raw: payloadRaw, roomVersion: 2, checksum }
      };
    },
    async retireTransferredRoom() {
      calls.push("old.retireTransferredRoom");
      return { ok: true, roomId };
    }
  };

  const newServer = {
    async importRoomTransfer(request) {
      calls.push("new.importRoomTransfer");
      if (!Buffer.from(request.payloadRaw).equals(payloadRaw)) {
        throw Object.assign(new Error("ROOM_TRANSFER_CHECKSUM_MISMATCH"), {
          code: "ROOM_TRANSFER_CHECKSUM_MISMATCH"
        });
      }
      return { ok: true, roomId, checksum, roomVersion: 3 };
    },
    async confirmRoomOwnership(request) {
      calls.push("new.confirmRoomOwnership");
      return {
        ok: true,
        roomId,
        checksum: request.checksum,
        roomVersion: request.roomVersion
      };
    }
  };

  const proxy = {
    async getRoomRoute() {
      calls.push("proxy.getRoomRoute");
      return existingRoute;
    },
    async upsertRoomRoute(route) {
      calls.push("proxy.upsertRoomRoute");
      const expectedRoomVersion = existingRoute?.room_version ?? existingRoute?.roomVersion ?? 0;
      const expectedChecksum = existingRoute?.last_transfer_checksum ??
        existingRoute?.lastTransferChecksum ??
        "";
      if (route.expectedRoomVersion !== expectedRoomVersion) {
        throw Object.assign(new Error("ROOM_ROUTE_VERSION_MISMATCH"), {
          code: "ROOM_ROUTE_VERSION_MISMATCH"
        });
      }
      if ((route.expectedLastTransferChecksum || "") !== expectedChecksum) {
        throw Object.assign(new Error("ROOM_ROUTE_CHECKSUM_MISMATCH"), {
          code: "ROOM_ROUTE_CHECKSUM_MISMATCH"
        });
      }
      return { ok: true };
    }
  };

  return { calls, clients: { oldServer, newServer, proxy } };
}

function buildTransferRequest(definition, options) {
  return {
    rolloutEpoch: options.rolloutEpoch || "rollout-fault-drill-sim",
    roomId: options.roomId || "room-fault-drill",
    oldServerId: options.oldServerId || "game-server-old",
    newServerId: options.newServerId || "game-server-new",
    proxyExpectedRoomVersion: options.proxyExpectedRoomVersion,
    proxyRoomVersion: options.proxyRoomVersion,
    proxyExpectedLastTransferChecksum: options.proxyExpectedLastTransferChecksum,
    requireExistingRouteMetadata: definition.name === ROLLOUT_FAULT_DRILL.ROUTE_METADATA_MISSING,
    expectMissingRouteMetadataFailure: definition.name === ROLLOUT_FAULT_DRILL.ROUTE_METADATA_MISSING,
    failureInjection: definition.failureInjection
  };
}

async function runTransferDrill(definition, options, mode) {
  const mock = mode === "simulate"
    ? createSimulatedTransferClients({
      ...options,
      ...(Object.hasOwn(definition, "simulateExistingRoute")
        ? { existingRoute: definition.simulateExistingRoute }
        : {})
    })
    : null;
  const clients = mock?.clients || createRealTransferClients(options);
  const result = await orchestrateRoomTransfer(buildTransferRequest(definition, options), clients);
  if (!mock) {
    return result;
  }
  return {
    ...result,
    mockCalls: [...mock.calls]
  };
}

function createOldServerClient(options) {
  return new GameServerTransferClient({
    host: options.oldAdminHost,
    port: options.oldAdminPort,
    token: options.oldAdminToken || "",
    timeoutMs: options.timeoutMs || DEFAULT_TIMEOUT_MS
  });
}

async function runRedirectDrill(options, mode) {
  const trigger = mode === "simulate"
    ? {
      ok: true,
      roomId: options.roomId || "room-fault-drill",
      deliveredCount: 1,
      failedCount: 0,
      onlineMemberCount: 1
    }
    : await createOldServerClient(options).triggerServerRedirect({
      rolloutEpoch: options.rolloutEpoch,
      roomId: options.roomId,
      reason: options.redirectReason || "rollout_fault_drill_redirect_no_reconnect",
      targetHost: options.redirectTargetHost,
      targetPort: options.redirectTargetPort,
      targetServerId: options.redirectTargetServerId || options.newServerId || "",
      transport: options.redirectTransport || "kcp",
      retryAfterMs: options.redirectRetryAfterMs || 0
    });

  if (!trigger.ok) {
    return {
      ok: false,
      stage: REDIRECT_NO_RECONNECT_STAGE,
      expectedFailure: false,
      errorCode: trigger.errorCode || "SERVER_REDIRECT_TRIGGER_FAILED",
      error: trigger.errorCode || "ServerRedirectPush trigger failed",
      reconnectAttempted: false,
      trigger
    };
  }

  return {
    ok: false,
    stage: REDIRECT_NO_RECONNECT_STAGE,
    errorCode: "CLIENT_DID_NOT_RECONNECT",
    error: "ServerRedirectPush was triggered; reconnect is intentionally not attempted in this drill.",
    expectedFailure: true,
    reconnectAttempted: false,
    trigger
  };
}

function hasNoForbiddenCompletedStage(result, definition) {
  const completed = new Set(result.completedStages || []);
  return definition.mustNotCompleteStages.every((stage) => !completed.has(stage));
}

function hasExpectedCompletedStages(result, definition) {
  const completed = result.completedStages || [];
  return definition.expectedCompletedStages.every((stage, index) => completed[index] === stage);
}

function validateDrillResult(definition, result) {
  if (definition.type === "redirect") {
    const ok = result.ok === false &&
      result.stage === definition.expectedStage &&
      result.expectedFailure === true &&
      result.reconnectAttempted === false;
    return {
      ok,
      expectedFailureObserved: result.expectedFailure === true,
      stoppedAtExpectedStage: result.stage === definition.expectedStage,
      reconnectAttempted: result.reconnectAttempted === true
    };
  }

  const stoppedAtExpectedStage = result.ok === false && result.stage === definition.expectedStage;
  const expectedFailureObserved = result.expectedFailure === true;
  const expectedErrorCodeObserved = !definition.expectedErrorCode ||
    result.errorCode === definition.expectedErrorCode;
  const noForbiddenCompletedStage = hasNoForbiddenCompletedStage(result, definition);
  const expectedCompletedStages = hasExpectedCompletedStages(result, definition);
  return {
    ok: stoppedAtExpectedStage &&
      expectedFailureObserved &&
      expectedErrorCodeObserved &&
      noForbiddenCompletedStage &&
      expectedCompletedStages,
    stoppedAtExpectedStage,
    expectedFailureObserved,
    expectedErrorCodeObserved,
    noForbiddenCompletedStage,
    expectedCompletedStages
  };
}

async function runOneDrill(definition, options, mode) {
  try {
    const result = definition.type === "redirect"
      ? await runRedirectDrill(options, mode)
      : await runTransferDrill(definition, options, mode);
    const validation = validateDrillResult(definition, result);
    return {
      name: definition.name,
      title: definition.title,
      type: definition.type,
      validation,
      result
    };
  } catch (error) {
    const normalized = normalizeError(error);
    const result = {
      ok: false,
      stage: definition.expectedStage,
      expectedFailure: false,
      errorCode: normalized.code,
      error: normalized.message
    };
    return {
      name: definition.name,
      title: definition.title,
      type: definition.type,
      validation: {
        ok: false,
        stoppedAtExpectedStage: false,
        expectedFailureObserved: false
      },
      result
    };
  }
}

async function maybeArchive(report, options) {
  const requestedPath = options.archiveFile || (
    options.archiveDir
      ? path.join(
        options.archiveDir,
        `rollout-fault-drill-${sanitizeTimestamp(report.generatedAt)}.json`
      )
      : ""
  );

  if (!requestedPath) {
    report.archive = { written: false };
    return report;
  }

  const archivePath = path.resolve(process.cwd(), requestedPath);
  report.archive = { written: true, path: archivePath };
  await mkdir(path.dirname(archivePath), { recursive: true });
  await writeFile(archivePath, `${JSON.stringify(report, null, 2)}\n`, "utf8");
  return report;
}

export async function runRolloutFaultDrills(options = {}) {
  options = {
    ...createDefaultRolloutTargetOptions(),
    oldServerId: "game-server-old",
    newServerId: "game-server-new",
    ...options
  };
  const definitions = selectRolloutFaultDrills(options.drills || []);
  const mode = options.execute ? "execute" : options.simulate ? "simulate" : "dry-run";

  if (mode === "dry-run") {
    return maybeArchive(buildDryRunReport(options, definitions), options);
  }

  if (mode === "execute" && !options.resolvedControlTargets) {
    await resolveAndApplyRolloutControlTargets(options, {
      requireNew: definitions.some((definition) => definition.type !== "redirect"),
      requireProxy: definitions.some((definition) => definition.type !== "redirect")
    });
  }

  const results = [];
  for (const definition of definitions) {
    results.push(await runOneDrill(definition, options, mode));
  }

  const report = {
    ok: results.every((item) => item.validation.ok),
    generatedAt: nowIso(),
    mode,
    execute: mode === "execute",
    simulate: mode === "simulate",
    archive: { written: false },
    safety: {
      startsServices: false,
      callsControlPlane: mode === "execute",
      requestsShutdown: false,
      runsReconnectClient: false
    },
    drills: results
  };

  return maybeArchive(report, options);
}
