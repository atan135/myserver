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
      process.env.TICKET_TTL_SECONDS || "300",
      10
    ),
    gameServerAdminHost: process.env.GAME_SERVER_ADMIN_HOST || "127.0.0.1",
    gameServerAdminPort: Number.parseInt(
      process.env.GAME_SERVER_ADMIN_PORT || "7001",
      10
    ),
    gameProxyHost: process.env.GAME_PROXY_HOST || "127.0.0.1",
    gameProxyPort: Number.parseInt(process.env.GAME_PROXY_PORT || "7002", 10)
  };
}
