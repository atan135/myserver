import assert from "node:assert/strict";
import { register } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";

import { JwtService } from "@nestjs/jwt";

process.env.TS_NODE_PROJECT = path.resolve("apps/admin-api/tsconfig.json");
process.env.TS_NODE_TRANSPILE_ONLY = "true";
register("ts-node/esm", pathToFileURL("./"));

const { AuthService } = await import("../../apps/admin-api/src/auth/auth.service.ts");
const { JwtAuthGuard } = await import("../../apps/admin-api/src/auth/jwt-auth.guard.ts");
const { AdminSessionStore } = await import("../../apps/admin-api/src/auth/admin-session-store.ts");
const { getClientIp } = await import("../../apps/admin-api/src/common/client-ip.ts");
const { getConfig } = await import("../../apps/admin-api/src/config.js");

class MemoryRedis {
  constructor() {
    this.values = new Map();
  }

  async get(key) {
    const record = this.values.get(key);
    if (!record) return null;
    if (record.expiresAt && record.expiresAt <= Date.now()) {
      this.values.delete(key);
      return null;
    }
    return record.value;
  }

  async set(key, value, mode, seconds) {
    this.values.set(key, {
      value: String(value),
      expiresAt: mode === "EX" ? Date.now() + Number(seconds) * 1000 : null
    });
  }

  async incr(key) {
    const current = Number.parseInt((await this.get(key)) || "0", 10);
    const next = current + 1;
    this.values.set(key, { value: String(next), expiresAt: this.values.get(key)?.expiresAt || null });
    return next;
  }

  async expire(key, seconds) {
    const record = this.values.get(key);
    if (record) {
      record.expiresAt = Date.now() + Number(seconds) * 1000;
    }
  }

  async ttl(key) {
    const record = this.values.get(key);
    if (!record) return -2;
    if (!record.expiresAt) return -1;
    const remaining = Math.ceil((record.expiresAt - Date.now()) / 1000);
    if (remaining <= 0) {
      this.values.delete(key);
      return -2;
    }
    return remaining;
  }

  async del(...keys) {
    let deleted = 0;
    for (const key of keys) {
      if (this.values.delete(key)) deleted += 1;
    }
    return deleted;
  }
}

function createAdminStore() {
  const admin = {
    id: 1,
    username: "admin",
    displayName: "Administrator",
    role: "admin",
    status: "active",
    passwordHash: "hash"
  };
  const auditLogs = [];
  const securityLogs = [];

  return {
    admin,
    auditLogs,
    securityLogs,
    async findAdminByUsername(username) {
      return username === admin.username ? admin : null;
    },
    async verifyPassword(password) {
      return password === "correct-password";
    },
    async updateLastLogin() {},
    async appendAuditLog(event) {
      auditLogs.push(event);
    },
    async appendSecurityAuditLog(event) {
      securityLogs.push(event);
    }
  };
}

function executionContext(req) {
  return {
    getHandler() {
      return null;
    },
    switchToHttp() {
      return {
        getRequest() {
          return req;
        }
      };
    }
  };
}

const baseConfig = {
  jwtSecret: "test-admin-jwt-secret",
  jwtExpiresIn: "8h",
  adminSessionTtlSeconds: 3600,
  adminLoginMaxFailures: 2,
  adminLoginFailureWindowSeconds: 60,
  adminLoginLockSeconds: 120,
  redisKeyPrefix: "test:",
  trustProxy: false,
  trustedProxies: []
};

test("admin login failures lock username and IP and write security audit", async () => {
  const redis = new MemoryRedis();
  const sessionStore = new AdminSessionStore(redis, baseConfig);
  const adminStore = createAdminStore();
  const service = new AuthService(new JwtService(), baseConfig, adminStore, sessionStore);
  const req = { ip: "10.0.0.5", headers: {}, socket: { remoteAddress: "10.0.0.5" } };

  await assert.rejects(
    () => service.login({ username: "admin", password: "wrong" }, req),
    (error) => error.getResponse?.().error === "INVALID_CREDENTIALS"
  );
  await assert.rejects(
    () => service.login({ username: "admin", password: "wrong" }, req),
    (error) => error.getResponse?.().error === "INVALID_CREDENTIALS"
  );
  await assert.rejects(
    () => service.login({ username: "admin", password: "correct-password" }, req),
    (error) => error.getResponse?.().error === "ADMIN_LOGIN_LOCKED"
  );

  assert.equal(adminStore.securityLogs.filter((event) => event.eventType === "admin_login_failed").length, 2);
  assert.equal(adminStore.securityLogs.some((event) => event.eventType === "admin_login_locked"), true);
});

test("admin logout revokes current JWT session", async () => {
  const redis = new MemoryRedis();
  const sessionStore = new AdminSessionStore(redis, baseConfig);
  const adminStore = createAdminStore();
  const jwtService = new JwtService();
  const service = new AuthService(jwtService, baseConfig, adminStore, sessionStore);
  const guard = new JwtAuthGuard(jwtService, baseConfig, adminStore, sessionStore);
  const req = { ip: "10.0.0.5", headers: {}, socket: { remoteAddress: "10.0.0.5" } };

  const login = await service.login({ username: "admin", password: "correct-password" }, req);
  const authedReq = {
    ...req,
    headers: { authorization: `Bearer ${login.accessToken}` }
  };

  assert.equal(await guard.canActivate(executionContext(authedReq)), true);
  await service.logout(authedReq);

  await assert.rejects(
    () => guard.canActivate(executionContext(authedReq)),
    (error) => error.getResponse?.().error === "SESSION_REVOKED"
  );
});

test("admin client IP only trusts X-Forwarded-For from configured proxy", () => {
  const req = {
    ip: "203.0.113.10",
    socket: { remoteAddress: "203.0.113.10" },
    headers: { "x-forwarded-for": "198.51.100.20, 203.0.113.10" }
  };

  assert.equal(getClientIp(req, { trustProxy: false, trustedProxies: [] }), "203.0.113.10");
  assert.equal(getClientIp(req, { trustProxy: true, trustedProxies: [] }), "203.0.113.10");
  assert.equal(getClientIp(req, { trustProxy: true, trustedProxies: ["192.0.2.1"] }), "203.0.113.10");
  assert.equal(getClientIp(req, { trustProxy: true, trustedProxies: ["203.0.113.10"] }), "198.51.100.20");
});

test("admin-api production config rejects default secrets", () => {
  const previousEnv = new Map(Object.entries(process.env));
  try {
    process.env.NODE_ENV = "production";
    delete process.env.JWT_SECRET;
    delete process.env.GAME_ADMIN_TOKEN;

    assert.throws(
      () => getConfig(),
      /JWT_SECRET must be set to a non-default value in production/
    );

    process.env.JWT_SECRET = "prod-admin-jwt-secret";
    process.env.GAME_ADMIN_TOKEN = "prod-game-admin-token";
    process.env.GAME_PROXY_ADMIN_READ_TOKEN = "prod-game-proxy-admin-read-token";
    process.env.ADMIN_PASSWORD = "ProdAdminPass123!";
    process.env.REGISTRY_ENABLED = "true";
    assert.equal(getConfig().env, "production");
  } finally {
    for (const key of Object.keys(process.env)) {
      if (!previousEnv.has(key)) {
        delete process.env[key];
      }
    }
    for (const [key, value] of previousEnv.entries()) {
      process.env[key] = value;
    }
  }
});

test("admin-api config validates positive security windows and derives session TTL from JWT expiry", () => {
  const previousEnv = new Map(Object.entries(process.env));
  try {
    process.env.NODE_ENV = "development";
    process.env.JWT_EXPIRES_IN = "2h";
    delete process.env.ADMIN_SESSION_TTL_SECONDS;
    assert.equal(getConfig().adminSessionTtlSeconds, 7200);

    process.env.ADMIN_LOGIN_MAX_FAILURES = "0";
    assert.throws(
      () => getConfig(),
      /ADMIN_LOGIN_MAX_FAILURES must be a positive integer/
    );
  } finally {
    for (const key of Object.keys(process.env)) {
      if (!previousEnv.has(key)) {
        delete process.env[key];
      }
    }
    for (const [key, value] of previousEnv.entries()) {
      process.env[key] = value;
    }
  }
});
