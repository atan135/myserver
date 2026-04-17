/**
 * Metrics module for announce-service
 *
 * Collects: QPS, HTTP request latency
 * Reports to Redis every 5 seconds
 */

const METRICS_TTL = 604800;
const HEARTBEAT_TTL = 30;
const REPORT_INTERVAL_MS = 5000;

function currentBucket() {
  return Math.floor(Date.now() / REPORT_INTERVAL_MS) * REPORT_INTERVAL_MS / 1000;
}

export class MetricsCollector {
  constructor(redis, serviceName) {
    this.redis = redis;
    this.serviceName = serviceName;
    this.qps = 0;
    this.latencySum = 0;
    this.latencyCount = 0;
    this.flushTimer = null;
  }

  middleware() {
    return (_req, res, next) => {
      const startedAt = Date.now();
      res.on("finish", () => {
        this.qps += 1;
        this.latencySum += Date.now() - startedAt;
        this.latencyCount += 1;
      });
      next();
    };
  }

  async flush() {
    const bucket = currentBucket();
    const metricsKey = `metrics:${this.serviceName}:${bucket}`;
    const heartbeatKey = `metrics:heartbeat:${this.serviceName}`;

    const qps = this.qps;
    const latencyMs =
      this.latencyCount > 0
        ? Math.round(this.latencySum / this.latencyCount)
        : 0;

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

  start() {
    this.flushTimer = setInterval(() => this.flush(), REPORT_INTERVAL_MS);
    this.flushTimer.unref();
  }

  async stop() {
    if (this.flushTimer) {
      clearInterval(this.flushTimer);
      this.flushTimer = null;
    }
    await this.flush();
  }
}

export function createMetricsCollector(redis, serviceName) {
  const collector = new MetricsCollector(redis, serviceName);
  collector.start();
  return collector;
}
