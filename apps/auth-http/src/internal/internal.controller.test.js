import assert from "node:assert/strict";
import test from "node:test";

process.env.TS_NODE_TRANSPILE_ONLY ??= "true";

const { InternalCharactersController } = await import("./internal-characters.controller.js");
const { InternalController } = await import("./internal.controller.js");

const request = { headers: { "x-service-token": "internal-test-token" } };

function createController() {
  const downstream = {
    getServerStatus: async () => ({ status: "ok" }),
    getRolloutDrainStatus: async () => ({ ok: true }),
    requestServerShutdown: async () => {
      throw new Error("retired shutdown route must not call game-server");
    },
    updateConfig: async () => {
      throw new Error("retired config route must not call game-server");
    }
  };
  return new InternalController({ internalApiToken: "internal-test-token", strictSecurity: true }, downstream);
}

async function expectControlPlaneOnly(action) {
  await assert.rejects(action, (error) => {
    assert.equal(error.getStatus(), 410);
    assert.deepEqual(error.getResponse(), {
      ok: false,
      error: "CONTROL_PLANE_ONLY",
      message: "Game-server write operations are available only through admin-api"
    });
    return true;
  });
}

test("auth-http retired shutdown route never calls game-server", async () => {
  await expectControlPlaneOnly(() => createController().shutdownIfDrained(request));
});

test("auth-http retired config route never calls game-server", async () => {
  await expectControlPlaneOnly(() => createController().updateConfig(request));
});

test("auth-http internal character creation requires a valid service token", async () => {
  const controller = new InternalCharactersController(
    { internalApiToken: "internal-test-token", strictSecurity: true },
    { createForAdmin: async () => ({ ok: true }) }
  );

  await assert.rejects(
    () => controller.create({ headers: {} }, {}),
    (error) => {
      assert.equal(error.getStatus(), 401);
      assert.equal(error.getResponse().error, "INVALID_SERVICE_TOKEN");
      return true;
    }
  );
});

test("auth-http internal character creation delegates trusted payload to CharactersService", async () => {
  const calls = [];
  const controller = new InternalCharactersController(
    { internalApiToken: "internal-test-token", strictSecurity: true },
    {
      async createForAdmin(body) {
        calls.push(body);
        return { ok: true, character: { character_id: "chr_0000000000001" } };
      }
    }
  );
  const body = {
    accountPlayerId: "plr_0000000000001",
    name: "Echo",
    adminActor: "operator",
    reason: "support request"
  };

  const result = await controller.create(request, body);

  assert.deepEqual(calls, [body]);
  assert.equal(result.character.character_id, "chr_0000000000001");
});
