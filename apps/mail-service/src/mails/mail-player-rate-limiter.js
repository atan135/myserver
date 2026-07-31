import { createHash } from "node:crypto";

function keyHash(value) {
  return createHash("sha256").update(String(value || "unknown")).digest("hex").slice(0, 40);
}

function retryAfterSeconds(windowMs, ttlMs) {
  const remaining = Number(ttlMs);
  if (Number.isFinite(remaining) && remaining > 0) return Math.max(1, Math.ceil(remaining / 1000));
  return Math.max(1, Math.ceil(windowMs / 1000));
}

function rateLimitError() {
  const error = new Error("mail player rate limit backend is unavailable");
  error.code = "MAIL_RATE_LIMIT_UNAVAILABLE";
  return error;
}

export class MailPlayerRateLimiter {
  constructor(redis, config = {}) {
    this.redis = redis;
    this.config = config;
  }

  enabled() {
    return this.config.mailPublicRateLimitEnabled !== false;
  }

  prefixedKey(key) {
    return `${this.config.redisKeyPrefix || ""}mail-public-rate:${key}`;
  }

  async consume(scope, identity, max) {
    if (!this.enabled()) return { limited: false, retryAfterSeconds: 0, dimension: scope };
    if (!this.redis || typeof this.redis.incr !== "function" || typeof this.redis.pexpire !== "function") {
      throw rateLimitError();
    }

    const windowMs = this.config.mailPublicRateLimitWindowMs || 60_000;
    const key = this.prefixedKey(`${scope}:${keyHash(identity)}`);
    try {
      const count = Number(await this.redis.incr(key));
      if (count === 1) await this.redis.pexpire(key, windowMs);
      if (count <= max) return { limited: false, retryAfterSeconds: 0, dimension: scope };
      const ttlMs = typeof this.redis.pttl === "function" ? await this.redis.pttl(key) : windowMs;
      return {
        limited: true,
        retryAfterSeconds: retryAfterSeconds(windowMs, ttlMs),
        dimension: scope
      };
    } catch (error) {
      if (error?.code === "MAIL_RATE_LIMIT_UNAVAILABLE") throw error;
      throw rateLimitError();
    }
  }

  async check(operation, playerId, clientIp) {
    if (!this.enabled()) return { limited: false, retryAfterSeconds: 0, dimension: "" };
    const isClaim = operation === "claim";
    const perPlayerMax = isClaim
      ? this.config.mailClaimRateLimitPerPlayer
      : this.config.mailReadRateLimitPerPlayer;
    const perIpMax = isClaim
      ? this.config.mailClaimRateLimitPerIp
      : this.config.mailReadRateLimitPerIp;

    for (const [scope, identity, max] of [
      [`${isClaim ? "claim" : "read"}:player`, playerId, perPlayerMax],
      [`${isClaim ? "claim" : "read"}:ip`, clientIp, perIpMax],
      ...(operation === "list" ? [["list-scan:player", playerId, this.config.mailListScanRateLimitPerPlayer]] : [])
    ]) {
      const result = await this.consume(scope, identity, max);
      if (result.limited) return result;
    }
    return { limited: false, retryAfterSeconds: 0, dimension: "" };
  }

  async acquireClaim(playerId) {
    if (!this.enabled()) return { acquired: true, release: async () => undefined };
    if (!this.redis || typeof this.redis.incr !== "function" || typeof this.redis.pexpire !== "function") {
      throw rateLimitError();
    }

    const windowMs = this.config.mailClaimConcurrencyLeaseMs || 15_000;
    const max = this.config.mailClaimConcurrentPerPlayer || 2;
    const key = this.prefixedKey(`claim-concurrency:player:${keyHash(playerId)}`);
    try {
      const count = Number(await this.redis.incr(key));
      if (count === 1) await this.redis.pexpire(key, windowMs);
      if (count > max) {
        if (typeof this.redis.decr === "function") await this.redis.decr(key);
        const ttlMs = typeof this.redis.pttl === "function" ? await this.redis.pttl(key) : windowMs;
        return {
          acquired: false,
          retryAfterSeconds: retryAfterSeconds(windowMs, ttlMs),
          dimension: "claim_concurrency"
        };
      }

      let released = false;
      return {
        acquired: true,
        release: async () => {
          if (released) return;
          released = true;
          try {
            if (typeof this.redis.decr === "function") {
              const remaining = Number(await this.redis.decr(key));
              if (remaining <= 0 && typeof this.redis.del === "function") await this.redis.del(key);
            }
          } catch {
            // The lease expiry bounds a failed best-effort release.
          }
        }
      };
    } catch (error) {
      if (error?.code === "MAIL_RATE_LIMIT_UNAVAILABLE") throw error;
      throw rateLimitError();
    }
  }
}
