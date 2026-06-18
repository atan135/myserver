import { log } from "./logger.js";
import {
  createServiceInstancePayload,
  recordRegistryLifecycleMetric,
  registryHeartbeatKey,
  registryInstanceKey
} from "../../../packages/service-registry/node/registry-schema.js";

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
      tags: ["announce", "http"],
      metadata: {
        service_name: this.serviceName,
        service_instance_id: this.instanceId,
        read_auth_required: this.config.announceReadAuthRequired === true,
        cache_ttl_seconds: this.config.announceCacheTtlSeconds,
        build_version: this.config.serviceBuildVersion || "dev",
        zone: this.config.serviceZone || "local"
      }
    });

    try {
      await this.redis.hset(key, "data", JSON.stringify(data));
    } catch (error) {
      this.recordLifecycleFailure("register_failed", {
        endpointName: "http",
        reason: "redis_error",
        error
      });
      throw error;
    }
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

    try {
      await this.redis.del(key);
      await this.redis.del(heartbeatKey);
    } catch (error) {
      this.recordLifecycleFailure("deregister_failed", {
        reason: "redis_error",
        error
      });
      throw error;
    }

    log("info", "registry.deregistered", {
      service: this.serviceName,
      instance: this.instanceId
    });
  }

  startHeartbeat(intervalSeconds = 10) {
    const heartbeatKey = registryHeartbeatKey(this.registryKeyPrefix, this.serviceName, this.instanceId);
    const ttl = 30;

    Promise.resolve()
      .then(() => this.redis.setex(heartbeatKey, ttl, "1"))
      .catch((error) => {
        this.recordLifecycleFailure("heartbeat_failed", {
          reason: "redis_error",
          error
        });
        log("error", "registry.heartbeat_failed", {
          error: error.message
        });
      });

    this.heartbeatInterval = setInterval(async () => {
      try {
        await this.redis.setex(heartbeatKey, ttl, "1");
      } catch (error) {
        this.recordLifecycleFailure("heartbeat_failed", {
          reason: "redis_error",
          error
        });
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

  recordLifecycleFailure(kind, context = {}) {
    recordRegistryLifecycleMetric(kind, {
      serviceName: this.serviceName,
      instanceId: this.instanceId,
      source: "registry",
      ...context
    });
  }
}
