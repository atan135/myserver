import {
  ROLLOUT_FAULT_DRILL_DEFINITIONS,
  runRolloutFaultDrills
} from "./rollout-fault-drill.js";

function optionValue(argv, index) {
  return { value: argv[index + 1] || "", nextIndex: index + 1 };
}

function printUsage() {
  const drills = ROLLOUT_FAULT_DRILL_DEFINITIONS.map((definition) => `  - ${definition.name}: ${definition.title}`).join("\n");
  console.log(`Usage:
  node tools/mock-client/src/rollout-fault-drill-cli.js [options]

Default mode is dry-run. It prints a JSON plan and does not call services.

Modes:
  --dry-run                              print planned fault drills only (default)
  --simulate                             run pure in-memory mock validation, no services
  --execute                              call existing control-plane endpoints

Drills:
${drills}

Options:
  --drill <name>                         repeatable; default: all
  --all                                  run all drills
  --rollout-epoch <epoch>
  --room-id <room-id>
  --old-server-id <server-id>            default: game-server-old
  --new-server-id <server-id>            default: game-server-new
  --old-admin-host <host>                default: MYSERVER_OLD_GAME_ADMIN_HOST or 127.0.0.1
  --old-admin-port <port>                default: MYSERVER_OLD_GAME_ADMIN_PORT or 7500
  --old-admin-token <token>              default: MYSERVER_OLD_GAME_ADMIN_TOKEN or GAME_ADMIN_TOKEN
  --new-admin-host <host>                default: MYSERVER_NEW_GAME_ADMIN_HOST or 127.0.0.1
  --new-admin-port <port>                default: MYSERVER_NEW_GAME_ADMIN_PORT or 7501
  --new-admin-token <token>              default: MYSERVER_NEW_GAME_ADMIN_TOKEN or GAME_ADMIN_TOKEN
  --proxy-admin-url <url>                default: MYSERVER_PROXY_ADMIN_URL or http://127.0.0.1:7101
  --proxy-admin-token <token>            default: PROXY_ADMIN_TOKEN
  --proxy-admin-actor <actor>            default: MYSERVER_PROXY_ADMIN_ACTOR or rollout-fault-drill
  --redirect-target-host <host>          required with --execute redirect-no-reconnect
  --redirect-target-port <port>          required with --execute redirect-no-reconnect
  --redirect-target-server-id <id>       default: --new-server-id
  --redirect-transport <transport>       default: kcp
  --redirect-reason <reason>             default: rollout_fault_drill_redirect_no_reconnect
  --redirect-retry-after-ms <ms>         default: 0
  --timeout-ms <ms>                      default: 5000
  --archive-dir <dir>                    write JSON report as rollout-fault-drill-<time>.json
  --archive-file <file>                  write JSON report to a specific file
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
    drills: [],
    oldAdminHost: process.env.MYSERVER_OLD_GAME_ADMIN_HOST || "127.0.0.1",
    oldAdminPort: parseNumber(process.env.MYSERVER_OLD_GAME_ADMIN_PORT, 7500),
    oldAdminToken: process.env.MYSERVER_OLD_GAME_ADMIN_TOKEN || process.env.GAME_ADMIN_TOKEN || "",
    newAdminHost: process.env.MYSERVER_NEW_GAME_ADMIN_HOST || "127.0.0.1",
    newAdminPort: parseNumber(process.env.MYSERVER_NEW_GAME_ADMIN_PORT, 7501),
    newAdminToken: process.env.MYSERVER_NEW_GAME_ADMIN_TOKEN || process.env.GAME_ADMIN_TOKEN || "",
    proxyAdminUrl: process.env.MYSERVER_PROXY_ADMIN_URL || "http://127.0.0.1:7101",
    proxyAdminToken: process.env.PROXY_ADMIN_TOKEN || "",
    proxyAdminActor: process.env.MYSERVER_PROXY_ADMIN_ACTOR || "rollout-fault-drill",
    oldServerId: process.env.MYSERVER_OLD_SERVER_ID || "game-server-old",
    newServerId: process.env.MYSERVER_NEW_SERVER_ID || "game-server-new",
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
        options.execute = false;
        options.simulate = false;
        break;
      case "--simulate":
        options.simulate = true;
        options.execute = false;
        break;
      case "--execute":
        options.execute = true;
        options.simulate = false;
        break;
      case "--drill":
        {
          const parsed = takeValue(index);
          options.drills.push(parsed.value);
          index = parsed.nextIndex;
        }
        break;
      case "--all":
        options.drills = ["all"];
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
      case "--timeout-ms":
        ({ value: options.timeoutMs, nextIndex: index } = takeNumber(index, options.timeoutMs));
        break;
      case "--archive-dir":
        ({ value: options.archiveDir, nextIndex: index } = takeValue(index));
        break;
      case "--archive-file":
        ({ value: options.archiveFile, nextIndex: index } = takeValue(index));
        break;
      default:
        throw new Error(`unknown option ${arg}`);
    }
  }

  return options;
}

function selectedDrillNames(options) {
  if (!options.drills || options.drills.length === 0 || options.drills.includes("all")) {
    return ROLLOUT_FAULT_DRILL_DEFINITIONS.map((definition) => definition.name);
  }
  return options.drills;
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

function requireExecuteOptions(options) {
  if (!options.execute) {
    return;
  }

  const names = selectedDrillNames(options);
  requireOption(options, "rolloutEpoch");
  requireOption(options, "roomId");

  if (names.some((name) => name !== "redirect-no-reconnect")) {
    requireOption(options, "oldServerId");
    requireOption(options, "newServerId");
    requirePort(options, "oldAdminPort");
    requirePort(options, "newAdminPort");
  }

  if (names.includes("redirect-no-reconnect")) {
    requireOption(options, "redirectTargetHost");
    requireOption(options, "redirectTargetPort");
    requirePort(options, "redirectTargetPort");
  }
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    printUsage();
    return;
  }

  requireExecuteOptions(options);
  const report = await runRolloutFaultDrills(options);
  console.log(JSON.stringify(report, null, 2));
  if (!report.ok) {
    process.exitCode = 1;
  }
}

main().catch((error) => {
  console.error(error.message);
  console.error("Run with --help for usage.");
  process.exitCode = 1;
});
