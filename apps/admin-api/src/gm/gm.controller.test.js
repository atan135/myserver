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
    }
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
