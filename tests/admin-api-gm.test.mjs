import assert from "node:assert/strict";
import { register } from "node:module";
import path from "node:path";
import { test } from "node:test";
import { pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT = path.resolve("apps/admin-api/tsconfig.json");
process.env.TS_NODE_TRANSPILE_ONLY = "true";
register("ts-node/esm", pathToFileURL("./"));

const { GmController } = await import("../apps/admin-api/src/gm/gm.controller.ts");

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

test("GM ban persists player status and tolerates offline game-server kick", async () => {
  const adminStore = createAdminStore();
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
    gameAdminClient
  );

  const result = await controller.banPlayer(
    { playerId: " player-001 ", durationSeconds: 3600, reason: " cheat " },
    createReq()
  );

  assert.equal(result.ok, true);
  assert.deepEqual(adminStore.updates, [{ playerId: "player-001", status: "banned" }]);
  assert.deepEqual(gameAdminClient.calls, [
    { playerId: "player-001", durationSeconds: 3600, reason: "cheat" }
  ]);
  assert.equal(adminStore.auditLogs.length, 1);
  assert.equal(adminStore.auditLogs[0].action, "gm_ban_player");
  assert.equal(adminStore.auditLogs[0].details.from, "active");
  assert.equal(adminStore.auditLogs[0].details.onlineKick.error, "PLAYER_OFFLINE");
});

test("GM ban rejects missing player before status update and game-server call", async () => {
  const adminStore = createAdminStore(null);
  const gameAdminClient = {
    calls: [],
    async banPlayer(...args) {
      this.calls.push(args);
    }
  };
  const controller = new GmController(
    { trustProxy: false, trustedProxies: [] },
    adminStore,
    gameAdminClient
  );

  await assert.rejects(
    () => controller.banPlayer({ playerId: "player-missing", durationSeconds: 3600 }, createReq()),
    (error) => error.getResponse?.().error === "PLAYER_NOT_FOUND"
  );

  assert.deepEqual(adminStore.updates, []);
  assert.deepEqual(gameAdminClient.calls, []);
  assert.deepEqual(adminStore.auditLogs, []);
});
