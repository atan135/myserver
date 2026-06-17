import crypto from "node:crypto";
import bcrypt from "bcrypt";

const SALT_ROUNDS = 10;
const MAINTENANCE_STATE_KEY = "maintenance:global";
const UNIQUE_VIOLATION = "23505";

function maintenanceStateKey(prefix = "") {
  return `${prefix || ""}${MAINTENANCE_STATE_KEY}`;
}

function normalizeOptionalString(value) {
  if (typeof value !== "string") {
    return null;
  }

  const normalized = value.trim();
  return normalized.length > 0 ? normalized : null;
}

function normalizeMaintenanceState(state = {}) {
  return {
    enabled: state.enabled === true,
    reason: normalizeOptionalString(state.reason),
    updatedAt: normalizeOptionalString(state.updatedAt),
    updatedBy: normalizeOptionalString(state.updatedBy)
  };
}

function parseMaintenanceState(raw) {
  if (!raw) {
    return null;
  }

  try {
    return normalizeMaintenanceState(JSON.parse(raw));
  } catch {
    return null;
  }
}

function toIsoString(value) {
  if (!value) {
    return null;
  }

  if (value instanceof Date) {
    return value.toISOString();
  }

  return String(value);
}

function hashPassword(password) {
  return bcrypt.hashSync(password, SALT_ROUNDS);
}

function verifyPassword(password, hash) {
  return bcrypt.compareSync(password, hash);
}

function hashToken(token) {
  return crypto.createHash("sha256").update(token).digest("hex");
}

function toJsonb(value) {
  return value ? JSON.stringify(value) : null;
}

function nextParam(params) {
  return `$${params.length}`;
}

function toNumericId(value) {
  if (value === null || value === undefined) {
    return value;
  }
  const numeric = Number(value);
  return Number.isSafeInteger(numeric) ? numeric : value;
}

function toAdmin(row) {
  return {
    id: toNumericId(row.id),
    username: row.username,
    displayName: row.display_name,
    role: row.role,
    status: row.status,
    passwordAlgo: row.password_algo,
    passwordSalt: row.password_salt,
    passwordHash: row.password_hash
  };
}

function toPlayer(row) {
  return {
    player_id: row.player_id,
    guest_id: row.guest_id,
    login_name: row.login_name,
    display_name: row.display_name,
    account_type: row.account_type,
    status: row.status,
    ban_expires_at: toIsoString(row.ban_expires_at),
    banExpiresAt: toIsoString(row.ban_expires_at),
    created_at: toIsoString(row.created_at),
    last_login_at: toIsoString(row.last_login_at)
  };
}

function toIdOrigin(row) {
  return {
    origin_id: toNumericId(row.origin_id),
    origin_key: row.origin_key,
    created_at: toIsoString(row.created_at),
    retired_at: toIsoString(row.retired_at)
  };
}

function toWorld(row) {
  return {
    world_id: toNumericId(row.world_id),
    world_key: row.world_key,
    active_origin_id: toNumericId(row.active_origin_id),
    active_origin_key: row.active_origin_key || null,
    origins: Array.isArray(row.origins) ? row.origins.map((origin) => ({
      origin_id: toNumericId(origin.origin_id),
      origin_key: origin.origin_key || null
    })) : [],
    created_at: toIsoString(row.created_at),
    retired_at: toIsoString(row.retired_at)
  };
}

function toWorldMembership(row) {
  return {
    world_id: toNumericId(row.world_id),
    world_key: row.world_key || null,
    origin_id: toNumericId(row.origin_id),
    origin_key: row.origin_key || null,
    active_origin_id: toNumericId(row.active_origin_id),
    active_origin_key: row.active_origin_key || null,
    joined_at: toIsoString(row.joined_at),
    left_at: toIsoString(row.left_at)
  };
}

function toWorldMergeEvent(row) {
  return {
    merge_id: toNumericId(row.merge_id),
    target_world_id: toNumericId(row.target_world_id),
    target_world_key: row.target_world_key || null,
    active_origin_id: toNumericId(row.active_origin_id),
    active_origin_key: row.active_origin_key || null,
    source_world_ids: Array.isArray(row.source_world_ids) ? row.source_world_ids.map(toNumericId) : [],
    source_world_keys: Array.isArray(row.source_world_keys) ? row.source_world_keys : [],
    source_origin_ids: Array.isArray(row.source_origin_ids) ? row.source_origin_ids.map(toNumericId) : [],
    source_origin_keys: Array.isArray(row.source_origin_keys) ? row.source_origin_keys : [],
    merged_at: toIsoString(row.merged_at),
    operator: row.operator || null,
    details_json: row.details_json || null
  };
}

function readTotal(rows) {
  return Number.parseInt(String(rows[0]?.total ?? "0"), 10);
}

export class AdminStore {
  constructor(pool, redis = null, config = {}) {
    this.pool = pool;
    this.redis = redis;
    this.redisKeyPrefix = config.redisKeyPrefix || "";
  }

  prefixedKey(key) {
    return `${this.redisKeyPrefix}${key}`;
  }

  maintenanceStateKey() {
    return maintenanceStateKey(this.redisKeyPrefix);
  }

  async findAdminByUsername(username) {
    const { rows } = await this.pool.query(
      `SELECT id, username, display_name, password_algo, password_salt, password_hash, role, status
       FROM admin_accounts
       WHERE username = $1
       LIMIT 1`,
      [username]
    );

    if (rows.length === 0) return null;

    return toAdmin(rows[0]);
  }

  async findAdminById(adminId) {
    const { rows } = await this.pool.query(
      `SELECT id, username, display_name, password_algo, password_salt, password_hash, role, status
       FROM admin_accounts
       WHERE id = $1
       LIMIT 1`,
      [adminId]
    );

    return rows.length > 0 ? toAdmin(rows[0]) : null;
  }

  async verifyPassword(password, hash) {
    return verifyPassword(password, hash);
  }

  async createAdmin({ username, displayName, password, role = "viewer" }) {
    const passwordSalt = crypto.randomBytes(16).toString("hex");
    const passwordHash = hashPassword(password);

    try {
      const { rows } = await this.pool.query(
        `INSERT INTO admin_accounts (username, display_name, password_algo, password_salt, password_hash, role, status)
         VALUES ($1, $2, 'bcrypt', $3, $4, $5, 'active')
         RETURNING id`,
        [username, displayName || username, passwordSalt, passwordHash, role]
      );

      return {
        id: toNumericId(rows[0].id),
        username,
        displayName: displayName || username,
        role
      };
    } catch (error) {
      if (error.code === UNIQUE_VIOLATION) {
        throw new Error("ADMIN_ALREADY_EXISTS");
      }
      throw error;
    }
  }

  async ensureInitialAdmin(config) {
    const existing = await this.findAdminByUsername(config.initialAdminUsername);
    if (existing) {
      return existing;
    }

    return this.createAdmin({
      username: config.initialAdminUsername,
      displayName: config.initialAdminDisplayName,
      password: config.initialAdminPassword,
      role: "admin"
    });
  }

  async updateLastLogin(adminId) {
    await this.pool.query(
      `UPDATE admin_accounts SET last_login_at = current_timestamp WHERE id = $1`,
      [adminId]
    );
  }

  async updateAdminPassword(adminId, password) {
    const passwordSalt = crypto.randomBytes(16).toString("hex");
    const passwordHash = hashPassword(password);
    const result = await this.pool.query(
      `UPDATE admin_accounts
       SET password_algo = 'bcrypt',
           password_salt = $1,
           password_hash = $2
       WHERE id = $3`,
      [passwordSalt, passwordHash, adminId]
    );

    return result.rowCount > 0;
  }

  async appendAuditLog({ adminId, adminUsername, action, targetType, targetValue, details, ip }) {
    await this.pool.query(
      `INSERT INTO admin_audit_logs (admin_id, admin_username, action, target_type, target_value, details_json, ip)
       VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7)`,
      [
        adminId,
        adminUsername,
        action,
        targetType || null,
        targetValue || null,
        toJsonb(details),
        ip || null
      ]
    );
  }

  async appendSecurityAuditLog({
    eventType,
    targetType,
    targetValue,
    severity = "warning",
    clientIp,
    details
  }) {
    await this.pool.query(
      `INSERT INTO security_audit_logs (event_type, target_type, target_value, severity, client_ip, details_json)
       VALUES ($1, $2, $3, $4, $5, $6::jsonb)`,
      [
        eventType,
        targetType || null,
        targetValue || null,
        severity,
        clientIp || null,
        toJsonb(details)
      ]
    );
  }

  async getSecurityLogs({ limit = 50, offset = 0, eventType, targetType, severity, clientIp } = {}) {
    let query = `SELECT * FROM security_audit_logs WHERE 1=1`;
    const params = [];

    if (eventType) {
      params.push(eventType);
      query += ` AND event_type = ${nextParam(params)}`;
    }

    if (targetType) {
      params.push(targetType);
      query += ` AND target_type = ${nextParam(params)}`;
    }

    if (severity) {
      params.push(severity);
      query += ` AND severity = ${nextParam(params)}`;
    }

    if (clientIp) {
      params.push(clientIp);
      query += ` AND client_ip = ${nextParam(params)}`;
    }

    params.push(limit);
    query += ` ORDER BY created_at DESC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows;
  }

  async countSecurityLogs({ eventType, targetType, severity, clientIp } = {}) {
    let query = `SELECT COUNT(*) as total FROM security_audit_logs WHERE 1=1`;
    const params = [];

    if (eventType) {
      params.push(eventType);
      query += ` AND event_type = ${nextParam(params)}`;
    }

    if (targetType) {
      params.push(targetType);
      query += ` AND target_type = ${nextParam(params)}`;
    }

    if (severity) {
      params.push(severity);
      query += ` AND severity = ${nextParam(params)}`;
    }

    if (clientIp) {
      params.push(clientIp);
      query += ` AND client_ip = ${nextParam(params)}`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  async getAuditLogs({ limit = 50, offset = 0, adminId, action, targetType } = {}) {
    let query = `SELECT * FROM admin_audit_logs WHERE 1=1`;
    const params = [];

    if (adminId) {
      params.push(adminId);
      query += ` AND admin_id = ${nextParam(params)}`;
    }

    if (action) {
      params.push(action);
      query += ` AND action = ${nextParam(params)}`;
    }

    if (targetType) {
      params.push(targetType);
      query += ` AND target_type = ${nextParam(params)}`;
    }

    params.push(limit);
    query += ` ORDER BY created_at DESC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows;
  }

  async countAuditLogs({ adminId, action, targetType } = {}) {
    let query = `SELECT COUNT(*) as total FROM admin_audit_logs WHERE 1=1`;
    const params = [];

    if (adminId) {
      params.push(adminId);
      query += ` AND admin_id = ${nextParam(params)}`;
    }

    if (action) {
      params.push(action);
      query += ` AND action = ${nextParam(params)}`;
    }

    if (targetType) {
      params.push(targetType);
      query += ` AND target_type = ${nextParam(params)}`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  // ============================================================
  // Player Management (read from player_accounts)
  // ============================================================

  async findPlayerById(playerId) {
    const { rows } = await this.pool.query(
      `SELECT player_id, guest_id, login_name, display_name, account_type, status, ban_expires_at, created_at, last_login_at
       FROM player_accounts
       WHERE player_id = $1
       LIMIT 1`,
      [playerId]
    );
    return rows.length > 0 ? toPlayer(rows[0]) : null;
  }

  async findPlayers({ loginName, guestId, status, limit = 50, offset = 0 } = {}) {
    let query = `SELECT player_id, guest_id, login_name, display_name, account_type, status, ban_expires_at, created_at, last_login_at
       FROM player_accounts
       WHERE 1=1`;
    const params = [];

    if (loginName) {
      params.push(`%${loginName}%`);
      query += ` AND login_name LIKE ${nextParam(params)}`;
    }

    if (guestId) {
      params.push(`%${guestId}%`);
      query += ` AND guest_id LIKE ${nextParam(params)}`;
    }

    if (status) {
      params.push(status);
      query += ` AND status = ${nextParam(params)}`;
    }

    params.push(limit);
    query += ` ORDER BY last_login_at DESC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows.map(toPlayer);
  }

  async countPlayers({ loginName, guestId, status } = {}) {
    let query = `SELECT COUNT(*) as total FROM player_accounts WHERE 1=1`;
    const params = [];

    if (loginName) {
      params.push(`%${loginName}%`);
      query += ` AND login_name LIKE ${nextParam(params)}`;
    }

    if (guestId) {
      params.push(`%${guestId}%`);
      query += ` AND guest_id LIKE ${nextParam(params)}`;
    }

    if (status) {
      params.push(status);
      query += ` AND status = ${nextParam(params)}`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  async updatePlayerStatus(playerId, status, { banExpiresAt = undefined } = {}) {
    const nextBanExpiresAt = status === "banned" ? banExpiresAt ?? null : null;
    const result = await this.pool.query(
      `UPDATE player_accounts SET status = $1, ban_expires_at = $2 WHERE player_id = $3`,
      [status, nextBanExpiresAt, playerId]
    );
    return result.rowCount > 0;
  }

  // ============================================================
  // Global ID metadata queries
  // ============================================================

  async findIdOrigin(originId) {
    const { rows } = await this.pool.query(
      `SELECT origin_id, origin_key, created_at, retired_at
       FROM id_origins
       WHERE origin_id = $1
       LIMIT 1`,
      [originId]
    );
    return rows.length > 0 ? toIdOrigin(rows[0]) : null;
  }

  async findWorldMembershipAt({ originId, createdAt }) {
    const { rows } = await this.pool.query(
      `SELECT
         wom.world_id,
         w.world_key,
         wom.origin_id,
         io.origin_key,
         w.active_origin_id,
         active_origin.origin_key AS active_origin_key,
         wom.joined_at,
         wom.left_at
       FROM world_origin_memberships wom
       LEFT JOIN worlds w ON w.world_id = wom.world_id
       LEFT JOIN id_origins io ON io.origin_id = wom.origin_id
       LEFT JOIN id_origins active_origin ON active_origin.origin_id = w.active_origin_id
       WHERE wom.origin_id = $1
         AND wom.joined_at <= $2
         AND (wom.left_at IS NULL OR wom.left_at > $2)
       ORDER BY wom.joined_at DESC
       LIMIT 1`,
      [originId, createdAt]
    );
    return rows.length > 0 ? toWorldMembership(rows[0]) : null;
  }

  async findCurrentWorldMembership(originId) {
    const { rows } = await this.pool.query(
      `SELECT
         wom.world_id,
         w.world_key,
         wom.origin_id,
         io.origin_key,
         w.active_origin_id,
         active_origin.origin_key AS active_origin_key,
         wom.joined_at,
         wom.left_at
       FROM world_origin_memberships wom
       LEFT JOIN worlds w ON w.world_id = wom.world_id
       LEFT JOIN id_origins io ON io.origin_id = wom.origin_id
       LEFT JOIN id_origins active_origin ON active_origin.origin_id = w.active_origin_id
       WHERE wom.origin_id = $1
         AND wom.left_at IS NULL
       ORDER BY wom.joined_at DESC
       LIMIT 1`,
      [originId]
    );
    return rows.length > 0 ? toWorldMembership(rows[0]) : null;
  }

  async findMergeContext({ originId, createdAt, worldId = null }) {
    const params = [originId, createdAt];
    let query = `SELECT
         wme.merge_id,
         wme.target_world_id,
         target_world.world_key AS target_world_key,
         wme.active_origin_id,
         active_origin.origin_key AS active_origin_key,
         wme.source_world_ids,
         (
           SELECT array_agg(source_world.world_key ORDER BY source_world_ref.ordinality)
           FROM unnest(wme.source_world_ids) WITH ORDINALITY AS source_world_ref(world_id, ordinality)
           LEFT JOIN worlds source_world ON source_world.world_id = source_world_ref.world_id
         ) AS source_world_keys,
         wme.source_origin_ids,
         (
           SELECT array_agg(source_origin.origin_key ORDER BY source_origin_ref.ordinality)
           FROM unnest(wme.source_origin_ids) WITH ORDINALITY AS source_origin_ref(origin_id, ordinality)
           LEFT JOIN id_origins source_origin ON source_origin.origin_id = source_origin_ref.origin_id
         ) AS source_origin_keys,
         wme.merged_at,
         wme.operator,
         wme.details_json
       FROM world_merge_events wme
       LEFT JOIN worlds target_world ON target_world.world_id = wme.target_world_id
       LEFT JOIN id_origins active_origin ON active_origin.origin_id = wme.active_origin_id
       WHERE $1 = ANY(wme.source_origin_ids)
         AND wme.merged_at >= $2`;

    if (worldId !== null && worldId !== undefined) {
      params.push(worldId);
      const placeholder = nextParam(params);
      query += ` AND (wme.target_world_id = ${placeholder} OR ${placeholder} = ANY(wme.source_world_ids))`;
    }

    query += ` ORDER BY wme.merged_at ASC LIMIT 1`;

    const { rows } = await this.pool.query(query, params);
    return rows.length > 0 ? toWorldMergeEvent(rows[0]) : null;
  }

  async findIdOrigins({ originId, originKey, limit = 50, offset = 0 } = {}) {
    let query = `SELECT origin_id, origin_key, created_at, retired_at
       FROM id_origins
       WHERE 1=1`;
    const params = [];

    if (originId !== undefined && originId !== null) {
      params.push(originId);
      query += ` AND origin_id = ${nextParam(params)}`;
    }

    if (originKey) {
      params.push(`%${originKey}%`);
      query += ` AND origin_key LIKE ${nextParam(params)}`;
    }

    params.push(limit);
    query += ` ORDER BY origin_id ASC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows.map(toIdOrigin);
  }

  async countIdOrigins({ originId, originKey } = {}) {
    let query = `SELECT COUNT(*) as total FROM id_origins WHERE 1=1`;
    const params = [];

    if (originId !== undefined && originId !== null) {
      params.push(originId);
      query += ` AND origin_id = ${nextParam(params)}`;
    }

    if (originKey) {
      params.push(`%${originKey}%`);
      query += ` AND origin_key LIKE ${nextParam(params)}`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  async findWorlds({ worldId, worldKey, originId, limit = 50, offset = 0 } = {}) {
    let query = `SELECT
         w.world_id,
         w.world_key,
         w.active_origin_id,
         active_origin.origin_key AS active_origin_key,
         COALESCE(
           jsonb_agg(
             DISTINCT jsonb_build_object(
               'origin_id', wom.origin_id,
               'origin_key', member_origin.origin_key
             )
           ) FILTER (WHERE wom.origin_id IS NOT NULL),
           '[]'::jsonb
         ) AS origins,
         w.created_at,
         w.retired_at
       FROM worlds w
       LEFT JOIN id_origins active_origin ON active_origin.origin_id = w.active_origin_id
       LEFT JOIN world_origin_memberships wom ON wom.world_id = w.world_id
       LEFT JOIN id_origins member_origin ON member_origin.origin_id = wom.origin_id
       WHERE 1=1`;
    const params = [];

    if (worldId !== undefined && worldId !== null) {
      params.push(worldId);
      query += ` AND w.world_id = ${nextParam(params)}`;
    }

    if (worldKey) {
      params.push(`%${worldKey}%`);
      query += ` AND w.world_key LIKE ${nextParam(params)}`;
    }

    if (originId !== undefined && originId !== null) {
      params.push(originId);
      const placeholder = nextParam(params);
      query += ` AND (w.active_origin_id = ${placeholder} OR EXISTS (
        SELECT 1 FROM world_origin_memberships filter_wom
        WHERE filter_wom.world_id = w.world_id AND filter_wom.origin_id = ${placeholder}
      ))`;
    }

    query += ` GROUP BY w.world_id, w.world_key, w.active_origin_id, active_origin.origin_key, w.created_at, w.retired_at`;
    params.push(limit);
    query += ` ORDER BY w.world_id ASC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows.map(toWorld);
  }

  async countWorlds({ worldId, worldKey, originId } = {}) {
    let query = `SELECT COUNT(*) as total FROM worlds w WHERE 1=1`;
    const params = [];

    if (worldId !== undefined && worldId !== null) {
      params.push(worldId);
      query += ` AND w.world_id = ${nextParam(params)}`;
    }

    if (worldKey) {
      params.push(`%${worldKey}%`);
      query += ` AND w.world_key LIKE ${nextParam(params)}`;
    }

    if (originId !== undefined && originId !== null) {
      params.push(originId);
      const placeholder = nextParam(params);
      query += ` AND (w.active_origin_id = ${placeholder} OR EXISTS (
        SELECT 1 FROM world_origin_memberships filter_wom
        WHERE filter_wom.world_id = w.world_id AND filter_wom.origin_id = ${placeholder}
      ))`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  async findWorldMergeEvents({ worldId, originId, limit = 50, offset = 0 } = {}) {
    let query = `SELECT
         wme.merge_id,
         wme.target_world_id,
         target_world.world_key AS target_world_key,
         wme.active_origin_id,
         active_origin.origin_key AS active_origin_key,
         wme.source_world_ids,
         (
           SELECT array_agg(source_world.world_key ORDER BY source_world_ref.ordinality)
           FROM unnest(wme.source_world_ids) WITH ORDINALITY AS source_world_ref(world_id, ordinality)
           LEFT JOIN worlds source_world ON source_world.world_id = source_world_ref.world_id
         ) AS source_world_keys,
         wme.source_origin_ids,
         (
           SELECT array_agg(source_origin.origin_key ORDER BY source_origin_ref.ordinality)
           FROM unnest(wme.source_origin_ids) WITH ORDINALITY AS source_origin_ref(origin_id, ordinality)
           LEFT JOIN id_origins source_origin ON source_origin.origin_id = source_origin_ref.origin_id
         ) AS source_origin_keys,
         wme.merged_at,
         wme.operator,
         wme.details_json
       FROM world_merge_events wme
       LEFT JOIN worlds target_world ON target_world.world_id = wme.target_world_id
       LEFT JOIN id_origins active_origin ON active_origin.origin_id = wme.active_origin_id
       WHERE 1=1`;
    const params = [];

    if (worldId !== undefined && worldId !== null) {
      params.push(worldId);
      const placeholder = nextParam(params);
      query += ` AND (wme.target_world_id = ${placeholder} OR ${placeholder} = ANY(wme.source_world_ids))`;
    }

    if (originId !== undefined && originId !== null) {
      params.push(originId);
      const placeholder = nextParam(params);
      query += ` AND (wme.active_origin_id = ${placeholder} OR ${placeholder} = ANY(wme.source_origin_ids))`;
    }

    params.push(limit);
    query += ` ORDER BY wme.merged_at DESC LIMIT ${nextParam(params)}`;
    params.push(offset);
    query += ` OFFSET ${nextParam(params)}`;

    const { rows } = await this.pool.query(query, params);
    return rows.map(toWorldMergeEvent);
  }

  async countWorldMergeEvents({ worldId, originId } = {}) {
    let query = `SELECT COUNT(*) as total FROM world_merge_events wme WHERE 1=1`;
    const params = [];

    if (worldId !== undefined && worldId !== null) {
      params.push(worldId);
      const placeholder = nextParam(params);
      query += ` AND (wme.target_world_id = ${placeholder} OR ${placeholder} = ANY(wme.source_world_ids))`;
    }

    if (originId !== undefined && originId !== null) {
      params.push(originId);
      const placeholder = nextParam(params);
      query += ` AND (wme.active_origin_id = ${placeholder} OR ${placeholder} = ANY(wme.source_origin_ids))`;
    }

    const { rows } = await this.pool.query(query, params);
    return readTotal(rows);
  }

  // ============================================================
  // Maintenance Mode
  // ============================================================

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

export {
  MAINTENANCE_STATE_KEY,
  hashPassword,
  maintenanceStateKey,
  normalizeMaintenanceState,
  parseMaintenanceState,
  verifyPassword,
  hashToken
};
