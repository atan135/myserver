export const SERVICE_INSTANCE_FIELDS = [
  "schema_version",
  "id",
  "name",
  "host",
  "port",
  "admin_port",
  "local_socket",
  "endpoints",
  "tags",
  "weight",
  "metadata",
  "registered_at",
  "healthy"
];

export const SERVICE_ENDPOINT_FIELDS = [
  "name",
  "protocol",
  "host",
  "port",
  "socket",
  "visibility",
  "metadata",
  "healthy"
];

export const SERVICE_INSTANCE_SCHEMA_VERSION = 2;
export const SERVICE_ENDPOINT_PROTOCOLS = ["http", "tcp", "udp", "kcp", "grpc", "local_socket"];
export const SERVICE_ENDPOINT_VISIBILITIES = ["public", "internal", "admin", "local"];
export const DEFAULT_DISCOVERY_CACHE_TTL_MS = 1000;
export const DEFAULT_DISCOVERY_REFRESH_INTERVAL_MS = 5000;
const DISCOVERY_LOG_SOURCES = new Set(["registry", "fallback"]);
const INSTANCE_DISCOVERY_STRATEGY = "healthy_instances_sorted_v1";
const ENDPOINT_PICK_STRATEGY = "weighted_stable_endpoint_v1";
const ALL_ENDPOINTS_STRATEGY = "all_healthy_endpoints_sorted_v1";
const discoveryClientByRedis = new WeakMap();

export function normalizeRegistryKeyPrefix(prefix) {
  return typeof prefix === "string" ? prefix : "";
}

export function registryInstanceKey(prefix, serviceName, instanceId) {
  return `${normalizeRegistryKeyPrefix(prefix)}service:${serviceName}:instances:${instanceId}`;
}

export function registryHeartbeatKey(prefix, serviceName, instanceId) {
  return `${normalizeRegistryKeyPrefix(prefix)}heartbeat:${serviceName}:${instanceId}`;
}

export function registryInstanceScanPattern(prefix, serviceName) {
  return `${normalizeRegistryKeyPrefix(prefix)}service:${serviceName}:instances:*`;
}

export function discoveryLogContext(context = {}) {
  const service = String(context.serviceName ?? context.service ?? "");
  const endpoint = String(context.endpointName ?? context.endpoint ?? "");
  const instanceId = String(context.instanceId ?? context.instance_id ?? "");
  const source = String(context.source ?? "registry");
  const reason = String(context.reason ?? "");
  const normalized = {
    service,
    endpoint,
    instance_id: instanceId,
    source: DISCOVERY_LOG_SOURCES.has(source) ? source : "registry",
    reason,
    serviceName: service,
    endpointName: endpoint,
    instanceId
  };

  if (context.error !== undefined && context.error !== null) {
    normalized.error = context.error instanceof Error
      ? context.error.message
      : String(context.error);
  }

  for (const [key, value] of Object.entries(context)) {
    if (
      value !== undefined &&
      ![
        "service",
        "serviceName",
        "endpoint",
        "endpointName",
        "instance_id",
        "instanceId",
        "source",
        "reason",
        "error"
      ].includes(key)
    ) {
      normalized[key] = value;
    }
  }

  return normalized;
}

export function discoveryLogContextFromSelection(serviceName, endpointName, selection, reason = "discovered") {
  return discoveryLogContext({
    serviceName,
    endpointName,
    instanceId: selection?.instance?.id || "",
    source: "registry",
    reason
  });
}

export function createServiceInstancePayload({
  schema_version = SERVICE_INSTANCE_SCHEMA_VERSION,
  id,
  name,
  host,
  port,
  admin_port = 0,
  local_socket = "",
  endpoints,
  tags = [],
  weight = 100,
  metadata = {},
  registered_at = Date.now(),
  healthy = true
}) {
  const payloadEndpoints = endpoints ?? legacyEndpoints({
    host: String(host ?? ""),
    port: toPort(port),
    admin_port: toPort(admin_port),
    local_socket: String(local_socket ?? "")
  });

  return normalizeServiceInstance({
    schema_version,
    id,
    name,
    host,
    port,
    admin_port,
    local_socket,
    endpoints: payloadEndpoints,
    tags,
    weight,
    metadata,
    registered_at,
    healthy
  });
}

export function normalizeServiceInstance(instance) {
  if (!instance || typeof instance !== "object") {
    return null;
  }

  const legacyHost = String(instance.host ?? "");
  const legacyPort = toPort(instance.port);
  const legacyAdminPort = toPort(instance.admin_port ?? 0);
  const legacyLocalSocket = String(instance.local_socket ?? "");
  const sourceSchemaVersion = sourceSchemaVersionValue(instance.schema_version);
  const normalized = {
    schema_version: normalizeSchemaVersion(sourceSchemaVersion),
    id: String(instance.id ?? ""),
    name: String(instance.name ?? ""),
    host: legacyHost,
    port: legacyPort,
    admin_port: legacyAdminPort,
    local_socket: legacyLocalSocket,
    endpoints: normalizeEndpointList(instance.endpoints, sourceSchemaVersion, {
      host: legacyHost,
      port: legacyPort,
      admin_port: legacyAdminPort,
      local_socket: legacyLocalSocket
    }),
    tags: Array.isArray(instance.tags) ? instance.tags.map(String) : [],
    weight: toNonNegativeInteger(instance.weight ?? 100),
    metadata: isPlainObject(instance.metadata) ? instance.metadata : {},
    registered_at: toNonNegativeInteger(instance.registered_at ?? Date.now()),
    healthy: instance.healthy !== false
  };

  return validateServiceInstance(normalized).ok ? normalized : null;
}

export function validateServiceInstance(instance) {
  const missing = SERVICE_INSTANCE_FIELDS.filter((field) => !(field in (instance ?? {})));
  if (missing.length > 0) {
    return { ok: false, errors: missing.map((field) => `missing field: ${field}`) };
  }

  const errors = [];
  if (instance.schema_version !== SERVICE_INSTANCE_SCHEMA_VERSION) {
    errors.push(`schema_version must be ${SERVICE_INSTANCE_SCHEMA_VERSION}`);
  }
  if (!instance.id) errors.push("id must be a non-empty string");
  if (!instance.name) errors.push("name must be a non-empty string");
  if (typeof instance.host !== "string") errors.push("host must be a string");
  if (!isPort(instance.port)) errors.push("port must be an integer in 0..65535");
  if (!isPort(instance.admin_port)) errors.push("admin_port must be an integer in 0..65535");
  if (typeof instance.local_socket !== "string") errors.push("local_socket must be a string");
  if (!Array.isArray(instance.endpoints)) {
    errors.push("endpoints must be an array");
  } else if (instance.endpoints.length === 0) {
    errors.push("endpoints must contain at least one endpoint");
  } else {
    for (const endpoint of instance.endpoints) {
      const validation = validateServiceEndpoint(endpoint);
      if (!validation.ok) {
        errors.push(...validation.errors.map((error) => `endpoint ${endpoint?.name ?? "<unknown>"}: ${error}`));
      }
    }
  }
  if (!Array.isArray(instance.tags) || instance.tags.some((tag) => typeof tag !== "string")) {
    errors.push("tags must be an array of strings");
  }
  if (!Number.isInteger(instance.weight) || instance.weight < 0) {
    errors.push("weight must be a non-negative integer");
  }
  if (!isPlainObject(instance.metadata)) errors.push("metadata must be an object");
  if (!Number.isInteger(instance.registered_at) || instance.registered_at < 0) {
    errors.push("registered_at must be a non-negative integer");
  }
  if (typeof instance.healthy !== "boolean") errors.push("healthy must be a boolean");

  return { ok: errors.length === 0, errors };
}

export function normalizeEndpoint(endpoint) {
  if (!endpoint || typeof endpoint !== "object") {
    return null;
  }

  const normalized = {
    name: String(endpoint.name ?? ""),
    protocol: String(endpoint.protocol ?? ""),
    host: String(endpoint.host ?? ""),
    port: toPort(endpoint.port ?? 0),
    socket: String(endpoint.socket ?? ""),
    visibility: String(endpoint.visibility ?? "internal"),
    metadata: isPlainObject(endpoint.metadata) ? endpoint.metadata : {},
    healthy: endpoint.healthy !== false
  };

  return validateServiceEndpoint(normalized).ok ? normalized : null;
}

export function validateServiceEndpoint(endpoint) {
  const missing = SERVICE_ENDPOINT_FIELDS.filter((field) => !(field in (endpoint ?? {})));
  if (missing.length > 0) {
    return { ok: false, errors: missing.map((field) => `missing field: ${field}`) };
  }

  const errors = [];
  if (!endpoint.name) errors.push("name must be a non-empty string");
  if (!SERVICE_ENDPOINT_PROTOCOLS.includes(endpoint.protocol)) {
    errors.push(`protocol must be one of: ${SERVICE_ENDPOINT_PROTOCOLS.join(", ")}`);
  }
  if (endpoint.protocol === "local_socket") {
    if (endpoint.socket.length === 0) errors.push("socket must be a non-empty string for local_socket endpoints");
    if (endpoint.host.length > 0) errors.push("host must be empty for local_socket endpoints");
    if (endpoint.port !== 0) errors.push("port must be 0 for local_socket endpoints");
  } else {
    if (endpoint.host.length === 0) errors.push("host must be a non-empty string for network endpoints");
    if (!isNetworkPort(endpoint.port)) errors.push("port must be an integer in 1..65535 for network endpoints");
    if (endpoint.socket.length > 0) errors.push("socket must be empty for network endpoints");
  }
  if (!SERVICE_ENDPOINT_VISIBILITIES.includes(endpoint.visibility)) {
    errors.push(`visibility must be one of: ${SERVICE_ENDPOINT_VISIBILITIES.join(", ")}`);
  }
  if (!isPlainObject(endpoint.metadata)) errors.push("metadata must be an object");
  if (typeof endpoint.healthy !== "boolean") errors.push("healthy must be a boolean");

  return { ok: errors.length === 0, errors };
}

export function isHealthyInstance(instance) {
  return instance?.healthy !== false;
}

export function isHealthyEndpoint(endpoint) {
  return endpoint?.healthy !== false;
}

export function pickServiceInstance(instances) {
  const candidates = instances
    .map(normalizeServiceInstance)
    .filter((instance) => instance && isHealthyInstance(instance) && instance.weight > 0)
    .sort((a, b) => a.id.localeCompare(b.id));

  if (candidates.length === 0) {
    return null;
  }

  let best = candidates[0];
  let bestScore = -1;
  for (const instance of candidates) {
    const score = stableHash(instance.id) * instance.weight;
    if (score > bestScore) {
      best = instance;
      bestScore = score;
    }
  }
  return best;
}

export function getEndpoint(instance, endpointName) {
  const normalized = normalizeServiceInstance(instance);
  if (!normalized) {
    return null;
  }
  return normalized.endpoints.find((endpoint) => endpoint.name === endpointName) ?? null;
}

export function discoverAllEndpoints(instances, endpointName) {
  return instances
    .map(normalizeServiceInstance)
    .filter((instance) => instance && isHealthyInstance(instance) && instance.weight > 0)
    .flatMap((instance) => {
      return instance.endpoints
        .filter((endpoint) => endpoint.name === endpointName && isHealthyEndpoint(endpoint))
        .map((endpoint) => ({ instance, endpoint }));
    })
    .sort((a, b) => {
      const instanceOrder = a.instance.id.localeCompare(b.instance.id);
      if (instanceOrder !== 0) return instanceOrder;
      return a.endpoint.name.localeCompare(b.endpoint.name);
    });
}

export function discoverEndpoint(instances, endpointName) {
  const candidates = discoverAllEndpoints(instances, endpointName);
  if (candidates.length === 0) {
    return null;
  }

  let best = candidates[0];
  let bestScore = -1;
  for (const candidate of candidates) {
    const score = stableHash(`${candidate.instance.id}:${candidate.endpoint.name}`) * candidate.instance.weight;
    if (score > bestScore) {
      best = candidate;
      bestScore = score;
    }
  }
  return best;
}

export function discoverRequiredEndpoint(instances, endpointName, serviceName = null) {
  const discovered = discoverEndpoint(instances, endpointName);
  if (discovered) {
    return discovered;
  }

  throw new Error(
    `service endpoint not found: service=${serviceName || inferServiceName(instances) || "<unknown>"}, endpoint=${endpointName}`
  );
}

export function pickServiceEndpoint(instances, endpointName) {
  return discoverEndpoint(instances, endpointName);
}

export class RegistryDiscoveryClient {
  constructor(redis, options = {}) {
    this.redis = redis;
    this.registryKeyPrefix = normalizeRegistryKeyPrefix(options.registryKeyPrefix);
    this.discoveryCacheTtlMs = normalizeDiscoveryCacheTtlMs(options.discoveryCacheTtlMs);
    this.now = typeof options.now === "function" ? options.now : () => Date.now();
    this.onParseError = typeof options.onParseError === "function" ? options.onParseError : null;
    this.onDiscoveryLog = typeof options.onDiscoveryLog === "function" ? options.onDiscoveryLog : null;
    this.cache = new Map();
    this.refreshHandles = new Map();
  }

  updateCallbacks(options = {}) {
    if (typeof options.onParseError === "function") {
      this.onParseError = options.onParseError;
    }
    if (typeof options.onDiscoveryLog === "function") {
      this.onDiscoveryLog = options.onDiscoveryLog;
    }
    if (typeof options.now === "function") {
      this.now = options.now;
    }
    return this;
  }

  async discoverInstances(serviceName) {
    const { value } = await this.discoverInstancesWithExpiry(serviceName);
    return value;
  }

  async discoverInstancesWithExpiry(serviceName) {
    const cacheKey = discoveryCacheKey({
      prefix: this.registryKeyPrefix,
      serviceName,
      endpointName: "",
      kind: "instances",
      strategy: INSTANCE_DISCOVERY_STRATEGY
    });
    const cached = this.getCachedEntry(cacheKey);
    if (cached) {
      return cached;
    }

    try {
      const instances = await scanServiceInstances(
        this.redis,
        serviceName,
        this.registryKeyPrefix,
        this.onParseError
      );
      this.emitDiscoveryLog(instances.length > 0 ? "info" : "warn", "registry.discovery_instances", {
        serviceName,
        source: "registry",
        reason: instances.length > 0 ? "discovered" : "no_healthy_instance",
        instance_count: instances.length
      });
      return this.setCached(cacheKey, instances);
    } catch (error) {
      this.emitDiscoveryLog("warn", "registry.discovery_instances_failed", {
        serviceName,
        source: "registry",
        reason: "registry_error",
        error
      });
      throw error;
    }
  }

  async discoverEndpoint(serviceName, endpointName) {
    const cacheKey = discoveryCacheKey({
      prefix: this.registryKeyPrefix,
      serviceName,
      endpointName,
      kind: "endpoint",
      strategy: ENDPOINT_PICK_STRATEGY
    });
    const cached = this.getCachedEntry(cacheKey);
    if (cached) {
      return cached.value;
    }

    const { value: instances, expiresAt } = await this.discoverInstancesWithExpiry(serviceName);
    const discovered = discoverEndpoint(instances, endpointName);
    this.emitDiscoveryLog(discovered ? "info" : "warn", "registry.discovery_endpoint", {
      serviceName,
      endpointName,
      instanceId: discovered?.instance?.id || "",
      source: "registry",
      reason: discovered ? "discovered" : "endpoint_missing"
    });
    this.setCached(cacheKey, discovered, expiresAt);
    return discovered;
  }

  async discoverRequiredEndpoint(serviceName, endpointName) {
    const discovered = await this.discoverEndpoint(serviceName, endpointName);
    if (discovered) {
      return discovered;
    }

    throw new Error(`service endpoint not found: service=${serviceName}, endpoint=${endpointName}`);
  }

  async discoverAllEndpoints(serviceName, endpointName) {
    const cacheKey = discoveryCacheKey({
      prefix: this.registryKeyPrefix,
      serviceName,
      endpointName,
      kind: "all_endpoints",
      strategy: ALL_ENDPOINTS_STRATEGY
    });
    const cached = this.getCachedEntry(cacheKey);
    if (cached) {
      return cached.value;
    }

    const { value: instances, expiresAt } = await this.discoverInstancesWithExpiry(serviceName);
    const endpoints = discoverAllEndpoints(instances, endpointName);
    this.emitDiscoveryLog(endpoints.length > 0 ? "info" : "warn", "registry.discovery_all_endpoints", {
      serviceName,
      endpointName,
      source: "registry",
      reason: endpoints.length > 0 ? "discovered" : "endpoint_missing",
      instance_count: endpoints.length
    });
    this.setCached(cacheKey, endpoints, expiresAt);
    return endpoints;
  }

  watch(serviceName, options = {}) {
    return this.getOrCreateRefreshHandle(serviceName, {
      ...options,
      autoStart: false
    });
  }

  startRefresh(serviceName, options = {}) {
    const handle = this.getOrCreateRefreshHandle(serviceName, options);
    handle.start();
    return handle;
  }

  async refreshSnapshot(serviceName, options = {}) {
    const handle = this.getOrCreateRefreshHandle(serviceName, {
      ...options,
      autoStart: false
    });
    return handle.refreshSnapshot();
  }

  getRefreshSnapshot(serviceName, options = {}) {
    const key = refreshSnapshotKey(this.registryKeyPrefix, serviceName, options);
    return this.refreshHandles.get(key)?.getSnapshot();
  }

  stop(serviceName = null, options = {}) {
    if (serviceName) {
      const key = refreshSnapshotKey(this.registryKeyPrefix, serviceName, options);
      const handle = this.refreshHandles.get(key);
      handle?.stop();
      this.refreshHandles.delete(key);
      return;
    }

    for (const handle of this.refreshHandles.values()) {
      handle.stop();
    }
    this.refreshHandles.clear();
  }

  clearCache() {
    this.cache.clear();
  }

  getCached(key) {
    return this.getCachedEntry(key)?.value;
  }

  getCachedEntry(key) {
    if (this.discoveryCacheTtlMs <= 0) {
      return undefined;
    }

    const entry = this.cache.get(key);
    if (!entry) {
      return undefined;
    }

    if (entry.expiresAt <= this.now()) {
      this.cache.delete(key);
      return undefined;
    }

    return entry;
  }

  setCached(key, value, expiresAt = null) {
    if (this.discoveryCacheTtlMs <= 0) {
      return { value, expiresAt: null };
    }

    const cacheExpiresAt = expiresAt ?? this.now() + this.discoveryCacheTtlMs;
    if (cacheExpiresAt <= this.now()) {
      this.cache.delete(key);
      return { value, expiresAt: null };
    }

    const entry = {
      expiresAt: cacheExpiresAt,
      value
    };
    this.cache.set(key, entry);
    return entry;
  }

  getOrCreateRefreshHandle(serviceName, options = {}) {
    const key = refreshSnapshotKey(this.registryKeyPrefix, serviceName, options);
    let handle = this.refreshHandles.get(key);
    if (!handle) {
      handle = new RegistryDiscoveryRefreshHandle(this, normalizeRefreshSpec(serviceName, options));
      this.refreshHandles.set(key, handle);
    }
    return handle;
  }

  async refreshInstancesUncached(serviceName) {
    const instances = await scanServiceInstances(
      this.redis,
      serviceName,
      this.registryKeyPrefix,
      this.onParseError
    );
    const expiresAt = this.discoveryCacheTtlMs > 0 ? this.now() + this.discoveryCacheTtlMs : null;
    this.clearCacheForService(serviceName);
    this.setCached(discoveryCacheKey({
      prefix: this.registryKeyPrefix,
      serviceName,
      endpointName: "",
      kind: "instances",
      strategy: INSTANCE_DISCOVERY_STRATEGY
    }), instances, expiresAt);
    return { instances, expiresAt };
  }

  async loadRefreshValue(spec) {
    const { instances, expiresAt } = await this.refreshInstancesUncached(spec.serviceName);
    if (spec.kind === "instances") {
      return instances;
    }

    if (spec.kind === "endpoint") {
      const endpoint = discoverEndpoint(instances, spec.endpointName);
      this.setCached(discoveryCacheKey({
        prefix: this.registryKeyPrefix,
        serviceName: spec.serviceName,
        endpointName: spec.endpointName,
        kind: "endpoint",
        strategy: ENDPOINT_PICK_STRATEGY
      }), endpoint, expiresAt);
      return endpoint;
    }

    const endpoints = discoverAllEndpoints(instances, spec.endpointName);
    this.setCached(discoveryCacheKey({
      prefix: this.registryKeyPrefix,
      serviceName: spec.serviceName,
      endpointName: spec.endpointName,
      kind: "all_endpoints",
      strategy: ALL_ENDPOINTS_STRATEGY
    }), endpoints, expiresAt);
    return endpoints;
  }

  clearCacheForService(serviceName) {
    const service = String(serviceName ?? "");
    for (const key of this.cache.keys()) {
      const parsed = parseDiscoveryCacheKey(key);
      if (parsed && parsed.prefix === this.registryKeyPrefix && parsed.service === service) {
        this.cache.delete(key);
      }
    }
  }

  emitDiscoveryLog(level, event, context = {}) {
    if (!this.onDiscoveryLog) {
      return;
    }

    try {
      this.onDiscoveryLog(level, event, discoveryLogContext(context));
    } catch {
      // Discovery logging must not affect discovery behavior.
    }
  }
}

export class RegistryDiscoveryRefreshHandle {
  constructor(client, spec) {
    this.client = client;
    this.spec = spec;
    this.timer = null;
    this.snapshot = null;
    this.refreshing = null;
  }

  start() {
    if (this.timer || this.spec.refreshIntervalMs <= 0) {
      return this;
    }

    if (this.spec.immediate) {
      this.refreshSnapshot().catch(() => {});
    }

    this.timer = setInterval(() => {
      this.refreshSnapshot().catch(() => {});
    }, this.spec.refreshIntervalMs);
    this.timer.unref?.();
    return this;
  }

  stop() {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  isRunning() {
    return Boolean(this.timer);
  }

  async refreshSnapshot() {
    if (this.refreshing) {
      return this.refreshing;
    }

    this.refreshing = this.doRefreshSnapshot().finally(() => {
      this.refreshing = null;
    });
    return this.refreshing;
  }

  getSnapshot() {
    if (!this.snapshot) {
      return null;
    }

    return {
      ...this.snapshot,
      value: cloneDiscoveryValue(this.snapshot.value),
      error: this.snapshot.error
    };
  }

  async doRefreshSnapshot() {
    try {
      const value = await this.client.loadRefreshValue(this.spec);
      this.client.emitDiscoveryLog(discoveryValueIsEmpty(value, this.spec.kind) ? "warn" : "info", "registry.discovery_refresh", {
        serviceName: this.spec.serviceName,
        endpointName: this.spec.endpointName,
        instanceId: discoveryValueInstanceId(value, this.spec.kind),
        source: "registry",
        reason: discoveryValueIsEmpty(value, this.spec.kind) ? refreshEmptyReason(this.spec.kind) : "discovered"
      });
      const snapshot = {
        ok: true,
        value: cloneDiscoveryValue(value),
        error: null,
        updatedAt: this.client.now(),
        failedAt: null
      };
      this.snapshot = snapshot;
      return this.getSnapshot();
    } catch (error) {
      const previous = this.snapshot;
      const retainedValue = this.spec.retainStaleOnError ? previous?.value : undefined;
      this.snapshot = {
        ok: false,
        value: cloneDiscoveryValue(retainedValue),
        error,
        updatedAt: this.spec.retainStaleOnError ? previous?.updatedAt ?? null : null,
        failedAt: this.client.now()
      };
      if (!this.spec.retainStaleOnError) {
        this.client.clearCacheForService(this.spec.serviceName);
      }
      this.client.emitDiscoveryLog("warn", "registry.discovery_refresh_failed", {
        serviceName: this.spec.serviceName,
        endpointName: this.spec.endpointName,
        source: "registry",
        reason: "registry_error",
        error
      });
      this.spec.onError?.(error, {
        serviceName: this.spec.serviceName,
        endpointName: this.spec.endpointName,
        kind: this.spec.kind
      });
      throw error;
    }
  }
}

export function getRegistryDiscoveryClient(redis, options = {}) {
  let clients = discoveryClientByRedis.get(redis);
  if (!clients) {
    clients = new Map();
    discoveryClientByRedis.set(redis, clients);
  }

  const registryKeyPrefix = normalizeRegistryKeyPrefix(options.registryKeyPrefix);
  const discoveryCacheTtlMs = normalizeDiscoveryCacheTtlMs(options.discoveryCacheTtlMs);
  const clientKey = `${registryKeyPrefix}\u0000${discoveryCacheTtlMs}`;
  let client = clients.get(clientKey);
  if (!client) {
    client = new RegistryDiscoveryClient(redis, {
      ...options,
      registryKeyPrefix,
      discoveryCacheTtlMs
    });
    clients.set(clientKey, client);
  } else {
    client.updateCallbacks(options);
  }
  return client;
}

export async function discoverServiceInstances(redis, serviceName, options = {}) {
  return getRegistryDiscoveryClient(redis, normalizeDiscoveryOptions(options)).discoverInstances(serviceName);
}

function normalizeEndpointList(endpoints, sourceSchemaVersion, legacy) {
  const explicit = Array.isArray(endpoints) ? endpoints.map(normalizeEndpoint).filter(Boolean) : [];
  if (Array.isArray(endpoints)) {
    return explicit.sort((a, b) => a.name.localeCompare(b.name));
  }
  if (Number(sourceSchemaVersion) >= SERVICE_INSTANCE_SCHEMA_VERSION) {
    return [];
  }

  return legacyEndpoints(legacy).sort((a, b) => a.name.localeCompare(b.name));
}

function normalizeSchemaVersion(value) {
  const parsed = Number(value);
  return parsed === 1 || parsed === SERVICE_INSTANCE_SCHEMA_VERSION ? SERVICE_INSTANCE_SCHEMA_VERSION : toNonNegativeInteger(value);
}

function sourceSchemaVersionValue(value) {
  return value === undefined || value === null ? 1 : value;
}

function legacyEndpoints({ host, port, admin_port, local_socket }) {
  return [
    normalizeEndpoint({
      name: "client",
      protocol: "tcp",
      host,
      port,
      socket: "",
      visibility: "public",
      metadata: {},
      healthy: true
    }),
    normalizeEndpoint({
      name: "admin",
      protocol: "tcp",
      host,
      port: admin_port,
      socket: "",
      visibility: "admin",
      metadata: {},
      healthy: true
    }),
    normalizeEndpoint({
      name: "local_socket",
      protocol: "local_socket",
      host: "",
      port: 0,
      socket: local_socket,
      visibility: "local",
      metadata: {},
      healthy: true
    })
  ].filter(Boolean);
}

function toPort(value) {
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed >= 0 && parsed <= 65535 ? parsed : -1;
}

function toNonNegativeInteger(value) {
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed >= 0 ? parsed : -1;
}

function isPort(value) {
  return Number.isInteger(value) && value >= 0 && value <= 65535;
}

function isNetworkPort(value) {
  return Number.isInteger(value) && value >= 1 && value <= 65535;
}

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function inferServiceName(instances) {
  const serviceNames = instances
    .map(normalizeServiceInstance)
    .filter(Boolean)
    .map((instance) => instance.name)
    .filter(Boolean);
  return serviceNames[0] ?? "";
}

function stableHash(value) {
  let hash = 2166136261;
  for (let i = 0; i < value.length; i += 1) {
    hash ^= value.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0) / 4294967295;
}

async function scanServiceInstances(redis, serviceName, registryKeyPrefix, onParseError = null) {
  const keys = await scanKeys(redis, registryInstanceScanPattern(registryKeyPrefix, serviceName));
  const instances = [];

  for (const key of keys.sort()) {
    const instanceId = key.split(":").at(-1);
    if (!instanceId) {
      continue;
    }

    const heartbeatExists = await redis.exists(registryHeartbeatKey(registryKeyPrefix, serviceName, instanceId));
    if (!heartbeatExists) {
      continue;
    }

    const data = await redis.hget(key, "data");
    if (!data) {
      continue;
    }

    try {
      const instance = normalizeServiceInstance(JSON.parse(data));
      if (instance) {
        instances.push(instance);
      }
    } catch (error) {
      onParseError?.(error, { serviceName, instanceId });
    }
  }

  return instances;
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

function normalizeDiscoveryCacheTtlMs(value) {
  if (value === null || value === undefined || value === "") {
    return DEFAULT_DISCOVERY_CACHE_TTL_MS;
  }

  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : DEFAULT_DISCOVERY_CACHE_TTL_MS;
}

function normalizeDiscoveryRefreshIntervalMs(value) {
  if (value === null || value === undefined || value === "") {
    return DEFAULT_DISCOVERY_REFRESH_INTERVAL_MS;
  }

  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? parsed : DEFAULT_DISCOVERY_REFRESH_INTERVAL_MS;
}

function normalizeDiscoveryOptions(options) {
  if (typeof options === "string") {
    return { registryKeyPrefix: options };
  }
  return options && typeof options === "object" ? options : {};
}

function normalizeRefreshSpec(serviceName, options = {}) {
  const endpointName = String(options.endpointName ?? "");
  let kind = String(options.kind ?? (endpointName ? "all_endpoints" : "instances"));
  if (!["instances", "endpoint", "all_endpoints"].includes(kind)) {
    kind = endpointName ? "all_endpoints" : "instances";
  }
  if (kind !== "instances" && !endpointName) {
    throw new Error("endpointName is required for endpoint refresh");
  }

  return {
    serviceName: String(serviceName ?? ""),
    endpointName,
    kind,
    refreshIntervalMs: normalizeDiscoveryRefreshIntervalMs(options.refreshIntervalMs),
    immediate: options.immediate !== false,
    retainStaleOnError: options.retainStaleOnError === true,
    onError: typeof options.onError === "function" ? options.onError : null
  };
}

function discoveryCacheKey({ prefix, serviceName, endpointName, kind, strategy }) {
  return JSON.stringify({
    prefix: normalizeRegistryKeyPrefix(prefix),
    service: String(serviceName ?? ""),
    endpoint: String(endpointName ?? ""),
    kind,
    strategy
  });
}

function parseDiscoveryCacheKey(key) {
  try {
    const parsed = JSON.parse(key);
    return parsed && typeof parsed === "object" ? parsed : null;
  } catch {
    return null;
  }
}

function refreshSnapshotKey(prefix, serviceName, options = {}) {
  const spec = normalizeRefreshSpec(serviceName, options);
  return JSON.stringify({
    prefix: normalizeRegistryKeyPrefix(prefix),
    service: spec.serviceName,
    endpoint: spec.endpointName,
    kind: spec.kind
  });
}

function cloneDiscoveryValue(value) {
  if (value === undefined || value === null) {
    return value;
  }
  if (Array.isArray(value)) {
    return value.map((item) => cloneDiscoveryValue(item));
  }
  if (typeof value === "object") {
    return {
      ...value,
      instance: value.instance ? cloneDiscoveryValue(value.instance) : value.instance,
      endpoint: value.endpoint ? cloneDiscoveryValue(value.endpoint) : value.endpoint,
      endpoints: Array.isArray(value.endpoints) ? cloneDiscoveryValue(value.endpoints) : value.endpoints,
      metadata: isPlainObject(value.metadata) ? { ...value.metadata } : value.metadata
    };
  }
  return value;
}

function discoveryValueIsEmpty(value, kind) {
  if (kind === "instances") {
    return !Array.isArray(value) || value.length === 0;
  }
  if (kind === "endpoint") {
    return !value;
  }
  return !Array.isArray(value) || value.length === 0;
}

function discoveryValueInstanceId(value, kind) {
  if (kind === "endpoint") {
    return value?.instance?.id || "";
  }
  return "";
}

function refreshEmptyReason(kind) {
  return kind === "instances" ? "no_healthy_instance" : "endpoint_missing";
}
