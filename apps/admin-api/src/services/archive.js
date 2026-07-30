/**
 * Archive bounded metrics-v2 history buckets into PostgreSQL.
 * Redis history is removed only after a successful idempotent database upsert.
 */
import {
  aggregateMetricRecords,
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

const ARCHIVE_DEFAULTS = {
  metricsKeyPrefix: "",
  metricsArchiveAfterSeconds: 3600,
  metricsHistoryRetentionSeconds: 4500,
  metricsArchiveBatchSize: 128
};

export async function runArchiveTask(redis, dbPool, options = {}) {
  const startedAt = Date.now();
  const resolved = resolveArchiveOptions(options);
  const archiveBeforeBucket = Math.floor(Date.now() / 1000) - resolved.metricsArchiveAfterSeconds;
  let archived = 0;
  let failed = 0;

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
  }

  return {
    archived,
    failed,
    duration_ms: Date.now() - startedAt
  };
}

/**
 * Archive at most one bounded v2 history-index batch for a service.
 * `toBucket` is exclusive, matching the archive-after cutoff.
 */
export async function archiveServiceMetrics(redis, dbPool, serviceName, fromBucket, toBucket, options = {}) {
  const resolved = resolveArchiveOptions(options);
  const indexKey = `${resolved.metricsKeyPrefix}metrics:v2:history-index:${serviceName}`;
  const bucketMembers = await redis.zrangebyscore(
    indexKey,
    fromBucket,
    `(${toBucket}`,
    "LIMIT",
    0,
    resolved.metricsArchiveBatchSize
  );
  const buckets = [...new Set((Array.isArray(bucketMembers) ? bucketMembers : []).map((value) => Number(value)))]
    .filter((bucket) => Number.isSafeInteger(bucket) && bucket >= fromBucket && bucket < toBucket)
    .sort((left, right) => left - right)
    .slice(0, resolved.metricsArchiveBatchSize);

  if (buckets.length === 0) {
    return { archived: 0, failed: 0 };
  }

  const readPipeline = redis.pipeline();
  for (const bucket of buckets) {
    readPipeline.hgetall(`${resolved.metricsKeyPrefix}metrics:v2:history:${serviceName}:${bucket}`);
  }
  const readResults = await readPipeline.exec();
  const rows = [];
  let failed = 0;

  for (let offset = 0; offset < buckets.length; offset += 1) {
    const bucket = buckets[offset];
    const result = pipelineValue(readResults[offset]);
    if (result.error) {
      failed += 1;
      continue;
    }

    const records = parseHistoryRecords(serviceName, bucket, result.value);
    if (!records.ok) {
      failed += 1;
      continue;
    }
    if (records.value.length === 0) {
      // A dangling index member is harmless. Remove it without touching any data key.
      rows.push({ bucket, empty: true, data: null });
      continue;
    }
    rows.push({
      bucket,
      empty: false,
      data: aggregateMetricRecords(records.value.map(metricRecordForAggregation))
    });
  }

  const writeRows = rows.filter((row) => !row.empty);
  if (writeRows.length > 0) {
    try {
      await upsertArchiveRows(dbPool, serviceName, writeRows);
    } catch (error) {
      console.error(`[archive] database upsert failed for ${serviceName}:`, error);
      return { archived: 0, failed: failed + writeRows.length };
    }
  }

  const cleanupPipeline = redis.pipeline();
  for (const row of rows) {
    cleanupPipeline.unlink(`${resolved.metricsKeyPrefix}metrics:v2:history:${serviceName}:${row.bucket}`);
    cleanupPipeline.zrem(indexKey, String(row.bucket));
  }
  try {
    const cleanupResults = await cleanupPipeline.exec();
    if (cleanupResults.some((result) => pipelineValue(result).error)) {
      throw new Error("Redis archive cleanup pipeline failed");
    }
  } catch (error) {
    // Database rows are idempotent. Keep the index/hash source for a later retry.
    console.error(`[archive] Redis cleanup failed for ${serviceName}:`, error);
    return { archived: 0, failed: failed + rows.length };
  }

  return { archived: writeRows.length, failed };
}

function resolveArchiveOptions(options) {
  const resolved = { ...ARCHIVE_DEFAULTS, ...(options || {}) };
  for (const name of ["metricsArchiveAfterSeconds", "metricsHistoryRetentionSeconds", "metricsArchiveBatchSize"]) {
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

async function upsertArchiveRows(dbPool, serviceName, rows) {
  const params = [];
  const values = rows.map((row, index) => {
    const archive = buildArchiveRow(serviceName, row.bucket, row.data);
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

function buildArchiveRow(serviceName, bucketTime, data) {
  const qps = parseMetricInt(data.qps);
  const latencyMs = parseMetricInt(data.latency_ms);
  const onlineValue = getOnlineValue(serviceName, data, ARCHIVE_SERVICE_CONFIGS);
  const excluded = new Set([
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
  const extra = Object.fromEntries(Object.entries(data).filter(([key]) => !excluded.has(key)));
  return { serviceName, bucketTime, qps, latencyMs, onlineValue, extra };
}

function pipelineValue(result) {
  if (Array.isArray(result)) {
    return { error: result[0] || null, value: result[1] };
  }
  return { error: null, value: result };
}
