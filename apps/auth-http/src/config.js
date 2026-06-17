import fs from "node:fs";
import path from "node:path";

import dotenv from "dotenv";

const envPath = path.resolve(process.cwd(), ".env");
if (fs.existsSync(envPath)) {
  dotenv.config({ path: envPath });
}

function parseBoolean(value, fallback) {
  if (value === undefined) {
    return fallback;
  }

  return value === "true" || value === "1";
}

function parseCsv(value) {
  if (typeof value !== "string") {
    return [];
  }

  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

function parsePositiveIntegerWithFallback(value, fallback) {
  const parsed = Number.parseInt(value ?? String(fallback), 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

const DEFAULT_TICKET_SECRETS = new Set([
  "dev-only-change-this-ticket-secret",
  "replace-with-a-long-random-string",
  "change-me",
  "changeme",
  "default",
  "password"
]);

const DEFAULT_GAME_ADMIN_TOKENS = new Set([
  "dev-only-change-this-game-admin-token",
  "change-me",
  "changeme",
  "default",
  "password"
]);

const DEFAULT_AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS = 2000;

function isProductionEnv() {
  return [process.env.NODE_ENV, process.env.APP_ENV].some(
    (value) => typeof value === "string" && value.trim().toLowerCase() === "production"
  );
}

function validateProductionConfig(config) {
  if (!isProductionEnv()) {
    return;
  }

  const errors = [];
  const ticketSecret = String(config.ticketSecret || "").trim();
  const gameAdminToken = String(config.gameAdminToken || "").trim();
  const internalApiToken = String(config.internalApiToken || "").trim();

  if (!ticketSecret || DEFAULT_TICKET_SECRETS.has(ticketSecret)) {
    errors.push("TICKET_SECRET must be set to a non-default value in production");
  }

  if (!gameAdminToken || DEFAULT_GAME_ADMIN_TOKENS.has(gameAdminToken)) {
    errors.push("GAME_ADMIN_TOKEN must be set to a non-default value in production");
  }

  if (!internalApiToken) {
    errors.push("INTERNAL_API_TOKEN must be set in production");
  }

  if (errors.length > 0) {
    throw new Error(`Invalid auth-http production config: ${errors.join("; ")}`);
  }
}

export function getConfig() {
  const env = process.env.NODE_ENV || "development";
  const config = {
    appName: "auth-http",
    env,
    host: process.env.HOST || "127.0.0.1",
    port: Number.parseInt(process.env.PORT || "3000", 10),
    logLevel: process.env.LOG_LEVEL || "info",
    logEnableConsole: parseBoolean(process.env.LOG_ENABLE_CONSOLE, true),
    logEnableFile: parseBoolean(process.env.LOG_ENABLE_FILE, true),
    logDir: process.env.LOG_DIR || "logs/auth-http",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    redisKeyPrefix: process.env.REDIS_KEY_PREFIX || "",
    authRedisBlocklistEnabled: parseBoolean(process.env.AUTH_REDIS_BLOCKLIST_ENABLED, false),
    authRedisBlocklistCacheTtlMs: Number.parseInt(
      process.env.AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS || String(DEFAULT_AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS),
      10
    ),
    natsUrl: process.env.NATS_URL || "nats://127.0.0.1:4222",
    serviceInstanceId:
      process.env.SERVICE_INSTANCE_ID || "auth-http-001",
    globalIdOriginId: process.env.GLOBAL_ID_ORIGIN_ID || "0",
    globalIdWorkerId: process.env.GLOBAL_ID_WORKER_ID,
    dbEnabled: parseBoolean(process.env.DB_ENABLED, false),
    databaseUrl:
      process.env.DATABASE_URL ||
      "postgresql://postgres:password@127.0.0.1:5432/myserver_auth",
    dbPoolSize: Number.parseInt(process.env.DB_POOL_SIZE || "10", 10),
    sessionTtlSeconds: Number.parseInt(
      process.env.SESSION_TTL_SECONDS || "86400",
      10
    ),
    ticketSecret:
      process.env.TICKET_SECRET || "dev-only-change-this-ticket-secret",
    ticketTtlSeconds: Number.parseInt(
      process.env.TICKET_TTL_SECONDS || "900",
      10
    ),
    gameServerAdminHost: process.env.GAME_SERVER_ADMIN_HOST || "127.0.0.1",
    gameServerAdminPort: Number.parseInt(
      process.env.GAME_SERVER_ADMIN_PORT || "7500",
      10
    ),
    gameAdminToken: process.env.GAME_ADMIN_TOKEN || "dev-only-change-this-game-admin-token",
    gameAdminActor: process.env.GAME_ADMIN_ACTOR || "auth-http",
    gameAdminConnectTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_CONNECT_TIMEOUT_MS, 3000),
    gameAdminReadTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_READ_TIMEOUT_MS, 3000),
    gameAdminWriteTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_WRITE_TIMEOUT_MS, 3000),
    gameAdminMaxResponseBytes: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_MAX_RESPONSE_BYTES, 1048576),
    gameProxyHost: process.env.GAME_PROXY_HOST || "127.0.0.1",
    gameProxyPort: Number.parseInt(process.env.GAME_PROXY_PORT || "4000", 10),
    registryDiscoveryEnabled: parseBoolean(process.env.REGISTRY_ENABLED, false),
    authRequireTls: parseBoolean(process.env.AUTH_REQUIRE_TLS, isProductionEnv()),
    trustProxy: parseBoolean(process.env.TRUST_PROXY, false),
    trustedProxies: parseCsv(process.env.TRUSTED_PROXIES),

    // Rate Limiting
    ratelimitEnabled: parseBoolean(process.env.RATELIMIT_ENABLED, true),
    ratelimitWindowMs: Number.parseInt(process.env.RATELIMIT_WINDOW_MS || "60000", 10),
    ratelimitMax: Number.parseInt(process.env.RATELIMIT_MAX || "60", 10),

    // Account Lockout
    accountLockEnabled: parseBoolean(process.env.ACCOUNT_LOCK_ENABLED, true),
    accountLockMaxAttempts: Number.parseInt(process.env.ACCOUNT_LOCK_MAX_ATTEMPTS || "5", 10),
    accountLockWindowSeconds: Number.parseInt(process.env.ACCOUNT_LOCK_WINDOW_SECONDS || "900", 10),
    accountLockTtlSeconds: Number.parseInt(process.env.ACCOUNT_LOCK_TTL_SECONDS || "900", 10),

    // Registration
    registerRequireReview: parseBoolean(process.env.AUTH_REGISTER_REQUIRE_REVIEW, false),

    // Ticket Validation
    ticketValidateEnabled: parseBoolean(process.env.TICKET_VALIDATE_ENABLED, true),

    // Security Audit
    securityAuditEnabled: parseBoolean(process.env.SECURITY_AUDIT_ENABLED, true),

    // Internal API Token
    internalApiToken: process.env.INTERNAL_API_TOKEN || "",
    strictSecurity: parseBoolean(
      process.env.AUTH_STRICT_SECURITY,
      env === "production"
    )
  };

  validateProductionConfig(config);
  return config;
}
