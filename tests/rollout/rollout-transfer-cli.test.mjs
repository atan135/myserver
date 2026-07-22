import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import test from "node:test";

import {
  parseControlPlaneArgs,
  runControlPlaneRoomTransfer,
  validateControlPlaneOptions
} from "../../tools/rollout/rollout-control-plane-cli.js";
import { parseControlPlaneArgs as parseLegacyEntrypointArgs } from "../../tools/rollout/rollout-transfer-cli.js";

function validArgs(extra = []) {
  return [
    "--world-id", "local",
    "--rollout-epoch", "rollout-test",
    "--room-id", "room-test",
    "--old-server-id", "game-server-001",
    "--new-server-id", "game-server-002",
    "--proxy-instance-id", "game-proxy-001",
    "--backup-reference", "backup-room-test",
    "--request-id", "room-transfer-cli-test",
    "--reason", "controlled transfer",
    ...extra
  ];
}

test("room transfer CLI validates the complete control-plane request", () => {
  const options = parseControlPlaneArgs(validArgs(["--dry-run"]));
  const validation = validateControlPlaneOptions(options);

  assert.equal(validation.ok, true);
  assert.equal(options.execute, false);
  assert.equal(options.dryRun, true);
  assert.equal(options.adminApiToken, "");
  assert.deepEqual(parseLegacyEntrypointArgs(validArgs(["--dry-run"])), options);
});

test("room transfer CLI requires an admin-api JWT outside local dry-run", () => {
  const options = parseControlPlaneArgs(validArgs());
  const validation = validateControlPlaneOptions(options);

  assert.equal(validation.ok, false);
  assert(validation.errors.includes("missing --admin-api-token or ADMIN_API_TOKEN"));
});

test("room transfer CLI executes a nonce-bound control-plane request", async () => {
  const calls = [];
  const originalFetch = globalThis.fetch;
  globalThis.fetch = async (url, init) => {
    calls.push({ url, init: { ...init, body: JSON.parse(init.body) } });
    const body = calls.length === 1
      ? { ok: true, preflight: { nonce: "nonce-1", summarySha256: "a".repeat(64) } }
      : { ok: true, stage: "complete" };
    return { ok: true, json: async () => body };
  };
  try {
    const result = await runControlPlaneRoomTransfer(parseControlPlaneArgs(validArgs([
      "--admin-api-token", "test-jwt",
      "--execute",
      "--request-id", "room-transfer-request-1"
    ])));

    assert.equal(result.ok, true);
    assert.equal(result.stage, "execute");
    assert.equal(calls.length, 2);
    assert.equal(calls[0].init.headers.authorization, "Bearer test-jwt");
    assert.equal(calls[1].init.body.preflightNonce, "nonce-1");
    assert.equal(calls[1].init.body.preflightSummarySha256, "a".repeat(64));
    assert.equal(calls[0].init.body.requestId, calls[1].init.body.requestId);
  } finally {
    globalThis.fetch = originalFetch;
  }
});

test("legacy room transfer entrypoint accepts only control-plane arguments", () => {
  const dryRun = spawnSync(process.execPath, ["tools/rollout/rollout-transfer-cli.js", ...validArgs(["--dry-run"])], {
    cwd: process.cwd(),
    encoding: "utf8"
  });
  assert.equal(dryRun.status, 0, dryRun.stderr);
  assert.match(dryRun.stdout, /"dryRun": true/);

  const rejected = spawnSync(process.execPath, ["tools/rollout/rollout-transfer-cli.js", "--old-admin-token", "legacy"], {
    cwd: process.cwd(),
    encoding: "utf8"
  });
  assert.notEqual(rejected.status, 0);
  assert.match(rejected.stdout, /unknown option --old-admin-token/);
});
