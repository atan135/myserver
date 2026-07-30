import Redis from "ioredis";
import { connect, StringCodec } from "nats";
import { pathToFileURL } from "node:url";

import { getConfig } from "./config.js";
import { maybeRegisterService } from "./registry-client.js";
import { natsConnectOptions } from "../../../packages/nats-client/node.js";

const codec = StringCodec();
const DEFAULT_INSTANCE_ID = "default";
const METRICS_BUCKET_SECONDS = 5;
const MAX_FUTURE_TIMESTAMP_SECONDS = 30;
const IDENTIFIER_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

const DEFAULT_STORAGE_CONFIG = Object.freeze({
  metricsKeyPrefix: "",
  metricsTtlSeconds: 604800,
  heartbeatTtlSeconds: 30,
  metricsStorageSchemaVersion: 2,
  metricsLatestTtlSeconds: 180,
  metricsLatestIndexTtlSeconds: 300,
  metricsHistoryRetentionSeconds: 4500,
  metricsMaxInstancesPerService: 64,
  metricsHistoryIndexMaxMembers: 900,
  metricsMaxRecordBytes: 16384
});

const storageCounters = {
  capacityRejected: 0,
  rejected: 0,
  writes: 0
};

// metrics-v2-write-v2. All v2 state transitions, including capacity checks,
// happen inside this one script so concurrent collector replicas cannot race.
export const METRICS_V2_WRITE_LUA = `
local function valid_identifier(value)
  if not value or string.len(value) == 0 or string.len(value) > 64 then
    return false
  end
  return string.match(value, "^[A-Za-z0-9][A-Za-z0-9%._%-]*$") ~= nil
end

local function write_hash(key, values)
  for field, value in pairs(values) do
    if type(field) ~= "string" or (type(value) ~= "string" and type(value) ~= "number") then
      return false
    end
    redis.call("HSET", key, field, tostring(value))
  end
  return true
end

local latest_key = KEYS[1]
local latest_index_key = KEYS[2]
local history_key = KEYS[3]
local history_index_key = KEYS[4]
local legacy_metrics_key = KEYS[5]
local legacy_heartbeat_key = KEYS[6]
local legacy_instance_heartbeat_key = KEYS[7]

local record_json = ARGV[1]
local legacy_fields_json = ARGV[2]
local service = ARGV[3]
local instance = ARGV[4]
local bucket = tonumber(ARGV[5])
local reported_at = tonumber(ARGV[6])
local received_at = tonumber(ARGV[7])
local latest_ttl = tonumber(ARGV[8])
local latest_index_ttl = tonumber(ARGV[9])
local history_ttl = tonumber(ARGV[10])
local max_instances = tonumber(ARGV[11])
local history_index_max_members = tonumber(ARGV[12])
local now = tonumber(ARGV[13])
local max_record_bytes = tonumber(ARGV[14])
local legacy_metrics_ttl = tonumber(ARGV[15])
local legacy_heartbeat_ttl = tonumber(ARGV[16])

if not valid_identifier(service) or not valid_identifier(instance) then
  return { "reject", "INVALID_IDENTIFIER" }
end
if not bucket or bucket % 5 ~= 0 or not reported_at or not received_at or not now then
  return { "reject", "INVALID_TIMESTAMP" }
end
if not latest_ttl or not latest_index_ttl or not history_ttl or not max_instances or not history_index_max_members or not max_record_bytes or not legacy_metrics_ttl or not legacy_heartbeat_ttl then
  return { "reject", "INVALID_CONFIGURATION" }
end
if reported_at > now + 30 or bucket > now + 30 then
  return { "reject", "FUTURE_TIMESTAMP" }
end
if bucket < now - history_ttl then
  return { "reject", "OUTSIDE_HISTORY_RETENTION" }
end
if string.len(record_json) > max_record_bytes then
  return { "reject", "RECORD_TOO_LARGE" }
end

local record_ok, record = pcall(cjson.decode, record_json)
local legacy_ok, legacy_fields = pcall(cjson.decode, legacy_fields_json)
if not record_ok or not legacy_ok or type(record) ~= "table" or type(legacy_fields) ~= "table" then
  return { "reject", "INVALID_RECORD" }
end
if record._schema ~= "metrics-v2" or record._service ~= service or record._instance_id ~= instance
  or tostring(record._bucket) ~= tostring(bucket) or tostring(record._reported_at) ~= tostring(reported_at)
  or tostring(record._received_at) ~= tostring(received_at) or record.instance_id ~= instance then
  return { "reject", "INVALID_RECORD" }
end

local existing_bucket = tonumber(redis.call("HGET", latest_key, "_bucket"))
local update_latest = not existing_bucket or bucket >= existing_bucket

if update_latest then
  redis.call("ZREMRANGEBYSCORE", latest_index_key, "-inf", now - latest_ttl)
  local existing_instance = redis.call("ZSCORE", latest_index_key, instance)
  if not existing_instance and redis.call("ZCARD", latest_index_key) >= max_instances then
    return { "reject", "LATEST_CAPACITY_EXCEEDED" }
  end
end

redis.call("ZREMRANGEBYSCORE", history_index_key, "-inf", now - history_ttl)
local existing_history_bucket = redis.call("ZSCORE", history_index_key, tostring(bucket))
if not existing_history_bucket and redis.call("ZCARD", history_index_key) >= history_index_max_members then
  return { "reject", "HISTORY_INDEX_CAPACITY_EXCEEDED" }
end
if not redis.call("HGET", history_key, instance) and redis.call("HLEN", history_key) >= max_instances then
  return { "reject", "HISTORY_INSTANCE_CAPACITY_EXCEEDED" }
end

if not write_hash(legacy_metrics_key, legacy_fields) then
  return { "reject", "INVALID_LEGACY_FIELDS" }
end
redis.call("EXPIRE", legacy_metrics_key, legacy_metrics_ttl)
redis.call("SET", legacy_heartbeat_key, tostring(reported_at), "EX", legacy_heartbeat_ttl)
redis.call("SET", legacy_instance_heartbeat_key, tostring(reported_at), "EX", legacy_heartbeat_ttl)

if update_latest then
  redis.call("DEL", latest_key)
  if not write_hash(latest_key, record) then
    return { "reject", "INVALID_RECORD_FIELDS" }
  end
  redis.call("EXPIRE", latest_key, latest_ttl)
  redis.call("ZADD", latest_index_key, reported_at, instance)
  redis.call("EXPIRE", latest_index_key, latest_index_ttl)
end

redis.call("HSET", history_key, instance, record_json)
redis.call("EXPIRE", history_key, history_ttl)
redis.call("ZADD", history_index_key, bucket, tostring(bucket))
redis.call("EXPIRE", history_index_key, history_ttl)

return { "ok", update_latest and "1" or "0" }
`;

function normalizeMetricFields(metrics) {
  if (!metrics || typeof metrics !== "object" || Array.isArray(metrics)) {
    throw new Error("invalid metrics fields");
  }

  const fields = Object.create(null);
  for (const [key, value] of Object.entries(metrics)) {
    if (value === undefined || value === null) {
      continue;
    }
    fields[key] = String(value);
  }
  return fields;
}

function parseTimestamp(name, value) {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 0) {
    throw new Error(`invalid metrics ${name}`);
  }
  return parsed;
}

function assertIdentifier(name, value) {
  if (!IDENTIFIER_PATTERN.test(value)) {
    throw new Error(`invalid metrics ${name}`);
  }
  return value;
}

function assertKeyPrefix(value) {
  if (typeof value !== "string" || /[\s\u0000-\u001f\u007f]/.test(value)) {
    throw new Error("invalid metrics key prefix");
  }
  return value;
}

function resolveStorageConfig(config) {
  const resolved = {
    ...DEFAULT_STORAGE_CONFIG,
    ...config
  };
  if (resolved.metricsStorageSchemaVersion !== 2) {
    throw new Error("unsupported metrics storage schema version");
  }
  if (!Number.isSafeInteger(resolved.metricsLatestTtlSeconds) || resolved.metricsLatestTtlSeconds <= METRICS_BUCKET_SECONDS * 4) {
    throw new Error("invalid metrics latest TTL");
  }
  if (!Number.isSafeInteger(resolved.metricsLatestIndexTtlSeconds) || resolved.metricsLatestIndexTtlSeconds < resolved.metricsLatestTtlSeconds) {
    throw new Error("invalid metrics latest index TTL");
  }
  if (!Number.isSafeInteger(resolved.metricsHistoryRetentionSeconds) || resolved.metricsHistoryRetentionSeconds < 3600) {
    throw new Error("invalid metrics history retention");
  }
  if (!Number.isSafeInteger(resolved.metricsMaxInstancesPerService) || resolved.metricsMaxInstancesPerService < 1) {
    throw new Error("invalid metrics instance capacity");
  }
  if (!Number.isSafeInteger(resolved.metricsHistoryIndexMaxMembers) || resolved.metricsHistoryIndexMaxMembers < Math.ceil(resolved.metricsHistoryRetentionSeconds / METRICS_BUCKET_SECONDS)) {
    throw new Error("invalid metrics history index capacity");
  }
  if (!Number.isSafeInteger(resolved.metricsMaxRecordBytes) || resolved.metricsMaxRecordBytes < 256) {
    throw new Error("invalid metrics record size limit");
  }
  if (!Number.isSafeInteger(resolved.metricsTtlSeconds) || resolved.metricsTtlSeconds < 1) {
    throw new Error("invalid legacy metrics TTL");
  }
  if (!Number.isSafeInteger(resolved.heartbeatTtlSeconds) || resolved.heartbeatTtlSeconds < 1) {
    throw new Error("invalid legacy metrics heartbeat TTL");
  }
  return {
    ...resolved,
    metricsKeyPrefix: assertKeyPrefix(resolved.metricsKeyPrefix)
  };
}

function currentUnixSeconds(config) {
  const now = typeof config.nowSeconds === "function"
    ? config.nowSeconds()
    : (config.nowSeconds ?? Math.floor(Date.now() / 1000));
  return parseTimestamp("collector clock", now);
}

export function normalizeInstanceId(instanceId) {
  if (instanceId === undefined || instanceId === null || String(instanceId).trim() === "") {
    return DEFAULT_INSTANCE_ID;
  }
  return String(instanceId);
}

export function buildMetricsKey(serviceName, instanceId, bucket, keyPrefix = "") {
  return `${keyPrefix}metrics:${serviceName}:${normalizeInstanceId(instanceId)}:${bucket}`;
}

export function buildMetricsV2Keys(serviceName, instanceId, bucket, keyPrefix = "") {
  return {
    latest: `${keyPrefix}metrics:v2:latest:${serviceName}:${instanceId}`,
    latestIndex: `${keyPrefix}metrics:v2:latest-index:${serviceName}`,
    history: `${keyPrefix}metrics:v2:history:${serviceName}:${bucket}`,
    historyIndex: `${keyPrefix}metrics:v2:history-index:${serviceName}`
  };
}

export function getMetricsStorageCounters() {
  return {
    ...storageCounters,
    metrics_storage_schema_version: 2,
    metrics_capacity_rejected_total: storageCounters.capacityRejected,
    metrics_storage_rejected_total: storageCounters.rejected
  };
}

function buildMetricRecord(payload, config, now, { enforceV2Window = true } = {}) {
  if (!payload || typeof payload !== "object" || !payload.service || payload.bucket === undefined || !payload.metrics) {
    throw new Error("invalid metrics payload");
  }

  const serviceName = assertIdentifier("service", String(payload.service));
  const instanceId = assertIdentifier(
    "instance_id",
    normalizeInstanceId(payload.instance_id)
  );
  const bucket = parseTimestamp("bucket", payload.bucket);
  const timestamp = parseTimestamp("timestamp", payload.timestamp ?? now);
  if (bucket % METRICS_BUCKET_SECONDS !== 0) {
    throw new Error("invalid metrics bucket");
  }
  if (timestamp > now + MAX_FUTURE_TIMESTAMP_SECONDS || bucket > now + MAX_FUTURE_TIMESTAMP_SECONDS) {
    throw new Error("metrics payload timestamp is too far in the future");
  }
  if (enforceV2Window && bucket < now - config.metricsHistoryRetentionSeconds) {
    throw new Error("metrics payload is outside history retention");
  }

  const metricFields = normalizeMetricFields(payload.metrics);
  if (Object.keys(metricFields).length === 0) {
    throw new Error("invalid metrics fields");
  }
  const receivedAt = now;
  const record = {
    ...metricFields,
    _schema: "metrics-v2",
    _service: serviceName,
    _instance_id: instanceId,
    _bucket: String(bucket),
    _reported_at: String(timestamp),
    _received_at: String(receivedAt),
    instance_id: instanceId
  };
  const recordJson = JSON.stringify(record);
  if (Buffer.byteLength(recordJson, "utf8") > config.metricsMaxRecordBytes) {
    throw new Error("metrics payload exceeds record size limit");
  }

  return {
    serviceName,
    instanceId,
    bucket,
    timestamp,
    receivedAt,
    record,
    recordJson,
    legacyFields: {
      ...metricFields,
      instance_id: instanceId
    }
  };
}

function classifyWriteResult(result) {
  if (!Array.isArray(result) || result[0] !== "ok") {
    const code = Array.isArray(result) && typeof result[1] === "string"
      ? result[1]
      : "REDIS_SCRIPT_FAILED";
    if (code.includes("CAPACITY")) {
      storageCounters.capacityRejected += 1;
    }
    storageCounters.rejected += 1;
    throw new Error(`metrics v2 write rejected: ${code}`);
  }
  storageCounters.writes += 1;
  return {
    latestUpdated: result[1] === "1"
  };
}

async function writeLegacyMetrics(redis, config, record) {
  const metricsKey = buildMetricsKey(
    record.serviceName,
    record.instanceId,
    record.bucket,
    config.metricsKeyPrefix
  );
  const heartbeatKey = `${config.metricsKeyPrefix}metrics:heartbeat:${record.serviceName}`;
  const instanceHeartbeatKey = `${heartbeatKey}:${record.instanceId}`;
  const pipe = redis.pipeline();

  pipe.hset(metricsKey, record.legacyFields);
  pipe.expire(metricsKey, config.metricsTtlSeconds);
  pipe.set(heartbeatKey, String(record.timestamp), "EX", config.heartbeatTtlSeconds);
  pipe.set(
    instanceHeartbeatKey,
    String(record.timestamp),
    "EX",
    config.heartbeatTtlSeconds
  );
  await pipe.exec();
}

async function writeMetricsV2(redis, config, record, now) {
  const keys = buildMetricsV2Keys(
    record.serviceName,
    record.instanceId,
    record.bucket,
    config.metricsKeyPrefix
  );
  const legacyMetricsKey = buildMetricsKey(
    record.serviceName,
    record.instanceId,
    record.bucket,
    config.metricsKeyPrefix
  );
  const legacyHeartbeatKey = `${config.metricsKeyPrefix}metrics:heartbeat:${record.serviceName}`;
  const legacyInstanceHeartbeatKey = `${legacyHeartbeatKey}:${record.instanceId}`;
  const result = await redis.eval(
    METRICS_V2_WRITE_LUA,
    7,
    keys.latest,
    keys.latestIndex,
    keys.history,
    keys.historyIndex,
    legacyMetricsKey,
    legacyHeartbeatKey,
    legacyInstanceHeartbeatKey,
    record.recordJson,
    JSON.stringify(record.legacyFields),
    record.serviceName,
    record.instanceId,
    String(record.bucket),
    String(record.timestamp),
    String(record.receivedAt),
    String(config.metricsLatestTtlSeconds),
    String(config.metricsLatestIndexTtlSeconds),
    String(config.metricsHistoryRetentionSeconds),
    String(config.metricsMaxInstancesPerService),
    String(config.metricsHistoryIndexMaxMembers),
    String(now),
    String(config.metricsMaxRecordBytes),
    String(config.metricsTtlSeconds),
    String(config.heartbeatTtlSeconds)
  );
  return classifyWriteResult(result);
}

export function isDirectRun(metaUrl = import.meta.url, argvPath = process.argv[1]) {
  return Boolean(argvPath) && metaUrl === pathToFileURL(argvPath).href;
}

export async function writeMetrics(redis, config, message) {
  const resolvedConfig = resolveStorageConfig(config);
  const now = currentUnixSeconds(resolvedConfig);
  const raw = codec.decode(message.data);
  const supportsV2 = typeof redis.eval === "function";
  const record = buildMetricRecord(JSON.parse(raw), resolvedConfig, now, {
    // Legacy-only in-memory adapters intentionally retain their previous
    // long-history behaviour while the production ioredis path is always v2.
    enforceV2Window: supportsV2
  });

  // The old test and migration adapters use a minimal pipeline-only fake. A
  // production ioredis client always has eval(), and therefore always v2-writes.
  if (!supportsV2) {
    await writeLegacyMetrics(redis, resolvedConfig, record);
    return { schemaVersion: 1, latestUpdated: true };
  }

  const result = await writeMetricsV2(redis, resolvedConfig, record, now);
  return {
    schemaVersion: resolvedConfig.metricsStorageSchemaVersion,
    ...result
  };
}

async function main() {
  const config = getConfig();
  const registryStatus = await maybeRegisterService(null, config);
  if (registryStatus.registered === false) {
    console.log(
      `[metrics-collector] service registry disabled: service=${registryStatus.service}, instance=${registryStatus.instance}, build=${registryStatus.build_version}; ${registryStatus.reason}`
    );
  }

  const redis = new Redis(config.redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 3
  });
  await redis.connect();

  const nats = await connect(natsConnectOptions(config.natsUrl, "metrics-collector"));

  nats.closed().then((error) => {
    if (error) {
      console.error("[metrics-collector] nats closed:", error.message);
    }
  });

  const subscription = nats.subscribe(config.metricsSubject);
  console.log(
    `metrics-collector subscribed to ${config.metricsSubject}, writing metrics-v${config.metricsStorageSchemaVersion} to Redis; metrics_storage_schema_version=${config.metricsStorageSchemaVersion}`
  );

  let shuttingDown = false;
  const shutdown = async (signal) => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.log(`metrics-collector shutdown: ${signal}`);

    subscription.unsubscribe();
    try {
      await nats.drain();
    } catch {
      nats.close();
    }
    await redis.quit();
    process.exit(0);
  };

  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("SIGINT", () => shutdown("SIGINT"));

  for await (const message of subscription) {
    try {
      await writeMetrics(redis, config, message);
    } catch (error) {
      const counters = getMetricsStorageCounters();
      console.error(
        `[metrics-collector] write failed: ${error.message}; metrics_capacity_rejected_total=${counters.metrics_capacity_rejected_total}`
      );
    }
  }
}

if (isDirectRun()) {
  main().catch((error) => {
    console.error("[metrics-collector] fatal:", error);
    process.exit(1);
  });
}
