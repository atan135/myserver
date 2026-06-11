import assert from "node:assert/strict";
import test from "node:test";

import { AdminStore } from "./admin-store.js";
import { computeBanExpiresAt } from "./ban-utils.js";

test("computeBanExpiresAt returns an ISO timestamp duration seconds after now", () => {
  const now = new Date("2026-06-11T00:00:00.000Z");

  assert.equal(computeBanExpiresAt(3600, now), "2026-06-11T01:00:00.000Z");
});

test("updatePlayerStatus writes ban_expires_at for timed ban", async () => {
  let captured = null;
  const store = new AdminStore({
    async execute(query, params) {
      captured = { query, params };
      return [{ affectedRows: 1 }];
    }
  });

  const updated = await store.updatePlayerStatus("player-1", "banned", {
    banExpiresAt: "2026-06-11T01:00:00.000Z"
  });

  assert.equal(updated, true);
  assert.match(captured.query, /ban_expires_at = \?/);
  assert.deepEqual(captured.params, ["banned", "2026-06-11T01:00:00.000Z", "player-1"]);
});

test("updatePlayerStatus clears ban_expires_at for active or disabled", async () => {
  const calls = [];
  const store = new AdminStore({
    async execute(query, params) {
      calls.push(params);
      return [{ affectedRows: 1 }];
    }
  });

  await store.updatePlayerStatus("player-1", "active");
  await store.updatePlayerStatus("player-2", "disabled");

  assert.deepEqual(calls, [
    ["active", null, "player-1"],
    ["disabled", null, "player-2"]
  ]);
});
