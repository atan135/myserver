function createGameService(config) {
  return {
    host: config.gameProxyHost,
    port: config.gameProxyPort,
    protocol: "kcp"
  };
}

function createServiceDescriptor(instance, protocol) {
  if (!instance?.host || !instance?.port) {
    return null;
  }

  return {
    host: instance.host,
    port: instance.port,
    protocol
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
      return services;
    }

    const [chatInstance, mailInstance, announceInstance] = await Promise.all([
      this.discoverOne("chat-server"),
      this.discoverOne("mail-service"),
      this.discoverOne("announce-service")
    ]);

    services.chat = createServiceDescriptor(chatInstance, "tcp");
    services.mail = createServiceDescriptor(mailInstance, "http");
    services.announce = createServiceDescriptor(announceInstance, "http");
    return services;
  }

  async discoverOne(serviceName) {
    const keys = await scanKeys(this.redis, `service:${serviceName}:instances:*`);

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
        return JSON.parse(data);
      } catch (error) {
        console.error("[service-discovery] parse error:", error);
      }
    }

    return null;
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
