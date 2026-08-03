import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const IDENTIFIER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const APPLY_CONFIRMATION = "legacy-metrics-unlink";
const DEFAULT_SCAN_COUNT = 500;
const DEFAULT_BATCH_SIZE = 100;
const DEFAULT_DELAY_MS = 100;
const UNLINK_HASHES_LUA = `
local deleted = 0
local wrong_type = 0
for _, key in ipairs(KEYS) do
  if redis.call("TYPE", key).ok == "hash" then
    deleted = deleted + redis.call("UNLINK", key)
  else
    wrong_type = wrong_type + 1
  end
end
return { deleted, wrong_type }
`;

export function classifyMetricsKey(key, keyPrefix = "") {
  const text = String(key ?? "");
  const metricsPrefix = `${keyPrefix}metrics:`;
  if (!text.startsWith(metricsPrefix)) {
    return { kind: "not_metrics" };
  }

  const body = text.slice(metricsPrefix.length);
  if (body.startsWith("v2:")) {
    return { kind: "v2" };
  }
  if (body.startsWith("heartbeat:")) {
    return { kind: "heartbeat" };
  }

  const parts = body.split(":");
  if (parts.length !== 2 && parts.length !== 3) {
    return { kind: "invalid_layout" };
  }

  const [service, instanceOrBucket, possibleBucket] = parts;
  const instanceId = parts.length === 3 ? instanceOrBucket : null;
  const bucketText = parts.length === 3 ? possibleBucket : instanceOrBucket;
  if (!IDENTIFIER_PATTERN.test(service) || (instanceId !== null && !IDENTIFIER_PATTERN.test(instanceId))) {
    return { kind: "invalid_identifier" };
  }
  if (!/^[0-9]{1,16}$/.test(bucketText)) {
    return { kind: "invalid_bucket" };
  }

  const bucket = Number(bucketText);
  if (!Number.isSafeInteger(bucket) || bucket <= 0 || bucket % 5 !== 0) {
    return { kind: "invalid_bucket" };
  }

  return {
    kind: "legacy",
    service,
    instanceId,
    bucket,
    layout: instanceId === null ? "service-bucket" : "service-instance-bucket"
  };
}

export async function cleanupLegacyMetrics(options = {}) {
  const config = normalizeOptions(options);
  const redis = options.redis ?? await createRedisClient(config.redisUrl);
  const ownsRedis = !options.redis;
  const checkpoint = readCheckpoint(config);
  const startedAt = checkpoint.startedAt ?? new Date().toISOString();
  const report = {
    ok: false,
    mode: config.apply ? "apply" : "dry-run",
    startedAt,
    completedAt: null,
    redisUrl: redactRedisUrl(config.redisUrl),
    keyPrefix: config.keyPrefix,
    operator: config.operator,
    scanPattern: `${config.keyPrefix}metrics:*`,
    cursor: checkpoint.cursor,
    completed: false,
    scanned: checkpoint.scanned ?? 0,
    legacyCandidates: checkpoint.legacyCandidates ?? 0,
    eligibleHashes: checkpoint.eligibleHashes ?? 0,
    deleted: checkpoint.deleted ?? 0,
    remainingEligibleHashes: checkpoint.remainingEligibleHashes ?? null,
    batches: checkpoint.batches ?? 0,
    excluded: {
      v2: 0,
      heartbeat: 0,
      invalid_layout: 0,
      invalid_identifier: 0,
      invalid_bucket: 0,
      wrong_type: 0,
      not_metrics: 0,
      ...(checkpoint.excluded ?? {})
    },
    services: { ...(checkpoint.services ?? {}) }
  };

  try {
    do {
      const [nextCursor, keys] = await redis.scan(
        report.cursor,
        "MATCH",
        report.scanPattern,
        "COUNT",
        config.scanCount
      );
      report.cursor = String(nextCursor);
      report.batches += 1;
      report.scanned += keys.length;

      const candidates = [];
      for (const key of keys) {
        const classification = classifyMetricsKey(key, config.keyPrefix);
        if (classification.kind !== "legacy") {
          report.excluded[classification.kind] += 1;
          continue;
        }
        report.legacyCandidates += 1;
        candidates.push({ key: String(key), ...classification });
      }

      const eligible = await filterHashKeys(redis, candidates, report);
      report.eligibleHashes += eligible.length;
      for (const candidate of eligible) {
        report.services[candidate.service] = (report.services[candidate.service] ?? 0) + 1;
      }

      if (config.apply) {
        for (let index = 0; index < eligible.length; index += config.batchSize) {
          const batch = eligible.slice(index, index + config.batchSize);
          if (batch.length === 0) continue;
          const [deleted, wrongType] = await unlinkHashKeys(
            redis,
            batch.map((candidate) => candidate.key)
          );
          report.deleted += deleted;
          report.excluded.wrong_type += wrongType;
          if (config.delayMs > 0) {
            await delay(config.delayMs);
          }
        }
      }

      writeCheckpoint(config, report);
    } while (report.cursor !== "0");

    report.remainingEligibleHashes = config.apply
      ? await countEligibleHashKeys(redis, config)
      : report.eligibleHashes;
    if (config.apply && report.remainingEligibleHashes !== 0) {
      throw new Error(`${report.remainingEligibleHashes} eligible legacy hashes remain after the apply pass; resume from the checkpoint`);
    }

    report.ok = true;
    report.completed = true;
    report.completedAt = new Date().toISOString();
    report.services = sortedObject(report.services);
    writeCheckpoint(config, report);
    appendAudit(config, report);
    return report;
  } catch (error) {
    report.completedAt = new Date().toISOString();
    report.error = error?.message || String(error);
    report.services = sortedObject(report.services);
    writeCheckpoint(config, report);
    appendAudit(config, report);
    return report;
  } finally {
    if (ownsRedis) {
      closeRedis(redis);
    }
  }
}

async function unlinkHashKeys(redis, keys) {
  if (keys.length === 0) return [0, 0];
  const result = await redis.eval(UNLINK_HASHES_LUA, keys.length, ...keys);
  return [Number(result?.[0]) || 0, Number(result?.[1]) || 0];
}

async function countEligibleHashKeys(redis, config) {
  let cursor = "0";
  let count = 0;
  do {
    const [nextCursor, keys] = await redis.scan(
      cursor,
      "MATCH",
      `${config.keyPrefix}metrics:*`,
      "COUNT",
      config.scanCount
    );
    cursor = String(nextCursor);
    const candidates = keys.filter((key) => classifyMetricsKey(key, config.keyPrefix).kind === "legacy");
    if (candidates.length === 0) continue;
    const pipeline = redis.pipeline();
    for (const key of candidates) pipeline.type(key);
    const results = await pipeline.exec();
    count += results.filter(([error, type]) => {
      if (error) throw error;
      return type === "hash";
    }).length;
  } while (cursor !== "0");
  return count;
}

async function filterHashKeys(redis, candidates, report) {
  if (candidates.length === 0) return [];
  const pipeline = redis.pipeline();
  for (const candidate of candidates) {
    pipeline.type(candidate.key);
  }
  const results = await pipeline.exec();
  const eligible = [];
  for (let index = 0; index < candidates.length; index += 1) {
    const [error, type] = results[index] ?? [];
    if (error) throw error;
    if (type !== "hash") {
      report.excluded.wrong_type += 1;
      continue;
    }
    eligible.push(candidates[index]);
  }
  return eligible;
}

function normalizeOptions(options) {
  const keyPrefixProvided = Object.hasOwn(options, "keyPrefix");
  if (!keyPrefixProvided) {
    throw new Error("keyPrefix must be provided explicitly");
  }
  const keyPrefix = String(options.keyPrefix ?? "");
  if (/[\s\u0000-\u001f\u007f*?\[\]\\]/.test(keyPrefix)) {
    throw new Error("keyPrefix must not contain whitespace, control characters or Redis glob syntax");
  }
  if (keyPrefix === "" && options.allowEmptyPrefix !== true) {
    throw new Error("allowEmptyPrefix must be true when keyPrefix is empty");
  }

  const redisUrlEnv = String(options.redisUrlEnv ?? "").trim();
  if (options.redisUrl && redisUrlEnv) {
    throw new Error("provide either redisUrl or redisUrlEnv, not both");
  }
  if (redisUrlEnv && !/^[A-Z][A-Z0-9_]{0,63}$/.test(redisUrlEnv)) {
    throw new Error("redisUrlEnv must be an uppercase environment variable name");
  }
  const redisUrl = String(options.redisUrl ?? (redisUrlEnv ? process.env[redisUrlEnv] : "") ?? "");
  if (!redisUrl) {
    throw new Error("redisUrl must be provided explicitly or through redisUrlEnv");
  }
  validateRedisUrl(redisUrl);

  const apply = options.apply === true;
  if (apply && options.confirm !== APPLY_CONFIRMATION) {
    throw new Error(`apply requires confirm=${APPLY_CONFIRMATION}`);
  }
  const operator = String(options.operator ?? "").trim();
  if (apply && (!operator || operator.length > 128 || /[\u0000-\u001f\u007f]/.test(operator))) {
    throw new Error("apply requires a valid operator identity");
  }

  const checkpointPath = normalizeOptionalPath(options.checkpointPath);
  const auditLogPath = normalizeOptionalPath(options.auditLogPath);
  if (apply && (!checkpointPath || !auditLogPath)) {
    throw new Error("apply requires checkpointPath and auditLogPath");
  }

  return {
    redisUrl,
    redisUrlEnv,
    keyPrefix,
    operator,
    apply,
    scanCount: parseInteger("scanCount", options.scanCount ?? DEFAULT_SCAN_COUNT, 10, 10_000),
    batchSize: parseInteger("batchSize", options.batchSize ?? DEFAULT_BATCH_SIZE, 1, 1_000),
    delayMs: parseInteger("delayMs", options.delayMs ?? DEFAULT_DELAY_MS, 0, 60_000),
    checkpointPath,
    auditLogPath,
    resume: options.resume === true
  };
}

function readCheckpoint(config) {
  if (!config.resume) return { cursor: "0" };
  if (!config.checkpointPath || !fs.existsSync(config.checkpointPath)) {
    throw new Error("resume requires an existing checkpoint file");
  }
  const checkpoint = JSON.parse(fs.readFileSync(config.checkpointPath, "utf8"));
  if (checkpoint.redisUrl !== redactRedisUrl(config.redisUrl) || checkpoint.keyPrefix !== config.keyPrefix) {
    throw new Error("checkpoint target does not match redisUrl and keyPrefix");
  }
  if (checkpoint.mode !== (config.apply ? "apply" : "dry-run")) {
    throw new Error("checkpoint mode does not match the requested operation");
  }
  if (checkpoint.completed === true) {
    throw new Error("checkpoint is already complete; use a new checkpoint path");
  }
  return {
    ...checkpoint,
    cursor: String(checkpoint.cursor ?? "0")
  };
}

function writeCheckpoint(config, report) {
  if (!config.checkpointPath) return;
  ensureParentDirectory(config.checkpointPath);
  const payload = {
    version: 1,
    redisUrl: report.redisUrl,
    keyPrefix: report.keyPrefix,
    operator: report.operator,
    mode: report.mode,
    startedAt: report.startedAt,
    cursor: report.cursor,
    completed: report.completed,
    scanned: report.scanned,
    legacyCandidates: report.legacyCandidates,
    eligibleHashes: report.eligibleHashes,
    deleted: report.deleted,
    remainingEligibleHashes: report.remainingEligibleHashes,
    batches: report.batches,
    excluded: report.excluded,
    services: report.services,
    updatedAt: report.completedAt ?? new Date().toISOString()
  };
  const temporaryPath = `${config.checkpointPath}.tmp`;
  fs.writeFileSync(temporaryPath, `${JSON.stringify(payload, null, 2)}\n`, "utf8");
  fs.renameSync(temporaryPath, config.checkpointPath);
}

function appendAudit(config, report) {
  if (!config.auditLogPath) return;
  ensureParentDirectory(config.auditLogPath);
  const event = {
    version: 1,
    operation: "legacy-metrics-cleanup",
    ...report
  };
  fs.appendFileSync(config.auditLogPath, `${JSON.stringify(event)}\n`, "utf8");
}

async function createRedisClient(redisUrl) {
  const { default: Redis } = await import("ioredis");
  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableOfflineQueue: false
  });
  await redis.connect();
  return redis;
}

function closeRedis(redis) {
  if (typeof redis.disconnect === "function") {
    redis.disconnect();
  }
}

function validateRedisUrl(value) {
  const url = new URL(value);
  if (url.protocol !== "redis:" && url.protocol !== "rediss:") {
    throw new Error("redisUrl must use redis:// or rediss://");
  }
  if (!url.hostname) {
    throw new Error("redisUrl must include a hostname");
  }
}

function redactRedisUrl(value) {
  const url = new URL(value);
  if (url.username) url.username = "***";
  if (url.password) url.password = "***";
  for (const [key] of url.searchParams) {
    if (/token|secret|password|credential|auth/i.test(key)) {
      url.searchParams.set(key, "***");
    }
  }
  return url.toString();
}

function parseInteger(name, value, minimum, maximum) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${name} must be an integer between ${minimum} and ${maximum}`);
  }
  return parsed;
}

function normalizeOptionalPath(value) {
  return value ? path.resolve(String(value)) : "";
}

function ensureParentDirectory(filePath) {
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
}

function sortedObject(value) {
  return Object.fromEntries(Object.entries(value).sort(([left], [right]) => left.localeCompare(right)));
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function parseArgs(argv) {
  const options = { apply: false, resume: false, allowEmptyPrefix: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--apply") options.apply = true;
    else if (argument === "--resume") options.resume = true;
    else if (argument === "--allow-empty-prefix") options.allowEmptyPrefix = true;
    else if (argument === "--pretty") options.pretty = true;
    else if (argument === "--redis-url") options.redisUrl = requiredValue(argv, ++index, argument);
    else if (argument === "--redis-url-env") options.redisUrlEnv = requiredValue(argv, ++index, argument);
    else if (argument === "--key-prefix") options.keyPrefix = requiredValue(argv, ++index, argument, true);
    else if (argument === "--confirm") options.confirm = requiredValue(argv, ++index, argument);
    else if (argument === "--operator") options.operator = requiredValue(argv, ++index, argument);
    else if (argument === "--scan-count") options.scanCount = requiredValue(argv, ++index, argument);
    else if (argument === "--batch-size") options.batchSize = requiredValue(argv, ++index, argument);
    else if (argument === "--delay-ms") options.delayMs = requiredValue(argv, ++index, argument);
    else if (argument === "--checkpoint") options.checkpointPath = requiredValue(argv, ++index, argument);
    else if (argument === "--audit-log") options.auditLogPath = requiredValue(argv, ++index, argument);
    else if (argument === "--help" || argument === "-h") options.help = true;
    else throw new Error(`unknown argument: ${argument}`);
  }
  return options;
}

function requiredValue(argv, index, argument, allowEmpty = false) {
  if (index >= argv.length || (!allowEmpty && !argv[index])) {
    throw new Error(`${argument} requires a value`);
  }
  return argv[index];
}

function usage() {
  return [
    "Usage: node tools/metrics-legacy-cleanup.js (--redis-url <url> | --redis-url-env <name>) --key-prefix <prefix> [options]",
    "",
    "Dry-run is the default. For an empty prefix also pass --allow-empty-prefix.",
    "Apply requires --apply --confirm legacy-metrics-unlink --operator <identity> --checkpoint <file> --audit-log <file>.",
    "Options: --scan-count <10-10000> --batch-size <1-1000> --delay-ms <0-60000> --resume --pretty"
  ].join("\n");
}

async function main() {
  try {
    const options = parseArgs(process.argv.slice(2));
    if (options.help) {
      console.log(usage());
      return;
    }
    const report = await cleanupLegacyMetrics(options);
    console.log(JSON.stringify(report, null, options.pretty ? 2 : 0));
    if (!report.ok) process.exitCode = 1;
  } catch (error) {
    console.error(error?.message || String(error));
    process.exitCode = 1;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main();
}
