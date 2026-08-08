import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { RolloutController } = await import("./rollout.controller.ts");
const { PERMISSIONS_KEY } = await import("../auth/roles.decorator.ts");

function body(overrides = {}) {
  return {
    enabled: true,
    reason: "rolling replacement",
    requestId: "control-request-1",
    preflightNonce: "nonce-1",
    preflightSummarySha256: "summary-1",
    ...overrides
  };
}

function createController({ client = {}, run } = {}) {
  let captured;
  const highRisk = {
    async run(input) {
      captured = input;
      if (run) return run(input);
      return { state: "executed", result: await input.execute() };
    }
  };
  return {
    controller: new RolloutController({}, highRisk, client),
    captured: () => captured
  };
}

test("game-server drain control binds registry target, assertion scope, and audit-safe payload", async () => {
  let options;
  const { controller, captured } = createController({
    client: {
      async updateConfig(key, value, inputOptions) {
        assert.equal(key, "drain_mode");
        assert.equal(value, "on");
        options = inputOptions;
        return { ok: true, errorCode: "", instanceId: "game-server-a" };
      }
    }
  });

  const result = await controller.setGameServerDrain(
    "game-server-a",
    body(),
    { admin: { sub: "admin-7" }, body: body() }
  );

  assert.equal(result.ok, true);
  assert.equal(options.targetInstanceId, "game-server-a");
  assert.equal(options.requireRegistryTarget, true);
  assert.equal(options.assertionContext.permission, "game.config.write");
  assert.equal(options.assertionContext.scope.instanceId, "game-server-a");
  assert.equal(captured().scope.targetType, "config");
  assert.equal(captured().payload.instanceId, "game-server-a");
  assert.doesNotMatch(JSON.stringify(captured().payload), /token|assertion|host|port|password/i);
});

test("game-server shutdown preserves immediate, armed, and blocked result fields", async (t) => {
  const cases = [
    { ok: true, error_code: "", shutdown_armed: true, connection_count: 0, owned_room_count: 0, migrating_room_count: 0 },
    { ok: false, error_code: "SHUTDOWN_CONNECTIONS_REMAIN", shutdown_armed: true, connection_count: 2, owned_room_count: 0, migrating_room_count: 0 },
    { ok: false, error_code: "SHUTDOWN_DRAIN_MODE_REQUIRED", shutdown_armed: false, connection_count: 0, owned_room_count: 0, migrating_room_count: 0 }
  ];
  for (const expected of cases) {
    await t.test(expected.error_code || "immediate", async () => {
      const { controller } = createController({
        client: { async requestServerShutdown() { return expected; } }
      });
      const result = await controller.shutdownGameServer(
        "game-server-a",
        body(),
        { admin: { sub: "admin-7" }, body: body() }
      );
      assert.deepEqual(result, expected);
    });
  }
});

test("game-server control maps unknown targets and downstream timeouts", async (t) => {
  for (const [code, status] of [
    ["GAME_SERVER_ADMIN_TARGET_NOT_FOUND", 404],
    ["GAME_ADMIN_READ_TIMEOUT", 502]
  ]) {
    await t.test(code, async () => {
      const error = Object.assign(new Error(code), { code });
      const { controller } = createController({
        client: { async requestServerShutdown() { throw error; } }
      });
      await assert.rejects(
        controller.shutdownGameServer(
          "game-server-a",
          body(),
          { admin: { sub: "admin-7" }, body: body() }
        ),
        (thrown) => thrown.getStatus() === status
      );
    });
  }
});

test("game-server control rejects invalid targets before downstream calls", async () => {
  const { controller } = createController({ client: {} });
  await assert.rejects(
    controller.setGameServerDrain("../other", body(), { admin: { sub: "admin-7" }, body: body() }),
    (error) => error.getStatus() === 400
  );
});

test("game-server control endpoints require game.config.write", () => {
  const drainPermission = Reflect.getMetadata(PERMISSIONS_KEY, RolloutController.prototype.setGameServerDrain);
  const shutdownPermission = Reflect.getMetadata(PERMISSIONS_KEY, RolloutController.prototype.shutdownGameServer);
  assert.deepEqual(drainPermission, ["game.config.write"]);
  assert.deepEqual(shutdownPermission, ["game.config.write"]);
});
