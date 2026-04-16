/**
 * Metrics module for admin-api
 *
 * Collects: QPS, HTTP request latency
 * Reports to Redis every 5 seconds
 */

const METRICS_TTL = 604800; // 7 days in seconds
const HEARTBEAT_TTL = 30; // seconds
const REPORT_INTERVAL_MS = 5000;

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

  /**
   * Flush metrics to Redis
   */
  async flush() {
    const bucket = currentBucket();
    const metricsKey = `metrics:${this.serviceName}:${bucket}`;
    const heartbeatKey = `metrics:heartbeat:${this.serviceName}`;

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
        latency_ms: latencyMs
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
    await this.flush();
  }
}

export function createMetricsCollector(redis, serviceName, redisKeyPrefix = "") {
  const collector = new MetricsCollector(redis, serviceName, redisKeyPrefix);
  collector.start();
  return collector;
}
