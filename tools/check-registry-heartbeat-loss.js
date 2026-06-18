import process from "node:process";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import path from "node:path";

import { discoverGameServerAdminEndpoints as discoverAdminApiGameServerAdminEndpoints } from "../apps/admin-api/src/registry-client.js";
import { discoverGameServerAdminEndpoints as discoverMailServiceGameServerAdminEndpoints } from "../apps/mail-service/src/registry-client.js";
import {
  RegistryDiscoveryClient,
  createServiceInstancePayload,
  getDiscoveryMetricsSnapshot,
  registryHeartbeatKey,
  registryInstanceKey,
  resetDiscoveryMetrics
} from "../packages/service-registry/node/registry-schema.js";
import { MemoryRedis } from "./check-registry-canary-lifecycle.js";

const DEFAULT_START_TIME_MS = 1_713_000_000_000;
const DEFAULT_HEARTBEAT_TTL_SECONDS = 1;
const REPLACEMENT_HEARTBEAT_TTL_SECONDS = 30;
const TARGET_SERVICE_NAME = "game-server";
const TARGET_ENDPOINT_NAME = "admin";

export async function runRegistryHeartbeatLossCheck(options = {}) {
  const config = normalizeOptions(options);
  const redis = options.redis ?? new MemoryRedis({ now: () => config.startTimeMs });
  const report = createEmptyReport(config);
  const errors = [];

  resetDiscoveryMetrics();

  try {
    await cleanup(redis, config);
    report.missingHeartbeat = await runMissingHeartbeatCheck(redis, config);
    await cleanup(redis, config);
    report.expirySwitch = await runExpirySwitchCheck(redis, config);
  } catch (error) {
    errors.push(errorObject("heartbeat_loss_check_failed", error?.message || String(error)));
  } finally {
    if (config.cleanup) {
      await cleanup(redis, config).catch((error) => {
        errors.push(errorObject("cleanup_failed", error?.message || String(error)));
      });
    }
    report.metrics = getDiscoveryMetricsSnapshot();
    resetDiscoveryMetrics();
  }

  report.errors = [
    ...errors,
    ...sectionErrors("missing_heartbeat", report.missingHeartbeat),
    ...sectionErrors("expiry_switch", report.expirySwitch)
  ];
  report.ok =
    report.errors.length === 0 &&
    report.missingHeartbeat?.ok === true &&
    report.expirySwitch?.ok === true;
  return report;
}

function normalizeOptions(options) {
  const checkId = String(options.checkId ?? `registry-heartbeat-${randomUUID().slice(0, 8)}`);
  const heartbeatTtlSeconds = positiveNumberOption(
    options.heartbeatTtlSeconds,
    DEFAULT_HEARTBEAT_TTL_SECONDS
  );
  return {
    checkId,
    mode: "memory",
    generatedAt: options.generatedAt ?? new Date().toISOString(),
    registryKeyPrefix: String(
      options.registryKeyPrefix ??
      process.env.REGISTRY_HEARTBEAT_LOSS_KEY_PREFIX ??
      `heartbeat-loss:${checkId}:`
    ),
    heartbeatTtlSeconds,
    expiryAdvanceMs: numberOption(
      options.expiryAdvanceMs,
      heartbeatTtlSeconds * 1000 + 1
    ),
    startTimeMs: numberOption(options.startTimeMs, DEFAULT_START_TIME_MS),
    cleanup: options.cleanup !== false
  };
}

function createEmptyReport(config) {
  return {
    ok: false,
    generatedAt: config.generatedAt,
    mode: config.mode,
    registryKeyPrefix: config.registryKeyPrefix,
    checkId: config.checkId,
    target: {
      service: TARGET_SERVICE_NAME,
      endpoint: TARGET_ENDPOINT_NAME
    },
    heartbeat: {
      ttlSeconds: config.heartbeatTtlSeconds,
      expiryAdvanceMs: config.expiryAdvanceMs
    },
    missingHeartbeat: null,
    expirySwitch: null,
    metrics: [],
    errors: []
  };
}

async function runMissingHeartbeatCheck(redis, config) {
  const healthyId = `${config.checkId}-healthy`;
  const missingHeartbeatId = `${config.checkId}-missing-heartbeat`;
  const expectedVisibleIds = [healthyId];
  const forbiddenIds = [missingHeartbeatId];
  const errors = [];

  await writeInstance(redis, config, gameServerPayload(config, healthyId, {
    host: "127.0.30.10",
    clientPort: 17000,
    adminPort: 17500,
    weight: 100
  }), REPLACEMENT_HEARTBEAT_TTL_SECONDS);
  await writeInstance(redis, config, gameServerPayload(config, missingHeartbeatId, {
    host: "127.0.30.11",
    clientPort: 17001,
    adminPort: 17501,
    weight: 100
  }), 0);

  const registryStates = await readRegistryStates(redis, config, [healthyId, missingHeartbeatId]);
  expectState(errors, registryStates, healthyId, true, true);
  expectState(errors, registryStates, missingHeartbeatId, true, false);

  const consumers = await discoverConsumers(redis, config);
  validateConsumer(errors, "RegistryDiscoveryClient.instances", consumers.registryDiscoveryClient.instanceIds, expectedVisibleIds, forbiddenIds);
  validateConsumer(errors, "RegistryDiscoveryClient.allEndpoints", consumers.registryDiscoveryClient.allEndpointInstanceIds, expectedVisibleIds, forbiddenIds);
  validateConsumer(errors, "admin-api.gameServerAdminEndpoints", consumers.adminApi.instanceIds, expectedVisibleIds, forbiddenIds);
  validateConsumer(errors, "mail-service.gameServerAdminEndpoints", consumers.mailService.instanceIds, expectedVisibleIds, forbiddenIds);
  validateNoFallback(errors, consumers);

  return {
    ok: errors.length === 0,
    mode: "record_without_heartbeat",
    expectedVisibleInstanceIds: expectedVisibleIds,
    expectedFilteredInstanceIds: forbiddenIds,
    registryStates,
    consumers,
    fallbackUsed: consumerFallbackUsed(consumers),
    errors
  };
}

async function runExpirySwitchCheck(redis, config) {
  const expiringId = `${config.checkId}-expiring`;
  const replacementId = `${config.checkId}-replacement`;
  const errors = [];

  await writeInstance(redis, config, gameServerPayload(config, expiringId, {
    host: "127.0.31.10",
    clientPort: 17100,
    adminPort: 17600,
    weight: 100
  }), config.heartbeatTtlSeconds);

  const beforeConsumers = await discoverConsumers(redis, config);
  const beforeStates = await readRegistryStates(redis, config, [expiringId, replacementId]);
  expectState(errors, beforeStates, expiringId, true, true);
  validateSelectedEndpoint(errors, "before expiry", beforeConsumers.registryDiscoveryClient.requiredEndpoint, expiringId);
  validateConsumer(errors, "before expiry RegistryDiscoveryClient.allEndpoints", beforeConsumers.registryDiscoveryClient.allEndpointInstanceIds, [expiringId], [replacementId]);

  await writeInstance(redis, config, gameServerPayload(config, replacementId, {
    host: "127.0.31.20",
    clientPort: 17101,
    adminPort: 17601,
    weight: 100
  }), REPLACEMENT_HEARTBEAT_TTL_SECONDS);

  if (typeof redis.advanceTime !== "function") {
    errors.push(errorObject("redis_clock_missing", "memory Redis must support advanceTime for heartbeat expiry simulation"));
  } else {
    redis.advanceTime(config.expiryAdvanceMs);
  }

  const afterConsumers = await discoverConsumers(redis, config);
  const afterStates = await readRegistryStates(redis, config, [expiringId, replacementId]);
  expectState(errors, afterStates, expiringId, true, false);
  expectState(errors, afterStates, replacementId, true, true);
  validateSelectedEndpoint(errors, "after expiry", afterConsumers.registryDiscoveryClient.requiredEndpoint, replacementId);
  validateConsumer(errors, "after expiry RegistryDiscoveryClient.instances", afterConsumers.registryDiscoveryClient.instanceIds, [replacementId], [expiringId]);
  validateConsumer(errors, "after expiry RegistryDiscoveryClient.allEndpoints", afterConsumers.registryDiscoveryClient.allEndpointInstanceIds, [replacementId], [expiringId]);
  validateConsumer(errors, "after expiry admin-api.gameServerAdminEndpoints", afterConsumers.adminApi.instanceIds, [replacementId], [expiringId]);
  validateConsumer(errors, "after expiry mail-service.gameServerAdminEndpoints", afterConsumers.mailService.instanceIds, [replacementId], [expiringId]);
  validateNoFallback(errors, afterConsumers);

  return {
    ok: errors.length === 0,
    mode: "heartbeat_expiry_with_replacement",
    expectedInitialInstanceId: expiringId,
    expectedReplacementInstanceId: replacementId,
    beforeExpiry: {
      registryStates: beforeStates,
      consumers: beforeConsumers,
      selectedInstanceId: beforeConsumers.registryDiscoveryClient.requiredEndpoint?.instanceId ?? null
    },
    afterExpiry: {
      registryStates: afterStates,
      consumers: afterConsumers,
      selectedInstanceId: afterConsumers.registryDiscoveryClient.requiredEndpoint?.instanceId ?? null
    },
    fallbackUsed: consumerFallbackUsed(beforeConsumers) || consumerFallbackUsed(afterConsumers),
    errors
  };
}

async function discoverConsumers(redis, config) {
  const options = {
    registryKeyPrefix: config.registryKeyPrefix,
    discoveryCacheTtlMs: 0
  };
  const client = new RegistryDiscoveryClient(redis, options);
  const instances = await client.discoverInstances(TARGET_SERVICE_NAME);
  const allEndpoints = await client.discoverAllEndpoints(TARGET_SERVICE_NAME, TARGET_ENDPOINT_NAME);
  const requiredEndpoint = await client.discoverEndpoint(TARGET_SERVICE_NAME, TARGET_ENDPOINT_NAME);
  const adminApiEndpoints = await discoverAdminApiGameServerAdminEndpoints(redis, options);
  const mailServiceEndpoints = await discoverMailServiceGameServerAdminEndpoints(redis, options);

  return {
    registryDiscoveryClient: {
      consumer: "RegistryDiscoveryClient",
      source: "registry",
      fallbackUsed: false,
      instanceIds: instances.map((instance) => instance.id),
      allEndpointInstanceIds: allEndpoints.map(({ instance }) => instance.id),
      requiredEndpoint: requiredEndpoint ? discoverySelectionSummary(requiredEndpoint) : null,
      endpoints: allEndpoints.map(discoverySelectionSummary)
    },
    adminApi: {
      consumer: "admin-api.registry-client",
      source: "registry",
      fallbackUsed: flatEndpointFallbackUsed(adminApiEndpoints),
      instanceIds: adminApiEndpoints.map((endpoint) => endpoint.instanceId),
      endpoints: adminApiEndpoints.map(flatEndpointSummary)
    },
    mailService: {
      consumer: "mail-service.registry-client",
      source: "registry",
      fallbackUsed: flatEndpointFallbackUsed(mailServiceEndpoints),
      instanceIds: mailServiceEndpoints.map((endpoint) => endpoint.instanceId),
      endpoints: mailServiceEndpoints.map(flatEndpointSummary)
    }
  };
}

async function writeInstance(redis, config, payload, heartbeatTtlSeconds) {
  await redis.hset(
    registryInstanceKey(config.registryKeyPrefix, payload.name, payload.id),
    "data",
    JSON.stringify(payload)
  );
  if (heartbeatTtlSeconds > 0) {
    await redis.setex(
      registryHeartbeatKey(config.registryKeyPrefix, payload.name, payload.id),
      heartbeatTtlSeconds,
      "1"
    );
  }
}

function gameServerPayload(config, instanceId, { host, clientPort, adminPort, weight }) {
  const metadata = {
    service_name: TARGET_SERVICE_NAME,
    service_instance_id: instanceId,
    instance_id: instanceId,
    build_version: "registry-heartbeat-loss",
    zone: "heartbeat-loss-drill"
  };

  return createServiceInstancePayload({
    id: instanceId,
    name: TARGET_SERVICE_NAME,
    host,
    port: clientPort,
    admin_port: adminPort,
    endpoints: [
      {
        name: "client",
        protocol: "tcp",
        host,
        port: clientPort,
        socket: "",
        visibility: "internal",
        metadata,
        healthy: true
      },
      {
        name: TARGET_ENDPOINT_NAME,
        protocol: "tcp",
        host,
        port: adminPort,
        socket: "",
        visibility: "admin",
        metadata,
        healthy: true
      }
    ],
    tags: ["heartbeat-loss", "game", "admin"],
    weight,
    metadata
  });
}

async function readRegistryStates(redis, config, instanceIds) {
  const states = [];
  for (const instanceId of instanceIds) {
    const instanceKey = registryInstanceKey(config.registryKeyPrefix, TARGET_SERVICE_NAME, instanceId);
    const heartbeatKey = registryHeartbeatKey(config.registryKeyPrefix, TARGET_SERVICE_NAME, instanceId);
    states.push({
      service: TARGET_SERVICE_NAME,
      instanceId,
      instanceKey,
      heartbeatKey,
      instanceRecordExists: await redis.exists(instanceKey) === 1,
      heartbeatExists: await redis.exists(heartbeatKey) === 1
    });
  }
  return states;
}

async function cleanup(redis, config) {
  const keys = trackedInstanceIds(config).flatMap((instanceId) => [
    registryInstanceKey(config.registryKeyPrefix, TARGET_SERVICE_NAME, instanceId),
    registryHeartbeatKey(config.registryKeyPrefix, TARGET_SERVICE_NAME, instanceId)
  ]);
  await redis.del(keys);
}

function trackedInstanceIds(config) {
  return [
    `${config.checkId}-healthy`,
    `${config.checkId}-missing-heartbeat`,
    `${config.checkId}-expiring`,
    `${config.checkId}-replacement`
  ];
}

function expectState(errors, states, instanceId, expectedRecordExists, expectedHeartbeatExists) {
  const state = states.find((candidate) => candidate.instanceId === instanceId);
  if (!state) {
    errors.push(errorObject("registry_state_missing", `missing registry state for ${instanceId}`, { instanceId }));
    return;
  }
  if (state.instanceRecordExists !== expectedRecordExists) {
    errors.push(errorObject(
      expectedRecordExists ? "instance_record_missing" : "instance_record_unexpected",
      `unexpected instance record state for ${instanceId}`,
      { instanceId, expected: expectedRecordExists, actual: state.instanceRecordExists }
    ));
  }
  if (state.heartbeatExists !== expectedHeartbeatExists) {
    errors.push(errorObject(
      expectedHeartbeatExists ? "heartbeat_missing" : "heartbeat_still_present",
      `unexpected heartbeat state for ${instanceId}`,
      { instanceId, expected: expectedHeartbeatExists, actual: state.heartbeatExists }
    ));
  }
}

function validateConsumer(errors, label, actualIds, expectedIds, forbiddenIds) {
  const actual = [...actualIds].sort();
  const expected = [...expectedIds].sort();
  if (!sameArray(actual, expected)) {
    errors.push(errorObject(
      "consumer_visible_instances_mismatch",
      `${label} visible instances mismatch`,
      { label, expected, actual }
    ));
  }

  for (const forbiddenId of forbiddenIds) {
    if (actual.includes(forbiddenId)) {
      errors.push(errorObject(
        "consumer_returned_heartbeatless_instance",
        `${label} returned heartbeatless instance ${forbiddenId}`,
        { label, forbiddenInstanceId: forbiddenId, actual }
      ));
    }
  }
}

function validateSelectedEndpoint(errors, label, endpoint, expectedInstanceId) {
  if (!endpoint) {
    errors.push(errorObject(
      "endpoint_missing",
      `${label} did not resolve ${TARGET_SERVICE_NAME}.${TARGET_ENDPOINT_NAME}`,
      { label, expectedInstanceId }
    ));
    return;
  }
  if (endpoint.instanceId !== expectedInstanceId) {
    errors.push(errorObject(
      "selected_endpoint_mismatch",
      `${label} selected ${endpoint.instanceId}, expected ${expectedInstanceId}`,
      { label, expectedInstanceId, actualInstanceId: endpoint.instanceId }
    ));
  }
}

function validateNoFallback(errors, consumers) {
  for (const consumer of Object.values(consumers)) {
    if (consumer.fallbackUsed) {
      errors.push(errorObject(
        "fallback_used",
        `${consumer.consumer} used fallback discovery`,
        { consumer: consumer.consumer }
      ));
    }
  }
}

function discoverySelectionSummary(selection) {
  return {
    service: selection.instance.name,
    instanceId: selection.instance.id,
    endpointName: selection.endpoint.name,
    protocol: selection.endpoint.protocol,
    host: selection.endpoint.host,
    port: selection.endpoint.port,
    socket: selection.endpoint.socket,
    visibility: selection.endpoint.visibility,
    source: "registry",
    fallback: false
  };
}

function flatEndpointSummary(endpoint) {
  return {
    service: endpoint.service,
    instanceId: endpoint.instanceId,
    endpointName: endpoint.endpointName,
    protocol: endpoint.protocol,
    host: endpoint.host,
    port: endpoint.port,
    visibility: "admin",
    source: endpoint.source || "registry",
    fallback: endpoint.fallback === true
  };
}

function flatEndpointFallbackUsed(endpoints) {
  return endpoints.some((endpoint) => endpoint.fallback === true || endpoint.source === "fallback");
}

function consumerFallbackUsed(consumers) {
  return Object.values(consumers).some((consumer) => consumer.fallbackUsed === true);
}

function sectionErrors(section, value) {
  return (value?.errors || []).map((error) => ({ section, ...error }));
}

function sameArray(left, right) {
  return left.length === right.length && left.every((value, index) => value === right[index]);
}

function errorObject(code, message, extra = {}) {
  return { code, message, ...extra };
}

function numberOption(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
}

function positiveNumberOption(value, fallback) {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function parseArgs(argv) {
  const args = { pretty: true };

  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === "--memory") {
      args.mode = "memory";
    } else if (arg === "--registry-key-prefix") {
      args.registryKeyPrefix = argv[index + 1] ?? "";
      index += 1;
    } else if (arg === "--check-id") {
      args.checkId = argv[index + 1];
      index += 1;
    } else if (arg === "--heartbeat-ttl-seconds") {
      args.heartbeatTtlSeconds = Number(argv[index + 1]);
      index += 1;
    } else if (arg === "--expiry-advance-ms") {
      args.expiryAdvanceMs = Number(argv[index + 1]);
      index += 1;
    } else if (arg === "--start-time-ms") {
      args.startTimeMs = Number(argv[index + 1]);
      index += 1;
    } else if (arg === "--no-cleanup") {
      args.cleanup = false;
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
    "Usage: node tools/check-registry-heartbeat-loss.js [options]",
    "",
    "Runs a pre-release registry heartbeat-loss gate using memory Redis:",
    "  keep stale instance records -> remove or expire heartbeat keys -> verify consumers only return live instances.",
    "",
    "Options:",
    "  --memory                       Use the built-in memory Redis simulation (default)",
    "  --registry-key-prefix <value>  Registry key prefix for drill keys",
    "  --check-id <value>             Stable drill instance id prefix",
    "  --heartbeat-ttl-seconds <n>    TTL for the expiring heartbeat",
    "  --expiry-advance-ms <ms>       Simulated clock advance before the expiry assertion",
    "  --no-cleanup                   Leave drill keys in memory for inspection in embedded runs",
    "  --compact                      Emit compact JSON"
  ].join("\n"));
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  if (args.help) {
    printHelp();
    return;
  }

  const report = await runRegistryHeartbeatLossCheck(args);
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
