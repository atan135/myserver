/**
 * Metrics module for mail-service
 *
 * Collects: QPS, HTTP request latency
 * Reports to NATS every 5 seconds
 */

import { encodeSubjectToken } from "./nats-client.js";
import {
  collectDiscoveryMetricFields,
  collectRegistryCapacityMetricFields,
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
    this.outboxBacklog = 0;
    this.outboxOldestAgeMs = 0;
    this.outboxPublishLatencySum = 0;
    this.outboxPublished = 0;
    this.outboxRetries = 0;
    this.outboxTerminal = 0;
    this.outboxLeaseTakeovers = 0;
    this.mailClaimRouteUnavailable = 0;
    this.mailClaimGrantFailures = 0;
    this.mailClaimResultUnknown = 0;
    this.mailClaimRetryableFailures = 0;
    this.mailClaimPermanentFailures = 0;

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

  setOutboxSnapshot({ backlog = 0, oldestAgeMs = 0 } = {}) {
    this.outboxBacklog = Math.max(0, Number(backlog) || 0);
    this.outboxOldestAgeMs = Math.max(0, Number(oldestAgeMs) || 0);
  }

  recordOutboxPublished(latencyMs = 0) {
    this.outboxPublished += 1;
    this.outboxPublishLatencySum += Math.max(0, Number(latencyMs) || 0);
  }

  recordOutboxRetry() {
    this.outboxRetries += 1;
  }

  recordOutboxTerminal() {
    this.outboxTerminal += 1;
  }

  recordOutboxLeaseTakeover() {
    this.outboxLeaseTakeovers += 1;
  }

  recordMailClaimRouteUnavailable() {
    this.mailClaimRouteUnavailable += 1;
  }

  recordMailClaimGrantFailure() {
    this.mailClaimGrantFailures += 1;
  }

  recordMailClaimResultUnknown() {
    this.mailClaimResultUnknown += 1;
  }

  recordMailClaimRetryableFailure() {
    this.mailClaimRetryableFailures += 1;
  }

  recordMailClaimPermanentFailure() {
    this.mailClaimPermanentFailures += 1;
  }

  /**
   * Flush metrics to NATS
   */
  async flush() {
    const bucket = currentBucket();

    const qps = this.qps;
    const latencyMs = this.latencyCount > 0 ? Math.round(this.latencySum / this.latencyCount) : 0;
    const outboxPublishLatencyMs = this.outboxPublished > 0
      ? Math.round(this.outboxPublishLatencySum / this.outboxPublished)
      : 0;
    const outboxPublished = this.outboxPublished;
    const outboxRetries = this.outboxRetries;
    const outboxTerminal = this.outboxTerminal;
    const outboxLeaseTakeovers = this.outboxLeaseTakeovers;
    const mailClaimRouteUnavailable = this.mailClaimRouteUnavailable;
    const mailClaimGrantFailures = this.mailClaimGrantFailures;
    const mailClaimResultUnknown = this.mailClaimResultUnknown;
    const mailClaimRetryableFailures = this.mailClaimRetryableFailures;
    const mailClaimPermanentFailures = this.mailClaimPermanentFailures;

    // Reset counters
    this.qps = 0;
    this.latencySum = 0;
    this.latencyCount = 0;
    this.outboxPublishLatencySum = 0;
    this.outboxPublished = 0;
    this.outboxRetries = 0;
    this.outboxTerminal = 0;
    this.outboxLeaseTakeovers = 0;
    this.mailClaimRouteUnavailable = 0;
    this.mailClaimGrantFailures = 0;
    this.mailClaimResultUnknown = 0;
    this.mailClaimRetryableFailures = 0;
    this.mailClaimPermanentFailures = 0;

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
            mail_outbox_backlog: this.outboxBacklog,
            mail_outbox_oldest_age_ms: this.outboxOldestAgeMs,
            mail_outbox_publish_latency_ms: outboxPublishLatencyMs,
            mail_outbox_published: outboxPublished,
            mail_outbox_retries: outboxRetries,
            mail_outbox_terminal: outboxTerminal,
            mail_outbox_lease_takeovers: outboxLeaseTakeovers,
            mail_claim_route_unavailable: mailClaimRouteUnavailable,
            mail_claim_grant_failures: mailClaimGrantFailures,
            mail_claim_result_unknown: mailClaimResultUnknown,
            mail_claim_retryable_failures: mailClaimRetryableFailures,
            mail_claim_permanent_failures: mailClaimPermanentFailures,
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
