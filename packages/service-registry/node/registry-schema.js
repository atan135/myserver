export const SERVICE_INSTANCE_FIELDS = [
  "id",
  "name",
  "host",
  "port",
  "admin_port",
  "local_socket",
  "tags",
  "weight",
  "metadata",
  "registered_at",
  "healthy"
];

export function createServiceInstancePayload({
  id,
  name,
  host,
  port,
  admin_port = 0,
  local_socket = "",
  tags = [],
  weight = 100,
  metadata = {},
  registered_at = Date.now(),
  healthy = true
}) {
  return normalizeServiceInstance({
    id,
    name,
    host,
    port,
    admin_port,
    local_socket,
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

  const normalized = {
    id: String(instance.id ?? ""),
    name: String(instance.name ?? ""),
    host: String(instance.host ?? ""),
    port: toPort(instance.port),
    admin_port: toPort(instance.admin_port ?? 0),
    local_socket: String(instance.local_socket ?? ""),
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
  if (!instance.id) errors.push("id must be a non-empty string");
  if (!instance.name) errors.push("name must be a non-empty string");
  if (!instance.host) errors.push("host must be a non-empty string");
  if (!isPort(instance.port)) errors.push("port must be an integer in 0..65535");
  if (!isPort(instance.admin_port)) errors.push("admin_port must be an integer in 0..65535");
  if (typeof instance.local_socket !== "string") errors.push("local_socket must be a string");
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

export function isHealthyInstance(instance) {
  return instance?.healthy !== false;
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

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function stableHash(value) {
  let hash = 2166136261;
  for (let i = 0; i < value.length; i += 1) {
    hash ^= value.charCodeAt(i);
    hash = Math.imul(hash, 16777619);
  }
  return (hash >>> 0) / 4294967295;
}
