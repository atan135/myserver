import fs from "node:fs";
import path from "node:path";

import log4js from "log4js";

let configured = false;
let logger = null;

const MAX_LOG_STRING_BYTES = 512;
const MAX_LOG_ARRAY_ITEMS = 32;
const MAX_LOG_OBJECT_KEYS = 64;
const MAX_LOG_DEPTH = 4;
const SENSITIVE_FIELD_PATTERN = /(authorization|ticket|token|secret|password|private.?key|signing.?key|content|body|attachments?|payload)/i;
const SENSITIVE_TEXT_PATTERN = /(?:GAME_ADMIN_TOKEN|MAIL_(?:SERVICE|OPERATIONS|HIGH_RISK)_TOKEN|MAIL_GRANT_ASSERTION_PRIVATE_KEY|\b(?:authorization|ticket|token|secret|password|content|body|attachments?|payload)\b)/i;
const ENDPOINT_PATTERN = /\b(redis|rediss|postgres|postgresql|nats|tcp|https?):\/\/[^\s"'`,;]+/gi;
const REDACTED_ERROR_DETAIL = "[REDACTED_ERROR_DETAIL]";
const ERROR_DETAIL_FIELDS = new Set([
  "error", "lasterror", "message", "detail", "details", "cause",
  "errormessage", "lasterrormessage", "errordetail", "errordetails"
]);
const STABLE_ERROR_IDENTITY_FIELDS = new Set(["code", "errorcode", "errorcategory"]);
const STABLE_ERROR_IDENTITY_PATTERN = /^[A-Z][A-Z0-9_]{0,127}$/;

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

  logger = log4js.getLogger(config.appName || "mail-service");
  configured = true;
  return logger;
}

export function getLogger() {
  if (!logger) {
    throw new Error("logger is not configured");
  }
  return logger;
}

function truncateUtf8(value, maxBytes = MAX_LOG_STRING_BYTES) {
  let result = "";
  let bytes = 0;
  for (const character of String(value ?? "")) {
    const characterBytes = Buffer.byteLength(character, "utf8");
    if (bytes + characterBytes > maxBytes) {
      return `${result}[TRUNCATED]`;
    }
    result += character;
    bytes += characterBytes;
  }
  return result;
}

function sanitizeLogText(value) {
  const text = String(value ?? "");
  if (SENSITIVE_TEXT_PATTERN.test(text)) {
    return "[REDACTED_SENSITIVE_DETAIL]";
  }
  return truncateUtf8(text.replace(ENDPOINT_PATTERN, "$1://[REDACTED_ENDPOINT]"));
}

function isErrorDetailField(fieldName) {
  const normalized = String(fieldName || "").replace(/[_-]/g, "").toLowerCase();
  return ERROR_DETAIL_FIELDS.has(normalized);
}

function isStableErrorIdentityField(fieldName) {
  const normalized = String(fieldName || "").replace(/[_-]/g, "").toLowerCase();
  return STABLE_ERROR_IDENTITY_FIELDS.has(normalized);
}

function sanitizeLogValue(value, fieldName = "", depth = 0) {
  if (value instanceof Error) {
    return {
      name: truncateUtf8(value.name, 64),
      ...(value.code ? { code: sanitizeLogValue(value.code, "code", depth + 1) } : {}),
      message: REDACTED_ERROR_DETAIL
    };
  }
  if (isStableErrorIdentityField(fieldName)) {
    return typeof value === "string" && STABLE_ERROR_IDENTITY_PATTERN.test(value)
      ? value
      : "[INVALID_ERROR_IDENTITY]";
  }
  if (isErrorDetailField(fieldName)) {
    return REDACTED_ERROR_DETAIL;
  }
  if (SENSITIVE_FIELD_PATTERN.test(fieldName)) {
    return "[REDACTED]";
  }
  if (value === null || value === undefined || typeof value === "boolean" || typeof value === "number") {
    return value;
  }
  if (typeof value === "string") {
    return sanitizeLogText(value);
  }
  if (value instanceof Date) {
    return value.toISOString();
  }
  if (depth >= MAX_LOG_DEPTH) {
    return "[TRUNCATED_DEPTH]";
  }
  if (Array.isArray(value)) {
    const values = value
      .slice(0, MAX_LOG_ARRAY_ITEMS)
      .map((item) => sanitizeLogValue(item, "", depth + 1));
    if (value.length > MAX_LOG_ARRAY_ITEMS) {
      values.push(`[TRUNCATED_${value.length - MAX_LOG_ARRAY_ITEMS}_ITEMS]`);
    }
    return values;
  }
  if (typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value)
        .slice(0, MAX_LOG_OBJECT_KEYS)
        .map(([key, item]) => [key, sanitizeLogValue(item, key, depth + 1)])
    );
  }
  return sanitizeLogText(value);
}

export function formatLogPayload(message, extra = {}) {
  const safeMessage = sanitizeLogText(message);
  const safeExtra = sanitizeLogValue(extra);
  return Object.keys(safeExtra).length === 0
    ? safeMessage
    : `${safeMessage} ${JSON.stringify(safeExtra)}`;
}

export function log(level, message, extra = {}) {
  const activeLogger = getLogger();
  const payload = formatLogPayload(message, extra);

  if (typeof activeLogger[level] === "function") {
    activeLogger[level](payload);
    return;
  }

  activeLogger.info(payload);
}
