import { maintenanceStateKey, normalizeMaintenanceState, parseMaintenanceState } from "../maintenance-utils.js";
import { toIsoString } from "../formatters.js";

export class MaintenanceStore {
  constructor(pool, redis = null, redisKeyPrefix = "") {
    this.pool = pool;
    this.redis = redis;
    this.redisKeyPrefix = redisKeyPrefix;
  }

  prefixedKey(key) {
    return `${this.redisKeyPrefix}${key}`;
  }

  maintenanceStateKey() {
    return maintenanceStateKey(this.redisKeyPrefix);
  }

  async setMaintenanceMode(enabled, { reason = null, updatedAt = null, updatedBy = null } = {}) {
    if (!this.redis) {
      throw new Error("MAINTENANCE_REDIS_UNAVAILABLE");
    }

    const state = normalizeMaintenanceState({
      enabled,
      reason,
      updatedAt: updatedAt || new Date().toISOString(),
      updatedBy
    });
    await this.redis.set(this.maintenanceStateKey(), JSON.stringify(state));
    return state;
  }

  async getMaintenanceStatus() {
    if (this.redis) {
      const raw = await this.redis.get(this.maintenanceStateKey());
      const state = parseMaintenanceState(raw);
      if (state) {
        return state;
      }
    }

    const { rows } = await this.pool.query(
      `SELECT action, admin_username, details_json, created_at
       FROM admin_audit_logs
       WHERE action IN ('maintenance_enabled', 'maintenance_disabled')
       ORDER BY created_at DESC
       LIMIT 1`
    );
    if (rows.length === 0) {
      return normalizeMaintenanceState();
    }
    const latest = rows[0];
    let details = {};
    try {
      details = typeof latest.details_json === "string"
        ? JSON.parse(latest.details_json)
        : latest.details_json || {};
    } catch {
      details = {};
    }

    return normalizeMaintenanceState({
      enabled: latest.action === "maintenance_enabled",
      reason: details.reason || null,
      updatedAt: toIsoString(latest.created_at),
      updatedBy: latest.admin_username || null
    });
  }
}

