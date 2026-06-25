import assert from "node:assert/strict";
import { test } from "node:test";

import {
  CharacterStore,
  DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT
} from "../apps/auth-http/src/character-store.js";

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

function createInput(characterId, accountPlayerId = "player-001", overrides = {}) {
  return {
    characterId,
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
  const store = new CharacterStore(pool);

  const created = await store.createCharacter(createInput("chr_001"));
  const fetched = await store.getByCharacterId("chr_001");

  assert.equal(created.characterId, "chr_001");
  assert.equal(created.accountPlayerId, "player-001");
  assert.equal(fetched.characterId, "chr_001");
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

test("CharacterStore lists by account and allows duplicate names with distinct character_id", async () => {
  const pool = new MemoryCharacterPool();
  const store = new CharacterStore(pool);

  await store.createCharacter(createInput("chr_001", "player-001", { name: "Echo" }));
  await store.createCharacter(createInput("chr_002", "player-001", { name: "Echo" }));
  await store.createCharacter(createInput("chr_003", "player-002", { name: "Echo" }));

  const list = await store.listByAccountPlayerId("player-001");
  assert.deepEqual(
    list.map((character) => [character.characterId, character.name]),
    [
      ["chr_001", "Echo"],
      ["chr_002", "Echo"]
    ]
  );

  assert.equal((await store.getByCharacterId("chr_001")).name, "Echo");
  assert.equal((await store.getByCharacterId("chr_002")).name, "Echo");
});

test("CharacterStore soft delete hides ordinary list and get while admin query can include deleted", async () => {
  const pool = new MemoryCharacterPool();
  const store = new CharacterStore(pool);

  await store.createCharacter(createInput("chr_001"));
  await store.createCharacter(createInput("chr_002"));

  assert.equal(await store.softDeleteCharacter("chr_001"), true);
  assert.equal(await store.getByCharacterId("chr_001"), null);

  const ordinaryList = await store.listByAccountPlayerId("player-001");
  assert.deepEqual(ordinaryList.map((character) => character.characterId), ["chr_002"]);

  const adminFetched = await store.getByCharacterId("chr_001", { includeDeleted: true });
  const adminList = await store.listByAccountPlayerId("player-001", { includeDeleted: true });
  assert.equal(adminFetched.characterId, "chr_001");
  assert.ok(adminFetched.deletedAt);
  assert.deepEqual(adminList.map((character) => character.characterId), ["chr_001", "chr_002"]);
});

test("CharacterStore updates last_login_at and current position for active characters only", async () => {
  const pool = new MemoryCharacterPool();
  const store = new CharacterStore(pool);

  await store.createCharacter(createInput("chr_001"));
  assert.equal(await store.updateLastLoginAt("chr_001"), true);
  assert.equal(
    await store.updatePosition("chr_001", {
      sceneId: 200,
      x: 11.5,
      y: -3,
      dirX: 1,
      dirY: 0
    }),
    true
  );

  const fetched = await store.getByCharacterId("chr_001");
  assert.ok(fetched.lastLoginAt);
  assert.deepEqual(fetched.position, {
    sceneId: 200,
    x: 11.5,
    y: -3,
    dirX: 1,
    dirY: 0
  });

  await store.softDeleteCharacter("chr_001");
  assert.equal(await store.updateLastLoginAt("chr_001"), false);
  assert.equal(await store.updatePosition("chr_001", { sceneId: 300 }), false);
});

test("CharacterStore normal creation rejects the seventh effective character", async () => {
  const pool = new MemoryCharacterPool();
  const store = new CharacterStore(pool);

  for (let index = 1; index <= DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT; index += 1) {
    await store.createCharacter(createInput(`chr_${index}`));
  }

  await assert.rejects(
    () => store.createCharacter(createInput("chr_7")),
    (error) => {
      assert.equal(error.code, "CHARACTER_LIMIT_EXCEEDED");
      assert.equal(error.current, 6);
      assert.equal(error.limit, 6);
      return true;
    }
  );
});

test("CharacterStore soft deleted characters do not count against normal creation limit", async () => {
  const pool = new MemoryCharacterPool();
  const store = new CharacterStore(pool);

  for (let index = 1; index <= DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT; index += 1) {
    await store.createCharacter(createInput(`chr_${index}`));
  }
  await store.softDeleteCharacter("chr_1");

  assert.equal(await store.countEffectiveByAccountPlayerId("player-001"), 5);
  const created = await store.createCharacter(createInput("chr_7"));
  assert.equal(created.characterId, "chr_7");
  assert.equal(await store.countEffectiveByAccountPlayerId("player-001"), 6);
});

test("CharacterStore admin creation bypasses normal limit through a separate method", async () => {
  const pool = new MemoryCharacterPool();
  const store = new CharacterStore(pool);

  for (let index = 1; index <= DEFAULT_MAX_EFFECTIVE_CHARACTERS_PER_ACCOUNT; index += 1) {
    await store.createCharacter(createInput(`chr_${index}`));
  }

  const beforeQueryCount = pool.queries.length;
  const gmCreated = await store.createCharacterForAdmin(
    createInput("chr_gm_7", "player-001", { name: "GmCreated" })
  );
  const bypassQueries = pool.queries.slice(beforeQueryCount);

  assert.equal(gmCreated.characterId, "chr_gm_7");
  assert.equal(await store.countEffectiveByAccountPlayerId("player-001"), 7);
  assert.equal(
    bypassQueries.some((query) => query.sql.startsWith("SELECT COUNT(*) AS total")),
    false
  );
});
