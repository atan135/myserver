import assert from "node:assert/strict";
import test from "node:test";

import { buildAdminAuthBody, normalizeGameAdminActor } from "./game-admin-client.js";

const config = { gameAdminToken: "secret-admin-token" };

test("admin auth body keeps legacy plain token when actor is missing", () => {
  const body = buildAdminAuthBody(config);

  assert.equal(body.toString("utf8"), "secret-admin-token");
});

test("admin auth body uses JSON envelope when actor is valid", () => {
  const body = buildAdminAuthBody(config, " ops@example.com ");

  assert.deepEqual(JSON.parse(body.toString("utf8")), {
    token: "secret-admin-token",
    actor: "ops@example.com"
  });
});

test("admin auth body falls back to plain token for invalid actor", () => {
  const body = buildAdminAuthBody(config, "ops+admin@example.com");

  assert.equal(normalizeGameAdminActor("ops+admin@example.com"), null);
  assert.equal(body.toString("utf8"), "secret-admin-token");
});

test("admin actor rejects values longer than game-server limit", () => {
  assert.equal(normalizeGameAdminActor("a".repeat(129)), null);
});
