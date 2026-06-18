/**
 * Metrics module for admin-api
 *
 * Collects: QPS, HTTP request latency
 * Reports to NATS every 5 seconds
 */

import { encodeSubjectToken } from "./nats-client.js";
import {
  collectDiscoveryMetricFields,
  collectRegistryLifecycleMetricFields
} from "../../../packages/service-registry/node/registry-schema.js";

const REPORT_INTERVAL_MS = 5000;

function currentBucket() {
  return Math.floor(Date.now() / REPORT_INTERVAL_MS) * REPORT_INTERVAL_MS / 1000;
}

export class MetricsCollector {
  constructor(nats, serviceName, serviceInstanceId = serviceName) {
    this.nats = nats;
    this.serviceName = serviceName;
    this.serviceInstanceId = serviceInstanceId;

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

  /**
   * Flush metrics to NATS
   */
  async flush() {
    const bucket = currentBucket();

    const qps = this.qps;
    const latencyMs = this.latencyCount > 0 ? Math.round(this.latencySum / this.latencyCount) : 0;

    // Reset counters
    this.qps = 0;
    this.latencySum = 0;
    this.latencyCount = 0;

    try {
      const discoveryMetrics = collectDiscoveryMetricFields({ reset: true });
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
            ...discoveryMetrics,
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

export function createMetricsCollector(nats, serviceName, serviceInstanceId = serviceName) {
  const collector = new MetricsCollector(nats, serviceName, serviceInstanceId);
  collector.start();
  return collector;
}
