import crypto from "node:crypto";

function safeParseJson(value: string | null): any | null {
  if (!value) {
    return null;
  }

  try {
    return JSON.parse(value);
  } catch {
    return null;
  }
}

function normalizeUsername(username: string): string {
  return username.trim().toLowerCase();
}

export class AdminSessionStore {
  private readonly redis: any;
  private readonly keyPrefix: string;

  constructor(redis: any, config: any) {
    this.redis = redis;
    this.keyPrefix = config.redisKeyPrefix || "";
  }

  key(key: string): string {
    return `${this.keyPrefix}${key}`;
  }

  createJti(): string {
    return crypto.randomUUID();
  }

  async getTokenVersion(adminId: number | string): Promise<number> {
    const value = await this.redis.get(this.key(`admin:token-version:${adminId}`));
    const parsed = Number.parseInt(value || "0", 10);
    return Number.isFinite(parsed) && parsed >= 0 ? parsed : 0;
  }

  async bumpTokenVersion(adminId: number | string): Promise<number> {
    return this.redis.incr(this.key(`admin:token-version:${adminId}`));
  }

  async createSession({
    adminId,
    username,
    role,
    jti,
    tokenVersion,
    clientIp,
    ttlSeconds
  }: {
    adminId: number | string;
    username: string;
    role: string;
    jti: string;
    tokenVersion: number;
    clientIp: string | null;
    ttlSeconds: number;
  }) {
    await this.redis.set(
      this.key(`admin:session:${jti}`),
      JSON.stringify({
        adminId,
        username,
        role,
        tokenVersion,
        clientIp,
        createdAt: new Date().toISOString()
      }),
      "EX",
      ttlSeconds
    );
  }

  async getSession(jti: string): Promise<any | null> {
    return safeParseJson(await this.redis.get(this.key(`admin:session:${jti}`)));
  }

  async deleteSession(jti: string) {
    await this.redis.del(this.key(`admin:session:${jti}`));
  }

  loginFailureKey(username: string, clientIp: string | null): string {
    const ipPart = clientIp || "unknown";
    return this.key(`admin:login-fail:${normalizeUsername(username)}:${ipPart}`);
  }

  loginLockKey(username: string, clientIp: string | null): string {
    const ipPart = clientIp || "unknown";
    return this.key(`admin:login-lock:${normalizeUsername(username)}:${ipPart}`);
  }

  async getLoginLock(username: string, clientIp: string | null): Promise<{ locked: boolean; remainingSeconds: number }> {
    const ttl = await this.redis.ttl(this.loginLockKey(username, clientIp));
    return {
      locked: ttl > 0,
      remainingSeconds: ttl > 0 ? ttl : 0
    };
  }

  async recordLoginFailure(username: string, clientIp: string | null, config: any): Promise<number> {
    const failureKey = this.loginFailureKey(username, clientIp);
    const attempts = await this.redis.incr(failureKey);

    if (attempts === 1) {
      await this.redis.expire(failureKey, config.adminLoginFailureWindowSeconds);
    }

    if (attempts >= config.adminLoginMaxFailures) {
      await this.redis.set(
        this.loginLockKey(username, clientIp),
        String(attempts),
        "EX",
        config.adminLoginLockSeconds
      );
    }

    return attempts;
  }

  async clearLoginFailures(username: string, clientIp: string | null) {
    await this.redis.del(this.loginFailureKey(username, clientIp), this.loginLockKey(username, clientIp));
  }
}
