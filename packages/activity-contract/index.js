// Shared admin-api/admin-web contract for activity type schemas. Runtime
// handlers remain in game-server; this module only describes registration and
// validates the versioned shape accepted by the control plane.

export const ACTIVITY_TYPE_SCHEMA_VERSION = 1;

export const ACTIVITY_TYPE_SCHEMAS = Object.freeze([
  Object.freeze({
    type: "login_reward",
    schemaVersion: ACTIVITY_TYPE_SCHEMA_VERSION,
    actions: Object.freeze(["list", "detail", "claim", "progress"]),
    configShape: "object"
  }),
  Object.freeze({
    type: "lottery",
    schemaVersion: ACTIVITY_TYPE_SCHEMA_VERSION,
    actions: Object.freeze(["list", "detail", "draw", "progress"]),
    configShape: "object"
  })
]);

export class ActivityTypeContractError extends Error {
  constructor(code, message) {
    super(`${code}: ${message}`);
    this.name = "ActivityTypeContractError";
    this.code = code;
  }
}

function assertDescriptor(descriptor) {
  if (!descriptor || typeof descriptor !== "object" || Array.isArray(descriptor)) {
    throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "type schema must be an object");
  }
  if (typeof descriptor.type !== "string" || !/^[a-z][a-z0-9_]{1,63}$/.test(descriptor.type)) {
    throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "type schema name is invalid");
  }
  if (!Number.isInteger(descriptor.schemaVersion) || descriptor.schemaVersion <= 0) {
    throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "schemaVersion must be a positive integer");
  }
  if (!Array.isArray(descriptor.actions) || descriptor.actions.some((action) => typeof action !== "string" || !action.trim())) {
    throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "actions must be non-empty strings");
  }
  if (new Set(descriptor.actions).size !== descriptor.actions.length) {
    throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "actions must be unique");
  }
}

export function registerActivityTypeSchema(registry, descriptor) {
  assertDescriptor(descriptor);
  if (!(registry instanceof Map)) {
    throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "registry must be a Map");
  }
  if (registry.has(descriptor.type)) {
    throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `activity type '${descriptor.type}' is already registered`);
  }
  registry.set(descriptor.type, Object.freeze({
    type: descriptor.type,
    schemaVersion: descriptor.schemaVersion,
    actions: Object.freeze([...descriptor.actions]),
    configShape: descriptor.configShape || "object"
  }));
  return registry;
}

export function createActivityTypeRegistry(descriptors = ACTIVITY_TYPE_SCHEMAS) {
  const registry = new Map();
  for (const descriptor of descriptors) registerActivityTypeSchema(registry, descriptor);
  return registry;
}

export function resolveActivityTypeSchema(registry, type) {
  const schema = registry instanceof Map ? registry.get(type) : undefined;
  if (!schema) {
    throw new ActivityTypeContractError("ACTIVITY_UNKNOWN_TYPE", `activity type '${type}' is not registered`);
  }
  return schema;
}

export function validateActivityTypeConfig(registry, type, config) {
  const schema = resolveActivityTypeSchema(registry, type);
  if (!config || typeof config !== "object" || Array.isArray(config)) {
    throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "type config must be an object");
  }
  if (config.schema_version !== schema.schemaVersion) {
    throw new ActivityTypeContractError("ACTIVITY_SCHEMA_VERSION_UNSUPPORTED", `schema version is not supported for '${type}'`);
  }
  return schema;
}

export function validateActivityAction(registry, type, action) {
  const schema = resolveActivityTypeSchema(registry, type);
  if (!schema.actions.includes(action)) {
    throw new ActivityTypeContractError("ACTIVITY_UNKNOWN_ACTION", `action '${action}' is not registered for '${type}'`);
  }
  return schema;
}
