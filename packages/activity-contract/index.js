// Shared admin-api/admin-web contract for activity type schemas. Runtime
// handlers remain in game-server; this module only describes registration and
// validates the versioned shape accepted by the control plane.

export const ACTIVITY_TYPE_SCHEMA_VERSION = 1;

export const ACTIVITY_TYPE_SCHEMAS = Object.freeze([
  Object.freeze({
    type: "login_reward",
    schemaVersion: ACTIVITY_TYPE_SCHEMA_VERSION,
    actions: Object.freeze(["list", "detail", "claim", "progress"]),
    configShape: "login_reward",
    fields: Object.freeze(["schema_version", "event_source", "cycle_unit", "progression", "miss_policy", "claim_mode", "stages"])
  }),
  Object.freeze({
    type: "lottery",
    schemaVersion: ACTIVITY_TYPE_SCHEMA_VERSION,
    actions: Object.freeze(["list", "detail", "draw", "progress"]),
    configShape: "lottery",
    fields: Object.freeze(["schema_version", "draw_source", "pool_version", "free_draw_count", "voucher_item_id", "daily_draw_limit", "total_draw_limit", "pool_items", "pity", "limited_stock"])
  })
]);

export const LOGIN_REWARD_SCHEMA = Object.freeze({
  type: "login_reward",
  schemaVersion: ACTIVITY_TYPE_SCHEMA_VERSION,
  eventSources: Object.freeze(["game_entry"]),
  cycleUnits: Object.freeze(["natural_day"]),
  progressions: Object.freeze(["consecutive", "cumulative"]),
  missPolicies: Object.freeze(["reset", "carry"]),
  claimModes: Object.freeze(["manual", "automatic"])
});

export const LOTTERY_SCHEMA = Object.freeze({
  type: "lottery",
  schemaVersion: ACTIVITY_TYPE_SCHEMA_VERSION,
  drawSources: Object.freeze(["player_action"]),
});

const LOTTERY_U32_MAX = 0xffffffff;
const LOTTERY_I32_MAX = 0x7fffffff;
const LOTTERY_CONFIG_FIELDS = new Set(["schema_version", "draw_source", "pool_version", "free_draw_count", "voucher_item_id", "daily_draw_limit", "total_draw_limit", "pool_items", "pity", "limited_stock"]);
const LOTTERY_POOL_FIELDS = new Set(["item_id", "quantity", "weight"]);
const LOTTERY_EXTENSION_FIELDS = new Set(["enabled", "threshold", "stock"]);

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
  if (type === "login_reward") validateLoginRewardConfig(config);
  if (type === "lottery") validateLotteryConfig(config);
  return schema;
}

export function validateLotteryConfig(config) {
  if (!config || typeof config !== "object" || Array.isArray(config)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "lottery config must be an object");
  for (const field of Object.keys(config)) if (!LOTTERY_CONFIG_FIELDS.has(field)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `lottery config.${field} is not allowed`);
  if (config.schema_version !== LOTTERY_SCHEMA.schemaVersion) throw new ActivityTypeContractError("ACTIVITY_SCHEMA_VERSION_UNSUPPORTED", "lottery schema version is unsupported");
  if (config.draw_source !== "player_action") throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "draw_source must be player_action");
  for (const field of ["pool_version", "free_draw_count", "daily_draw_limit", "total_draw_limit"]) {
    if (!Number.isInteger(config[field]) || config[field] < 0 || config[field] > LOTTERY_U32_MAX || (field === "pool_version" && config[field] < 1)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `${field} must be a uint32`);
  }
  if (config.voucher_item_id !== undefined && (!Number.isInteger(config.voucher_item_id) || config.voucher_item_id < 1 || config.voucher_item_id > LOTTERY_I32_MAX)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "voucher_item_id must be a positive int32");
  if (!Array.isArray(config.pool_items) || config.pool_items.length === 0) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "pool_items must be non-empty");
  const itemIds = new Set();
  let totalWeight = 0;
  for (const [index, item] of config.pool_items.entries()) {
    if (!item || typeof item !== "object" || Array.isArray(item)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `pool_items[${index}] must be an object`);
    for (const field of Object.keys(item)) if (!LOTTERY_POOL_FIELDS.has(field)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `pool_items[${index}].${field} is not allowed`);
    if (!Number.isInteger(item.item_id) || item.item_id < 1 || item.item_id > LOTTERY_I32_MAX || itemIds.has(item.item_id)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `pool_items[${index}].item_id must be a unique positive int32`);
    if (!Number.isInteger(item.quantity) || item.quantity < 1 || item.quantity > LOTTERY_U32_MAX) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `pool_items[${index}].quantity must be a positive uint32`);
    if (!Number.isSafeInteger(item.weight) || item.weight < 1) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `pool_items[${index}].weight must be a positive safe integer`);
    itemIds.add(item.item_id); totalWeight += item.weight;
    if (!Number.isSafeInteger(totalWeight)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "pool item weights exceed safe integer range");
  }
  for (const field of ["pity", "limited_stock"]) if (config[field] !== undefined) {
    if (!config[field] || typeof config[field] !== "object" || Array.isArray(config[field])) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `${field} must be an object when provided`);
    for (const extensionField of Object.keys(config[field])) if (!LOTTERY_EXTENSION_FIELDS.has(extensionField)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `${field}.${extensionField} is not allowed`);
    if ("enabled" in config[field] && typeof config[field].enabled !== "boolean") throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `${field}.enabled must be boolean`);
    for (const extensionField of ["threshold", "stock"]) if (extensionField in config[field] && (!Number.isInteger(config[field][extensionField]) || config[field][extensionField] < 0 || config[field][extensionField] > LOTTERY_U32_MAX)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `${field}.${extensionField} must be uint32`);
  }
  for (const field of [
    "progress",
    "state",
    "result",
    "random_value",
    "draw_request_id",
    "consumed_items",
    "free_draws_remaining",
    "voucher_count",
    "daily_draw_count",
    "total_draw_count",
    "last_draw_period_key",
    "result_item_id",
    "result_state"
  ]) if (field in config) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `${field} is server-owned and cannot be submitted`);
  return config;
}

export function validateLoginRewardConfig(config) {
  if (!config || typeof config !== "object" || Array.isArray(config)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "login_reward config must be an object");
  for (const field of ["event_source", "cycle_unit", "progression", "miss_policy", "claim_mode", "stages"]) {
    if (!(field in config)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `login_reward config requires ${field}`);
  }
  const choices = [["event_source", LOGIN_REWARD_SCHEMA.eventSources], ["cycle_unit", LOGIN_REWARD_SCHEMA.cycleUnits], ["progression", LOGIN_REWARD_SCHEMA.progressions], ["miss_policy", LOGIN_REWARD_SCHEMA.missPolicies], ["claim_mode", LOGIN_REWARD_SCHEMA.claimModes]];
  for (const [field, values] of choices) if (!values.includes(config[field])) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `${field} is not supported for login_reward`);
  if (!Array.isArray(config.stages) || config.stages.length === 0) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", "login_reward stages must be a non-empty array");
  for (const field of ["progress", "state", "last_period_key", "consecutive_count", "cumulative_count", "claimed_stage_ids", "current_stage_id", "today_period_key", "reward_items"]) {
    if (field in config) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `${field} is server-owned and cannot be submitted`);
  }
  const stageNos = new Set();
  for (const [index, stage] of config.stages.entries()) {
    if (!stage || typeof stage !== "object" || Array.isArray(stage)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `stages[${index}] must be an object`);
    if (!Number.isInteger(stage.stage_no) || stage.stage_no < 1 || stageNos.has(stage.stage_no)) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `stages[${index}].stage_no must be unique and positive`);
    stageNos.add(stage.stage_no);
    if (!Number.isInteger(stage.required_count) || stage.required_count < 1) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `stages[${index}].required_count must be positive`);
    if (typeof stage.reward_group_key !== "string" || !stage.reward_group_key.trim()) throw new ActivityTypeContractError("ACTIVITY_INVALID_CONFIG", `stages[${index}].reward_group_key is required`);
  }
  return config;
}

export function validateActivityAction(registry, type, action) {
  const schema = resolveActivityTypeSchema(registry, type);
  if (!schema.actions.includes(action)) {
    throw new ActivityTypeContractError("ACTIVITY_UNKNOWN_ACTION", `action '${action}' is not registered for '${type}'`);
  }
  return schema;
}
