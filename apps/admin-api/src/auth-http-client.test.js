import assert from "node:assert/strict";
import test from "node:test";

import { AuthHttpClient } from "./auth-http-client.js";

function config(overrides = {}) {
  return {
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: false,
    localDiscoveryFallbackEnabled: true,
    internalApiToken: "internal-test-token",
    authHttpRequestTimeoutMs: 3000,
    ...overrides
  };
}

function jsonResponse(status, body) {
  return {
    ok: status >= 200 && status < 300,
    status,
    async text() {
      return JSON.stringify(body);
    }
  };
}

test("AuthHttpClient creates a character through the local auth-http internal endpoint", async () => {
  const requests = [];
  const client = new AuthHttpClient(config(), null, async (url, options) => {
    requests.push({ url, options });
    return jsonResponse(201, {
      ok: true,
      character: { character_id: "chr_0000000000001", name: "Echo" }
    });
  });
  const payload = {
    accountPlayerId: "plr_0000000000001",
    name: "Echo",
    adminActor: "operator",
    reason: "support request"
  };

  const character = await client.createCharacterForAdmin(payload, { requestId: "request-1" });

  assert.equal(character.character_id, "chr_0000000000001");
  assert.equal(requests[0].url, "http://127.0.0.1:3000/api/v1/internal/characters");
  assert.equal(requests[0].options.method, "POST");
  assert.equal(requests[0].options.headers["x-service-token"], "internal-test-token");
  assert.equal(requests[0].options.headers["x-request-id"], "request-1");
  assert.deepEqual(JSON.parse(requests[0].options.body), payload);
});

test("AuthHttpClient preserves auth-http status and error code", async () => {
  const client = new AuthHttpClient(config(), null, async () => jsonResponse(409, {
    ok: false,
    error: "CHARACTER_NAME_DUPLICATE",
    message: "character name already exists"
  }));

  await assert.rejects(
    () => client.createCharacterForAdmin({}, { requestId: "request-2" }),
    (error) => {
      assert.equal(error.statusCode, 409);
      assert.equal(error.code, "CHARACTER_NAME_DUPLICATE");
      return true;
    }
  );
});

test("AuthHttpClient forbids local fallback when registry discovery is required", async () => {
  const client = new AuthHttpClient(config({
    registryDiscoveryRequired: true,
    localDiscoveryFallbackEnabled: false
  }), null, async () => {
    throw new Error("fetch must not be called");
  });

  await assert.rejects(
    () => client.createCharacterForAdmin({}, { requestId: "request-3" }),
    (error) => {
      assert.equal(error.statusCode, 503);
      assert.equal(error.code, "SERVICE_DISCOVERY_REQUIRED");
      return true;
    }
  );
});
