import test from "node:test";
import assert from "node:assert/strict";

import {
  composeGlobalId,
  createGlobalIdGeneratorFromEnv,
  decodeGlobalIdInput,
  decodeNumericGlobalId,
  encodeBase32,
  decodeBase32,
  encodeGlobalId,
  acquireRedisWorkerLease,
  DEFAULT_WORKER_LEASE_RENEW_INTERVAL_MS,
  DEFAULT_WORKER_LEASE_TTL_SECONDS,
  workerLeaseRenewIntervalMs,
  workerLeaseKey,
  lastTimestampKey,
  originMetadataKey
} from "./index.js";

test("compose and decode numeric id", () => {
  const id = composeGlobalId({ timeMs: 123n, originId: 45n, workerId: 6n, sequence: 7n });
  const decoded = decodeNumericGlobalId(id);
  assert.equal(decoded.timeMs, "123");
  assert.equal(decoded.originId, 45);
  assert.equal(decoded.workerId, 6);
  assert.equal(decoded.sequence, 7);
});

test("base32 round trip", () => {
  const id = composeGlobalId({ timeMs: 987654n, originId: 12n, workerId: 3n, sequence: 4n });
  const encoded = encodeBase32(id);
  assert.equal(decodeBase32(encoded), id);
});

test("prefixed decode", () => {
  const id = composeGlobalId({ timeMs: 1n, originId: 2n, workerId: 3n, sequence: 4n });
  const encoded = encodeGlobalId("plr", id);
  const decoded = decodeGlobalIdInput(encoded);
  assert.equal(decoded.normalizedId, encoded);
  assert.equal(decoded.idKind, "player");
  assert.equal(decoded.originId, 2);
});

test("character prefixed decode", () => {
  const id = composeGlobalId({ timeMs: 2n, originId: 3n, workerId: 4n, sequence: 5n });
  const encoded = encodeGlobalId("chr", id);
  const decoded = decodeGlobalIdInput(encoded);
  assert.equal(decoded.normalizedId, encoded);
  assert.equal(decoded.idKind, "character");
  assert.equal(decoded.prefix, "chr");
});

test("numeric decode is treated as item uid", () => {
  const id = composeGlobalId({ timeMs: 1n, originId: 2n, workerId: 3n, sequence: 4n });
  const decoded = decodeGlobalIdInput(id.toString());
  assert.equal(decoded.idKind, "item");
  assert.equal(decoded.originId, 2);
  assert.equal(decoded.workerId, 3);
});

test("generator uses env origin and worker", () => {
  const generator = createGlobalIdGeneratorFromEnv({
    prefix: "mail",
    env: {
      GLOBAL_ID_ORIGIN_ID: "5",
      GLOBAL_ID_WORKER_ID: "6"
    },
    now: () => Number(1767225600000n + 1000n)
  });

  const id = generator.generateString();
  const decoded = decodeGlobalIdInput(id);
  assert.equal(decoded.originId, 5);
  assert.equal(decoded.workerId, 6);
});

test("redis key helpers use bounded ids", () => {
  assert.equal(workerLeaseKey(1, 2), "id:worker:1:2");
  assert.equal(lastTimestampKey(1, 2), "id:last-ts:1:2");
  assert.equal(originMetadataKey(1), "id:origin:1");
});

test("worker lease renewal interval follows its TTL", () => {
  assert.equal(DEFAULT_WORKER_LEASE_TTL_SECONDS, 90);
  assert.equal(DEFAULT_WORKER_LEASE_RENEW_INTERVAL_MS, 30_000);
  assert.equal(workerLeaseRenewIntervalMs(9), 3_000);
});

test("redis worker lease claims, rejects conflict, and releases by token", async () => {
  const values = new Map();
  const redis = {
    async set(key, value, _ex, _ttl, mode) {
      if (mode === "NX" && values.has(key)) {
        return null;
      }
      values.set(key, value);
      return "OK";
    },
    async eval(_script, _keyCount, key, value) {
      if (values.get(key) !== value) {
        return 0;
      }
      values.delete(key);
      return 1;
    }
  };

  const lease = await acquireRedisWorkerLease({
    redis,
    originId: 1,
    workerId: 2,
    serviceName: "test",
    serviceInstanceId: "test-1",
    ttlSeconds: 30
  });

  assert.equal(lease.workerId, 2n);
  await assert.rejects(
    () => acquireRedisWorkerLease({
      redis,
      originId: 1,
      workerId: 2,
      serviceName: "test",
      serviceInstanceId: "test-2",
      ttlSeconds: 30
    }),
    /worker lease is already held/
  );

  assert.equal(await lease.release(), true);
  const nextLease = await acquireRedisWorkerLease({
    redis,
    originId: 1,
    workerId: 2,
    serviceName: "test",
    serviceInstanceId: "test-3",
    ttlSeconds: 30
  });
  assert.equal(await nextLease.release(), true);
});

test("redis worker lease generator rejects after lease release", async () => {
  const values = new Map();
  const redis = {
    async set(key, value, _ex, _ttl, mode) {
      if (mode === "NX" && values.has(key)) {
        return null;
      }
      values.set(key, value);
      return "OK";
    },
    async eval(_script, _keyCount, key, value) {
      if (values.get(key) !== value) {
        return 0;
      }
      values.delete(key);
      return 1;
    }
  };

  const lease = await acquireRedisWorkerLease({
    redis,
    originId: 1,
    workerId: 2,
    serviceName: "test",
    serviceInstanceId: "test-1",
    ttlSeconds: 30
  });
  const generator = lease.createGenerator({ prefix: "plr" });

  assert.match(generator.generateString(), /^plr_/);
  assert.equal(await lease.release(), true);
  assert.throws(() => generator.generateString(), /worker lease is no longer active/);
});

test("redis worker lease reports ownership loss once and never on normal release", async () => {
  const values = new Map();
  let ownershipLost = false;
  const losses = [];
  const redis = {
    async set(key, value, _ex, _ttl, mode) {
      if (mode === "NX" && values.has(key)) {
        return null;
      }
      values.set(key, value);
      return "OK";
    },
    async eval(script, _keyCount, key, value) {
      if (script.includes('redis.call("SET"')) {
        return ownershipLost || values.get(key) !== value ? 0 : 1;
      }
      if (values.get(key) !== value) {
        return 0;
      }
      values.delete(key);
      return 1;
    }
  };

  const lease = await acquireRedisWorkerLease({
    redis,
    originId: 1,
    workerId: 2,
    serviceName: "test",
    serviceInstanceId: "test-1",
    onLeaseLost: (details) => losses.push(details)
  });

  ownershipLost = true;
  assert.equal(await lease.renew(), false);
  await Promise.resolve();
  assert.equal(losses.length, 1);
  assert.equal(losses[0].key, "id:worker:1:2");
  assert.equal(await lease.renew(), false);
  assert.equal(losses.length, 1);

  const healthyLease = await acquireRedisWorkerLease({
    redis,
    originId: 1,
    workerId: 3,
    serviceName: "test",
    serviceInstanceId: "test-2",
    onLeaseLost: (details) => losses.push(details)
  });
  assert.equal(await healthyLease.release(), true);
  assert.equal(losses.length, 1);
});

test("redis worker lease reports a Redis renewal error", async () => {
  const redis = {
    async set() {
      return "OK";
    },
    async eval() {
      throw new Error("Redis unavailable");
    }
  };
  const losses = [];
  const lease = await acquireRedisWorkerLease({
    redis,
    originId: 1,
    workerId: 2,
    serviceName: "test",
    serviceInstanceId: "test-1",
    onLeaseLost: (details) => losses.push(details)
  });

  await assert.rejects(() => lease.renew(), /Redis unavailable/);
  await Promise.resolve();
  assert.equal(losses.length, 1);
  assert.match(losses[0].error.message, /Redis unavailable/);
});
