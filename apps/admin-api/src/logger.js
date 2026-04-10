import log4js from "log4js";
import path from "node:path";
import fs from "node:fs";

let logger = null;

export function configureLogger(config) {
  const logDir = config.logDir || "logs/admin-api";
  if (config.logEnableFile) {
    fs.mkdirSync(logDir, { recursive: true });
  }

  log4js.configure({
    appenders: {
      console: {
        type: "console",
        layout: {
          type: "pattern",
          pattern: "[%d{yyyy-MM-dd hh:mm:ss.SSS}] [%p] %c - %m"
        }
      },
      file: {
        type: "dateFile",
        filename: path.join(logDir, "admin-api.log"),
        pattern: "yyyy-MM-dd",
        daysToKeep: 7,
        layout: {
          type: "pattern",
          pattern: "[%d{yyyy-MM-dd hh:mm:ss.SSS}] [%p] %c - %m"
        }
      }
    },
    categories: {
      default: {
        appenders: [
          ...(config.logEnableConsole ? ["console"] : []),
          ...(config.logEnableFile ? ["file"] : [])
        ],
        level: config.logLevel || "info"
      }
    }
  });

  logger = log4js.getLogger("admin-api");
  return logger;
}

export function log(level, category, meta = {}) {
  if (!logger) return;
  const message = meta ? JSON.stringify(meta) : "";
  logger[level]?.(`[${category}] ${message}`);
}

export function getLogger() {
  return logger;
}
