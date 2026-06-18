import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

import {
  normalizeServiceInstance,
  registryHeartbeatKey,
  registryInstanceScanPattern
} from "../packages/service-registry/node/registry-schema.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const DEFAULT_ROOT_DIR = path.resolve(__dirname, "..");
const DEFAULT_REGISTRY_URL = "redis://127.0.0.1:6379";

const STRICT_ENV_NAMES = new Set(["production", "prod", "staging", "stage", "test", "testing"]);

export const DEPLOYMENT_DISCOVERY_CHECKS = [
  dependencyCheck("auth-http", "game-proxy", "client", ["kcp"], ["public"]),
  dependencyCheck("auth-http", "game-server", "admin", ["tcp"], ["admin"]),
  dependencyCheck("admin-api", "game-server", "admin", ["tcp"], ["admin"]),
  dependencyCheck("admin-api", "game-proxy", "admin", ["http"], ["admin"]),
  dependencyCheck("game-proxy", "game-server", "proxy-local", ["local_socket"], ["local"]),
  dependencyCheck("match-service", "game-server", "internal", ["local_socket"], ["local", "internal"]),
  dependencyCheck("mail-service", "game-server", "admin", ["tcp"], ["admin"]),
  dependencyCheck("game-server", "match-service", "grpc", ["grpc"], ["internal"]),
  publishedEndpointCheck("deployment-preflight", "auth-http", "http", ["http"], ["public"]),
  publishedEndpointCheck("deployment-preflight", "auth-http", "internal", ["http"], ["internal"]),
  publishedEndpointCheck("deployment-preflight", "admin-api", "http", ["http"], ["admin"]),
  publishedEndpointCheck("deployment-preflight", "mail-service", "http", ["http"], ["internal"]),
  publishedEndpointCheck("deployment-preflight", "announce-service", "http", ["http"], ["internal"])
];

export async function checkDeploymentDiscoveryDryRun(options = {}) {
  const config = normalizeOptions(options);
  const topLevelErrors = [];

  if (config.strict && config.registryEnabled === false) {
    topLevelErrors.push(errorObject(
      "strict_registry_disabled",
      "strict deployment discovery dry-run requires REGISTRY_ENABLED=true"
    ));
  }

  let snapshot;
  try {
    snapshot = config.fixturePath
      ? await readFixtureSnapshot(config)
      : await readRedisSnapshot(config);
  } catch (error) {
    const loadError = errorObject(
      "snapshot_load_failed",
      `failed to load registry snapshot: ${error?.message || error}`
    );
    return buildReport(config, {
      source: config.fixturePath ? "fixture" : "registry",
      services: {},
      serviceCounts: {},
      errors: [loadError]
    }, [], [...topLevelErrors, loadError]);
  }

  const checks = config.checks.map((definition) =>
    evaluateCheck(definition, snapshot.services, snapshot.source)
  );
  const checkErrors = checks.flatMap((check) =>
    check.errors.map((error) => ({
      ...error,
      consumer: check.consumer,
      targetService: check.targetService,
      endpointName: check.endpointName
    }))
  );
  const allErrors = [...topLevelErrors, ...snapshot.errors, ...checkErrors];

  return buildReport(config, snapshot, checks, allErrors);
}

function dependencyCheck(consumer, targetService, endpointName, protocols, visibilities) {
  return {
    kind: "consumer-dependency",
    consumer,
    targetService,
    endpointName,
    protocols,
    visibilities,
    required: true
  };
}

function publishedEndpointCheck(consumer, targetService, endpointName, protocols, visibilities) {
  return {
    kind: "published-endpoint",
    consumer,
    targetService,
    endpointName,
    protocols,
    visibilities,
    required: true
  };
}

function normalizeOptions(options) {
  const environmentName = String(
    options.environment ??
    process.env.MYSERVER_ENVIRONMENT_NAME ??
    process.env.APP_ENV ??
    process.env.NODE_ENV ??
    "local"
  ).trim() || "local";
  const discoveryRequired = parseOptionalBoolean(
    options.discoveryRequired ?? process.env.DISCOVERY_REQUIRED
  );
  const registryEnabled = parseOptionalBoolean(
    options.registryEnabled ?? process.env.REGISTRY_ENABLED
  );
  const fixturePath = options.fixturePath
    ? path.resolve(String(options.fixturePath))
    : "";

  return {
    rootDir: path.resolve(options.rootDir ?? DEFAULT_ROOT_DIR),
    environmentName,
    strict: isStrictEnvironment(environmentName) || discoveryRequired === true,
    discoveryRequired: isStrictEnvironment(environmentName) ? true : discoveryRequired === true,
    registryEnabled: registryEnabled ?? true,
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
    fixturePath,
    checks: options.checks ?? DEPLOYMENT_DISCOVERY_CHECKS,
    generatedAt: options.generatedAt ?? new Date().toISOString()
  };
}

function isStrictEnvironment(environmentName) {
  return STRICT_ENV_NAMES.has(String(environmentName ?? "").trim().toLowerCase());
}

async function readFixtureSnapshot(config) {
  const payload = JSON.parse(fs.readFileSync(config.fixturePath, "utf8"));
  const services = {};
  const errors = [];

  for (const serviceName of targetServiceNames(config.checks)) {
    services[serviceName] = normalizeFixtureInstances(payload, serviceName, errors);
  }

  return snapshotResult("fixture", services, errors);
}

async function readRedisSnapshot(config) {
  const { default: Redis } = await import("ioredis");
  const redis = new Redis(config.registryUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableOfflineQueue: false
  });
  const services = {};
  const errors = [];

  try {
    await redis.connect();
    for (const serviceName of targetServiceNames(config.checks)) {
      services[serviceName] = await readRedisServiceInstances(redis, config.registryKeyPrefix, serviceName, errors);
    }
  } finally {
    redis.disconnect();
  }

  return snapshotResult("registry", services, errors);
}

async function readRedisServiceInstances(redis, registryKeyPrefix, serviceName, errors) {
  const keys = await scanKeys(redis, registryInstanceScanPattern(registryKeyPrefix, serviceName));
  const instances = [];

  for (const key of keys.sort()) {
    const instanceId = key.split(":").at(-1);
    if (!instanceId) {
      continue;
    }

    const heartbeatExists = await redis.exists(registryHeartbeatKey(registryKeyPrefix, serviceName, instanceId));
    if (!heartbeatExists) {
      continue;
    }

    const data = await redis.hget(key, "data");
    if (!data) {
      continue;
    }

    try {
      const normalized = normalizeServiceInstance(JSON.parse(data));
      if (normalized && normalized.name === serviceName) {
        instances.push(normalized);
      } else {
        errors.push(errorObject(
          "invalid_registry_payload",
          `invalid registry payload for ${serviceName} instance ${instanceId}`,
          { service: serviceName, instanceId, key }
        ));
      }
    } catch (error) {
      errors.push(errorObject(
        "invalid_registry_json",
        `invalid registry JSON for ${serviceName} instance ${instanceId}: ${error?.message || error}`,
        { service: serviceName, instanceId, key }
      ));
    }
  }

  return instances.sort(compareInstances);
}

async function scanKeys(redis, pattern) {
  const keys = [];
  let cursor = "0";

  do {
    const [nextCursor, batch] = await redis.scan(cursor, "MATCH", pattern, "COUNT", 100);
    cursor = nextCursor;
    keys.push(...batch);
  } while (cursor !== "0");

  return keys;
}

function normalizeFixtureInstances(payload, serviceName, errors) {
  const rawInstances = fixtureInstanceCandidates(payload, serviceName);
  const byId = new Map();

  for (const raw of rawInstances) {
    const normalized = normalizeServiceInstance(raw);
    if (!normalized) {
      errors.push(errorObject(
        "invalid_fixture_payload",
        `invalid fixture payload for ${serviceName}`,
        { service: serviceName }
      ));
      continue;
    }
    if (normalized.name !== serviceName) {
      continue;
    }
    byId.set(normalized.id, normalized);
  }

  return [...byId.values()].sort(compareInstances);
}

function fixtureInstanceCandidates(payload, serviceName) {
  const candidates = [];
  if (Array.isArray(payload)) {
    candidates.push(...payload);
  }
  if (Array.isArray(payload?.instances)) {
    candidates.push(...payload.instances);
  }
  if (Array.isArray(payload?.services)) {
    candidates.push(...payload.services);
  }
  if (Array.isArray(payload?.registry)) {
    candidates.push(...payload.registry);
  }
  if (Array.isArray(payload?.instances?.[serviceName])) {
    candidates.push(...payload.instances[serviceName]);
  }
  if (Array.isArray(payload?.services?.[serviceName])) {
    candidates.push(...payload.services[serviceName]);
  }
  return candidates.filter((item) => item?.name === serviceName || item?.name === undefined);
}

function snapshotResult(source, services, errors) {
  return {
    source,
    services,
    serviceCounts: Object.fromEntries(
      Object.entries(services)
        .sort(([left], [right]) => left.localeCompare(right))
        .map(([serviceName, instances]) => [serviceName, instances.length])
    ),
    errors
  };
}

function evaluateCheck(definition, services, source) {
  const instances = services[definition.targetService] ?? [];
  const healthyInstances = instances.filter(isDiscoverableInstance);
  const namedCandidates = collectEndpointCandidates(healthyInstances, definition.endpointName);
  const protocolCandidates = namedCandidates.filter(({ endpoint }) =>
    definition.protocols.includes(endpoint.protocol)
  );
  const resolvedCandidates = protocolCandidates.filter(({ endpoint }) =>
    definition.visibilities.includes(endpoint.visibility)
  );
  const resolvedEndpoints = resolvedCandidates.map(({ instance, endpoint }) =>
    endpointReport(instance, endpoint, source)
  );
  const errors = [];

  if (instances.length === 0) {
    errors.push(errorObject(
      "no_registry_instances",
      `${definition.targetService} has no registry instances`
    ));
  } else if (healthyInstances.length === 0) {
    errors.push(errorObject(
      "no_healthy_instance",
      `${definition.targetService} has no healthy weighted registry instances`
    ));
  } else if (namedCandidates.length === 0) {
    errors.push(errorObject(
      "endpoint_missing",
      `${definition.targetService}.${definition.endpointName} endpoint not found`
    ));
  } else if (protocolCandidates.length === 0) {
    errors.push(errorObject(
      "protocol_mismatch",
      `${definition.targetService}.${definition.endpointName} expected protocol ${definition.protocols.join("|")}, found ${uniqueSorted(namedCandidates.map(({ endpoint }) => endpoint.protocol)).join("|") || "<none>"}`
    ));
  } else if (resolvedCandidates.length === 0) {
    errors.push(errorObject(
      "visibility_mismatch",
      `${definition.targetService}.${definition.endpointName} expected visibility ${definition.visibilities.join("|")}, found ${uniqueSorted(protocolCandidates.map(({ endpoint }) => endpoint.visibility)).join("|") || "<none>"}`
    ));
  }

  if (resolvedEndpoints.length === 0) {
    errors.push(errorObject(
      "fallback_forbidden",
      `${definition.consumer} must resolve ${definition.targetService}.${definition.endpointName} from registry data; fallback is forbidden for deployment dry-run`
    ));
  }

  return {
    ok: errors.length === 0,
    kind: definition.kind,
    consumer: definition.consumer,
    targetService: definition.targetService,
    endpointName: definition.endpointName,
    target: `${definition.targetService}.${definition.endpointName}`,
    expected: {
      protocols: [...definition.protocols],
      visibilities: [...definition.visibilities]
    },
    expectedProtocol: definition.protocols.length === 1 ? definition.protocols[0] : [...definition.protocols],
    expectedVisibility: definition.visibilities.length === 1 ? definition.visibilities[0] : [...definition.visibilities],
    source,
    fallback: false,
    resolvedEndpoints,
    errors
  };
}

function collectEndpointCandidates(instances, endpointName) {
  return instances.flatMap((instance) =>
    instance.endpoints
      .filter((endpoint) => endpoint.name === endpointName && endpoint.healthy !== false)
      .map((endpoint) => ({ instance, endpoint }))
  ).sort((left, right) =>
    left.instance.id.localeCompare(right.instance.id) ||
    left.endpoint.name.localeCompare(right.endpoint.name)
  );
}

function endpointReport(instance, endpoint, source) {
  const address = endpoint.protocol === "local_socket"
    ? endpoint.socket
    : `${endpoint.host}:${endpoint.port}`;
  return {
    service: instance.name,
    instanceId: instance.id,
    instance_id: instance.id,
    endpointName: endpoint.name,
    endpoint_name: endpoint.name,
    protocol: endpoint.protocol,
    visibility: endpoint.visibility,
    host: endpoint.host,
    port: endpoint.port,
    socket: endpoint.socket,
    address,
    url: endpoint.protocol === "http" ? `http://${endpoint.host}:${endpoint.port}` : "",
    healthy: instance.healthy !== false && endpoint.healthy !== false,
    weight: instance.weight,
    metadata: endpoint.metadata || {},
    source,
    fallback: false,
    reason: "discovered"
  };
}

function isDiscoverableInstance(instance) {
  return instance?.healthy !== false && Number(instance?.weight ?? 0) > 0;
}

function compareInstances(left, right) {
  return left.id.localeCompare(right.id);
}

function buildReport(config, snapshot, checks, errors) {
  const failedChecks = checks.filter((check) => !check.ok).length;
  const resolvedEndpointCount = checks.reduce((total, check) => total + check.resolvedEndpoints.length, 0);
  const fallbackUsed = checks.some((check) => check.fallback || check.resolvedEndpoints.some((endpoint) => endpoint.fallback));
  return {
    ok: errors.length === 0 && failedChecks === 0 && !fallbackUsed,
    generatedAt: config.generatedAt,
    environment: {
      name: config.environmentName,
      strict: config.strict,
      discoveryRequired: config.discoveryRequired,
      registryEnabled: config.registryEnabled
    },
    source: snapshot.source,
    registryUrl: config.registryUrl,
    registryKeyPrefix: config.registryKeyPrefix,
    fixturePath: config.fixturePath || "",
    summary: {
      checks: checks.length,
      passed: checks.length - failedChecks,
      failed: failedChecks,
      resolvedEndpoints: resolvedEndpointCount,
      fallbackUsed,
      services: Object.keys(snapshot.serviceCounts ?? {}).length,
      snapshotErrors: snapshot.errors?.length ?? 0
    },
    serviceCounts: snapshot.serviceCounts ?? {},
    checks,
    errors
  };
}

function targetServiceNames(checks) {
  return uniqueSorted(checks.map((check) => check.targetService));
}

function errorObject(code, message, extra = {}) {
  return { code, message, ...extra };
}

function uniqueSorted(values) {
  return [...new Set(values.filter((value) => value !== undefined && value !== null).map(String))].sort();
}

function parseOptionalBoolean(value) {
  if (value === undefined || value === null || value === "") {
    return null;
  }
  if (typeof value === "boolean") {
    return value;
  }
  if (typeof value === "number") {
    return value !== 0;
  }
  const text = String(value).trim();
  if (/^(1|true|yes|on)$/i.test(text)) {
    return true;
  }
  if (/^(0|false|no|off)$/i.test(text)) {
    return false;
  }
  throw new Error(`invalid boolean value: ${value}`);
}

function parseArgs(argv) {
  const args = {
    pretty: true
  };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--fixture") {
      args.fixturePath = path.resolve(argv[index + 1]);
      index += 1;
    } else if (arg === "--registry-url") {
      args.registryUrl = argv[index + 1];
      index += 1;
    } else if (arg === "--registry-key-prefix") {
      args.registryKeyPrefix = argv[index + 1];
      index += 1;
    } else if (arg === "--environment") {
      args.environment = argv[index + 1];
      index += 1;
    } else if (arg === "--registry-enabled") {
      args.registryEnabled = parseOptionalBoolean(argv[index + 1]);
      index += 1;
    } else if (arg === "--discovery-required") {
      args.discoveryRequired = parseOptionalBoolean(argv[index + 1]);
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
    "Usage: node tools/check-deployment-discovery-dry-run.js [options]",
    "",
    "Options:",
    "  --fixture <file>              Read a registry snapshot fixture instead of Redis",
    "  --registry-url <url>          Redis registry URL (default: REGISTRY_URL, REDIS_URL, or redis://127.0.0.1:6379)",
    "  --registry-key-prefix <value> Redis registry key prefix",
    "  --environment <name>          Environment name used for strict-mode reporting",
    "  --registry-enabled <bool>     Override REGISTRY_ENABLED for the report gate",
    "  --discovery-required <bool>   Override DISCOVERY_REQUIRED for the report gate",
    "  --compact                     Emit compact JSON"
  ].join("\n"));
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    printHelp();
    return;
  }

  const result = await checkDeploymentDiscoveryDryRun({
    rootDir: DEFAULT_ROOT_DIR,
    fixturePath: args.fixturePath,
    registryUrl: args.registryUrl,
    registryKeyPrefix: args.registryKeyPrefix,
    environment: args.environment,
    registryEnabled: args.registryEnabled,
    discoveryRequired: args.discoveryRequired
  });

  console.log(JSON.stringify(result, null, args.pretty ? 2 : 0));
  if (!result.ok) {
    process.exitCode = 1;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    console.error(error?.stack || error?.message || String(error));
    process.exitCode = 1;
  });
}
