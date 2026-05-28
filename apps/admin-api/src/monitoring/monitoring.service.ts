import { Inject, Injectable } from "@nestjs/common";

import { badRequest } from "../common/http-exception.js";
import { ApiHttpException } from "../common/http-exception.js";
import { runArchiveTask } from "../services/archive.js";
import { ADMIN_MYSQL_POOL, ADMIN_REDIS } from "../tokens.js";

const SERVICE_CONFIGS: Record<string, { onlineField: string | null }> = {
  "auth-http": { onlineField: "unique_players" },
  "game-server": { onlineField: "online_players" },
  "game-proxy": { onlineField: "connections" },
  "chat-server": { onlineField: "online_players" },
  "match-service": { onlineField: "pool_size" },
  "announce-service": { onlineField: null },
  "mail-service": { onlineField: null },
  "admin-api": { onlineField: null }
};

const SERVICE_NAMES = Object.keys(SERVICE_CONFIGS);
const HEARTBEAT_TTL = 30;

const WINDOW_SECONDS: Record<string, number> = {
  "1m": 60,
  "5m": 300,
  "15m": 900,
  "1h": 3600
};

function parseMetricInt(value: any) {
  return parseInt(value || "0", 10);
}

function getOnlineValue(serviceName: string, data: Record<string, any>) {
  const onlineField = SERVICE_CONFIGS[serviceName]?.onlineField;
  if (!onlineField) {
    return 0;
  }

  return parseMetricInt(data[onlineField]);
}

@Injectable()
export class MonitoringService {
  constructor(
    @Inject(ADMIN_REDIS) private readonly redis: any,
    @Inject(ADMIN_MYSQL_POOL) private readonly mysqlPool: any
  ) {}

  async services() {
    const services = [];

    for (const serviceName of SERVICE_NAMES) {
      const heartbeatKey = `metrics:heartbeat:${serviceName}`;
      const lastHeartbeat = await this.redis.get(heartbeatKey);

      let status = "offline";
      let qps = 0;
      let latencyMs = 0;
      let onlineValue = 0;
      let metricsData = {};

      if (lastHeartbeat) {
        const heartbeatAge = Date.now() / 1000 - parseInt(lastHeartbeat, 10);
        if (heartbeatAge <= HEARTBEAT_TTL) {
          status = "online";
        }
      }

      if (status === "online") {
        const latestMetrics = await this.getLatestMetrics(serviceName);
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

    return { services };
  }

  async metrics(name: string, window = "5m") {
    if (!SERVICE_NAMES.includes(name)) {
      throw badRequest("INVALID_SERVICE", `Unknown service: ${name}`);
    }

    const windowSeconds = WINDOW_SECONDS[window];
    if (!windowSeconds) {
      throw badRequest("INVALID_WINDOW", `window must be one of: ${Object.keys(WINDOW_SECONDS).join(", ")}`);
    }

    const now = Math.floor(Date.now() / 1000);
    const fromBucket = now - windowSeconds;
    const points = await this.getHistoricalMetrics(name, fromBucket, now);

    return {
      service: name,
      window,
      points
    };
  }

  async archive() {
    try {
      const result = await runArchiveTask(this.redis, this.mysqlPool);
      return {
        ok: true,
        archived: result.archived,
        duration_ms: result.duration_ms
      };
    } catch (error: any) {
      console.error("[monitoring] archive error:", error);
      throw new ApiHttpException(500, {
        ok: false,
        error: "ARCHIVE_FAILED",
        message: error.message
      });
    }
  }

  private async getLatestMetrics(serviceName: string) {
    let cursor = "0";
    let latestKey = null;
    let latestBucket = 0;

    do {
      const [nextCursor, keys] = await this.redis.scan(cursor, "MATCH", `metrics:${serviceName}:*`, "COUNT", 100);
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

    return this.redis.hgetall(latestKey);
  }

  private async getHistoricalMetrics(serviceName: string, fromBucket: number, toBucket: number) {
    const points = [];
    let cursor = "0";

    do {
      const [nextCursor, keys] = await this.redis.scan(cursor, "MATCH", `metrics:${serviceName}:*`, "COUNT", 100);
      cursor = nextCursor;

      for (const key of keys) {
        const parts = key.split(":");
        const bucket = parseInt(parts[parts.length - 1], 10);

        if (bucket >= fromBucket && bucket <= toBucket) {
          const data = await this.redis.hgetall(key);
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

    points.sort((a, b) => a.timestamp - b.timestamp);

    return points;
  }
}
