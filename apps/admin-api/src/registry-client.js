import { log } from "./logger.js";
import {
  DEFAULT_DISCOVERY_REFRESH_INTERVAL_MS,
  getRegistryDiscoveryClient,
  createServiceInstancePayload,
  discoveryLogContext,
  discoverAllEndpoints,
  discoverServiceInstances as discoverRegistryServiceInstances,
  recordDiscoveryMetric,
  registryHeartbeatKey,
  registryInstanceKey
} from "../../../packages/service-registry/node/registry-schema.js";

const GAME_SERVER_SERVICE_NAME = "game-server";
const GAME_SERVER_ADMIN_ENDPOINT_NAME = "admin";
const GAME_SERVER_ADMIN_PROTOCOLS = new Set(["tcp"]);
const ADMIN_ENDPOINT_VISIBILITY = "admin";
const GAME_PROXY_SERVICE_NAME = "game-proxy";
const GAME_PROXY_ADMIN_ENDPOINT_NAME = "admin";
const GAME_PROXY_ADMIN_PROTOCOLS = new Set(["http"]);
const UNSAFE_ADVERTISED_HOSTS = new Set(["", "0.0.0.0", "::", "[::]"]);

function publishedHostFromConfig(config) {
  const configured = typeof config.advertisedHost === "string" && config.advertisedHost.trim()
    ? config.advertisedHost
    : config.host;
  const host = String(configured ?? "").trim();
  return UNSAFE_ADVERTISED_HOSTS.has(host) ? "127.0.0.1" : host;
}

export class RegistryClient {
  constructor(redis, config) {
    this.redis = redis;
    this.config = config;
    this.instanceId = config.serviceInstanceId;
    this.serviceName = config.serviceName;
    this.registryKeyPrefix = config.registryKeyPrefix || "";
    this.heartbeatInterval = null;
    this.discoveryClient = createRegistryDiscoveryClient(redis, config);
    this.discoveryRefreshHandles = [];
  }

  async register() {
    const key = registryInstanceKey(this.registryKeyPrefix, this.serviceName, this.instanceId);
    const endpointHost = publishedHostFromConfig(this.config);
    const data = createServiceInstancePayload({
      id: this.instanceId,
      name: this.serviceName,
      host: endpointHost,
      port: this.config.port,
      admin_port: 0,
      local_socket: "",
      endpoints: [
        {
          name: "http",
          protocol: "http",
          host: endpointHost,
          port: this.config.port,
          socket: "",
          visibility: "admin",
          metadata: {
            service_name: this.serviceName,
            service_instance_id: this.instanceId,
            build_version: this.config.serviceBuildVersion || "dev",
            zone: this.config.serviceZone || "local"
          },
          healthy: true
        }
      ],
      tags: ["admin", "http", "control-plane"],
      metadata: {
        service_name: this.serviceName,
        service_instance_id: this.instanceId,
        require_tls: this.config.adminApiRequireTls === true,
        ip_allowlist_enabled: this.config.adminApiRequireIpAllowlist === true,
        ip_allowlist: Array.isArray(this.config.adminApiIpAllowlist)
          ? this.config.adminApiIpAllowlist
          : [],
        build_version: this.config.serviceBuildVersion || "dev",
        zone: this.config.serviceZone || "local"
      }
    });

    await this.redis.hset(key, "data", JSON.stringify(data));
    log("info", "registry.registered", {
      service: this.serviceName,
      instance: this.instanceId,
      host: endpointHost,
      port: this.config.port
    });
  }

  async deregister() {
    const key = registryInstanceKey(this.registryKeyPrefix, this.serviceName, this.instanceId);
    const heartbeatKey = registryHeartbeatKey(this.registryKeyPrefix, this.serviceName, this.instanceId);

    await this.redis.del(key);
    await this.redis.del(heartbeatKey);

    log("info", "registry.deregistered", {
      service: this.serviceName,
      instance: this.instanceId
    });
  }

  startHeartbeat(intervalSeconds = 10) {
    const heartbeatKey = registryHeartbeatKey(this.registryKeyPrefix, this.serviceName, this.instanceId);
    const ttl = 30;

    Promise.resolve(this.redis.setex(heartbeatKey, ttl, "1")).catch((error) => {
      log("error", "registry.heartbeat_failed", {
        error: error.message
      });
    });

    this.heartbeatInterval = setInterval(async () => {
      try {
        await this.redis.setex(heartbeatKey, ttl, "1");
      } catch (error) {
        log("error", "registry.heartbeat_failed", {
          error: error.message
        });
      }
    }, intervalSeconds * 1000);

    log("info", "registry.heartbeat_started", {
      interval: intervalSeconds,
      ttl
    });
  }

  stopHeartbeat() {
    if (this.heartbeatInterval) {
      clearInterval(this.heartbeatInterval);
      this.heartbeatInterval = null;
    }
  }

  startDiscoveryRefresh(intervalMs = this.config.registryDiscoveryRefreshIntervalMs) {
    if (!this.config.registryDiscoveryEnabled) {
      return [];
    }

    this.stopDiscoveryRefresh();

    const refreshIntervalMs = normalizeRefreshIntervalMs(intervalMs);
    const onError = (error, context) => {
      logDiscovery("warn", "registry.discovery_refresh_failed", {
        serviceName: context.serviceName,
        endpointName: context.endpointName,
        source: "registry",
        reason: "registry_error",
        error
      });
    };

    this.discoveryRefreshHandles = [
      this.discoveryClient.startRefresh(GAME_SERVER_SERVICE_NAME, {
        endpointName: GAME_SERVER_ADMIN_ENDPOINT_NAME,
        kind: "all_endpoints",
        refreshIntervalMs,
        onError
      }),
      this.discoveryClient.startRefresh(GAME_PROXY_SERVICE_NAME, {
        endpointName: GAME_PROXY_ADMIN_ENDPOINT_NAME,
        kind: "all_endpoints",
        refreshIntervalMs,
        onError
      })
    ];

    log("info", "registry.discovery_refresh_started", {
      interval_ms: refreshIntervalMs,
      services: [GAME_SERVER_SERVICE_NAME, GAME_PROXY_SERVICE_NAME]
    });

    return this.discoveryRefreshHandles;
  }

  stopDiscoveryRefresh() {
    this.discoveryClient.stop(GAME_SERVER_SERVICE_NAME, {
      endpointName: GAME_SERVER_ADMIN_ENDPOINT_NAME,
      kind: "all_endpoints"
    });
    this.discoveryClient.stop(GAME_PROXY_SERVICE_NAME, {
      endpointName: GAME_PROXY_ADMIN_ENDPOINT_NAME,
      kind: "all_endpoints"
    });
    this.discoveryRefreshHandles = [];
  }
}

export async function discoverGameServerAdminEndpoints(redis, registryKeyPrefix = "") {
  const options = normalizeAdminDiscoveryOptions(registryKeyPrefix);
  const candidates = await discoverAdminEndpointCandidates(
    redis,
    options,
    GAME_SERVER_SERVICE_NAME,
    GAME_SERVER_ADMIN_ENDPOINT_NAME
  );
  const endpoints = candidates
    .filter(({ endpoint }) =>
      endpoint.visibility === ADMIN_ENDPOINT_VISIBILITY &&
      GAME_SERVER_ADMIN_PROTOCOLS.has(endpoint.protocol)
    )
    .map(({ instance, endpoint }) => ({
      service: GAME_SERVER_SERVICE_NAME,
      instanceId: instance.id,
      instance_id: instance.id,
      endpointName: endpoint.name,
      endpoint_name: endpoint.name,
      protocol: endpoint.protocol,
      host: endpoint.host,
      port: endpoint.port,
      healthy: instance.healthy !== false && endpoint.healthy !== false,
      weight: instance.weight,
      metadata: endpoint.metadata || {},
      fallback: false,
      source: "registry",
      reason: "discovered"
    }));
  emitDiscoveryLog(options, endpoints.length > 0 ? "info" : "warn", "registry.discovery_all_endpoints", {
    serviceName: GAME_SERVER_SERVICE_NAME,
    endpointName: GAME_SERVER_ADMIN_ENDPOINT_NAME,
    source: "registry",
    reason: endpoints.length > 0 ? "discovered" : "endpoint_missing",
    instance_count: endpoints.length
  });
  return endpoints;
}

export async function discoverGameProxyAdminEndpoints(redis, registryKeyPrefix = "") {
  const options = normalizeAdminDiscoveryOptions(registryKeyPrefix);
  const candidates = await discoverAdminEndpointCandidates(
    redis,
    options,
    GAME_PROXY_SERVICE_NAME,
    GAME_PROXY_ADMIN_ENDPOINT_NAME
  );
  const endpoints = candidates
    .filter(({ endpoint }) =>
      endpoint.visibility === ADMIN_ENDPOINT_VISIBILITY &&
      GAME_PROXY_ADMIN_PROTOCOLS.has(endpoint.protocol)
    )
    .map(({ instance, endpoint }) => ({
      service: GAME_PROXY_SERVICE_NAME,
      instanceId: instance.id,
      instance_id: instance.id,
      endpointName: endpoint.name,
      endpoint_name: endpoint.name,
      protocol: endpoint.protocol,
      host: endpoint.host,
      port: endpoint.port,
      healthy: instance.healthy !== false && endpoint.healthy !== false,
      weight: instance.weight,
      metadata: endpoint.metadata || {},
      fallback: false,
      source: "registry",
      reason: "discovered"
    }));
  emitDiscoveryLog(options, endpoints.length > 0 ? "info" : "warn", "registry.discovery_all_endpoints", {
    serviceName: GAME_PROXY_SERVICE_NAME,
    endpointName: GAME_PROXY_ADMIN_ENDPOINT_NAME,
    source: "registry",
    reason: endpoints.length > 0 ? "discovered" : "endpoint_missing",
    instance_count: endpoints.length
  });
  return endpoints;
}

export async function discoverServiceInstances(redis, serviceName, registryKeyPrefix = "") {
  const options = normalizeAdminDiscoveryOptions(registryKeyPrefix);

  return discoverRegistryServiceInstances(redis, serviceName, {
    ...options,
    onParseError: options.onParseError || ((error, context) => {
      logDiscovery("warn", "registry.discovery_parse_failed", {
        serviceName: context.serviceName,
        instanceId: context.instanceId,
        source: "registry",
        reason: "registry_error",
        error
      });
    }),
    onDiscoveryLog: options.onDiscoveryLog || logDiscovery
  });
}

export function createRegistryDiscoveryClient(redis, configOrOptions = {}) {
  const options = normalizeAdminDiscoveryOptions(configOrOptions);
  return getRegistryDiscoveryClient(redis, {
    ...options,
    onParseError: options.onParseError || ((error, context) => {
      logDiscovery("warn", "registry.discovery_parse_failed", {
        serviceName: context.serviceName,
        instanceId: context.instanceId,
        source: "registry",
        reason: "registry_error",
        error
      });
    }),
    onDiscoveryLog: options.onDiscoveryLog || logDiscovery
  });
}

export function normalizeAdminDiscoveryOptions(configOrOptions = "") {
  if (typeof configOrOptions === "string") {
    return { registryKeyPrefix: configOrOptions };
  }

  const options = configOrOptions && typeof configOrOptions === "object" ? configOrOptions : {};
  return {
    registryKeyPrefix: options.registryKeyPrefix || "",
    discoveryCacheTtlMs: options.discoveryCacheTtlMs ?? options.registryDiscoveryCacheTtlMs,
    onParseError: options.onParseError,
    onDiscoveryLog: options.onDiscoveryLog
  };
}

async function discoverAdminEndpointCandidates(redis, configOrOptions, serviceName, endpointName) {
  const options = normalizeAdminDiscoveryOptions(configOrOptions);
  const client = createRegistryDiscoveryClient(redis, options);
  const snapshot = client.getRefreshSnapshot(serviceName, {
    endpointName,
    kind: "all_endpoints"
  });

  if (snapshot?.ok) {
    return snapshot.value || [];
  }

  if (snapshot && !snapshot.ok) {
    throw snapshot.error || new Error(`service discovery refresh failed: service=${serviceName}, endpoint=${endpointName}`);
  }

  const instances = await discoverServiceInstances(redis, serviceName, options);
  return discoverAllEndpoints(instances, endpointName);
}

function normalizeRefreshIntervalMs(value) {
  if (value === null || value === undefined || value === "") {
    return DEFAULT_DISCOVERY_REFRESH_INTERVAL_MS;
  }

  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : DEFAULT_DISCOVERY_REFRESH_INTERVAL_MS;
}

function logDiscovery(level, event, context = {}) {
  if (!context.__discoveryMetricRecorded) {
    recordDiscoveryMetric(context);
  }

  log(level, event, discoveryLogContext(context));
}

function emitDiscoveryLog(options, level, event, context = {}) {
  if (typeof options?.onDiscoveryLog !== "function") {
    logDiscovery(level, event, context);
    return;
  }

  const metricRecorded = recordDiscoveryMetric(context) !== null;
  const normalized = discoveryLogContext(context);
  if (metricRecorded) {
    normalized.__discoveryMetricRecorded = true;
  }
  options.onDiscoveryLog(level, event, normalized);
}
