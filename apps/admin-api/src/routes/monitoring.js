/**
 * Monitoring Routes
 *
 * 提供服务状态和 metrics 数据查询接口
 */

import { Router } from "express";
import { badRequest } from "../http-errors.js";
import { runArchiveTask } from "../services/archive.js";

/**
 * 服务列表及字段配置
 */
const SERVICE_CONFIGS = {
  "auth-http": { onlineField: "unique_players" },
  "game-server": { onlineField: "online_players" },
  "game-proxy": { onlineField: "connections" },
  "chat-server": { onlineField: "online_players" },
  "match-service": { onlineField: "pool_size" },
  "mail-service": { onlineField: null },
  "admin-api": { onlineField: null }
};

const SERVICE_NAMES = Object.keys(SERVICE_CONFIGS);
const HEARTBEAT_TTL = 30; // 30 秒内无心跳视为离线

/**
 * window 参数对应的时间范围（秒）
 */
const WINDOW_SECONDS = {
  "1m": 60,
  "5m": 300,
  "15m": 900,
  "1h": 3600
};

function parseMetricInt(value) {
  return parseInt(value || "0", 10);
}

function getOnlineValue(serviceName, data) {
  const onlineField = SERVICE_CONFIGS[serviceName]?.onlineField;
  if (!onlineField) {
    return 0;
  }

  return parseMetricInt(data[onlineField]);
}

/**
 * 创建监控路由
 *
 * @param {import("ioredis").Redis} redis
 * @param {import("mysql2/promise").Pool} mysqlPool
 */
export function createMonitoringRoutes(redis, mysqlPool) {
  const router = Router();

  /**
   * GET /api/admin/monitoring/services
   * 获取所有服务状态（总览页用）
   */
  router.get("/services", async (_req, res) => {
    const services = [];

    for (const serviceName of SERVICE_NAMES) {
      const heartbeatKey = `heartbeat:${serviceName}`;
      const lastHeartbeat = await redis.get(heartbeatKey);

      let status = "offline";
      let qps = 0;
      let latencyMs = 0;
      let onlineValue = 0;
      let metricsData = {};

      // 检查心跳，确定在线状态
      if (lastHeartbeat) {
        const heartbeatAge = (Date.now() / 1000) - parseInt(lastHeartbeat, 10);
        if (heartbeatAge <= HEARTBEAT_TTL) {
          status = "online";
        }
      }

      // 获取最新 metrics
      if (status === "online") {
        const latestMetrics = await getLatestMetrics(redis, serviceName);
        if (latestMetrics) {
          qps = parseMetricInt(latestMetrics.qps);
          latencyMs = parseMetricInt(latestMetrics.latency_ms);
          onlineValue = getOnlineValue(serviceName, latestMetrics);
          metricsData = latestMetrics;
        }
      }

      services.push({
        name: serviceName,
        status,
        qps,
        latency_ms: latencyMs,
        online_value: onlineValue,
        last_heartbeat: lastHeartbeat ? parseInt(lastHeartbeat, 10) * 1000 : null,
        ...metricsData
      });
    }

    return res.json({ services });
  });

  /**
   * GET /api/admin/monitoring/services/:name/metrics
   * 获取指定服务历史 metrics（图表用）
   */
  router.get("/services/:name/metrics", async (req, res) => {
    const { name } = req.params;
    const { window = "5m" } = req.query;

    if (!SERVICE_NAMES.includes(name)) {
      return badRequest(res, "INVALID_SERVICE", `Unknown service: ${name}`);
    }

    const windowSeconds = WINDOW_SECONDS[window];
    if (!windowSeconds) {
      return badRequest(res, "INVALID_WINDOW", `window must be one of: ${Object.keys(WINDOW_SECONDS).join(", ")}`);
    }

    // 计算时间范围
    const now = Math.floor(Date.now() / 1000);
    const fromBucket = now - windowSeconds;

    // 获取历史数据
    const points = await getHistoricalMetrics(redis, name, fromBucket, now);

    return res.json({
      service: name,
      window,
      points
    });
  });

  /**
   * POST /api/admin/monitoring/archive
   * 手动触发归档任务
   */
  router.post("/archive", async (_req, res) => {
    try {
      const result = await runArchiveTask(redis, mysqlPool);
      return res.json({
        ok: true,
        archived: result.archived,
        duration_ms: result.duration_ms
      });
    } catch (error) {
      console.error("[monitoring] archive error:", error);
      return res.status(500).json({
        ok: false,
        error: "ARCHIVE_FAILED",
        message: error.message
      });
    }
  });

  return router;
}

/**
 * 获取服务的最新 metrics 数据
 */
async function getLatestMetrics(redis, serviceName) {
  // 扫描获取最新的 bucket
  let cursor = "0";
  let latestKey = null;
  let latestBucket = 0;

  do {
    const [nextCursor, keys] = await redis.scan(cursor, "MATCH", `metrics:${serviceName}:*`, "COUNT", 100);
    cursor = nextCursor;

    for (const key of keys) {
      const parts = key.split(":");
      const bucket = parseInt(parts[parts.length - 1], 10);
      if (bucket > latestBucket) {
        latestBucket = bucket;
        latestKey = key;
      }
    }
  } while (cursor !== "0");

  if (!latestKey) return null;

  return redis.hgetall(latestKey);
}

/**
 * 获取服务的历史 metrics 数据
 */
async function getHistoricalMetrics(redis, serviceName, fromBucket, toBucket) {
  const points = [];
  let cursor = "0";

  do {
    const [nextCursor, keys] = await redis.scan(cursor, "MATCH", `metrics:${serviceName}:*`, "COUNT", 100);
    cursor = nextCursor;

    for (const key of keys) {
      const parts = key.split(":");
      const bucket = parseInt(parts[parts.length - 1], 10);

      if (bucket >= fromBucket && bucket <= toBucket) {
        const data = await redis.hgetall(key);
        if (data && Object.keys(data).length > 0) {
          points.push({
            timestamp: bucket,
            qps: parseMetricInt(data.qps),
            latency_ms: parseMetricInt(data.latency_ms),
            online_value: getOnlineValue(serviceName, data),
            online_sessions: parseMetricInt(data.online_sessions),
            unique_players: parseMetricInt(data.unique_players),
            active_sessions_5m: parseMetricInt(data.active_sessions_5m)
          });
        }
      }
    }
  } while (cursor !== "0");

  // 按时间戳排序
  points.sort((a, b) => a.timestamp - b.timestamp);

  return points;
}
