export const IP_BLOCKED_ERROR = "IP_BLOCKED";
export const PLAYER_BLOCKED_ERROR = "PLAYER_BLOCKED";
export const BLOCKLIST_UNAVAILABLE_ERROR = "BLOCKLIST_UNAVAILABLE";

export function blocklistIpKey(redisKeyPrefix, ip) {
  return `${redisKeyPrefix || ""}security:blocklist:ip:${ip}`;
}

export function blocklistPlayerKey(redisKeyPrefix, playerId) {
  return `${redisKeyPrefix || ""}security:blocklist:player:${playerId}`;
}

export function parseBlocklistDecision(raw, nowUnixMs = Date.now(), blockedError) {
  if (raw === null || raw === undefined) {
    return { blocked: false };
  }

  try {
    const entry = JSON.parse(raw);
    if (
      entry &&
      typeof entry === "object" &&
      Number.isFinite(entry.until) &&
      entry.until < nowUnixMs
    ) {
      return { blocked: false };
    }
  } catch {
    // Any existing non-JSON value means blocked.
  }

  return { blocked: true, error: blockedError };
}

export class RedisBlocklistChecker {
  constructor(config, redis) {
    this.enabled = Boolean(config?.authRedisBlocklistEnabled);
    this.redis = redis;
    this.keyPrefix = config?.redisKeyPrefix || "";
    this.cacheTtlMs = Math.max(0, Number(config?.authRedisBlocklistCacheTtlMs || 0));
    this.cache = new Map();
  }

  static disabled() {
    return new RedisBlocklistChecker({ authRedisBlocklistEnabled: false }, null);
  }

  async checkIp(ip) {
    return this.check(`ip:${ip}`, blocklistIpKey(this.keyPrefix, ip), IP_BLOCKED_ERROR);
  }

  async checkPlayer(playerId) {
    return this.check(
      `player:${playerId}`,
      blocklistPlayerKey(this.keyPrefix, playerId),
      PLAYER_BLOCKED_ERROR
    );
  }

  async check(cacheKey, redisKey, blockedError) {
    if (!this.enabled) {
      return { blocked: false };
    }

    const now = Date.now();
    const cached = this.cache.get(cacheKey);
    if (cached && cached.expiresAt > now) {
      return cached.decision;
    }

    let raw;
    try {
      raw = await this.redis.get(redisKey);
    } catch {
      return { blocked: true, error: BLOCKLIST_UNAVAILABLE_ERROR, unavailable: true };
    }

    const decision = parseBlocklistDecision(raw, now, blockedError);
    this.cache.set(cacheKey, {
      decision,
      expiresAt: now + this.cacheTtlMs
    });
    return decision;
  }
}
