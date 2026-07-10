import crypto from "node:crypto";
import process from "node:process";
import { pathToFileURL } from "node:url";

const REDIS_URL_ENV = "MYSERVER_LOCKSTEP_REDIS_URL_RUNTIME";
const ENV_NAME_PATTERN = /^[A-Za-z_][A-Za-z0-9_]*$/;
const RUN_ID_PATTERN = /^[a-z0-9][a-z0-9-]{2,39}$/;
const TICKET_HASH_PATTERN = /^ticket:[0-9a-f]{64}$/;
const VERSION_KEY_PATTERN = /^player-ticket-version:plr_[0-9a-f]{16,64}$/;
const PLAYER_ID_PATTERN = /^[A-Za-z0-9_-]{1,128}$/;
const CHARACTER_ID_PATTERN = /^chr_[0-9a-hj-km-np-tv-z]+$/;
const REGISTRY_SERVICE_NAME = "game-server";

function requireString(value, name) {
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new Error(`${name} must be a non-empty string`);
  }
  return value.trim();
}

export function validateEnvName(value, name = "environment variable name") {
  const envName = requireString(value, name);
  if (!ENV_NAME_PATTERN.test(envName)) {
    throw new Error(`${name} has an invalid name`);
  }
  return envName;
}

export function assertLocalRedisUrl(value) {
  const raw = requireString(value, "Redis URL");
  let parsed;
  try {
    parsed = new URL(raw);
  } catch {
    throw new Error("Redis URL is invalid");
  }
  if (!new Set(["redis:", "rediss:"]).has(parsed.protocol)) {
    throw new Error("Redis URL must use redis:// or rediss://");
  }
  const host = parsed.hostname.toLowerCase();
  if (!new Set(["127.0.0.1", "localhost", "::1", "[::1]"]).has(host)) {
    throw new Error("lockstep Redis operations are restricted to loopback Redis");
  }
  return parsed;
}

function validateKeyPrefix(value) {
  if (typeof value !== "string") {
    throw new Error("keyPrefix must be a string");
  }
  if (/[*?\[\]\0\r\n]/u.test(value)) {
    throw new Error("keyPrefix contains wildcard or control characters");
  }
  return value;
}

function sha256(value) {
  return crypto.createHash("sha256").update(value).digest("hex");
}

function encodeBase64Url(value) {
  return Buffer.from(value).toString("base64url");
}

function createTicket(secret, identity, version, ttlSeconds, worldId) {
  const payload = {
    playerId: identity.playerId,
    characterId: identity.characterId,
    nonce: crypto.randomBytes(12).toString("hex"),
    ver: version,
    exp: new Date(Date.now() + ttlSeconds * 1000).toISOString(),
    worldId
  };
  const payloadB64 = encodeBase64Url(JSON.stringify(payload));
  const signature = crypto
    .createHmac("sha256", secret)
    .update(payloadB64)
    .digest("base64url");
  const ticket = `${payloadB64}.${signature}`;
  const hash = sha256(ticket);
  return {
    ticket,
    fingerprint: hash.slice(0, 12),
    hash,
    playerId: identity.playerId,
    characterId: identity.characterId,
    version,
    expiresAt: payload.exp
  };
}

export function parseTicketMetadata(ticket, keyPrefix = "") {
  const value = requireString(ticket, "ticket");
  const parts = value.split(".");
  if (parts.length !== 2 || !parts[0] || !parts[1]) {
    throw new Error("ticket has an invalid signed payload format");
  }

  let payload;
  try {
    payload = JSON.parse(Buffer.from(parts[0], "base64url").toString("utf8"));
  } catch {
    throw new Error("ticket payload is not valid base64url JSON");
  }

  const playerId = requireString(payload.playerId, "ticket playerId");
  const characterId = requireString(payload.characterId, "ticket characterId");
  if (!PLAYER_ID_PATTERN.test(playerId)) {
    throw new Error("ticket playerId has an invalid format");
  }
  if (!CHARACTER_ID_PATTERN.test(characterId)) {
    throw new Error("ticket characterId has an invalid format");
  }
  const version = Number(payload.ver ?? 1);
  if (!Number.isSafeInteger(version) || version < 1) {
    throw new Error("ticket version must be a positive integer");
  }
  const expiresAt = requireString(payload.exp, "ticket exp");
  const expiresAtMs = Date.parse(expiresAt);
  if (!Number.isFinite(expiresAtMs)) {
    throw new Error("ticket exp is invalid");
  }
  const hash = sha256(value);
  return {
    fingerprint: hash.slice(0, 12),
    playerId,
    characterId,
    version,
    expiresAt,
    expired: expiresAtMs <= Date.now(),
    ticketKey: `${keyPrefix}ticket:${hash}`,
    versionKey: `${keyPrefix}player-ticket-version:${playerId}`
  };
}

function newIdentity(runId, role) {
  const marker = sha256(`${runId}:${role}:${crypto.randomBytes(8).toString("hex")}`).slice(0, 20);
  return {
    playerId: `plr_${marker}`,
    characterId: `chr_${marker}`
  };
}

export function buildProvisionPlan(request, secret) {
  const runId = requireString(request.runId, "runId");
  if (!RUN_ID_PATTERN.test(runId)) {
    throw new Error("runId has an invalid format");
  }
  const keyPrefix = validateKeyPrefix(request.keyPrefix ?? "");
  const ttlSeconds = Number(request.ttlSeconds ?? 900);
  if (!Number.isSafeInteger(ttlSeconds) || ttlSeconds < 30 || ttlSeconds > 3600) {
    throw new Error("ttlSeconds must be an integer from 30 through 3600");
  }
  const worldId = Number(request.worldId ?? 1);
  if (!Number.isSafeInteger(worldId) || worldId < 0) {
    throw new Error("worldId must be a non-negative integer");
  }
  const signingSecret = requireString(secret, "ticket signing secret");
  const primary = createTicket(signingSecret, newIdentity(runId, "primary"), 1, ttlSeconds, worldId);
  const observer = createTicket(signingSecret, newIdentity(runId, "observer"), 1, ttlSeconds, worldId);
  const tickets = [primary, observer];
  const entries = tickets.flatMap((ticket, index) => {
    const role = index === 0 ? "primary" : "observer";
    return [
      {
        key: `${keyPrefix}ticket:${ticket.hash}`,
        expectedValue: ticket.playerId,
        kind: `${role}-ticket-owner`
      },
      {
        key: `${keyPrefix}player-ticket-version:${ticket.playerId}`,
        expectedValue: String(ticket.version),
        kind: `${role}-ticket-version`
      }
    ];
  });
  return { runId, keyPrefix, ttlSeconds, worldId, primary, observer, entries };
}

export function validateCleanupEntries(entries, keyPrefix = "") {
  const prefix = validateKeyPrefix(keyPrefix);
  if (!Array.isArray(entries) || entries.length === 0 || entries.length > 8) {
    throw new Error("cleanup entries must contain between 1 and 8 exact keys");
  }
  const seen = new Set();
  return entries.map((entry) => {
    const key = requireString(entry?.key, "cleanup key");
    const expectedValue = requireString(entry?.expectedValue, "cleanup expectedValue");
    if (!key.startsWith(prefix)) {
      throw new Error("cleanup key is outside the configured prefix");
    }
    const suffix = key.slice(prefix.length);
    if (!TICKET_HASH_PATTERN.test(suffix) && !VERSION_KEY_PATTERN.test(suffix)) {
      throw new Error("cleanup key is not an exact lockstep ticket key");
    }
    if (seen.has(key)) {
      throw new Error("cleanup entries contain a duplicate key");
    }
    seen.add(key);
    return { key, expectedValue, kind: String(entry.kind ?? "ticket-key") };
  });
}

export function buildRegistryCleanupPlan(request) {
  const runId = requireString(request.runId, "runId");
  if (!RUN_ID_PATTERN.test(runId)) {
    throw new Error("runId has an invalid format");
  }
  const keyPrefix = validateKeyPrefix(request.keyPrefix ?? "");
  const serviceName = requireString(request.serviceName, "serviceName");
  const instanceId = requireString(request.instanceId, "instanceId");
  const expectedInstanceId = `lockstep-${runId}`;
  if (serviceName !== REGISTRY_SERVICE_NAME) {
    throw new Error("registry cleanup is restricted to service game-server");
  }
  if (instanceId !== expectedInstanceId) {
    throw new Error("registry cleanup instanceId does not match this run");
  }
  return {
    runId,
    keyPrefix,
    serviceName,
    instanceId,
    instanceKey: `${keyPrefix}service:${serviceName}:instances:${instanceId}`,
    heartbeatKey: `${keyPrefix}heartbeat:${serviceName}:${instanceId}`,
    expectedInstanceHash: { id: instanceId, name: serviceName },
    expectedHeartbeatValue: "1"
  };
}

async function readStdinJson() {
  const chunks = [];
  for await (const chunk of process.stdin) {
    chunks.push(chunk);
  }
  const raw = Buffer.concat(chunks).toString("utf8").trim();
  if (!raw) {
    throw new Error("stdin request JSON is required");
  }
  try {
    return JSON.parse(raw);
  } catch {
    throw new Error("stdin request is not valid JSON");
  }
}

async function openRedis() {
  const redisUrl = process.env[REDIS_URL_ENV];
  assertLocalRedisUrl(redisUrl);
  const { default: Redis } = await import("ioredis");
  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    connectTimeout: 5_000,
    commandTimeout: 5_000,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });
  redis.on("error", () => {});
  await redis.connect();
  return redis;
}

async function provision(request) {
  const secretEnvVar = validateEnvName(request.secretEnvVar, "secretEnvVar");
  const plan = buildProvisionPlan(request, process.env[secretEnvVar]);
  const entries = validateCleanupEntries(plan.entries, plan.keyPrefix);
  const redis = await openRedis();
  try {
    const script = `
      for index = 1, #KEYS do
        if redis.call('EXISTS', KEYS[index]) == 1 then
          return {0, index}
        end
      end
      local ttl = ARGV[#KEYS + 1]
      for index = 1, #KEYS do
        redis.call('SET', KEYS[index], ARGV[index], 'EX', ttl)
      end
      return {1, #KEYS}
    `;
    const result = await redis.eval(
      script,
      entries.length,
      ...entries.map((entry) => entry.key),
      ...entries.map((entry) => entry.expectedValue),
      String(plan.ttlSeconds)
    );
    if (!Array.isArray(result) || Number(result[0]) !== 1) {
      throw new Error("one or more exact ticket keys already exist; no keys were written");
    }
    return {
      ok: true,
      action: "provision",
      primary: plan.primary,
      observer: plan.observer,
      entries,
      ttlSeconds: plan.ttlSeconds
    };
  } finally {
    redis.disconnect();
  }
}

function ticketFromEnv(request, field) {
  const envName = validateEnvName(request[field], field);
  return { envName, ticket: requireString(process.env[envName], `${field} value`) };
}

function inspectTickets(request) {
  const keyPrefix = validateKeyPrefix(request.keyPrefix ?? "");
  const primaryInput = ticketFromEnv(request, "ticketEnvVar");
  const observerInput = request.observerTicketEnvVar
    ? ticketFromEnv(request, "observerTicketEnvVar")
    : null;
  const primary = parseTicketMetadata(primaryInput.ticket, keyPrefix);
  const observer = observerInput ? parseTicketMetadata(observerInput.ticket, keyPrefix) : null;
  if (primary.expired || observer?.expired) {
    throw new Error("one or more supplied tickets are expired");
  }
  const publicMetadata = (ticket) => ({
    fingerprint: ticket.fingerprint,
    playerId: ticket.playerId,
    characterId: ticket.characterId,
    version: ticket.version,
    expiresAt: ticket.expiresAt,
    ticketKey: ticket.ticketKey,
    versionKey: ticket.versionKey
  });
  return {
    primary: publicMetadata(primary),
    observer: observer ? publicMetadata(observer) : null
  };
}

async function validateBindings(request) {
  const keyPrefix = validateKeyPrefix(request.keyPrefix ?? "");
  const inspected = inspectTickets(request);
  const metadata = [inspected.primary, ...(inspected.observer ? [inspected.observer] : [])];
  const entries = metadata.flatMap((ticket, index) => {
    const role = index === 0 ? "primary" : "observer";
    return [
      { key: ticket.ticketKey, expectedValue: ticket.playerId, kind: `${role}-ticket-owner` },
      { key: ticket.versionKey, expectedValue: String(ticket.version), kind: `${role}-ticket-version` }
    ];
  });
  const redis = await openRedis();
  try {
    const actual = await redis.mget(entries.map((entry) => entry.key));
    const mismatches = entries
      .map((entry, index) => ({ ...entry, actual: actual[index] }))
      .filter((entry) => entry.actual !== entry.expectedValue)
      .map((entry) => ({ key: entry.key, kind: entry.kind, present: entry.actual !== null }));
    if (mismatches.length > 0) {
      const error = new Error("ticket Redis owner/version binding validation failed");
      error.details = mismatches;
      throw error;
    }
    return {
      ok: true,
      action: "validate-bindings",
      primary: inspected.primary,
      observer: inspected.observer,
      signatureVerified: false,
      redisBindingsVerified: true
    };
  } finally {
    redis.disconnect();
  }
}

async function cleanup(request) {
  const keyPrefix = validateKeyPrefix(request.keyPrefix ?? "");
  const entries = validateCleanupEntries(request.entries, keyPrefix);
  const redis = await openRedis();
  const script = `
    local actual = redis.call('GET', KEYS[1])
    if not actual then return 0 end
    if actual ~= ARGV[1] then return -1 end
    return redis.call('DEL', KEYS[1])
  `;
  try {
    const results = [];
    for (const entry of entries) {
      const code = Number(await redis.eval(script, 1, entry.key, entry.expectedValue));
      results.push({
        key: entry.key,
        kind: entry.kind,
        result: code === 1 ? "deleted" : code === 0 ? "already-expired" : "value-mismatch-not-deleted"
      });
    }
    return {
      ok: results.every((entry) => entry.result !== "value-mismatch-not-deleted"),
      action: "cleanup",
      results
    };
  } finally {
    redis.disconnect();
  }
}

async function cleanupRegistry(request) {
  const plan = buildRegistryCleanupPlan(request);
  const redis = await openRedis();
  const script = `
    local instance_exists = redis.call('EXISTS', KEYS[1])
    local heartbeat_exists = redis.call('EXISTS', KEYS[2])

    if instance_exists == 1 then
      local instance_type = redis.call('TYPE', KEYS[1])['ok']
      if instance_type ~= 'hash' then return {0, 1} end
      local raw = redis.call('HGET', KEYS[1], 'data')
      if not raw then return {0, 2} end
      local decoded_ok, data = pcall(cjson.decode, raw)
      if not decoded_ok then return {0, 3} end
      if data['id'] ~= ARGV[1] or data['name'] ~= ARGV[2] then return {0, 4} end
    end

    if heartbeat_exists == 1 then
      local heartbeat_type = redis.call('TYPE', KEYS[2])['ok']
      if heartbeat_type ~= 'string' then return {0, 5} end
      if redis.call('GET', KEYS[2]) ~= ARGV[3] then return {0, 6} end
    end

    if instance_exists == 1 then redis.call('DEL', KEYS[1]) end
    if heartbeat_exists == 1 then redis.call('DEL', KEYS[2]) end
    return {1, instance_exists, heartbeat_exists}
  `;
  try {
    const result = await redis.eval(
      script,
      2,
      plan.instanceKey,
      plan.heartbeatKey,
      plan.instanceId,
      plan.serviceName,
      plan.expectedHeartbeatValue
    );
    if (!Array.isArray(result) || Number(result[0]) !== 1) {
      const guardCode = Array.isArray(result) ? Number(result[1]) : 0;
      return {
        ok: false,
        action: "cleanup-registry",
        guardCode,
        results: [
          {
            key: plan.instanceKey,
            kind: "registry-instance",
            result: "ownership-mismatch-not-deleted"
          },
          {
            key: plan.heartbeatKey,
            kind: "registry-heartbeat",
            result: "ownership-mismatch-not-deleted"
          }
        ]
      };
    }
    const instanceExisted = Number(result[1]) === 1;
    const heartbeatExisted = Number(result[2]) === 1;
    return {
      ok: true,
      action: "cleanup-registry",
      guardCode: null,
      results: [
        {
          key: plan.instanceKey,
          kind: "registry-instance",
          result: instanceExisted ? "deleted" : "already-absent"
        },
        {
          key: plan.heartbeatKey,
          kind: "registry-heartbeat",
          result: heartbeatExisted ? "deleted" : "already-absent"
        }
      ]
    };
  } finally {
    redis.disconnect();
  }
}

function sanitizeError(error) {
  let message = error instanceof Error ? error.message : String(error);
  const sensitiveValues = [
    process.env[REDIS_URL_ENV],
    ...Object.entries(process.env)
      .filter(([name]) => name.includes("LOCKSTEP") && (name.includes("TICKET") || name.includes("SECRET")))
      .map(([, value]) => value)
  ].filter((value) => typeof value === "string" && value.length > 0);
  for (const value of sensitiveValues) {
    message = message.split(value).join("<redacted>");
  }
  return {
    ok: false,
    error: message,
    details: Array.isArray(error?.details) ? error.details : []
  };
}

async function main() {
  const request = await readStdinJson();
  switch (request.action) {
    case "provision":
      return provision(request);
    case "validate-bindings":
      return validateBindings(request);
    case "inspect": {
      const inspected = inspectTickets(request);
      return {
        ok: true,
        action: "inspect",
        ...inspected,
        signatureVerified: false,
        redisBindingsVerified: false
      };
    }
    case "cleanup":
      return cleanup(request);
    case "cleanup-registry":
      return cleanupRegistry(request);
    default:
      throw new Error(
        "action must be provision, validate-bindings, inspect, cleanup, or cleanup-registry"
      );
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const result = await main();
    process.stdout.write(`${JSON.stringify(result)}\n`);
  } catch (error) {
    process.stderr.write(`${JSON.stringify(sanitizeError(error))}\n`);
    process.exitCode = 1;
  }
}
