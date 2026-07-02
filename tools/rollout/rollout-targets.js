import {
  RegistryDiscoveryClient
} from "../../packages/service-registry/node/registry-schema.js";

const GAME_SERVER_ADMIN_PROTOCOLS = new Set(["tcp"]);
const GAME_PROXY_ADMIN_PROTOCOLS = new Set(["http", "https"]);
const ADMIN_VISIBILITY = "admin";

export const DEFAULT_REGISTRY_URL = process.env.REGISTRY_URL || process.env.REDIS_URL || "redis://127.0.0.1:6379";
export const DEFAULT_REGISTRY_KEY_PREFIX = process.env.REGISTRY_KEY_PREFIX ?? process.env.REDIS_KEY_PREFIX ?? "";
export const DEFAULT_GAME_SERVER_ADMIN_ENDPOINT_NAME = "admin";
export const DEFAULT_GAME_PROXY_ADMIN_ENDPOINT_NAME = "admin";

function parseNumber(value, fallback) {
  if (value === "" || value === undefined || value === null) {
    return fallback;
  }
  if (!/^-?\d+$/.test(String(value))) {
    return Number.NaN;
  }
  return Number.parseInt(value, 10);
}

function truthy(value) {
  return /^(1|true|yes|on)$/i.test(String(value ?? "").trim());
}

function validPort(value) {
  return Number.isInteger(value) && value > 0 && value <= 65535;
}

function isHttpUrl(value) {
  try {
    const parsed = new URL(value);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

function endpointUrl(endpoint) {
  if (endpoint.protocol !== "http" && endpoint.protocol !== "https") {
    return "";
  }
  return `${endpoint.protocol}://${endpoint.host}:${endpoint.port}`;
}

function endpointReport(selection, source, reason) {
  const instance = selection.instance;
  const endpoint = selection.endpoint;
  return {
    service: instance.name,
    serviceName: instance.name,
    instanceId: instance.id,
    instance_id: instance.id,
    endpointName: endpoint.name,
    endpoint_name: endpoint.name,
    protocol: endpoint.protocol,
    visibility: endpoint.visibility,
    host: endpoint.host,
    port: endpoint.port,
    url: endpointUrl(endpoint),
    source,
    reason,
    fallback: source === "local-debug"
  };
}

export function createDefaultRolloutTargetOptions() {
  return {
    registryUrl: DEFAULT_REGISTRY_URL,
    registryKeyPrefix: DEFAULT_REGISTRY_KEY_PREFIX,
    discoveryCacheTtlMs: 1000,
    localDebugTargets: truthy(process.env.MYSERVER_LOCAL_DEBUG_TARGETS),
    resolvedControlTargetsInput: false,
    oldAdminHost: "",
    oldAdminPort: 0,
    oldAdminInstanceId: process.env.MYSERVER_OLD_GAME_ADMIN_INSTANCE_ID || "",
    oldAdminEndpointName: process.env.MYSERVER_OLD_GAME_ADMIN_ENDPOINT_NAME || DEFAULT_GAME_SERVER_ADMIN_ENDPOINT_NAME,
    newAdminHost: "",
    newAdminPort: 0,
    newAdminInstanceId: process.env.MYSERVER_NEW_GAME_ADMIN_INSTANCE_ID || "",
    newAdminEndpointName: process.env.MYSERVER_NEW_GAME_ADMIN_ENDPOINT_NAME || DEFAULT_GAME_SERVER_ADMIN_ENDPOINT_NAME,
    proxyAdminUrl: "",
    proxyInstanceId: process.env.MYSERVER_PROXY_INSTANCE_ID || "",
    proxyAdminEndpointName: process.env.MYSERVER_PROXY_ADMIN_ENDPOINT_NAME || DEFAULT_GAME_PROXY_ADMIN_ENDPOINT_NAME
  };
}

export function applyLocalDebugTargetEnvDefaults(options) {
  if (!options.localDebugTargets) {
    return options;
  }

  if (!options.oldAdminHost && process.env.MYSERVER_OLD_GAME_ADMIN_HOST) {
    options.oldAdminHost = process.env.MYSERVER_OLD_GAME_ADMIN_HOST;
  }
  if (!validPort(options.oldAdminPort) && process.env.MYSERVER_OLD_GAME_ADMIN_PORT) {
    options.oldAdminPort = parseNumber(process.env.MYSERVER_OLD_GAME_ADMIN_PORT, 0);
  }
  if (!options.newAdminHost && process.env.MYSERVER_NEW_GAME_ADMIN_HOST) {
    options.newAdminHost = process.env.MYSERVER_NEW_GAME_ADMIN_HOST;
  }
  if (!validPort(options.newAdminPort) && process.env.MYSERVER_NEW_GAME_ADMIN_PORT) {
    options.newAdminPort = parseNumber(process.env.MYSERVER_NEW_GAME_ADMIN_PORT, 0);
  }
  if (!options.proxyAdminUrl && process.env.MYSERVER_PROXY_ADMIN_URL) {
    options.proxyAdminUrl = process.env.MYSERVER_PROXY_ADMIN_URL;
  }
  return options;
}

export function controlTargetSpec(options, target) {
  if (target === "oldGameServerAdmin") {
    return {
      key: target,
      serviceName: "game-server",
      endpointName: options.oldAdminEndpointName || DEFAULT_GAME_SERVER_ADMIN_ENDPOINT_NAME,
      instanceId: options.oldAdminInstanceId || options.oldServerId || "",
      protocols: GAME_SERVER_ADMIN_PROTOCOLS,
      host: options.oldAdminHost,
      port: options.oldAdminPort,
      tokenKey: "oldAdminToken"
    };
  }
  if (target === "newGameServerAdmin") {
    return {
      key: target,
      serviceName: "game-server",
      endpointName: options.newAdminEndpointName || DEFAULT_GAME_SERVER_ADMIN_ENDPOINT_NAME,
      instanceId: options.newAdminInstanceId || options.newServerId || "",
      protocols: GAME_SERVER_ADMIN_PROTOCOLS,
      host: options.newAdminHost,
      port: options.newAdminPort,
      tokenKey: "newAdminToken"
    };
  }
  if (target === "gameProxyAdmin") {
    return {
      key: target,
      serviceName: "game-proxy",
      endpointName: options.proxyAdminEndpointName || DEFAULT_GAME_PROXY_ADMIN_ENDPOINT_NAME,
      instanceId: options.proxyInstanceId || "",
      protocols: GAME_PROXY_ADMIN_PROTOCOLS,
      url: options.proxyAdminUrl,
      tokenKey: "proxyAdminToken"
    };
  }
  throw new Error(`unknown rollout control target: ${target}`);
}

export function hasDirectControlTarget(options, target) {
  const spec = controlTargetSpec(options, target);
  if (target === "gameProxyAdmin") {
    return Boolean(spec.url);
  }
  return Boolean(spec.host) || spec.port !== undefined && spec.port !== null && spec.port !== 0 && spec.port !== "";
}

export function rolloutTargetLabel(spec) {
  return `${spec.serviceName}.${spec.endpointName}`;
}

export function registryTargetReport(options, target) {
  const spec = controlTargetSpec(options, target);
  return {
    source: "registry",
    target: rolloutTargetLabel(spec),
    service: spec.serviceName,
    endpointName: spec.endpointName,
    endpoint_name: spec.endpointName,
    instanceId: spec.instanceId || "",
    instance_id: spec.instanceId || ""
  };
}

export function directTargetReport(options, target) {
  const spec = controlTargetSpec(options, target);
  const source = options.localDebugTargets ? "local-debug" : "resolved-input";
  if (target === "gameProxyAdmin") {
    return {
      source,
      url: spec.url,
      fallback: source === "local-debug"
    };
  }
  return {
    source,
    endpoint: `${spec.host}:${spec.port}`,
    host: spec.host,
    port: spec.port,
    fallback: source === "local-debug"
  };
}

export function controlTargetPlan(options, target) {
  return hasDirectControlTarget(options, target)
    ? directTargetReport(options, target)
    : registryTargetReport(options, target);
}

function validateDirectTargetPermission(options, errors, target) {
  if (!hasDirectControlTarget(options, target)) {
    return;
  }

  if (options.localDebugTargets || options.resolvedControlTargetsInput) {
    return;
  }

  const spec = controlTargetSpec(options, target);
  errors.push(
    `${rolloutTargetLabel(spec)} direct endpoint override requires --resolved-control-targets or --local-debug-targets`
  );
}

export function validateControlTargetOptions(options, { requireNew = true, requireProxy = true } = {}) {
  const errors = [];

  for (const target of [
    "oldGameServerAdmin",
    ...(requireNew ? ["newGameServerAdmin"] : []),
    ...(requireProxy ? ["gameProxyAdmin"] : [])
  ]) {
    validateDirectTargetPermission(options, errors, target);
  }

  for (const target of ["oldGameServerAdmin", ...(requireNew ? ["newGameServerAdmin"] : [])]) {
    const spec = controlTargetSpec(options, target);
    const hasHost = Boolean(spec.host);
    const hasPort = validPort(spec.port);
    const hasPortInput = spec.port !== undefined && spec.port !== null && spec.port !== 0 && spec.port !== "";
    if (hasHost !== hasPort) {
      errors.push(`${rolloutTargetLabel(spec)} direct endpoint override requires both host and port`);
    }
    if (hasPortInput && !hasPort) {
      errors.push(`invalid ${target} port: expected 1-65535`);
    }
  }

  if (requireProxy && hasDirectControlTarget(options, "gameProxyAdmin")) {
    const spec = controlTargetSpec(options, "gameProxyAdmin");
    if (!isHttpUrl(spec.url)) {
      errors.push("invalid game-proxy.admin direct endpoint override: expected http(s) URL");
    }
  }

  return errors;
}

function endpointMatches(selection, spec) {
  const endpoint = selection.endpoint;
  return endpoint.visibility === ADMIN_VISIBILITY &&
    endpoint.healthy !== false &&
    endpoint.host &&
    validPort(endpoint.port) &&
    spec.protocols.has(endpoint.protocol);
}

async function createRedisClient(registryUrl) {
  const { default: Redis } = await import("ioredis");
  return new Redis(registryUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 0,
    enableOfflineQueue: false
  });
}

function selectEndpoint(candidates, spec) {
  const filtered = candidates.filter((selection) => endpointMatches(selection, spec));
  const matched = spec.instanceId
    ? filtered.filter((selection) => selection.instance.id === spec.instanceId)
    : filtered;

  if (matched.length === 0) {
    throw new Error(
      `${rolloutTargetLabel(spec)} endpoint not found${spec.instanceId ? ` for instance ${spec.instanceId}` : ""}`
    );
  }
  if (!spec.instanceId && matched.length > 1) {
    throw new Error(`${rolloutTargetLabel(spec)} has multiple candidates; pass an instance id`);
  }
  return matched[0];
}

async function discoverTarget(discovery, options, target) {
  const spec = controlTargetSpec(options, target);
  const candidates = await discovery.discoverAllEndpoints(spec.serviceName, spec.endpointName);
  return endpointReport(selectEndpoint(candidates, spec), "registry", "discovered");
}

function directTargetSelection(options, target) {
  const spec = controlTargetSpec(options, target);
  const source = options.localDebugTargets ? "local-debug" : "resolved-input";
  if (target === "gameProxyAdmin") {
    const url = new URL(spec.url);
    return {
      service: spec.serviceName,
      serviceName: spec.serviceName,
      instanceId: spec.instanceId,
      instance_id: spec.instanceId,
      endpointName: spec.endpointName,
      endpoint_name: spec.endpointName,
      protocol: url.protocol.replace(":", ""),
      visibility: ADMIN_VISIBILITY,
      host: url.hostname,
      port: Number.parseInt(url.port || (url.protocol === "https:" ? "443" : "80"), 10),
      url: spec.url.replace(/\/+$/, ""),
      source,
      reason: source === "local-debug" ? "local_debug_fallback" : "pre_resolved_input",
      fallback: source === "local-debug"
    };
  }

  return {
    service: spec.serviceName,
    serviceName: spec.serviceName,
    instanceId: spec.instanceId,
    instance_id: spec.instanceId,
    endpointName: spec.endpointName,
    endpoint_name: spec.endpointName,
    protocol: "tcp",
    visibility: ADMIN_VISIBILITY,
    host: spec.host,
    port: spec.port,
    url: "",
    source,
    reason: source === "local-debug" ? "local_debug_fallback" : "pre_resolved_input",
    fallback: source === "local-debug"
  };
}

export async function resolveRolloutControlTargets(options, {
  requireNew = true,
  requireProxy = true,
  redis: providedRedis = null
} = {}) {
  applyLocalDebugTargetEnvDefaults(options);

  const errors = validateControlTargetOptions(options, { requireNew, requireProxy });
  if (errors.length > 0) {
    throw new Error(errors.join("; "));
  }

  const targets = [
    "oldGameServerAdmin",
    ...(requireNew ? ["newGameServerAdmin"] : []),
    ...(requireProxy ? ["gameProxyAdmin"] : [])
  ];
  const selections = {};
  const registryTargets = targets.filter((target) => !hasDirectControlTarget(options, target));
  let redis = providedRedis;
  let ownsRedis = false;

  try {
    let discovery = null;
    if (registryTargets.length > 0) {
      if (!redis) {
        redis = await createRedisClient(options.registryUrl || DEFAULT_REGISTRY_URL);
        ownsRedis = true;
        await redis.connect();
      }
      discovery = new RegistryDiscoveryClient(redis, {
        registryKeyPrefix: options.registryKeyPrefix || "",
        discoveryCacheTtlMs: options.discoveryCacheTtlMs
      });
    }

    for (const target of targets) {
      selections[target] = hasDirectControlTarget(options, target)
        ? directTargetSelection(options, target)
        : await discoverTarget(discovery, options, target);
    }
  } finally {
    if (redis && ownsRedis) {
      await redis.quit().catch(() => redis.disconnect());
    }
  }

  return selections;
}

export function applyResolvedRolloutControlTargets(options, resolvedTargets) {
  const oldAdmin = resolvedTargets.oldGameServerAdmin;
  if (oldAdmin) {
    options.oldAdminHost = oldAdmin.host;
    options.oldAdminPort = oldAdmin.port;
    options.oldAdminEndpointName = oldAdmin.endpointName;
    options.oldAdminInstanceId = oldAdmin.instanceId;
  }

  const newAdmin = resolvedTargets.newGameServerAdmin;
  if (newAdmin) {
    options.newAdminHost = newAdmin.host;
    options.newAdminPort = newAdmin.port;
    options.newAdminEndpointName = newAdmin.endpointName;
    options.newAdminInstanceId = newAdmin.instanceId;
  }

  const proxyAdmin = resolvedTargets.gameProxyAdmin;
  if (proxyAdmin) {
    options.proxyAdminUrl = proxyAdmin.url;
    options.proxyAdminEndpointName = proxyAdmin.endpointName;
    options.proxyInstanceId = proxyAdmin.instanceId;
  }

  options.resolvedControlTargets = resolvedTargets;
  options.resolvedControlTargetsInput = true;
  return options;
}

export async function resolveAndApplyRolloutControlTargets(options, needs = {}) {
  const resolved = await resolveRolloutControlTargets(options, needs);
  return applyResolvedRolloutControlTargets(options, resolved);
}
