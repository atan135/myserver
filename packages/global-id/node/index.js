export const EPOCH_MS = 1767225600000n;
export const TIME_BITS = 41n;
export const ORIGIN_BITS = 10n;
export const WORKER_BITS = 6n;
export const SEQUENCE_BITS = 6n;
export const MAX_ORIGIN_ID = (1n << ORIGIN_BITS) - 1n;
export const MAX_WORKER_ID = (1n << WORKER_BITS) - 1n;
export const MAX_SEQUENCE = (1n << SEQUENCE_BITS) - 1n;
export const WORKER_SHIFT = SEQUENCE_BITS;
export const ORIGIN_SHIFT = WORKER_BITS + SEQUENCE_BITS;
export const TIME_SHIFT = ORIGIN_BITS + WORKER_BITS + SEQUENCE_BITS;
export const MAX_CLOCK_BACKWARD_MS = 5n;
export const DEFAULT_WORKER_LEASE_TTL_SECONDS = 30;
export const DEFAULT_WORKER_LEASE_RENEW_INTERVAL_MS = 10000;

const BASE32_ALPHABET = "0123456789abcdefghjkmnpqrstvwxyz";
const KIND_BY_PREFIX = new Map([
  ["plr", "player"],
  ["room", "room"],
  ["mail", "mail"],
  ["ann", "announcement"],
  ["msg", "chat_message"],
  ["grp", "chat_group"],
  ["req", "request"]
]);

export class GlobalIdError extends Error {
  constructor(code, message, details = {}) {
    super(message);
    this.name = "GlobalIdError";
    this.code = code;
    this.details = details;
  }
}

export class GlobalIdGenerator {
  constructor({ originId = 0, workerId = 0, prefix = null, now = () => Date.now(), leaseState = null } = {}) {
    this.originId = parseOriginId(originId);
    this.workerId = parseWorkerId(workerId);
    this.prefix = prefix;
    this.now = now;
    this.leaseState = leaseState;
    this.lastTimeMs = -1n;
    this.sequence = 0n;
  }

  generate() {
    this.assertLeaseActive();
    while (true) {
      this.assertLeaseActive();
      let nowMs = BigInt(this.now()) - EPOCH_MS;
      if (nowMs < 0n) {
        throw new GlobalIdError("CLOCK_BEFORE_EPOCH", "system clock is before global id epoch");
      }

      if (nowMs < this.lastTimeMs) {
        const drift = this.lastTimeMs - nowMs;
        if (drift <= MAX_CLOCK_BACKWARD_MS) {
          nowMs = this.lastTimeMs;
        } else {
          throw new GlobalIdError("CLOCK_MOVED_BACKWARD", "system clock moved backward beyond tolerance", {
            lastTimeMs: this.lastTimeMs.toString(),
            nowMs: nowMs.toString()
          });
        }
      }

      if (nowMs === this.lastTimeMs) {
        if (this.sequence < MAX_SEQUENCE) {
          this.sequence += 1n;
        } else {
          waitNextMillis(this.now, nowMs);
          continue;
        }
      } else {
        this.sequence = 0n;
      }

      this.lastTimeMs = nowMs;
      return composeGlobalId({
        timeMs: nowMs,
        originId: this.originId,
        workerId: this.workerId,
        sequence: this.sequence
      });
    }
  }

  assertLeaseActive() {
    if (this.leaseState && !this.leaseState.active) {
      throw new GlobalIdError(
        "WORKER_LEASE_INACTIVE",
        `worker lease is no longer active: ${this.leaseState.key || "<unknown>"}`
      );
    }
  }

  generateNumericString() {
    return this.generate().toString();
  }

  generateString(prefix = this.prefix) {
    if (!prefix) {
      throw new GlobalIdError("MISSING_PREFIX", "global id prefix is required");
    }
    return encodeGlobalId(prefix, this.generate());
  }
}

export function createGlobalIdGeneratorFromEnv({ prefix = null, env = process.env, now } = {}) {
  return new GlobalIdGenerator({
    originId: env.GLOBAL_ID_ORIGIN_ID ?? "0",
    workerId: env.GLOBAL_ID_WORKER_ID ?? "0",
    prefix,
    now
  });
}

export async function acquireRedisWorkerLease({
  redis,
  originId = process.env.GLOBAL_ID_ORIGIN_ID ?? "0",
  workerId = process.env.GLOBAL_ID_WORKER_ID,
  serviceName = "unknown-service",
  serviceInstanceId = `${serviceName}-${process.pid}`,
  redisKeyPrefix = "",
  ttlSeconds = DEFAULT_WORKER_LEASE_TTL_SECONDS,
  renewIntervalMs = DEFAULT_WORKER_LEASE_RENEW_INTERVAL_MS
} = {}) {
  if (!redis || typeof redis.set !== "function") {
    throw new GlobalIdError("WORKER_LEASE_REDIS_UNAVAILABLE", "redis client is required for worker lease");
  }

  const origin = parseOriginId(originId);
  const explicitWorker = workerId !== undefined && workerId !== null && String(workerId).trim() !== "";
  const candidates = explicitWorker
    ? [parseWorkerId(workerId)]
    : Array.from({ length: Number(MAX_WORKER_ID) + 1 }, (_, index) => BigInt(index));
  const ttl = parsePositiveInteger(ttlSeconds, "INVALID_WORKER_LEASE_TTL_SECONDS");
  const renewMs = parsePositiveInteger(renewIntervalMs, "INVALID_WORKER_LEASE_RENEW_INTERVAL_MS");
  const token = `${serviceName}:${serviceInstanceId}:${process.pid}:${Date.now()}:${Math.random().toString(16).slice(2)}`;
  const acquiredAt = new Date().toISOString();

  for (const candidate of candidates) {
    const key = `${redisKeyPrefix || ""}${workerLeaseKey(origin, candidate)}`;
    const value = JSON.stringify({
      token,
      serviceName,
      serviceInstanceId,
      originId: Number(origin),
      workerId: Number(candidate),
      pid: process.pid,
      acquiredAt
    });
    const result = await redis.set(key, value, "EX", ttl, "NX");
    if (isRedisSetOk(result)) {
      return new RedisWorkerLease({
        redis,
        key,
        value,
        originId: origin,
        workerId: candidate,
        ttlSeconds: ttl,
        renewIntervalMs: renewMs
      });
    }
  }

  throw new GlobalIdError(
    "WORKER_LEASE_UNAVAILABLE",
    explicitWorker
      ? `worker lease is already held: origin=${origin} worker=${candidates[0]}`
      : `no available worker lease for origin=${origin}`
  );
}

export class RedisWorkerLease {
  constructor({ redis, key, value, originId, workerId, ttlSeconds, renewIntervalMs }) {
    this.redis = redis;
    this.key = key;
    this.value = value;
    this.originId = originId;
    this.workerId = workerId;
    this.ttlSeconds = ttlSeconds;
    this.active = true;
    this.renewTimer = setInterval(() => {
      this.renew().catch(() => {});
    }, renewIntervalMs);
    this.renewTimer.unref?.();
  }

  createGenerator({ prefix = null, now } = {}) {
    return new GlobalIdGenerator({
      originId: this.originId,
      workerId: this.workerId,
      prefix,
      now,
      leaseState: this
    });
  }

  async renew() {
    if (!this.active) {
      return false;
    }
    const script = `
      if redis.call("GET", KEYS[1]) == ARGV[1] then
        redis.call("SET", KEYS[1], ARGV[1], "EX", tonumber(ARGV[2]))
        return 1
      end
      return 0
    `;
    let result;
    try {
      result = await this.redis.eval(script, 1, this.key, this.value, String(this.ttlSeconds));
    } catch (error) {
      this.deactivate();
      throw error;
    }
    const renewed = Number(result) === 1;
    if (!renewed) {
      this.deactivate();
    }
    return renewed;
  }

  async release() {
    if (!this.active) {
      return false;
    }
    this.deactivate();
    const script = `
      if redis.call("GET", KEYS[1]) == ARGV[1] then
        return redis.call("DEL", KEYS[1])
      end
      return 0
    `;
    const result = await this.redis.eval(script, 1, this.key, this.value);
    return Number(result) === 1;
  }

  deactivate() {
    if (!this.active) {
      return;
    }
    this.active = false;
    clearInterval(this.renewTimer);
  }
}

export function workerLeaseKey(originId, workerId) {
  const origin = parseOriginId(originId);
  const worker = parseWorkerId(workerId);
  return `id:worker:${origin}:${worker}`;
}

export function lastTimestampKey(originId, workerId) {
  const origin = parseOriginId(originId);
  const worker = parseWorkerId(workerId);
  return `id:last-ts:${origin}:${worker}`;
}

export function originMetadataKey(originId) {
  const origin = parseOriginId(originId);
  return `id:origin:${origin}`;
}

export function parseOriginId(value) {
  const parsed = parseBoundedInteger(value, "INVALID_ORIGIN_ID", 0n, MAX_ORIGIN_ID);
  return parsed;
}

export function parseWorkerId(value) {
  const parsed = parseBoundedInteger(value, "INVALID_WORKER_ID", 0n, MAX_WORKER_ID);
  return parsed;
}

export function composeGlobalId({ timeMs, originId, workerId, sequence }) {
  const time = parseBoundedInteger(timeMs, "INVALID_TIME_MS", 0n, (1n << TIME_BITS) - 1n);
  const origin = parseBoundedInteger(originId, "INVALID_ORIGIN_ID", 0n, MAX_ORIGIN_ID);
  const worker = parseBoundedInteger(workerId, "INVALID_WORKER_ID", 0n, MAX_WORKER_ID);
  const seq = parseBoundedInteger(sequence, "INVALID_SEQUENCE", 0n, MAX_SEQUENCE);
  return (time << TIME_SHIFT) | (origin << ORIGIN_SHIFT) | (worker << WORKER_SHIFT) | seq;
}

export function decodeNumericGlobalId(id) {
  const numericId = parseBoundedInteger(id, "INVALID_GLOBAL_ID", 0n, (1n << 63n) - 1n);
  const sequence = numericId & ((1n << SEQUENCE_BITS) - 1n);
  const workerId = (numericId >> WORKER_SHIFT) & ((1n << WORKER_BITS) - 1n);
  const originId = (numericId >> ORIGIN_SHIFT) & ((1n << ORIGIN_BITS) - 1n);
  const timeMs = numericId >> TIME_SHIFT;
  const unixMs = EPOCH_MS + timeMs;

  return {
    numericId: numericId.toString(),
    timeMs: timeMs.toString(),
    unixMs: unixMs.toString(),
    createdAt: new Date(Number(unixMs)).toISOString(),
    originId: Number(originId),
    workerId: Number(workerId),
    sequence: Number(sequence)
  };
}

export function encodeBase32(value) {
  let current = parseBoundedInteger(value, "INVALID_GLOBAL_ID", 0n, (1n << 63n) - 1n);
  if (current === 0n) {
    return "0";
  }

  let output = "";
  while (current > 0n) {
    const idx = Number(current & 31n);
    output = BASE32_ALPHABET[idx] + output;
    current >>= 5n;
  }
  return output;
}

export function decodeBase32(value) {
  const raw = String(value ?? "").trim().toLowerCase();
  if (!raw) {
    throw new GlobalIdError("INVALID_BASE32", "base32 value is empty");
  }

  let result = 0n;
  for (const char of raw) {
    const idx = BASE32_ALPHABET.indexOf(char);
    if (idx < 0) {
      throw new GlobalIdError("INVALID_BASE32", `invalid base32 character: ${char}`);
    }
    result = result * 32n + BigInt(idx);
  }
  return result;
}

export function encodeGlobalId(prefix, id) {
  validatePrefix(prefix);
  return `${prefix}_${encodeBase32(id)}`;
}

export function decodeGlobalIdInput(input) {
  const rawId = String(input ?? "").trim();
  if (!rawId) {
    throw new GlobalIdError("INVALID_GLOBAL_ID", "global id is required");
  }

  if (/^[0-9]+$/.test(rawId)) {
    const decoded = decodeNumericGlobalId(rawId);
    return {
      rawId,
      normalizedId: rawId,
      idKind: "item",
      prefix: null,
      ...decoded
    };
  }

  const separator = rawId.indexOf("_");
  if (separator <= 0 || separator === rawId.length - 1) {
    throw new GlobalIdError("INVALID_GLOBAL_ID", "global id must be numeric or prefixed base32");
  }

  const prefix = rawId.slice(0, separator);
  const encoded = rawId.slice(separator + 1);
  validatePrefix(prefix);
  if (!KIND_BY_PREFIX.has(prefix)) {
    throw new GlobalIdError("UNKNOWN_GLOBAL_ID_PREFIX", `unknown global id prefix: ${prefix}`);
  }

  const numericId = decodeBase32(encoded);
  const decoded = decodeNumericGlobalId(numericId);
  return {
    rawId,
    normalizedId: encodeGlobalId(prefix, numericId),
    idKind: KIND_BY_PREFIX.get(prefix),
    prefix,
    ...decoded
  };
}

function parseBoundedInteger(value, code, min, max) {
  let parsed;
  try {
    parsed = typeof value === "bigint" ? value : BigInt(String(value).trim());
  } catch {
    throw new GlobalIdError(code, `invalid integer: ${value}`);
  }

  if (parsed < min || parsed > max) {
    throw new GlobalIdError(code, `integer out of range: ${value}`, {
      min: min.toString(),
      max: max.toString()
    });
  }
  return parsed;
}

function parsePositiveInteger(value, code) {
  const parsed = Number.parseInt(String(value), 10);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new GlobalIdError(code, `invalid positive integer: ${value}`);
  }
  return parsed;
}

function isRedisSetOk(result) {
  return result === "OK" || result === true;
}

function validatePrefix(prefix) {
  const raw = String(prefix ?? "");
  if (!/^[a-z0-9]+$/.test(raw)) {
    throw new GlobalIdError("INVALID_PREFIX", `invalid global id prefix: ${prefix}`);
  }
}

function waitNextMillis(now, currentMs) {
  while (BigInt(now()) - EPOCH_MS <= currentMs) {
    // Busy wait is acceptable here because it only occurs after 64 IDs in one millisecond per worker.
  }
}
