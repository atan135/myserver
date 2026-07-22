import { randomUUID } from "node:crypto";

const IDENTIFIER = /^[A-Za-z0-9][A-Za-z0-9._:@-]{0,127}$/;

function takeValue(argv, index) {
  const value = argv[index + 1] || "";
  if (!value || value.startsWith("--")) throw new Error(`missing value for ${argv[index]}`);
  return { value, next: index + 1 };
}

export function parseControlPlaneArgs(argv) {
  const options = {
    adminApiUrl: process.env.ADMIN_API_URL || "http://127.0.0.1:3001",
    adminApiToken: process.env.ADMIN_API_TOKEN || "",
    requestId: `room-transfer-${randomUUID()}`,
    execute: false,
    dryRun: false
  };
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--execute") options.execute = true;
    else if (arg === "--dry-run") options.dryRun = true;
    else if (arg === "--help" || arg === "-h") options.help = true;
    else {
      const key = {
        "--admin-api-url": "adminApiUrl",
        "--admin-api-token": "adminApiToken",
        "--world-id": "worldId",
        "--rollout-epoch": "rolloutEpoch",
        "--room-id": "roomId",
        "--old-server-id": "oldServerId",
        "--new-server-id": "newServerId",
        "--proxy-instance-id": "proxyInstanceId",
        "--backup-reference": "backupReference",
        "--request-id": "requestId",
        "--reason": "reason"
      }[arg];
      if (!key) throw new Error(`unknown option ${arg}`);
      const parsed = takeValue(argv, index);
      options[key] = parsed.value;
      index = parsed.next;
    }
  }
  return options;
}

export function validateControlPlaneOptions(options) {
  const errors = [];
  for (const key of ["worldId", "rolloutEpoch", "roomId", "oldServerId", "newServerId", "proxyInstanceId", "backupReference", "requestId", "reason"]) {
    const value = String(options[key] || "").trim();
    if (!value || (key !== "reason" && !IDENTIFIER.test(value))) errors.push(`invalid or missing --${key.replace(/[A-Z]/g, (part) => `-${part.toLowerCase()}`)}`);
  }
  if (options.oldServerId && options.oldServerId === options.newServerId) errors.push("--old-server-id and --new-server-id must differ");
  if (!options.dryRun && !String(options.adminApiToken || "").trim()) errors.push("missing --admin-api-token or ADMIN_API_TOKEN");
  try {
    const url = new URL(options.adminApiUrl);
    if (!/^https?:$/.test(url.protocol)) errors.push("--admin-api-url must use http or https");
  } catch {
    errors.push("--admin-api-url must be a URL");
  }
  return { ok: errors.length === 0, errors };
}

function requestBody(options) {
  return {
    worldId: options.worldId,
    rolloutEpoch: options.rolloutEpoch,
    roomId: options.roomId,
    oldServerId: options.oldServerId,
    newServerId: options.newServerId,
    proxyInstanceId: options.proxyInstanceId,
    backupReference: options.backupReference,
    requestId: options.requestId,
    reason: options.reason
  };
}

async function post(options, body) {
  const response = await fetch(`${String(options.adminApiUrl).replace(/\/+$/, "")}/api/v1/rollouts/room-transfer`, {
    method: "POST",
    headers: { authorization: `Bearer ${options.adminApiToken}`, "content-type": "application/json" },
    body: JSON.stringify(body)
  });
  const data = await response.json().catch(() => ({ ok: false, error: "INVALID_CONTROL_PLANE_RESPONSE" }));
  if (!response.ok) {
    const error = new Error(data?.error || `admin-api HTTP ${response.status}`);
    error.code = data?.error || "CONTROL_PLANE_REQUEST_FAILED";
    throw error;
  }
  return data;
}

export async function runControlPlaneRoomTransfer(options) {
  const validation = validateControlPlaneOptions(options);
  if (!validation.ok) return { ok: false, stage: "validation", validation };
  const body = requestBody(options);
  const preflight = await post(options, body);
  if (!options.execute) return { ok: true, stage: "preflight", preflight };
  if (!preflight?.preflight?.nonce || !preflight?.preflight?.summarySha256) {
    return { ok: false, stage: "preflight", errorCode: "ADMIN_OPERATION_PREFLIGHT_INVALID", preflight };
  }
  const result = await post(options, {
    ...body,
    preflightNonce: preflight.preflight.nonce,
    preflightSummarySha256: preflight.preflight.summarySha256
  });
  return { ok: result?.ok === true, stage: "execute", preflight, result };
}

function usage() {
  console.log("Usage: node tools/rollout/rollout-control-plane-cli.js --world-id <id> --rollout-epoch <id> --room-id <id> --old-server-id <id> --new-server-id <id> --proxy-instance-id <id> --backup-reference <id> --reason <text> [--execute]");
}

export async function main() {
  const options = parseControlPlaneArgs(process.argv.slice(2));
  if (options.help) return usage();
  if (options.dryRun) {
    console.log(JSON.stringify({ ok: validateControlPlaneOptions({ ...options, dryRun: true }).ok, dryRun: true, request: requestBody(options) }, null, 2));
    return;
  }
  const result = await runControlPlaneRoomTransfer(options);
  console.log(JSON.stringify(result, null, 2));
  if (!result.ok) process.exitCode = 1;
}

if (process.argv[1] && new URL(import.meta.url).pathname === new URL(`file://${process.argv[1].replaceAll("\\", "/")}`).pathname) {
  main().catch((error) => {
    console.log(JSON.stringify({ ok: false, stage: "control_plane", errorCode: error.code || "CONTROL_PLANE_REQUEST_FAILED", error: error.message }, null, 2));
    process.exitCode = 1;
  });
}
