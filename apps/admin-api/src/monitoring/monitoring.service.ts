import { Inject, Injectable } from "@nestjs/common";
import http from "node:http";

import {
  discoveryLogContext,
  getDiscoveryMetricsSnapshot,
  getRegistryLifecycleMetricsSnapshot,
  normalizeServiceInstance,
  recordDiscoveryMetric,
  registryHeartbeatKey,
  registryInstanceIndexKey,
  REGISTRY_HEARTBEAT_TTL_SECONDS,
  REGISTRY_MAX_INSTANCES_PER_SERVICE
} from "../../../../packages/service-registry/node/registry-schema.js";
import { badRequest } from "../common/http-exception.js";
import { ApiHttpException } from "../common/http-exception.js";
import { log } from "../logger.js";
import { runArchiveTask } from "../services/archive.js";
import { ADMIN_CONFIG, ADMIN_DB_POOL, ADMIN_REDIS } from "../tokens.js";
import {
  discoverGameProxyAdminEndpoints,
  discoverGameServerAdminEndpoints
} from "../registry-client.js";
import {
  aggregateMetricRecordsDetailed,
  buildMetricPoint,
  buildInstanceMetricPoint,
  getOnlineValue,
  parseMetricInt
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

const REGISTRY_LIFECYCLE_METRIC_ALERTS = [
  { kind: "register_failed", field: "register_failed_total" },
  { kind: "heartbeat_failed", field: "heartbeat_failed_total" },
  { kind: "deregister_failed", field: "deregister_failed_total" }
];

const WINDOW_SECONDS: Record<string, number> = {
  "1m": 60,
  "5m": 300,
  "15m": 900,
  "1h": 3600
};

const METRICS_BUCKET_SECONDS = 5;
const MAX_METRICS_INSTANCES = 64;
const MAX_HISTORY_BUCKETS_BY_WINDOW: Record<string, number> = {
  "1m": 12,
  "5m": 60,
  "15m": 180,
  "1h": 720
};

@Injectable()
export class MonitoringService {
  private readonly snapshotCache = new Map<string, { value: any; checkedAt: number; expiresAt: number }>();
  private readonly snapshotFlights = new Map<string, Promise<{ value: any; checkedAt: number }>>();

  constructor(
    @Inject(ADMIN_CONFIG) private readonly config: any,
    @Inject(ADMIN_REDIS) private readonly redis: any,
    @Inject(ADMIN_DB_POOL) private readonly dbPool: any
  ) {}

  async services() {
    return this.readSnapshot("services", async () => this.buildServicesSnapshot());
  }

  async registry() {
    return this.readSnapshot("registry", async () => this.buildRegistrySnapshot());
  }

  async metrics(name: string, window = "5m") {
    if (!SERVICE_NAMES.includes(name)) {
      throw badRequest("INVALID_SERVICE", `Unknown service: ${name}`);
    }

    const windowSeconds = WINDOW_SECONDS[window];
    if (!windowSeconds) {
      throw badRequest("INVALID_WINDOW", `window must be one of: ${Object.keys(WINDOW_SECONDS).join(", ")}`);
    }

    return this.readSnapshot(`metrics:${name}:${window}`, async () => {
      const now = currentMetricsBucket();
      const fromBucket = now - windowSeconds + METRICS_BUCKET_SECONDS;
      const history = await this.getHistoricalMetrics(name, fromBucket, now, MAX_HISTORY_BUCKETS_BY_WINDOW[window]);
      if (history.unavailable) {
        throw monitoringDataUnavailable();
      }
      return {
        service: name,
        window,
        points: history.points,
        partial: history.errors.length > 0,
        errors: history.errors,
        sources: [sourceStatus("metrics-v2", history.errors)]
      };
    });
  }

  async archive() {
    try {
      const result = await runArchiveTask(this.redis, this.dbPool, this.monitoringOptions());
      return {
        ok: true,
        archived: result.archived,
        failed: result.failed,
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

  private monitoringOptions() {
    return {
      metricsKeyPrefix: String(this.config.metricsKeyPrefix ?? this.config.redisKeyPrefix ?? ""),
      registryKeyPrefix: String(this.config.registryKeyPrefix ?? this.config.redisKeyPrefix ?? ""),
      snapshotCacheTtlMs: boundedNumber(this.config.monitoringSnapshotCacheTtlMs, 3000, 0, 30000),
      serviceReadConcurrency: boundedNumber(this.config.monitoringServiceReadConcurrency, 4, 1, 16),
      redisTimeoutMs: boundedNumber(this.config.monitoringRedisTimeoutMs, 1000, 100, 10000),
      metricsLatestTtlSeconds: boundedNumber(this.config.metricsLatestTtlSeconds, 180, 21, 86400),
      metricsMaxInstancesPerService: Math.min(
        MAX_METRICS_INSTANCES,
        boundedNumber(this.config.metricsMaxInstancesPerService, MAX_METRICS_INSTANCES, 1, MAX_METRICS_INSTANCES)
      ),
      metricsHistoryRetentionSeconds: boundedNumber(this.config.metricsHistoryRetentionSeconds, 4500, 3600, 86400),
      metricsArchiveAfterSeconds: boundedNumber(this.config.metricsArchiveAfterSeconds, 3600, 60, 86399),
      metricsArchiveBatchSize: boundedNumber(this.config.metricsArchiveBatchSize, 128, 1, 720)
    };
  }

  private async readSnapshot(key: string, loader: () => Promise<any>): Promise<any> {
    const options = this.monitoringOptions();
    const cacheKey = [key, options.metricsKeyPrefix, options.registryKeyPrefix].join("\u0000");
    const now = Date.now();
    const cached = this.snapshotCache.get(cacheKey);
    if (cached && cached.expiresAt > now) {
      return decorateSnapshot(cached.value, cached.checkedAt, true);
    }

    let flight = this.snapshotFlights.get(cacheKey);
    if (!flight) {
      flight = Promise.resolve()
        .then(loader)
        .then((value) => {
          const checkedAt = Date.now();
          if (options.snapshotCacheTtlMs > 0) {
            this.snapshotCache.set(cacheKey, {
              value,
              checkedAt,
              expiresAt: checkedAt + options.snapshotCacheTtlMs
            });
          }
          return { value, checkedAt };
        })
        .finally(() => this.snapshotFlights.delete(cacheKey));
      this.snapshotFlights.set(cacheKey, flight);
    }

    const snapshot = await flight;
    return decorateSnapshot(snapshot.value, snapshot.checkedAt, false);
  }

  private async buildServicesSnapshot(): Promise<any> {
    const observations = await mapWithConcurrency(
      SERVICE_NAMES,
      this.monitoringOptions().serviceReadConcurrency,
      (serviceName) => this.readServiceReadModel(serviceName)
    );
    if (observations.every((observation) => observation.unavailable)) {
      throw monitoringDataUnavailable();
    }

    const errors = observations.flatMap((observation) => observation.errors);
    return {
      services: observations.map((observation) => {
        const latest = observation.metrics.aggregation;
        const metricInstances = this.buildServiceInstances(observation.name, observation.metrics.records);
        const adminEndpoints = buildRegistryAdminEndpoints(observation.name, observation.registry.instances);
        const instances = ["game-server", "game-proxy"].includes(observation.name)
          ? mergeGameServerAdminEndpoints(metricInstances, adminEndpoints)
          : metricInstances;
        const registryActive = observation.registry.instances.some((item: any) => item.heartbeat.status === "alive");
        const metricsAvailable = observation.metrics.records.length > 0;
        const lastReportedAt = Math.max(
          0,
          ...observation.metrics.records.map((record: any) => parseMetricInt(record.data?._reported_at))
        );

        return {
          name: observation.name,
          status: metricsAvailable || registryActive ? "online" : observation.unavailable ? "unknown" : "offline",
          ...latest,
          qps: parseMetricInt(latest.qps),
          latency_ms: parseMetricInt(latest.latency_ms),
          online_value: getOnlineValue(observation.name, latest, SERVICE_CONFIGS),
          last_heartbeat: lastReportedAt > 0 ? lastReportedAt * 1000 : null,
          instances,
          endpoints: adminEndpoints,
          partial: observation.errors.length > 0,
          errors: observation.errors,
          sources: [
            sourceStatus("registry-index-v1", observation.registry.errors),
            sourceStatus("metrics-v2", observation.metrics.errors)
          ]
        };
      }),
      partial: errors.length > 0,
      errors,
      sources: buildSourcesFromObservations(observations)
    };
  }

  private async buildRegistrySnapshot(): Promise<any> {
    const checkedAt = Date.now();
    const services = [];
    const capacitySummaries = [];
    const alerts = [];
    const observations = await mapWithConcurrency(
      SERVICE_NAMES,
      this.monitoringOptions().serviceReadConcurrency,
      (serviceName) => this.readServiceReadModel(serviceName)
    );
    if (observations.every((observation) => observation.unavailable)) {
      throw monitoringDataUnavailable();
    }

    for (const observation of observations) {
      const capacity = buildRegistryCapacitySummary(observation.metrics.aggregation);
      capacitySummaries.push(capacity);
      const normalizedInstances = observation.registry.instances.map((entry: any) => {
        const { instance, heartbeat } = entry;
        const metricRecord = observation.metrics.byInstance.get(instance.id);
        const metricsState = metricRecord ? metricFreshnessState(metricRecord.data) : "missing";
        const registryState = heartbeat.status === "alive"
          ? "healthy"
          : heartbeat.status === "unknown" ? "unknown" : "unhealthy";
        return {
          instance_id: instance.id,
          service: instance.name,
          healthy: instance.healthy !== false,
          status: registryState === "healthy" && metricsState === "fresh" ? "healthy"
            : registryState === "healthy" ? "degraded"
            : registryState === "unhealthy" ? "unhealthy" : "unknown",
          registry_state: registryState,
          metrics_state: metricsState,
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
        };
      });

      const healthyInstances = normalizedInstances.filter((instance) => instance.registry_state === "healthy");
      const service: any = {
        name: observation.name,
        instance_count: normalizedInstances.length,
        healthy_instance_count: healthyInstances.length,
        status: normalizedInstances.length === 0 ? "missing" : healthyInstances.length > 0 ? "healthy" : "unhealthy",
        capacity,
        instances: normalizedInstances,
        partial: observation.errors.length > 0,
        errors: observation.errors,
        sources: [
          sourceStatus("registry-index-v1", observation.registry.errors),
          sourceStatus("metrics-v2", observation.metrics.errors)
        ],
        alerts: []
      };
      service.alerts = buildServiceDiscoveryAlerts(service, observation.registry.schemaFailures);
      alerts.push(...service.alerts);
      services.push(service);
    }

    alerts.push(...buildDiscoveryMetricAlerts());
    alerts.push(...buildRegistryLifecycleMetricAlerts());
    alerts.push(...buildRegistryLifecycleMetricAlertsFromRecords(observations));
    const dedupedAlerts = dedupeDiscoveryAlerts(alerts);
    const alertLevel = aggregateDiscoveryAlertLevel(dedupedAlerts);
    const errors = observations.flatMap((observation) => observation.errors);

    return {
      ok: true,
      checked_at: checkedAt,
      alert_level: alertLevel,
      alert_message: discoveryAlertMessage(alertLevel, dedupedAlerts),
      capacity: aggregateRegistryCapacitySummaries(capacitySummaries),
      alerts: dedupedAlerts,
      services,
      partial: errors.length > 0,
      errors,
      sources: buildSourcesFromObservations(observations)
    };
  }

  private async readServiceReadModel(serviceName: string): Promise<any> {
    const [registry, metrics] = await Promise.all([
      this.readRegistryInstances(serviceName),
      this.readLatestMetricRecords(serviceName)
    ]);
    return {
      name: serviceName,
      registry,
      metrics,
      errors: [...registry.errors, ...metrics.errors],
      unavailable: registry.unavailable && metrics.unavailable
    };
  }

  private async readRegistryInstances(serviceName: string): Promise<any> {
    const options = this.monitoringOptions();
    const errors = [];
    let instanceIds: string[];
    try {
      instanceIds = await this.redisCall(() => this.redis.zrangebyscore(
        registryInstanceIndexKey(options.registryKeyPrefix, serviceName),
        Math.floor(Date.now() / 1000) - REGISTRY_HEARTBEAT_TTL_SECONDS,
        "+inf",
        "LIMIT",
        0,
        Math.min(REGISTRY_MAX_INSTANCES_PER_SERVICE, options.metricsMaxInstancesPerService)
      ));
    } catch {
      return {
        instances: [],
        schemaFailures: [],
        errors: [monitoringError("registry-index-v1", serviceName, "REGISTRY_INDEX_READ_FAILED")],
        unavailable: true
      };
    }

    const ids = [...new Set(Array.isArray(instanceIds) ? instanceIds : [])]
      .filter((instanceId) => typeof instanceId === "string" && /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(instanceId))
      .slice(0, Math.min(REGISTRY_MAX_INSTANCES_PER_SERVICE, options.metricsMaxInstancesPerService));
    if (ids.length === 0) {
      return { instances: [], schemaFailures: [], errors, unavailable: false };
    }

    const pipeline = this.redis.pipeline();
    for (const instanceId of ids) {
      pipeline.hget(`${options.registryKeyPrefix}service:${serviceName}:instances:${instanceId}`, "data");
      pipeline.exists(registryHeartbeatKey(options.registryKeyPrefix, serviceName, instanceId));
      if (typeof this.redis.ttl === "function") {
        pipeline.ttl(registryHeartbeatKey(options.registryKeyPrefix, serviceName, instanceId));
      }
    }

    let values: any[];
    try {
      values = await this.redisCall(() => pipeline.exec());
    } catch {
      return {
        instances: [],
        schemaFailures: [],
        errors: [monitoringError("registry-index-v1", serviceName, "REGISTRY_INSTANCE_BATCH_READ_FAILED")],
        unavailable: true
      };
    }

    const instances = [];
    const schemaFailures = [];
    const width = typeof this.redis.ttl === "function" ? 3 : 2;
    for (let offset = 0; offset < ids.length; offset += 1) {
      const instanceId = ids[offset];
      const dataResult = pipelineValue(values[offset * width]);
      const existsResult = pipelineValue(values[offset * width + 1]);
      const ttlResult: { value: any; error: any } = width === 3
        ? pipelineValue(values[offset * width + 2])
        : { value: null, error: null };
      if (dataResult.error || existsResult.error || ttlResult.error) {
        errors.push(monitoringError("registry-index-v1", serviceName, "REGISTRY_INSTANCE_READ_FAILED", instanceId));
        continue;
      }
      if (!dataResult.value) {
        errors.push(monitoringError("registry-index-v1", serviceName, "REGISTRY_INDEX_PAYLOAD_MISSING", instanceId));
        continue;
      }

      try {
        const instance = normalizeServiceInstance(JSON.parse(dataResult.value));
        if (!instance || instance.id !== instanceId || instance.name !== serviceName) {
          throw new Error("registry instance identity mismatch");
        }
        const ttl = Number(ttlResult.value);
        const heartbeat = typeof this.redis.ttl !== "function"
          ? { ttl_seconds: null, status: "unknown" }
          : Number.isFinite(ttl) && ttl > 0
            ? { ttl_seconds: ttl, status: "alive" }
            : Number.isFinite(ttl) && ttl === -1
              ? { ttl_seconds: ttl, status: "no_expire" }
              : { ttl_seconds: Number.isFinite(ttl) ? ttl : null, status: existsResult.value ? "unknown" : "missing" };
        instances.push({ instance, heartbeat });
      } catch {
        const failure = { service: serviceName, instance_id: instanceId, reason: "parse_failed" };
        schemaFailures.push(failure);
        errors.push(monitoringError("registry-index-v1", serviceName, "REGISTRY_SCHEMA_PARSE_FAILED", instanceId));
      }
    }
    return { instances, schemaFailures, errors, unavailable: false };
  }

  private async readLatestMetricRecords(serviceName: string): Promise<any> {
    const options = this.monitoringOptions();
    let instanceIds: string[];
    try {
      instanceIds = await this.redisCall(() => this.redis.zrangebyscore(
        `${options.metricsKeyPrefix}metrics:v2:latest-index:${serviceName}`,
        Math.floor(Date.now() / 1000) - options.metricsLatestTtlSeconds,
        "+inf",
        "LIMIT",
        0,
        options.metricsMaxInstancesPerService
      ));
    } catch {
      return emptyMetricRead(serviceName, "METRICS_LATEST_INDEX_READ_FAILED", true);
    }

    const ids = [...new Set(Array.isArray(instanceIds) ? instanceIds : [])]
      .filter((instanceId) => typeof instanceId === "string" && /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(instanceId))
      .slice(0, options.metricsMaxInstancesPerService);
    if (ids.length === 0) {
      return emptyMetricRead(serviceName);
    }

    const pipeline = this.redis.pipeline();
    for (const instanceId of ids) {
      pipeline.hgetall(`${options.metricsKeyPrefix}metrics:v2:latest:${serviceName}:${instanceId}`);
    }
    let values: any[];
    try {
      values = await this.redisCall(() => pipeline.exec());
    } catch {
      return emptyMetricRead(serviceName, "METRICS_LATEST_BATCH_READ_FAILED", true);
    }

    const errors = [];
    const records = [];
    const byInstance = new Map<string, any>();
    for (let offset = 0; offset < ids.length; offset += 1) {
      const instanceId = ids[offset];
      const result = pipelineValue(values[offset]);
      if (result.error) {
        errors.push(monitoringError("metrics-v2", serviceName, "METRICS_LATEST_INSTANCE_READ_FAILED", instanceId));
        continue;
      }
      const data = result.value && typeof result.value === "object" ? result.value : {};
      if (Object.keys(data).length === 0) {
        errors.push(monitoringError("metrics-v2", serviceName, "METRICS_LATEST_INDEX_PAYLOAD_MISSING", instanceId));
        continue;
      }
      if (data._schema !== "metrics-v2" || data._service !== serviceName || data._instance_id !== instanceId) {
        errors.push(monitoringError("metrics-v2", serviceName, "METRICS_LATEST_SCHEMA_INVALID", instanceId));
        continue;
      }
      const record = { instanceId, data, legacy: false };
      records.push(record);
      byInstance.set(instanceId, record);
    }

    const aggregation = aggregateMetricRecordsDetailed(records.map(metricRecordForAggregation));
    return { records, byInstance, aggregation: aggregation.data, errors, unavailable: false };
  }

  private async getHistoricalMetrics(serviceName: string, fromBucket: number, toBucket: number, maxBuckets: number): Promise<any> {
    const options = this.monitoringOptions();
    const historyIndexKey = `${options.metricsKeyPrefix}metrics:v2:history-index:${serviceName}`;
    let bucketMembers: string[];
    try {
      bucketMembers = await this.redisCall(() => this.redis.zrangebyscore(
        historyIndexKey,
        fromBucket,
        toBucket,
        "LIMIT",
        0,
        maxBuckets
      ));
    } catch {
      return { points: [], errors: [monitoringError("metrics-v2", serviceName, "METRICS_HISTORY_INDEX_READ_FAILED")], unavailable: true };
    }

    const buckets = [...new Set((Array.isArray(bucketMembers) ? bucketMembers : []).map((value) => Number(value)))]
      .filter((bucket) => Number.isSafeInteger(bucket) && bucket >= fromBucket && bucket <= toBucket)
      .sort((left, right) => left - right)
      .slice(0, maxBuckets);
    if (buckets.length === 0) {
      return { points: [], errors: [], unavailable: false };
    }

    const pipeline = this.redis.pipeline();
    for (const bucket of buckets) {
      pipeline.hgetall(`${options.metricsKeyPrefix}metrics:v2:history:${serviceName}:${bucket}`);
    }
    let values: any[];
    try {
      values = await this.redisCall(() => pipeline.exec());
    } catch {
      return { points: [], errors: [monitoringError("metrics-v2", serviceName, "METRICS_HISTORY_BATCH_READ_FAILED")], unavailable: true };
    }

    const errors = [];
    const points = [];
    for (let offset = 0; offset < buckets.length; offset += 1) {
      const bucket = buckets[offset];
      const result = pipelineValue(values[offset]);
      if (result.error) {
        errors.push(monitoringError("metrics-v2", serviceName, "METRICS_HISTORY_BUCKET_READ_FAILED", String(bucket)));
        continue;
      }
      const hash = result.value && typeof result.value === "object" ? result.value : {};
      const records = [];
      for (const [instanceId, encoded] of Object.entries(hash)) {
        try {
          const data = JSON.parse(String(encoded));
          if (data._schema !== "metrics-v2" || data._service !== serviceName || data._instance_id !== instanceId || Number(data._bucket) !== bucket) {
            throw new Error("invalid metrics v2 history record");
          }
          records.push({ instanceId, data, legacy: false });
        } catch {
          errors.push(monitoringError("metrics-v2", serviceName, "METRICS_HISTORY_RECORD_INVALID", String(instanceId)));
        }
      }
      if (records.length > 0) {
        const aggregation = aggregateMetricRecordsDetailed(records.map(metricRecordForAggregation));
        points.push(buildMetricPoint(serviceName, aggregation.data, SERVICE_CONFIGS, bucket, aggregation.instances));
      }
    }
    return { points: points.sort((left, right) => left.timestamp - right.timestamp), errors, unavailable: false };
  }

  private buildServiceInstances(serviceName: string, records: any[]): any[] {
    return records.map((record) => {
      const point = buildInstanceMetricPoint(serviceName, metricRecordForAggregation(record), SERVICE_CONFIGS);
      const freshness = metricFreshnessState(record.data);
      const reportedAt = parseMetricInt(record.data?._reported_at);
      return {
        ...point,
        status: freshness === "fresh" || freshness === "delayed" ? "online" : "offline",
        metrics_state: freshness,
        last_heartbeat: reportedAt > 0 ? reportedAt * 1000 : null
      };
    });
  }

  private async redisCall<T>(operation: () => Promise<T>): Promise<T> {
    const timeoutMs = this.monitoringOptions().redisTimeoutMs;
    let timer: NodeJS.Timeout | null = null;
    try {
      return await Promise.race([
        Promise.resolve().then(operation),
        new Promise<T>((_resolve, reject) => {
          timer = setTimeout(() => {
            const error: any = new Error("monitoring Redis request timed out");
            error.code = "MONITORING_REDIS_TIMEOUT";
            reject(error);
          }, timeoutMs);
          timer.unref?.();
        })
      ]);
    } finally {
      if (timer) clearTimeout(timer);
    }
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

}

function boundedNumber(value: unknown, fallback: number, minimum: number, maximum: number): number {
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) && parsed >= minimum && parsed <= maximum ? parsed : fallback;
}

function currentMetricsBucket(nowSeconds = Math.floor(Date.now() / 1000)): number {
  return Math.floor(nowSeconds / METRICS_BUCKET_SECONDS) * METRICS_BUCKET_SECONDS;
}

function decorateSnapshot(value: any, checkedAt: number, cached: boolean): any {
  return {
    ...value,
    checked_at: checkedAt,
    data_age_ms: Math.max(0, Date.now() - checkedAt),
    cached,
    partial: value.partial === true,
    errors: Array.isArray(value.errors) ? value.errors : [],
    sources: Array.isArray(value.sources) ? value.sources : []
  };
}

function monitoringDataUnavailable(): ApiHttpException {
  return new ApiHttpException(503, {
    ok: false,
    error_code: "MONITORING_DATA_UNAVAILABLE",
    message: "monitoring Redis read model is unavailable"
  });
}

function monitoringError(source: string, service: string, errorCode: string, instanceId = ""): any {
  return {
    source,
    service,
    ...(instanceId ? { instance_id: instanceId } : {}),
    error_code: errorCode
  };
}

function sourceStatus(name: string, errors: any[]): any {
  return {
    name,
    status: errors.length > 0 ? "degraded" : "healthy",
    error_count: errors.length
  };
}

function buildSourcesFromObservations(observations: any[]): any[] {
  const registryErrors = observations.flatMap((observation) => observation.registry.errors);
  const metricsErrors = observations.flatMap((observation) => observation.metrics.errors);
  return [
    sourceStatus("registry-index-v1", registryErrors),
    sourceStatus("metrics-v2", metricsErrors)
  ];
}

async function mapWithConcurrency<T, R>(values: T[], limit: number, mapper: (value: T) => Promise<R>): Promise<R[]> {
  const output = new Array<R>(values.length);
  let next = 0;
  const workerCount = Math.min(values.length, Math.max(1, limit));
  await Promise.all(Array.from({ length: workerCount }, async () => {
    while (true) {
      const index = next;
      next += 1;
      if (index >= values.length) return;
      output[index] = await mapper(values[index]);
    }
  }));
  return output;
}

function pipelineValue(result: any): { value: any; error: any } {
  if (Array.isArray(result)) {
    return { error: result[0] || null, value: result[1] };
  }
  return { error: null, value: result };
}

function emptyMetricRead(serviceName: string, errorCode = "", unavailable = false): any {
  const errors = errorCode ? [monitoringError("metrics-v2", serviceName, errorCode)] : [];
  return {
    records: [],
    byInstance: new Map(),
    aggregation: {},
    errors,
    unavailable
  };
}

function metricRecordForAggregation(record: any): any {
  const data = Object.fromEntries(
    Object.entries(record?.data || {}).filter(([key]) => !key.startsWith("_"))
  );
  return {
    instanceId: record?.instanceId || record?.data?._instance_id || "",
    legacy: false,
    data
  };
}

function metricFreshnessState(data: any): "fresh" | "delayed" | "stale" | "missing" {
  const now = Math.floor(Date.now() / 1000);
  const reportedAt = parseMetricInt(data?._reported_at);
  const receivedAt = parseMetricInt(data?._received_at);
  if (reportedAt <= 0) return "missing";
  const reportAge = Math.max(0, now - reportedAt);
  const transportLag = receivedAt > 0 ? Math.max(0, receivedAt - reportedAt) : Number.POSITIVE_INFINITY;
  if (reportAge <= 30 && transportLag <= 10) return "fresh";
  if (reportAge <= 120) return "delayed";
  if (reportAge <= 180) return "stale";
  return "missing";
}

function buildRegistryAdminEndpoints(serviceName: string, entries: any[]): any[] {
  if (serviceName !== "game-server" && serviceName !== "game-proxy") {
    return [];
  }
  const protocol = serviceName === "game-server" ? "tcp" : "http";
  return entries.flatMap(({ instance, heartbeat }) =>
    (Array.isArray(instance.endpoints) ? instance.endpoints : [])
      .filter((endpoint: any) =>
        endpoint.name === "admin" &&
        endpoint.visibility === "admin" &&
        endpoint.protocol === protocol &&
        endpoint.healthy !== false
      )
      .map((endpoint: any) => ({
        service: serviceName,
        instanceId: instance.id,
        instance_id: instance.id,
        endpointName: endpoint.name,
        endpoint_name: endpoint.name,
        protocol: endpoint.protocol,
        host: endpoint.host,
        port: endpoint.port,
        healthy: instance.healthy !== false && heartbeat.status === "alive",
        weight: instance.weight,
        metadata: endpoint.metadata || {},
        fallback: false,
        source: "registry",
        reason: "discovered"
      }))
  ).sort((left, right) => String(left.instance_id).localeCompare(String(right.instance_id)));
}

function buildRegistryLifecycleMetricAlertsFromRecords(observations: any[]): any[] {
  const alerts = [];
  for (const observation of observations) {
    for (const record of observation.metrics.records) {
      const instanceId = record.instanceId || record.data?._instance_id || "";
      for (const spec of REGISTRY_LIFECYCLE_METRIC_ALERTS) {
        const count = parseMetricInt(record.data?.[spec.field]);
        if (count <= 0) continue;
        alerts.push({
          kind: spec.kind,
          service: observation.name,
          endpoint: "",
          instance_id: instanceId,
          severity: "critical",
          message: registryLifecycleMetricMessage({ kind: spec.kind, service: observation.name, instance_id: instanceId }),
          source: "metrics",
          reason: "reported_total",
          count
        });
      }
    }
  }
  return alerts;
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

function buildRegistryLifecycleMetricAlerts(): any[] {
  try {
    return getRegistryLifecycleMetricsSnapshot()
      .filter((metric: any) => ["register_failed", "heartbeat_failed", "deregister_failed"].includes(metric.kind))
      .map((metric: any) => ({
        kind: metric.kind,
        service: metric.service || "",
        endpoint: metric.endpoint || "",
        instance_id: metric.instance_id || "",
        severity: "critical",
        message: registryLifecycleMetricMessage(metric),
        source: metric.source || "registry",
        reason: metric.reason || "",
        count: metric.count || 0
      }));
  } catch {
    return [];
  }
}

function buildRegistryCapacitySummary(metrics: any = {}): any {
  const cacheHits = parseMetricInt(metrics.registry_discovery_cache_hit_total);
  const cacheMisses = parseMetricInt(metrics.registry_discovery_cache_miss_total);
  return {
    scan_total: parseMetricInt(metrics.registry_scan_total),
    scan_duration_ms_total: parseMetricInt(metrics.registry_scan_duration_ms_total),
    scan_duration_ms_last: parseMetricInt(metrics.registry_scan_duration_ms_last),
    scan_duration_ms_max: parseMetricInt(metrics.registry_scan_duration_ms_max),
    scan_instance_keys_total: parseMetricInt(metrics.registry_scan_instance_keys_total),
    scan_instance_keys_last: parseMetricInt(metrics.registry_scan_instance_keys_last),
    scan_visible_instances_total: parseMetricInt(metrics.registry_scan_visible_instances_total),
    scan_visible_instances_last: parseMetricInt(metrics.registry_scan_visible_instances_last),
    cache_hit_total: cacheHits,
    cache_miss_total: cacheMisses,
    cache_hit_rate_basis_points: cacheHitRateBasisPoints(cacheHits, cacheMisses)
  };
}

function aggregateRegistryCapacitySummaries(summaries: any[]): any {
  const aggregate = {
    scan_total: 0,
    scan_duration_ms_total: 0,
    scan_duration_ms_last: 0,
    scan_duration_ms_max: 0,
    scan_instance_keys_total: 0,
    scan_instance_keys_last: 0,
    scan_visible_instances_total: 0,
    scan_visible_instances_last: 0,
    cache_hit_total: 0,
    cache_miss_total: 0,
    cache_hit_rate_basis_points: 0
  };

  for (const summary of summaries) {
    aggregate.scan_total += parseMetricInt(summary.scan_total);
    aggregate.scan_duration_ms_total += parseMetricInt(summary.scan_duration_ms_total);
    aggregate.scan_duration_ms_last = Math.max(
      aggregate.scan_duration_ms_last,
      parseMetricInt(summary.scan_duration_ms_last)
    );
    aggregate.scan_duration_ms_max = Math.max(
      aggregate.scan_duration_ms_max,
      parseMetricInt(summary.scan_duration_ms_max)
    );
    aggregate.scan_instance_keys_total += parseMetricInt(summary.scan_instance_keys_total);
    aggregate.scan_instance_keys_last = Math.max(
      aggregate.scan_instance_keys_last,
      parseMetricInt(summary.scan_instance_keys_last)
    );
    aggregate.scan_visible_instances_total += parseMetricInt(summary.scan_visible_instances_total);
    aggregate.scan_visible_instances_last = Math.max(
      aggregate.scan_visible_instances_last,
      parseMetricInt(summary.scan_visible_instances_last)
    );
    aggregate.cache_hit_total += parseMetricInt(summary.cache_hit_total);
    aggregate.cache_miss_total += parseMetricInt(summary.cache_miss_total);
  }

  aggregate.cache_hit_rate_basis_points = cacheHitRateBasisPoints(
    aggregate.cache_hit_total,
    aggregate.cache_miss_total
  );
  return aggregate;
}

function cacheHitRateBasisPoints(cacheHits: number, cacheMisses: number): number {
  const total = cacheHits + cacheMisses;
  return total > 0 ? Math.round((cacheHits * 10000) / total) : 0;
}

function registryLifecycleMetricMessage(metric: any): string {
  const service = metric.service || "unknown-service";
  const instance = metric.instance_id ? `/${metric.instance_id}` : "";
  if (metric.kind === "register_failed") {
    return `${service}${instance} 注册失败`;
  }
  if (metric.kind === "heartbeat_failed") {
    return `${service}${instance} heartbeat 续期失败`;
  }
  return `${service}${instance} deregister 失败`;
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
