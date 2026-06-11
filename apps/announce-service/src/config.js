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

export const DEFAULT_ANNOUNCE_ADMIN_TOKEN =
  "dev-only-change-this-announce-admin-token";
export const DEFAULT_ANNOUNCE_READ_TOKEN =
  "dev-only-change-this-announce-read-token";
export const DEFAULT_TICKET_SECRET = "dev-only-change-this-ticket-secret";

const DEFAULT_ANNOUNCE_ADMIN_TOKENS = new Set([
  DEFAULT_ANNOUNCE_ADMIN_TOKEN,
  "change-me",
  "changeme",
  "default",
  "password"
]);

const DEFAULT_ANNOUNCE_READ_TOKENS = new Set([
  DEFAULT_ANNOUNCE_READ_TOKEN,
  "change-me",
  "changeme",
  "default",
  "password"
]);

const DEFAULT_TICKET_SECRETS = new Set([
  DEFAULT_TICKET_SECRET,
  "replace-with-a-long-random-string",
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

function validateProductionConfig(config) {
  if (!isProductionEnv()) {
    return;
  }

  const errors = [];
  const adminToken = String(config.announceAdminToken || "").trim();
  const readToken = String(config.announceReadToken || "").trim();
  const ticketSecret = String(config.ticketSecret || "").trim();

  if (!config.announceReadAuthRequired) {
    errors.push("ANNOUNCE_READ_AUTH_REQUIRED must be true in production");
  }

  if (
    !adminToken ||
    DEFAULT_ANNOUNCE_ADMIN_TOKENS.has(adminToken) ||
    isWeakSecret(adminToken)
  ) {
    errors.push("ANNOUNCE_ADMIN_TOKEN must be set to a non-default value in production");
  }

  if (
    readToken &&
    (DEFAULT_ANNOUNCE_READ_TOKENS.has(readToken) || isWeakSecret(readToken))
  ) {
    errors.push("ANNOUNCE_READ_TOKEN must be set to a non-default value in production");
  }

  if (readToken && adminToken && readToken === adminToken) {
    errors.push("ANNOUNCE_READ_TOKEN must not match ANNOUNCE_ADMIN_TOKEN in production");
  }

  if (
    !ticketSecret ||
    DEFAULT_TICKET_SECRETS.has(ticketSecret) ||
    isWeakSecret(ticketSecret)
  ) {
    errors.push("TICKET_SECRET must be set to a non-default value in production");
  }

  if (errors.length > 0) {
    throw new Error(`Invalid announce-service production config: ${errors.join("; ")}`);
  }
}

export function getConfig() {
  const config = {
    appName: "announce-service",
    env: process.env.NODE_ENV || "development",
    appEnv: process.env.APP_ENV || "",
    host: process.env.HOST || "127.0.0.1",
    port: Number.parseInt(process.env.ANNOUNCE_PORT || "9004", 10),
    logLevel: process.env.LOG_LEVEL || "info",
    logEnableConsole: parseBoolean(process.env.LOG_ENABLE_CONSOLE, true),
    logEnableFile: parseBoolean(process.env.LOG_ENABLE_FILE, true),
    logDir: process.env.LOG_DIR || "logs/announce-service",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    redisKeyPrefix: process.env.REDIS_KEY_PREFIX || "",
    natsUrl: process.env.NATS_URL || "nats://127.0.0.1:4222",
    mysqlEnabled: parseBoolean(process.env.MYSQL_ENABLED, false),
    mysqlUrl:
      process.env.MYSQL_URL ||
      "mysql://root:password@127.0.0.1:3306/myserver_announce",
    mysqlPoolSize: Number.parseInt(process.env.MYSQL_POOL_SIZE || "10", 10),
    announceCacheTtlSeconds: Number.parseInt(
      process.env.ANNOUNCE_CACHE_TTL_SECONDS || "10",
      10
    ),
    announceAdminToken:
      process.env.ANNOUNCE_ADMIN_TOKEN || DEFAULT_ANNOUNCE_ADMIN_TOKEN,
    announceReadAuthRequired: parseBoolean(process.env.ANNOUNCE_READ_AUTH_REQUIRED, true),
    announceReadToken: process.env.ANNOUNCE_READ_TOKEN || "",
    ticketSecret: process.env.TICKET_SECRET || DEFAULT_TICKET_SECRET,
    serviceName: process.env.SERVICE_NAME || "announce-service",
    serviceInstanceId:
      process.env.SERVICE_INSTANCE_ID || "announce-001"
  };

  validateProductionConfig(config);
  return config;
}
