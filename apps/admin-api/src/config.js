import fs from "node:fs";
import path from "node:path";

import dotenv from "dotenv";

const envPath = path.resolve(process.cwd(), ".env");
if (fs.existsSync(envPath)) {
  dotenv.config({ path: envPath });
}

function parseBoolean(value, fallback) {
  if (value === undefined) return fallback;
  return value === "true" || value === "1";
}

const DEFAULT_JWT_SECRETS = new Set([
  "dev-only-change-this-jwt-secret",
  "replace-with-a-long-random-string-for-jwt"
]);

const DEFAULT_GAME_ADMIN_TOKENS = new Set([
  "dev-only-change-this-game-admin-token"
]);

const DEFAULT_GAME_PROXY_ADMIN_TOKENS = new Set([
  "dev-only-change-this-proxy-admin-token",
  "dev-only-change-this-proxy-admin-read-token"
]);

const DEFAULT_INITIAL_ADMIN_PASSWORDS = new Set([
  "AdminPass123!"
]);

function parseCsv(value) {
  if (typeof value !== "string") return [];
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

function parsePositiveInteger(name, value, fallback) {
  const parsed = Number.parseInt(value ?? String(fallback), 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return parsed;
}

function parsePositiveIntegerWithFallback(value, fallback) {
  const parsed = Number.parseInt(value ?? String(fallback), 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function parseDurationSeconds(value, fallbackSeconds) {
  if (value === undefined || value === null || value === "") {
    return fallbackSeconds;
  }

  if (typeof value === "number") {
    return Number.isFinite(value) && value > 0 ? Math.floor(value) : fallbackSeconds;
  }

  const text = String(value).trim();
  if (/^\d+$/.test(text)) {
    return Number.parseInt(text, 10);
  }

  const match = text.match(/^(\d+)(s|m|h|d)$/i);
  if (!match) {
    return fallbackSeconds;
  }

  const amount = Number.parseInt(match[1], 10);
  const unit = match[2].toLowerCase();
  const multiplier = unit === "s" ? 1 : unit === "m" ? 60 : unit === "h" ? 3600 : 86400;
  return amount * multiplier;
}

function isStrictDiscoveryEnv() {
  return [process.env.NODE_ENV, process.env.APP_ENV].some(
    (value) => typeof value === "string" && ["production", "test"].includes(value.trim().toLowerCase())
  );
}

function validateDiscoveryConfig(config) {
  if (config.registryDiscoveryRequired && !config.registryDiscoveryEnabled) {
    throw new Error("Invalid admin-api discovery config: DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true");
  }
}

function validateProductionConfig(config) {
  if (config.env !== "production") {
    return;
  }

  const errors = [];
  if (!config.jwtSecret || DEFAULT_JWT_SECRETS.has(config.jwtSecret)) {
    errors.push("JWT_SECRET must be set to a non-default value in production");
  }

  if (!config.gameAdminToken || DEFAULT_GAME_ADMIN_TOKENS.has(config.gameAdminToken)) {
    errors.push("GAME_ADMIN_TOKEN must be set to a non-default value in production");
  }

  const gameProxyAdminToken = String(config.gameProxyAdminToken || "").trim();
  const gameProxyAdminReadToken = String(config.gameProxyAdminReadToken || "").trim();
  if (gameProxyAdminReadToken && DEFAULT_GAME_PROXY_ADMIN_TOKENS.has(gameProxyAdminReadToken)) {
    errors.push("GAME_PROXY_ADMIN_READ_TOKEN must be set to a non-default value in production");
  }
  if (!gameProxyAdminReadToken && (!gameProxyAdminToken || DEFAULT_GAME_PROXY_ADMIN_TOKENS.has(gameProxyAdminToken))) {
    errors.push("GAME_PROXY_ADMIN_READ_TOKEN or GAME_PROXY_ADMIN_TOKEN must be set to a non-default value in production");
  }

  if (!config.initialAdminPassword || DEFAULT_INITIAL_ADMIN_PASSWORDS.has(config.initialAdminPassword)) {
    errors.push("ADMIN_PASSWORD must be set to a non-default value in production");
  }

  if (errors.length > 0) {
    throw new Error(`Invalid admin-api production config: ${errors.join("; ")}`);
  }
}

export function getConfig() {
  const env = process.env.NODE_ENV || "development";
  const jwtExpiresIn = process.env.JWT_EXPIRES_IN || "8h";
  const jwtExpiresInSeconds = parseDurationSeconds(jwtExpiresIn, 28800);
  const config = {
    appName: "admin-api",
    env,
    host: process.env.HOST || "127.0.0.1",
    port: Number.parseInt(process.env.PORT || "3001", 10),
    logLevel: process.env.LOG_LEVEL || "info",
    logEnableConsole: parseBoolean(process.env.LOG_ENABLE_CONSOLE, true),
    logEnableFile: parseBoolean(process.env.LOG_ENABLE_FILE, true),
    logDir: process.env.LOG_DIR || "logs/admin-api",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    redisKeyPrefix: process.env.REDIS_KEY_PREFIX || "",
    natsUrl: process.env.NATS_URL || "nats://127.0.0.1:4222",
    serviceInstanceId:
      process.env.SERVICE_INSTANCE_ID || "admin-api-001",
    serviceName: process.env.SERVICE_NAME || "admin-api",
    serviceBuildVersion: process.env.SERVICE_BUILD_VERSION || "dev",
    databaseUrl:
      process.env.DATABASE_URL ||
      "postgresql://postgres:password@127.0.0.1:5432/myserver_auth",
    dbPoolSize: parsePositiveInteger("DB_POOL_SIZE", process.env.DB_POOL_SIZE, 10),
    jwtSecret: process.env.JWT_SECRET || "dev-only-change-this-jwt-secret",
    jwtExpiresIn,
    adminSessionTtlSeconds: parsePositiveInteger(
      "ADMIN_SESSION_TTL_SECONDS",
      process.env.ADMIN_SESSION_TTL_SECONDS,
      jwtExpiresInSeconds
    ),
    adminLoginMaxFailures: parsePositiveInteger("ADMIN_LOGIN_MAX_FAILURES", process.env.ADMIN_LOGIN_MAX_FAILURES, 5),
    adminLoginFailureWindowSeconds: parsePositiveInteger(
      "ADMIN_LOGIN_FAILURE_WINDOW_SECONDS",
      process.env.ADMIN_LOGIN_FAILURE_WINDOW_SECONDS,
      900
    ),
    adminLoginLockSeconds: parsePositiveInteger("ADMIN_LOGIN_LOCK_SECONDS", process.env.ADMIN_LOGIN_LOCK_SECONDS, 900),
    trustProxy: parseBoolean(process.env.TRUST_PROXY, false),
    trustedProxies: parseCsv(process.env.TRUSTED_PROXIES),
    adminApiRequireTls: parseBoolean(process.env.ADMIN_API_REQUIRE_TLS, env === "production"),
    adminApiRequireIpAllowlist: parseBoolean(process.env.ADMIN_API_REQUIRE_IP_ALLOWLIST, false),
    adminApiIpAllowlist: parseCsv(process.env.ADMIN_API_IP_ALLOWLIST),
    gameServerAdminHost: process.env.GAME_SERVER_ADMIN_HOST || "127.0.0.1",
    gameServerAdminPort: Number.parseInt(process.env.GAME_SERVER_ADMIN_PORT || "7500", 10),
    registryDiscoveryEnabled: parseBoolean(process.env.REGISTRY_ENABLED, false),
    registryDiscoveryRequired: parseBoolean(process.env.DISCOVERY_REQUIRED, isStrictDiscoveryEnv()),
    gameAdminToken: process.env.GAME_ADMIN_TOKEN || "dev-only-change-this-game-admin-token",
    gameAdminConnectTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_CONNECT_TIMEOUT_MS, 3000),
    gameAdminWriteTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_WRITE_TIMEOUT_MS, 3000),
    gameAdminReadTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_READ_TIMEOUT_MS, 3000),
    gameAdminMaxResponseBytes: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_MAX_RESPONSE_BYTES, 1048576),
    gameProxyAdminHost: process.env.GAME_PROXY_ADMIN_HOST || "127.0.0.1",
    gameProxyAdminPort: Number.parseInt(process.env.GAME_PROXY_ADMIN_PORT || "7101", 10),
    gameProxyAdminToken: process.env.GAME_PROXY_ADMIN_TOKEN || "dev-only-change-this-proxy-admin-token",
    gameProxyAdminReadToken: process.env.GAME_PROXY_ADMIN_READ_TOKEN || "",
    gameProxyAdminRequestTimeoutMs: parsePositiveIntegerWithFallback(
      process.env.GAME_PROXY_ADMIN_REQUEST_TIMEOUT_MS,
      3000
    ),
    gameProxyAdminMaxResponseBytes: parsePositiveIntegerWithFallback(
      process.env.GAME_PROXY_ADMIN_MAX_RESPONSE_BYTES,
      1048576
    ),
    initialAdminUsername: process.env.ADMIN_USERNAME || "admin",
    initialAdminPassword: process.env.ADMIN_PASSWORD || "AdminPass123!",
    initialAdminDisplayName: process.env.ADMIN_DISPLAY_NAME || "Administrator"
  };

  validateProductionConfig(config);
  validateDiscoveryConfig(config);
  return config;
}
