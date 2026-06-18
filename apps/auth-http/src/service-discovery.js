import {
  RegistryDiscoveryClient
} from "../../../packages/service-registry/node/registry-schema.js";
import { serviceUnavailable } from "./common/http-exception.js";

function createGameService(config) {
  if (!config.localDiscoveryFallbackEnabled) {
    return null;
  }

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
    this.registryDiscovery = new RegistryDiscoveryClient(redis, {
      registryKeyPrefix: config.registryKeyPrefix || "",
      discoveryCacheTtlMs: config.registryDiscoveryCacheTtlMs,
      onParseError: (error) => {
        console.error("[service-discovery] parse error:", error);
      }
    });
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
        throw requiredDiscoveryFailed("REGISTRY_ENABLED=false");
      }
      return services;
    }

    const exposeInternalServiceEndpoints = this.config.authExposeInternalServiceEndpoints === true;
    const sideServiceDiscoveryTasks = exposeInternalServiceEndpoints
      ? [
          this.discoverOneEndpoint("chat-server", "tcp", "client"),
          this.discoverOneEndpoint("mail-service", "http", "client"),
          this.discoverOneEndpoint("announce-service", "http", "client")
        ]
      : [Promise.resolve(null), Promise.resolve(null), Promise.resolve(null)];

    const [gameEndpoint, chatEndpoint, mailEndpoint, announceEndpoint] = await Promise.all([
      this.discoverOneEndpoint("game-proxy", "client"),
      ...sideServiceDiscoveryTasks
    ]);

    const discoveredGame = createEndpointDescriptor(gameEndpoint, "kcp");
    if (discoveredGame) {
      services.game = discoveredGame;
    } else if (this.config.registryDiscoveryRequired) {
      throw requiredDiscoveryFailed("game-proxy.client endpoint not found");
    }

    services.chat = createEndpointDescriptor(chatEndpoint, "tcp");
    services.mail = createEndpointDescriptor(mailEndpoint, "http");
    services.announce = createEndpointDescriptor(announceEndpoint, "http");
    return services;
  }

  async discoverOneEndpoint(serviceName, endpointName, legacyEndpointName = null) {
    const discovered = await this.registryDiscovery.discoverEndpoint(serviceName, endpointName);
    if (discovered || !legacyEndpointName) {
      return discovered;
    }
    return this.registryDiscovery.discoverEndpoint(serviceName, legacyEndpointName);
  }

  async discoverInstances(serviceName) {
    return this.registryDiscovery.discoverInstances(serviceName);
  }
}

function requiredDiscoveryFailed(reason) {
  return serviceUnavailable(
    "SERVICE_DISCOVERY_UNAVAILABLE",
    `Required registry discovery failed: ${reason}`
  );
}
