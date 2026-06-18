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

function parseCsv(value) {
  if (typeof value !== "string") {
    return [];
  }

  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

function parsePositiveIntegerWithFallback(value, fallback) {
  const parsed = Number.parseInt(value ?? String(fallback), 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
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

const DEFAULT_GAME_ADMIN_TOKENS = new Set([
  "dev-only-change-this-game-admin-token",
  "change-me",
  "changeme",
  "default",
  "password"
]);

const DEFAULT_AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS = 2000;
const DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME = "DISALLOW_LEGACY_DIRECT_CONFIG";
const LEGACY_DIRECT_CONFIG_ENV_NAMES = [
  "GAME_PROXY_HOST",
  "GAME_PROXY_PORT",
  "GAME_SERVER_ADMIN_HOST",
  "GAME_SERVER_ADMIN_PORT"
];

function isProductionEnv() {
  return [process.env.NODE_ENV, process.env.APP_ENV].some(
    (value) => typeof value === "string" && value.trim().toLowerCase() === "production"
  );
}

function isStrictDiscoveryEnv() {
  return [process.env.NODE_ENV, process.env.APP_ENV].some(
    (value) => typeof value === "string" && ["production", "test"].includes(value.trim().toLowerCase())
  );
}

function isLocalDiscoveryFallbackEnv() {
  if (isStrictDiscoveryEnv()) {
    return false;
  }

  const names = [process.env.NODE_ENV, process.env.APP_ENV]
    .map((value) => typeof value === "string" ? value.trim().toLowerCase() : "")
    .filter(Boolean);
  return names.length === 0 || names.some((value) => ["development", "local"].includes(value));
}

function validateProductionConfig(config) {
  if (!isProductionEnv()) {
    return;
  }

  const errors = [];
  const ticketSecret = String(config.ticketSecret || "").trim();
  const gameAdminToken = String(config.gameAdminToken || "").trim();
  const internalApiToken = String(config.internalApiToken || "").trim();

  if (!ticketSecret || DEFAULT_TICKET_SECRETS.has(ticketSecret)) {
    errors.push("TICKET_SECRET must be set to a non-default value in production");
  }

  if (!gameAdminToken || DEFAULT_GAME_ADMIN_TOKENS.has(gameAdminToken)) {
    errors.push("GAME_ADMIN_TOKEN must be set to a non-default value in production");
  }

  if (!internalApiToken) {
    errors.push("INTERNAL_API_TOKEN must be set in production");
  }

  if (errors.length > 0) {
    throw new Error(`Invalid auth-http production config: ${errors.join("; ")}`);
  }
}

function validateDiscoveryConfig(config) {
  if (config.registryDiscoveryRequired && !config.registryDiscoveryEnabled) {
    throw new Error("Invalid auth-http discovery config: DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true");
  }
}

function collectConfiguredLegacyDirectConfigNames(envNames) {
  return envNames.filter((name) => process.env[name] !== undefined);
}

function validateLegacyDirectConfig(appName, envNames, disallowLegacyDirectConfig) {
  if (!disallowLegacyDirectConfig) {
    return;
  }

  const configured = collectConfiguredLegacyDirectConfigNames(envNames);
  if (configured.length > 0) {
    throw new Error(
      `Invalid ${appName} discovery config: ${DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME}=true forbids legacy direct config: ${configured.join(", ")}; remove these variables and use service registry endpoints instead`
    );
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
  validateLegacyDirectConfig("auth-http", LEGACY_DIRECT_CONFIG_ENV_NAMES, disallowLegacyDirectConfig);
  const legacyDirectConfigWarnings = collectLegacyDirectConfigWarnings(
    LEGACY_DIRECT_CONFIG_ENV_NAMES,
    registryDiscoveryRequired || !localDiscoveryFallbackEnabled
  );
  const config = {
    appName: "auth-http",
    env,
    host: bindHost,
    bindHost,
    advertisedHost: advertisedHostFromEnv(["SERVICE_ADVERTISED_HOST", "SERVICE_PUBLIC_HOST", "HOST"], bindHost),
    port: Number.parseInt(process.env.PORT || "3000", 10),
    logLevel: process.env.LOG_LEVEL || "info",
    logEnableConsole: parseBoolean(process.env.LOG_ENABLE_CONSOLE, true),
    logEnableFile: parseBoolean(process.env.LOG_ENABLE_FILE, true),
    logDir: process.env.LOG_DIR || "logs/auth-http",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    redisKeyPrefix: process.env.REDIS_KEY_PREFIX || "",
    registryKeyPrefix: process.env.REGISTRY_KEY_PREFIX ?? process.env.REDIS_KEY_PREFIX ?? "",
    authRedisBlocklistEnabled: parseBoolean(process.env.AUTH_REDIS_BLOCKLIST_ENABLED, false),
    authRedisBlocklistCacheTtlMs: Number.parseInt(
      process.env.AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS || String(DEFAULT_AUTH_REDIS_BLOCKLIST_CACHE_TTL_MS),
      10
    ),
    natsUrl: process.env.NATS_URL || "nats://127.0.0.1:4222",
    serviceName: process.env.SERVICE_NAME || "auth-http",
    serviceInstanceId:
      process.env.SERVICE_INSTANCE_ID || "auth-http-001",
    serviceZone: process.env.SERVICE_ZONE || "local",
    serviceBuildVersion: process.env.SERVICE_BUILD_VERSION || "dev",
    globalIdOriginId: process.env.GLOBAL_ID_ORIGIN_ID || "0",
    globalIdWorkerId: process.env.GLOBAL_ID_WORKER_ID,
    dbEnabled: parseBoolean(process.env.DB_ENABLED, false),
    databaseUrl:
      process.env.DATABASE_URL ||
      "postgresql://postgres:password@127.0.0.1:5432/myserver_auth",
    dbPoolSize: Number.parseInt(process.env.DB_POOL_SIZE || "10", 10),
    sessionTtlSeconds: Number.parseInt(
      process.env.SESSION_TTL_SECONDS || "86400",
      10
    ),
    ticketSecret:
      process.env.TICKET_SECRET || "dev-only-change-this-ticket-secret",
    ticketTtlSeconds: Number.parseInt(
      process.env.TICKET_TTL_SECONDS || "900",
      10
    ),
    gameServerAdminHost: localDiscoveryFallbackEnabled
      ? process.env.GAME_SERVER_ADMIN_HOST || "127.0.0.1"
      : "127.0.0.1",
    gameServerAdminPort: Number.parseInt(
      localDiscoveryFallbackEnabled ? process.env.GAME_SERVER_ADMIN_PORT || "7500" : "7500",
      10
    ),
    gameAdminToken: process.env.GAME_ADMIN_TOKEN || "dev-only-change-this-game-admin-token",
    gameAdminActor: process.env.GAME_ADMIN_ACTOR || "auth-http",
    gameAdminConnectTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_CONNECT_TIMEOUT_MS, 3000),
    gameAdminReadTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_READ_TIMEOUT_MS, 3000),
    gameAdminWriteTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_WRITE_TIMEOUT_MS, 3000),
    gameAdminMaxResponseBytes: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_MAX_RESPONSE_BYTES, 1048576),
    gameProxyHost: localDiscoveryFallbackEnabled
      ? process.env.GAME_PROXY_HOST || "127.0.0.1"
      : "127.0.0.1",
    gameProxyPort: Number.parseInt(
      localDiscoveryFallbackEnabled ? process.env.GAME_PROXY_PORT || "4000" : "4000",
      10
    ),
    registryDiscoveryEnabled,
    registryDiscoveryRequired,
    localDiscoveryFallbackEnabled,
    disallowLegacyDirectConfig,
    legacyDirectConfigWarnings,
    authExposeInternalServiceEndpoints:
      !isProductionEnv() &&
      parseBoolean(process.env.AUTH_EXPOSE_INTERNAL_SERVICE_ENDPOINTS, false),
    authRequireTls: parseBoolean(process.env.AUTH_REQUIRE_TLS, isProductionEnv()),
    trustProxy: parseBoolean(process.env.TRUST_PROXY, false),
    trustedProxies: parseCsv(process.env.TRUSTED_PROXIES),

    // Rate Limiting
    ratelimitEnabled: parseBoolean(process.env.RATELIMIT_ENABLED, true),
    ratelimitWindowMs: Number.parseInt(process.env.RATELIMIT_WINDOW_MS || "60000", 10),
    ratelimitMax: Number.parseInt(process.env.RATELIMIT_MAX || "60", 10),

    // Account Lockout
    accountLockEnabled: parseBoolean(process.env.ACCOUNT_LOCK_ENABLED, true),
    accountLockMaxAttempts: Number.parseInt(process.env.ACCOUNT_LOCK_MAX_ATTEMPTS || "5", 10),
    accountLockWindowSeconds: Number.parseInt(process.env.ACCOUNT_LOCK_WINDOW_SECONDS || "900", 10),
    accountLockTtlSeconds: Number.parseInt(process.env.ACCOUNT_LOCK_TTL_SECONDS || "900", 10),

    // Registration
    registerRequireReview: parseBoolean(process.env.AUTH_REGISTER_REQUIRE_REVIEW, false),

    // Ticket Validation
    ticketValidateEnabled: parseBoolean(process.env.TICKET_VALIDATE_ENABLED, true),

    // Security Audit
    securityAuditEnabled: parseBoolean(process.env.SECURITY_AUDIT_ENABLED, true),

    // Internal API Token
    internalApiToken: process.env.INTERNAL_API_TOKEN || "",
    strictSecurity: parseBoolean(
      process.env.AUTH_STRICT_SECURITY,
      env === "production"
    )
  };

  emitLegacyDirectConfigWarnings(config.appName, config.legacyDirectConfigWarnings);
  validateProductionConfig(config);
  validateDiscoveryConfig(config);
  return config;
}
