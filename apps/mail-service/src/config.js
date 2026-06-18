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

const DEFAULT_MAIL_SERVICE_TOKENS = new Set([
  "dev-only-change-this-mail-service-token",
  "change-me",
  "changeme",
  "default",
  "password"
]);

function isProductionEnv() {
  return [process.env.NODE_ENV, process.env.APP_ENV].some(
    (value) => typeof value === "string" && value.trim().toLowerCase() === "production"
  );
}

function isStrictDiscoveryEnv() {
  return [process.env.NODE_ENV, process.env.APP_ENV].some(
    (value) => typeof value === "string" && ["production", "test"].includes(value.trim().toLowerCase())
  );
}

function validateDiscoveryConfig(config) {
  if (config.registryDiscoveryRequired && !config.registryDiscoveryEnabled) {
    throw new Error("Invalid mail-service discovery config: DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true");
  }
}

function validateProductionConfig(config) {
  if (!isProductionEnv()) {
    return;
  }

  const errors = [];
  const ticketSecret = String(config.ticketSecret || "").trim();
  const mailServiceToken = String(config.mailServiceToken || "").trim();

  if (!config.mailPlayerAuthRequired) {
    errors.push("MAIL_PLAYER_AUTH_REQUIRED must be true in production");
  }

  if (!ticketSecret || DEFAULT_TICKET_SECRETS.has(ticketSecret) || isWeakSecret(ticketSecret)) {
    errors.push("TICKET_SECRET must be set to a non-default value in production");
  }

  if (
    !mailServiceToken ||
    DEFAULT_MAIL_SERVICE_TOKENS.has(mailServiceToken) ||
    isWeakSecret(mailServiceToken)
  ) {
    errors.push("MAIL_SERVICE_TOKEN must be set to a non-default value in production");
  }

  if (errors.length > 0) {
    throw new Error(`Invalid mail-service production config: ${errors.join("; ")}`);
  }
}

function isWeakSecret(value) {
  const normalized = String(value || "").trim().toLowerCase();
  if (normalized.length < 16) {
    return true;
  }
  if (["admin", "root", "test", "token", "secret"].includes(normalized)) {
    return true;
  }
  return normalized.length > 0 && normalized.split("").every((ch) => ch === normalized[0]);
}

export function getConfig() {
  const env = process.env.NODE_ENV || "development";
  const config = {
    appName: "mail-service",
    env,
    host: process.env.HOST || "127.0.0.1",
    port: Number.parseInt(process.env.MAIL_PORT || "9003", 10),
    logLevel: process.env.LOG_LEVEL || "info",
    logEnableConsole: parseBoolean(process.env.LOG_ENABLE_CONSOLE, true),
    logEnableFile: parseBoolean(process.env.LOG_ENABLE_FILE, true),
    logDir: process.env.LOG_DIR || "logs/mail-service",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    redisKeyPrefix: process.env.REDIS_KEY_PREFIX || "",
    registryKeyPrefix: process.env.REGISTRY_KEY_PREFIX ?? process.env.REDIS_KEY_PREFIX ?? "",
    natsUrl: process.env.NATS_URL || "nats://127.0.0.1:4222",
    dbEnabled: parseBoolean(process.env.DB_ENABLED, false),
    databaseUrl:
      process.env.DATABASE_URL ||
      "postgres://postgres:password@127.0.0.1:5432/myserver_mail",
    dbPoolSize: Number.parseInt(process.env.DB_POOL_SIZE || "10", 10),
    gameServerAdminHost: process.env.GAME_SERVER_ADMIN_HOST || "127.0.0.1",
    gameServerAdminPort: Number.parseInt(process.env.GAME_SERVER_ADMIN_PORT || "7500", 10),
    registryDiscoveryEnabled: parseBoolean(process.env.REGISTRY_ENABLED, false),
    registryDiscoveryRequired: parseBoolean(process.env.DISCOVERY_REQUIRED, isStrictDiscoveryEnv()),
    gameAdminToken: process.env.GAME_ADMIN_TOKEN || "dev-only-change-this-game-admin-token",
    gameAdminActor: process.env.GAME_ADMIN_ACTOR || "",
    gameAdminConnectTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_CONNECT_TIMEOUT_MS, 3000),
    gameAdminWriteTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_WRITE_TIMEOUT_MS, 3000),
    gameAdminReadTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_READ_TIMEOUT_MS, 3000),
    gameAdminMaxResponseBytes: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_MAX_RESPONSE_BYTES, 1048576),
    ticketSecret: process.env.TICKET_SECRET || "dev-only-change-this-ticket-secret",
    mailPlayerAuthRequired: parseBoolean(process.env.MAIL_PLAYER_AUTH_REQUIRED, true),
    mailServiceToken: process.env.MAIL_SERVICE_TOKEN || "dev-only-change-this-mail-service-token",
    serviceName: process.env.SERVICE_NAME || "mail-service",
    serviceInstanceId:
      process.env.SERVICE_INSTANCE_ID || "mail-001",
    serviceZone: process.env.SERVICE_ZONE || "local",
    serviceBuildVersion: process.env.SERVICE_BUILD_VERSION || "dev",
    globalIdOriginId: process.env.GLOBAL_ID_ORIGIN_ID || "0",
    globalIdWorkerId: process.env.GLOBAL_ID_WORKER_ID
  };

  validateProductionConfig(config);
  validateDiscoveryConfig(config);
  return config;
}
