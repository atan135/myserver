import crypto from "node:crypto";
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

const DEFAULT_JWT_SECRETS = new Set([
  "dev-only-change-this-jwt-secret",
  "replace-with-a-long-random-string-for-jwt"
]);

const DEFAULT_GAME_ADMIN_TOKENS = new Set([
  "dev-only-change-this-game-admin-token"
]);

const DEFAULT_GAME_PROXY_ADMIN_TOKENS = new Set([
  "dev-only-change-this-proxy-admin-token",
  "dev-only-change-this-proxy-admin-read-token"
]);

const DEFAULT_INITIAL_ADMIN_PASSWORDS = new Set([
  "AdminPass123!"
]);
const DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME = "DISALLOW_LEGACY_DIRECT_CONFIG";
const LEGACY_DIRECT_CONFIG_ENV_NAMES = [
  "GAME_SERVER_ADMIN_HOST",
  "GAME_SERVER_ADMIN_PORT",
  "GAME_PROXY_ADMIN_HOST",
  "GAME_PROXY_ADMIN_PORT"
];

const MYFORGE_ID_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const MYFORGE_NUMERIC_LIMITS = Object.freeze({
  MYFORGE_AUTH_TTL_MS: { key: "authTtlMs", fallback: 60000, min: 5000, max: 300000 },
  MYFORGE_COMMAND_TTL_MS: { key: "commandTtlMs", fallback: 60000, min: 5000, max: 300000 },
  MYFORGE_CLOCK_SKEW_MS: { key: "clockSkewMs", fallback: 5000, min: 0, max: 30000 },
  MYFORGE_HEARTBEAT_INTERVAL_MS: { key: "heartbeatIntervalMs", fallback: 15000, min: 1000, max: 60000 },
  MYFORGE_HEARTBEAT_TIMEOUT_MS: { key: "heartbeatTimeoutMs", fallback: 45000, min: 3000, max: 180000 },
  MYFORGE_QUEUE_TTL_MS: { key: "queueTtlMs", fallback: 900000, min: 10000, max: 86400000 },
  MYFORGE_COMMAND_TIMEOUT_MS: { key: "commandTimeoutMs", fallback: 600000, min: 1000, max: 1800000 },
  MYFORGE_CANCEL_TIMEOUT_MS: { key: "cancelTimeoutMs", fallback: 10000, min: 1000, max: 30000 },
  MYFORGE_MAX_OUTPUT_BYTES: { key: "maxOutputBytes", fallback: 1048576, min: 4096, max: 4194304 },
  MYFORGE_WS_MAX_MESSAGE_BYTES: { key: "wsMaxMessageBytes", fallback: 16777216, min: 524288, max: 33554432 },
  MYFORGE_WS_WRITE_TIMEOUT_MS: { key: "wsWriteTimeoutMs", fallback: 5000, min: 1000, max: 30000 }
});

function createMyforgeConfigError(name, reason) {
  const error = new Error(`MYFORGE_CONFIG_INVALID: ${name} ${reason}`);
  error.code = "MYFORGE_CONFIG_INVALID";
  error.configName = name;
  return error;
}

function trimAsciiWhitespace(value) {
  return value.replace(/^[\t\n\v\f\r ]+|[\t\n\v\f\r ]+$/g, "");
}

export function strictBoolean(name, env, fallback = false) {
  if (!Object.prototype.hasOwnProperty.call(env, name) || env[name] === undefined) {
    return fallback;
  }

  const value = trimAsciiWhitespace(String(env[name]));
  if (value === "true" || value === "1") return true;
  if (value === "false" || value === "0") return false;
  throw createMyforgeConfigError(name, "invalid boolean");
}

function strictInteger(name, env, { fallback, min, max }) {
  if (!Object.prototype.hasOwnProperty.call(env, name) || env[name] === undefined) {
    return fallback;
  }

  const raw = String(env[name]);
  if (!/^[0-9]+$/.test(raw)) {
    throw createMyforgeConfigError(name, "must be an unsigned decimal integer");
  }

  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value < min || value > max) {
    throw createMyforgeConfigError(name, `must be between ${min} and ${max}`);
  }
  return value;
}

function requiredConfigText(name, env) {
  const value = trimAsciiWhitespace(String(env[name] ?? ""));
  if (!value) {
    throw createMyforgeConfigError(name, "is required when MYFORGE_ENABLED=true");
  }
  return value;
}

function readPemFile(name, configuredPath, cwd, kind) {
  const resolvedPath = path.resolve(cwd, configuredPath);
  let pem;
  try {
    pem = fs.readFileSync(resolvedPath, "utf8");
  } catch {
    throw createMyforgeConfigError(name, "is not readable");
  }

  const normalized = pem.trim();
  const header = kind === "private" ? "PRIVATE KEY" : "PUBLIC KEY";
  if (!normalized.startsWith(`-----BEGIN ${header}-----`) ||
      !normalized.endsWith(`-----END ${header}-----`)) {
    const format = kind === "private" ? "PKCS#8" : "SPKI";
    throw createMyforgeConfigError(name, `must contain an Ed25519 ${format} PEM key`);
  }

  try {
    const key = kind === "private" ? crypto.createPrivateKey(pem) : crypto.createPublicKey(pem);
    if (key.asymmetricKeyType !== "ed25519") {
      throw new Error("wrong key type");
    }
    return { key, resolvedPath };
  } catch {
    const format = kind === "private" ? "PKCS#8" : "SPKI";
    throw createMyforgeConfigError(name, `must contain an Ed25519 ${format} PEM key`);
  }
}

function publicKeyFingerprint(publicKey) {
  const der = publicKey.export({ format: "der", type: "spki" });
  return crypto.createHash("sha256").update(der).digest("hex");
}

function parseKnownMyforgeAgents(env, cwd) {
  const name = "MYFORGE_AGENT_PUBLIC_KEYS_JSON";
  const raw = requiredConfigText(name, env);
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw createMyforgeConfigError(name, "must be valid JSON");
  }

  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw createMyforgeConfigError(name, "must be a JSON object");
  }

  return Object.entries(parsed).map(([agentId, entry]) => {
    if (!MYFORGE_ID_PATTERN.test(agentId)) {
      throw createMyforgeConfigError(name, "contains an invalid agentId");
    }
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
      throw createMyforgeConfigError(name, `agent ${agentId} must be an object`);
    }

    const allowedFields = new Set(["projectId", "publicKeyPath", "label"]);
    const unknownField = Object.keys(entry).find((field) => !allowedFields.has(field));
    if (unknownField) {
      throw createMyforgeConfigError(name, `agent ${agentId} contains unknown field ${unknownField}`);
    }

    const projectId = typeof entry.projectId === "string" ? entry.projectId : "";
    if (!MYFORGE_ID_PATTERN.test(projectId)) {
      throw createMyforgeConfigError(name, `agent ${agentId} contains an invalid projectId`);
    }
    const publicKeyPath = typeof entry.publicKeyPath === "string"
      ? trimAsciiWhitespace(entry.publicKeyPath)
      : "";
    if (!publicKeyPath) {
      throw createMyforgeConfigError(name, `agent ${agentId} publicKeyPath is required`);
    }
    const rawLabel = entry.label === undefined || entry.label === null
      ? null
      : typeof entry.label === "string" ? entry.label : null;
    const label = rawLabel === null ? null : trimAsciiWhitespace(rawLabel);
    if (entry.label !== undefined && entry.label !== null && (
      !label ||
      rawLabel === null ||
      /[\u0000-\u001f\u007f]/.test(rawLabel) ||
      Buffer.byteLength(label, "utf8") > 128
    )) {
      throw createMyforgeConfigError(
        name,
        `agent ${agentId} label must be 1 to 128 UTF-8 bytes without control characters`
      );
    }

    const loaded = readPemFile(name, publicKeyPath, cwd, "public");
    return {
      agentId,
      projectId,
      label,
      publicKeyPath: loaded.resolvedPath,
      publicKey: loaded.key,
      publicKeyFingerprint: publicKeyFingerprint(loaded.key)
    };
  });
}

function validateMyforgeLimitInvariants(limits) {
  if (2 * limits.clockSkewMs >= limits.authTtlMs) {
    throw createMyforgeConfigError("MYFORGE_CLOCK_SKEW_MS", "must satisfy 2 * clock skew < auth TTL");
  }
  if (2 * limits.clockSkewMs >= limits.commandTtlMs) {
    throw createMyforgeConfigError("MYFORGE_CLOCK_SKEW_MS", "must satisfy 2 * clock skew < command TTL");
  }
  if (limits.heartbeatTimeoutMs < 2 * limits.heartbeatIntervalMs + limits.clockSkewMs) {
    throw createMyforgeConfigError(
      "MYFORGE_HEARTBEAT_TIMEOUT_MS",
      "must be at least 2 * heartbeat interval + clock skew"
    );
  }
  if (limits.cancelTimeoutMs > limits.commandTimeoutMs) {
    throw createMyforgeConfigError("MYFORGE_CANCEL_TIMEOUT_MS", "must not exceed command timeout");
  }
  if (limits.wsWriteTimeoutMs >= limits.authTtlMs || limits.wsWriteTimeoutMs >= limits.commandTtlMs) {
    throw createMyforgeConfigError("MYFORGE_WS_WRITE_TIMEOUT_MS", "must be less than auth TTL and command TTL");
  }
}

export function createMyforgeServerConfig(env = process.env, cwd = process.cwd()) {
  const enabled = strictBoolean("MYFORGE_ENABLED", env, false);
  const limits = {};
  for (const [name, definition] of Object.entries(MYFORGE_NUMERIC_LIMITS)) {
    limits[definition.key] = strictInteger(name, env, definition);
  }
  validateMyforgeLimitInvariants(limits);

  if (!enabled) {
    return {
      enabled,
      ...limits,
      serverPrivateKeyPath: null,
      serverPublicKeyPath: null,
      serverPrivateKey: null,
      serverPublicKey: null,
      serverPublicKeyFingerprint: null,
      agents: [],
      agentsById: new Map()
    };
  }

  const privatePath = requiredConfigText("MYFORGE_SERVER_PRIVATE_KEY_PATH", env);
  const publicPath = requiredConfigText("MYFORGE_SERVER_PUBLIC_KEY_PATH", env);
  const privateKey = readPemFile("MYFORGE_SERVER_PRIVATE_KEY_PATH", privatePath, cwd, "private");
  const publicKey = readPemFile("MYFORGE_SERVER_PUBLIC_KEY_PATH", publicPath, cwd, "public");
  const derivedPublic = crypto.createPublicKey(privateKey.key).export({ format: "der", type: "spki" });
  const configuredPublic = publicKey.key.export({ format: "der", type: "spki" });
  if (!crypto.timingSafeEqual(derivedPublic, configuredPublic)) {
    throw createMyforgeConfigError(
      "MYFORGE_SERVER_PUBLIC_KEY_PATH",
      "does not match MYFORGE_SERVER_PRIVATE_KEY_PATH"
    );
  }

  const agents = parseKnownMyforgeAgents(env, cwd);
  return {
    enabled,
    ...limits,
    serverPrivateKeyPath: privateKey.resolvedPath,
    serverPublicKeyPath: publicKey.resolvedPath,
    serverPrivateKey: privateKey.key,
    serverPublicKey: publicKey.key,
    serverPublicKeyFingerprint: publicKeyFingerprint(publicKey.key),
    agents,
    agentsById: new Map(agents.map((agent) => [agent.agentId, agent]))
  };
}

function parseCsv(value) {
  if (typeof value !== "string") return [];
  return value
    .split(",")
    .map((item) => item.trim())
    .filter(Boolean);
}

function parsePositiveInteger(name, value, fallback) {
  const parsed = Number.parseInt(value ?? String(fallback), 10);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(`${name} must be a positive integer`);
  }
  return parsed;
}

function parsePositiveIntegerWithFallback(value, fallback) {
  const parsed = Number.parseInt(value ?? String(fallback), 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function parseBoundedPositiveInteger(name, value, fallback, min, max) {
  const raw = value ?? String(fallback);
  if (!/^\d+$/.test(String(raw))) {
    throw new Error(`${name} must be an integer between ${min} and ${max}`);
  }
  const parsed = Number(raw);
  if (!Number.isSafeInteger(parsed) || parsed < min || parsed > max) {
    throw new Error(`${name} must be an integer between ${min} and ${max}`);
  }
  return parsed;
}

function parseNonNegativeIntegerWithFallback(value, fallback) {
  const parsed = Number.parseInt(value ?? String(fallback), 10);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
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

function deriveDatabaseUrl(databaseUrl, databaseName) {
  const value = typeof databaseUrl === "string" ? databaseUrl.trim() : "";
  if (!value) {
    return value;
  }

  try {
    const url = new URL(value);
    if (url.protocol !== "postgres:" && url.protocol !== "postgresql:") {
      return value;
    }

    url.pathname = `/${databaseName}`;
    return url.toString();
  } catch {
    const derived = value.replace(/myserver_auth(?=([?#]|$))/, databaseName);
    return derived || value;
  }
}

function resolveGameDatabaseUrl(databaseUrl) {
  return firstNonEmptyEnv(["GAME_DATABASE_URL", "ADMIN_GAME_DATABASE_URL"])
    || deriveDatabaseUrl(databaseUrl, "myserver_game");
}

function parseDurationSeconds(value, fallbackSeconds) {
  if (value === undefined || value === null || value === "") {
    return fallbackSeconds;
  }

  if (typeof value === "number") {
    return Number.isFinite(value) && value > 0 ? Math.floor(value) : fallbackSeconds;
  }

  const text = String(value).trim();
  if (/^\d+$/.test(text)) {
    return Number.parseInt(text, 10);
  }

  const match = text.match(/^(\d+)(s|m|h|d)$/i);
  if (!match) {
    return fallbackSeconds;
  }

  const amount = Number.parseInt(match[1], 10);
  const unit = match[2].toLowerCase();
  const multiplier = unit === "s" ? 1 : unit === "m" ? 60 : unit === "h" ? 3600 : 86400;
  return amount * multiplier;
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
    throw new Error("Invalid admin-api discovery config: DISCOVERY_REQUIRED=true requires REGISTRY_ENABLED=true");
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
  if (config.env !== "production") {
    return;
  }

  const errors = [];
  if (!config.jwtSecret || DEFAULT_JWT_SECRETS.has(config.jwtSecret)) {
    errors.push("JWT_SECRET must be set to a non-default value in production");
  }

  if (!config.gameAdminToken || DEFAULT_GAME_ADMIN_TOKENS.has(config.gameAdminToken)) {
    errors.push("GAME_ADMIN_TOKEN must be set to a non-default value in production");
  }

  if (!String(config.adminAssertionPrivateKey || "").trim()) {
    errors.push("ADMIN_ASSERTION_PRIVATE_KEY must be configured in production");
  }

  const gameProxyAdminToken = String(config.gameProxyAdminToken || "").trim();
  const gameProxyAdminReadToken = String(config.gameProxyAdminReadToken || "").trim();
  if (gameProxyAdminReadToken && DEFAULT_GAME_PROXY_ADMIN_TOKENS.has(gameProxyAdminReadToken)) {
    errors.push("GAME_PROXY_ADMIN_READ_TOKEN must be set to a non-default value in production");
  }
  if (!gameProxyAdminReadToken && (!gameProxyAdminToken || DEFAULT_GAME_PROXY_ADMIN_TOKENS.has(gameProxyAdminToken))) {
    errors.push("GAME_PROXY_ADMIN_READ_TOKEN or GAME_PROXY_ADMIN_TOKEN must be set to a non-default value in production");
  }

  if (!config.initialAdminPassword || DEFAULT_INITIAL_ADMIN_PASSWORDS.has(config.initialAdminPassword)) {
    errors.push("ADMIN_PASSWORD must be set to a non-default value in production");
  }

  if (errors.length > 0) {
    throw new Error(`Invalid admin-api production config: ${errors.join("; ")}`);
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
  const jwtExpiresIn = process.env.JWT_EXPIRES_IN || "8h";
  const jwtExpiresInSeconds = parseDurationSeconds(jwtExpiresIn, 28800);
  const bindHost = firstNonEmptyEnv(["SERVICE_BIND_HOST", "HOST"]) || "127.0.0.1";
  const localDiscoveryFallbackEnabled = isLocalDiscoveryFallbackEnv();
  const registryDiscoveryEnabled = parseBoolean(process.env.REGISTRY_ENABLED, false);
  const registryDiscoveryRequired = parseBoolean(process.env.DISCOVERY_REQUIRED, false) || isStrictDiscoveryEnv();
  const disallowLegacyDirectConfig = parseBoolean(process.env[DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME], false);
  const databaseUrl = process.env.DATABASE_URL || "postgresql://postgres:password@127.0.0.1:5432/myserver_auth";
  const dbPoolSize = parsePositiveInteger("DB_POOL_SIZE", process.env.DB_POOL_SIZE, 10);
  const myforge = createMyforgeServerConfig(process.env, process.cwd());
  validateLegacyDirectConfig(
    "admin-api",
    LEGACY_DIRECT_CONFIG_ENV_NAMES,
    disallowLegacyDirectConfig,
    registryDiscoveryRequired
  );
  const legacyDirectConfigWarnings = collectLegacyDirectConfigWarnings(
    LEGACY_DIRECT_CONFIG_ENV_NAMES,
    registryDiscoveryRequired || !localDiscoveryFallbackEnabled
  );
  const config = {
    appName: "admin-api",
    env,
    host: bindHost,
    bindHost,
    advertisedHost: advertisedHostFromEnv(["SERVICE_ADVERTISED_HOST", "SERVICE_PUBLIC_HOST", "HOST"], bindHost),
    port: Number.parseInt(process.env.PORT || "3001", 10),
    logLevel: process.env.LOG_LEVEL || "info",
    logEnableConsole: parseBoolean(process.env.LOG_ENABLE_CONSOLE, true),
    logEnableFile: parseBoolean(process.env.LOG_ENABLE_FILE, true),
    logDir: process.env.LOG_DIR || "logs/admin-api",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    redisKeyPrefix: process.env.REDIS_KEY_PREFIX || "",
    registryKeyPrefix: process.env.REGISTRY_KEY_PREFIX ?? process.env.REDIS_KEY_PREFIX ?? "",
    natsUrl: process.env.NATS_URL || "nats://127.0.0.1:4222",
    serviceInstanceId:
      process.env.SERVICE_INSTANCE_ID || "admin-api-001",
    serviceName: process.env.SERVICE_NAME || "admin-api",
    serviceZone: process.env.SERVICE_ZONE || "local",
    serviceBuildVersion: process.env.SERVICE_BUILD_VERSION || "dev",
    databaseUrl,
    gameDatabaseUrl: resolveGameDatabaseUrl(databaseUrl),
    dbPoolSize,
    gameDbPoolSize: dbPoolSize,
    jwtSecret: process.env.JWT_SECRET || "dev-only-change-this-jwt-secret",
    jwtExpiresIn,
    adminSessionTtlSeconds: parsePositiveInteger(
      "ADMIN_SESSION_TTL_SECONDS",
      process.env.ADMIN_SESSION_TTL_SECONDS,
      jwtExpiresInSeconds
    ),
    adminLoginMaxFailures: parsePositiveInteger("ADMIN_LOGIN_MAX_FAILURES", process.env.ADMIN_LOGIN_MAX_FAILURES, 5),
    adminLoginFailureWindowSeconds: parsePositiveInteger(
      "ADMIN_LOGIN_FAILURE_WINDOW_SECONDS",
      process.env.ADMIN_LOGIN_FAILURE_WINDOW_SECONDS,
      900
    ),
    adminLoginLockSeconds: parsePositiveInteger("ADMIN_LOGIN_LOCK_SECONDS", process.env.ADMIN_LOGIN_LOCK_SECONDS, 900),
    trustProxy: parseBoolean(process.env.TRUST_PROXY, false),
    trustedProxies: parseCsv(process.env.TRUSTED_PROXIES),
    adminApiRequireTls: parseBoolean(process.env.ADMIN_API_REQUIRE_TLS, env === "production"),
    adminApiRequireIpAllowlist: parseBoolean(process.env.ADMIN_API_REQUIRE_IP_ALLOWLIST, false),
    adminApiIpAllowlist: parseCsv(process.env.ADMIN_API_IP_ALLOWLIST),
    gameServerAdminHost: localDiscoveryFallbackEnabled
      ? process.env.GAME_SERVER_ADMIN_HOST || "127.0.0.1"
      : "127.0.0.1",
    gameServerAdminPort: Number.parseInt(
      localDiscoveryFallbackEnabled ? process.env.GAME_SERVER_ADMIN_PORT || "7500" : "7500",
      10
    ),
    registryDiscoveryEnabled,
    registryDiscoveryRequired,
    registryDiscoveryCacheTtlMs: parseNonNegativeIntegerWithFallback(
      process.env.REGISTRY_DISCOVERY_CACHE_TTL_MS,
      1000
    ),
    registryDiscoveryRefreshIntervalMs: parsePositiveIntegerWithFallback(
      process.env.REGISTRY_DISCOVERY_REFRESH_INTERVAL_MS,
      5000
    ),
    localDiscoveryFallbackEnabled,
    disallowLegacyDirectConfig,
    legacyDirectConfigWarnings,
    gameAdminToken: process.env.GAME_ADMIN_TOKEN || "dev-only-change-this-game-admin-token",
    adminAssertionIssuer: process.env.ADMIN_ASSERTION_ISSUER || "admin-api",
    adminAssertionKeyId: process.env.ADMIN_ASSERTION_KEY_ID || "admin-api-v1",
    adminAssertionPrivateKey: process.env.ADMIN_ASSERTION_PRIVATE_KEY || "",
    adminAssertionTtlMs: parsePositiveIntegerWithFallback(process.env.ADMIN_ASSERTION_TTL_MS, 60000),
    adminOperationPreflightTtlMs: parseBoundedPositiveInteger(
      "ADMIN_OPERATION_PREFLIGHT_TTL_MS",
      process.env.ADMIN_OPERATION_PREFLIGHT_TTL_MS,
      120000,
      10000,
      900000
    ),
    gameAdminConnectTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_CONNECT_TIMEOUT_MS, 3000),
    gameAdminWriteTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_WRITE_TIMEOUT_MS, 3000),
    gameAdminReadTimeoutMs: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_READ_TIMEOUT_MS, 3000),
    gameAdminMaxResponseBytes: parsePositiveIntegerWithFallback(process.env.GAME_ADMIN_MAX_RESPONSE_BYTES, 1048576),
    gameProxyAdminHost: localDiscoveryFallbackEnabled
      ? process.env.GAME_PROXY_ADMIN_HOST || "127.0.0.1"
      : "127.0.0.1",
    gameProxyAdminPort: Number.parseInt(
      localDiscoveryFallbackEnabled ? process.env.GAME_PROXY_ADMIN_PORT || "7101" : "7101",
      10
    ),
    gameProxyAdminToken: process.env.GAME_PROXY_ADMIN_TOKEN || "dev-only-change-this-proxy-admin-token",
    gameProxyAdminReadToken: process.env.GAME_PROXY_ADMIN_READ_TOKEN || "",
    gameProxyAdminRequestTimeoutMs: parsePositiveIntegerWithFallback(
      process.env.GAME_PROXY_ADMIN_REQUEST_TIMEOUT_MS,
      3000
    ),
    gameProxyAdminMaxResponseBytes: parsePositiveIntegerWithFallback(
      process.env.GAME_PROXY_ADMIN_MAX_RESPONSE_BYTES,
      1048576
    ),
    initialAdminUsername: process.env.ADMIN_USERNAME || "admin",
    initialAdminPassword: process.env.ADMIN_PASSWORD || "AdminPass123!",
    initialAdminDisplayName: process.env.ADMIN_DISPLAY_NAME || "Administrator",
    bootstrapAdminRole: process.env.ADMIN_BOOTSTRAP_ROLE || "super_admin",
    myforge
  };

  emitLegacyDirectConfigWarnings(config.appName, config.legacyDirectConfigWarnings);
  validateProductionConfig(config);
  validateDiscoveryConfig(config);
  return config;
}
