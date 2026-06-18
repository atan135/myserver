import assert from "node:assert/strict";
import test from "node:test";

import { MetricsCollector } from "./metrics.js";
import {
  recordDiscoveryMetric,
  resetDiscoveryMetrics
} from "../../../packages/service-registry/node/registry-schema.js";

test("MetricsCollector flush includes discovery metric counters", async () => {
  resetDiscoveryMetrics();
  recordDiscoveryMetric({
    serviceName: "game-proxy",
    endpointName: "client",
    source: "fallback",
    reason: "fallback_used"
  });
  recordDiscoveryMetric({
    serviceName: "game-server",
    endpointName: "admin",
    source: "registry",
    reason: "registry_error"
  });

  const published = [];
  const collector = new MetricsCollector(
    {
      async scan() {
        return ["0", []];
      }
    },
    {
      async publishJson(subject, payload) {
        published.push({ subject, payload });
      }
    },
    "auth-http",
    "",
    "auth-test"
  );

  await collector.flush();

  assert.equal(published.length, 1);
  assert.equal(published[0].payload.metrics.fallback_used_total, "1");
  assert.equal(published[0].payload.metrics.discovery_failure_total, "1");
  assert.equal(published[0].payload.metrics.discovery_success_total, "0");
});
