import { Inject, Injectable } from "@nestjs/common";
import http from "node:http";

import {
  discoveryLogContext,
  getDiscoveryMetricsSnapshot,
  normalizeServiceInstance,
  recordDiscoveryMetric,
  registryHeartbeatKey,
  registryInstanceScanPattern
} from "../../../../packages/service-registry/node/registry-schema.js";
import { badRequest } from "../common/http-exception.js";
import { ApiHttpException } from "../common/http-exception.js";
import { log } from "../logger.js";
import { runArchiveTask } from "../services/archive.js";
import { ADMIN_CONFIG, ADMIN_DB_POOL, ADMIN_REDIS } from "../tokens.js";
import {
  discoverGameProxyAdminEndpoints,
  discoverGameServerAdminEndpoints,
  discoverServiceInstances
} from "../registry-client.js";
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

const EXPECTED_REGISTRY_ENDPOINTS: Record<string, Array<{ name: string; protocol: string; visibility: string }>> = {
  "auth-http": [
    { name: "http", protocol: "http", visibility: "public" },
    { name: "internal", protocol: "http", visibility: "internal" }
  ],
  "game-server": [
    { name: "client", protocol: "tcp", visibility: "internal" },
    { name: "admin", protocol: "tcp", visibility: "admin" },
    { name: "internal", protocol: "local_socket", visibility: "local" },
    { name: "proxy-local", protocol: "local_socket", visibility: "local" }
  ],
  "game-proxy": [
    { name: "client", protocol: "kcp", visibility: "public" },
    { name: "client-tcp-fallback", protocol: "tcp", visibility: "public" },
    { name: "admin", protocol: "http", visibility: "admin" }
  ],
  "chat-server": [{ name: "tcp", protocol: "tcp", visibility: "internal" }],
  "match-service": [{ name: "grpc", protocol: "grpc", visibility: "internal" }],
  "announce-service": [{ name: "http", protocol: "http", visibility: "internal" }],
  "mail-service": [{ name: "http", protocol: "http", visibility: "internal" }],
  "admin-api": [{ name: "http", protocol: "http", visibility: "admin" }]
};

const DISCOVERY_ALERT_SEVERITY_RANK: Record<string, number> = {
  info: 0,
  warning: 1,
  critical: 2
};

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
      let adminEndpoints: any[] = [];

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

      if (serviceName === "game-server") {
        adminEndpoints = await this.getGameServerAdminEndpoints();
        instances = mergeGameServerAdminEndpoints(instances, adminEndpoints);
      } else if (serviceName === "game-proxy") {
        adminEndpoints = await this.getGameProxyAdminEndpoints();
        instances = mergeGameServerAdminEndpoints(instances, adminEndpoints);
      }

      services.push({
        name: serviceName,
        status,
        ...metricsData,
        qps,
        latency_ms: latencyMs,
        online_value: onlineValue,
        last_heartbeat: lastHeartbeat ? parseInt(lastHeartbeat, 10) * 1000 : null,
        instances,
        endpoints: ["game-server", "game-proxy"].includes(serviceName) ? adminEndpoints : []
      });
    }

    return { services };
  }

  async registry() {
    const checkedAt = Date.now();
    const services = [];
    const alerts = [];

    for (const serviceName of SERVICE_NAMES) {
      const instances = await discoverServiceInstances(this.redis, serviceName, this.config.registryKeyPrefix || "");
      const schemaParseFailures = await this.findRegistrySchemaParseFailures(serviceName);
      const normalizedInstances = [];

      for (const instance of instances) {
        const heartbeat = await this.getRegistryHeartbeatStatus(serviceName, instance.id);
        normalizedInstances.push({
          instance_id: instance.id,
          service: instance.name,
          healthy: instance.healthy !== false,
          status: instance.healthy !== false && heartbeat.status === "alive" ? "healthy" : "missing",
          registered_at: instance.registered_at || null,
          last_registered_at: instance.registered_at || null,
          heartbeat_ttl_seconds: heartbeat.ttl_seconds,
          heartbeat_status: heartbeat.status,
          tags: Array.isArray(instance.tags) ? instance.tags : [],
          metadata: instance.metadata || {},
          weight: instance.weight,
          endpoints: Array.isArray(instance.endpoints)
            ? instance.endpoints.map((endpoint: any) => ({
                name: endpoint.name,
                protocol: endpoint.protocol,
                host: endpoint.host,
                port: endpoint.port,
                socket: endpoint.socket,
                visibility: endpoint.visibility,
                healthy: endpoint.healthy !== false,
                metadata: endpoint.metadata || {}
              }))
            : []
        });
      }

      const healthyInstances = normalizedInstances.filter((instance) => instance.healthy && instance.heartbeat_status === "alive");
      const service: any = {
        name: serviceName,
        instance_count: normalizedInstances.length,
        healthy_instance_count: healthyInstances.length,
        status: normalizedInstances.length === 0 ? "missing" : healthyInstances.length > 0 ? "healthy" : "unhealthy",
        instances: normalizedInstances,
        alerts: []
      };
      service.alerts = buildServiceDiscoveryAlerts(service, schemaParseFailures);
      alerts.push(...service.alerts);
      services.push(service);
    }

    alerts.push(...buildDiscoveryMetricAlerts());
    const dedupedAlerts = dedupeDiscoveryAlerts(alerts);
    const alertLevel = aggregateDiscoveryAlertLevel(dedupedAlerts);

    return {
      ok: true,
      checked_at: checkedAt,
      alert_level: alertLevel,
      alert_message: discoveryAlertMessage(alertLevel, dedupedAlerts),
      alerts: dedupedAlerts,
      services
    };
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
      const upstreams = await this.fetchProxyRollouts();
      return buildAggregatedRolloutDrainSnapshot(upstreams, checkedAt);
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
          host: this.config.localDiscoveryFallbackEnabled ? this.config.gameProxyAdminHost : null,
          port: this.config.localDiscoveryFallbackEnabled ? this.config.gameProxyAdminPort : null
        },
        instances: []
      };
    }
  }

  private async fetchProxyRollouts(): Promise<any[]> {
    const timeoutMs = Number.parseInt(String(this.config.gameProxyAdminRequestTimeoutMs || 3000), 10);
    const maxResponseBytes = Number.parseInt(String(this.config.gameProxyAdminMaxResponseBytes || 1048576), 10);
    const token = this.config.gameProxyAdminReadToken || this.config.gameProxyAdminToken;

    if (!token) {
      const error: any = new Error("GAME_PROXY_ADMIN_TOKEN is required");
      error.code = "GAME_PROXY_ADMIN_TOKEN_REQUIRED";
      throw error;
    }

    const endpoints = await this.getGameProxyAdminEndpoints();
    if (endpoints.length === 0) {
      const error: any = new Error("game-proxy admin endpoint not found in service registry");
      error.code = "GAME_PROXY_ADMIN_ENDPOINT_NOT_FOUND";
      throw error;
    }

    const results = [];
    for (const endpoint of endpoints) {
      try {
        const body = await httpGetJsonBody({
          host: endpoint.host,
          port: endpoint.port,
          path: "/rollout",
          token,
          timeoutMs,
          maxResponseBytes
        });

        try {
          results.push({ endpoint, upstream: JSON.parse(body) });
        } catch (error: any) {
          const parseError: any = new Error(`invalid proxy admin rollout JSON: ${error.message}`);
          parseError.code = "PROXY_ADMIN_INVALID_JSON";
          throw parseError;
        }
      } catch (error: any) {
        results.push({
          endpoint,
          error: error.code || "PROXY_ADMIN_UNAVAILABLE",
          message: error.message || "failed to query game-proxy admin rollout status"
        });
      }
    }
    return results;
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

  private async getGameServerAdminEndpoints(): Promise<any[]> {
    if (!this.config.registryDiscoveryEnabled) {
      if (this.config.registryDiscoveryRequired || !this.config.localDiscoveryFallbackEnabled) {
        logDiscovery("warn", "registry.discovery_fallback_forbidden", {
          serviceName: "game-server",
          endpointName: "admin",
          source: "registry",
          reason: this.config.registryDiscoveryRequired ? "registry_disabled" : "fallback_forbidden"
        });
        return [];
      }

      logDiscovery("warn", "registry.discovery_fallback", {
        serviceName: "game-server",
        endpointName: "admin",
        instanceId: "local-fallback",
        source: "fallback",
        reason: "fallback_used"
      });
      return [
        {
          service: "game-server",
          instanceId: "local-fallback",
          instance_id: "local-fallback",
          endpointName: "admin",
          endpoint_name: "admin",
          protocol: "tcp",
          host: this.config.gameServerAdminHost,
          port: this.config.gameServerAdminPort,
          healthy: true,
          fallback: true,
          source: "fallback",
          reason: "fallback_used"
        }
      ];
    }

    return discoverGameServerAdminEndpoints(this.redis, this.config);
  }

  private async getGameProxyAdminEndpoints(): Promise<any[]> {
    if (!this.config.registryDiscoveryEnabled) {
      if (this.config.registryDiscoveryRequired || !this.config.localDiscoveryFallbackEnabled) {
        logDiscovery("warn", "registry.discovery_fallback_forbidden", {
          serviceName: "game-proxy",
          endpointName: "admin",
          source: "registry",
          reason: this.config.registryDiscoveryRequired ? "registry_disabled" : "fallback_forbidden"
        });
        const error: any = new Error("Required registry discovery failed: REGISTRY_ENABLED=false");
        error.code = "SERVICE_DISCOVERY_REQUIRED";
        throw error;
      }

      logDiscovery("warn", "registry.discovery_fallback", {
        serviceName: "game-proxy",
        endpointName: "admin",
        instanceId: "local-fallback",
        source: "fallback",
        reason: "fallback_used"
      });
      return [
        {
          service: "game-proxy",
          instanceId: "local-fallback",
          instance_id: "local-fallback",
          endpointName: "admin",
          endpoint_name: "admin",
          protocol: "http",
          host: this.config.gameProxyAdminHost || "127.0.0.1",
          port: Number.parseInt(String(this.config.gameProxyAdminPort || 7101), 10),
          healthy: true,
          fallback: true,
          source: "fallback",
          reason: "fallback_used"
        }
      ];
    }

    return discoverGameProxyAdminEndpoints(this.redis, this.config);
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

  private async getRegistryHeartbeatStatus(serviceName: string, instanceId: string): Promise<any> {
    if (typeof this.redis.ttl !== "function") {
      return {
        ttl_seconds: null,
        status: "unknown"
      };
    }

    try {
      const key = registryHeartbeatKey(this.config.registryKeyPrefix || "", serviceName, instanceId);
      const ttl = await this.redis.ttl(key);
      const ttlSeconds = Number.isFinite(Number(ttl)) ? Number(ttl) : null;
      let status = "unknown";

      if (ttlSeconds === null) {
        status = "unknown";
      } else if (ttlSeconds > 0) {
        status = "alive";
      } else if (ttlSeconds === -1) {
        status = "no_expire";
      } else {
        status = "missing";
      }

      return {
        ttl_seconds: ttlSeconds && ttlSeconds > 0 ? ttlSeconds : ttlSeconds,
        status
      };
    } catch {
      return {
        ttl_seconds: null,
        status: "unknown"
      };
    }
  }

  private async findRegistrySchemaParseFailures(serviceName: string): Promise<any[]> {
    const failures = [];

    try {
      const keys = await this.scanRedisKeys(registryInstanceScanPattern(this.config.registryKeyPrefix || "", serviceName));
      for (const key of keys.sort()) {
        const instanceId = key.split(":").at(-1) || "";

        try {
          const data = await this.redis.hget(key, "data");
          if (!data) {
            continue;
          }
          const normalized = normalizeServiceInstance(JSON.parse(data));
          if (!normalized) {
            failures.push({
              service: serviceName,
              instance_id: instanceId,
              key,
              reason: "invalid_schema"
            });
          }
        } catch (error: any) {
          failures.push({
            service: serviceName,
            instance_id: instanceId,
            key,
            reason: "parse_failed",
            error: error.message || String(error)
          });
        }
      }
    } catch (error: any) {
      failures.push({
        service: serviceName,
        instance_id: "",
        reason: "scan_failed",
        error: error.message || String(error)
      });
    }

    return failures;
  }

  private async scanRedisKeys(pattern: string): Promise<string[]> {
    const keys = [];
    let cursor = "0";

    do {
      const [nextCursor, batch] = await this.redis.scan(cursor, "MATCH", pattern, "COUNT", 100);
      cursor = nextCursor;
      keys.push(...batch);
    } while (cursor !== "0");

    return keys;
  }
}

function buildServiceDiscoveryAlerts(service: any, schemaParseFailures: any[]): any[] {
  const alerts = [];

  if (service.instance_count === 0 || service.healthy_instance_count === 0) {
    alerts.push({
      kind: "no_healthy_instance",
      service: service.name,
      endpoint: "",
      instance_id: "",
      severity: "critical",
      message: service.instance_count === 0
        ? `${service.name} 没有注册实例`
        : `${service.name} 没有健康实例`
    });
  }

  for (const instance of service.instances) {
    if (!Array.isArray(instance.endpoints) || instance.endpoints.length === 0) {
      alerts.push({
        kind: "endpoint_missing",
        service: service.name,
        endpoint: "",
        instance_id: instance.instance_id,
        severity: "critical",
        message: `${service.name}/${instance.instance_id} 未注册 endpoint`
      });
    }
  }

  for (const expected of EXPECTED_REGISTRY_ENDPOINTS[service.name] || []) {
    const hasEndpoint = service.instances.some((instance: any) =>
      Array.isArray(instance.endpoints) &&
      instance.endpoints.some((endpoint: any) =>
        endpoint.name === expected.name &&
        endpoint.protocol === expected.protocol &&
        endpoint.visibility === expected.visibility &&
        endpoint.healthy !== false
      )
    );

    if (service.instance_count > 0 && !hasEndpoint) {
      alerts.push({
        kind: "endpoint_missing",
        service: service.name,
        endpoint: expected.name,
        instance_id: "",
        severity: "warning",
        message: `${service.name}.${expected.name} endpoint 缺失或不健康`
      });
    }
  }

  for (const failure of schemaParseFailures) {
    alerts.push({
      kind: "schema_parse_failed",
      service: failure.service || service.name,
      endpoint: "",
      instance_id: failure.instance_id || "",
      severity: "warning",
      message: `${failure.service || service.name} registry schema 解析失败${failure.instance_id ? `：${failure.instance_id}` : ""}`,
      reason: failure.reason,
      error: failure.error || ""
    });
  }

  return alerts;
}

function buildDiscoveryMetricAlerts(): any[] {
  try {
    return getDiscoveryMetricsSnapshot()
      .filter((metric: any) => ["discovery_failure", "fallback_used", "no_healthy_instance", "endpoint_missing"].includes(metric.kind))
      .map((metric: any) => ({
        kind: metric.kind,
        service: metric.service || "",
        endpoint: metric.endpoint || "",
        instance_id: "",
        severity: discoveryMetricSeverity(metric),
        message: discoveryMetricMessage(metric),
        source: metric.source || "registry",
        reason: metric.reason || "",
        count: metric.count || 0
      }));
  } catch {
    return [];
  }
}

function discoveryMetricSeverity(metric: any): string {
  if (metric.kind === "fallback_used") {
    return "warning";
  }
  if (metric.kind === "endpoint_missing") {
    return "warning";
  }
  return "critical";
}

function discoveryMetricMessage(metric: any): string {
  const service = metric.service || "unknown-service";
  const endpoint = metric.endpoint ? `.${metric.endpoint}` : "";
  if (metric.kind === "fallback_used") {
    return `${service}${endpoint} 使用本地 fallback`;
  }
  if (metric.kind === "endpoint_missing") {
    return `${service}${endpoint} endpoint 发现缺失`;
  }
  if (metric.kind === "no_healthy_instance") {
    return `${service} 未发现健康实例`;
  }
  return `${service}${endpoint} 服务发现失败`;
}

function dedupeDiscoveryAlerts(alerts: any[]): any[] {
  const byKey = new Map<string, any>();

  for (const alert of alerts) {
    const normalized = {
      kind: String(alert.kind || "discovery_failure"),
      service: String(alert.service || ""),
      endpoint: String(alert.endpoint || ""),
      instance_id: String(alert.instance_id || ""),
      severity: ["info", "warning", "critical"].includes(alert.severity) ? alert.severity : "warning",
      message: String(alert.message || "服务发现告警"),
      ...(alert.source ? { source: alert.source } : {}),
      ...(alert.reason ? { reason: alert.reason } : {}),
      ...(alert.count ? { count: alert.count } : {}),
      ...(alert.error ? { error: alert.error } : {})
    };
    const key = [
      normalized.kind,
      normalized.service,
      normalized.endpoint,
      normalized.instance_id,
    ].join("|");
    const existing = byKey.get(key);

    if (!existing || DISCOVERY_ALERT_SEVERITY_RANK[normalized.severity] > DISCOVERY_ALERT_SEVERITY_RANK[existing.severity]) {
      byKey.set(key, normalized);
    }
  }

  return [...byKey.values()].sort((a, b) =>
    DISCOVERY_ALERT_SEVERITY_RANK[b.severity] - DISCOVERY_ALERT_SEVERITY_RANK[a.severity] ||
    a.service.localeCompare(b.service) ||
    a.kind.localeCompare(b.kind) ||
    a.endpoint.localeCompare(b.endpoint) ||
    a.instance_id.localeCompare(b.instance_id)
  );
}

function aggregateDiscoveryAlertLevel(alerts: any[]): string {
  if (alerts.some((alert) => alert.severity === "critical")) {
    return "critical";
  }
  if (alerts.some((alert) => alert.severity === "warning")) {
    return "warning";
  }
  return "info";
}

function discoveryAlertMessage(level: string, alerts: any[]): string {
  if (alerts.length === 0) {
    return "服务发现正常";
  }

  const criticalCount = alerts.filter((alert) => alert.severity === "critical").length;
  const warningCount = alerts.filter((alert) => alert.severity === "warning").length;
  if (level === "critical") {
    return `服务发现存在 ${criticalCount} 个严重告警${warningCount ? `，${warningCount} 个警告` : ""}`;
  }
  return `服务发现存在 ${warningCount} 个警告`;
}

function mergeGameServerAdminEndpoints(instances: any[], endpoints: any[]): any[] {
  const byId = new Map<string, any>();
  for (const instance of instances) {
    byId.set(instance.instance_id, { ...instance, endpoints: [] });
  }

  for (const endpoint of endpoints) {
    const existing = byId.get(endpoint.instance_id) || {
      instance_id: endpoint.instance_id,
      status: endpoint.healthy ? "online" : "offline",
      last_heartbeat: null,
      endpoints: []
    };
    existing.endpoints = [...(existing.endpoints || []), endpoint];
    byId.set(endpoint.instance_id, existing);
  }

  return [...byId.values()].sort((a, b) => String(a.instance_id).localeCompare(String(b.instance_id)));
}

function buildAggregatedRolloutDrainSnapshot(results: any[], checkedAt: number) {
  const instances = results.map((result) => {
    const endpoint = result.endpoint || {};
    if (result.error) {
      return {
        instance_id: endpoint.instance_id || endpoint.instanceId || "",
        endpoint,
        ok: false,
        status: "error",
        alert_level: "critical",
        alert_message: "控制面不可达",
        error: result.error,
        message: result.message,
        active: false,
        drained: false,
        rollout: null,
        drain_evaluation: null,
        blockers: emptyRolloutBlockers()
      };
    }

    const snapshot = buildRolloutDrainSnapshot(result.upstream, checkedAt);
    return {
      instance_id: endpoint.instance_id || endpoint.instanceId || "",
      endpoint,
      ...snapshot
    };
  });

  const failed = instances.filter((instance) => instance.ok === false);
  const active = instances.some((instance) => instance.active);
  const interrupted = instances.some((instance) => instance.status === "interrupted");
  const blocked = instances.some((instance) => instance.status === "blocked");
  const drained = active && failed.length === 0 && instances.filter((instance) => instance.active).every((instance) => instance.drained);
  const blockers = mergeRolloutBlockers(instances.map((instance) => instance.blockers || emptyRolloutBlockers()));
  const rollout = pickAggregateRollout(instances);

  let ok = failed.length === 0;
  let status = "empty";
  let alertLevel = "info";
  let alertMessage = "当前没有进行中的 rollout";

  if (failed.length > 0) {
    status = "error";
    alertLevel = "critical";
    alertMessage = `${failed.length}/${instances.length} 个 game-proxy 控制面不可达`;
  } else if (interrupted) {
    status = "interrupted";
    alertLevel = "critical";
    alertMessage = "至少一个 game-proxy rollout 已中断，需要人工复查";
  } else if (blocked) {
    status = "blocked";
    alertLevel = "warning";
    alertMessage = "至少一个 game-proxy 仍有旧服房间/玩家/迁移中阻塞";
  } else if (drained) {
    status = "drained";
    alertLevel = "warning";
    alertMessage = "所有 active game-proxy 已排空可收尾";
  }

  return {
    ok,
    source: "game-proxy",
    checked_at: checkedAt,
    updated_at: checkedAt,
    active,
    status,
    alert_level: alertLevel,
    alert_message: alertMessage,
    drained,
    rollout,
    drain_evaluation: null,
    blockers,
    instances
  };
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

function mergeRolloutBlockers(blockersList: any[]) {
  const merged = emptyRolloutBlockers();
  for (const blockers of blockersList) {
    merged.blocked_room_count += readNumber(blockers, "blocked_room_count", "blockedRoomCount");
    merged.blocked_player_count += readNumber(blockers, "blocked_player_count", "blockedPlayerCount");
    merged.stale_room_route_count += readNumber(blockers, "stale_room_route_count", "staleRoomRouteCount");
    merged.stale_player_route_count += readNumber(blockers, "stale_player_route_count", "stalePlayerRouteCount");
    merged.blocked_room_samples.push(...readStringSamples(blockers, "blocked_room_samples", "blockedRoomSamples"));
    merged.blocked_player_samples.push(...readStringSamples(blockers, "blocked_player_samples", "blockedPlayerSamples"));
  }
  merged.blocked_room_samples = merged.blocked_room_samples.slice(0, DEFAULT_ROLLOUT_DRAIN_SAMPLES_LIMIT);
  merged.blocked_player_samples = merged.blocked_player_samples.slice(0, DEFAULT_ROLLOUT_DRAIN_SAMPLES_LIMIT);
  return merged;
}

function pickAggregateRollout(instances: any[]) {
  const rollouts = instances.map((instance) => instance.rollout).filter(Boolean);
  if (rollouts.length === 0) {
    return null;
  }

  const first = rollouts[0];
  const same = rollouts.every(
    (rollout) =>
      rollout.epoch === first.epoch &&
      rollout.old_server === first.old_server &&
      rollout.new_server === first.new_server &&
      rollout.state === first.state
  );

  if (same) {
    return first;
  }

  const startedAtValues = rollouts.map((rollout) => readNumber(rollout, "started_at")).filter((value) => value > 0);
  return {
    epoch: "mixed",
    old_server: "mixed",
    new_server: "mixed",
    state: "Mixed",
    started_at: startedAtValues.length > 0 ? Math.min(...startedAtValues) : 0
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

function logDiscovery(level: string, event: string, context: Record<string, unknown>) {
  if (!context.__discoveryMetricRecorded) {
    recordDiscoveryMetric(context);
  }

  log(level, event, discoveryLogContext(context));
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
