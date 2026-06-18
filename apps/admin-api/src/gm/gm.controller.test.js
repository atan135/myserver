import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { GmController } = await import("./gm.controller.ts");

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

function makeController(gameAdminClient) {
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
    }
  };
  const nats = {
    calls: natsCalls,
    async publishJson(subject, payload) {
      natsCalls.push({ subject, payload });
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
      { playerId: "plr_1", itemId: "item_1", itemCount: 1 },
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
  const { controller, audits } = makeController({
    async sendItem(_playerId, _itemId, _itemCount, _reason, options) {
      capturedOptions = options;
      return { ok: true, instanceId: options.targetInstanceId };
    }
  });

  const result = await controller.sendItem(
    {
      playerId: "plr_1",
      itemId: "item_1",
      itemCount: 2,
      targetInstanceId: "game-server-b"
    },
    makeReq()
  );

  assert.equal(result.ok, true);
  assert.equal(capturedOptions.targetInstanceId, "game-server-b");
  assert.equal(capturedOptions.actor, "ops");
  assert.equal(audits[0].details.targetInstanceId, "game-server-b");
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
