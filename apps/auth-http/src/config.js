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

export function getConfig() {
  return {
    appName: "auth-http",
    env: process.env.NODE_ENV || "development",
    host: process.env.HOST || "127.0.0.1",
    port: Number.parseInt(process.env.PORT || "3000", 10),
    logLevel: process.env.LOG_LEVEL || "info",
    logEnableConsole: parseBoolean(process.env.LOG_ENABLE_CONSOLE, true),
    logEnableFile: parseBoolean(process.env.LOG_ENABLE_FILE, true),
    logDir: process.env.LOG_DIR || "logs/auth-http",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    redisKeyPrefix: process.env.REDIS_KEY_PREFIX || "",
    mysqlEnabled: parseBoolean(process.env.MYSQL_ENABLED, false),
    mysqlUrl:
      process.env.MYSQL_URL ||
      "mysql://root:password@127.0.0.1:3306/myserver_auth",
    mysqlPoolSize: Number.parseInt(process.env.MYSQL_POOL_SIZE || "10", 10),
    sessionTtlSeconds: Number.parseInt(
      process.env.SESSION_TTL_SECONDS || "86400",
      10
    ),
    ticketSecret:
      process.env.TICKET_SECRET || "dev-only-change-this-ticket-secret",
    ticketTtlSeconds: Number.parseInt(
      process.env.TICKET_TTL_SECONDS || "86400",
      10
    ),
    gameServerAdminHost: process.env.GAME_SERVER_ADMIN_HOST || "127.0.0.1",
    gameServerAdminPort: Number.parseInt(
      process.env.GAME_SERVER_ADMIN_PORT || "7500",
      10
    ),
    gameProxyHost: process.env.GAME_PROXY_HOST || "127.0.0.1",
    gameProxyPort: Number.parseInt(process.env.GAME_PROXY_PORT || "7002", 10),

    // Rate Limiting
    ratelimitEnabled: parseBoolean(process.env.RATELIMIT_ENABLED, true),
    ratelimitWindowMs: Number.parseInt(process.env.RATELIMIT_WINDOW_MS || "60000", 10),
    ratelimitMax: Number.parseInt(process.env.RATELIMIT_MAX || "60", 10),

    // Account Lockout
    accountLockEnabled: parseBoolean(process.env.ACCOUNT_LOCK_ENABLED, true),
    accountLockMaxAttempts: Number.parseInt(process.env.ACCOUNT_LOCK_MAX_ATTEMPTS || "5", 10),
    accountLockWindowSeconds: Number.parseInt(process.env.ACCOUNT_LOCK_WINDOW_SECONDS || "900", 10),
    accountLockTtlSeconds: Number.parseInt(process.env.ACCOUNT_LOCK_TTL_SECONDS || "900", 10),

    // Ticket Validation
    ticketValidateEnabled: parseBoolean(process.env.TICKET_VALIDATE_ENABLED, true),

    // Security Audit
    securityAuditEnabled: parseBoolean(process.env.SECURITY_AUDIT_ENABLED, true)
  };
}
