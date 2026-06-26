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
    titleQuery: null,
    async findPlayerById() {
      return { id: "player-1", status: "active", banExpiresAt: null };
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
