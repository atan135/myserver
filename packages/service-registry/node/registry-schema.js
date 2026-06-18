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
