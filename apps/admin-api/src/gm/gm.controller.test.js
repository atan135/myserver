import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { GmController } = await import("./gm.controller.ts");
const { AdminStore } = await import("../admin-store.js");

function makeReq() {
  return {
    admin: {
      sub: "1",
      username: "ops"
    },
    ip: "127.0.0.1",
    headers: {},
    socket: { remoteAddress: "127.0.0.1" }
  };
}

function endpointSummary(instanceId, host = "10.0.0.2", port = 7501) {
  return {
    service: "game-server",
    instanceId,
    instance_id: instanceId,
    endpointName: "admin",
    endpoint_name: "admin",
    protocol: "tcp",
    host,
    port,
    healthy: true,
    fallback: false,
    source: "registry",
    reason: "discovered"
  };
}

function makeController(gameAdminClient, options = {}) {
  const audits = [];
  const natsCalls = [];
  const adminStore = {
    audits,
    async appendAuditLog(entry) {
      audits.push(entry);
    },
    async findPlayerById(playerId) {
      return { id: playerId, status: "active" };
    },
    async updatePlayerStatus() {
      return true;
    },
    ...(options.adminStore || {})
  };
  const nats = {
    calls: natsCalls,
    async publishJson(subject, payload) {
      natsCalls.push({ subject, payload });
      if (options.publishJson) {
        return options.publishJson(subject, payload);
      }
      return { ok: true };
    }
  };

  return {
    controller: new GmController({}, adminStore, nats, gameAdminClient),
    audits,
    nats
  };
}

test("send-item returns explicit target required error from GameAdminClient", async () => {
  const error = new Error("multiple game-server admin endpoints are available; targetInstanceId is required");
  error.code = "GAME_SERVER_ADMIN_TARGET_REQUIRED";
  const { controller } = makeController({
    async sendItem() {
      throw error;
    }
  });

  await assert.rejects(
    controller.sendItem(
      { characterId: "chr_1", itemId: "item_1", itemCount: 1 },
      makeReq()
    ),
    (caught) => {
      assert.equal(caught.getStatus(), 400);
      assert.equal(caught.getResponse().error, "GAME_SERVER_ADMIN_TARGET_REQUIRED");
      return true;
    }
  );
});

test("send-item passes explicit targetInstanceId to GameAdminClient", async () => {
  let capturedOptions = null;
  let capturedCharacterId = null;
  const resolvedEndpoint = endpointSummary("game-server-resolved", "10.0.0.9", 7599);
  const { controller, audits } = makeController({
    async sendItem(characterId, _itemId, _itemCount, _reason, options) {
      capturedCharacterId = characterId;
      capturedOptions = options;
      return { ok: true, instanceId: resolvedEndpoint.instanceId, endpoint: resolvedEndpoint };
    }
  });

  const result = await controller.sendItem(
    {
      characterId: " chr_1 ",
      itemId: "item_1",
      itemCount: 2,
      targetInstanceId: "game-server-b"
    },
    makeReq()
  );

  assert.equal(result.ok, true);
  assert.equal(capturedCharacterId, "chr_1");
  assert.equal(capturedOptions.targetInstanceId, "game-server-b");
  assert.equal(capturedOptions.actor, "ops");
  assert.equal(audits[0].targetType, "character");
  assert.equal(audits[0].targetValue, "chr_1");
  assert.equal(audits[0].details.requestedTargetInstanceId, "game-server-b");
  assert.equal(audits[0].details.gameAdmin.instanceId, "game-server-resolved");
  assert.deepEqual(audits[0].details.gameAdmin.endpoint, resolvedEndpoint);
  assert.equal(audits[0].details.targetInstanceId, undefined);
});

test("send-item rejects legacy playerId target field", async () => {
  let called = false;
  const { controller } = makeController({
    async sendItem() {
      called = true;
      return { ok: true };
    }
  });

  await assert.rejects(
    controller.sendItem(
      { playerId: "plr_1", itemId: "item_1", itemCount: 1 },
      makeReq()
    ),
    (caught) => {
      assert.equal(caught.getStatus(), 400);
      assert.equal(caught.getResponse().error, "INVALID_CHARACTER_ID");
      return true;
    }
  );
  assert.equal(called, false);
});

test("large GM item grant produces a redacted security audit event", async () => {
  const securityAudits = [];
  const { controller } = makeController({
    async sendItem() {
      return { ok: true };
    }
  }, {
    adminStore: {
      async countRecentAdminAuditActions() {
        return 1;
      },
      async appendSecurityAuditLog(entry) {
        securityAudits.push(entry);
      }
    }
  });

  await controller.sendItem(
    { characterId: "chr_1", itemId: "1001", itemCount: 10_000, reason: "restore" },
    makeReq()
  );

  assert.equal(securityAudits.length, 1);
  assert.equal(securityAudits[0].eventType, "asset_grant_anomaly");
  assert.equal(securityAudits[0].severity, "critical");
  assert.equal(securityAudits[0].details.itemCount, 10_000);
  assert.equal("reason" in securityAudits[0].details, false);
});

test("emergency compensation uses its dedicated source, request ID, permission audit, and security event", async () => {
  let captured = null;
  const securityAudits = [];
  const { controller, audits } = makeController({
    async sendItem(characterId, itemId, itemCount, reason, options) {
      captured = { characterId, itemId, itemCount, reason, options };
      return { ok: true };
    }
  }, {
    adminStore: {
      async countRecentAdminAuditActions() {
        return 1;
      },
      async appendSecurityAuditLog(entry) {
        securityAudits.push(entry);
      }
    }
  });

  const result = await controller.emergencyCompensateItem(
    { characterId: "chr_1", itemId: "1001", itemCount: 2, reason: "incident-42" },
    makeReq()
  );

  assert.equal(result.ok, true);
  assert.equal(captured.options.source, "gm-emergency-correction");
  assert.match(captured.options.requestId, /^gm-emergency-correction:/);
  assert.equal(captured.options.actor, "ops");
  assert.equal(audits[0].action, "gm_emergency_asset_correction");
  assert.equal(audits[0].details.permission, "gm.asset_correction.emergency");
  assert.equal(securityAudits[0].eventType, "asset_emergency_correction");
  assert.equal(securityAudits[0].severity, "critical");
});

test("kick-player returns explicit target required error from GameAdminClient", async () => {
  const error = new Error("multiple game-server admin endpoints are available; targetInstanceId is required");
  error.code = "GAME_SERVER_ADMIN_TARGET_REQUIRED";
  const { controller, nats } = makeController({
    async resolveAdminEndpoint() {
      throw error;
    },
    async kickPlayer() {
      throw new Error("kickPlayer should not be called");
    }
  });

  await assert.rejects(
    controller.kickPlayer(
      { playerId: "plr_1" },
      makeReq()
    ),
    (caught) => {
      assert.equal(caught.getStatus(), 400);
      assert.equal(caught.getResponse().error, "GAME_SERVER_ADMIN_TARGET_REQUIRED");
      return true;
    }
  );
  assert.equal(nats.calls.length, 0);
});

test("ban-player returns explicit target required error from GameAdminClient", async () => {
  const error = new Error("multiple game-server admin endpoints are available; targetInstanceId is required");
  error.code = "GAME_SERVER_ADMIN_TARGET_REQUIRED";
  const { controller, nats } = makeController({
    async resolveAdminEndpoint() {
      throw error;
    },
    async banPlayer() {
      throw new Error("banPlayer should not be called");
    }
  });

  await assert.rejects(
    controller.banPlayer(
      { playerId: "plr_1", durationSeconds: 3600 },
      makeReq()
    ),
    (caught) => {
      assert.equal(caught.getStatus(), 400);
      assert.equal(caught.getResponse().error, "GAME_SERVER_ADMIN_TARGET_REQUIRED");
      return true;
    }
  );
  assert.equal(nats.calls.length, 0);
});

test("kick-player returns target not found error from GameAdminClient", async () => {
  const error = new Error("game-server admin target instance not found: game-server-missing");
  error.code = "GAME_SERVER_ADMIN_TARGET_NOT_FOUND";
  const { controller, nats } = makeController({
    async resolveAdminEndpoint() {
      throw error;
    },
    async kickPlayer() {
      throw new Error("kickPlayer should not be called");
    }
  });

  await assert.rejects(
    controller.kickPlayer(
      { playerId: "plr_1", targetInstanceId: "game-server-missing" },
      makeReq()
    ),
    (caught) => {
      assert.equal(caught.getStatus(), 404);
      assert.equal(caught.getResponse().error, "GAME_SERVER_ADMIN_TARGET_NOT_FOUND");
      return true;
    }
  );
  assert.equal(nats.calls.length, 0);
});

test("kick-player audit records resolved game-server admin endpoint", async () => {
  const resolvedEndpoint = endpointSummary("game-server-a", "10.0.0.1", 7500);
  let capturedOptions = null;
  const { controller, audits } = makeController({
    async resolveAdminEndpoint(options) {
      assert.equal(options.targetInstanceId, "requested-game-server");
      assert.equal(options.requireExplicitTarget, true);
      return resolvedEndpoint;
    },
    async kickPlayer(_playerId, _reason, options) {
      capturedOptions = options;
      return { ok: true, instanceId: resolvedEndpoint.instanceId, endpoint: resolvedEndpoint };
    }
  });

  const result = await controller.kickPlayer(
    { playerId: "plr_1", reason: "duplicate login", targetInstanceId: "requested-game-server" },
    makeReq()
  );

  assert.equal(result.ok, true);
  assert.equal(capturedOptions.endpoint, resolvedEndpoint);
  assert.equal(audits[0].details.requestedTargetInstanceId, "requested-game-server");
  assert.equal(audits[0].details.legacyKick.instanceId, "game-server-a");
  assert.deepEqual(audits[0].details.legacyKick.endpoint, resolvedEndpoint);
  assert.equal(audits[0].details.targetInstanceId, undefined);
});

test("ban-player audit records resolved game-server admin endpoint", async () => {
  const resolvedEndpoint = endpointSummary("game-server-ban", "10.0.0.3", 7503);
  const { controller, audits } = makeController({
    async resolveAdminEndpoint(options) {
      assert.equal(options.targetInstanceId, "game-server-requested");
      assert.equal(options.requireExplicitTarget, true);
      return resolvedEndpoint;
    },
    async banPlayer(_playerId, _durationSeconds, _reason, options) {
      assert.equal(options.endpoint, resolvedEndpoint);
      return { ok: true, instanceId: resolvedEndpoint.instanceId, endpoint: resolvedEndpoint };
    }
  });

  const result = await controller.banPlayer(
    { playerId: "plr_1", durationSeconds: 3600, reason: "abuse", targetInstanceId: "game-server-requested" },
    makeReq()
  );

  assert.equal(result.ok, true);
  assert.equal(audits[0].details.requestedTargetInstanceId, "game-server-requested");
  assert.equal(audits[0].details.legacyBan.instanceId, "game-server-ban");
  assert.deepEqual(audits[0].details.legacyBan.endpoint, resolvedEndpoint);
  assert.equal(audits[0].details.targetInstanceId, undefined);
});

test("broadcast legacy fallback audit records all called game-server endpoints", async () => {
  const endpoints = [
    endpointSummary("game-server-a", "10.0.0.1", 7500),
    endpointSummary("game-server-b", "10.0.0.2", 7501)
  ];
  const { controller, audits } = makeController(
    {
      async broadcast(_title, _content, _sender, options) {
        assert.equal(options.targetInstanceId, undefined);
        return {
          ok: true,
          instances: endpoints.map((endpoint) => ({
            ok: true,
            instanceId: endpoint.instanceId,
            endpoint
          }))
        };
      }
    },
    {
      publishJson() {
        const error = new Error("nats unavailable");
        error.code = "NATS_DOWN";
        throw error;
      }
    }
  );

  await assert.rejects(
    controller.broadcast(
      { title: "Notice", content: "Server restart", sender: "Ops" },
      makeReq()
    ),
    (caught) => {
      assert.equal(caught.getStatus(), 502);
      assert.equal(caught.getResponse().error, "GM_BROADCAST_PUBLISH_FAILED");
      return true;
    }
  );

  assert.equal(audits[0].details.requestedTargetInstanceId, undefined);
  assert.deepEqual(
    audits[0].details.legacyBroadcast.instances.map((instance) => instance.endpoint),
    endpoints
  );
  assert.deepEqual(
    audits[0].details.legacyBroadcast.instances.map((instance) => instance.instanceId),
    ["game-server-a", "game-server-b"]
  );
  assert.equal(audits[0].details.legacyBroadcast.fallback, true);
});

test("GM character element set validates, writes admin audit, and returns deltas", async () => {
  let capturedInput = null;
  const { controller, audits } = makeController({}, {
    adminStore: {
      async setCharacterElementsForAdmin(input) {
        capturedInput = input;
        return {
          changed: true,
          character: {
            character_id: input.characterId,
            attributes: {
              affinity: input.affinity,
              mastery: input.mastery
            }
          },
          before: {
            character_id: input.characterId,
            affinity: { earth: 2500, fire: 2500, water: 2500, wind: 2500 },
            mastery: { earth: 0, fire: 0, water: 0, wind: 0 }
          },
          after: {
            character_id: input.characterId,
            affinity: input.affinity,
            mastery: input.mastery
          },
          affinityDelta: { earth: -100, fire: 100, water: 0, wind: 0 },
          masteryDelta: { earth: 0, fire: 10, water: 0, wind: 0 }
        };
      }
    }
  });

  const response = await controller.setCharacterElements(
    " chr_1 ",
    {
      affinity: { earth: 2400, fire: 2600, water: 2500, wind: 2500 },
      mastery: { earth: 0, fire: 10, water: 0, wind: 0 },
      reason: "support adjust"
    },
    makeReq()
  );

  assert.equal(response.ok, true);
  assert.equal(response.changed, true);
  assert.equal(capturedInput.characterId, "chr_1");
  assert.equal(capturedInput.operatorType, "admin");
  assert.equal(capturedInput.operatorId, "1");
  assert.equal(capturedInput.sourceId, "admin-api-character-elements");
  assert.equal(audits.length, 1);
  assert.equal(audits[0].action, "gm_character_elements_set");
  assert.equal(audits[0].targetType, "character");
  assert.equal(audits[0].targetValue, "chr_1");
  assert.equal(audits[0].details.permission, "gm.character_elements.write");
  assert.equal(audits[0].details.affinityDelta.fire, 100);
});

test("GM character element set rejects invalid affinity total and writes failed audit", async () => {
  const { controller, audits } = makeController({});

  await assert.rejects(
    controller.setCharacterElements(
      "chr_1",
      {
        affinity: { earth: 2500, fire: 2500, water: 2500, wind: 2400 },
        reason: "bad adjust"
      },
      makeReq()
    ),
    (caught) => {
      assert.equal(caught.getStatus(), 400);
      assert.equal(caught.getResponse().error, "INVALID_AFFINITY_TOTAL");
      return true;
    }
  );

  assert.equal(audits.length, 1);
  assert.equal(audits[0].action, "gm_character_elements_set_failed");
  assert.equal(audits[0].details.error, "INVALID_AFFINITY_TOTAL");
});

test("GM character title grant checks config and records audit", async () => {
  let capturedInput = null;
  const { controller, audits } = makeController({}, {
    adminStore: {
      async applyCharacterTitleForAdmin(input) {
        capturedInput = input;
        return {
          action: "grant",
          status: "granted",
          changed: true,
          title: { character_id: input.characterId, title_id: input.titleId, is_equipped: false },
          before: null,
          after: { character_id: input.characterId, title_id: input.titleId, is_equipped: false }
        };
      }
    }
  });

  const response = await controller.applyCharacterTitle(
    "chr_1",
    { action: "grant", titleId: "9001", reason: "support title" },
    makeReq()
  );

  assert.equal(response.ok, true);
  assert.equal(response.status, "granted");
  assert.equal(capturedInput.titleId, "9001");
  assert.equal(capturedInput.sourceId, "admin-api-character-titles");
  assert.equal(audits[0].action, "gm_character_title_apply");
  assert.equal(audits[0].details.gmAction, "grant");
  assert.equal(audits[0].details.permission, "gm.character_titles.write");
});

test("GM limited title grant requires expiresAt and writes failed audit", async () => {
  const { controller, audits } = makeController({});

  await assert.rejects(
    controller.applyCharacterTitle(
      "chr_1",
      { action: "grant", titleId: "9101", reason: "support title" },
      makeReq()
    ),
    (caught) => {
      assert.equal(caught.getStatus(), 400);
      assert.equal(caught.getResponse().error, "LIMITED_TITLE_REQUIRES_EXPIRES_AT");
      return true;
    }
  );

  assert.equal(audits.length, 1);
  assert.equal(audits[0].action, "gm_character_title_apply_failed");
  assert.equal(audits[0].details.error, "LIMITED_TITLE_REQUIRES_EXPIRES_AT");
});

test("GM character discipline set writes admin audit", async () => {
  let capturedInput = null;
  const { controller, audits } = makeController({}, {
    adminStore: {
      async setCharacterDisciplineForAdmin(input) {
        capturedInput = input;
        return {
          action: "upgrade",
          status: "updated",
          changed: true,
          discipline: {
            character_id: input.characterId,
            discipline_id: input.disciplineId,
            points: input.points,
            tier: input.tier,
            active: input.active
          },
          before: null,
          after: {
            character_id: input.characterId,
            discipline_id: input.disciplineId,
            points: input.points,
            tier: input.tier,
            active: input.active
          }
        };
      }
    }
  });

  const response = await controller.setCharacterDiscipline(
    "chr_1",
    {
      disciplineId: "forging",
      points: 120,
      tier: "apprentice",
      active: true,
      reason: "support discipline"
    },
    makeReq()
  );

  assert.equal(response.ok, true);
  assert.equal(response.changed, true);
  assert.equal(capturedInput.disciplineId, "forging");
  assert.equal(capturedInput.operatorId, "1");
  assert.equal(audits[0].action, "gm_character_discipline_set");
  assert.equal(audits[0].details.permission, "gm.character_disciplines.write");
});

test("GM unlock check requires both title and discipline audit context", async () => {
  const { controller, audits } = makeController({}, {
    adminStore: {
      async runCharacterUnlockCheckForAdmin(input) {
        assert.equal(input.characterId, "chr_1");
        assert.equal(input.operatorId, "1");
        assert.ok(input.titleDefinitions["2001"]);
        return {
          characterId: input.characterId,
          checked: 2,
          granted: 1,
          results: [{ title_id: "2001", status: "granted", changed: true }]
        };
      }
    }
  });

  const response = await controller.runCharacterUnlockCheck(
    "chr_1",
    { reason: "support unlock" },
    makeReq()
  );

  assert.equal(response.ok, true);
  assert.equal(response.granted, 1);
  assert.equal(audits[0].action, "gm_character_unlock_check");
  assert.deepEqual(audits[0].details.permission, [
    "gm.character_titles.write",
    "gm.character_disciplines.write"
  ]);
});

test("AdminStore element GM update writes characters and character_element_logs transactionally", async () => {
  const queries = [];
  const row = {
    character_id: "chr_1",
    account_player_id: "player-1",
    world_id: 0,
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
  };
  const client = {
    async query(query, params = []) {
      queries.push({ query, params });
      if (query === "BEGIN" || query === "COMMIT" || query === "ROLLBACK") {
        return { rows: [], rowCount: 0 };
      }
      if (query.includes("FOR UPDATE") && query.includes("FROM characters")) {
        return { rows: [row] };
      }
      if (query.includes("UPDATE characters")) {
        return {
          rows: [{
            ...row,
            affinity_earth: params[0],
            affinity_fire: params[1],
            affinity_water: params[2],
            affinity_wind: params[3],
            mastery_earth: params[4],
            mastery_fire: params[5],
            mastery_water: params[6],
            mastery_wind: params[7]
          }],
          rowCount: 1
        };
      }
      if (query.includes("INSERT INTO character_element_logs")) {
        return { rows: [], rowCount: 1 };
      }
      throw new Error(`UNEXPECTED_QUERY: ${query}`);
    },
    release() {}
  };
  const gamePool = {
    async connect() {
      return client;
    }
  };
  const store = new AdminStore({ async query() { throw new Error("UNEXPECTED_MAIN_QUERY"); } }, null, {}, gamePool);

  const result = await store.setCharacterElementsForAdmin({
    characterId: "chr_1",
    affinity: { earth: 2400, fire: 2600, water: 2500, wind: 2500 },
    mastery: { earth: 0, fire: 10, water: 0, wind: 0 },
    operatorType: "admin",
    operatorId: "1",
    reason: "support adjust"
  });

  assert.equal(result.changed, true);
  assert.deepEqual(result.affinityDelta, { earth: -100, fire: 100, water: 0, wind: 0 });
  assert.deepEqual(result.masteryDelta, { earth: 0, fire: 10, water: 0, wind: 0 });
  assert.equal(queries[0].query, "BEGIN");
  assert.equal(queries.at(-1).query, "COMMIT");
  const logQuery = queries.find((entry) => entry.query.includes("INSERT INTO character_element_logs"));
  assert.ok(logQuery);
  assert.equal(logQuery.params[0], "chr_1");
  assert.equal(logQuery.params[1], "gm");
  assert.equal(logQuery.params[4], "1");
  assert.equal(logQuery.params[5], -100);
  assert.equal(logQuery.params[9], 0);
  assert.equal(logQuery.params[10], 10);
  assert.equal(logQuery.params[15], "support adjust");
});
