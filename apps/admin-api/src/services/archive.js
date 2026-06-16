/**
 * Archive Service - 将超期 metrics 数据从 Redis 归档到 PostgreSQL
 */
import {
  aggregateMetricRecords,
  getOnlineValue,
  parseMetricInt,
  parseMetricKey
} from "../monitoring/metrics-aggregation.js";

const REPORT_INTERVAL_MS = 5000;

function currentBucket() {
  return Math.floor(Date.now() / REPORT_INTERVAL_MS) * REPORT_INTERVAL_MS / 1000;
}

/**
 * 服务列表
 */
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

/**
 * 执行归档任务
 * 将 7 天前 ~ 8 天前的 Redis metrics 数据迁移到 PostgreSQL
 *
 * @param {import("ioredis").Redis} redis
 * @param {import("pg").Pool} dbPool
 * @returns {Promise<{archived: number, duration_ms: number}>}
 */
export async function runArchiveTask(redis, dbPool) {
  const startTime = Date.now();

  // 计算归档时间范围（7天前 ~ 8天前）
  const now = Math.floor(Date.now() / 1000);
  const sevenDaysAgo = now - 7 * 24 * 3600;
  const eightDaysAgo = now - 8 * 24 * 3600;

  let totalArchived = 0;

  for (const serviceName of SERVICE_NAMES) {
    const archived = await archiveServiceMetrics(redis, dbPool, serviceName, eightDaysAgo, sevenDaysAgo);
    totalArchived += archived;
  }

  return {
    archived: totalArchived,
    duration_ms: Date.now() - startTime
  };
}

/**
 * 归档单个服务的 metrics 数据
 */
export async function archiveServiceMetrics(redis, dbPool, serviceName, fromBucket, toBucket) {
  let archived = 0;
  let cursor = "0";
  const recordsByBucket = new Map();

  do {
    // 扫描该服务的 metrics keys
    const pattern = `metrics:${serviceName}:*`;
    const [nextCursor, keys] = await redis.scan(cursor, "MATCH", pattern, "COUNT", 100);
    cursor = nextCursor;

    for (const key of keys) {
      const parsed = parseMetricKey(serviceName, key);
      if (!parsed) {
        continue;
      }
      const bucket = parsed.bucket;

      // 只处理 7 天前 ~ 8 天前的数据
      if (bucket >= fromBucket && bucket < toBucket) {
        const data = await redis.hgetall(key);
        if (data && Object.keys(data).length > 0) {
          const records = recordsByBucket.get(bucket) || [];
          records.push({
            key,
            ...parsed,
            data
          });
          recordsByBucket.set(bucket, records);
        }
      }
    }
  } while (cursor !== "0");

  for (const [bucket, records] of recordsByBucket.entries()) {
    const data = aggregateMetricRecords(records);
    await insertArchiveRecord(dbPool, serviceName, bucket, data);
    for (const record of records) {
      await redis.del(record.key);
    }
    archived++;
  }

  return archived;
}

/**
 * 插入归档记录到 PostgreSQL
 */
async function insertArchiveRecord(dbPool, serviceName, bucketTime, data) {
  const qps = parseMetricInt(data.qps);
  const latencyMs = parseMetricInt(data.latency_ms);
  const onlineValue = getOnlineValue(serviceName, data, ARCHIVE_SERVICE_CONFIGS);

  // 收集扩展字段
  const extra = {};
  for (const [k, v] of Object.entries(data)) {
    if (!["qps", "latency_ms", "online_sessions", "unique_players", "active_sessions_5m", "active_window_seconds", "online_players", "connections", "room_count", "pool_size"].includes(k)) {
      extra[k] = v;
    }
  }

  try {
    await dbPool.query(
      `INSERT INTO metrics_archive (service_name, bucket_time, qps, latency_ms, online_value, extra)
       VALUES ($1, $2, $3, $4, $5, $6::jsonb)
       ON CONFLICT (service_name, bucket_time)
       DO UPDATE SET qps = EXCLUDED.qps,
                     latency_ms = EXCLUDED.latency_ms,
                     online_value = EXCLUDED.online_value,
                     extra = EXCLUDED.extra`,
      [serviceName, bucketTime, qps, latencyMs, onlineValue, JSON.stringify(extra)]
    );
  } catch (error) {
    console.error(`[archive] failed to insert record for ${serviceName}:${bucketTime}:`, error);
  }
}
