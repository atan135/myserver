import { log } from "./logger.js";
import { createServiceInstancePayload } from "../../../packages/service-registry/node/registry-schema.js";

export class RegistryClient {
  constructor(redis, config) {
    this.redis = redis;
    this.config = config;
    this.instanceId = config.serviceInstanceId;
    this.serviceName = config.serviceName;
    this.heartbeatInterval = null;
  }

  async register() {
    const key = `service:${this.serviceName}:instances:${this.instanceId}`;
    const data = createServiceInstancePayload({
      id: this.instanceId,
      name: this.serviceName,
      host: this.config.host,
      port: this.config.port,
      admin_port: 0,
      local_socket: "",
      endpoints: [
        {
          name: "http",
          protocol: "http",
          host: this.config.host,
          port: this.config.port,
          socket: "",
          visibility: "admin",
          metadata: {},
          healthy: true
        }
      ],
      tags: ["admin", "http", "control-plane"],
      metadata: {
        require_tls: this.config.adminApiRequireTls === true,
        ip_allowlist_enabled: this.config.adminApiRequireIpAllowlist === true,
        ip_allowlist: Array.isArray(this.config.adminApiIpAllowlist)
          ? this.config.adminApiIpAllowlist
          : [],
        build_version: this.config.serviceBuildVersion || "dev"
      }
    });

    await this.redis.hset(key, "data", JSON.stringify(data));
    log("info", "registry.registered", {
      service: this.serviceName,
      instance: this.instanceId,
      host: this.config.host,
      port: this.config.port
    });
  }

  async deregister() {
    const key = `service:${this.serviceName}:instances:${this.instanceId}`;
    const heartbeatKey = `heartbeat:${this.serviceName}:${this.instanceId}`;

    await this.redis.del(key);
    await this.redis.del(heartbeatKey);

    log("info", "registry.deregistered", {
      service: this.serviceName,
      instance: this.instanceId
    });
  }

  startHeartbeat(intervalSeconds = 10) {
    const heartbeatKey = `heartbeat:${this.serviceName}:${this.instanceId}`;
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
}
