/**
 * Metrics module for auth-http
 *
 * Collects: QPS, HTTP request latency, online sessions count
 * Reports to NATS every 5 seconds
 */

import { encodeSubjectToken } from "./nats-client.js";
import {
  collectDiscoveryMetricFields,
  collectRegistryCapacityMetricFields,
  collectRegistryLifecycleMetricFields
} from "../../../packages/service-registry/node/registry-schema.js";

const REPORT_INTERVAL_MS = 5000;
const ACTIVE_SESSION_WINDOW_SECONDS = 300;

function currentBucket() {
  return Math.floor(Date.now() / REPORT_INTERVAL_MS) * REPORT_INTERVAL_MS / 1000;
}

export class MetricsCollector {
  constructor(redis, nats, serviceName, redisKeyPrefix = "", serviceInstanceId = serviceName) {
    this.redis = redis;
    this.nats = nats;
    this.serviceName = serviceName;
    this.serviceInstanceId = serviceInstanceId;
    this.keyPrefix = redisKeyPrefix;

    // Counters
    this.qps = 0;
    this.latencySum = 0;
    this.latencyCount = 0;
    this.onlineSessions = 0;
    this.uniquePlayers = 0;
    this.activeSessions5m = 0;

    // Pending flush flag
    this.flushTimer = null;
  }

  /**
   * Middleware to track request count and latency
   */
  middleware() {
    return (req, res, next) => {
      const start = Date.now();
      res.on("finish", () => {
        this.recordRequest(Date.now() - start);
      });
      next();
    };
  }

  recordRequest(latencyMs = 0) {
    this.qps += 1;
    this.latencySum += latencyMs;
    this.latencyCount += 1;
  }

  async countSessionStats() {
    let totalSessions = 0;
    const uniquePlayers = new Set();
    const pattern = `${this.keyPrefix}session:*`;
    let cursor = "0";

    try {
      do {
        const [nextCursor, keys] = await this.redis.scan(cursor, "MATCH", pattern, "COUNT", 100);
        cursor = nextCursor;

        if (keys.length === 0) {
          continue;
        }

        totalSessions += keys.length;

        const pipe = this.redis.pipeline();
        for (const key of keys) {
          pipe.get(key);
        }

        const results = await pipe.exec();
        for (const [, raw] of results) {
          if (!raw) {
            continue;
          }

          try {
            const session = JSON.parse(raw);
            if (session.playerId) {
              uniquePlayers.add(session.playerId);
            }
          } catch (error) {
            console.error("[metrics] parse session error:", error);
          }
        }
      } while (cursor !== "0");
    } catch (error) {
      console.error("[metrics] countSessionStats error:", error);
    }

    this.onlineSessions = totalSessions;
    this.uniquePlayers = uniquePlayers.size;
  }

  async countActiveSessions() {
    let count = 0;
    const pattern = `${this.keyPrefix}session-activity:*`;
    let cursor = "0";

    try {
      do {
        const [nextCursor, keys] = await this.redis.scan(cursor, "MATCH", pattern, "COUNT", 100);
        cursor = nextCursor;
        count += keys.length;
      } while (cursor !== "0");
    } catch (error) {
      console.error("[metrics] countActiveSessions error:", error);
    }

    this.activeSessions5m = count;
  }

  /**
   * Flush metrics to NATS
   */
  async flush() {
    const bucket = currentBucket();

    await Promise.all([
      this.countSessionStats(),
      this.countActiveSessions()
    ]);

    const qps = this.qps;
    const latencyMs = this.latencyCount > 0 ? Math.round(this.latencySum / this.latencyCount) : 0;

    // Reset counters
    this.qps = 0;
    this.latencySum = 0;
    this.latencyCount = 0;

    try {
      const discoveryMetrics = collectDiscoveryMetricFields({ reset: true });
      const capacityMetrics = collectRegistryCapacityMetricFields({ reset: true });
      const lifecycleMetrics = collectRegistryLifecycleMetricFields({ reset: true });
      await this.nats.publishJson(
        `myserver.metrics.${this.serviceName}.${encodeSubjectToken(this.serviceInstanceId)}`,
        {
          service: this.serviceName,
          instance_id: this.serviceInstanceId,
          bucket,
          timestamp: Math.floor(Date.now() / 1000),
          metrics: {
            qps,
            latency_ms: latencyMs,
            online_sessions: this.onlineSessions,
            unique_players: this.uniquePlayers,
            active_sessions_5m: this.activeSessions5m,
            active_window_seconds: ACTIVE_SESSION_WINDOW_SECONDS,
            ...discoveryMetrics,
            ...capacityMetrics,
            ...lifecycleMetrics
          }
        }
      );
    } catch (error) {
      console.error("[metrics] flush error:", error);
    }
  }

  /**
   * Start periodic reporting
   */
  start() {
    this.flushTimer = setInterval(() => this.flush(), REPORT_INTERVAL_MS);
    // Disable timer during shutdown
    this.flushTimer.unref();
  }

  /**
   * Stop periodic reporting and flush remaining metrics
   */
  async stop() {
    if (this.flushTimer) {
      clearInterval(this.flushTimer);
      this.flushTimer = null;
    }
    // Final flush
    await this.flush();
  }
}

/**
 * Create and start a metrics collector
 * @param {import("ioredis").Redis} redis
 * @param {{ publishJson(subject: string, payload: object): Promise<void> }} nats
 * @param {string} serviceName
 * @param {string} redisKeyPrefix
 * @returns {MetricsCollector}
 */
export function createMetricsCollector(
  redis,
  nats,
  serviceName,
  redisKeyPrefix = "",
  serviceInstanceId = serviceName
) {
  const collector = new MetricsCollector(
    redis,
    nats,
    serviceName,
    redisKeyPrefix,
    serviceInstanceId
  );
  collector.start();
  return collector;
}
