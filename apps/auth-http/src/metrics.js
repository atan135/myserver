/**
 * Metrics module for auth-http
 *
 * Collects: QPS, HTTP request latency, online sessions count
 * Reports to Redis every 5 seconds
 */

const METRICS_TTL = 604800; // 7 days in seconds
const HEARTBEAT_TTL = 30; // seconds
const REPORT_INTERVAL_MS = 5000;
const ACTIVE_SESSION_WINDOW_SECONDS = 300;

function currentBucket() {
  return Math.floor(Date.now() / REPORT_INTERVAL_MS) * REPORT_INTERVAL_MS / 1000;
}

export class MetricsCollector {
  constructor(redis, serviceName, redisKeyPrefix = "") {
    this.redis = redis;
    this.serviceName = serviceName;
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
        this.qps += 1;
        this.latencySum += Date.now() - start;
        this.latencyCount += 1;
      });
      next();
    };
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
   * Flush metrics to Redis
   */
  async flush() {
    const bucket = currentBucket();
    const metricsKey = `metrics:${this.serviceName}:${bucket}`;
    const heartbeatKey = `metrics:heartbeat:${this.serviceName}`;

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
      const pipe = this.redis.pipeline();
      pipe.hset(metricsKey, {
        qps,
        latency_ms: latencyMs,
        online_sessions: this.onlineSessions,
        unique_players: this.uniquePlayers,
        active_sessions_5m: this.activeSessions5m,
        active_window_seconds: ACTIVE_SESSION_WINDOW_SECONDS
      });
      pipe.expire(metricsKey, METRICS_TTL);
      pipe.set(heartbeatKey, Date.now(), "EX", HEARTBEAT_TTL);
      await pipe.exec();
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
 * @param {string} serviceName
 * @param {string} redisKeyPrefix
 * @returns {MetricsCollector}
 */
export function createMetricsCollector(redis, serviceName, redisKeyPrefix = "") {
  const collector = new MetricsCollector(redis, serviceName, redisKeyPrefix);
  collector.start();
  return collector;
}
