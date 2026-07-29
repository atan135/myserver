import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

const runtimeSources = [
  "apps/auth-http/src/db-client.js",
  "apps/admin-api/src/db-client.js",
  "apps/announce-service/src/db-client.js",
  "apps/mail-service/src/db-client.js",
  "apps/game-server/src/db_store.rs",
  "apps/game-server/src/core/player/db_player_store.rs",
  "apps/game-server/src/core/inventory/reward_delivery.rs",
  "apps/chat-server/src/chat_store.rs"
];

const schemaDdl = /\b(?:CREATE|ALTER|DROP)\s+(?:OR\s+REPLACE\s+)?(?:TABLE|INDEX|TRIGGER|FUNCTION)\b/i;

test("runtime database clients leave schema DDL to migrations", () => {
  for (const source of runtimeSources) {
    const content = readFileSync(join(process.cwd(), source), "utf8");
    assert.doesNotMatch(content, schemaDdl, `${source} must not execute runtime schema DDL`);
  }
});
