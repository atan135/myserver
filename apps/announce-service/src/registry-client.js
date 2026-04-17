import { log } from "./logger.js";

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
    const data = {
      id: this.instanceId,
      name: this.serviceName,
      host: this.config.host,
      port: this.config.port,
      tags: ["announce", "http"],
      weight: 100,
      metadata: {},
      registered_at: Date.now(),
      healthy: true
    };

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
