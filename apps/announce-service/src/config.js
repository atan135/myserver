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
    appName: "announce-service",
    env: process.env.NODE_ENV || "development",
    host: process.env.HOST || "127.0.0.1",
    port: Number.parseInt(process.env.ANNOUNCE_PORT || "9004", 10),
    logLevel: process.env.LOG_LEVEL || "info",
    logEnableConsole: parseBoolean(process.env.LOG_ENABLE_CONSOLE, true),
    logEnableFile: parseBoolean(process.env.LOG_ENABLE_FILE, true),
    logDir: process.env.LOG_DIR || "logs/announce-service",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    mysqlEnabled: parseBoolean(process.env.MYSQL_ENABLED, false),
    mysqlUrl:
      process.env.MYSQL_URL ||
      "mysql://root:password@127.0.0.1:3306/myserver_announce",
    mysqlPoolSize: Number.parseInt(process.env.MYSQL_POOL_SIZE || "10", 10),
    serviceName: process.env.SERVICE_NAME || "announce-service",
    serviceInstanceId:
      process.env.SERVICE_INSTANCE_ID || "announce-001"
  };
}
