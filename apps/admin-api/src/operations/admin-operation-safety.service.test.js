import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { AdminOperationSafetyService } = await import("./admin-operation-safety.service.ts");

const scope = { worldId: "world-1", targetType: "player", targetIds: ["player-1"], targetCount: 1 };

class ControlledRateLimitRedis {
  constructor() {
    this.now = 0;
    this.counters = new Map();
    this.requests = new Map();
  }

  advance(ms) {
    this.now += ms;
  }

  active(map, key) {
    const value = map.get(key);
    if (!value || value.expiresAt <= this.now) {
      map.delete(key);
      return null;
    }
    return value;
  }

  async eval(script, keyCount, counterKey, requestKey, windowMsText, permitTtlText, limitText) {
    assert.equal(keyCount, 2);
    assert.match(script, /request_state == 'deny'/);
    const request = this.active(this.requests, requestKey);
    if (request?.value === "allow") return 0;
    if (request?.value === "deny") return -1;

    const windowMs = Number(windowMsText);
    const permitTtlMs = Number(permitTtlText);
    const limit = Number(limitText);
    const existing = this.active(this.counters, counterKey);
    const counter = existing || { value: 0, expiresAt: this.now + windowMs };
    counter.value += 1;
    this.counters.set(counterKey, counter);
    if (counter.value <= limit) {
      this.requests.set(requestKey, { value: "allow", expiresAt: this.now + permitTtlMs });
    } else {
      this.requests.set(requestKey, { value: "deny", expiresAt: counter.expiresAt });
    }
    return counter.value;
  }
}

test("operation rate-limit rejections are request-id idempotent until the window expires", async () => {
  const redis = new ControlledRateLimitRedis();
  const alerts = [];
  const service = new AdminOperationSafetyService(redis, {
    async appendSecurityAuditLog(event) { alerts.push(event); }
  }, { adminOperationRateLimitMax: 1, adminOperationRateLimitWindowMs: 60000 });
  await service.enforceExecutionRateLimit({ actorAdminId: 7, permission: "gm.send_item", scope, requestId: "request-1" });
  await assert.rejects(
    () => service.enforceExecutionRateLimit({ actorAdminId: 7, permission: "gm.send_item", scope, requestId: "request-2" }),
    (error) => error.code === "ADMIN_OPERATION_RATE_LIMITED"
  );
  const counter = [...redis.counters.values()][0];
  assert.equal(counter.value, 2);
  await assert.rejects(
    () => service.enforceExecutionRateLimit({ actorAdminId: 7, permission: "gm.send_item", scope, requestId: "request-2" }),
    (error) => error.code === "ADMIN_OPERATION_RATE_LIMITED"
  );
  assert.equal([...redis.counters.values()][0].value, 2);
  assert.equal(alerts.length, 1);
  assert.equal(alerts[0].eventType, "admin_operation_rate_limited");
  assert.equal(alerts[0].details.requestId, "request-2");
  assert.equal(alerts[0].details.permission, "gm.send_item");
  assert.match(alerts[0].details.scopeFingerprint, /^[0-9a-f]{24}$/);

  redis.advance(60000);
  const reEvaluated = await service.enforceExecutionRateLimit({
    actorAdminId: 7,
    permission: "gm.send_item",
    scope,
    requestId: "request-2"
  });
  assert.equal(reEvaluated.count, 1);
  assert.equal(alerts.length, 1);
});

test("accepted request_id retries do not consume another rate-limit count", async () => {
  const redis = new ControlledRateLimitRedis();
  const service = new AdminOperationSafetyService(redis, {
    async appendSecurityAuditLog() {}
  }, { adminOperationRateLimitMax: 1, adminOperationRateLimitWindowMs: 60000 });
  await service.enforceExecutionRateLimit({ actorAdminId: 7, permission: "gm.send_item", scope, requestId: "request-1" });
  const duplicate = await service.enforceExecutionRateLimit({ actorAdminId: 7, permission: "gm.send_item", scope, requestId: "request-1" });
  assert.equal(duplicate.duplicate, true);
  assert.equal([...redis.counters.values()][0].value, 1);
});

test("rate-limit and security-audit dependency failures are fail-closed", async () => {
  const unavailableRateLimit = new AdminOperationSafetyService({}, {
    async appendSecurityAuditLog() {}
  }, {});
  await assert.rejects(
    () => unavailableRateLimit.enforceExecutionRateLimit({ actorAdminId: 7, permission: "gm.send_item", scope, requestId: "request-1" }),
    (error) => error.code === "ADMIN_RATE_LIMIT_DEPENDENCY_UNAVAILABLE"
  );

  const unavailableAudit = new AdminOperationSafetyService({
    async eval() { return 2; }
  }, {}, { adminOperationRateLimitMax: 1, adminOperationRateLimitWindowMs: 60000 });
  await assert.rejects(
    () => unavailableAudit.enforceExecutionRateLimit({ actorAdminId: 7, permission: "gm.send_item", scope, requestId: "request-1" }),
    (error) => error.code === "ADMIN_SECURITY_AUDIT_UNAVAILABLE"
  );
});
