import { createPrivateKey } from "node:crypto";
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

function parseStrictBoolean(name, value, fallback) {
  if (value === undefined || value === "") {
    return fallback;
  }
  if (value === "true" || value === "1") {
    return true;
  }
  if (value === "false" || value === "0") {
    return false;
  }
  throw new Error(`Invalid mail-service config: ${name} must be true, false, 1, or 0`);
}

function parseIntegerInRange(name, value, fallback, min, max) {
  if (value === undefined || value === "") {
    return fallback;
  }
  if (!/^\d+$/.test(String(value))) {
    throw new Error(`Invalid mail-service config: ${name} must be an integer between ${min} and ${max}`);
  }
  const parsed = Number.parseInt(value, 10);
  if (!Number.isSafeInteger(parsed) || parsed < min || parsed > max) {
    throw new Error(`Invalid mail-service config: ${name} must be an integer between ${min} and ${max}`);
  }
  return parsed;
}

function parseNumberInRange(name, value, fallback, min, max) {
  if (value === undefined || value === "") {
    return fallback;
  }
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < min || parsed > max) {
    throw new Error(`Invalid mail-service config: ${name} must be a number between ${min} and ${max}`);
  }
  return parsed;
}

function firstNonEmptyEnv(names) {
  for (const name of names) {
    const value = process.env[name];
    if (typeof value === "string" && value.trim()) {
      return value.trim();
    }
  }
  return undefined;
}

function advertisedHostFromEnv(names, fallbackHost) {
  const configured = firstNonEmptyEnv(names);
  if (configured) {
    return normalizeAdvertisedHost(configured);
  }

  return normalizeAdvertisedHost(fallbackHost);
}

function normalizeAdvertisedHost(host) {
  return ["0.0.0.0", "::", "[::]"].includes(String(host).trim())
    ? "127.0.0.1"
    : host;
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
const DEFAULT_MAIL_OPERATIONS_TOKENS = new Set(["dev-only-change-this-mail-operations-token"]);
const DEFAULT_MAIL_HIGH_RISK_TOKENS = new Set(["dev-only-change-this-mail-high-risk-token"]);
const DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME = "DISALLOW_LEGACY_DIRECT_CONFIG";
const LEGACY_DIRECT_CONFIG_ENV_NAMES = [
  "GAME_SERVER_ADMIN_HOST",
  "GAME_SERVER_ADMIN_PORT"
];

function isProductionEnv() {
  return [process.env.NODE_ENV, process.env.APP_ENV].some(
    (value) => typeof value === "string" && value.trim().toLowerCase() === "production"
  );
}

function isStrictDiscoveryEnv() {
  const strictEnvNames = new Set(["production", "prod", "staging", "stage", "test", "testing"]);
  return [process.env.NODE_ENV, process.env.APP_ENV].some(
    (value) => typeof value === "string" && strictEnvNames.has(value.trim().toLowerCase())
  );
}

function isLocalDiscoveryFallbackEnv() {
  if (isStrictDiscoveryEnv() || parseBoolean(process.env.DISCOVERY_REQUIRED, false)) {
    return false;
  }

  const nodeEnv = typeof process.env.NODE_ENV === "string" ? process.env.NODE_ENV.trim().toLowerCase() : "";
  const appEnv = typeof process.env.APP_ENV === "string" ? process.env.APP_ENV.trim().toLowerCase() : "";
  return nodeEnv === "development" || appEnv === "local";
}

function validateDiscoveryConfig(config) {
  if (config.registryDiscoveryRequired && !config.registryDiscoveryEnabled) {
    throw new Error("Invalid mail-service discovery config: DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true");
  }
}

function collectConfiguredLegacyDirectConfigNames(envNames) {
  return envNames.filter((name) => process.env[name] !== undefined);
}

function validateLegacyDirectConfig(appName, envNames, disallowLegacyDirectConfig, strictDiscovery) {
  if (!disallowLegacyDirectConfig && !strictDiscovery) {
    return;
  }

  const configured = collectConfiguredLegacyDirectConfigNames(envNames);
  if (configured.length === 0) {
    return;
  }

  if (disallowLegacyDirectConfig) {
    throw new Error(
      `Invalid ${appName} discovery config: ${DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME}=true forbids legacy direct config: ${configured.join(", ")}; remove these variables and use service registry endpoints instead`
    );
  }

  throw new Error(
    `Invalid ${appName} discovery config: strict service discovery forbids legacy direct config: ${configured.join(", ")}; remove these variables and use service registry endpoints instead`
  );
}

function validateProductionConfig(config) {
  if (!isProductionEnv()) {
    return;
  }

  const errors = [];
  const ticketSecret = String(config.ticketSecret || "").trim();
  const mailServiceToken = String(config.mailServiceToken || "").trim();
  const mailOperationsToken = String(config.mailOperationsToken || "").trim();
  const mailHighRiskToken = String(config.mailHighRiskToken || "").trim();

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
  if (!mailOperationsToken || DEFAULT_MAIL_OPERATIONS_TOKENS.has(mailOperationsToken) || isWeakSecret(mailOperationsToken)) {
    errors.push("MAIL_OPERATIONS_TOKEN must be a non-default secret with at least 16 characters");
  }
  if (!mailHighRiskToken || DEFAULT_MAIL_HIGH_RISK_TOKENS.has(mailHighRiskToken) || isWeakSecret(mailHighRiskToken)) {
    errors.push("MAIL_HIGH_RISK_TOKEN must be a non-default secret with at least 16 characters");
  }
  if (mailOperationsToken && mailOperationsToken === mailServiceToken) {
    errors.push("MAIL_OPERATIONS_TOKEN must not reuse MAIL_SERVICE_TOKEN");
  }
  if (mailHighRiskToken && [mailServiceToken, mailOperationsToken].includes(mailHighRiskToken)) {
    errors.push("MAIL_HIGH_RISK_TOKEN must be independent from service and downstream tokens");
  }
  if (!isEd25519PrivateKey(config.mailGrantAssertionPrivateKey)) {
    errors.push("MAIL_GRANT_ASSERTION_PRIVATE_KEY must contain a valid Ed25519 private key in production");
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

function isEd25519PrivateKey(value) {
  try {
    return createPrivateKey(String(value || "")).asymmetricKeyType === "ed25519";
  } catch {
    return false;
  }
}

function collectLegacyDirectConfigWarnings(envNames, strictDiscovery) {
  if (!strictDiscovery) {
    return [];
  }
  return collectConfiguredLegacyDirectConfigNames(envNames)
    .map((name) => ({
      name,
      message: `${name} is ignored while strict service discovery is active; use service registry endpoints instead`
    }));
}

function emitLegacyDirectConfigWarnings(appName, warnings) {
  for (const warning of warnings) {
    console.warn(`[${appName}] ${warning.message}`);
  }
}

export function getConfig() {
  const env = process.env.NODE_ENV || "development";
  const bindHost = firstNonEmptyEnv(["SERVICE_BIND_HOST", "HOST"]) || "127.0.0.1";
  const localDiscoveryFallbackEnabled = isLocalDiscoveryFallbackEnv();
  const registryDiscoveryEnabled = parseBoolean(process.env.REGISTRY_ENABLED, false);
  const registryDiscoveryRequired = parseBoolean(process.env.DISCOVERY_REQUIRED, false) || isStrictDiscoveryEnv();
  const disallowLegacyDirectConfig = parseBoolean(process.env[DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME], false);
  validateLegacyDirectConfig(
    "mail-service",
    LEGACY_DIRECT_CONFIG_ENV_NAMES,
    disallowLegacyDirectConfig,
    registryDiscoveryRequired
  );
  const legacyDirectConfigWarnings = collectLegacyDirectConfigWarnings(
    LEGACY_DIRECT_CONFIG_ENV_NAMES,
    registryDiscoveryRequired || !localDiscoveryFallbackEnabled
  );
  const config = {
    appName: "mail-service",
    env,
    host: bindHost,
    bindHost,
    advertisedHost: advertisedHostFromEnv(["SERVICE_ADVERTISED_HOST", "SERVICE_PUBLIC_HOST", "MAIL_PUBLIC_HOST", "HOST"], bindHost),
    port: Number.parseInt(process.env.MAIL_PORT || "9003", 10),
    logLevel: process.env.LOG_LEVEL || "info",
    logEnableConsole: parseBoolean(process.env.LOG_ENABLE_CONSOLE, true),
    logEnableFile: parseBoolean(process.env.LOG_ENABLE_FILE, true),
    logDir: process.env.LOG_DIR || "logs/mail-service",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    redisKeyPrefix: process.env.REDIS_KEY_PREFIX || "",
    registryKeyPrefix: process.env.REGISTRY_KEY_PREFIX ?? process.env.REDIS_KEY_PREFIX ?? "",
    natsUrl: process.env.NATS_URL || "nats://127.0.0.1:4222",
    outboxPollIntervalMs: parseIntegerInRange("MAIL_OUTBOX_POLL_INTERVAL_MS", process.env.MAIL_OUTBOX_POLL_INTERVAL_MS, 5000, 100, 60_000),
    outboxBatchSize: parseIntegerInRange("MAIL_OUTBOX_BATCH_SIZE", process.env.MAIL_OUTBOX_BATCH_SIZE, 20, 1, 1000),
    outboxLeaseMs: parseIntegerInRange("MAIL_OUTBOX_LEASE_MS", process.env.MAIL_OUTBOX_LEASE_MS, 30_000, 1000, 300_000),
    outboxMaxAttempts: parseIntegerInRange("MAIL_OUTBOX_MAX_ATTEMPTS", process.env.MAIL_OUTBOX_MAX_ATTEMPTS, 8, 1, 100),
    outboxBackoffBaseMs: parseIntegerInRange("MAIL_OUTBOX_BACKOFF_BASE_MS", process.env.MAIL_OUTBOX_BACKOFF_BASE_MS, 1000, 100, 60_000),
    outboxBackoffMaxMs: parseIntegerInRange("MAIL_OUTBOX_BACKOFF_MAX_MS", process.env.MAIL_OUTBOX_BACKOFF_MAX_MS, 60_000, 1000, 3_600_000),
    outboxBackoffJitterRatio: parseNumberInRange("MAIL_OUTBOX_BACKOFF_JITTER_RATIO", process.env.MAIL_OUTBOX_BACKOFF_JITTER_RATIO, 0.2, 0, 1),
    outboxSentRetentionDays: parseIntegerInRange("MAIL_OUTBOX_SENT_RETENTION_DAYS", process.env.MAIL_OUTBOX_SENT_RETENTION_DAYS, 7, 1, 3650),
    outboxTerminalRetentionDays: parseIntegerInRange("MAIL_OUTBOX_TERMINAL_RETENTION_DAYS", process.env.MAIL_OUTBOX_TERMINAL_RETENTION_DAYS, 30, 1, 3650),
    outboxCleanupIntervalMs: parseIntegerInRange("MAIL_OUTBOX_CLEANUP_INTERVAL_MS", process.env.MAIL_OUTBOX_CLEANUP_INTERVAL_MS, 3_600_000, 10_000, 86_400_000),
    outboxCleanupBatchSize: parseIntegerInRange("MAIL_OUTBOX_CLEANUP_BATCH_SIZE", process.env.MAIL_OUTBOX_CLEANUP_BATCH_SIZE, 500, 1, 10_000),
    claimLeaseMs: parseIntegerInRange("MAIL_CLAIM_LEASE_MS", process.env.MAIL_CLAIM_LEASE_MS, 30_000, 1000, 300_000),
    claimNewRequestsEnabled: parseStrictBoolean("MAIL_CLAIM_NEW_REQUESTS_ENABLED", process.env.MAIL_CLAIM_NEW_REQUESTS_ENABLED, true),
    claimRecoveryEnabled: parseStrictBoolean("MAIL_CLAIM_RECOVERY_ENABLED", process.env.MAIL_CLAIM_RECOVERY_ENABLED, true),
    claimRecoveryPollIntervalMs: parseIntegerInRange("MAIL_CLAIM_RECOVERY_POLL_INTERVAL_MS", process.env.MAIL_CLAIM_RECOVERY_POLL_INTERVAL_MS, 5000, 100, 60_000),
    claimRecoveryBatchSize: parseIntegerInRange("MAIL_CLAIM_RECOVERY_BATCH_SIZE", process.env.MAIL_CLAIM_RECOVERY_BATCH_SIZE, 20, 1, 100),
    claimRecoveryLeaseMs: parseIntegerInRange("MAIL_CLAIM_RECOVERY_LEASE_MS", process.env.MAIL_CLAIM_RECOVERY_LEASE_MS, 60_000, 5000, 300_000),
    claimRecoveryBackoffBaseMs: parseIntegerInRange("MAIL_CLAIM_RECOVERY_BACKOFF_BASE_MS", process.env.MAIL_CLAIM_RECOVERY_BACKOFF_BASE_MS, 1000, 100, 60_000),
    claimRecoveryBackoffMaxMs: parseIntegerInRange("MAIL_CLAIM_RECOVERY_BACKOFF_MAX_MS", process.env.MAIL_CLAIM_RECOVERY_BACKOFF_MAX_MS, 300_000, 1000, 3_600_000),
    claimRecoveryMaxAttempts: parseIntegerInRange("MAIL_CLAIM_RECOVERY_MAX_ATTEMPTS", process.env.MAIL_CLAIM_RECOVERY_MAX_ATTEMPTS, 12, 1, 100),
    claimRecoveryShutdownTimeoutMs: parseIntegerInRange("MAIL_CLAIM_RECOVERY_SHUTDOWN_TIMEOUT_MS", process.env.MAIL_CLAIM_RECOVERY_SHUTDOWN_TIMEOUT_MS, 10_000, 100, 60_000),
    dbEnabled: parseBoolean(process.env.DB_ENABLED, false),
    databaseUrl:
      process.env.DATABASE_URL ||
      "postgres://postgres:password@127.0.0.1:5432/myserver_mail",
    dbPoolSize: Number.parseInt(process.env.DB_POOL_SIZE || "10", 10),
    gameServerAdminHost: localDiscoveryFallbackEnabled
      ? process.env.GAME_SERVER_ADMIN_HOST || "127.0.0.1"
      : "127.0.0.1",
    gameServerAdminPort: Number.parseInt(
      localDiscoveryFallbackEnabled ? process.env.GAME_SERVER_ADMIN_PORT || "7500" : "7500",
      10
    ),
    registryDiscoveryEnabled,
    registryDiscoveryRequired,
    localDiscoveryFallbackEnabled,
    disallowLegacyDirectConfig,
    legacyDirectConfigWarnings,
    mailGrantAssertionIssuer: process.env.MAIL_GRANT_ASSERTION_ISSUER || "mail-service",
    mailGrantAssertionKeyId: process.env.MAIL_GRANT_ASSERTION_KEY_ID || "mail-service-v1",
    mailGrantAssertionPrivateKey: process.env.MAIL_GRANT_ASSERTION_PRIVATE_KEY || "",
    mailGrantAssertionTtlMs: parseIntegerInRange(
      "MAIL_GRANT_ASSERTION_TTL_MS",
      process.env.MAIL_GRANT_ASSERTION_TTL_MS,
      60_000,
      1_000,
      300_000
    ),
    gameAdminConnectTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_CONNECT_TIMEOUT_MS, 3000),
    gameAdminWriteTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_WRITE_TIMEOUT_MS, 3000),
    gameAdminReadTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_READ_TIMEOUT_MS, 3000),
    gameAdminMaxResponseBytes: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_MAX_RESPONSE_BYTES, 1048576),
    ticketSecret: process.env.TICKET_SECRET || "dev-only-change-this-ticket-secret",
    mailPlayerAuthRequired: parseBoolean(process.env.MAIL_PLAYER_AUTH_REQUIRED, true),
    mailServiceToken: process.env.MAIL_SERVICE_TOKEN || "dev-only-change-this-mail-service-token",
    mailOperationsToken: process.env.MAIL_OPERATIONS_TOKEN || "dev-only-change-this-mail-operations-token",
    mailHighRiskToken: process.env.MAIL_HIGH_RISK_TOKEN || "dev-only-change-this-mail-high-risk-token",
    mailRetentionDays: parseIntegerInRange("MAIL_RETENTION_DAYS", process.env.MAIL_RETENTION_DAYS, 400, 30, 3650),
    claimWorkflowRetentionDays: parseIntegerInRange("MAIL_CLAIM_WORKFLOW_RETENTION_DAYS", process.env.MAIL_CLAIM_WORKFLOW_RETENTION_DAYS, 400, 30, 3650),
    gameGrantRetentionDays: parseIntegerInRange("MAIL_GAME_GRANT_RETENTION_DAYS", process.env.MAIL_GAME_GRANT_RETENTION_DAYS, 400, 30, 3650),
    claimAlertWindowMinutes: parseIntegerInRange("MAIL_CLAIM_ALERT_WINDOW_MINUTES", process.env.MAIL_CLAIM_ALERT_WINDOW_MINUTES, 10, 1, 1440),
    claimAlertFailureRatePercent: parseIntegerInRange("MAIL_CLAIM_ALERT_FAILURE_RATE_PERCENT", process.env.MAIL_CLAIM_ALERT_FAILURE_RATE_PERCENT, 20, 1, 100),
    claimAlertLongRunningMinutes: parseIntegerInRange("MAIL_CLAIM_ALERT_LONG_RUNNING_MINUTES", process.env.MAIL_CLAIM_ALERT_LONG_RUNNING_MINUTES, 15, 1, 10080),
    claimAlertManualReviewCount: parseIntegerInRange("MAIL_CLAIM_ALERT_MANUAL_REVIEW_COUNT", process.env.MAIL_CLAIM_ALERT_MANUAL_REVIEW_COUNT, 1, 1, 100000),
    serviceName: process.env.SERVICE_NAME || "mail-service",
    serviceInstanceId:
      process.env.SERVICE_INSTANCE_ID || "mail-001",
    serviceZone: process.env.SERVICE_ZONE || "local",
    serviceBuildVersion: process.env.SERVICE_BUILD_VERSION || "dev",
    globalIdOriginId: process.env.GLOBAL_ID_ORIGIN_ID || "0",
    globalIdWorkerId: process.env.GLOBAL_ID_WORKER_ID
  };

  if (config.outboxBackoffMaxMs < config.outboxBackoffBaseMs) {
    throw new Error("Invalid mail-service config: MAIL_OUTBOX_BACKOFF_MAX_MS must be greater than or equal to MAIL_OUTBOX_BACKOFF_BASE_MS");
  }
  if (config.claimRecoveryBackoffMaxMs < config.claimRecoveryBackoffBaseMs) {
    throw new Error("Invalid mail-service config: MAIL_CLAIM_RECOVERY_BACKOFF_MAX_MS must be greater than or equal to MAIL_CLAIM_RECOVERY_BACKOFF_BASE_MS");
  }
  if (!config.claimRecoveryEnabled && config.claimNewRequestsEnabled) {
    throw new Error("Invalid mail-service config: MAIL_CLAIM_RECOVERY_ENABLED=false requires MAIL_CLAIM_NEW_REQUESTS_ENABLED=false");
  }
  if (config.mailRetentionDays < config.claimWorkflowRetentionDays) {
    throw new Error("Invalid mail-service config: MAIL_RETENTION_DAYS must be greater than or equal to MAIL_CLAIM_WORKFLOW_RETENTION_DAYS");
  }
  if (config.gameGrantRetentionDays < config.claimWorkflowRetentionDays) {
    throw new Error("Invalid mail-service config: MAIL_GAME_GRANT_RETENTION_DAYS must be greater than or equal to MAIL_CLAIM_WORKFLOW_RETENTION_DAYS");
  }

  emitLegacyDirectConfigWarnings(config.appName, config.legacyDirectConfigWarnings);
  validateProductionConfig(config);
  validateDiscoveryConfig(config);
  return config;
}
