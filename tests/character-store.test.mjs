import assert from "node:assert/strict";
import { test } from "node:test";

import {
  CharacterStore,
  DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT
} from "../apps/auth-http/src/character-store.js";
import { encodeGlobalId } from "../packages/global-id/node/index.js";

class MemoryCharacterPool {
  constructor() {
    this.rows = [];
    this.queries = [];
    this.now = 0;
    this.releaseCount = 0;
  }

  async connect() {
    return {
      query: (sql, params = []) => this.query(sql, params),
      release: () => {
        this.releaseCount += 1;
      }
    };
  }

  async query(sql, params = []) {
    const normalizedSql = sql.replace(/\s+/g, " ").trim();
    this.queries.push({ sql: normalizedSql, params });

    if (normalizedSql === "BEGIN" || normalizedSql === "COMMIT" || normalizedSql === "ROLLBACK") {
      return { rows: [], rowCount: 0 };
    }

    if (normalizedSql.startsWith("LOCK TABLE characters")) {
      return { rows: [], rowCount: 0 };
    }

    if (normalizedSql.startsWith("SELECT COUNT(*) AS total FROM characters")) {
      const [accountPlayerId] = params;
      return {
        rows: [
          {
            total: this.rows.filter(
              (row) => row.account_player_id === accountPlayerId && row.deleted_at === null
            ).length
          }
        ],
        rowCount: 1
      };
    }

    if (normalizedSql.startsWith("INSERT INTO characters")) {
      const characterId = params[0];
      if (this.rows.some((row) => row.character_id === characterId)) {
        const error = new Error("duplicate key value violates unique constraint");
        error.code = "23505";
        throw error;
      }

      const row = {
        character_id: characterId,
        account_player_id: params[1],
        world_id: params[2],
        name: params[3],
        status: params[4],
        appearance_json: JSON.parse(params[5]),
        scene_id: params[6],
        x: params[7],
        y: params[8],
        dir_x: params[9],
        dir_y: params[10],
        affinity_earth: params[11],
        affinity_fire: params[12],
        affinity_water: params[13],
        affinity_wind: params[14],
        mastery_earth: params[15],
        mastery_fire: params[16],
        mastery_water: params[17],
        mastery_wind: params[18],
        created_at: this.nextDate(),
        last_login_at: null,
        deleted_at: null
      };
      this.rows.push(row);
      return { rows: [{ ...row }], rowCount: 1 };
    }

    if (normalizedSql.startsWith("SELECT character_id, account_player_id") && normalizedSql.includes("WHERE account_player_id = $1")) {
      const [accountPlayerId] = params;
      const includeDeleted = !normalizedSql.includes("deleted_at IS NULL");
      const rows = this.rows
        .filter(
          (row) =>
            row.account_player_id === accountPlayerId &&
            (includeDeleted || row.deleted_at === null)
        )
        .sort((left, right) => left.created_at - right.created_at || left.character_id.localeCompare(right.character_id))
        .map((row) => ({ ...row }));
      return { rows, rowCount: rows.length };
    }

    if (normalizedSql.startsWith("SELECT character_id, account_player_id") && normalizedSql.includes("WHERE name = $1")) {
      const [name] = params;
      const includeDeleted = !normalizedSql.includes("deleted_at IS NULL");
      const accountParamIndex = normalizedSql.includes("account_player_id = $2") ? 1 : -1;
      const worldParamIndex = normalizedSql.includes("world_id = $2")
        ? 1
        : normalizedSql.includes("world_id = $3")
          ? 2
          : -1;
      const limit = params.at(-1);
      const rows = this.rows
        .filter((row) => row.name === name)
        .filter((row) => accountParamIndex === -1 || row.account_player_id === params[accountParamIndex])
        .filter((row) => worldParamIndex === -1 || row.world_id === params[worldParamIndex])
        .filter((row) => includeDeleted || row.deleted_at === null)
        .sort((left, right) => left.world_id - right.world_id || left.created_at - right.created_at || left.character_id.localeCompare(right.character_id))
        .slice(0, limit)
        .map((row) => ({ ...row }));
      return { rows, rowCount: rows.length };
    }

    if (normalizedSql.startsWith("SELECT character_id, account_player_id") && normalizedSql.includes("WHERE character_id = $1")) {
      const [characterId] = params;
      const includeDeleted = !normalizedSql.includes("deleted_at IS NULL");
      const row = this.rows.find(
        (candidate) =>
          candidate.character_id === characterId &&
          (includeDeleted || candidate.deleted_at === null)
      );
      return { rows: row ? [{ ...row }] : [], rowCount: row ? 1 : 0 };
    }

    if (normalizedSql.startsWith("SELECT character_id FROM characters WHERE world_id = $1 AND name = $2")) {
      const [worldId, name] = params;
      const row = this.rows.find(
        (candidate) =>
          candidate.world_id === worldId &&
          candidate.name === name &&
          candidate.deleted_at === null
      );
      return { rows: row ? [{ character_id: row.character_id }] : [], rowCount: row ? 1 : 0 };
    }

    if (normalizedSql.startsWith("UPDATE characters SET deleted_at = current_timestamp")) {
      const [characterId] = params;
      const row = this.rows.find(
        (candidate) => candidate.character_id === characterId && candidate.deleted_at === null
      );
      if (!row) {
        return { rows: [], rowCount: 0 };
      }
      row.deleted_at = this.nextDate();
      return { rows: [], rowCount: 1 };
    }

    if (normalizedSql.startsWith("UPDATE characters SET last_login_at = current_timestamp")) {
      const [characterId] = params;
      const row = this.rows.find(
        (candidate) => candidate.character_id === characterId && candidate.deleted_at === null
      );
      if (!row) {
        return { rows: [], rowCount: 0 };
      }
      row.last_login_at = this.nextDate();
      return { rows: [], rowCount: 1 };
    }

    if (normalizedSql.startsWith("UPDATE characters SET scene_id = $2")) {
      const [characterId, sceneId, x, y, dirX, dirY] = params;
      const row = this.rows.find(
        (candidate) => candidate.character_id === characterId && candidate.deleted_at === null
      );
      if (!row) {
        return { rows: [], rowCount: 0 };
      }
      row.scene_id = sceneId;
      row.x = x;
      row.y = y;
      row.dir_x = dirX;
      row.dir_y = dirY;
      return { rows: [], rowCount: 1 };
    }

    throw new Error(`unexpected query: ${normalizedSql}`);
  }

  nextDate() {
    this.now += 1;
    return new Date(Date.UTC(2026, 0, 1, 0, 0, this.now));
  }
}

function createCharacterIdGenerator() {
  let next = 0n;
  return () => {
    next += 1n;
    return encodeGlobalId("chr", next);
  };
}

function createStore(pool, options = {}) {
  return new CharacterStore(pool, {
    characterIdGenerator: createCharacterIdGenerator(),
    ...options
  });
}

function createInput(accountPlayerId = "player-001", overrides = {}) {
  return {
    accountPlayerId,
    name: overrides.name ?? "SameName",
    appearance: overrides.appearance ?? { body: "default" },
    position: overrides.position ?? {
      sceneId: 100,
      x: 1,
      y: 2,
      dirX: 0,
      dirY: 1
    },
    ...overrides
  };
}

test("CharacterStore creates characters and fetches by character_id", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool);

  const created = await store.createCharacter(createInput());
  const fetched = await store.getByCharacterId(created.characterId);

  assert.match(created.characterId, /^chr_[0-9a-hjkmnp-tv-z]+$/);
  assert.equal(created.accountPlayerId, "player-001");
  assert.equal(fetched.characterId, created.characterId);
  assert.equal(pool.queries.some((query) => query.sql === "BEGIN"), true);
  assert.equal(
    pool.queries.some((query) => query.sql.startsWith("LOCK TABLE characters")),
    true
  );
  assert.equal(pool.queries.some((query) => query.sql === "COMMIT"), true);
  assert.equal(pool.releaseCount, 1);
  assert.deepEqual(fetched.affinity, {
    earth: 2500,
    fire: 2500,
    water: 2500,
    wind: 2500
  });
  assert.deepEqual(fetched.mastery, {
    earth: 0,
    fire: 0,
    water: 0,
    wind: 0
  });
});

test("CharacterStore ignores caller supplied characterId on normal creation", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool, {
    characterIdGenerator: () => "chr_generated"
  });

  const created = await store.createCharacter(
    createInput("player-001", { characterId: "chr_client_supplied" })
  );

  assert.equal(created.characterId, "chr_generated");
  assert.equal(await store.getByCharacterId("chr_client_supplied"), null);
  assert.equal(pool.rows[0].character_id, "chr_generated");
});

test("CharacterStore creates distinct generated character IDs across repeated creation", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool);

  const created = [];
  for (let index = 0; index < 5; index += 1) {
    created.push(await store.createCharacter(createInput()));
  }

  const ids = created.map((character) => character.characterId);
  assert.equal(new Set(ids).size, ids.length);
  assert.equal(ids.every((id) => /^chr_[0-9a-hjkmnp-tv-z]+$/.test(id)), true);
});

test("CharacterStore lists by account and allows duplicate names with distinct character_id", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool);

  const first = await store.createCharacter(createInput("player-001", { name: "Echo" }));
  const second = await store.createCharacter(createInput("player-001", { name: "Echo" }));
  await store.createCharacter(createInput("player-002", { name: "Echo" }));

  const list = await store.listByAccountPlayerId("player-001");
  assert.deepEqual(
    list.map((character) => [character.characterId, character.name]),
    [
      [first.characterId, "Echo"],
      [second.characterId, "Echo"]
    ]
  );

  assert.notEqual(first.characterId, second.characterId);
  assert.equal((await store.getByCharacterId(first.characterId)).name, "Echo");
  assert.equal((await store.getByCharacterId(second.characterId)).name, "Echo");
});

test("CharacterStore soft delete hides ordinary list and get while admin query can include deleted", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool);

  const first = await store.createCharacter(createInput());
  const second = await store.createCharacter(createInput());

  assert.equal(await store.softDeleteCharacter(first.characterId), true);
  assert.equal(await store.getByCharacterId(first.characterId), null);

  const ordinaryList = await store.listByAccountPlayerId("player-001");
  assert.deepEqual(ordinaryList.map((character) => character.characterId), [second.characterId]);

  const adminFetched = await store.getByCharacterId(first.characterId, { includeDeleted: true });
  const adminList = await store.listByAccountPlayerId("player-001", { includeDeleted: true });
  assert.equal(adminFetched.characterId, first.characterId);
  assert.ok(adminFetched.deletedAt);
  assert.deepEqual(adminList.map((character) => character.characterId), [
    first.characterId,
    second.characterId
  ]);
});

test("CharacterStore updates last_login_at and current position for active characters only", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool);

  const created = await store.createCharacter(createInput());
  assert.equal(await store.updateLastLoginAt(created.characterId), true);
  assert.equal(
    await store.updatePosition(created.characterId, {
      sceneId: 200,
      x: 11.5,
      y: -3,
      dirX: 1,
      dirY: 0
    }),
    true
  );

  const fetched = await store.getByCharacterId(created.characterId);
  assert.ok(fetched.lastLoginAt);
  assert.deepEqual(fetched.position, {
    sceneId: 200,
    x: 11.5,
    y: -3,
    dirX: 1,
    dirY: 0
  });

  await store.softDeleteCharacter(created.characterId);
  assert.equal(await store.updateLastLoginAt(created.characterId), false);
  assert.equal(await store.updatePosition(created.characterId, { sceneId: 300 }), false);
});

test("CharacterStore normal creation rejects the seventh effective character", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool);

  for (let index = 1; index <= DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT; index += 1) {
    await store.createCharacter(createInput());
  }

  await assert.rejects(
    () => store.createCharacter(createInput()),
    (error) => {
      assert.equal(error.code, "CHARACTER_LIMIT_EXCEEDED");
      assert.equal(error.current, 6);
      assert.equal(error.limit, 6);
      return true;
    }
  );
});

test("CharacterStore normal creation limit is configurable", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool, {
    maxEffectiveCharactersPerAccount: 2
  });

  await store.createCharacter(createInput());
  await store.createCharacter(createInput());

  await assert.rejects(
    () => store.createCharacter(createInput()),
    (error) => {
      assert.equal(error.code, "CHARACTER_LIMIT_EXCEEDED");
      assert.equal(error.current, 2);
      assert.equal(error.limit, 2);
      return true;
    }
  );
});

test("CharacterStore soft deleted characters do not count against normal creation limit", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool);

  let firstCharacterId = null;
  for (let index = 1; index <= DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT; index += 1) {
    const created = await store.createCharacter(createInput());
    firstCharacterId ??= created.characterId;
  }
  await store.softDeleteCharacter(firstCharacterId);

  assert.equal(await store.countEffectiveByAccountPlayerId("player-001"), 5);
  const created = await store.createCharacter(createInput());
  assert.match(created.characterId, /^chr_[0-9a-hjkmnp-tv-z]+$/);
  assert.equal(await store.countEffectiveByAccountPlayerId("player-001"), 6);
});

test("CharacterStore can reject duplicate character names when configured", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool, {
    allowDuplicateNames: false
  });

  const first = await store.createCharacter(createInput("player-001", { worldId: 9, name: "Echo" }));
  await assert.rejects(
    () => store.createCharacter(createInput("player-002", { worldId: 9, name: "Echo" })),
    (error) => {
      assert.equal(error.code, "CHARACTER_NAME_DUPLICATE");
      assert.equal(error.worldId, 9);
      assert.equal(error.name, "Echo");
      assert.equal(error.existingCharacterId, first.characterId);
      return true;
    }
  );

  const differentWorld = await store.createCharacter(createInput("player-002", { worldId: 10, name: "Echo" }));
  assert.equal(differentWorld.worldId, 10);
});

test("CharacterStore searchByCharacterName returns multiple disambiguated candidates", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool);

  const first = await store.createCharacter(createInput("player-001", { worldId: 2, name: "Echo" }));
  const second = await store.createCharacter(createInput("player-002", { worldId: 1, name: "Echo" }));
  const third = await store.createCharacter(createInput("player-003", { worldId: 3, name: "Echo" }));
  await store.createCharacter(createInput("player-004", { name: "Other" }));
  await store.softDeleteCharacter(third.characterId);

  const candidates = await store.searchByCharacterName("Echo");
  assert.deepEqual(
    candidates.map((character) => [character.characterId, character.accountPlayerId, character.worldId, character.name]),
    [
      [second.characterId, "player-002", 1, "Echo"],
      [first.characterId, "player-001", 2, "Echo"]
    ]
  );

  const scoped = await store.searchByCharacterName("Echo", { accountPlayerId: "player-001", limit: 1 });
  assert.deepEqual(scoped.map((character) => character.characterId), [first.characterId]);

  const withDeleted = await store.searchByCharacterName("Echo", { includeDeleted: true });
  assert.deepEqual(
    withDeleted.map((character) => character.characterId),
    [second.characterId, first.characterId, third.characterId]
  );
});

test("CharacterStore admin creation bypasses normal limit through explicit audited options", async () => {
  const pool = new MemoryCharacterPool();
  const logs = [];
  const store = createStore(pool);
  store.logger = (level, message, extra) => {
    logs.push({ level, message, extra });
  };

  for (let index = 1; index <= DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT; index += 1) {
    await store.createCharacter(createInput());
  }

  const beforeQueryCount = pool.queries.length;
  const gmCreated = await store.createCharacterForAdmin(
    createInput("player-001", { name: "GmCreated" }),
    {
      bypassCharacterLimit: true,
      adminActor: "ops@example.com",
      reason: "restore after support ticket",
      targetAccountPlayerId: "player-001"
    }
  );
  const bypassQueries = pool.queries.slice(beforeQueryCount);

  assert.match(gmCreated.characterId, /^chr_[0-9a-hjkmnp-tv-z]+$/);
  assert.deepEqual(gmCreated.adminAudit, {
    auditLogTable: "admin_audit_logs",
    action: "character.create",
    adminActor: "ops@example.com",
    reason: "restore after support ticket",
    targetAccountPlayerId: "player-001",
    generatedCharacterId: gmCreated.characterId,
    targetType: "character",
    targetValue: gmCreated.characterId,
    characterName: "GmCreated",
    worldId: 0,
    bypassCharacterLimit: true
  });
  assert.deepEqual(logs, [
    {
      level: "info",
      message: "character.admin_bypass_create",
      extra: gmCreated.adminAudit
    }
  ]);
  assert.equal(await store.countEffectiveByAccountPlayerId("player-001"), 7);
  assert.equal(
    bypassQueries.some((query) => query.sql.startsWith("SELECT COUNT(*) AS total")),
    false
  );
});

test("CharacterStore admin creation requires explicit bypass audit fields", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool);

  await assert.rejects(
    () => store.createCharacterForAdmin(createInput("player-001")),
    { code: "ADMIN_BYPASS_CHARACTER_LIMIT_REQUIRED" }
  );
  await assert.rejects(
    () => store.createCharacterForAdmin(createInput("player-001"), {
      bypassCharacterLimit: true,
      reason: "restore",
      targetAccountPlayerId: "player-001"
    }),
    { code: "ADMIN_AUDIT_ACTOR_REQUIRED" }
  );
  await assert.rejects(
    () => store.createCharacterForAdmin(createInput("player-001"), {
      bypassCharacterLimit: true,
      adminActor: "ops@example.com",
      targetAccountPlayerId: "player-001"
    }),
    { code: "ADMIN_AUDIT_REASON_REQUIRED" }
  );
  await assert.rejects(
    () => store.createCharacterForAdmin(createInput("player-001"), {
      bypassCharacterLimit: true,
      adminActor: "ops@example.com",
      reason: "restore",
      targetAccountPlayerId: "player-002"
    }),
    { code: "ADMIN_AUDIT_TARGET_ACCOUNT_MISMATCH" }
  );
});

test("CharacterStore admin creation also ignores caller supplied characterId", async () => {
  const pool = new MemoryCharacterPool();
  const store = createStore(pool, {
    characterIdGenerator: () => "chr_admngenerated"
  });

  const created = await store.createCharacterForAdmin(
    createInput("player-001", {
      characterId: "chr_admin_supplied",
      name: "GmCreated"
    }),
    {
      bypassCharacterLimit: true,
      adminActor: "ops@example.com",
      reason: "restore",
      targetAccountPlayerId: "player-001"
    }
  );

  assert.equal(created.characterId, "chr_admngenerated");
  assert.equal(await store.getByCharacterId("chr_admin_supplied"), null);
});

test("CharacterStore reports and logs character ID generation failures", async () => {
  const pool = new MemoryCharacterPool();
  const logs = [];
  const generatorError = new Error("lease inactive");
  generatorError.code = "WORKER_LEASE_INACTIVE";
  const store = createStore(pool, {
    characterIdGenerator: () => {
      throw generatorError;
    },
    logger: (level, message, extra) => {
      logs.push({ level, message, extra });
    }
  });

  await assert.rejects(
    () => store.createCharacter(createInput()),
    (error) => {
      assert.equal(error.code, "CHARACTER_ID_GENERATION_FAILED");
      assert.equal(error.generatorErrorCode, "WORKER_LEASE_INACTIVE");
      assert.equal(error.accountPlayerId, "player-001");
      assert.equal(error.cause, generatorError);
      return true;
    }
  );
  assert.deepEqual(logs, [
    {
      level: "error",
      message: "character.id_generation_failed",
      extra: {
        errorCode: "CHARACTER_ID_GENERATION_FAILED",
        generatorErrorCode: "WORKER_LEASE_INACTIVE",
        accountPlayerId: "player-001",
        message: "lease inactive"
      }
    }
  ]);
  assert.equal(pool.rows.length, 0);
});

test("CharacterStore rejects invalid generated character ID format", async () => {
  const pool = new MemoryCharacterPool();
  const logs = [];
  const store = createStore(pool, {
    characterIdGenerator: () => "player_supplied_id",
    logger: (level, message, extra) => {
      logs.push({ level, message, extra });
    }
  });

  await assert.rejects(
    () => store.createCharacter(createInput()),
    (error) => {
      assert.equal(error.code, "CHARACTER_ID_GENERATION_FAILED");
      assert.equal(error.generatorErrorCode, "INVALID_CHARACTER_ID_FORMAT");
      return true;
    }
  );
  assert.equal(logs[0].extra.errorCode, "CHARACTER_ID_GENERATION_FAILED");
  assert.equal(logs[0].extra.generatorErrorCode, "INVALID_CHARACTER_ID_FORMAT");
  assert.equal(pool.rows.length, 0);
});
