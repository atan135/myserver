import assert from "node:assert/strict";
import test from "node:test";

import { ApiHttpException } from "./common/http-exception.js";
import { ServiceDiscovery } from "./service-discovery.js";

function createRedis(instancesByService) {
  const dataByKey = new Map();
  const heartbeatKeys = new Set();
  const registryKeyPrefix = instancesByService.__registryKeyPrefix || "";

  for (const [serviceName, instances] of Object.entries(instancesByService)) {
    if (serviceName === "__registryKeyPrefix") {
      continue;
    }
    for (const instance of instances) {
      const key = `${registryKeyPrefix}service:${serviceName}:instances:${instance.id}`;
      dataByKey.set(key, JSON.stringify(instance));
      heartbeatKeys.add(`${registryKeyPrefix}heartbeat:${serviceName}:${instance.id}`);
    }
  }

  return {
    async scan(_cursor, _match, pattern) {
      const prefix = pattern.replace("*", "");
      return ["0", [...dataByKey.keys()].filter((key) => key.startsWith(prefix))];
    },
    async exists(key) {
      return heartbeatKeys.has(key) ? 1 : 0;
    },
    async hget(key, field) {
      return field === "data" ? dataByKey.get(key) ?? null : null;
    }
  };
}

function createConfig(overrides = {}) {
  return {
    gameProxyHost: "127.0.0.1",
    gameProxyPort: 4000,
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: false,
    authExposeInternalServiceEndpoints: true,
    ...overrides
  };
}

test("ServiceDiscovery uses configured fallback when registry discovery is disabled", async () => {
  const discovery = new ServiceDiscovery(createRedis({}), createConfig());

  assert.deepEqual(await discovery.discoverClientServices(), {
    game: {
      host: "127.0.0.1",
      port: 4000,
      protocol: "kcp"
    },
    chat: null,
    mail: null,
    announce: null
  });
});

test("ServiceDiscovery discovers game-proxy.client and named service endpoints", async () => {
  const redis = createRedis({
    "game-proxy": [
      {
        id: "proxy-a",
        name: "game-proxy",
        host: "10.0.0.1",
        port: 4000,
        endpoints: [
          {
            name: "client",
            protocol: "kcp",
            host: "203.0.113.10",
            port: 4100,
            socket: "",
            visibility: "public",
            metadata: {},
            healthy: true
          }
        ]
      }
    ],
    "chat-server": [
      {
        id: "chat-a",
        name: "chat-server",
        host: "10.0.0.2",
        port: 9001,
        endpoints: [
          {
            name: "tcp",
            protocol: "tcp",
            host: "10.0.0.20",
            port: 9011,
            socket: "",
            visibility: "internal",
            metadata: {},
            healthy: true
          }
        ]
      }
    ],
    "mail-service": [
      {
        id: "mail-a",
        name: "mail-service",
        host: "10.0.0.3",
        port: 9003,
        endpoints: [
          {
            name: "http",
            protocol: "http",
            host: "10.0.0.30",
            port: 9013,
            socket: "",
            visibility: "internal",
            metadata: {},
            healthy: true
          }
        ]
      }
    ],
    "announce-service": [
      {
        id: "announce-a",
        name: "announce-service",
        host: "10.0.0.4",
        port: 9004,
        endpoints: [
          {
            name: "http",
            protocol: "http",
            host: "10.0.0.40",
            port: 9014,
            socket: "",
            visibility: "internal",
            metadata: {},
            healthy: true
          }
        ]
      }
    ]
  });
  const discovery = new ServiceDiscovery(redis, createConfig({ registryDiscoveryEnabled: true }));

  assert.deepEqual(await discovery.discoverClientServices(), {
    game: {
      host: "203.0.113.10",
      port: 4100,
      protocol: "kcp"
    },
    chat: {
      host: "10.0.0.20",
      port: 9011,
      protocol: "tcp"
    },
    mail: {
      host: "10.0.0.30",
      port: 9013,
      protocol: "http"
    },
    announce: {
      host: "10.0.0.40",
      port: 9014,
      protocol: "http"
    }
  });
});

test("ServiceDiscovery uses registry key prefix for scans and heartbeats", async () => {
  const redis = createRedis({
    __registryKeyPrefix: "test:",
    "game-proxy": [
      {
        id: "proxy-a",
        name: "game-proxy",
        host: "10.0.0.1",
        port: 4000,
        endpoints: [
          {
            name: "client",
            protocol: "kcp",
            host: "203.0.113.10",
            port: 4100,
            socket: "",
            visibility: "public",
            metadata: {},
            healthy: true
          }
        ]
      }
    ]
  });
  const discovery = new ServiceDiscovery(
    redis,
    createConfig({
      registryDiscoveryEnabled: true,
      authExposeInternalServiceEndpoints: false,
      registryKeyPrefix: "test:"
    })
  );

  const services = await discovery.discoverClientServices();

  assert.deepEqual(services.game, {
    host: "203.0.113.10",
    port: 4100,
    protocol: "kcp"
  });
});

test("ServiceDiscovery hides internal service endpoints when exposure is disabled", async () => {
  const redis = createRedis({
    "game-proxy": [
      {
        id: "proxy-a",
        name: "game-proxy",
        host: "10.0.0.1",
        port: 4000,
        endpoints: [
          {
            name: "client",
            protocol: "kcp",
            host: "203.0.113.10",
            port: 4100,
            socket: "",
            visibility: "public",
            metadata: {},
            healthy: true
          }
        ]
      }
    ],
    "chat-server": [
      {
        id: "chat-a",
        name: "chat-server",
        host: "10.0.0.2",
        port: 9001,
        endpoints: [
          {
            name: "tcp",
            protocol: "tcp",
            host: "10.0.0.20",
            port: 9011,
            socket: "",
            visibility: "internal",
            metadata: {},
            healthy: true
          }
        ]
      }
    ],
    "mail-service": [
      {
        id: "mail-a",
        name: "mail-service",
        host: "10.0.0.3",
        port: 9003,
        endpoints: [
          {
            name: "http",
            protocol: "http",
            host: "10.0.0.30",
            port: 9013,
            socket: "",
            visibility: "internal",
            metadata: {},
            healthy: true
          }
        ]
      }
    ],
    "announce-service": [
      {
        id: "announce-a",
        name: "announce-service",
        host: "10.0.0.4",
        port: 9004,
        endpoints: [
          {
            name: "http",
            protocol: "http",
            host: "10.0.0.40",
            port: 9014,
            socket: "",
            visibility: "internal",
            metadata: {},
            healthy: true
          }
        ]
      }
    ]
  });
  const discovery = new ServiceDiscovery(
    redis,
    createConfig({
      registryDiscoveryEnabled: true,
      authExposeInternalServiceEndpoints: false
    })
  );

  assert.deepEqual(await discovery.discoverClientServices(), {
    game: {
      host: "203.0.113.10",
      port: 4100,
      protocol: "kcp"
    },
    chat: null,
    mail: null,
    announce: null
  });
});

test("ServiceDiscovery throws when game-proxy.client is required but missing", async () => {
  const discovery = new ServiceDiscovery(
    createRedis({}),
    createConfig({
      registryDiscoveryEnabled: true,
      registryDiscoveryRequired: true
    })
  );

  await assert.rejects(
    () => discovery.discoverClientServices(),
    (error) => {
      assert.ok(error instanceof ApiHttpException);
      assert.equal(error.getStatus(), 503);
      assert.deepEqual(error.getResponse(), {
        ok: false,
        error: "SERVICE_DISCOVERY_UNAVAILABLE",
        message: "Required registry discovery failed: game-proxy.client endpoint not found"
      });
      return true;
    }
  );
});

test("ServiceDiscovery throws when discovery is required but registry is disabled", async () => {
  const discovery = new ServiceDiscovery(
    createRedis({}),
    createConfig({
      registryDiscoveryEnabled: false,
      registryDiscoveryRequired: true
    })
  );

  await assert.rejects(
    () => discovery.discoverClientServices(),
    (error) => {
      assert.ok(error instanceof ApiHttpException);
      assert.equal(error.getStatus(), 503);
      assert.deepEqual(error.getResponse(), {
        ok: false,
        error: "SERVICE_DISCOVERY_UNAVAILABLE",
        message: "Required registry discovery failed: REGISTRY_ENABLED=false"
      });
      return true;
    }
  );
});

test("ServiceDiscovery falls back to legacy client endpoint for side services", async () => {
  const redis = createRedis({
    "game-proxy": [
      {
        id: "proxy-a",
        name: "game-proxy",
        host: "127.0.0.1",
        port: 4000,
        endpoints: [
          {
            name: "client",
            protocol: "kcp",
            host: "127.0.0.1",
            port: 4000,
            socket: "",
            visibility: "public",
            metadata: {},
            healthy: true
          }
        ]
      }
    ],
    "mail-service": [
      {
        id: "mail-legacy",
        name: "mail-service",
        host: "127.0.0.1",
        port: 9003,
        admin_port: 0,
        local_socket: "",
        tags: [],
        weight: 100,
        metadata: {},
        registered_at: 1,
        healthy: true
      }
    ]
  });
  const discovery = new ServiceDiscovery(redis, createConfig({ registryDiscoveryEnabled: true }));

  const services = await discovery.discoverClientServices();

  assert.deepEqual(services.mail, {
    host: "127.0.0.1",
    port: 9003,
    protocol: "http"
  });
});

test("ServiceDiscovery does not fabricate side service endpoints when registry discovery is disabled", async () => {
  const discovery = new ServiceDiscovery(
    createRedis({}),
    createConfig({
      registryDiscoveryEnabled: false,
      authExposeInternalServiceEndpoints: true
    })
  );

  const services = await discovery.discoverClientServices();

  assert.deepEqual(services, {
    game: {
      host: "127.0.0.1",
      port: 4000,
      protocol: "kcp"
    },
    chat: null,
    mail: null,
    announce: null
  });
});
