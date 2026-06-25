const DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT = 6;
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
    characterId: input.characterId,
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
  constructor(pool) {
    this.pool = pool;
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

    return this.#createCharacterWithLimit(input, DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT);
  }

  async createCharacterForAdmin(input) {
    if (!this.enabled) {
      throw createCharacterStoreError("CHARACTER_STORE_DISABLED", "character store is disabled");
    }

    return this.#insertCharacter(this.pool, input);
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
          normalized.characterId,
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
}

export {
  DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT,
  createCharacterStoreError,
  toCharacter
};
