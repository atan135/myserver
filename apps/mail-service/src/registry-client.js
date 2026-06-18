import { log } from "./logger.js";
import {
  createServiceInstancePayload,
  discoveryLogContext,
  discoverAllEndpoints,
  discoverServiceInstances as discoverRegistryServiceInstances,
  registryHeartbeatKey,
  registryInstanceKey
} from "../../../packages/service-registry/node/registry-schema.js";

const GAME_SERVER_SERVICE_NAME = "game-server";
const GAME_SERVER_ADMIN_ENDPOINT_NAME = "admin";
const GAME_SERVER_ADMIN_PROTOCOLS = new Set(["tcp"]);
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
          visibility: "internal",
          metadata: {
            service_name: this.serviceName,
            service_instance_id: this.instanceId,
            build_version: this.config.serviceBuildVersion || "dev",
            zone: this.config.serviceZone || "local"
          },
          healthy: true
        }
      ],
      tags: ["mail", "http"],
      metadata: {
        service_name: this.serviceName,
        service_instance_id: this.instanceId,
        player_auth_required: this.config.mailPlayerAuthRequired === true,
        service_token_enabled: String(this.config.mailServiceToken || "").trim().length > 0,
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

    // 立即发送一次心跳
    this.redis.setex(heartbeatKey, ttl, "1");

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
}

export async function discoverGameServerAdminEndpoints(redis, registryKeyPrefix = "") {
  const instances = await discoverServiceInstances(redis, GAME_SERVER_SERVICE_NAME, registryKeyPrefix);
  const endpoints = discoverAllEndpoints(instances, GAME_SERVER_ADMIN_ENDPOINT_NAME)
    .filter(({ endpoint }) => GAME_SERVER_ADMIN_PROTOCOLS.has(endpoint.protocol))
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
      metadata: endpoint.metadata || {}
    }));
  logDiscovery(endpoints.length > 0 ? "info" : "warn", "registry.discovery_all_endpoints", {
    serviceName: GAME_SERVER_SERVICE_NAME,
    endpointName: GAME_SERVER_ADMIN_ENDPOINT_NAME,
    source: "registry",
    reason: endpoints.length > 0 ? "discovered" : "endpoint_missing",
    instance_count: endpoints.length
  });
  return endpoints;
}

export async function discoverServiceInstances(redis, serviceName, registryKeyPrefix = "") {
  const options = typeof registryKeyPrefix === "object"
    ? registryKeyPrefix
    : { registryKeyPrefix };

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

function logDiscovery(level, event, context = {}) {
  try {
    log(level, event, discoveryLogContext(context));
  } catch {
    // Focused tests may instantiate registry helpers before logger bootstrap.
  }
}
