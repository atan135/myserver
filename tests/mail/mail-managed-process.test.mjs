import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";

import {
  resolveProjectExecutable,
  spawnManaged,
  waitForManagedPort
} from "../helpers/managed-process.mjs";

const projectRoot = path.resolve(import.meta.dirname, "..", "..");

test("relative acceptance binary overrides resolve from the project root", () => {
  const fallback = path.join(projectRoot, "target", "debug", "game-server.exe");
  assert.equal(
    resolveProjectExecutable(projectRoot, "target/mail-acceptance-main/debug/game-server.exe", fallback),
    path.join(projectRoot, "target", "mail-acceptance-main", "debug", "game-server.exe")
  );
  assert.equal(resolveProjectExecutable(projectRoot, undefined, fallback), fallback);
});

test("managed port wait reports a spawn error promptly without an uncaught child error", async () => {
  const missing = path.join(projectRoot, "target", "missing-mail-acceptance-binary.exe");
  const processRef = spawnManaged("missing-acceptance-binary", missing, [], { cwd: projectRoot });
  const startedAt = Date.now();

  await assert.rejects(
    waitForManagedPort(1, { processRef, timeoutMs: 5_000, intervalMs: 20 }),
    (error) => {
      assert.match(error.message, /missing-acceptance-binary failed to spawn/);
      assert.equal(error.cause?.code, "ENOENT");
      return true;
    }
  );
  assert.ok(Date.now() - startedAt < 2_000, "spawn errors should not wait for the port timeout");
});
