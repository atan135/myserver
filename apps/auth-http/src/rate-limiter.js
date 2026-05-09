/**
 * Rate Limiter & Security Module
 * - IP rate limiting (sliding window)
 * - Account lockout tracking
 * - Security event logging
 */

export class RateLimiter {
  constructor(redis, config) {
    this.redis = redis;
    this.config = config;
  }

  prefixedKey(key) {
    return `${this.config.redisKeyPrefix || ""}${key}`;
  }

  async isIpRateLimited(ip) {
    if (!this.config.ratelimitEnabled) {
      return { limited: false, retryAfterSeconds: 0 };
    }

    const key = this.prefixedKey(`ratelimit:ip:${ip}`);
    const now = Date.now();
    const windowMs = this.config.ratelimitWindowMs;
    const max = this.config.ratelimitMax;

    // Sliding window using sorted set
    const pipeline = this.redis.pipeline();
    pipeline.zremrangebyscore(key, 0, now - windowMs);
    pipeline.zadd(key, now, `${now}-${Math.random()}`);
    pipeline.zcard(key);
    pipeline.expire(key, Math.ceil(windowMs / 1000) + 1);
    pipeline.zrange(key, 0, 0, "WITHSCORES");
    const results = await pipeline.exec();

    const count = results[2][1];
    if (count <= max) {
      return { limited: false, retryAfterSeconds: 0 };
    }

    // Calculate retry-after from oldest entry in window
    const zrangeResult = results[4][1];
    let retryAfterSeconds = Math.ceil(windowMs / 1000);
    if (zrangeResult && zrangeResult.length >= 2) {
      const oldestTs = Number(zrangeResult[1]);
      const expiresAt = oldestTs + windowMs;
      retryAfterSeconds = Math.max(1, Math.ceil((expiresAt - now) / 1000));
    }

    return { limited: true, retryAfterSeconds };
  }

  async getIpRequestCount(ip) {
    const key = this.prefixedKey(`ratelimit:ip:${ip}`);
    const now = Date.now();
    const windowMs = this.config.ratelimitWindowMs;

    await this.redis.zremrangebyscore(key, 0, now - windowMs);
    return this.redis.zcard(key);
  }

  async resetIpRateLimit(ip) {
    const key = this.prefixedKey(`ratelimit:ip:${ip}`);
    await this.redis.del(key);
  }
}

export class AccountLockout {
  constructor(redis, config) {
    this.redis = redis;
    this.config = config;
  }

  prefixedKey(key) {
    return `${this.config.redisKeyPrefix || ""}${key}`;
  }

  async isLocked(loginName) {
    if (!this.config.accountLockEnabled) {
      return false;
    }

    const key = this.prefixedKey(`account:lock:${loginName}`);
    const locked = await this.redis.exists(key);
    return locked === 1;
  }

  async recordFailedAttempt(loginName) {
    if (!this.config.accountLockEnabled) {
      return { locked: false, attempts: 0 };
    }

    const key = this.prefixedKey(`account:lock:${loginName}`);
    const attempts = await this.redis.incr(key);

    if (attempts === 1) {
      // First failure, set expiry for the lock window
      await this.redis.expire(key, this.config.accountLockWindowSeconds);
    }

    if (attempts >= this.config.accountLockMaxAttempts) {
      // Lock the account
      await this.redis.setex(
        this.prefixedKey(`account:locked:${loginName}`),
        this.config.accountLockTtlSeconds,
        "1"
      );
      await this.redis.del(key);
      return { locked: true, attempts };
    }

    return { locked: false, attempts };
  }

  async clearFailedAttempts(loginName) {
    const key = this.prefixedKey(`account:lock:${loginName}`);
    await this.redis.del(key);
  }

  async getLockStatus(loginName) {
    const lockedKey = this.prefixedKey(`account:locked:${loginName}`);
    const ttl = await this.redis.ttl(lockedKey);

    if (ttl > 0) {
      return { locked: true, remainingSeconds: ttl };
    }

    return { locked: false, remainingSeconds: 0 };
  }
}

export class TicketValidator {
  constructor(config) {
    this.config = config;
  }

  /**
   * Validate ticket signature and integrity
   * @returns {object} { valid: boolean, error?: string, playerId?: string }
   */
  validate(ticket) {
    if (!ticket || typeof ticket !== "string") {
      return { valid: false, error: "INVALID_TICKET_FORMAT" };
    }

    const parts = ticket.split(".");
    if (parts.length !== 2) {
      return { valid: false, error: "INVALID_TICKET_FORMAT" };
    }

    const [payloadB64, signature] = parts;

    // Verify signature
    const expectedSignature = this._signPayload(payloadB64);
    if (signature !== expectedSignature) {
      return { valid: false, error: "INVALID_TICKET_SIGNATURE" };
    }

    // Decode and validate payload
    try {
      const payload = JSON.parse(Buffer.from(payloadB64, "base64url").toString());

      if (!payload.playerId || !payload.exp) {
        return { valid: false, error: "INVALID_TICKET_PAYLOAD" };
      }

      const expiresAt = new Date(payload.exp).getTime();
      if (Date.now() > expiresAt) {
        return { valid: false, error: "TICKET_EXPIRED" };
      }

      return { valid: true, playerId: payload.playerId };
    } catch {
      return { valid: false, error: "INVALID_TICKET_PAYLOAD" };
    }
  }

  _signPayload(payloadB64) {
    const crypto = require("node:crypto");
    return crypto
      .createHmac("sha256", this.config.ticketSecret)
      .update(payloadB64)
      .digest("base64url");
  }
}
