import fs from "node:fs";
import path from "node:path";

import dotenv from "dotenv";

const envPath = path.resolve(process.cwd(), ".env");
if (fs.existsSync(envPath)) {
  dotenv.config({ path: envPath });
}

const METRICS_BUCKET_SECONDS = 5;
const MIN_LATEST_TTL_SECONDS = METRICS_BUCKET_SECONDS * 4 + 1;

function parseBoolean(value, fallback) {
  if (value === undefined) return fallback;
  return value === "true" || value === "1";
}

function parseStrictBoolean(name, value, fallback) {
  if (value === undefined) return fallback;
  if (value === "true" || value === "1") return true;
  if (value === "false" || value === "0") return false;
  throw new Error(`${name} must be one of true, false, 1 or 0`);
}

function parseInteger(name, rawValue, { minimum, maximum = Number.MAX_SAFE_INTEGER }) {
  const value = Number(rawValue);
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) {
    throw new Error(
      `${name} must be an integer between ${minimum} and ${maximum}`
    );
  }
  return value;
}

function parseKeyPrefix(name, value) {
  if (typeof value !== "string") {
    throw new Error(`${name} must be a string`);
  }
  if (/[\s\u0000-\u001f\u007f]/.test(value)) {
    throw new Error(`${name} must not contain whitespace or control characters`);
  }
  return value;
}

export function getConfig() {
  const metricsLatestTtlSeconds = parseInteger(
    "METRICS_LATEST_TTL_SECONDS",
    process.env.METRICS_LATEST_TTL_SECONDS || "180",
    { minimum: MIN_LATEST_TTL_SECONDS, maximum: 86400 }
  );
  const metricsLatestIndexTtlSeconds = parseInteger(
    "METRICS_LATEST_INDEX_TTL_SECONDS",
    process.env.METRICS_LATEST_INDEX_TTL_SECONDS || "300",
    { minimum: metricsLatestTtlSeconds, maximum: 86400 }
  );
  const metricsHistoryRetentionSeconds = parseInteger(
    "METRICS_HISTORY_RETENTION_SECONDS",
    process.env.METRICS_HISTORY_RETENTION_SECONDS || "4500",
    { minimum: 3600, maximum: 86400 }
  );
  const metricsMaxInstancesPerService = parseInteger(
    "METRICS_MAX_INSTANCES_PER_SERVICE",
    process.env.METRICS_MAX_INSTANCES_PER_SERVICE || "64",
    { minimum: 1, maximum: 4096 }
  );
  const metricsHistoryIndexMaxMembers = parseInteger(
    "METRICS_HISTORY_INDEX_MAX_MEMBERS",
    process.env.METRICS_HISTORY_INDEX_MAX_MEMBERS || "900",
    { minimum: Math.ceil(metricsHistoryRetentionSeconds / METRICS_BUCKET_SECONDS), maximum: 17280 }
  );
  const metricsMaxRecordBytes = parseInteger(
    "METRICS_MAX_RECORD_BYTES",
    process.env.METRICS_MAX_RECORD_BYTES || "16384",
    { minimum: 256, maximum: 1048576 }
  );
  const metricsStorageSchemaVersion = parseInteger(
    "METRICS_STORAGE_SCHEMA_VERSION",
    process.env.METRICS_STORAGE_SCHEMA_VERSION || "2",
    { minimum: 2, maximum: 2 }
  );
  const metricsLegacyWriteEnabled = parseStrictBoolean(
    "METRICS_LEGACY_WRITE_ENABLED",
    process.env.METRICS_LEGACY_WRITE_ENABLED,
    false
  );
  const metricsTtlSeconds = parseInteger(
    "METRICS_TTL_SECONDS",
    process.env.METRICS_TTL_SECONDS || "604800",
    { minimum: 1, maximum: 31536000 }
  );
  const heartbeatTtlSeconds = parseInteger(
    "METRICS_HEARTBEAT_TTL_SECONDS",
    process.env.METRICS_HEARTBEAT_TTL_SECONDS || "30",
    { minimum: 1, maximum: 86400 }
  );
  const metricsKeyPrefix = parseKeyPrefix(
    "METRICS_KEY_PREFIX",
    process.env.METRICS_KEY_PREFIX ?? process.env.REDIS_KEY_PREFIX ?? ""
  );

  return {
    serviceName: process.env.SERVICE_NAME || "metrics-collector",
    serviceInstanceId:
      process.env.SERVICE_INSTANCE_ID || "metrics-collector-001",
    serviceZone: process.env.SERVICE_ZONE || "local",
    serviceBuildVersion: process.env.SERVICE_BUILD_VERSION || "dev",
    registryKeyPrefix: process.env.REGISTRY_KEY_PREFIX ?? process.env.REDIS_KEY_PREFIX ?? "",
    metricsKeyPrefix,
    serviceRegistryRegister: parseBoolean(
      process.env.SERVICE_REGISTRY_REGISTER,
      false
    ),
    natsUrl: process.env.NATS_URL || "nats://127.0.0.1:4222",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    metricsSubject: process.env.METRICS_SUBJECT || "myserver.metrics.>",
    metricsLegacyWriteEnabled,
    metricsTtlSeconds,
    heartbeatTtlSeconds,
    metricsStorageSchemaVersion,
    metricsLatestTtlSeconds,
    metricsLatestIndexTtlSeconds,
    metricsHistoryRetentionSeconds,
    metricsMaxInstancesPerService,
    metricsHistoryIndexMaxMembers,
    metricsMaxRecordBytes
  };
}
