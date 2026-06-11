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
    natsUrl: process.env.NATS_URL || "nats://127.0.0.1:4222",
    mysqlEnabled: parseBoolean(process.env.MYSQL_ENABLED, false),
    mysqlUrl:
      process.env.MYSQL_URL ||
      "mysql://root:password@127.0.0.1:3306/myserver_mail",
    mysqlPoolSize: Number.parseInt(process.env.MYSQL_POOL_SIZE || "10", 10),
    gameServerAdminHost: process.env.GAME_SERVER_ADMIN_HOST || "127.0.0.1",
    gameServerAdminPort: Number.parseInt(process.env.GAME_SERVER_ADMIN_PORT || "7500", 10),
    gameAdminToken: process.env.GAME_ADMIN_TOKEN || "dev-only-change-this-game-admin-token",
    gameAdminActor: process.env.GAME_ADMIN_ACTOR || "",
    ticketSecret: process.env.TICKET_SECRET || "dev-only-change-this-ticket-secret",
    mailPlayerAuthRequired: parseBoolean(process.env.MAIL_PLAYER_AUTH_REQUIRED, true),
    mailServiceToken: process.env.MAIL_SERVICE_TOKEN || "dev-only-change-this-mail-service-token",
    serviceName: process.env.SERVICE_NAME || "mail-service",
    serviceInstanceId:
      process.env.SERVICE_INSTANCE_ID || "mail-001"
  };

  validateProductionConfig(config);
  return config;
}
