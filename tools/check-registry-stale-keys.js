import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import {
  normalizeServiceInstance,
  registryHeartbeatKey,
  registryInstanceScanPattern,
  validateServiceInstance
} from "../packages/service-registry/node/registry-schema.js";

const DEFAULT_REGISTRY_URL = "redis://127.0.0.1:6379";
const INSTANCE_SCAN_COUNT = 100;

export async function checkRegistryStaleKeys(options = {}) {
  const config = normalizeOptions(options);
  const errors = [];
  let snapshot;

  try {
    snapshot = config.fixturePath
      ? await readFixtureSnapshot(config)
      : await readRedisSnapshot(config, options.redis);
  } catch (error) {
    const loadError = errorObject(
      "snapshot_load_failed",
      `failed to load registry stale-key snapshot: ${error?.message || error}`
    );
    return buildReport(config, {
      source: config.fixturePath ? "fixture" : "registry",
      instanceKeys: [],
      errors: [loadError]
    }, [loadError]);
  }

  const staleInstances = [];
  for (const record of snapshot.instanceKeys) {
    if (!record.heartbeatExists) {
      staleInstances.push(staleInstanceReport(record));
    }
  }

  errors.push(...snapshot.errors);
  return buildReport(config, snapshot, errors, staleInstances);
}

async function readRedisSnapshot(config, injectedRedis = null) {
  const redis = injectedRedis ?? await createRedisClient(config);
  const ownsRedis = !injectedRedis;
  const errors = [];
  const instanceKeys = [];

  try {
    const keys = await scanInstanceKeys(redis, config);
    for (const key of keys) {
      const parsed = parseRegistryInstanceKey(config.registryKeyPrefix, key);
      if (!parsed) {
        errors.push(errorObject(
          "invalid_instance_key",
          `registry instance key does not match expected layout: ${key}`,
          { instanceKey: key }
        ));
        continue;
      }

      if (!serviceAllowed(config, parsed.service)) {
        continue;
      }

      instanceKeys.push(await inspectInstanceKey(redis, config, parsed, errors));
    }
  } finally {
    if (ownsRedis) {
      await closeRedis(redis);
    }
  }

  return snapshotResult("registry", instanceKeys, errors);
}

async function readFixtureSnapshot(config) {
  const payload = JSON.parse(fs.readFileSync(config.fixturePath, "utf8"));
  const redis = FixtureRedis.fromPayload(payload, config.registryKeyPrefix);
  const snapshot = await readRedisSnapshot(config, redis);
  return {
    ...snapshot,
    source: "fixture"
  };
}

async function createRedisClient(config) {
  const { default: Redis } = await import("ioredis");
  const redis = new Redis(config.registryUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableOfflineQueue: false
  });
  await redis.connect();
  return redis;
}

async function closeRedis(redis) {
  if (typeof redis.disconnect === "function") {
    redis.disconnect();
  } else if (typeof redis.quit === "function") {
    await redis.quit();
  }
}

async function scanInstanceKeys(redis, config) {
  const serviceNames = config.services;
  if (serviceNames.length === 0) {
    return scanKeys(redis, `${config.registryKeyPrefix}service:*:instances:*`);
  }

  const keys = [];
  for (const serviceName of serviceNames) {
    keys.push(...await scanKeys(
      redis,
      registryInstanceScanPattern(config.registryKeyPrefix, serviceName)
    ));
  }
  return uniqueSorted(keys);
}

async function inspectInstanceKey(redis, config, parsed, errors) {
  const { service, instanceId, instanceKey } = parsed;
  const heartbeatKey = registryHeartbeatKey(config.registryKeyPrefix, service, instanceId);
  const rawData = await redis.hget(instanceKey, "data");
  const heartbeatExists = await redis.exists(heartbeatKey) === 1;
  const record = {
    service,
    instanceId,
    instanceKey,
    heartbeatKey,
    heartbeatExists,
    rawDataPresent: rawData !== null && rawData !== undefined && rawData !== "",
    registeredAt: null,
    endpoints: []
  };

  if (!record.rawDataPresent) {
    errors.push(errorObject(
      "missing_registry_data",
      `registry instance ${service}.${instanceId} is missing hash field data`,
      { service, instanceId, instanceKey }
    ));
    return record;
  }

  let parsedPayload;
  try {
    parsedPayload = JSON.parse(rawData);
  } catch (error) {
    errors.push(errorObject(
      "invalid_registry_json",
      `invalid registry JSON for ${service}.${instanceId}: ${error?.message || error}`,
      { service, instanceId, instanceKey }
    ));
    return record;
  }

  const normalized = normalizeServiceInstance(parsedPayload);
  if (!normalized) {
    errors.push(errorObject(
      "invalid_registry_schema",
      `invalid registry payload schema for ${service}.${instanceId}`,
      { service, instanceId, instanceKey, validationErrors: validationErrorsForPayload(parsedPayload) }
    ));
    record.registeredAt = registeredAtCandidate(parsedPayload);
    return record;
  }

  record.registeredAt = normalized.registered_at;
  record.endpoints = endpointSummaries(normalized);

  if (normalized.name !== service) {
    errors.push(errorObject(
      "registry_service_mismatch",
      `registry key service ${service} does not match payload service ${normalized.name}`,
      { service, instanceId, instanceKey, payloadService: normalized.name }
    ));
  }
  if (normalized.id !== instanceId) {
    errors.push(errorObject(
      "registry_instance_id_mismatch",
      `registry key instance ${instanceId} does not match payload id ${normalized.id}`,
      { service, instanceId, instanceKey, payloadInstanceId: normalized.id }
    ));
  }

  return record;
}

async function scanKeys(redis, pattern) {
  const keys = [];
  let cursor = "0";

  do {
    const [nextCursor, batch] = await redis.scan(
      cursor,
      "MATCH",
      pattern,
      "COUNT",
      INSTANCE_SCAN_COUNT
    );
    cursor = nextCursor;
    keys.push(...batch);
  } while (cursor !== "0");

  return uniqueSorted(keys);
}

function buildReport(config, snapshot, errors, staleInstances = []) {
  const services = serviceSummary(config, snapshot.instanceKeys);
  const summary = summaryFields(services, errors, staleInstances);
  return {
    ok: staleInstances.length === 0 && errors.length === 0,
    generatedAt: config.generatedAt,
    source: snapshot.source,
    registryUrl: redactRegistryUrl(config.registryUrl),
    registryKeyPrefix: config.registryKeyPrefix,
    fixturePath: config.fixturePath || "",
    requestedServices: config.services,
    services,
    summary,
    staleInstances: staleInstances.sort(compareStaleInstances),
    errors
  };
}

function snapshotResult(source, instanceKeys, errors) {
  return {
    source,
    instanceKeys: instanceKeys.sort(compareRecords),
    errors
  };
}

function serviceSummary(config, records) {
  const byService = new Map();
  for (const serviceName of config.services) {
    byService.set(serviceName, emptyServiceSummary());
  }

  for (const record of records) {
    const summary = byService.get(record.service) ?? emptyServiceSummary();
    summary.instanceKeys += 1;
    if (record.heartbeatExists) {
      summary.heartbeatPresent += 1;
    } else {
      summary.staleInstances += 1;
    }
    if (!record.rawDataPresent) {
      summary.missingData += 1;
    }
    byService.set(record.service, summary);
  }

  return Object.fromEntries(
    [...byService.entries()]
      .sort(([left], [right]) => left.localeCompare(right))
  );
}

function emptyServiceSummary() {
  return {
    instanceKeys: 0,
    heartbeatPresent: 0,
    staleInstances: 0,
    missingData: 0
  };
}

function summaryFields(services, errors, staleInstances) {
  const serviceValues = Object.values(services);
  return {
    services: serviceValues.length,
    instanceKeys: serviceValues.reduce((total, service) => total + service.instanceKeys, 0),
    heartbeatPresent: serviceValues.reduce((total, service) => total + service.heartbeatPresent, 0),
    staleInstances: staleInstances.length,
    missingData: serviceValues.reduce((total, service) => total + service.missingData, 0),
    invalidJson: errors.filter((error) => error.code === "invalid_registry_json").length,
    invalidSchema: errors.filter((error) => error.code === "invalid_registry_schema").length,
    keyMismatches: errors.filter((error) =>
      error.code === "registry_service_mismatch" ||
      error.code === "registry_instance_id_mismatch"
    ).length,
    errors: errors.length
  };
}

function staleInstanceReport(record) {
  return {
    service: record.service,
    instanceId: record.instanceId,
    instanceKey: record.instanceKey,
    heartbeatKey: record.heartbeatKey,
    registeredAt: record.registeredAt,
    endpoints: record.endpoints
  };
}

function endpointSummaries(instance) {
  return [...(instance.endpoints ?? [])]
    .map((endpoint) => ({
      name: endpoint.name,
      protocol: endpoint.protocol,
      visibility: endpoint.visibility,
      host: endpoint.host,
      port: endpoint.port,
      socket: endpoint.socket,
      healthy: endpoint.healthy
    }))
    .sort((left, right) =>
      left.name.localeCompare(right.name) ||
      left.protocol.localeCompare(right.protocol) ||
      left.visibility.localeCompare(right.visibility)
    );
}

function registeredAtCandidate(payload) {
  const value = payload?.registered_at ?? payload?.registeredAt;
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : null;
}

function redactRegistryUrl(value) {
  const text = String(value ?? "");
  if (!text) {
    return "";
  }

  try {
    const url = new URL(text);
    if (url.username) {
      url.username = "***";
    }
    if (url.password) {
      url.password = "***";
    }

    for (const [key] of url.searchParams) {
      if (isSensitiveUrlParam(key)) {
        url.searchParams.set(key, "***");
      }
    }
    return url.toString();
  } catch {
    return "<invalid-registry-url>";
  }
}

function isSensitiveUrlParam(key) {
  return /^(access_token|auth|credential|credentials|pass|password|refresh_token|secret|token)$/i
    .test(String(key ?? ""));
}

function validationErrorsForPayload(payload) {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return ["payload must be an object"];
  }
  return validateServiceInstance(payload).errors ?? [];
}

function parseRegistryInstanceKey(registryKeyPrefix, key) {
  const prefix = String(registryKeyPrefix ?? "");
  if (!String(key).startsWith(prefix)) {
    return null;
  }

  const body = String(key).slice(prefix.length);
  const match = /^service:([^:]+):instances:(.+)$/.exec(body);
  if (!match) {
    return null;
  }

  return {
    service: match[1],
    instanceId: match[2],
    instanceKey: String(key)
  };
}

function serviceAllowed(config, serviceName) {
  return config.services.length === 0 || config.services.includes(serviceName);
}

function normalizeOptions(options) {
  const fixturePath = options.fixturePath
    ? path.resolve(String(options.fixturePath))
    : "";
  return {
    generatedAt: options.generatedAt ?? new Date().toISOString(),
    registryUrl: String(
      options.registryUrl ??
      process.env.REGISTRY_URL ??
      process.env.REDIS_URL ??
      DEFAULT_REGISTRY_URL
    ),
    registryKeyPrefix: String(
      options.registryKeyPrefix ??
      process.env.REGISTRY_KEY_PREFIX ??
      process.env.REDIS_KEY_PREFIX ??
      ""
    ),
    services: normalizeServiceFilters(options.services ?? options.service ?? []),
    fixturePath
  };
}

function normalizeServiceFilters(value) {
  const values = Array.isArray(value) ? value : [value];
  return uniqueSorted(values.flatMap((item) =>
    String(item ?? "")
      .split(",")
      .map((part) => part.trim())
      .filter(Boolean)
  ));
}

function compareRecords(left, right) {
  return left.service.localeCompare(right.service) ||
    left.instanceId.localeCompare(right.instanceId) ||
    left.instanceKey.localeCompare(right.instanceKey);
}

function compareStaleInstances(left, right) {
  return left.service.localeCompare(right.service) ||
    left.instanceId.localeCompare(right.instanceId);
}

function errorObject(code, message, extra = {}) {
  return { code, message, ...extra };
}

function uniqueSorted(values) {
  return [...new Set(values.filter((value) => value !== undefined && value !== null).map(String))]
    .sort();
}

class FixtureRedis {
  constructor() {
    this.hashes = new Map();
    this.values = new Map();
  }

  static fromPayload(payload, registryKeyPrefix) {
    const redis = new FixtureRedis();
    redis.load(payload, registryKeyPrefix);
    return redis;
  }

  load(payload, registryKeyPrefix) {
    if (Array.isArray(payload)) {
      this.loadInstances(payload, registryKeyPrefix);
      return;
    }

    this.loadKeyMap(payload?.keys);
    this.loadHashes(payload?.hashes);
    this.loadHeartbeatKeys(payload?.heartbeats ?? payload?.heartbeatKeys);
    this.loadInstances(payload?.instances, registryKeyPrefix);
    this.loadInstances(payload?.registry, registryKeyPrefix);
    this.loadServiceMap(payload?.services, registryKeyPrefix);
  }

  loadKeyMap(keys) {
    if (!isPlainObject(keys)) {
      return;
    }

    for (const [key, value] of Object.entries(keys)) {
      if (isHeartbeatKey(key)) {
        this.values.set(key, "1");
        continue;
      }
      this.loadHashRecord(key, value);
    }
  }

  loadHashes(hashes) {
    if (!isPlainObject(hashes)) {
      return;
    }

    for (const [key, value] of Object.entries(hashes)) {
      this.loadHashRecord(key, value);
    }
  }

  loadHashRecord(key, value) {
    const hash = new Map();
    if (typeof value === "string") {
      hash.set("data", value);
    } else if (isPlainObject(value)) {
      const fields = isPlainObject(value.fields) ? value.fields : value;
      if ("data" in fields) {
        hash.set("data", serializeFixtureData(fields.data));
      }
    }
    this.hashes.set(key, hash);
  }

  loadHeartbeatKeys(heartbeats) {
    if (Array.isArray(heartbeats)) {
      for (const item of heartbeats) {
        const key = typeof item === "string" ? item : item?.key;
        if (key) {
          this.values.set(String(key), "1");
        }
      }
    } else if (isPlainObject(heartbeats)) {
      for (const [key, value] of Object.entries(heartbeats)) {
        if (value !== false && value !== null) {
          this.values.set(key, "1");
        }
      }
    }
  }

  loadInstances(instances, registryKeyPrefix) {
    if (!Array.isArray(instances)) {
      return;
    }

    for (const item of instances) {
      this.loadInstanceItem(item, registryKeyPrefix);
    }
  }

  loadServiceMap(services, registryKeyPrefix) {
    if (!isPlainObject(services)) {
      return;
    }

    for (const [serviceName, instances] of Object.entries(services)) {
      if (!Array.isArray(instances)) {
        continue;
      }
      for (const item of instances) {
        this.loadInstanceItem({ service: serviceName, data: item, heartbeat: item?.heartbeat }, registryKeyPrefix);
      }
    }
  }

  loadInstanceItem(item, registryKeyPrefix) {
    if (!item || typeof item !== "object") {
      return;
    }

    const data = "data" in item
      ? item.data
      : ("payload" in item ? item.payload : ("instance" in item ? item.instance : item));
    const service = String(item.service ?? item.serviceName ?? data?.name ?? "");
    const instanceId = String(item.instanceId ?? item.instance_id ?? data?.id ?? "");
    if (!service || !instanceId) {
      return;
    }

    const instanceKey = String(
      item.instanceKey ??
      item.key ??
      `${registryKeyPrefix}service:${service}:instances:${instanceId}`
    );
    const heartbeatKey = String(
      item.heartbeatKey ??
      `${registryKeyPrefix}heartbeat:${service}:${instanceId}`
    );

    const hash = new Map();
    if (!item.missingData && data !== undefined && data !== null) {
      hash.set("data", serializeFixtureData(data));
    }
    this.hashes.set(instanceKey, hash);

    const heartbeatExists = item.heartbeatExists ?? item.heartbeat ?? true;
    if (heartbeatExists) {
      this.values.set(heartbeatKey, "1");
    }
  }

  async hget(key, field) {
    return this.hashes.get(key)?.get(field) ?? null;
  }

  async exists(key) {
    return this.hashes.has(key) || this.values.has(key) ? 1 : 0;
  }

  async scan(cursor, ...args) {
    if (cursor !== "0") {
      return ["0", []];
    }

    const matchIndex = args.findIndex((arg) => String(arg).toUpperCase() === "MATCH");
    const pattern = matchIndex >= 0 ? String(args[matchIndex + 1]) : "*";
    const keys = [...new Set([...this.hashes.keys(), ...this.values.keys()])]
      .filter((key) => matchesGlob(key, pattern))
      .sort();
    return ["0", keys];
  }
}

function serializeFixtureData(data) {
  return typeof data === "string" ? data : JSON.stringify(data);
}

function isHeartbeatKey(key) {
  return /(^|:)heartbeat:[^:]+:.+$/.test(key);
}

function matchesGlob(value, pattern) {
  const escaped = pattern
    .replace(/[.+?^${}()|[\]\\]/g, "\\$&")
    .replace(/\*/g, ".*");
  return new RegExp(`^${escaped}$`).test(value);
}

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function parseArgs(argv) {
  const args = {
    services: [],
    pretty: true
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--registry-url") {
      args.registryUrl = argv[index + 1];
      index += 1;
    } else if (arg === "--registry-key-prefix") {
      args.registryKeyPrefix = argv[index + 1] ?? "";
      index += 1;
    } else if (arg === "--service") {
      args.services.push(argv[index + 1] ?? "");
      index += 1;
    } else if (arg === "--fixture") {
      args.fixturePath = path.resolve(argv[index + 1]);
      index += 1;
    } else if (arg === "--compact") {
      args.pretty = false;
    } else if (arg === "--pretty") {
      args.pretty = true;
    } else if (arg === "--help" || arg === "-h") {
      args.help = true;
    } else {
      throw new Error(`unknown argument: ${arg}`);
    }
  }

  return args;
}

function printHelp() {
  console.log([
    "Usage: node tools/check-registry-stale-keys.js [options]",
    "",
    "Scans service registry instance keys and reports records whose heartbeat key is missing.",
    "",
    "Options:",
    "  --registry-url <url>           Redis registry URL (default: REGISTRY_URL, REDIS_URL, or redis://127.0.0.1:6379)",
    "  --registry-key-prefix <value>  Registry key prefix",
    "  --service <name[,name]>        Limit scan to one or more services; can be repeated",
    "  --fixture <file>               Read a registry-key fixture instead of Redis",
    "  --compact                      Emit compact JSON",
    "  --pretty                       Emit formatted JSON (default)",
    "  --help                         Show this help"
  ].join("\n"));
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    printHelp();
    return;
  }

  const report = await checkRegistryStaleKeys(args);
  console.log(JSON.stringify(report, null, args.pretty ? 2 : 0));
  if (!report.ok) {
    process.exitCode = 1;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(error?.stack || error?.message || String(error));
    process.exitCode = 1;
  });
}
