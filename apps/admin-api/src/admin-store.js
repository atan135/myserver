import crypto from "node:crypto";
import bcrypt from "bcrypt";

import { createGlobalIdGeneratorFromEnv } from "../../../packages/global-id/node/index.js";

const SALT_ROUNDS = 10;
const MAINTENANCE_STATE_KEY = "maintenance:global";
const UNIQUE_VIOLATION = "23505";
const CHARACTER_ID_PATTERN = /^chr_[0-9a-hjkmnp-tv-z]+$/;

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

function toRequiredJsonb(value) {
  return JSON.stringify(value ?? {});
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

function characterSelectColumns() {
  return `character_id,
          account_player_id,
          world_id,
          name,
          status,
          appearance_json,
          scene_id,
          x,
          y,
          dir_x,
          dir_y,
          affinity_earth,
          affinity_fire,
          affinity_water,
          affinity_wind,
          mastery_earth,
          mastery_fire,
          mastery_water,
          mastery_wind,
          created_at,
          last_login_at,
          deleted_at`;
}

function toCharacter(row) {
  return {
    characterId: row.character_id,
    character_id: row.character_id,
    accountPlayerId: row.account_player_id,
    account_player_id: row.account_player_id,
    worldId: toNumericId(row.world_id),
    world_id: toNumericId(row.world_id),
    name: row.name,
    status: row.status,
    appearance: normalizeJson(row.appearance_json) || {},
    appearance_json: normalizeJson(row.appearance_json) || {},
    position: {
      sceneId: toNumericId(row.scene_id),
      scene_id: toNumericId(row.scene_id),
      x: Number(row.x),
      y: Number(row.y),
      dirX: Number(row.dir_x),
      dir_x: Number(row.dir_x),
      dirY: Number(row.dir_y),
      dir_y: Number(row.dir_y)
    },
    attributes: {
      affinity: {
        earth: Number(row.affinity_earth),
        fire: Number(row.affinity_fire),
        water: Number(row.affinity_water),
        wind: Number(row.affinity_wind)
      },
      mastery: {
        earth: Number(row.mastery_earth),
        fire: Number(row.mastery_fire),
        water: Number(row.mastery_water),
        wind: Number(row.mastery_wind)
      }
    },
    createdAt: toIsoString(row.created_at),
    created_at: toIsoString(row.created_at),
    lastLoginAt: toIsoString(row.last_login_at),
    last_login_at: toIsoString(row.last_login_at),
    deletedAt: toIsoString(row.deleted_at),
    deleted_at: toIsoString(row.deleted_at)
  };
}

function normalizeJson(value) {
  if (value === undefined || value === null) {
    return null;
  }

  if (typeof value !== "string") {
    return value;
  }

  try {
    return JSON.parse(value);
  } catch {
    return value;
  }
}

function createAdminStoreError(code, message = code, details = {}) {
  const error = new Error(message);
  error.code = code;
  Object.assign(error, details);
  return error;
}

function normalizedPosition(position = {}) {
  return {
    sceneId: position.sceneId ?? position.scene_id ?? 100,
    x: position.x ?? 0,
    y: position.y ?? 0,
    dirX: position.dirX ?? position.dir_x ?? 0,
    dirY: position.dirY ?? position.dir_y ?? 1
  };
}

function normalizedElements(elements = {}, defaults) {
  return {
    earth: elements.earth ?? defaults.earth,
    fire: elements.fire ?? defaults.fire,
    water: elements.water ?? defaults.water,
    wind: elements.wind ?? defaults.wind
  };
}

function normalizeCharacterCreateInput(input = {}) {
  return {
    accountPlayerId: input.accountPlayerId,
    worldId: input.worldId ?? input.world_id ?? 0,
    name: input.name,
    status: input.status || "active",
    appearance: input.appearance ?? input.appearance_json ?? {},
    position: normalizedPosition(input.position),
    affinity: normalizedElements(input.affinity, {
      earth: 2500,
      fire: 2500,
      water: 2500,
      wind: 2500
    }),
    mastery: normalizedElements(input.mastery, {
      earth: 0,
      fire: 0,
      water: 0,
      wind: 0
    })
  };
}

function toCharacterTitle(row) {
  const operator = row.latest_operator_type || row.latest_operator_id
    ? {
        type: row.latest_operator_type || null,
        id: row.latest_operator_id || null
      }
    : null;

  return {
    character_id: row.character_id,
    title_id: row.title_id,
    source_type: row.source_type,
    source_id: row.source_id,
    is_equipped: row.is_equipped === true,
    unlocked_at: toIsoString(row.unlocked_at),
    expires_at: toIsoString(row.expires_at),
    expired: row.expired === true,
    created_at: toIsoString(row.created_at),
    updated_at: toIsoString(row.updated_at),
    operator_type: row.latest_operator_type || null,
    operator_id: row.latest_operator_id || null,
    operator,
    latest_log: row.latest_action ? {
      action: row.latest_action,
      operator_type: row.latest_operator_type || null,
      operator_id: row.latest_operator_id || null,
      operator,
      reason: row.latest_reason || null,
      created_at: toIsoString(row.latest_created_at)
    } : null
  };
}

function toCharacterDiscipline(row) {
  return {
    discipline_id: row.discipline_id,
    points: toNumericId(row.points),
    tier: row.tier,
    active: row.active === true,
    learned_at: toIsoString(row.learned_at),
    updated_at: toIsoString(row.updated_at)
  };
}

function toCharacterTitleLog(row) {
  const operator = row.operator_type || row.operator_id
    ? {
        type: row.operator_type || null,
        id: row.operator_id || null
      }
    : null;

  return {
    id: toNumericId(row.id),
    character_id: row.character_id,
    title_id: row.title_id,
    action: row.action,
    source_type: row.source_type || null,
    source_id: row.source_id || null,
    operator_type: row.operator_type || null,
    operator_id: row.operator_id || null,
    operator,
    before_json: normalizeJson(row.before_json),
    after_json: normalizeJson(row.after_json),
    reason: row.reason || null,
    created_at: toIsoString(row.created_at)
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
  constructor(pool, redis = null, config = {}, gamePool = null) {
    this.pool = pool;
    this.gamePool = gamePool || pool;
    this.redis = redis;
    this.redisKeyPrefix = config.redisKeyPrefix || "";
    this.characterIdGenerator = config.characterIdGenerator || createGlobalIdGeneratorFromEnv({ prefix: "chr" });
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

  async findCharacterById(characterId, { includeDeleted = true } = {}) {
    const { rows } = await this.gamePool.query(
      `SELECT ${characterSelectColumns()}
       FROM characters
       WHERE character_id = $1
         ${includeDeleted ? "" : "AND deleted_at IS NULL"}
       LIMIT 1`,
      [characterId]
    );

    return rows.length > 0 ? toCharacter(rows[0]) : null;
  }

  async createCharacterForAdmin(input) {
    const normalized = normalizeCharacterCreateInput(input);
    const characterId = this.generateCharacterId();

    if (!CHARACTER_ID_PATTERN.test(characterId)) {
      throw createAdminStoreError("CHARACTER_ID_GENERATION_FAILED", "generated characterId has invalid format", {
        generatedCharacterId: characterId
      });
    }

    try {
      const { rows } = await this.gamePool.query(
        `INSERT INTO characters (
           character_id,
           account_player_id,
           world_id,
           name,
           status,
           appearance_json,
           scene_id,
           x,
           y,
           dir_x,
           dir_y,
           affinity_earth,
           affinity_fire,
           affinity_water,
           affinity_wind,
           mastery_earth,
           mastery_fire,
           mastery_water,
           mastery_wind
         ) VALUES (
           $1, $2, $3, $4, $5, $6::jsonb, $7, $8, $9, $10,
           $11, $12, $13, $14, $15, $16, $17, $18, $19
         )
         RETURNING ${characterSelectColumns()}`,
        [
          characterId,
          normalized.accountPlayerId,
          normalized.worldId,
          normalized.name,
          normalized.status,
          toRequiredJsonb(normalized.appearance),
          normalized.position.sceneId,
          normalized.position.x,
          normalized.position.y,
          normalized.position.dirX,
          normalized.position.dirY,
          normalized.affinity.earth,
          normalized.affinity.fire,
          normalized.affinity.water,
          normalized.affinity.wind,
          normalized.mastery.earth,
          normalized.mastery.fire,
          normalized.mastery.water,
          normalized.mastery.wind
        ]
      );

      return toCharacter(rows[0]);
    } catch (error) {
      if (error?.code === UNIQUE_VIOLATION) {
        throw createAdminStoreError("CHARACTER_ID_EXISTS", "characterId already exists");
      }
      throw error;
    }
  }

  async restoreCharacterForAdmin(characterId) {
    const { rows } = await this.gamePool.query(
      `UPDATE characters
       SET status = 'active',
           deleted_at = NULL
       WHERE character_id = $1
         AND deleted_at IS NOT NULL
         AND status = 'deleted'
       RETURNING ${characterSelectColumns()}`,
      [characterId]
    );

    return rows.length > 0 ? toCharacter(rows[0]) : null;
  }

  generateCharacterId() {
    if (typeof this.characterIdGenerator === "function") {
      return this.characterIdGenerator();
    }

    return this.characterIdGenerator.generateString("chr");
  }

  async findCharacterTitleOverview({ characterId, logLimit = 20 } = {}) {
    const [titleResult, disciplineResult, logResult] = await Promise.all([
      this.gamePool.query(
        `SELECT
           ct.character_id,
           ct.title_id,
           ct.source_type,
           ct.source_id,
           ct.is_equipped,
           ct.unlocked_at,
           ct.expires_at,
           ct.created_at,
           ct.updated_at,
           (ct.expires_at IS NOT NULL AND ct.expires_at <= current_timestamp) AS expired,
           latest_log.action AS latest_action,
           latest_log.operator_type AS latest_operator_type,
           latest_log.operator_id AS latest_operator_id,
           latest_log.reason AS latest_reason,
           latest_log.created_at AS latest_created_at
         FROM character_titles ct
         LEFT JOIN LATERAL (
           SELECT action, operator_type, operator_id, reason, created_at
           FROM character_title_logs ctl
           WHERE ctl.character_id = ct.character_id
             AND ctl.title_id = ct.title_id
           ORDER BY ctl.created_at DESC, ctl.id DESC
           LIMIT 1
         ) latest_log ON true
         WHERE ct.character_id = $1
         ORDER BY ct.is_equipped DESC, expired ASC, ct.unlocked_at DESC, ct.title_id ASC`,
        [characterId]
      ),
      this.gamePool.query(
        `SELECT discipline_id, points, tier, active, learned_at, updated_at
         FROM character_disciplines
         WHERE character_id = $1
         ORDER BY active DESC, updated_at DESC, discipline_id ASC`,
        [characterId]
      ),
      this.gamePool.query(
        `SELECT id, character_id, title_id, action, source_type, source_id, operator_type, operator_id,
                before_json, after_json, reason, created_at
         FROM character_title_logs
         WHERE character_id = $1
         ORDER BY created_at DESC, id DESC
         LIMIT $2`,
        [characterId, logLimit]
      )
    ]);

    const titles = titleResult.rows.map(toCharacterTitle);
    return {
      titles,
      equippedTitle: titles.find((title) => title.is_equipped && !title.expired) || null,
      disciplines: disciplineResult.rows.map(toCharacterDiscipline),
      titleLogs: logResult.rows.map(toCharacterTitleLog)
    };
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
