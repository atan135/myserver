import { Inject, Injectable } from "@nestjs/common";

import { badRequest } from "../common/http-exception.js";
import { ApiHttpException } from "../common/http-exception.js";
import { runArchiveTask } from "../services/archive.js";
import { ADMIN_MYSQL_POOL, ADMIN_REDIS } from "../tokens.js";
import {
  aggregateMetricRecordsDetailed,
  buildMetricPoint,
  buildInstanceMetricPoint,
  getOnlineValue,
  parseMetricInt,
  parseMetricHeartbeatKey,
  parseMetricKey
} from "./metrics-aggregation.js";

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
      let instances = [];

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
          onlineValue = getOnlineValue(serviceName, latestMetrics, SERVICE_CONFIGS);
          instances = await this.buildServiceInstances(serviceName, latestMetrics.instances || []);
          const { instances: _rawInstances, ...latestMetricFields } = latestMetrics;
          metricsData = latestMetricFields;
        }
      }

      services.push({
        name: serviceName,
        status,
        ...metricsData,
        qps,
        latency_ms: latencyMs,
        online_value: onlineValue,
        last_heartbeat: lastHeartbeat ? parseInt(lastHeartbeat, 10) * 1000 : null,
        instances
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

  private async getLatestMetrics(serviceName: string): Promise<any | null> {
    let cursor = "0";
    let latestBucket = 0;
    const latestKeys = [];

    do {
      const [nextCursor, keys] = await this.redis.scan(cursor, "MATCH", `metrics:${serviceName}:*`, "COUNT", 100);
      cursor = nextCursor;

      for (const key of keys) {
        const parsed = parseMetricKey(serviceName, key);
        if (!parsed) {
          continue;
        }

        if (parsed.bucket > latestBucket) {
          latestKeys.length = 0;
          latestBucket = parsed.bucket;
          latestKeys.push({ key, ...parsed });
        } else if (parsed.bucket === latestBucket) {
          latestKeys.push({ key, ...parsed });
        }
      }
    } while (cursor !== "0");

    if (latestKeys.length === 0) return null;

    const records = [];
    for (const item of latestKeys) {
      const data = await this.redis.hgetall(item.key);
      if (data && Object.keys(data).length > 0) {
        records.push({ ...item, data });
      }
    }

    if (records.length === 0) return null;

    const aggregated = aggregateMetricRecordsDetailed(records);
    return {
      ...aggregated.data,
      instances: aggregated.instances
    };
  }

  private async getHistoricalMetrics(serviceName: string, fromBucket: number, toBucket: number): Promise<any[]> {
    const recordsByBucket = new Map<number, any[]>();
    let cursor = "0";

    do {
      const [nextCursor, keys] = await this.redis.scan(cursor, "MATCH", `metrics:${serviceName}:*`, "COUNT", 100);
      cursor = nextCursor;

      for (const key of keys) {
        const parsed = parseMetricKey(serviceName, key);
        if (!parsed) {
          continue;
        }
        const bucket = parsed.bucket;

        if (bucket >= fromBucket && bucket <= toBucket) {
          const data = await this.redis.hgetall(key);
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

    const points = [];
    for (const [bucket, records] of recordsByBucket.entries()) {
      const aggregated = aggregateMetricRecordsDetailed(records);
      points.push(buildMetricPoint(serviceName, aggregated.data, SERVICE_CONFIGS, bucket, aggregated.instances));
    }

    points.sort((a, b) => a.timestamp - b.timestamp);

    return points;
  }

  private async buildServiceInstances(serviceName: string, instances: any[]): Promise<any[]> {
    const heartbeats = await this.getInstanceHeartbeats(serviceName);

    return instances.map((instance) => {
      const point = buildInstanceMetricPoint(serviceName, instance, SERVICE_CONFIGS);
      const heartbeat = heartbeats.get(point.instance_id);
      let status = heartbeat ? "offline" : "unknown";

      if (heartbeat) {
        const heartbeatAge = Date.now() / 1000 - heartbeat;
        if (heartbeatAge <= HEARTBEAT_TTL) {
          status = "online";
        }
      }

      return {
        ...point,
        status,
        last_heartbeat: heartbeat ? heartbeat * 1000 : null
      };
    });
  }

  private async getInstanceHeartbeats(serviceName: string): Promise<Map<string, number>> {
    const heartbeats = new Map<string, number>();
    let cursor = "0";

    do {
      const [nextCursor, keys] = await this.redis.scan(
        cursor,
        "MATCH",
        `metrics:heartbeat:${serviceName}:*`,
        "COUNT",
        100
      );
      cursor = nextCursor;

      for (const key of keys) {
        const parsed = parseMetricHeartbeatKey(serviceName, key);
        if (!parsed) {
          continue;
        }

        const value = await this.redis.get(key);
        const timestamp = parseMetricInt(value);
        if (timestamp > 0) {
          heartbeats.set(parsed.instanceId, timestamp);
        }
      }
    } while (cursor !== "0");

    return heartbeats;
  }
}
