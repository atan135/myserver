import { generateCharacterId } from "./global-id.js";
import { log } from "./logger.js";

const DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT = 6;
const DEFAULT_CHARACTER_NAME_SEARCH_LIMIT = 20;
const MAX_CHARACTER_NAME_SEARCH_LIMIT = 100;
const UNIQUE_VIOLATION = "23505";

function toIsoString(value) {
  if (!value) {
    return null;
  }

  if (value instanceof Date) {
    return value.toISOString();
  }

  return String(value);
}

function toNumericId(value) {
  if (value === null || value === undefined) {
    return value;
  }

  const numeric = Number(value);
  return Number.isSafeInteger(numeric) ? numeric : value;
}

function toJsonb(value) {
  return JSON.stringify(value ?? {});
}

function toJsonValue(value) {
  if (typeof value !== "string") {
    return value ?? {};
  }

  try {
    return JSON.parse(value);
  } catch {
    return {};
  }
}

function createCharacterStoreError(code, message = code, details = {}) {
  const error = new Error(message);
  error.code = code;
  Object.assign(error, details);
  return error;
}

function defaultLog(level, message, extra) {
  try {
    log(level, message, extra);
  } catch {
    // Focused store tests may instantiate CharacterStore without configuring log4js.
  }
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
    accountPlayerId: row.account_player_id,
    worldId: toNumericId(row.world_id),
    name: row.name,
    status: row.status,
    appearance: toJsonValue(row.appearance_json),
    position: {
      sceneId: toNumericId(row.scene_id),
      x: Number(row.x),
      y: Number(row.y),
      dirX: Number(row.dir_x),
      dirY: Number(row.dir_y)
    },
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
    },
    createdAt: toIsoString(row.created_at),
    lastLoginAt: toIsoString(row.last_login_at),
    deletedAt: toIsoString(row.deleted_at)
  };
}

function readTotal(rows) {
  return Number.parseInt(String(rows[0]?.total ?? "0"), 10);
}

function normalizedPosition(position = {}) {
  return {
    sceneId: position.sceneId ?? position.scene_id ?? 0,
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

function normalizePositiveInteger(value, fallback) {
  const numeric = Number.parseInt(String(value), 10);
  return Number.isSafeInteger(numeric) && numeric > 0 ? numeric : fallback;
}

function normalizeBoolean(value, fallback) {
  if (typeof value === "boolean") {
    return value;
  }

  if (typeof value === "string") {
    return value === "true" || value === "1";
  }

  return fallback;
}

function normalizeOptionalString(value) {
  if (typeof value !== "string") {
    return null;
  }

  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
}

function normalizeCreateInput(input) {
  const position = normalizedPosition(input.position);
  const affinity = normalizedElements(input.affinity, {
    earth: 2500,
    fire: 2500,
    water: 2500,
    wind: 2500
  });
  const mastery = normalizedElements(input.mastery, {
    earth: 0,
    fire: 0,
    water: 0,
    wind: 0
  });

  return {
    accountPlayerId: input.accountPlayerId,
    worldId: input.worldId ?? 0,
    name: input.name,
    status: input.status || "active",
    appearance: input.appearance ?? {},
    position,
    affinity,
    mastery
  };
}

function isUniqueViolation(error) {
  return error?.code === UNIQUE_VIOLATION;
}

export class CharacterStore {
  constructor(pool, options = {}) {
    this.pool = pool;
    this.characterIdGenerator = options.characterIdGenerator || generateCharacterId;
    this.logger = options.logger || defaultLog;
    this.maxEffectiveCharactersPerAccount = normalizePositiveInteger(
      options.maxEffectiveCharactersPerAccount ?? options.characterMaxEffectivePerAccount,
      DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT
    );
    this.allowDuplicateNames = normalizeBoolean(
      options.allowDuplicateNames ?? options.characterAllowDuplicateNames,
      true
    );
  }

  get enabled() {
    return Boolean(this.pool);
  }

  async listByAccountPlayerId(accountPlayerId, { includeDeleted = false } = {}) {
    if (!this.enabled) {
      return [];
    }

    const { rows } = await this.pool.query(
      `SELECT ${characterSelectColumns()}
       FROM characters
       WHERE account_player_id = $1
         ${includeDeleted ? "" : "AND deleted_at IS NULL"}
       ORDER BY created_at ASC, character_id ASC`,
      [accountPlayerId]
    );

    return rows.map(toCharacter);
  }

  async getByCharacterId(characterId, { includeDeleted = false } = {}) {
    if (!this.enabled) {
      return null;
    }

    const { rows } = await this.pool.query(
      `SELECT ${characterSelectColumns()}
       FROM characters
       WHERE character_id = $1
         ${includeDeleted ? "" : "AND deleted_at IS NULL"}
       LIMIT 1`,
      [characterId]
    );

    return rows.length > 0 ? toCharacter(rows[0]) : null;
  }

  async searchByCharacterName(name, options = {}) {
    if (!this.enabled) {
      return [];
    }

    const normalizedName = normalizeOptionalString(name);
    if (!normalizedName) {
      return [];
    }

    const {
      accountPlayerId = null,
      worldId = null,
      includeDeleted = false
    } = options;
    const limit = Math.min(
      normalizePositiveInteger(options.limit, DEFAULT_CHARACTER_NAME_SEARCH_LIMIT),
      MAX_CHARACTER_NAME_SEARCH_LIMIT
    );

    const params = [normalizedName];
    const where = ["name = $1"];

    if (accountPlayerId) {
      params.push(accountPlayerId);
      where.push(`account_player_id = $${params.length}`);
    }

    if (worldId !== null && worldId !== undefined) {
      params.push(worldId);
      where.push(`world_id = $${params.length}`);
    }

    if (!includeDeleted) {
      where.push("deleted_at IS NULL");
    }

    params.push(limit);
    const { rows } = await this.pool.query(
      `SELECT ${characterSelectColumns()}
       FROM characters
       WHERE ${where.join(" AND ")}
       ORDER BY world_id ASC, created_at ASC, character_id ASC
       LIMIT $${params.length}`,
      params
    );

    return rows.map(toCharacter);
  }

  async countEffectiveByAccountPlayerId(accountPlayerId) {
    if (!this.enabled) {
      return 0;
    }

    return this.#countEffectiveByAccountPlayerIdOn(this.pool, accountPlayerId);
  }

  async createCharacter(input) {
    if (!this.enabled) {
      throw createCharacterStoreError("CHARACTER_STORE_DISABLED", "character store is disabled");
    }

    return this.#createCharacterWithLimit(input, this.maxEffectiveCharactersPerAccount);
  }

  async createCharacterForAdmin(input, options = {}) {
    if (!this.enabled) {
      throw createCharacterStoreError("CHARACTER_STORE_DISABLED", "character store is disabled");
    }

    const auditOptions = this.#normalizeAdminBypassOptions(input, options);
    const character = await this.#createCharacterBypassingLimit(input);
    const adminAudit = this.#buildAdminBypassAudit(character, auditOptions);

    try {
      this.logger?.("info", "character.admin_bypass_create", adminAudit);
    } catch {
      // Logging failure must not hide that the admin bypass has succeeded.
    }

    return {
      ...character,
      adminAudit
    };
  }

  async softDeleteCharacter(characterId) {
    if (!this.enabled) {
      return false;
    }

    const result = await this.pool.query(
      `UPDATE characters
       SET deleted_at = current_timestamp
       WHERE character_id = $1
         AND deleted_at IS NULL`,
      [characterId]
    );
    return result.rowCount > 0;
  }

  async updateLastLoginAt(characterId) {
    if (!this.enabled) {
      return false;
    }

    const result = await this.pool.query(
      `UPDATE characters
       SET last_login_at = current_timestamp
       WHERE character_id = $1
         AND deleted_at IS NULL`,
      [characterId]
    );
    return result.rowCount > 0;
  }

  async updatePosition(characterId, position) {
    if (!this.enabled) {
      return false;
    }

    const normalized = normalizedPosition(position);
    const result = await this.pool.query(
      `UPDATE characters
       SET scene_id = $2,
           x = $3,
           y = $4,
           dir_x = $5,
           dir_y = $6
       WHERE character_id = $1
         AND deleted_at IS NULL`,
      [
        characterId,
        normalized.sceneId,
        normalized.x,
        normalized.y,
        normalized.dirX,
        normalized.dirY
      ]
    );
    return result.rowCount > 0;
  }

  async #createCharacterWithLimit(input, maxEffectiveCharacters) {
    const client = await this.#checkoutClient();
    const transactional = client !== this.pool;
    let transactionClosed = false;

    try {
      if (transactional) {
        await client.query("BEGIN");
        await client.query("LOCK TABLE characters IN SHARE ROW EXCLUSIVE MODE");
      }

      const accountPlayerId = input.accountPlayerId;
      const current = await this.#countEffectiveByAccountPlayerIdOn(client, accountPlayerId);

      if (current >= maxEffectiveCharacters) {
        if (transactional) {
          await client.query("ROLLBACK");
          transactionClosed = true;
        }
        throw createCharacterStoreError(
          "CHARACTER_LIMIT_EXCEEDED",
          "effective character limit exceeded",
          {
            accountPlayerId,
            current,
            limit: maxEffectiveCharacters
          }
        );
      }

      const character = await this.#insertCharacter(client, input);

      if (transactional) {
        await client.query("COMMIT");
        transactionClosed = true;
      }
      return character;
    } catch (error) {
      if (transactional && !transactionClosed) {
        await client.query("ROLLBACK");
      }
      throw error;
    } finally {
      if (transactional) {
        client.release();
      }
    }
  }

  async #createCharacterBypassingLimit(input) {
    const client = await this.#checkoutClient();
    const transactional = client !== this.pool;
    let transactionClosed = false;

    try {
      if (transactional) {
        await client.query("BEGIN");
        await client.query("LOCK TABLE characters IN SHARE ROW EXCLUSIVE MODE");
      }

      const character = await this.#insertCharacter(client, input);

      if (transactional) {
        await client.query("COMMIT");
        transactionClosed = true;
      }
      return character;
    } catch (error) {
      if (transactional && !transactionClosed) {
        await client.query("ROLLBACK");
      }
      throw error;
    } finally {
      if (transactional) {
        client.release();
      }
    }
  }

  async #checkoutClient() {
    if (typeof this.pool.connect !== "function") {
      return this.pool;
    }

    return this.pool.connect();
  }

  async #countEffectiveByAccountPlayerIdOn(client, accountPlayerId) {
    const { rows } = await client.query(
      `SELECT COUNT(*) AS total
       FROM characters
       WHERE account_player_id = $1
         AND deleted_at IS NULL`,
      [accountPlayerId]
    );

    return readTotal(rows);
  }

  async #insertCharacter(client, input) {
    const normalized = normalizeCreateInput(input);
    await this.#assertDuplicateNameAllowed(client, normalized);
    const characterId = this.#generateCharacterId(normalized);

    try {
      const { rows } = await client.query(
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
          toJsonb(normalized.appearance),
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
      if (isUniqueViolation(error)) {
        throw createCharacterStoreError("CHARACTER_ID_EXISTS", "characterId already exists");
      }
      throw error;
    }
  }

  async #assertDuplicateNameAllowed(client, normalized) {
    if (this.allowDuplicateNames) {
      return;
    }

    const { rows } = await client.query(
      `SELECT character_id
       FROM characters
       WHERE world_id = $1
         AND name = $2
         AND deleted_at IS NULL
       LIMIT 1`,
      [normalized.worldId, normalized.name]
    );

    if (rows.length === 0) {
      return;
    }

    throw createCharacterStoreError("CHARACTER_NAME_DUPLICATE", "character name already exists in world", {
      worldId: normalized.worldId,
      name: normalized.name,
      existingCharacterId: rows[0].character_id
    });
  }

  #normalizeAdminBypassOptions(input, options) {
    if (options?.bypassCharacterLimit !== true) {
      throw createCharacterStoreError(
        "ADMIN_BYPASS_CHARACTER_LIMIT_REQUIRED",
        "admin character creation must explicitly set bypassCharacterLimit=true"
      );
    }

    const adminActor = normalizeOptionalString(options.adminActor ?? options.actor);
    if (!adminActor) {
      throw createCharacterStoreError("ADMIN_AUDIT_ACTOR_REQUIRED", "admin character creation requires adminActor");
    }

    const reason = normalizeOptionalString(options.reason);
    if (!reason) {
      throw createCharacterStoreError("ADMIN_AUDIT_REASON_REQUIRED", "admin character creation requires reason");
    }

    const targetAccountPlayerId = normalizeOptionalString(
      options.targetAccountPlayerId ?? input?.accountPlayerId
    );
    if (!targetAccountPlayerId) {
      throw createCharacterStoreError(
        "ADMIN_AUDIT_TARGET_ACCOUNT_REQUIRED",
        "admin character creation requires target account"
      );
    }

    if (input?.accountPlayerId !== targetAccountPlayerId) {
      throw createCharacterStoreError(
        "ADMIN_AUDIT_TARGET_ACCOUNT_MISMATCH",
        "admin character creation target account must match input account",
        {
          inputAccountPlayerId: input?.accountPlayerId,
          targetAccountPlayerId
        }
      );
    }

    return {
      action: normalizeOptionalString(options.action) || "character.create",
      adminActor,
      reason,
      targetAccountPlayerId,
      bypassCharacterLimit: true
    };
  }

  #buildAdminBypassAudit(character, auditOptions) {
    return {
      auditLogTable: "admin_audit_logs",
      action: auditOptions.action,
      adminActor: auditOptions.adminActor,
      reason: auditOptions.reason,
      targetAccountPlayerId: auditOptions.targetAccountPlayerId,
      generatedCharacterId: character.characterId,
      targetType: "character",
      targetValue: character.characterId,
      characterName: character.name,
      worldId: character.worldId,
      bypassCharacterLimit: auditOptions.bypassCharacterLimit
    };
  }

  #generateCharacterId(normalized) {
    try {
      const characterId = this.characterIdGenerator();
      if (!/^chr_[0-9a-hjkmnp-tv-z]+$/.test(characterId)) {
        throw createCharacterStoreError(
          "INVALID_CHARACTER_ID_FORMAT",
          "generated characterId has invalid format",
          { generatedCharacterId: characterId }
        );
      }
      return characterId;
    } catch (error) {
      const wrapped = createCharacterStoreError(
        "CHARACTER_ID_GENERATION_FAILED",
        "failed to generate characterId",
        {
          cause: error,
          generatorErrorCode: error?.code || null,
          accountPlayerId: normalized.accountPlayerId
        }
      );
      try {
        this.logger?.("error", "character.id_generation_failed", {
          errorCode: wrapped.code,
          generatorErrorCode: wrapped.generatorErrorCode,
          accountPlayerId: wrapped.accountPlayerId,
          message: error?.message || String(error)
        });
      } catch {
        // Logging failure must not replace the explicit store error.
      }
      throw wrapped;
    }
  }
}

export {
  DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT,
  DEFAULT_CHARACTER_NAME_SEARCH_LIMIT,
  MAX_CHARACTER_NAME_SEARCH_LIMIT,
  createCharacterStoreError,
  toCharacter
};
