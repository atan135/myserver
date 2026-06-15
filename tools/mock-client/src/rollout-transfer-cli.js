import {
  GameServerTransferClient,
  ROOM_TRANSFER_STAGE,
  ProxyAdminClient,
  orchestrateRoomTransfer
} from "./rollout-transfer.js";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

function optionValue(argv, index) {
  return { value: argv[index + 1] || "", nextIndex: index + 1 };
}

function printUsage() {
  console.log(`Usage:
  node tools/mock-client/src/rollout-transfer-cli.js \\
    --rollout-epoch <epoch> \\
    --room-id <room-id> \\
    --old-server-id <server-id> \\
    --new-server-id <server-id> [options]

Required:
  --rollout-epoch <epoch>
  --room-id <room-id>
  --old-server-id <server-id>
  --new-server-id <server-id>

Options:
  --dry-run                              validate arguments and print a JSON plan; no service calls
  The default transfer order is old_freeze -> old_export -> new_import ->
  new_confirm_ownership -> proxy_route_upsert -> old_retire.
  new_confirm_ownership uses the checksum and roomVersion returned by import.
  --trigger-redirect-only                 only call old game-server TriggerServerRedirectReq
  --redirect-target-host <host>           required with --trigger-redirect-only
  --redirect-target-port <port>           required with --trigger-redirect-only
  --redirect-target-server-id <server-id> default: --new-server-id
  --redirect-transport <transport>        default: kcp
  --redirect-reason <reason>              default: rollout_redirect
  --redirect-retry-after-ms <ms>          default: 0
  --old-admin-host <host>                 default: MYSERVER_OLD_GAME_ADMIN_HOST or 127.0.0.1
  --old-admin-port <port>                 default: MYSERVER_OLD_GAME_ADMIN_PORT or 7500
  --old-admin-token <token>               default: MYSERVER_OLD_GAME_ADMIN_TOKEN or GAME_ADMIN_TOKEN
  --new-admin-host <host>                 default: MYSERVER_NEW_GAME_ADMIN_HOST or 127.0.0.1
  --new-admin-port <port>                 default: MYSERVER_NEW_GAME_ADMIN_PORT or 7501
  --new-admin-token <token>               default: MYSERVER_NEW_GAME_ADMIN_TOKEN or GAME_ADMIN_TOKEN
  --proxy-admin-url <url>                 default: MYSERVER_PROXY_ADMIN_URL or http://127.0.0.1:7101
  --proxy-admin-token <token>             default: PROXY_ADMIN_TOKEN
  --proxy-admin-actor <actor>             default: MYSERVER_PROXY_ADMIN_ACTOR or rollout-transfer-cli
  --proxy-expected-room-version <n>
  --proxy-room-version <n>
  --proxy-expected-last-transfer-checksum <checksum>
  --require-existing-route-metadata       fail before /room-route/upsert if proxy has no room route
  --timeout-ms <ms>                       default: 5000
  -h, --help`);
}

function parseNumber(value, fallback) {
  if (value === "" || value === undefined || value === null) {
    return fallback;
  }
  if (!/^-?\d+$/.test(String(value))) {
    return Number.NaN;
  }
  return Number.parseInt(value, 10);
}

export function parseArgs(argv) {
  const options = {
    oldAdminHost: process.env.MYSERVER_OLD_GAME_ADMIN_HOST || "127.0.0.1",
    oldAdminPort: parseNumber(process.env.MYSERVER_OLD_GAME_ADMIN_PORT, 7500),
    oldAdminToken: process.env.MYSERVER_OLD_GAME_ADMIN_TOKEN || process.env.GAME_ADMIN_TOKEN || "",
    newAdminHost: process.env.MYSERVER_NEW_GAME_ADMIN_HOST || "127.0.0.1",
    newAdminPort: parseNumber(process.env.MYSERVER_NEW_GAME_ADMIN_PORT, 7501),
    newAdminToken: process.env.MYSERVER_NEW_GAME_ADMIN_TOKEN || process.env.GAME_ADMIN_TOKEN || "",
    proxyAdminUrl: process.env.MYSERVER_PROXY_ADMIN_URL || "http://127.0.0.1:7101",
    proxyAdminToken: process.env.PROXY_ADMIN_TOKEN || "",
    proxyAdminActor: process.env.MYSERVER_PROXY_ADMIN_ACTOR || "rollout-transfer-cli",
    timeoutMs: 5000
  };

  const takeValue = (index) => {
    const { value, nextIndex } = optionValue(argv, index);
    if (!value || value.startsWith("--")) {
      throw new Error(`missing value for ${argv[index]}`);
    }
    return { value, nextIndex };
  };

  const takeNumber = (index, fallback) => {
    const { value, nextIndex } = takeValue(index);
    return { value: parseNumber(value, fallback), nextIndex };
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    switch (arg) {
      case "-h":
      case "--help":
        options.help = true;
        break;
      case "--dry-run":
        options.dryRun = true;
        break;
      case "--rollout-epoch":
        ({ value: options.rolloutEpoch, nextIndex: index } = takeValue(index));
        break;
      case "--room-id":
        ({ value: options.roomId, nextIndex: index } = takeValue(index));
        break;
      case "--old-server-id":
        ({ value: options.oldServerId, nextIndex: index } = takeValue(index));
        break;
      case "--new-server-id":
        ({ value: options.newServerId, nextIndex: index } = takeValue(index));
        break;
      case "--trigger-redirect-only":
        options.triggerRedirectOnly = true;
        break;
      case "--redirect-target-host":
        ({ value: options.redirectTargetHost, nextIndex: index } = takeValue(index));
        break;
      case "--redirect-target-port":
        ({ value: options.redirectTargetPort, nextIndex: index } = takeNumber(index, 0));
        break;
      case "--redirect-target-server-id":
        ({ value: options.redirectTargetServerId, nextIndex: index } = takeValue(index));
        break;
      case "--redirect-transport":
        ({ value: options.redirectTransport, nextIndex: index } = takeValue(index));
        break;
      case "--redirect-reason":
        ({ value: options.redirectReason, nextIndex: index } = takeValue(index));
        break;
      case "--redirect-retry-after-ms":
        ({ value: options.redirectRetryAfterMs, nextIndex: index } = takeNumber(index, 0));
        break;
      case "--old-admin-host":
        ({ value: options.oldAdminHost, nextIndex: index } = takeValue(index));
        break;
      case "--old-admin-port":
        ({ value: options.oldAdminPort, nextIndex: index } = takeNumber(index, options.oldAdminPort));
        break;
      case "--old-admin-token":
        ({ value: options.oldAdminToken, nextIndex: index } = takeValue(index));
        break;
      case "--new-admin-host":
        ({ value: options.newAdminHost, nextIndex: index } = takeValue(index));
        break;
      case "--new-admin-port":
        ({ value: options.newAdminPort, nextIndex: index } = takeNumber(index, options.newAdminPort));
        break;
      case "--new-admin-token":
        ({ value: options.newAdminToken, nextIndex: index } = takeValue(index));
        break;
      case "--proxy-admin-url":
        ({ value: options.proxyAdminUrl, nextIndex: index } = takeValue(index));
        break;
      case "--proxy-admin-token":
        ({ value: options.proxyAdminToken, nextIndex: index } = takeValue(index));
        break;
      case "--proxy-admin-actor":
        ({ value: options.proxyAdminActor, nextIndex: index } = takeValue(index));
        break;
      case "--proxy-expected-room-version":
        ({ value: options.proxyExpectedRoomVersion, nextIndex: index } = takeNumber(index, undefined));
        break;
      case "--proxy-room-version":
        ({ value: options.proxyRoomVersion, nextIndex: index } = takeNumber(index, undefined));
        break;
      case "--proxy-expected-last-transfer-checksum":
        ({ value: options.proxyExpectedLastTransferChecksum, nextIndex: index } = takeValue(index));
        break;
      case "--require-existing-route-metadata":
        options.requireExistingRouteMetadata = true;
        break;
      case "--timeout-ms":
        ({ value: options.timeoutMs, nextIndex: index } = takeNumber(index, options.timeoutMs));
        break;
      default:
        throw new Error(`unknown option ${arg}`);
    }
  }

  return options;
}

function optionName(key) {
  return `--${key.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}`;
}

function requiredOptionKeys(options) {
  return options.triggerRedirectOnly
    ? ["rolloutEpoch", "roomId", "redirectTargetHost", "redirectTargetPort"]
    : ["rolloutEpoch", "roomId", "oldServerId", "newServerId"];
}

function validPort(value) {
  return Number.isInteger(value) && value > 0 && value <= 65535;
}

function validateHttpUrl(value, name) {
  try {
    const parsed = new URL(value);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      throw new Error("expected http or https");
    }
    return null;
  } catch {
    return `invalid option ${name}: expected http(s) URL`;
  }
}

function tokenState(token) {
  if (!token) {
    return "missing";
  }
  if (token.startsWith("dev-only-change-this-")) {
    return "default-dev";
  }
  return "set";
}

function actorState(actor) {
  return actor ? "set" : "missing";
}

function validProxyAdminActor(value) {
  return typeof value === "string" &&
    value.length > 0 &&
    value.length <= 128 &&
    /^[A-Za-z0-9_.@-]+$/.test(value);
}

function isPlaceholderValue(value) {
  return typeof value === "string" && /^<[^>]+>$/.test(value);
}

export function validateTransferCliOptions(options, { allowPlaceholders = false } = {}) {
  const errors = [];
  const warnings = [];

  for (const key of requiredOptionKeys(options)) {
    if (!options[key]) {
      errors.push(`missing required option ${optionName(key)}`);
    } else if (!allowPlaceholders && isPlaceholderValue(options[key])) {
      errors.push(`invalid option ${optionName(key)}: placeholder values are only allowed in --dry-run`);
    }
  }

  if (options.triggerRedirectOnly) {
    if (!validPort(options.oldAdminPort)) {
      errors.push(`invalid option --old-admin-port: expected 1-65535`);
    }
    if (!validPort(options.redirectTargetPort)) {
      errors.push(`invalid option --redirect-target-port: expected 1-65535`);
    }
    if (!Number.isInteger(options.redirectRetryAfterMs ?? 0) || (options.redirectRetryAfterMs ?? 0) < 0) {
      errors.push(`invalid option --redirect-retry-after-ms: expected non-negative integer`);
    }
  } else {
    if (!validPort(options.oldAdminPort)) {
      errors.push(`invalid option --old-admin-port: expected 1-65535`);
    }
    if (!validPort(options.newAdminPort)) {
      errors.push(`invalid option --new-admin-port: expected 1-65535`);
    }
    const proxyUrlError = validateHttpUrl(options.proxyAdminUrl, "--proxy-admin-url");
    if (proxyUrlError) {
      errors.push(proxyUrlError);
    }
    if (options.oldServerId && options.newServerId && options.oldServerId === options.newServerId) {
      errors.push("--old-server-id and --new-server-id must be different for a transfer drill");
    }
    if (
      options.oldAdminHost &&
      options.newAdminHost &&
      options.oldAdminHost === options.newAdminHost &&
      options.oldAdminPort === options.newAdminPort
    ) {
      errors.push("old and new game-server admin endpoints must be different for a three-process transfer drill");
    }
    for (const key of [
      "proxyExpectedRoomVersion",
      "proxyRoomVersion"
    ]) {
      if (options[key] !== undefined && (!Number.isInteger(options[key]) || options[key] < 0)) {
        errors.push(`invalid option ${optionName(key)}: expected non-negative integer`);
      }
    }
  }

  if (!Number.isInteger(options.timeoutMs) || options.timeoutMs <= 0) {
    errors.push("invalid option --timeout-ms: expected positive integer");
  }

  if (!options.oldAdminToken) {
    warnings.push("old game-server admin token is missing");
  }
  if (!options.triggerRedirectOnly && !options.newAdminToken) {
    warnings.push("new game-server admin token is missing");
  }
  if (!options.triggerRedirectOnly && !options.proxyAdminToken) {
    warnings.push("game-proxy admin token is missing");
  }
  if (!options.triggerRedirectOnly && !validProxyAdminActor(options.proxyAdminActor)) {
    errors.push("invalid option --proxy-admin-actor: expected 1-128 chars matching [A-Za-z0-9_.@-]");
  }

  return {
    ok: errors.length === 0,
    errors,
    warnings,
    requiredOptions: requiredOptionKeys(options).map((key) => ({
      name: optionName(key),
      key,
      present: Boolean(options[key])
    }))
  };
}

function endpoint(host, port) {
  return `${host}:${port}`;
}

export function buildTransferCliDryRunPlan(options) {
  const validation = validateTransferCliOptions(options, { allowPlaceholders: true });
  const common = {
    rolloutEpoch: options.rolloutEpoch || "<ROLLOUT_EPOCH>",
    roomId: options.roomId || "<ROOM_ID>",
    oldServerId: options.oldServerId || "<OLD_SERVER_ID>",
    newServerId: options.newServerId || "<NEW_SERVER_ID>"
  };

  if (options.triggerRedirectOnly) {
    return {
      ok: validation.ok,
      mode: "redirect-dry-run",
      dryRun: true,
      safety: {
        startsServices: false,
        callsControlPlane: false,
        requestsShutdown: false,
        runsReconnectClient: false
      },
      validation,
      plan: {
        ...common,
        plannedCalls: ["old.triggerServerRedirect"],
        endpoints: {
          oldGameServerAdmin: {
            endpoint: endpoint(options.oldAdminHost, options.oldAdminPort),
            tokenState: tokenState(options.oldAdminToken)
          }
        },
        redirectTarget: {
          host: options.redirectTargetHost || "<REDIRECT_TARGET_HOST>",
          port: options.redirectTargetPort || "<REDIRECT_TARGET_PORT>",
          serverId: options.redirectTargetServerId || options.newServerId || "<TARGET_SERVER_ID>",
          transport: options.redirectTransport || "kcp",
          retryAfterMs: options.redirectRetryAfterMs || 0
        },
        timeoutMs: options.timeoutMs
      }
    };
  }

  return {
    ok: validation.ok,
    mode: "transfer-dry-run",
    dryRun: true,
    safety: {
      startsServices: false,
      callsControlPlane: false,
      requestsShutdown: false,
      runsReconnectClient: false
    },
    validation,
    plan: {
      ...common,
      plannedStages: [
        ROOM_TRANSFER_STAGE.OLD_FREEZE,
        ROOM_TRANSFER_STAGE.OLD_EXPORT,
        ROOM_TRANSFER_STAGE.NEW_IMPORT,
        ROOM_TRANSFER_STAGE.NEW_CONFIRM_OWNERSHIP,
        ROOM_TRANSFER_STAGE.PROXY_ROUTE_UPSERT,
        ROOM_TRANSFER_STAGE.OLD_RETIRE
      ],
      plannedCalls: [
        "old.freezeRoomForTransfer",
        "old.exportRoomTransfer",
        "new.importRoomTransfer",
        "new.confirmRoomOwnership",
        "proxy.getRoomRoute",
        "proxy.upsertRoomRoute",
        "old.retireTransferredRoom"
      ],
      endpoints: {
        oldGameServerAdmin: {
          endpoint: endpoint(options.oldAdminHost, options.oldAdminPort),
          tokenState: tokenState(options.oldAdminToken)
        },
        newGameServerAdmin: {
          endpoint: endpoint(options.newAdminHost, options.newAdminPort),
          tokenState: tokenState(options.newAdminToken)
        },
        gameProxyAdmin: {
          url: options.proxyAdminUrl,
          tokenState: tokenState(options.proxyAdminToken),
          actorState: actorState(options.proxyAdminActor),
          actor: options.proxyAdminActor || "<PROXY_ADMIN_ACTOR>"
        }
      },
      routeCas: {
        proxyExpectedRoomVersion: options.proxyExpectedRoomVersion ?? "auto-from-existing-route",
        proxyRoomVersion: options.proxyRoomVersion ?? "auto-next-version",
        proxyExpectedLastTransferChecksum: options.proxyExpectedLastTransferChecksum ?? "auto-from-existing-route"
      },
      routeMetadata: {
        requiredExistingRoute: options.requireExistingRouteMetadata === true,
        actionOnMissing: options.requireExistingRouteMetadata === true
          ? "fail_before_proxy_route_upsert"
          : "allow_first_route_create"
      },
      timeoutMs: options.timeoutMs
    }
  };
}

export function buildTransferCliExecutionEnvelope(options, result, safetyOverrides = {}) {
  const validation = validateTransferCliOptions(options);
  const ok = Boolean(result?.ok);
  const mode = options.triggerRedirectOnly ? "redirect-execute" : "transfer-execute";
  const summary = options.triggerRedirectOnly
    ? {
        ok,
        rolloutEpoch: options.rolloutEpoch,
        roomId: options.roomId,
        stage: result?.stage || (ok ? "complete" : "trigger_redirect"),
        errorCode: result?.errorCode || result?.code || "",
        deliveredCount: result?.deliveredCount,
        failedCount: result?.failedCount,
        onlineMemberCount: result?.onlineMemberCount
      }
    : {
        ok,
        rolloutEpoch: options.rolloutEpoch,
        roomId: options.roomId,
        oldServerId: options.oldServerId,
        newServerId: options.newServerId,
        stage: result?.stage || "",
        completedStages: result?.completedStages || [],
        errorCode: result?.errorCode || result?.code || "",
        checksum: result?.confirmed?.checksum || result?.imported?.checksum || result?.exported?.checksum || "",
        importedRoomVersion: result?.proxyRoute?.importedRoomVersion ?? result?.imported?.roomVersion,
        proxyRoomVersion: result?.proxyRoute?.roomVersion,
        routeMetadata: result?.routeMetadata
      };

  return {
    ok,
    mode,
    dryRun: false,
    safety: {
      startsServices: false,
      callsControlPlane: true,
      requestsShutdown: false,
      runsReconnectClient: false,
      ...safetyOverrides
    },
    validation,
    summary,
    result
  };
}

function errorResult(stage, error) {
  return {
    ok: false,
    stage,
    errorCode: error?.code || error?.errorCode || "ERROR",
    error: error?.message || String(error)
  };
}

function invalidOptionsResult(validation) {
  return {
    ok: false,
    stage: "validation",
    errorCode: "INVALID_OPTIONS",
    error: validation.errors.join("; ")
  };
}

export function buildTransferCliParseErrorEnvelope(error) {
  const message = error?.message || String(error);
  return {
    ok: false,
    mode: "argument-error",
    dryRun: false,
    safety: {
      startsServices: false,
      callsControlPlane: false,
      requestsShutdown: false,
      runsReconnectClient: false
    },
    validation: {
      ok: false,
      errors: [message],
      warnings: [],
      requiredOptions: []
    },
    summary: {
      ok: false,
      stage: "argument_parse",
      errorCode: "INVALID_OPTIONS",
      error: message
    },
    result: {
      ok: false,
      stage: "argument_parse",
      errorCode: "INVALID_OPTIONS",
      error: message
    }
  };
}

export function buildTransferCliFatalErrorEnvelope(error) {
  const message = error?.message || String(error);
  return {
    ok: false,
    mode: "fatal-error",
    dryRun: false,
    safety: {
      startsServices: false,
      callsControlPlane: false,
      requestsShutdown: false,
      runsReconnectClient: false
    },
    validation: {
      ok: false,
      errors: [message],
      warnings: [],
      requiredOptions: []
    },
    summary: {
      ok: false,
      stage: "fatal",
      errorCode: error?.code || error?.errorCode || "FATAL_ERROR",
      error: message
    },
    result: {
      ok: false,
      stage: "fatal",
      errorCode: error?.code || error?.errorCode || "FATAL_ERROR",
      error: message
    }
  };
}

async function main() {
  let options;
  try {
    options = parseArgs(process.argv.slice(2));
  } catch (error) {
    console.log(JSON.stringify(buildTransferCliParseErrorEnvelope(error), null, 2));
    process.exitCode = 1;
    return;
  }

  if (options.help) {
    printUsage();
    return;
  }

  if (options.dryRun) {
    const plan = buildTransferCliDryRunPlan(options);
    console.log(JSON.stringify(plan, null, 2));
    if (!plan.ok) {
      process.exitCode = 1;
    }
    return;
  }

  const validation = validateTransferCliOptions(options);
  if (!validation.ok) {
    const envelope = buildTransferCliExecutionEnvelope(
      options,
      invalidOptionsResult(validation),
      { callsControlPlane: false }
    );
    console.log(JSON.stringify(envelope, null, 2));
    process.exitCode = 1;
    return;
  }

  const oldServer = new GameServerTransferClient({
    host: options.oldAdminHost,
    port: options.oldAdminPort,
    token: options.oldAdminToken,
    timeoutMs: options.timeoutMs
  });

  if (options.triggerRedirectOnly) {
    let result;
    try {
      result = await oldServer.triggerServerRedirect({
        rolloutEpoch: options.rolloutEpoch,
        roomId: options.roomId,
        reason: options.redirectReason || "rollout_redirect",
        targetHost: options.redirectTargetHost,
        targetPort: options.redirectTargetPort,
        targetServerId: options.redirectTargetServerId || options.newServerId || "",
        transport: options.redirectTransport || "kcp",
        retryAfterMs: options.redirectRetryAfterMs || 0
      });
    } catch (error) {
      result = errorResult("trigger_redirect", error);
    }
    const envelope = buildTransferCliExecutionEnvelope(options, result);
    console.log(JSON.stringify(envelope, null, 2));
    if (!envelope.ok) {
      process.exitCode = 1;
    }
    return;
  }

  const newServer = new GameServerTransferClient({
    host: options.newAdminHost,
    port: options.newAdminPort,
    token: options.newAdminToken,
    timeoutMs: options.timeoutMs
  });
  const proxy = new ProxyAdminClient({
    baseUrl: options.proxyAdminUrl,
    token: options.proxyAdminToken,
    actor: options.proxyAdminActor,
    timeoutMs: options.timeoutMs
  });

  let result;
  try {
    result = await orchestrateRoomTransfer(
      {
        rolloutEpoch: options.rolloutEpoch,
        roomId: options.roomId,
        oldServerId: options.oldServerId,
        newServerId: options.newServerId,
        proxyExpectedRoomVersion: options.proxyExpectedRoomVersion,
        proxyRoomVersion: options.proxyRoomVersion,
        proxyExpectedLastTransferChecksum: options.proxyExpectedLastTransferChecksum,
        requireExistingRouteMetadata: options.requireExistingRouteMetadata === true
      },
      { oldServer, newServer, proxy }
    );
  } catch (error) {
    result = errorResult("transfer_execute", error);
  }

  const envelope = buildTransferCliExecutionEnvelope(options, result);
  console.log(JSON.stringify(envelope, null, 2));
  if (!envelope.ok) {
    process.exitCode = 1;
  }
}

function isMainModule() {
  if (!process.argv[1]) {
    return false;
  }
  return fileURLToPath(import.meta.url) === resolve(process.argv[1]);
}

if (isMainModule()) {
  main().catch((error) => {
    console.log(JSON.stringify(buildTransferCliFatalErrorEnvelope(error), null, 2));
    process.exitCode = 1;
  });
}
