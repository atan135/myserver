import assert from "node:assert/strict";
import test from "node:test";

import { InternalController } from "./internal.controller.js";

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
