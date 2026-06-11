import assert from "node:assert/strict";
import { register } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT = path.resolve("apps/admin-api/tsconfig.json");
process.env.TS_NODE_TRANSPILE_ONLY = "true";
register("ts-node/esm", pathToFileURL("./"));

const { GmController } = await import("../apps/admin-api/src/gm/gm.controller.ts");
const { encodeSubjectToken } = await import("../apps/admin-api/src/nats-client.js");

function createReq() {
  return {
    admin: {
      sub: 1,
      username: "admin",
      role: "admin"
    },
    ip: "10.0.0.5",
    headers: {},
    socket: { remoteAddress: "10.0.0.5" }
  };
}

function createNats({ fail = false } = {}) {
  return {
    publishes: [],
    async publishJson(subject, payload) {
      this.publishes.push({ subject, payload });
      if (fail) {
        const error = new Error("nats unavailable");
        error.code = "NATS_UNAVAILABLE";
        throw error;
      }
    }
  };
}

function createAdminStore(player = { player_id: "player-001", status: "active" }) {
  const updates = [];
  const auditLogs = [];
  return {
    updates,
    auditLogs,
    async findPlayerById(playerId) {
      return player && player.player_id === playerId ? player : null;
    },
    async updatePlayerStatus(playerId, status) {
      updates.push({ playerId, status });
      return Boolean(player && player.player_id === playerId);
    },
    async appendAuditLog(event) {
      auditLogs.push(event);
    }
  };
}

test("GM kick publishes global NATS kick and ignores legacy PLAYER_OFFLINE", async () => {
  const adminStore = createAdminStore();
  const nats = createNats();
  const gameAdminClient = {
    calls: [],
    async kickPlayer(playerId, reason) {
      this.calls.push({ playerId, reason });
      const error = new Error("player is not on this game-server");
      error.code = "PLAYER_OFFLINE";
      throw error;
    }
  };
  const controller = new GmController(
    { trustProxy: false, trustedProxies: [] },
    adminStore,
    nats,
    gameAdminClient
  );

  const result = await controller.kickPlayer(
    { playerId: " player-001 ", reason: " toxic chat " },
    createReq()
  );

  assert.equal(result.ok, true);
  assert.deepEqual(nats.publishes, [
    {
      subject: `myserver.session.kick.${encodeSubjectToken("player-001")}`,
      payload: { player_id: "player-001", reason: "gm_kick:toxic chat" }
    }
  ]);
  assert.deepEqual(gameAdminClient.calls, [
    { playerId: "player-001", reason: "gm_kick:toxic chat" }
  ]);
  assert.equal(result.legacyKick.error, "PLAYER_OFFLINE");
  assert.equal(adminStore.auditLogs.length, 1);
  assert.equal(adminStore.auditLogs[0].details.globalKick.ok, true);
  assert.equal(adminStore.auditLogs[0].details.legacyKick.error, "PLAYER_OFFLINE");
});

test("GM broadcast publishes global NATS broadcast and skips legacy admin TCP", async () => {
  const adminStore = createAdminStore();
  const nats = createNats();
  const gameAdminClient = {
    calls: [],
    async broadcast(...args) {
      this.calls.push(args);
    }
  };
  const controller = new GmController(
    { trustProxy: false, trustedProxies: [] },
    adminStore,
    nats,
    gameAdminClient
  );

  const result = await controller.broadcast(
    { title: " Notice ", content: " Hello all ", sender: " GM " },
    createReq()
  );

  assert.equal(result.ok, true);
  assert.equal(result.globalBroadcast.ok, true);
  assert.equal(result.globalBroadcast.subject, "myserver.gm.broadcast");
  assert.match(result.globalBroadcast.payload.broadcast_id, /^[0-9a-f-]{36}$/);
  assert.equal(result.globalBroadcast.payload.title, "Notice");
  assert.equal(result.globalBroadcast.payload.content, "Hello all");
  assert.equal(result.globalBroadcast.payload.sender, "GM");
  assert.match(result.globalBroadcast.payload.created_at, /^\d{4}-\d{2}-\d{2}T/);
  assert.deepEqual(nats.publishes, [
    {
      subject: "myserver.gm.broadcast",
      payload: result.globalBroadcast.payload
    }
  ]);
  assert.deepEqual(gameAdminClient.calls, []);
  assert.equal(result.legacyBroadcast.skipped, true);
  assert.equal(adminStore.auditLogs.length, 1);
  assert.equal(adminStore.auditLogs[0].details.title, "Notice");
  assert.equal(adminStore.auditLogs[0].details.globalBroadcast.ok, true);
  assert.equal(adminStore.auditLogs[0].details.legacyBroadcast.skipped, true);
});

test("GM broadcast falls back to legacy and returns structured error when NATS fails", async () => {
  const adminStore = createAdminStore();
  const nats = createNats({ fail: true });
  const gameAdminClient = {
    calls: [],
    async broadcast(...args) {
      this.calls.push(args);
    }
  };
  const controller = new GmController(
    { trustProxy: false, trustedProxies: [] },
    adminStore,
    nats,
    gameAdminClient
  );

  await assert.rejects(
    () => controller.broadcast({ title: " Notice ", content: " Hello " }, createReq()),
    (error) => {
      const response = error.getResponse?.();
      assert.equal(error.getStatus?.(), 502);
      assert.equal(response.error, "GM_BROADCAST_PUBLISH_FAILED");
      assert.equal(response.ok, false);
      assert.equal(response.partialDelivered, true);
      assert.equal(response.globalBroadcast.error, "NATS_UNAVAILABLE");
      assert.equal(response.legacyBroadcast.ok, true);
      assert.equal(response.legacyBroadcast.fallback, true);
      return true;
    }
  );

  assert.deepEqual(gameAdminClient.calls, [["Notice", "Hello", "System"]]);
  assert.equal(adminStore.auditLogs.length, 1);
  assert.equal(adminStore.auditLogs[0].details.globalBroadcast.ok, false);
  assert.equal(adminStore.auditLogs[0].details.legacyBroadcast.ok, true);
});

test("GM kick returns structured error when global NATS kick fails", async () => {
  const adminStore = createAdminStore();
  const nats = createNats({ fail: true });
  const gameAdminClient = {
    calls: [],
    async kickPlayer(playerId, reason) {
      this.calls.push({ playerId, reason });
    }
  };
  const controller = new GmController(
    { trustProxy: false, trustedProxies: [] },
    adminStore,
    nats,
    gameAdminClient
  );

  await assert.rejects(
    () => controller.kickPlayer({ playerId: "player-001", reason: "manual" }, createReq()),
    (error) => {
      const response = error.getResponse?.();
      assert.equal(error.getStatus?.(), 502);
      assert.equal(response.error, "SESSION_KICK_PUBLISH_FAILED");
      assert.equal(response.globalKick.error, "NATS_UNAVAILABLE");
      assert.equal(response.legacyResult.ok, true);
      return true;
    }
  );
  assert.equal(adminStore.auditLogs.length, 1);
  assert.equal(adminStore.auditLogs[0].details.globalKick.ok, false);
  assert.deepEqual(gameAdminClient.calls, [
    { playerId: "player-001", reason: "gm_kick:manual" }
  ]);
});

test("GM ban persists player status and tolerates offline game-server kick", async () => {
  const adminStore = createAdminStore();
  const nats = createNats();
  const gameAdminClient = {
    calls: [],
    async banPlayer(playerId, durationSeconds, reason) {
      this.calls.push({ playerId, durationSeconds, reason });
      const error = new Error("failed to ban player on this game-server");
      error.code = "PLAYER_OFFLINE";
      throw error;
    }
  };
  const controller = new GmController(
    { trustProxy: false, trustedProxies: [] },
    adminStore,
    nats,
    gameAdminClient
  );

  const result = await controller.banPlayer(
    { playerId: " player-001 ", durationSeconds: 3600, reason: " cheat " },
    createReq()
  );

  assert.equal(result.ok, true);
  assert.deepEqual(adminStore.updates, [{ playerId: "player-001", status: "banned" }]);
  assert.deepEqual(nats.publishes, [
    {
      subject: `myserver.session.kick.${encodeSubjectToken("player-001")}`,
      payload: { player_id: "player-001", reason: "gm_ban:cheat" }
    }
  ]);
  assert.deepEqual(gameAdminClient.calls, [
    { playerId: "player-001", durationSeconds: 3600, reason: "gm_ban:cheat" }
  ]);
  assert.equal(adminStore.auditLogs.length, 1);
  assert.equal(adminStore.auditLogs[0].action, "gm_ban_player");
  assert.equal(adminStore.auditLogs[0].details.from, "active");
  assert.equal(adminStore.auditLogs[0].details.globalKick.ok, true);
  assert.equal(adminStore.auditLogs[0].details.legacyBan.error, "PLAYER_OFFLINE");
});

test("GM ban keeps banned status and reports audit when global NATS kick fails", async () => {
  const adminStore = createAdminStore();
  const nats = createNats({ fail: true });
  const gameAdminClient = {
    calls: [],
    async banPlayer(playerId, durationSeconds, reason) {
      this.calls.push({ playerId, durationSeconds, reason });
    }
  };
  const controller = new GmController(
    { trustProxy: false, trustedProxies: [] },
    adminStore,
    nats,
    gameAdminClient
  );

  const result = await controller.banPlayer(
    { playerId: " player-001 ", durationSeconds: 7200, reason: " exploit " },
    createReq()
  );

  assert.equal(result.ok, false);
  assert.equal(result.error, "SESSION_KICK_PUBLISH_FAILED");
  assert.equal(result.banStatus, "banned");
  assert.equal(result.globalKick.error, "NATS_UNAVAILABLE");
  assert.deepEqual(adminStore.updates, [{ playerId: "player-001", status: "banned" }]);
  assert.equal(adminStore.auditLogs.length, 1);
  assert.equal(adminStore.auditLogs[0].details.globalKick.ok, false);
  assert.equal(adminStore.auditLogs[0].details.legacyBan.ok, true);
  assert.deepEqual(gameAdminClient.calls, [
    { playerId: "player-001", durationSeconds: 7200, reason: "gm_ban:exploit" }
  ]);
});

test("GM ban rejects missing player before status update and game-server call", async () => {
  const adminStore = createAdminStore(null);
  const nats = createNats();
  const gameAdminClient = {
    calls: [],
    async banPlayer(...args) {
      this.calls.push(args);
    }
  };
  const controller = new GmController(
    { trustProxy: false, trustedProxies: [] },
    adminStore,
    nats,
    gameAdminClient
  );

  await assert.rejects(
    () => controller.banPlayer({ playerId: "player-missing", durationSeconds: 3600 }, createReq()),
    (error) => error.getResponse?.().error === "PLAYER_NOT_FOUND"
  );

  assert.deepEqual(adminStore.updates, []);
  assert.deepEqual(nats.publishes, []);
  assert.deepEqual(gameAdminClient.calls, []);
  assert.deepEqual(adminStore.auditLogs, []);
});
