/**
 * Archive Service - 将超期 metrics 数据从 Redis 归档到 MySQL
 */

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
  "mail-service",
  "admin-api"
];

function parseMetricInt(value) {
  return parseInt(value || "0", 10);
}

function getArchiveOnlineValue(serviceName, data) {
  if (serviceName === "auth-http") {
    return parseMetricInt(data.unique_players);
  }

  return parseMetricInt(data.online_players || data.connections || data.pool_size || data.online_sessions);
}

/**
 * 执行归档任务
 * 将 7 天前 ~ 8 天前的 Redis metrics 数据迁移到 MySQL
 *
 * @param {import("ioredis").Redis} redis
 * @param {import("mysql2/promise").Pool} mysqlPool
 * @returns {Promise<{archived: number, duration_ms: number}>}
 */
export async function runArchiveTask(redis, mysqlPool) {
  const startTime = Date.now();

  // 计算归档时间范围（7天前 ~ 8天前）
  const now = Math.floor(Date.now() / 1000);
  const sevenDaysAgo = now - 7 * 24 * 3600;
  const eightDaysAgo = now - 8 * 24 * 3600;

  let totalArchived = 0;

  for (const serviceName of SERVICE_NAMES) {
    const archived = await archiveServiceMetrics(redis, mysqlPool, serviceName, eightDaysAgo, sevenDaysAgo);
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
async function archiveServiceMetrics(redis, mysqlPool, serviceName, fromBucket, toBucket) {
  let archived = 0;
  let cursor = "0";

  do {
    // 扫描该服务的 metrics keys
    const pattern = `metrics:${serviceName}:*`;
    const [nextCursor, keys] = await redis.scan(cursor, "MATCH", pattern, "COUNT", 100);
    cursor = nextCursor;

    for (const key of keys) {
      // 提取 bucket 时间戳
      const parts = key.split(":");
      const bucketStr = parts[parts.length - 1];
      const bucket = parseInt(bucketStr, 10);

      // 只处理 7 天前 ~ 8 天前的数据
      if (bucket >= fromBucket && bucket < toBucket) {
        const data = await redis.hgetall(key);
        if (data && Object.keys(data).length > 0) {
          // 写入 MySQL
          await insertArchiveRecord(mysqlPool, serviceName, bucket, data);
          // 从 Redis 删除
          await redis.del(key);
          archived++;
        }
      }
    }
  } while (cursor !== "0");

  return archived;
}

/**
 * 插入归档记录到 MySQL
 */
async function insertArchiveRecord(mysqlPool, serviceName, bucketTime, data) {
  const qps = parseMetricInt(data.qps);
  const latencyMs = parseMetricInt(data.latency_ms);
  const onlineValue = getArchiveOnlineValue(serviceName, data);

  // 收集扩展字段
  const extra = {};
  for (const [k, v] of Object.entries(data)) {
    if (!["qps", "latency_ms", "online_sessions", "unique_players", "active_sessions_5m", "active_window_seconds", "online_players", "connections", "room_count", "pool_size"].includes(k)) {
      extra[k] = v;
    }
  }

  try {
    await mysqlPool.execute(
      `INSERT INTO metrics_archive (service_name, bucket_time, qps, latency_ms, online_value, extra)
       VALUES (?, ?, ?, ?, ?, ?)
       ON DUPLICATE KEY UPDATE qps = VALUES(qps), latency_ms = VALUES(latency_ms), online_value = VALUES(online_value), extra = VALUES(extra)`,
      [serviceName, bucketTime, qps, latencyMs, onlineValue, JSON.stringify(extra)]
    );
  } catch (error) {
    console.error(`[archive] failed to insert record for ${serviceName}:${bucketTime}:`, error);
  }
}
