import assert from "node:assert/strict";
import test from "node:test";

import {
  SERVICE_ENDPOINT_VISIBILITIES,
  normalizeEndpoint,
  normalizeServiceInstance,
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
