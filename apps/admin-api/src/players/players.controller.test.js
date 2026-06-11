import assert from "node:assert/strict";
import { register } from "node:module";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../../tsconfig.json", import.meta.url));
process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
register("ts-node/esm", pathToFileURL("./"));

const { PlayersController } = await import("./players.controller.ts");

function storeFixture() {
  return {
    status: null,
    audits: [],
    async findPlayerById() {
      return { id: "player-1", status: "active", banExpiresAt: null };
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
