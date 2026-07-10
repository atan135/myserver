import assert from "node:assert/strict";
import test from "node:test";

import {
  assertLocalRedisUrl,
  buildProvisionPlan,
  buildRegistryCleanupPlan,
  parseTicketMetadata,
  validateCleanupEntries,
  validateEnvName
} from "./online-ticket-store.mjs";

test("provision plan creates character-bound signed tickets and exact owned keys", () => {
  const plan = buildProvisionPlan(
    {
      runId: "20260710-120000-a1b2c3d4",
      keyPrefix: "test:",
      ttlSeconds: 300,
      worldId: 7
    },
    "unit-test-ticket-secret"
  );

  assert.notEqual(plan.primary.ticket, plan.observer.ticket);
  assert.match(plan.primary.playerId, /^plr_[0-9a-f]{20}$/);
  assert.match(plan.primary.characterId, /^chr_[0-9a-f]{20}$/);
  assert.equal(plan.entries.length, 4);
  assert.ok(plan.entries.every((entry) => entry.key.startsWith("test:")));

  const parsed = parseTicketMetadata(plan.primary.ticket, "test:");
  assert.equal(parsed.playerId, plan.primary.playerId);
  assert.equal(parsed.characterId, plan.primary.characterId);
  assert.equal(parsed.version, 1);
  assert.equal(parsed.fingerprint, plan.primary.fingerprint);
  assert.equal(parsed.expired, false);
  assert.equal(parsed.ticketKey, plan.entries[0].key);

  const payload = JSON.parse(Buffer.from(plan.primary.ticket.split(".")[0], "base64url").toString("utf8"));
  payload.characterId = "character-not-bound";
  const malformed = `${Buffer.from(JSON.stringify(payload)).toString("base64url")}.ignored-signature`;
  assert.throws(() => parseTicketMetadata(malformed, "test:"), /characterId has an invalid format/);
});

test("cleanup guard accepts only exact ticket owner and generated-player version keys", () => {
  const ticketKey = `lockstep:ticket:${"a".repeat(64)}`;
  const versionKey = "lockstep:player-ticket-version:plr_0123456789abcdef";
  const entries = validateCleanupEntries(
    [
      { key: ticketKey, expectedValue: "plr_0123456789abcdef" },
      { key: versionKey, expectedValue: "1" }
    ],
    "lockstep:"
  );

  assert.equal(entries.length, 2);
  assert.throws(
    () => validateCleanupEntries([{ key: "lockstep:ticket:*", expectedValue: "x" }], "lockstep:"),
    /not an exact lockstep ticket key/
  );
  assert.throws(
    () => validateCleanupEntries([{ key: "other:ticket:" + "a".repeat(64), expectedValue: "x" }], "lockstep:"),
    /outside the configured prefix/
  );
  assert.throws(
    () => validateCleanupEntries([{ key: "lockstep:session:all", expectedValue: "x" }], "lockstep:"),
    /not an exact lockstep ticket key/
  );
});

test("dev ticket operations are restricted to loopback Redis", () => {
  assert.equal(assertLocalRedisUrl("redis://127.0.0.1:6379").hostname, "127.0.0.1");
  assert.equal(assertLocalRedisUrl("rediss://localhost:6380/1").hostname, "localhost");
  assert.throws(() => assertLocalRedisUrl("redis://redis.internal:6379"), /restricted to loopback/);
  assert.throws(() => assertLocalRedisUrl("https://127.0.0.1:6379"), /must use redis/);
});

test("registry cleanup guard is bound to the exact run-owned game-server keys", () => {
  const plan = buildRegistryCleanupPlan({
    runId: "20260710-120000-a1b2c3d4",
    keyPrefix: "test:",
    serviceName: "game-server",
    instanceId: "lockstep-20260710-120000-a1b2c3d4"
  });

  assert.equal(
    plan.instanceKey,
    "test:service:game-server:instances:lockstep-20260710-120000-a1b2c3d4"
  );
  assert.equal(
    plan.heartbeatKey,
    "test:heartbeat:game-server:lockstep-20260710-120000-a1b2c3d4"
  );
  assert.deepEqual(plan.expectedInstanceHash, {
    id: "lockstep-20260710-120000-a1b2c3d4",
    name: "game-server"
  });
  assert.equal(plan.expectedHeartbeatValue, "1");

  assert.throws(
    () => buildRegistryCleanupPlan({ ...plan, serviceName: "auth-http" }),
    /restricted to service game-server/
  );
  assert.throws(
    () => buildRegistryCleanupPlan({ ...plan, instanceId: "game-server-001" }),
    /does not match this run/
  );
  assert.throws(
    () => buildRegistryCleanupPlan({ ...plan, keyPrefix: "test:*" }),
    /wildcard or control/
  );
});

test("environment variable names are validated before secret lookup", () => {
  assert.equal(validateEnvName("MYSERVER_LOCKSTEP_TICKET"), "MYSERVER_LOCKSTEP_TICKET");
  assert.throws(() => validateEnvName("BAD-NAME"), /invalid name/);
  assert.throws(() => validateEnvName(""), /non-empty/);
});
