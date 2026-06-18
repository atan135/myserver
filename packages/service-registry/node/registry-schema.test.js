import assert from "node:assert/strict";
import test from "node:test";

import {
  collectRegistryLifecycleMetricFields,
  getRegistryLifecycleMetricsSnapshot,
  recordRegistryLifecycleMetric,
  SERVICE_ENDPOINT_VISIBILITIES,
  normalizeEndpoint,
  normalizeServiceInstance,
  resetRegistryLifecycleMetrics,
  validateServiceEndpoint
} from "./registry-schema.js";

function networkEndpoint(overrides = {}) {
  return {
    name: "client",
    protocol: "tcp",
    host: "127.0.0.1",
    port: 7000,
    socket: "",
    visibility: "internal",
    metadata: {},
    healthy: true,
    ...overrides
  };
}

function socketEndpoint(overrides = {}) {
  return {
    name: "local-socket",
    protocol: "local_socket",
    host: "",
    port: 0,
    socket: "service.sock",
    visibility: "local",
    metadata: {},
    healthy: true,
    ...overrides
  };
}

test("validateServiceEndpoint accepts supported endpoint visibilities", () => {
  assert.deepEqual(SERVICE_ENDPOINT_VISIBILITIES, ["public", "internal", "admin", "local"]);

  for (const visibility of SERVICE_ENDPOINT_VISIBILITIES) {
    assert.deepEqual(validateServiceEndpoint(networkEndpoint({ visibility })), {
      ok: true,
      errors: []
    });
  }
});

test("validateServiceEndpoint rejects unsupported endpoint visibility", () => {
  const validation = validateServiceEndpoint(networkEndpoint({ visibility: "private" }));

  assert.equal(validation.ok, false);
  assert.match(validation.errors.join("\n"), /visibility must be one of: public, internal, admin, local/);
  assert.equal(normalizeEndpoint(networkEndpoint({ visibility: "private" })), null);
});

test("normalizeEndpoint defaults empty or missing visibility by endpoint transport", () => {
  assert.equal(normalizeEndpoint(networkEndpoint({ visibility: "" })).visibility, "internal");
  assert.equal(normalizeEndpoint(networkEndpoint({ visibility: undefined })).visibility, "internal");
  assert.equal(normalizeEndpoint(socketEndpoint({ visibility: "" })).visibility, "local");
  assert.equal(normalizeEndpoint(socketEndpoint({ visibility: undefined })).visibility, "local");
});

test("normalizeServiceInstance applies endpoint visibility defaults", () => {
  const instance = normalizeServiceInstance({
    schema_version: 2,
    id: "visibility-001",
    name: "visibility-service",
    host: "127.0.0.1",
    port: 7000,
    admin_port: 0,
    local_socket: "",
    endpoints: [
      networkEndpoint({ name: "client-default", visibility: "" }),
      socketEndpoint({ name: "socket-default", visibility: undefined })
    ],
    tags: [],
    weight: 100,
    metadata: {},
    registered_at: 1,
    healthy: true
  });

  assert.equal(instance.endpoints.find((endpoint) => endpoint.name === "client-default").visibility, "internal");
  assert.equal(instance.endpoints.find((endpoint) => endpoint.name === "socket-default").visibility, "local");
});

test("normalizeServiceInstance backfills game-proxy legacy endpoints with proxy protocols", () => {
  const instance = normalizeServiceInstance({
    schema_version: 1,
    id: "proxy-legacy-001",
    name: "game-proxy",
    host: "127.0.0.1",
    port: 4000,
    admin_port: 7101,
    local_socket: "",
    tags: [],
    weight: 100,
    metadata: {},
    registered_at: 1,
    healthy: true
  });

  assert.equal(instance.schema_version, 2);
  assert.deepEqual(
    instance.endpoints.map(({ name, protocol, host, port, visibility }) => ({
      name,
      protocol,
      host,
      port,
      visibility
    })),
    [
      {
        name: "admin",
        protocol: "http",
        host: "127.0.0.1",
        port: 7101,
        visibility: "admin"
      },
      {
        name: "client",
        protocol: "kcp",
        host: "127.0.0.1",
        port: 4000,
        visibility: "public"
      }
    ]
  );
});

test("registry lifecycle metrics aggregate failure events by operation labels", () => {
  resetRegistryLifecycleMetrics();

  assert.equal(recordRegistryLifecycleMetric("unknown_kind", { serviceName: "admin-api" }), null);
  assert.deepEqual(recordRegistryLifecycleMetric("register_failed", {
    serviceName: "admin-api",
    endpointName: "http",
    instanceId: "admin-api-001",
    reason: "redis_error",
    error: new Error("SET_FAILED")
  }), {
    kind: "register_failed",
    service: "admin-api",
    endpoint: "http",
    instance_id: "admin-api-001",
    source: "registry",
    reason: "redis_error",
    count: 1
  });
  recordRegistryLifecycleMetric("register_failed", {
    service: "admin-api",
    endpoint: "http",
    instance_id: "admin-api-001",
    reason: "redis_error"
  });
  recordRegistryLifecycleMetric("heartbeat_failed", {
    serviceName: "admin-api",
    instanceId: "admin-api-001",
    reason: "redis_error"
  });

  assert.deepEqual(getRegistryLifecycleMetricsSnapshot(), [
    {
      kind: "heartbeat_failed",
      service: "admin-api",
      endpoint: "",
      instance_id: "admin-api-001",
      source: "registry",
      reason: "redis_error",
      count: 1
    },
    {
      kind: "register_failed",
      service: "admin-api",
      endpoint: "http",
      instance_id: "admin-api-001",
      source: "registry",
      reason: "redis_error",
      count: 2
    }
  ]);
  assert.deepEqual(collectRegistryLifecycleMetricFields(), {
    register_failed_total: "2",
    heartbeat_failed_total: "1",
    deregister_failed_total: "0"
  });
  assert.deepEqual(collectRegistryLifecycleMetricFields({ reset: true }), {
    register_failed_total: "2",
    heartbeat_failed_total: "1",
    deregister_failed_total: "0"
  });
  assert.deepEqual(getRegistryLifecycleMetricsSnapshot(), []);
});
