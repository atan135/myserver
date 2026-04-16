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

export function getConfig() {
  return {
    appName: "admin-api",
    env: process.env.NODE_ENV || "development",
    host: process.env.HOST || "127.0.0.1",
    port: Number.parseInt(process.env.PORT || "3001", 10),
    logLevel: process.env.LOG_LEVEL || "info",
    logEnableConsole: parseBoolean(process.env.LOG_ENABLE_CONSOLE, true),
    logEnableFile: parseBoolean(process.env.LOG_ENABLE_FILE, true),
    logDir: process.env.LOG_DIR || "logs/admin-api",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    redisKeyPrefix: process.env.REDIS_KEY_PREFIX || "",
    mysqlUrl: process.env.MYSQL_URL || "mysql://root:password@127.0.0.1:3306/myserver_auth",
    mysqlPoolSize: Number.parseInt(process.env.MYSQL_POOL_SIZE || "10", 10),
    jwtSecret: process.env.JWT_SECRET || "dev-only-change-this-jwt-secret",
    jwtExpiresIn: process.env.JWT_EXPIRES_IN || "8h",
    gameServerAdminHost: process.env.GAME_SERVER_ADMIN_HOST || "127.0.0.1",
    gameServerAdminPort: Number.parseInt(process.env.GAME_SERVER_ADMIN_PORT || "7500", 10),
    initialAdminUsername: process.env.ADMIN_USERNAME || "admin",
    initialAdminPassword: process.env.ADMIN_PASSWORD || "AdminPass123!",
    initialAdminDisplayName: process.env.ADMIN_DISPLAY_NAME || "Administrator"
  };
}
