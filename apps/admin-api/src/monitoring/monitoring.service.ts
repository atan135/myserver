import { Inject, Injectable } from "@nestjs/common";
import http from "node:http";

import { badRequest } from "../common/http-exception.js";
import { ApiHttpException } from "../common/http-exception.js";
import { runArchiveTask } from "../services/archive.js";
import { ADMIN_CONFIG, ADMIN_DB_POOL, ADMIN_REDIS } from "../tokens.js";
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
const DEFAULT_ROLLOUT_DRAIN_SAMPLES_LIMIT = 5;

const WINDOW_SECONDS: Record<string, number> = {
  "1m": 60,
  "5m": 300,
  "15m": 900,
  "1h": 3600
};

@Injectable()
export class MonitoringService {
  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_REDIS) private readonly redis: any,
    @Inject(ADMIN_DB_POOL) private readonly dbPool: any
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
      const result = await runArchiveTask(this.redis, this.dbPool);
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

  async rolloutDrain() {
    const checkedAt = Date.now();

    try {
      const upstream = await this.fetchProxyRollout();
      return buildRolloutDrainSnapshot(upstream, checkedAt);
    } catch (error: any) {
      return {
        ok: false,
        source: "game-proxy",
        checked_at: checkedAt,
        updated_at: checkedAt,
        active: false,
        status: "error",
        alert_level: "critical",
        alert_message: "控制面不可达",
        drained: false,
        error: error.code || "PROXY_ADMIN_UNAVAILABLE",
        message: error.message || "failed to query game-proxy admin rollout status",
        rollout: null,
        drain_evaluation: null,
        blockers: {
          blocked_room_count: 0,
          blocked_player_count: 0,
          stale_room_route_count: 0,
          stale_player_route_count: 0,
          blocked_room_samples: [],
          blocked_player_samples: []
        },
        upstream: {
          host: this.config.gameProxyAdminHost,
          port: this.config.gameProxyAdminPort
        }
      };
    }
  }

  private async fetchProxyRollout(): Promise<any> {
    const host = this.config.gameProxyAdminHost || "127.0.0.1";
    const port = Number.parseInt(String(this.config.gameProxyAdminPort || 7101), 10);
    const timeoutMs = Number.parseInt(String(this.config.gameProxyAdminRequestTimeoutMs || 3000), 10);
    const maxResponseBytes = Number.parseInt(String(this.config.gameProxyAdminMaxResponseBytes || 1048576), 10);
    const token = this.config.gameProxyAdminReadToken || this.config.gameProxyAdminToken;

    if (!token) {
      const error: any = new Error("GAME_PROXY_ADMIN_TOKEN is required");
      error.code = "GAME_PROXY_ADMIN_TOKEN_REQUIRED";
      throw error;
    }

    const body = await httpGetJsonBody({
      host,
      port,
      path: "/rollout",
      token,
      timeoutMs,
      maxResponseBytes
    });

    try {
      return JSON.parse(body);
    } catch (error: any) {
      const parseError: any = new Error(`invalid proxy admin rollout JSON: ${error.message}`);
      parseError.code = "PROXY_ADMIN_INVALID_JSON";
      throw parseError;
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

function buildRolloutDrainSnapshot(upstream: any, checkedAt: number) {
  if (!upstream || upstream.ok === false) {
    return {
      ok: false,
      source: "game-proxy",
      checked_at: checkedAt,
      updated_at: checkedAt,
      active: false,
      status: "error",
      alert_level: "critical",
      alert_message: "控制面返回异常",
      drained: false,
      error: upstream?.error || "PROXY_ROLLOUT_STATUS_NOT_OK",
      message: upstream?.message || "game-proxy admin rollout status returned ok=false",
      rollout: null,
      drain_evaluation: upstream?.drain_evaluation || null,
      blockers: emptyRolloutBlockers()
    };
  }

  const session = upstream.rollout_session || upstream.rolloutSession || null;
  const evaluation = upstream.drain_evaluation || upstream.drainEvaluation || {};
  const upstreamStatus = readString(evaluation, "status") || (session ? "Blocked" : "NoActiveRollout");
  const active = Boolean(session) && upstreamStatus !== "NoActiveRollout";
  const rollout = session
    ? {
        epoch: readString(session, "rollout_epoch", "rolloutEpoch"),
        old_server: readString(session, "old_server_id", "oldServerId"),
        new_server: readString(session, "new_server_id", "newServerId"),
        state: readString(session, "state") || "Active",
        started_at: readNumber(session, "started_at_ms", "startedAtMs")
      }
    : null;

  const blockers = {
    blocked_room_count: readNumber(evaluation, "blocked_room_count", "blockedRoomCount"),
    blocked_player_count: readNumber(evaluation, "blocked_player_count", "blockedPlayerCount"),
    stale_room_route_count: readNumber(evaluation, "stale_room_route_count", "staleRoomRouteCount"),
    stale_player_route_count: readNumber(evaluation, "stale_player_route_count", "stalePlayerRouteCount"),
    blocked_room_samples: readStringSamples(evaluation, "blocked_room_samples", "blockedRoomSamples"),
    blocked_player_samples: readStringSamples(evaluation, "blocked_player_samples", "blockedPlayerSamples")
  };

  const drained = active && upstreamStatus === "Drained";
  const interrupted = active && rollout?.state === "Interrupted";
  const blocked = active && !drained;
  let status = "empty";
  let alertLevel = "info";
  let alertMessage = "当前没有进行中的 rollout";

  if (interrupted) {
    status = "interrupted";
    alertLevel = "critical";
    alertMessage = "rollout 已中断，需要人工复查";
  } else if (drained) {
    status = "drained";
    alertLevel = "warning";
    alertMessage = "已排空可收尾";
  } else if (blocked) {
    status = "blocked";
    alertLevel = "warning";
    alertMessage = "仍有旧服房间/玩家/迁移中阻塞";
  }

  return {
    ok: true,
    source: "game-proxy",
    checked_at: checkedAt,
    updated_at: checkedAt,
    active,
    status,
    alert_level: alertLevel,
    alert_message: alertMessage,
    drained,
    rollout,
    drain_evaluation: evaluation,
    blockers
  };
}

function emptyRolloutBlockers() {
  return {
    blocked_room_count: 0,
    blocked_player_count: 0,
    stale_room_route_count: 0,
    stale_player_route_count: 0,
    blocked_room_samples: [],
    blocked_player_samples: []
  };
}

function readValue(source: any, ...keys: string[]) {
  for (const key of keys) {
    if (source && source[key] !== undefined && source[key] !== null) {
      return source[key];
    }
  }
  return undefined;
}

function readString(source: any, ...keys: string[]) {
  const value = readValue(source, ...keys);
  return typeof value === "string" ? value : "";
}

function readNumber(source: any, ...keys: string[]) {
  const value = readValue(source, ...keys);
  const parsed = Number.parseInt(String(value ?? "0"), 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : 0;
}

function readStringSamples(source: any, ...keys: string[]) {
  const value = readValue(source, ...keys);
  if (!Array.isArray(value)) {
    return [];
  }

  return value
    .filter((item) => typeof item === "string" && item.length > 0)
    .slice(0, DEFAULT_ROLLOUT_DRAIN_SAMPLES_LIMIT);
}

function httpGetJsonBody(options: {
  host: string;
  port: number;
  path: string;
  token: string;
  timeoutMs: number;
  maxResponseBytes: number;
}): Promise<string> {
  return new Promise((resolve, reject) => {
    let settled = false;
    let req: http.ClientRequest;

    const fail = (code: string, message: string) => {
      if (settled) {
        return;
      }
      settled = true;
      req?.destroy();
      const error: any = new Error(message);
      error.code = code;
      reject(error);
    };

    req = http.request(
      {
        hostname: options.host,
        port: options.port,
        path: options.path,
        method: "GET",
        headers: {
          Authorization: `Bearer ${options.token}`,
          Accept: "application/json"
        }
      },
      (res) => {
        const chunks: Buffer[] = [];
        let totalBytes = 0;

        res.on("data", (chunk: Buffer) => {
          totalBytes += chunk.length;
          if (totalBytes > options.maxResponseBytes) {
            fail(
              "PROXY_ADMIN_RESPONSE_TOO_LARGE",
              `proxy admin response exceeds ${options.maxResponseBytes} bytes`
            );
            return;
          }
          chunks.push(chunk);
        });

        res.on("end", () => {
          if (settled) {
            return;
          }

          const body = Buffer.concat(chunks).toString("utf8");
          const statusCode = res.statusCode || 0;
          if (statusCode < 200 || statusCode >= 300) {
            const error: any = new Error(`proxy admin returned HTTP ${statusCode}`);
            error.code = "PROXY_ADMIN_HTTP_ERROR";
            error.statusCode = statusCode;
            error.body = body.slice(0, 256);
            settled = true;
            reject(error);
            return;
          }

          settled = true;
          resolve(body);
        });
      }
    );

    req.setTimeout(options.timeoutMs, () => {
      fail("PROXY_ADMIN_TIMEOUT", `proxy admin request timed out after ${options.timeoutMs}ms`);
    });

    req.on("error", (error: any) => {
      if (settled) {
        return;
      }
      const wrapped: any = new Error(`proxy admin request failed: ${error.message}`);
      wrapped.code = error.code || "PROXY_ADMIN_REQUEST_FAILED";
      settled = true;
      reject(wrapped);
    });

    req.end();
  });
}
