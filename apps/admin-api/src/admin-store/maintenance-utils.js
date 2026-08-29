import { MAINTENANCE_STATE_KEY } from "./constants.js";
import { normalizeOptionalString } from "./formatters.js";

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
    return null;
  }

  try {
    return normalizeMaintenanceState(JSON.parse(raw));
  } catch {
    return null;
  }
}
