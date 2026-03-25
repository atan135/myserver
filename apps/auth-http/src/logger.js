import fs from "node:fs";
import path from "node:path";

import log4js from "log4js";

let configured = false;
let logger = null;

function normalizeLevel(level) {
  return (level || "info").toUpperCase();
}

export function configureLogger(config) {
  if (configured) {
    return logger;
  }

  const appenders = {};
  const activeAppenders = [];

  if (config.logEnableConsole) {
    appenders.console = {
      type: "stdout",
      layout: {
        type: "pattern",
        pattern: "%d{yyyy-MM-dd hh:mm:ss.SSS} [%p] %c - %m"
      }
    };
    activeAppenders.push("console");
  }

  if (config.logEnableFile) {
    fs.mkdirSync(path.resolve(config.logDir), { recursive: true });
    appenders.file = {
      type: "dateFile",
      filename: path.join(config.logDir, "app.log"),
      pattern: "yyyy-MM-dd",
      keepFileExt: true,
      alwaysIncludePattern: false,
      layout: {
        type: "pattern",
        pattern: "%d{yyyy-MM-dd hh:mm:ss.SSS} [%p] %c - %m"
      }
    };
    activeAppenders.push("file");
  }

  if (activeAppenders.length === 0) {
    appenders.console = { type: "stdout" };
    activeAppenders.push("console");
  }

  log4js.configure({
    appenders,
    categories: {
      default: {
        appenders: activeAppenders,
        level: normalizeLevel(config.logLevel)
      }
    }
  });

  logger = log4js.getLogger(config.appName || "auth-http");
  configured = true;
  return logger;
}

export function getLogger() {
  if (!logger) {
    throw new Error("logger is not configured");
  }

  return logger;
}

export function log(level, message, extra = {}) {
  const activeLogger = getLogger();
  const payload = Object.keys(extra).length === 0 ? message : `${message} ${JSON.stringify(extra)}`;

  if (typeof activeLogger[level] === "function") {
    activeLogger[level](payload);
    return;
  }

  activeLogger.info(payload);
}
