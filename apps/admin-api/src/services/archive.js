/**
 * Archive bounded metrics-v2 history buckets into PostgreSQL.
 * Redis keeps five-second samples; PostgreSQL stores one aggregate per minute.
 */
import crypto from "node:crypto";

import {
  aggregateMetricRecordsDetailed,
  getOnlineValue,
  parseMetricInt
} from "../monitoring/metrics-aggregation.js";

const SERVICE_NAMES = [
  "auth-http",
  "game-server",
  "game-proxy",
  "chat-server",
  "match-service",
  "announce-service",
  "mail-service",
  "admin-api"
];

const ARCHIVE_SERVICE_CONFIGS = {
  "auth-http": { onlineField: "unique_players" },
  "game-server": { onlineField: "online_players" },
  "game-proxy": { onlineField: "connections" },
  "chat-server": { onlineField: "online_players" },
  "match-service": { onlineField: "pool_size" },
  "announce-service": { onlineField: null },
  "mail-service": { onlineField: null },
  "admin-api": { onlineField: null }
};

const METRICS_BUCKET_SECONDS = 5;
export const METRICS_ARCHIVE_RESOLUTION_SECONDS = 60;
const EXPECTED_BUCKETS_PER_ARCHIVE_ROW = METRICS_ARCHIVE_RESOLUTION_SECONDS / METRICS_BUCKET_SECONDS;

const ARCHIVE_DEFAULTS = {
  metricsKeyPrefix: "",
  metricsArchiveAfterSeconds: 3600,
  metricsHistoryRetentionSeconds: 4500,
  metricsArchiveBatchSize: 240,
  metricsArchiveLockTtlMs: 240000
};

const ARCHIVE_CORE_FIELDS = new Set([
  "qps",
  "latency_ms",
  "online_sessions",
  "unique_players",
  "active_sessions_5m",
  "active_window_seconds",
  "online_players",
  "connections",
  "room_count",
  "pool_size"
]);

export async function runArchiveTask(redis, dbPool, options = {}) {
  const startedAt = Date.now();
  const resolved = resolveArchiveOptions(options);
  const nowSeconds = Number.isSafeInteger(options.nowSeconds)
    ? options.nowSeconds
    : Math.floor(Date.now() / 1000);
  const archiveBeforeBucket = alignBucket(
    nowSeconds - resolved.metricsArchiveAfterSeconds,
    METRICS_ARCHIVE_RESOLUTION_SECONDS
  );
  let archived = 0;
  let failed = 0;
  let sourceBuckets = 0;

  for (const serviceName of SERVICE_NAMES) {
    const result = await archiveServiceMetrics(
      redis,
      dbPool,
      serviceName,
      0,
      archiveBeforeBucket,
      resolved
    );
    archived += result.archived;
    failed += result.failed;
    sourceBuckets += result.source_buckets;
  }

  return {
    archived,
    failed,
    source_buckets: sourceBuckets,
    resolution_seconds: METRICS_ARCHIVE_RESOLUTION_SECONDS,
    duration_ms: Date.now() - startedAt
  };
}

export async function runArchiveTaskWithLock(redis, dbPool, options = {}) {
  const resolved = resolveArchiveOptions(options);
  const lockKey = `${resolved.metricsKeyPrefix}metrics:v2:archive-lock`;
  const lockToken = crypto.randomUUID();
  const acquired = await redis.set(
    lockKey,
    lockToken,
    "PX",
    resolved.metricsArchiveLockTtlMs,
    "NX"
  );

  if (acquired !== "OK") {
    return {
      archived: 0,
      failed: 0,
      source_buckets: 0,
      resolution_seconds: METRICS_ARCHIVE_RESOLUTION_SECONDS,
      duration_ms: 0,
      skipped: true,
      reason: "archive_locked"
    };
  }

  let renewalInFlight = null;
  const renewalTimer = setInterval(() => {
    if (renewalInFlight) {
      return;
    }
    renewalInFlight = renewArchiveLock(
      redis,
      lockKey,
      lockToken,
      resolved.metricsArchiveLockTtlMs
    )
      .then((renewed) => {
        if (!renewed) {
          console.error("[archive] archive lock was lost while the task was running");
        }
      })
      .catch((error) => {
        console.error("[archive] failed to renew archive lock:", error);
      })
      .finally(() => {
        renewalInFlight = null;
      });
  }, Math.max(1000, Math.floor(resolved.metricsArchiveLockTtlMs / 3)));
  renewalTimer.unref?.();

  try {
    return await runArchiveTask(redis, dbPool, resolved);
  } finally {
    clearInterval(renewalTimer);
    if (renewalInFlight) {
      await renewalInFlight;
    }
    try {
      await releaseArchiveLock(redis, lockKey, lockToken);
    } catch (error) {
      console.error("[archive] failed to release archive lock:", error);
    }
  }
}

async function renewArchiveLock(redis, lockKey, lockToken, ttlMs) {
  const result = await redis.eval(
    `
      if redis.call("GET", KEYS[1]) == ARGV[1] then
        return redis.call("PEXPIRE", KEYS[1], ARGV[2])
      end
      return 0
    `,
    1,
    lockKey,
    lockToken,
    ttlMs
  );
  return Number(result) === 1;
}

async function releaseArchiveLock(redis, lockKey, lockToken) {
  return redis.eval(
    `
      if redis.call("GET", KEYS[1]) == ARGV[1] then
        return redis.call("DEL", KEYS[1])
      end
      return 0
    `,
    1,
    lockKey,
    lockToken
  );
}

/**
 * Archive a bounded number of source buckets for one service. The selected
 * score range always ends on a minute boundary, so the batch limit cannot
 * split an available minute.
 */
export async function archiveServiceMetrics(redis, dbPool, serviceName, fromBucket, toBucket, options = {}) {
  const resolved = resolveArchiveOptions(options);
  const indexKey = `${resolved.metricsKeyPrefix}metrics:v2:history-index:${serviceName}`;
  const firstMembers = await redis.zrangebyscore(
    indexKey,
    fromBucket,
    `(${toBucket}`,
    "LIMIT",
    0,
    1
  );
  const firstBucket = Number(firstMembers?.[0]);

  if (!Number.isSafeInteger(firstBucket)) {
    return { archived: 0, failed: 0, source_buckets: 0 };
  }

  const firstMinute = alignBucket(firstBucket, METRICS_ARCHIVE_RESOLUTION_SECONDS);
  const minuteBatchSize = Math.max(
    1,
    Math.floor(resolved.metricsArchiveBatchSize / EXPECTED_BUCKETS_PER_ARCHIVE_ROW)
  );
  const rangeEnd = Math.min(
    toBucket,
    firstMinute + minuteBatchSize * METRICS_ARCHIVE_RESOLUTION_SECONDS
  );
  const bucketMembers = await redis.zrangebyscore(
    indexKey,
    Math.max(fromBucket, firstMinute),
    `(${rangeEnd}`
  );
  const buckets = [...new Set((Array.isArray(bucketMembers) ? bucketMembers : []).map((value) => Number(value)))]
    .filter((bucket) => Number.isSafeInteger(bucket) && bucket >= fromBucket && bucket < rangeEnd)
    .sort((left, right) => left - right);

  if (buckets.length === 0) {
    return { archived: 0, failed: 0, source_buckets: 0 };
  }

  const readPipeline = redis.pipeline();
  for (const bucket of buckets) {
    readPipeline.hgetall(`${resolved.metricsKeyPrefix}metrics:v2:history:${serviceName}:${bucket}`);
  }
  const readResults = await readPipeline.exec();
  const minuteGroups = new Map();
  const invalidMinutes = new Set();
  let failed = 0;

  for (let offset = 0; offset < buckets.length; offset += 1) {
    const bucket = buckets[offset];
    const minuteBucket = alignBucket(bucket, METRICS_ARCHIVE_RESOLUTION_SECONDS);
    const result = pipelineValue(readResults[offset]);
    if (result.error) {
      failed += 1;
      invalidMinutes.add(minuteBucket);
      continue;
    }

    const records = parseHistoryRecords(serviceName, bucket, result.value);
    if (!records.ok) {
      failed += 1;
      invalidMinutes.add(minuteBucket);
      continue;
    }

    let group = minuteGroups.get(minuteBucket);
    if (!group) {
      group = [];
      minuteGroups.set(minuteBucket, group);
    }
    group.push({ bucket, records: records.value });
  }

  const archiveRows = [];
  const cleanupBuckets = [];
  for (const [minuteBucket, entries] of [...minuteGroups.entries()].sort(([left], [right]) => left - right)) {
    if (invalidMinutes.has(minuteBucket)) {
      continue;
    }
    cleanupBuckets.push(...entries.map((entry) => entry.bucket));
    const nonEmptyEntries = entries.filter((entry) => entry.records.length > 0);
    if (nonEmptyEntries.length > 0) {
      archiveRows.push(buildMinuteArchiveRow(serviceName, minuteBucket, nonEmptyEntries));
    }
  }

  if (archiveRows.length > 0) {
    try {
      await upsertArchiveRows(dbPool, archiveRows);
    } catch (error) {
      console.error(`[archive] database upsert failed for ${serviceName}:`, error);
      return {
        archived: 0,
        failed: failed + cleanupBuckets.length,
        source_buckets: 0
      };
    }
  }

  if (cleanupBuckets.length > 0) {
    const cleanupPipeline = redis.pipeline();
    for (const bucket of cleanupBuckets) {
      cleanupPipeline.unlink(`${resolved.metricsKeyPrefix}metrics:v2:history:${serviceName}:${bucket}`);
      cleanupPipeline.zrem(indexKey, String(bucket));
    }
    try {
      const cleanupResults = await cleanupPipeline.exec();
      if (cleanupResults.some((result) => pipelineValue(result).error)) {
        throw new Error("Redis archive cleanup pipeline failed");
      }
    } catch (error) {
      console.error(`[archive] Redis cleanup failed for ${serviceName}:`, error);
      return {
        archived: 0,
        failed: failed + cleanupBuckets.length,
        source_buckets: 0
      };
    }
  }

  return {
    archived: archiveRows.length,
    failed,
    source_buckets: cleanupBuckets.length
  };
}

function resolveArchiveOptions(options) {
  const resolved = { ...ARCHIVE_DEFAULTS, ...(options || {}) };
  for (const name of [
    "metricsArchiveAfterSeconds",
    "metricsHistoryRetentionSeconds",
    "metricsArchiveBatchSize",
    "metricsArchiveLockTtlMs"
  ]) {
    const value = Number(resolved[name]);
    if (!Number.isSafeInteger(value) || value <= 0) {
      throw new Error(`invalid archive option: ${name}`);
    }
    resolved[name] = value;
  }
  if (resolved.metricsArchiveAfterSeconds >= resolved.metricsHistoryRetentionSeconds) {
    throw new Error("metricsArchiveAfterSeconds must be less than metricsHistoryRetentionSeconds");
  }
  resolved.metricsKeyPrefix = String(resolved.metricsKeyPrefix || "");
  return resolved;
}

function parseHistoryRecords(serviceName, bucket, hash) {
  if (!hash || typeof hash !== "object" || Array.isArray(hash)) {
    return { ok: false, value: [] };
  }
  const records = [];
  for (const [instanceId, encoded] of Object.entries(hash)) {
    try {
      const data = JSON.parse(String(encoded));
      if (
        data._schema !== "metrics-v2" ||
        data._service !== serviceName ||
        data._instance_id !== instanceId ||
        Number(data._bucket) !== bucket
      ) {
        throw new Error("invalid metrics v2 history record");
      }
      records.push({ instanceId, data, legacy: false });
    } catch {
      return { ok: false, value: [] };
    }
  }
  return { ok: true, value: records };
}

function metricRecordForAggregation(record) {
  return {
    instanceId: record.instanceId,
    legacy: false,
    data: Object.fromEntries(Object.entries(record.data || {}).filter(([key]) => !key.startsWith("_")))
  };
}

function buildMinuteArchiveRow(serviceName, minuteBucket, entries) {
  const bucketData = [];
  let requestCount = 0;
  let weightedLatencyTotal = 0;
  let latencyWeight = 0;
  let fallbackLatency = 0;

  for (const entry of entries) {
    const detailed = aggregateMetricRecordsDetailed(entry.records.map(metricRecordForAggregation));
    bucketData.push(detailed.data);
    for (const instance of detailed.instances) {
      const instanceRequestCount = parseMetricInt(instance.data.qps);
      const instanceLatency = parseMetricInt(instance.data.latency_ms);
      requestCount += instanceRequestCount;
      fallbackLatency = Math.max(fallbackLatency, instanceLatency);
      if (instanceRequestCount > 0) {
        weightedLatencyTotal += instanceLatency * instanceRequestCount;
        latencyWeight += instanceRequestCount;
      }
    }
  }

  const onlineValues = bucketData.map((data) => getOnlineValue(serviceName, data, ARCHIVE_SERVICE_CONFIGS));
  const onlineValue = onlineValues.length > 0
    ? Math.round(onlineValues.reduce((sum, value) => sum + value, 0) / onlineValues.length)
    : 0;
  const extra = aggregateMinuteExtra(bucketData);
  extra.archive_resolution_seconds = METRICS_ARCHIVE_RESOLUTION_SECONDS;
  extra.source_bucket_count = entries.length;
  extra.expected_bucket_count = EXPECTED_BUCKETS_PER_ARCHIVE_ROW;
  extra.request_count = requestCount;
  extra.online_max = onlineValues.length > 0 ? Math.max(...onlineValues) : 0;

  return {
    serviceName,
    bucketTime: minuteBucket,
    qps: Math.round(requestCount / METRICS_ARCHIVE_RESOLUTION_SECONDS),
    latencyMs: latencyWeight > 0 ? Math.round(weightedLatencyTotal / latencyWeight) : fallbackLatency,
    onlineValue,
    extra
  };
}

function aggregateMinuteExtra(bucketData) {
  const numericValues = new Map();
  const textValues = new Map();
  const instanceIds = new Set();

  for (const data of bucketData) {
    for (const [key, rawValue] of Object.entries(data || {})) {
      if (ARCHIVE_CORE_FIELDS.has(key)) {
        continue;
      }
      if (key === "instance_ids") {
        for (const instanceId of String(rawValue).split(",")) {
          if (instanceId) {
            instanceIds.add(instanceId);
          }
        }
        continue;
      }

      const numericValue = Number(rawValue);
      if (Number.isFinite(numericValue)) {
        const values = numericValues.get(key) || [];
        values.push(numericValue);
        numericValues.set(key, values);
      } else {
        textValues.set(key, String(rawValue));
      }
    }
  }

  const extra = {};
  for (const [key, values] of numericValues) {
    extra[key] = isMinuteGaugeField(key)
      ? Math.max(...values)
      : values.reduce((sum, value) => sum + value, 0);
  }
  for (const [key, value] of textValues) {
    extra[key] = value;
  }
  if (instanceIds.size > 0) {
    extra.instance_ids = [...instanceIds].sort().join(",");
    extra.instance_count = instanceIds.size;
  }
  return extra;
}

function isMinuteGaugeField(key) {
  return key === "instance_count" ||
    key.endsWith("_current") ||
    key.endsWith("_backlog") ||
    key.endsWith("_age_ms") ||
    key.endsWith("_latency_ms") ||
    key.endsWith("_duration_ms") ||
    key.endsWith("_last") ||
    key.endsWith("_max") ||
    key.endsWith("_rate_basis_points");
}

async function upsertArchiveRows(dbPool, rows) {
  const params = [];
  const values = rows.map((archive, index) => {
    const base = index * 6;
    params.push(
      archive.serviceName,
      archive.bucketTime,
      archive.qps,
      archive.latencyMs,
      archive.onlineValue,
      JSON.stringify(archive.extra)
    );
    return `($${base + 1}, $${base + 2}, $${base + 3}, $${base + 4}, $${base + 5}, $${base + 6}::jsonb)`;
  });
  const statement = `INSERT INTO metrics_archive (service_name, bucket_time, qps, latency_ms, online_value, extra)
    VALUES ${values.join(", ")}
    ON CONFLICT (service_name, bucket_time)
    DO UPDATE SET qps = EXCLUDED.qps,
                  latency_ms = EXCLUDED.latency_ms,
                  online_value = EXCLUDED.online_value,
                  extra = EXCLUDED.extra`;

  if (typeof dbPool?.connect !== "function") {
    await dbPool.query(statement, params);
    return;
  }

  const client = await dbPool.connect();
  try {
    await client.query("BEGIN");
    await client.query(statement, params);
    await client.query("COMMIT");
  } catch (error) {
    try {
      await client.query("ROLLBACK");
    } catch {
      // The original database error is more useful to the caller.
    }
    throw error;
  } finally {
    client.release();
  }
}

function alignBucket(bucket, resolutionSeconds) {
  return Math.floor(bucket / resolutionSeconds) * resolutionSeconds;
}

function pipelineValue(result) {
  if (Array.isArray(result)) {
    return { error: result[0] || null, value: result[1] };
  }
  return { error: null, value: result };
}
