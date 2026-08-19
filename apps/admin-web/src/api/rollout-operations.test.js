import assert from "node:assert/strict";
import test from "node:test";

const storage = new Map();
globalThis.localStorage = {
  getItem: (key) => storage.get(key) || null,
  setItem: (key, value) => storage.set(key, String(value)),
  removeItem: (key) => storage.delete(key)
};

const { adminOperationApi, rolloutApi, default: api, highRiskRequestBody } = await import("./index.js");

test("rollout API sends high-risk drain requests only through the authenticated admin-api client", async () => {
  storage.set("admin_token", "jwt-token");
  let request;
  const previousAdapter = api.defaults.adapter;
  api.defaults.adapter = async (config) => {
    request = config;
    return { data: { ok: true }, status: 200, statusText: "OK", headers: {}, config };
  };
  try {
    await rolloutApi.setDrain("game server/a", {
      enabled: true,
      reason: "graceful replacement",
      requestId: "drain-request-1",
      preflightNonce: "nonce-1",
      preflightSummarySha256: "a".repeat(64)
    });
  } finally {
    api.defaults.adapter = previousAdapter;
  }

  assert.equal(request.baseURL, "/api/v1");
  assert.equal(request.url, "/rollouts/game-server/game%20server%2Fa/drain");
  assert.equal(request.headers.Authorization, "Bearer jwt-token");
  assert.deepEqual(JSON.parse(request.data), {
    enabled: true,
    reason: "graceful replacement",
    requestId: "drain-request-1",
    preflightNonce: "nonce-1",
    preflightSummarySha256: "a".repeat(64)
  });
});

test("approval API carries a request id and supports approval and rejection evidence", async () => {
  let request;
  const previousAdapter = api.defaults.adapter;
  api.defaults.adapter = async (config) => {
    request = config;
    return { data: { ok: true }, status: 200, statusText: "OK", headers: {}, config };
  };
  try {
    await adminOperationApi.reject("request / 1", "unsafe target", { ticket: "OPS-1" });
  } finally {
    api.defaults.adapter = previousAdapter;
  }

  assert.equal(request.url, "/admin-operations/request%20%2F%201/approval");
  assert.deepEqual(JSON.parse(request.data), {
    status: "rejected",
    evidenceSummary: { ticket: "OPS-1" },
    rejectionReason: "unsafe target",
    requestId: "request / 1"
  });
  assert.match(highRiskRequestBody({}).requestId, /^admin-web-/);
});

test("rollout read APIs use admin-api paths and never receive endpoint input", async () => {
  const seen = [];
  const previousAdapter = api.defaults.adapter;
  api.defaults.adapter = async (config) => {
    seen.push(config);
    return { data: { ok: true }, status: 200, statusText: "OK", headers: {}, config };
  };
  try {
    await rolloutApi.getInstances();
    await rolloutApi.getDrainStatus("game-server-a");
  } finally {
    api.defaults.adapter = previousAdapter;
  }
  assert.deepEqual(seen.map((config) => config.url), [
    "/rollouts/game-server/instances",
    "/rollouts/game-server/game-server-a/drain-status"
  ]);
  assert.equal(seen.every((config) => config.baseURL === "/api/v1"), true);
});
