import {
  discoverEndpoint,
  normalizeServiceInstance,
} from "../../../packages/service-registry/node/registry-schema.js";

function createGameService(config) {
  return {
    host: config.gameProxyHost,
    port: config.gameProxyPort,
    protocol: "kcp"
  };
}

function createEndpointDescriptor(selection, protocolOverride = null) {
  const endpoint = selection?.endpoint;
  if (!endpoint?.host || !endpoint?.port) {
    return null;
  }

  return {
    host: endpoint.host,
    port: endpoint.port,
    protocol: protocolOverride || endpoint.protocol
  };
}

export class ServiceDiscovery {
  constructor(redis, config) {
    this.redis = redis;
    this.config = config;
  }

  async discoverClientServices() {
    const services = {
      game: createGameService(this.config),
      chat: null,
      mail: null,
      announce: null
    };

    if (!this.config.registryDiscoveryEnabled) {
      if (this.config.registryDiscoveryRequired) {
        throw new Error("Required registry discovery failed: REGISTRY_ENABLED=false");
      }
      return services;
    }

    const [gameEndpoint, chatEndpoint, mailEndpoint, announceEndpoint] = await Promise.all([
      this.discoverOneEndpoint("game-proxy", "client"),
      this.discoverOneEndpoint("chat-server", "tcp", "client"),
      this.discoverOneEndpoint("mail-service", "http", "client"),
      this.discoverOneEndpoint("announce-service", "http", "client")
    ]);

    const discoveredGame = createEndpointDescriptor(gameEndpoint, "kcp");
    if (discoveredGame) {
      services.game = discoveredGame;
    } else if (this.config.registryDiscoveryRequired) {
      throw new Error("Required registry discovery failed: game-proxy.client endpoint not found");
    }

    services.chat = createEndpointDescriptor(chatEndpoint, "tcp");
    services.mail = createEndpointDescriptor(mailEndpoint, "http");
    services.announce = createEndpointDescriptor(announceEndpoint, "http");
    return services;
  }

  async discoverOneEndpoint(serviceName, endpointName, legacyEndpointName = null) {
    const instances = await this.discoverInstances(serviceName);
    return discoverEndpoint(instances, endpointName) ||
      (legacyEndpointName ? discoverEndpoint(instances, legacyEndpointName) : null);
  }

  async discoverInstances(serviceName) {
    const keys = await scanKeys(this.redis, `service:${serviceName}:instances:*`);
    const instances = [];

    for (const key of keys.sort()) {
      const instanceId = key.split(":").at(-1);
      const heartbeatKey = `heartbeat:${serviceName}:${instanceId}`;
      const heartbeatExists = await this.redis.exists(heartbeatKey);
      if (!heartbeatExists) {
        continue;
      }

      const data = await this.redis.hget(key, "data");
      if (!data) {
        continue;
      }

      try {
        const instance = normalizeServiceInstance(JSON.parse(data));
        if (instance) {
          instances.push(instance);
        }
      } catch (error) {
        console.error("[service-discovery] parse error:", error);
      }
    }

    return instances;
  }
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
