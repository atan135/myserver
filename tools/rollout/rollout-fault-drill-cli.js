import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { ROLLOUT_FAULT_DRILL_DEFINITIONS, runRolloutFaultDrills } from "./rollout-fault-drill.js";

function optionValue(argv, index) {
  const value = argv[index + 1] || "";
  if (!value || value.startsWith("--")) throw new Error(`missing value for ${argv[index]}`);
  return { value, next: index + 1 };
}

function parseNumber(value, fallback) {
  if (!/^[0-9]+$/.test(String(value))) return fallback;
  return Number.parseInt(value, 10);
}

function printUsage() {
  const drills = ROLLOUT_FAULT_DRILL_DEFINITIONS.map((item) => `  - ${item.name}: ${item.title}`).join("\n");
  console.log(`Usage:
  node tools/rollout/rollout-fault-drill-cli.js [options]

Default mode is a dry-run. --simulate runs only in-memory clients. Direct service execution is disabled;
Room Transfer mutations must use rollout-control-plane-cli.js and the admin-api high-risk protocol.

Drills:
${drills}

Options:
  --dry-run
  --simulate
  --execute                              rejected: CONTROL_PLANE_ONLY
  --drill <name>                         repeatable; default: all
  --all
  --rollout-epoch <epoch>
  --room-id <room-id>
  --old-server-id <server-id>
  --new-server-id <server-id>
  --redirect-target-host <host>
  --redirect-target-port <port>
  --redirect-target-server-id <id>
  --redirect-transport <transport>
  --redirect-reason <reason>
  --redirect-retry-after-ms <ms>
  --archive-dir <dir>
  --archive-file <file>
  -h, --help`);
}

export function parseFaultDrillArgs(argv) {
  const options = { drills: [], simulate: false, execute: false };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "-h" || arg === "--help") options.help = true;
    else if (arg === "--dry-run") options.simulate = false;
    else if (arg === "--simulate") options.simulate = true;
    else if (arg === "--execute") options.execute = true;
    else if (arg === "--all") options.drills = ["all"];
    else if (arg === "--drill") {
      const parsed = optionValue(argv, index);
      options.drills.push(parsed.value);
      index = parsed.next;
    } else {
      const key = {
        "--rollout-epoch": "rolloutEpoch",
        "--room-id": "roomId",
        "--old-server-id": "oldServerId",
        "--new-server-id": "newServerId",
        "--redirect-target-host": "redirectTargetHost",
        "--redirect-target-server-id": "redirectTargetServerId",
        "--redirect-transport": "redirectTransport",
        "--redirect-reason": "redirectReason",
        "--archive-dir": "archiveDir",
        "--archive-file": "archiveFile"
      }[arg];
      if (key) {
        const parsed = optionValue(argv, index);
        options[key] = parsed.value;
        index = parsed.next;
      } else if (arg === "--redirect-target-port" || arg === "--redirect-retry-after-ms") {
        const parsed = optionValue(argv, index);
        options[arg === "--redirect-target-port" ? "redirectTargetPort" : "redirectRetryAfterMs"] = parseNumber(parsed.value, 0);
        index = parsed.next;
      } else {
        throw new Error(`unknown option ${arg}`);
      }
    }
  }
  return options;
}

export async function main() {
  const options = parseFaultDrillArgs(process.argv.slice(2));
  if (options.help) return printUsage();
  if (options.execute) throw Object.assign(new Error("Fault drill execution must use an admin-api control-plane operation"), {
    code: "CONTROL_PLANE_ONLY"
  });
  const report = await runRolloutFaultDrills(options);
  console.log(JSON.stringify(report, null, 2));
  if (!report.ok) process.exitCode = 1;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolve(process.argv[1])) {
  main().catch((error) => {
    console.error(`${error.code || "ERROR"}: ${error.message}`);
    process.exitCode = 1;
  });
}
