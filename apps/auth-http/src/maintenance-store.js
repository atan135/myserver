export const MAINTENANCE_STATE_KEY = "maintenance:global";

function normalizeOptionalString(value) {
  if (typeof value !== "string") {
    return null;
  }

  const normalized = value.trim();
  return normalized.length > 0 ? normalized : null;
}

export function maintenanceStateKey(prefix = "") {
  return `${prefix || ""}${MAINTENANCE_STATE_KEY}`;
}

export function normalizeMaintenanceState(state = {}) {
  return {
    enabled: state.enabled === true,
    reason: normalizeOptionalString(state.reason),
    updatedAt: normalizeOptionalString(state.updatedAt),
    updatedBy: normalizeOptionalString(state.updatedBy)
  };
}

export function parseMaintenanceState(raw) {
  if (!raw) {
    return normalizeMaintenanceState();
  }

  try {
    return normalizeMaintenanceState(JSON.parse(raw));
  } catch {
    return normalizeMaintenanceState();
  }
}

export class MaintenanceStore {
  constructor(redis, config = {}) {
    this.redis = redis;
    this.redisKeyPrefix = config.redisKeyPrefix || "";
  }

  key() {
    return maintenanceStateKey(this.redisKeyPrefix);
  }

  async getStatus() {
    const raw = await this.redis.get(this.key());
    return parseMaintenanceState(raw);
  }
}
