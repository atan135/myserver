import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { PlayersController } = await import("./players.controller.ts");
const { AdminStore } = await import("../admin-store.js");

function storeFixture() {
  return {
    status: null,
    audits: [],
    createdCharacters: [],
    restoredCharacters: [],
    characters: new Map(),
    titleQuery: null,
    characterListQuery: null,
    profileQuery: null,
    async findPlayerById() {
      return { id: "player-1", status: "active", banExpiresAt: null };
    },
    async findCharactersByAccountPlayerId(playerId, options) {
      this.characterListQuery = { playerId, options };
      return Array.from(this.characters.values())
        .filter((character) => character.account_player_id === playerId || character.accountPlayerId === playerId);
    },
    async countCharactersByAccountPlayerId(playerId, options) {
      this.characterListQuery = { playerId, options };
      return Array.from(this.characters.values())
        .filter((character) => character.account_player_id === playerId || character.accountPlayerId === playerId)
        .length;
    },
    async findCharacterProfileOverview(input) {
      this.profileQuery = input;
      const character = this.characters.get(input.characterId);
      if (!character) {
        return null;
      }
      return {
        character,
        titles: [],
        equippedTitle: null,
        disciplines: [{
          character_id: input.characterId,
          discipline_id: "forging",
          points: 120,
          tier: "apprentice",
          active: true,
          learned_at: "2026-05-01T00:00:00.000Z",
          updated_at: "2026-06-01T00:00:00.000Z"
        }],
        titleLogs: [],
        elementLogs: [{
          id: 1,
          character_id: input.characterId,
          affinity_delta: { earth: -100, fire: 100, water: 0, wind: 0 },
          mastery_delta: { earth: 0, fire: 10, water: 0, wind: 0 },
          reason: "gm adjust"
        }],
        disciplineLogs: [{
          id: 2,
          character_id: input.characterId,
          discipline_id: "forging",
          action: "upgrade",
          reason: "gm discipline"
        }]
      };
    },
    async findCharacterTitleOverview(input) {
      this.titleQuery = input;
      const title = {
        character_id: input.characterId,
        title_id: "9001",
        source_type: "system",
        source_id: "debug-grant",
        is_equipped: true,
        unlocked_at: "2026-06-01T00:00:00.000Z",
        expires_at: "2026-07-01T00:00:00.000Z",
        expired: false,
        created_at: "2026-06-01T00:00:00.000Z",
        updated_at: "2026-06-02T00:00:00.000Z",
        operator_type: "admin",
        operator_id: "ops",
        operator: {
          type: "admin",
          id: "ops"
        },
        latest_log: {
          action: "grant",
          operator_type: "admin",
          operator_id: "ops",
          operator: {
            type: "admin",
            id: "ops"
          },
          reason: "test",
          created_at: "2026-06-01T00:00:00.000Z"
        }
      };

      return {
        titles: [title],
        equippedTitle: title,
        disciplines: [{
          discipline_id: "forging",
          points: 120,
          tier: "novice",
          active: true,
          learned_at: "2026-05-01T00:00:00.000Z",
          updated_at: "2026-06-01T00:00:00.000Z"
        }],
        titleLogs: [{
          id: 7,
          character_id: input.characterId,
          title_id: "9001",
          action: "grant",
          source_type: "system",
          source_id: "debug-grant",
          operator_type: "admin",
          operator_id: "ops",
          operator: {
            type: "admin",
            id: "ops"
          },
          before_json: null,
          after_json: { title_id: "9001" },
          reason: "test",
          created_at: "2026-06-01T00:00:00.000Z"
        }]
      };
    },
    async updatePlayerStatus(playerId, status) {
      this.status = { playerId, status };
    },
    async createCharacterForAdmin(input) {
      this.createdCharacters.push(input);
      const character = {
        characterId: "chr_0000000000009",
        character_id: "chr_0000000000009",
        accountPlayerId: input.accountPlayerId,
        account_player_id: input.accountPlayerId,
        worldId: input.worldId,
        world_id: input.worldId,
        name: input.name,
        status: "active",
        deletedAt: null,
        deleted_at: null
      };
      this.characters.set(character.character_id, character);
      return character;
    },
    async findCharacterById(characterId) {
      return this.characters.get(characterId) ?? null;
    },
    async restoreCharacterForAdmin(characterId) {
      this.restoredCharacters.push(characterId);
      const character = this.characters.get(characterId);
      if (!character || character.status !== "deleted" || !character.deleted_at) {
        return null;
      }
      character.status = "active";
      character.deletedAt = null;
      character.deleted_at = null;
      return character;
    },
    async appendAuditLog(entry) {
      this.audits.push(entry);
    }
  };
}

function request(role) {
  return {
    admin: {
      sub: 1,
      username: "worker",
      role
    },
    socket: {
      remoteAddress: "127.0.0.1"
    },
    headers: {}
  };
}

test("viewer can query character title overview with title metadata and audit", async () => {
  const store = storeFixture();
  const controller = new PlayersController({}, store);

  const response = await controller.characterTitles(" char_1 ", "150", request("viewer"));

  assert.equal(response.ok, true);
  assert.equal(response.characterId, "char_1");
  assert.deepEqual(store.titleQuery, { characterId: "char_1", logLimit: 100 });
  assert.equal(response.titles.length, 1);
  assert.equal(response.titles[0].title_id, "9001");
  assert.equal(response.titles[0].source_type, "system");
  assert.equal(response.titles[0].source_id, "debug-grant");
  assert.equal(response.titles[0].operator.id, "ops");
  assert.equal(response.titles[0].hidden, true);
  assert.equal(response.titles[0].limited, false);
  assert.equal(response.titles[0].is_equipped, true);
  assert.equal(response.equippedTitle.title_id, "9001");
  assert.equal(response.disciplines[0].discipline_id, "forging");
  assert.equal(response.titleLogs[0].operator_id, "ops");
  assert.equal(store.audits.length, 1);
  assert.equal(store.audits[0].action, "character_titles_query");
  assert.equal(store.audits[0].targetType, "character");
  assert.equal(store.audits[0].targetValue, "char_1");
  assert.equal(store.audits[0].details.result, "success");
  assert.equal(store.audits[0].details.logLimit, 100);
  assert.equal(store.audits[0].details.titleCount, 1);
});

test("viewer can list account characters", async () => {
  const store = storeFixture();
  store.characters.set("chr_0000000000011", {
    characterId: "chr_0000000000011",
    character_id: "chr_0000000000011",
    accountPlayerId: "player-1",
    account_player_id: "player-1",
    name: "Echo",
    status: "active"
  });
  const controller = new PlayersController({}, store);

  const response = await controller.playerCharacters("player-1", {
    includeDeleted: "false",
    limit: "10",
    offset: "5"
  });

  assert.equal(response.ok, true);
  assert.equal(response.playerId, "player-1");
  assert.equal(response.characters.length, 1);
  assert.equal(response.total, 1);
  assert.deepEqual(store.characterListQuery, {
    playerId: "player-1",
    options: { includeDeleted: false }
  });
});

test("viewer can query character profile with attributes, title, discipline, and logs", async () => {
  const store = storeFixture();
  store.characters.set("chr_0000000000012", {
    characterId: "chr_0000000000012",
    character_id: "chr_0000000000012",
    accountPlayerId: "player-1",
    account_player_id: "player-1",
    name: "Echo",
    status: "active",
    attributes: {
      affinity: { earth: 2400, fire: 2600, water: 2500, wind: 2500 },
      mastery: { earth: 0, fire: 10, water: 0, wind: 0 }
    }
  });
  const controller = new PlayersController({}, store);

  const response = await controller.characterProfile(" chr_0000000000012 ", "25", request("viewer"));

  assert.equal(response.ok, true);
  assert.equal(response.characterId, "chr_0000000000012");
  assert.deepEqual(store.profileQuery, { characterId: "chr_0000000000012", logLimit: 25 });
  assert.equal(response.attributes.affinity.fire, 2600);
  assert.equal(response.disciplines[0].tier, "apprentice");
  assert.equal(response.logs.elements[0].reason, "gm adjust");
  assert.equal(response.logs.disciplines[0].action, "upgrade");
  assert.equal(store.audits.length, 1);
  assert.equal(store.audits[0].action, "character_profile_query");
  assert.equal(store.audits[0].targetValue, "chr_0000000000012");
  assert.equal(store.audits[0].details.elementLogCount, 1);
  assert.equal(store.audits[0].details.disciplineLogCount, 1);
});

test("invalid character title query writes failed audit", async () => {
  const store = storeFixture();
  const controller = new PlayersController({}, store);

  await assert.rejects(
    () => controller.characterTitles(" ", undefined, request("viewer")),
    (error) => {
      assert.equal(error.getStatus(), 400);
      assert.deepEqual(error.getResponse(), {
        ok: false,
        error: "INVALID_CHARACTER_ID",
        message: "characterId is required"
      });
      return true;
    }
  );

  assert.equal(store.titleQuery, null);
  assert.equal(store.audits.length, 1);
  assert.equal(store.audits[0].action, "character_titles_query_failed");
  assert.equal(store.audits[0].targetType, "character");
  assert.equal(store.audits[0].targetValue, null);
  assert.equal(store.audits[0].details.error, "INVALID_CHARACTER_ID");
});

test("operator can create character for player bypassing ordinary limit and writes audit", async () => {
  const store = storeFixture();
  const controller = new PlayersController({}, store);

  const response = await controller.createCharacterForPlayer(
    "player-1",
    {
      name: "Echo",
      reason: "support restore",
      worldId: 9,
      appearance: { body: "default" }
    },
    request("operator")
  );

  assert.equal(response.ok, true);
  assert.equal(response.character.character_id, "chr_0000000000009");
  assert.deepEqual(store.createdCharacters, [{
    accountPlayerId: "player-1",
    name: "Echo",
    worldId: 9,
    appearance: { body: "default" },
    position: { scene_id: 100, x: 0, y: 0, dir_x: 0, dir_y: 1 },
    affinity: { earth: 2500, fire: 2500, water: 2500, wind: 2500 },
    mastery: { earth: 0, fire: 0, water: 0, wind: 0 }
  }]);
  assert.equal(store.audits.length, 1);
  assert.equal(store.audits[0].action, "admin_character_create");
  assert.equal(store.audits[0].targetType, "character");
  assert.equal(store.audits[0].targetValue, "chr_0000000000009");
  assert.equal(store.audits[0].adminId, 1);
  assert.equal(store.audits[0].adminUsername, "worker");
  assert.equal(store.audits[0].ip, "127.0.0.1");
  assert.deepEqual(store.audits[0].details, {
    result: "success",
    reason: "support restore",
    targetAccountPlayerId: "player-1",
    bypassCharacterLimit: true,
    characterId: "chr_0000000000009",
    characterName: "Echo",
    worldId: 9,
    permission: "players.status.update"
  });
});

test("operator can restore deleted character bypassing ordinary limit and writes audit", async () => {
  const store = storeFixture();
  store.characters.set("chr_0000000000008", {
    characterId: "chr_0000000000008",
    character_id: "chr_0000000000008",
    accountPlayerId: "player-1",
    account_player_id: "player-1",
    status: "deleted",
    deletedAt: "2026-06-25T12:00:00.000Z",
    deleted_at: "2026-06-25T12:00:00.000Z"
  });
  const controller = new PlayersController({}, store);

  const response = await controller.restoreCharacter(
    "chr_0000000000008",
    { reason: "support restore" },
    request("operator")
  );

  assert.equal(response.ok, true);
  assert.equal(response.character.status, "active");
  assert.deepEqual(store.restoredCharacters, ["chr_0000000000008"]);
  assert.equal(store.audits.length, 1);
  assert.equal(store.audits[0].action, "admin_character_restore");
  assert.equal(store.audits[0].targetType, "character");
  assert.equal(store.audits[0].targetValue, "chr_0000000000008");
  assert.deepEqual(store.audits[0].details, {
    result: "success",
    reason: "support restore",
    targetAccountPlayerId: "player-1",
    bypassCharacterLimit: true,
    characterId: "chr_0000000000008",
    fromStatus: "deleted",
    toStatus: "active",
    permission: "players.status.update"
  });
});

test("admin character create missing reason writes failed audit", async () => {
  const store = storeFixture();
  const controller = new PlayersController({}, store);

  await assert.rejects(
    () => controller.createCharacterForPlayer("player-1", { name: "Echo" }, request("operator")),
    (error) => {
      assert.equal(error.getStatus(), 400);
      assert.deepEqual(error.getResponse(), {
        ok: false,
        error: "MISSING_REASON",
        message: "reason is required"
      });
      return true;
    }
  );

  assert.equal(store.createdCharacters.length, 0);
  assert.equal(store.audits.length, 1);
  assert.equal(store.audits[0].action, "admin_character_create_failed");
  assert.equal(store.audits[0].targetType, "character");
  assert.equal(store.audits[0].targetValue, null);
  assert.equal(store.audits[0].details.error, "MISSING_REASON");
  assert.equal(store.audits[0].details.bypassCharacterLimit, true);
});

test("admin character restore invalid state writes failed audit", async () => {
  const store = storeFixture();
  store.characters.set("chr_0000000000007", {
    characterId: "chr_0000000000007",
    character_id: "chr_0000000000007",
    accountPlayerId: "player-1",
    account_player_id: "player-1",
    status: "active",
    deletedAt: null,
    deleted_at: null
  });
  const controller = new PlayersController({}, store);

  await assert.rejects(
    () => controller.restoreCharacter("chr_0000000000007", { reason: "support restore" }, request("operator")),
    (error) => {
      assert.equal(error.getStatus(), 400);
      assert.deepEqual(error.getResponse(), {
        ok: false,
        error: "CHARACTER_NOT_DELETED",
        message: "character is not deleted"
      });
      return true;
    }
  );

  assert.deepEqual(store.restoredCharacters, []);
  assert.equal(store.audits.length, 1);
  assert.equal(store.audits[0].action, "admin_character_restore_failed");
  assert.equal(store.audits[0].targetType, "character");
  assert.equal(store.audits[0].targetValue, "chr_0000000000007");
  assert.equal(store.audits[0].details.error, "CHARACTER_NOT_DELETED");
  assert.equal(store.audits[0].details.reason, "support restore");
  assert.equal(store.audits[0].details.bypassCharacterLimit, true);
});

test("admin character restore missing reason writes failed audit", async () => {
  const store = storeFixture();
  const controller = new PlayersController({}, store);

  await assert.rejects(
    () => controller.restoreCharacter("chr_0000000000007", {}, request("operator")),
    (error) => {
      assert.equal(error.getStatus(), 400);
      assert.deepEqual(error.getResponse(), {
        ok: false,
        error: "MISSING_REASON",
        message: "reason is required"
      });
      return true;
    }
  );

  assert.deepEqual(store.restoredCharacters, []);
  assert.equal(store.audits.length, 1);
  assert.equal(store.audits[0].action, "admin_character_restore_failed");
  assert.equal(store.audits[0].targetType, "character");
  assert.equal(store.audits[0].targetValue, null);
  assert.equal(store.audits[0].details.error, "MISSING_REASON");
  assert.equal(store.audits[0].details.bypassCharacterLimit, true);
});

test("AdminStore maps character title overview by character_id", async () => {
  const mainQueries = [];
  const gameQueries = [];
  const mainPool = {
    async query(query, params) {
      mainQueries.push({ query, params });
      if (query.includes("INSERT INTO admin_audit_logs")) {
        return { rowCount: 1 };
      }

      throw new Error("UNEXPECTED_MAIN_DB_QUERY");
    }
  };
  const gamePool = {
    async query(query, params) {
      gameQueries.push({ query, params });

      if (query.includes("FROM character_titles ct")) {
        return {
          rows: [{
            character_id: "char_1",
            title_id: "1001",
            source_type: "identity",
            source_id: "character_created",
            is_equipped: false,
            unlocked_at: new Date("2026-06-01T00:00:00.000Z"),
            expires_at: new Date("2026-06-02T00:00:00.000Z"),
            expired: true,
            created_at: new Date("2026-06-01T00:00:00.000Z"),
            updated_at: new Date("2026-06-02T00:00:00.000Z"),
            latest_action: "expire",
            latest_operator_type: "system",
            latest_operator_id: "title-service",
            latest_reason: "expired",
            latest_created_at: new Date("2026-06-02T00:00:00.000Z")
          }]
        };
      }

      if (query.includes("FROM character_disciplines")) {
        return {
          rows: [{
            discipline_id: "forging",
            points: "30",
            tier: "novice",
            active: true,
            learned_at: new Date("2026-05-01T00:00:00.000Z"),
            updated_at: new Date("2026-06-01T00:00:00.000Z")
          }]
        };
      }

      return {
        rows: [{
          id: "9",
          character_id: "char_1",
          title_id: "1001",
          action: "expire",
          source_type: "identity",
          source_id: "character_created",
          operator_type: "system",
          operator_id: "title-service",
          before_json: "{\"is_equipped\":true}",
          after_json: { is_equipped: false },
          reason: "expired",
          created_at: new Date("2026-06-02T00:00:00.000Z")
        }]
      };
    }
  };
  const store = new AdminStore(mainPool, null, {}, gamePool);

  const overview = await store.findCharacterTitleOverview({ characterId: "char_1", logLimit: 5 });

  assert.equal(mainQueries.length, 0);
  assert.equal(gameQueries.length, 3);
  assert.ok(gameQueries.every((entry) => entry.params[0] === "char_1"));
  assert.deepEqual(gameQueries[2].params, ["char_1", 5]);
  assert.equal(overview.titles[0].expired, true);
  assert.equal(overview.titles[0].operator_id, "title-service");
  assert.equal(overview.equippedTitle, null);
  assert.equal(overview.disciplines[0].points, 30);
  assert.deepEqual(overview.titleLogs[0].before_json, { is_equipped: true });
  assert.deepEqual(overview.titleLogs[0].after_json, { is_equipped: false });

  await store.appendAuditLog({
    adminId: 1,
    adminUsername: "worker",
    action: "character_titles_query",
    targetType: "character",
    targetValue: "char_1",
    details: { result: "success" },
    ip: "127.0.0.1"
  });
  assert.equal(mainQueries.length, 1);
  assert.match(mainQueries[0].query, /INSERT INTO admin_audit_logs/);
  assert.equal(gameQueries.length, 3);
});

test("AdminStore admin character create writes characters table without ordinary limit query and allows duplicate names", async () => {
  const mainQueries = [];
  const gameQueries = [];
  const mainPool = {
    async query(query, params) {
      mainQueries.push({ query, params });
      if (query.includes("INSERT INTO admin_audit_logs")) {
        return { rowCount: 1, rows: [] };
      }
      throw new Error("UNEXPECTED_MAIN_DB_QUERY");
    }
  };
  const gamePool = {
    rows: [],
    async query(query, params) {
      gameQueries.push({ query, params });
      if (query.includes("INSERT INTO characters")) {
        const row = {
          character_id: params[0],
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
          created_at: new Date("2026-06-25T12:00:00.000Z"),
          last_login_at: null,
          deleted_at: null
        };
        this.rows.push(row);
        return { rows: [row], rowCount: 1 };
      }
      throw new Error(`UNEXPECTED_GAME_DB_QUERY: ${query}`);
    }
  };
  const store = new AdminStore(mainPool, null, {
    characterIdGenerator: (() => {
      let next = 0;
      return () => `chr_${String(++next).padStart(13, "0")}`;
    })()
  }, gamePool);

  const first = await store.createCharacterForAdmin({
    accountPlayerId: "player-1",
    name: "Echo",
    worldId: 9,
    appearance: { body: "default" }
  });
  const second = await store.createCharacterForAdmin({
    accountPlayerId: "player-1",
    name: "Echo",
    worldId: 9
  });

  assert.equal(mainQueries.length, 0);
  assert.equal(gameQueries.length, 2);
  assert.ok(gameQueries.every((entry) => entry.query.includes("INSERT INTO characters")));
  assert.equal(gameQueries.some((entry) => entry.query.includes("COUNT(*)")), false);
  assert.equal(first.character_id, "chr_0000000000001");
  assert.equal(second.character_id, "chr_0000000000002");
  assert.equal(first.account_player_id, "player-1");
  assert.equal(first.name, "Echo");
  assert.deepEqual(first.attributes.affinity, {
    earth: 2500,
    fire: 2500,
    water: 2500,
    wind: 2500
  });
  assert.equal(gamePool.rows.length, 2);
});

test("AdminStore admin character restore updates deleted character without ordinary limit query", async () => {
  const mainPool = {
    async query() {
      throw new Error("UNEXPECTED_MAIN_DB_QUERY");
    }
  };
  const gameQueries = [];
  const gamePool = {
    async query(query, params) {
      gameQueries.push({ query, params });
      if (query.includes("SELECT") && query.includes("FROM characters")) {
        return {
          rows: [{
            character_id: params[0],
            account_player_id: "player-1",
            world_id: 9,
            name: "Echo",
            status: "deleted",
            appearance_json: {},
            scene_id: 100,
            x: 0,
            y: 0,
            dir_x: 0,
            dir_y: 1,
            affinity_earth: 2500,
            affinity_fire: 2500,
            affinity_water: 2500,
            affinity_wind: 2500,
            mastery_earth: 0,
            mastery_fire: 0,
            mastery_water: 0,
            mastery_wind: 0,
            created_at: new Date("2026-06-25T11:00:00.000Z"),
            last_login_at: null,
            deleted_at: new Date("2026-06-25T12:00:00.000Z")
          }]
        };
      }
      if (query.includes("UPDATE characters") && query.includes("deleted_at = NULL")) {
        return {
          rows: [{
            character_id: params[0],
            account_player_id: "player-1",
            world_id: 9,
            name: "Echo",
            status: "active",
            appearance_json: {},
            scene_id: 100,
            x: 0,
            y: 0,
            dir_x: 0,
            dir_y: 1,
            affinity_earth: 2500,
            affinity_fire: 2500,
            affinity_water: 2500,
            affinity_wind: 2500,
            mastery_earth: 0,
            mastery_fire: 0,
            mastery_water: 0,
            mastery_wind: 0,
            created_at: new Date("2026-06-25T11:00:00.000Z"),
            last_login_at: null,
            deleted_at: null
          }],
          rowCount: 1
        };
      }
      throw new Error(`UNEXPECTED_GAME_DB_QUERY: ${query}`);
    }
  };
  const store = new AdminStore(mainPool, null, {}, gamePool);

  const before = await store.findCharacterById("chr_0000000000001", { includeDeleted: true });
  const restored = await store.restoreCharacterForAdmin("chr_0000000000001");

  assert.equal(before.status, "deleted");
  assert.equal(restored.status, "active");
  assert.equal(restored.deleted_at, null);
  assert.equal(gameQueries.length, 2);
  assert.equal(gameQueries.some((entry) => entry.query.includes("COUNT(*)")), false);
  assert.equal(gameQueries[1].query.includes("UPDATE characters"), true);
});

test("operator can update non-ban player status", async () => {
  const store = storeFixture();
  const controller = new PlayersController({}, store);

  const response = await controller.updateStatus(
    "player-1",
    { status: "disabled" },
    request("operator")
  );

  assert.deepEqual(response, { ok: true, message: "Player status updated", banExpiresAt: null });
  assert.deepEqual(store.status, { playerId: "player-1", status: "disabled" });
  assert.equal(store.audits.length, 1);
});

test("operator can approve pending review player", async () => {
  const store = storeFixture();
  store.findPlayerById = async () => ({ id: "player-1", status: "pending_review", banExpiresAt: null });
  const controller = new PlayersController({}, store);

  const response = await controller.updateStatus(
    "player-1",
    { status: "active" },
    request("operator")
  );

  assert.deepEqual(response, { ok: true, message: "Player status updated", banExpiresAt: null });
  assert.deepEqual(store.status, { playerId: "player-1", status: "active" });
  assert.equal(store.audits[0].details.from, "pending_review");
  assert.equal(store.audits[0].details.to, "active");
});

test("operator can reject pending review player", async () => {
  const store = storeFixture();
  store.findPlayerById = async () => ({ id: "player-1", status: "pending_review", banExpiresAt: null });
  const controller = new PlayersController({}, store);

  const response = await controller.updateStatus(
    "player-1",
    { status: "disabled" },
    request("operator")
  );

  assert.deepEqual(response, { ok: true, message: "Player status updated", banExpiresAt: null });
  assert.deepEqual(store.status, { playerId: "player-1", status: "disabled" });
  assert.equal(store.audits[0].details.from, "pending_review");
  assert.equal(store.audits[0].details.to, "disabled");
});

test("operator cannot ban player through status update", async () => {
  const store = storeFixture();
  const controller = new PlayersController({}, store);

  await assert.rejects(
    () => controller.updateStatus("player-1", { status: "banned" }, request("operator")),
    (error) => {
      assert.equal(error.getStatus(), 403);
      assert.deepEqual(error.getResponse(), {
        ok: false,
        error: "INSUFFICIENT_PERMISSION",
        message: "Insufficient permission"
      });
      return true;
    }
  );
  assert.equal(store.status, null);
  assert.equal(store.audits.length, 0);
});

test("invalid player status is rejected", async () => {
  const store = storeFixture();
  const controller = new PlayersController({}, store);

  await assert.rejects(
    () => controller.updateStatus("player-1", { status: "reviewed" }, request("operator")),
    (error) => {
      assert.equal(error.getStatus(), 400);
      assert.deepEqual(error.getResponse(), {
        ok: false,
        error: "INVALID_STATUS",
        message: "status must be active, disabled, banned, or pending_review"
      });
      return true;
    }
  );
  assert.equal(store.status, null);
});
