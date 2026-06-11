import {
  GameServerTransferClient,
  ProxyAdminClient,
  orchestrateRoomTransfer
} from "./rollout-transfer.js";

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
  --proxy-expected-room-version <n>
  --proxy-room-version <n>
  --proxy-expected-last-transfer-checksum <checksum>
  --timeout-ms <ms>                       default: 5000
  -h, --help`);
}

function parseNumber(value, fallback) {
  if (value === "" || value === undefined || value === null) {
    return fallback;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) ? parsed : fallback;
}

function parseArgs(argv) {
  const options = {
    oldAdminHost: process.env.MYSERVER_OLD_GAME_ADMIN_HOST || "127.0.0.1",
    oldAdminPort: parseNumber(process.env.MYSERVER_OLD_GAME_ADMIN_PORT, 7500),
    oldAdminToken: process.env.MYSERVER_OLD_GAME_ADMIN_TOKEN || process.env.GAME_ADMIN_TOKEN || "",
    newAdminHost: process.env.MYSERVER_NEW_GAME_ADMIN_HOST || "127.0.0.1",
    newAdminPort: parseNumber(process.env.MYSERVER_NEW_GAME_ADMIN_PORT, 7501),
    newAdminToken: process.env.MYSERVER_NEW_GAME_ADMIN_TOKEN || process.env.GAME_ADMIN_TOKEN || "",
    proxyAdminUrl: process.env.MYSERVER_PROXY_ADMIN_URL || "http://127.0.0.1:7101",
    proxyAdminToken: process.env.PROXY_ADMIN_TOKEN || "",
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
      case "--proxy-expected-room-version":
        ({ value: options.proxyExpectedRoomVersion, nextIndex: index } = takeNumber(index, undefined));
        break;
      case "--proxy-room-version":
        ({ value: options.proxyRoomVersion, nextIndex: index } = takeNumber(index, undefined));
        break;
      case "--proxy-expected-last-transfer-checksum":
        ({ value: options.proxyExpectedLastTransferChecksum, nextIndex: index } = takeValue(index));
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

function requireOption(options, key) {
  if (!options[key]) {
    throw new Error(`missing required option --${key.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}`);
  }
}

function requirePort(options, key) {
  const value = options[key];
  if (!Number.isInteger(value) || value <= 0 || value > 65535) {
    throw new Error(`invalid option --${key.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}: expected 1-65535`);
  }
}

function requireNonNegativeInteger(options, key) {
  const value = options[key] ?? 0;
  if (!Number.isInteger(value) || value < 0) {
    throw new Error(`invalid option --${key.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}: expected non-negative integer`);
  }
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    printUsage();
    return;
  }

  const requiredKeys = options.triggerRedirectOnly
    ? ["rolloutEpoch", "roomId", "redirectTargetHost", "redirectTargetPort"]
    : ["rolloutEpoch", "roomId", "oldServerId", "newServerId"];
  for (const key of requiredKeys) {
    requireOption(options, key);
  }
  if (options.triggerRedirectOnly) {
    requirePort(options, "redirectTargetPort");
    requireNonNegativeInteger(options, "redirectRetryAfterMs");
  }

  const oldServer = new GameServerTransferClient({
    host: options.oldAdminHost,
    port: options.oldAdminPort,
    token: options.oldAdminToken,
    timeoutMs: options.timeoutMs
  });

  if (options.triggerRedirectOnly) {
    const result = await oldServer.triggerServerRedirect({
      rolloutEpoch: options.rolloutEpoch,
      roomId: options.roomId,
      reason: options.redirectReason || "rollout_redirect",
      targetHost: options.redirectTargetHost,
      targetPort: options.redirectTargetPort,
      targetServerId: options.redirectTargetServerId || options.newServerId || "",
      transport: options.redirectTransport || "kcp",
      retryAfterMs: options.redirectRetryAfterMs || 0
    });
    console.log(JSON.stringify(result, null, 2));
    if (!result.ok) {
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
    timeoutMs: options.timeoutMs
  });

  const result = await orchestrateRoomTransfer(
    {
      rolloutEpoch: options.rolloutEpoch,
      roomId: options.roomId,
      oldServerId: options.oldServerId,
      newServerId: options.newServerId,
      proxyExpectedRoomVersion: options.proxyExpectedRoomVersion,
      proxyRoomVersion: options.proxyRoomVersion,
      proxyExpectedLastTransferChecksum: options.proxyExpectedLastTransferChecksum
    },
    { oldServer, newServer, proxy }
  );

  console.log(JSON.stringify(result, null, 2));
  if (!result.ok) {
    process.exitCode = 1;
  }
}

main().catch((error) => {
  console.error(error.message);
  console.error("Run with --help for usage.");
  process.exitCode = 1;
});
