import process from "node:process";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import path from "node:path";

import {
  createRegistryDiscoveryClient as createAdminApiRegistryDiscoveryClient,
  discoverGameProxyAdminEndpoints as discoverAdminApiGameProxyAdminEndpoints,
  discoverGameServerAdminEndpoints as discoverAdminApiGameServerAdminEndpoints
} from "../apps/admin-api/src/registry-client.js";
import {
  RegistryDiscoveryClient,
  createServiceInstancePayload,
  getDiscoveryMetricsSnapshot,
  normalizeServiceInstance,
  registryHeartbeatKey,
  registryInstanceKey,
  resetDiscoveryMetrics
} from "../packages/service-registry/node/registry-schema.js";
import { MemoryRedis } from "./check-registry-canary-lifecycle.js";

const DEFAULT_START_TIME_MS = 1_713_000_000_000;
const HEARTBEAT_TTL_SECONDS = 30;

export async function runRegistrySchemaMixedRolloutCheck(options = {}) {
  const config = normalizeOptions(options);
  const redis = options.redis ?? new MemoryRedis({ now: () => config.startTimeMs });
  const report = createEmptyReport(config);
  const errors = [];
  const fixtures = createFixtures(config);

  resetDiscoveryMetrics();

  try {
    await cleanup(redis, config, fixtures);
    await seedRegistry(redis, config, fixtures);

    report.registryRecords = await readRegistryRecords(redis, config, fixtures);
    report.discovery = await runDiscoveryChecks(redis, config, fixtures);
    report.adminApiHelpers = await runAdminApiHelperChecks(redis, config, fixtures);
    report.adminApiRefreshSnapshots = await runAdminApiRefreshChecks(redis, config, fixtures);
  } catch (error) {
    errors.push(errorObject("schema_mixed_rollout_check_failed", error?.message || String(error)));
  } finally {
    if (config.cleanup) {
      await cleanup(redis, config, fixtures).catch((error) => {
        errors.push(errorObject("cleanup_failed", error?.message || String(error)));
      });
    }
    report.metrics = getDiscoveryMetricsSnapshot();
    resetDiscoveryMetrics();
  }

  report.errors = [
    ...errors,
    ...sectionErrors("registry_records", report.registryRecords),
    ...sectionErrors("discovery", report.discovery),
    ...sectionErrors("admin_api_helpers", report.adminApiHelpers),
    ...sectionErrors("admin_api_refresh_snapshots", report.adminApiRefreshSnapshots)
  ];
  report.fallbackUsed = [
    ...flattenChecks(report.discovery),
    ...flattenChecks(report.adminApiHelpers),
    ...flattenChecks(report.adminApiRefreshSnapshots)
  ].some((check) => check.fallbackUsed === true);
  report.ok =
    report.errors.length === 0 &&
    report.fallbackUsed === false &&
    report.discovery?.ok === true &&
    report.adminApiHelpers?.ok === true &&
    report.adminApiRefreshSnapshots?.ok === true;
  return report;
}

function normalizeOptions(options) {
  const checkId = String(options.checkId ?? `registry-schema-mixed-${randomUUID().slice(0, 8)}`);
  return {
    checkId,
    mode: "memory",
    generatedAt: options.generatedAt ?? new Date().toISOString(),
    registryKeyPrefix: String(
      options.registryKeyPrefix ??
      process.env.REGISTRY_SCHEMA_MIXED_KEY_PREFIX ??
      `schema-mixed:${checkId}:`
    ),
    cleanup: options.cleanup !== false,
    startTimeMs: numberOption(options.startTimeMs, DEFAULT_START_TIME_MS)
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
      services: ["game-server", "game-proxy"],
      endpoints: [
        "game-server.admin",
        "game-server.local_socket",
        "game-server.proxy-local",
        "game-proxy.client",
        "game-proxy.admin"
      ]
    },
    registryRecords: null,
    discovery: null,
    adminApiHelpers: null,
    adminApiRefreshSnapshots: null,
    fallbackUsed: false,
    metrics: [],
    errors: []
  };
}

function createFixtures(config) {
  const ids = {
    gameServerLegacy: `${config.checkId}-game-server-legacy-v1`,
    gameServerV2: `${config.checkId}-game-server-endpoint-v2`,
    gameProxyLegacy: `${config.checkId}-game-proxy-legacy-v1`,
    gameProxyV2: `${config.checkId}-game-proxy-endpoint-v2`
  };
  const endpoints = {
    gameServerLegacy: {
      client: networkEndpoint("client", "tcp", "127.0.40.10", 17000, "public"),
      admin: networkEndpoint("admin", "tcp", "127.0.40.10", 17500, "admin"),
      localSocket: socketEndpoint("local_socket", `${ids.gameServerLegacy}.sock`)
    },
    gameServerV2: {
      client: networkEndpoint("client", "tcp", "127.0.40.21", 17021, "internal"),
      admin: networkEndpoint("admin", "tcp", "127.0.40.22", 17522, "admin"),
      internal: socketEndpoint("internal", `${ids.gameServerV2}-internal.sock`),
      proxyLocal: socketEndpoint("proxy-local", `${ids.gameServerV2}-proxy.sock`)
    },
    gameProxyLegacy: {
      client: networkEndpoint("client", "kcp", "127.0.41.10", 14000, "public"),
      admin: networkEndpoint("admin", "http", "127.0.41.10", 17101, "admin")
    },
    gameProxyV2: {
      client: networkEndpoint("client", "kcp", "127.0.41.21", 14021, "public"),
      admin: networkEndpoint("admin", "http", "127.0.41.22", 17122, "admin")
    }
  };

  const payloads = [
    legacyPayload({
      id: ids.gameServerLegacy,
      name: "game-server",
      host: endpoints.gameServerLegacy.client.host,
      port: endpoints.gameServerLegacy.client.port,
      adminPort: endpoints.gameServerLegacy.admin.port,
      localSocket: endpoints.gameServerLegacy.localSocket.socket,
      metadata: metadata("game-server", ids.gameServerLegacy, "legacy-v1")
    }),
    createServiceInstancePayload({
      id: ids.gameServerV2,
      name: "game-server",
      host: "127.0.40.20",
      port: 17020,
      admin_port: 17520,
      local_socket: `${ids.gameServerV2}-legacy-unused.sock`,
      endpoints: [
        withMetadata(endpoints.gameServerV2.client, metadata("game-server", ids.gameServerV2, "endpoint-v2")),
        withMetadata(endpoints.gameServerV2.admin, metadata("game-server", ids.gameServerV2, "endpoint-v2")),
        withMetadata(endpoints.gameServerV2.internal, metadata("game-server", ids.gameServerV2, "endpoint-v2")),
        withMetadata(endpoints.gameServerV2.proxyLocal, metadata("game-server", ids.gameServerV2, "endpoint-v2"))
      ],
      tags: ["schema-mixed", "endpoint-v2", "game"],
      weight: 100,
      metadata: metadata("game-server", ids.gameServerV2, "endpoint-v2")
    }),
    legacyPayload({
      id: ids.gameProxyLegacy,
      name: "game-proxy",
      host: endpoints.gameProxyLegacy.client.host,
      port: endpoints.gameProxyLegacy.client.port,
      adminPort: endpoints.gameProxyLegacy.admin.port,
      localSocket: "",
      metadata: metadata("game-proxy", ids.gameProxyLegacy, "legacy-v1")
    }),
    createServiceInstancePayload({
      id: ids.gameProxyV2,
      name: "game-proxy",
      host: "127.0.41.20",
      port: 14020,
      admin_port: 17120,
      endpoints: [
        withMetadata(endpoints.gameProxyV2.client, metadata("game-proxy", ids.gameProxyV2, "endpoint-v2")),
        withMetadata(endpoints.gameProxyV2.admin, metadata("game-proxy", ids.gameProxyV2, "endpoint-v2"))
      ],
      tags: ["schema-mixed", "endpoint-v2", "proxy"],
      weight: 100,
      metadata: metadata("game-proxy", ids.gameProxyV2, "endpoint-v2")
    })
  ];

  return {
    ids,
    endpoints,
    payloads,
    instances: [
      { serviceName: "game-server", instanceId: ids.gameServerLegacy },
      { serviceName: "game-server", instanceId: ids.gameServerV2 },
      { serviceName: "game-proxy", instanceId: ids.gameProxyLegacy },
      { serviceName: "game-proxy", instanceId: ids.gameProxyV2 }
    ]
  };
}

function legacyPayload({ id, name, host, port, adminPort, localSocket, metadata: instanceMetadata }) {
  return {
    schema_version: 1,
    id,
    name,
    host,
    port,
    admin_port: adminPort,
    local_socket: localSocket,
    tags: ["schema-mixed", "legacy-v1"],
    weight: 100,
    metadata: instanceMetadata,
    registered_at: 1,
    healthy: true
  };
}

async function seedRegistry(redis, config, fixtures) {
  for (const payload of fixtures.payloads) {
    await redis.hset(
      registryInstanceKey(config.registryKeyPrefix, payload.name, payload.id),
      "data",
      JSON.stringify(payload)
    );
    await redis.setex(
      registryHeartbeatKey(config.registryKeyPrefix, payload.name, payload.id),
      HEARTBEAT_TTL_SECONDS,
      "1"
    );
  }
}

async function runDiscoveryChecks(redis, config, fixtures) {
  const client = new RegistryDiscoveryClient(redis, {
    registryKeyPrefix: config.registryKeyPrefix,
    discoveryCacheTtlMs: 0
  });
  const schemaByInstanceId = schemaMap(fixtures.payloads);
  const checks = [
    await endpointDiscoveryCheck({
      name: "RegistryDiscoveryClient game-server.admin",
      serviceName: "game-server",
      endpointName: "admin",
      discover: () => client.discoverAllEndpoints("game-server", "admin"),
      expected: [
        expectedSelection(fixtures.ids.gameServerLegacy, fixtures.endpoints.gameServerLegacy.admin),
        expectedSelection(fixtures.ids.gameServerV2, fixtures.endpoints.gameServerV2.admin)
      ],
      schemaByInstanceId
    }),
    await endpointDiscoveryCheck({
      name: "RegistryDiscoveryClient game-server.local_socket",
      serviceName: "game-server",
      endpointName: "local_socket",
      discover: () => client.discoverAllEndpoints("game-server", "local_socket"),
      expected: [
        expectedSelection(fixtures.ids.gameServerLegacy, fixtures.endpoints.gameServerLegacy.localSocket)
      ],
      schemaByInstanceId
    }),
    await endpointDiscoveryCheck({
      name: "RegistryDiscoveryClient game-server.proxy-local",
      serviceName: "game-server",
      endpointName: "proxy-local",
      discover: () => client.discoverAllEndpoints("game-server", "proxy-local"),
      expected: [
        expectedSelection(fixtures.ids.gameServerV2, fixtures.endpoints.gameServerV2.proxyLocal)
      ],
      schemaByInstanceId
    }),
    await endpointDiscoveryCheck({
      name: "RegistryDiscoveryClient game-proxy.client",
      serviceName: "game-proxy",
      endpointName: "client",
      discover: () => client.discoverAllEndpoints("game-proxy", "client"),
      expected: [
        expectedSelection(fixtures.ids.gameProxyLegacy, fixtures.endpoints.gameProxyLegacy.client),
        expectedSelection(fixtures.ids.gameProxyV2, fixtures.endpoints.gameProxyV2.client)
      ],
      schemaByInstanceId
    }),
    await endpointDiscoveryCheck({
      name: "RegistryDiscoveryClient game-proxy.admin",
      serviceName: "game-proxy",
      endpointName: "admin",
      discover: () => client.discoverAllEndpoints("game-proxy", "admin"),
      expected: [
        expectedSelection(fixtures.ids.gameProxyLegacy, fixtures.endpoints.gameProxyLegacy.admin),
        expectedSelection(fixtures.ids.gameProxyV2, fixtures.endpoints.gameProxyV2.admin)
      ],
      schemaByInstanceId
    })
  ];

  return checksReport(checks);
}

async function runAdminApiHelperChecks(redis, config, fixtures) {
  const schemaByInstanceId = schemaMap(fixtures.payloads);
  const options = {
    registryKeyPrefix: config.registryKeyPrefix,
    discoveryCacheTtlMs: 0
  };
  const checks = [
    await flatEndpointDiscoveryCheck({
      name: "admin-api helper game-server.admin",
      serviceName: "game-server",
      endpointName: "admin",
      discover: () => discoverAdminApiGameServerAdminEndpoints(redis, options),
      expected: [
        expectedSelection(fixtures.ids.gameServerLegacy, fixtures.endpoints.gameServerLegacy.admin),
        expectedSelection(fixtures.ids.gameServerV2, fixtures.endpoints.gameServerV2.admin)
      ],
      schemaByInstanceId
    }),
    await flatEndpointDiscoveryCheck({
      name: "admin-api helper game-proxy.admin",
      serviceName: "game-proxy",
      endpointName: "admin",
      discover: () => discoverAdminApiGameProxyAdminEndpoints(redis, options),
      expected: [
        expectedSelection(fixtures.ids.gameProxyLegacy, fixtures.endpoints.gameProxyLegacy.admin),
        expectedSelection(fixtures.ids.gameProxyV2, fixtures.endpoints.gameProxyV2.admin)
      ],
      schemaByInstanceId
    })
  ];

  return checksReport(checks);
}

async function runAdminApiRefreshChecks(redis, config, fixtures) {
  const schemaByInstanceId = schemaMap(fixtures.payloads);
  const client = createAdminApiRegistryDiscoveryClient(redis, {
    registryKeyPrefix: config.registryKeyPrefix,
    discoveryCacheTtlMs: 0
  });

  try {
    const checks = [
      await endpointDiscoveryCheck({
        name: "admin-api refresh snapshot game-server.admin",
        serviceName: "game-server",
        endpointName: "admin",
        discover: async () => {
          const snapshot = await client.refreshSnapshot("game-server", {
            endpointName: "admin",
            kind: "all_endpoints",
            refreshIntervalMs: 0
          });
          return snapshot.value || [];
        },
        expected: [
          expectedSelection(fixtures.ids.gameServerLegacy, fixtures.endpoints.gameServerLegacy.admin),
          expectedSelection(fixtures.ids.gameServerV2, fixtures.endpoints.gameServerV2.admin)
        ],
        schemaByInstanceId
      }),
      await endpointDiscoveryCheck({
        name: "admin-api refresh snapshot game-proxy.admin",
        serviceName: "game-proxy",
        endpointName: "admin",
        discover: async () => {
          const snapshot = await client.refreshSnapshot("game-proxy", {
            endpointName: "admin",
            kind: "all_endpoints",
            refreshIntervalMs: 0
          });
          return snapshot.value || [];
        },
        expected: [
          expectedSelection(fixtures.ids.gameProxyLegacy, fixtures.endpoints.gameProxyLegacy.admin),
          expectedSelection(fixtures.ids.gameProxyV2, fixtures.endpoints.gameProxyV2.admin)
        ],
        schemaByInstanceId
      })
    ];

    return checksReport(checks);
  } finally {
    client.stop();
  }
}

async function endpointDiscoveryCheck({
  name,
  serviceName,
  endpointName,
  discover,
  expected,
  schemaByInstanceId
}) {
  const errors = [];
  let selections = [];

  try {
    selections = await discover();
  } catch (error) {
    errors.push(errorObject("discovery_failed", error?.message || String(error)));
  }

  const results = selections.map(({ instance, endpoint }) =>
    selectionSummary(instance, endpoint, schemaByInstanceId)
  );
  validateExpectedResults(errors, name, results, expected);
  validateNoFallback(errors, name, results);

  return {
    ok: errors.length === 0,
    name,
    consumer: name.startsWith("admin-api") ? "admin-api.registry-client" : "RegistryDiscoveryClient",
    service: serviceName,
    endpointName,
    expected: expected.map((item) => expectedReport(item)),
    discoveredInstanceIds: results.map((result) => result.instanceId).sort(),
    fallbackUsed: results.some((result) => result.fallback === true || result.source === "fallback"),
    results,
    errors
  };
}

async function flatEndpointDiscoveryCheck({
  name,
  serviceName,
  endpointName,
  discover,
  expected,
  schemaByInstanceId
}) {
  const errors = [];
  let endpoints = [];

  try {
    endpoints = await discover();
  } catch (error) {
    errors.push(errorObject("discovery_failed", error?.message || String(error)));
  }

  const results = endpoints.map((endpoint) =>
    flatEndpointSummary(endpoint, schemaByInstanceId)
  );
  validateExpectedResults(errors, name, results, expected);
  validateNoFallback(errors, name, results);

  return {
    ok: errors.length === 0,
    name,
    consumer: "admin-api.registry-client",
    service: serviceName,
    endpointName,
    expected: expected.map((item) => expectedReport(item)),
    discoveredInstanceIds: results.map((result) => result.instanceId).sort(),
    fallbackUsed: results.some((result) => result.fallback === true || result.source === "fallback"),
    results,
    errors
  };
}

function validateExpectedResults(errors, label, results, expected) {
  const actualIds = results.map((result) => result.instanceId).sort();
  const expectedIds = expected.map((item) => item.instanceId).sort();
  if (!sameArray(actualIds, expectedIds)) {
    errors.push(errorObject(
      "discovered_instances_mismatch",
      `${label} discovered instances mismatch`,
      { expectedInstanceIds: expectedIds, actualInstanceIds: actualIds }
    ));
  }

  for (const expectedEndpoint of expected) {
    const actual = results.find((result) => result.instanceId === expectedEndpoint.instanceId);
    if (!actual) {
      errors.push(errorObject(
        "expected_instance_missing",
        `${label} missing ${expectedEndpoint.instanceId}`,
        { expected: expectedReport(expectedEndpoint) }
      ));
      continue;
    }

    for (const field of ["endpointName", "protocol", "host", "port", "socket", "visibility"]) {
      if (actual[field] !== expectedEndpoint[field]) {
        errors.push(errorObject(
          "endpoint_field_mismatch",
          `${label} ${expectedEndpoint.instanceId} ${field} mismatch`,
          {
            instanceId: expectedEndpoint.instanceId,
            field,
            expected: expectedEndpoint[field],
            actual: actual[field]
          }
        ));
      }
    }
  }
}

function validateNoFallback(errors, label, results) {
  for (const result of results) {
    if (result.fallback === true || result.source === "fallback") {
      errors.push(errorObject(
        "fallback_used",
        `${label} used fallback discovery`,
        { instanceId: result.instanceId, source: result.source }
      ));
    }
  }
}

async function readRegistryRecords(redis, config, fixtures) {
  const records = [];
  const errors = [];
  const schemaByInstanceId = schemaMap(fixtures.payloads);

  for (const spec of fixtures.instances) {
    const instanceKey = registryInstanceKey(config.registryKeyPrefix, spec.serviceName, spec.instanceId);
    const heartbeatKey = registryHeartbeatKey(config.registryKeyPrefix, spec.serviceName, spec.instanceId);
    const raw = await redis.hget(instanceKey, "data");
    const heartbeatExists = await redis.exists(heartbeatKey) === 1;
    let parsed = null;
    let normalized = null;
    try {
      parsed = raw ? JSON.parse(raw) : null;
      normalized = parsed ? normalizeServiceInstance(parsed) : null;
    } catch (error) {
      errors.push(errorObject(
        "invalid_registry_record_json",
        `invalid JSON for ${spec.serviceName}.${spec.instanceId}: ${error?.message || error}`,
        { service: spec.serviceName, instanceId: spec.instanceId }
      ));
    }

    if (!raw) {
      errors.push(errorObject(
        "instance_record_missing",
        `missing instance record for ${spec.serviceName}.${spec.instanceId}`,
        { service: spec.serviceName, instanceId: spec.instanceId }
      ));
    }
    if (!heartbeatExists) {
      errors.push(errorObject(
        "heartbeat_missing",
        `missing heartbeat for ${spec.serviceName}.${spec.instanceId}`,
        { service: spec.serviceName, instanceId: spec.instanceId }
      ));
    }
    if (!normalized) {
      errors.push(errorObject(
        "normalization_failed",
        `failed to normalize ${spec.serviceName}.${spec.instanceId}`,
        { service: spec.serviceName, instanceId: spec.instanceId }
      ));
    }

    records.push({
      service: spec.serviceName,
      instanceId: spec.instanceId,
      instanceKey,
      heartbeatKey,
      instanceRecordExists: Boolean(raw),
      heartbeatExists,
      schema: schemaByInstanceId.get(spec.instanceId) || schemaSummary(parsed),
      normalizedEndpoints: normalized?.endpoints.map(endpointReport) ?? []
    });
  }

  return {
    ok: errors.length === 0,
    records,
    errors
  };
}

async function cleanup(redis, config, fixtures) {
  const keys = fixtures.instances.flatMap((spec) => [
    registryInstanceKey(config.registryKeyPrefix, spec.serviceName, spec.instanceId),
    registryHeartbeatKey(config.registryKeyPrefix, spec.serviceName, spec.instanceId)
  ]);
  if (keys.length > 0) {
    await redis.del(keys);
  }
}

function checksReport(checks) {
  return {
    ok: checks.every((check) => check.ok),
    fallbackUsed: checks.some((check) => check.fallbackUsed === true),
    checks,
    errors: checks.flatMap((check) =>
      check.errors.map((error) => ({ ...error, check: check.name }))
    )
  };
}

function schemaMap(payloads) {
  return new Map(payloads.map((payload) => [payload.id, schemaSummary(payload)]));
}

function schemaSummary(payload) {
  const rawVersion = payload?.schema_version ?? 1;
  const explicitEndpoints = Array.isArray(payload?.endpoints);
  return {
    rawVersion,
    normalizedVersion: 2,
    source: Number(rawVersion) >= 2 && explicitEndpoints ? "endpoint-v2" : "legacy-v1",
    explicitEndpoints
  };
}

function selectionSummary(instance, endpoint, schemaByInstanceId) {
  return {
    service: instance.name,
    instanceId: instance.id,
    instance_id: instance.id,
    endpointName: endpoint.name,
    endpoint_name: endpoint.name,
    protocol: endpoint.protocol,
    host: endpoint.host,
    port: endpoint.port,
    socket: endpoint.socket,
    visibility: endpoint.visibility,
    healthy: instance.healthy !== false && endpoint.healthy !== false,
    weight: instance.weight,
    metadata: endpoint.metadata || {},
    schema: schemaByInstanceId.get(instance.id) || schemaSummary(instance),
    source: "registry",
    reason: "discovered",
    fallback: false
  };
}

function flatEndpointSummary(endpoint, schemaByInstanceId) {
  return {
    service: endpoint.service,
    instanceId: endpoint.instanceId,
    instance_id: endpoint.instance_id || endpoint.instanceId,
    endpointName: endpoint.endpointName,
    endpoint_name: endpoint.endpoint_name || endpoint.endpointName,
    protocol: endpoint.protocol,
    host: endpoint.host,
    port: endpoint.port,
    socket: "",
    visibility: "admin",
    healthy: endpoint.healthy,
    weight: endpoint.weight,
    metadata: endpoint.metadata || {},
    schema: schemaByInstanceId.get(endpoint.instanceId) || null,
    source: endpoint.source || "registry",
    reason: endpoint.reason || "discovered",
    fallback: endpoint.fallback === true
  };
}

function expectedSelection(instanceId, endpoint) {
  return {
    instanceId,
    endpointName: endpoint.name,
    protocol: endpoint.protocol,
    host: endpoint.host,
    port: endpoint.port,
    socket: endpoint.socket,
    visibility: endpoint.visibility
  };
}

function expectedReport(expected) {
  return { ...expected };
}

function endpointReport(endpoint) {
  return {
    name: endpoint.name,
    protocol: endpoint.protocol,
    host: endpoint.host,
    port: endpoint.port,
    socket: endpoint.socket,
    visibility: endpoint.visibility,
    healthy: endpoint.healthy
  };
}

function networkEndpoint(name, protocol, host, port, visibility) {
  return {
    name,
    protocol,
    host,
    port,
    socket: "",
    visibility,
    metadata: {},
    healthy: true
  };
}

function socketEndpoint(name, socket) {
  return {
    name,
    protocol: "local_socket",
    host: "",
    port: 0,
    socket,
    visibility: "local",
    metadata: {},
    healthy: true
  };
}

function withMetadata(endpoint, endpointMetadata) {
  return {
    ...endpoint,
    metadata: endpointMetadata
  };
}

function metadata(serviceName, instanceId, registryShape) {
  return {
    service_name: serviceName,
    service_instance_id: instanceId,
    instance_id: instanceId,
    build_version: "registry-schema-mixed-rollout",
    zone: "schema-mixed",
    registry_shape: registryShape
  };
}

function flattenChecks(section) {
  return section?.checks ?? [];
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
  if (value === null || value === undefined || value === "") {
    return fallback;
  }
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : fallback;
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
    "Usage: node tools/check-registry-schema-mixed-rollout.js [options]",
    "",
    "Runs a pre-release registry endpoint-schema mixed rollout gate using memory Redis:",
    "  seed v1 legacy records without explicit endpoints and v2 records with explicit endpoints,",
    "  then verify registry consumers return the correct endpoints without local fallback.",
    "",
    "Options:",
    "  --memory                       Use the built-in memory Redis simulation (default)",
    "  --registry-key-prefix <value>  Registry key prefix for drill keys",
    "  --check-id <value>             Stable drill instance id prefix",
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

  const report = await runRegistrySchemaMixedRolloutCheck(args);
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
