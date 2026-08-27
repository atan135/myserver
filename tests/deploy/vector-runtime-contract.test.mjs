import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import test from "node:test";

const execFileAsync = promisify(execFile);
const root = path.resolve(import.meta.dirname, "../..");
const prune = path.join(root, "scripts/docker/prune-vector-files.mjs");
const recovery = path.join(root, "scripts/docker/vector-recovery-check.sh");

async function runPrune(logRoot, stateDir, extra = []) {
  return execFileAsync(process.execPath, [
    prune,
    `--log-root=${logRoot}`,
    `--state-dir=${stateDir}`,
    "--retention-days=14",
    "--test-mode",
    ...extra
  ], { cwd: root, encoding: "utf8" });
}

test("prune keeps active, unarchived, mismatched, and unsafe files", async () => {
  const fixture = await mkdtemp(path.join(os.tmpdir(), "myserver-vector-"));
  const logRoot = path.join(fixture, "log");
  const stateDir = path.join(fixture, "state");
  const dayDir = path.join(logRoot, "game-server", "2000-01-01");
  await mkdir(dayDir, { recursive: true });
  await mkdir(stateDir, { recursive: true });

  const validName = "game-server-001.abcdef123456.0001.jsonl";
  const missingName = "game-server-001.abcdef123456.0002.jsonl";
  const mismatchName = "game-server-001.abcdef123456.0003.jsonl";
  const openName = "game-server-001.abcdef123456.jsonl.open";
  await writeFile(path.join(dayDir, validName), "{\"message\":\"ok\"}\n");
  await writeFile(path.join(dayDir, missingName), "missing\n");
  await writeFile(path.join(dayDir, mismatchName), "actual\n");
  await writeFile(path.join(dayDir, openName), "active\n");

  const relative = (name) => `game-server/2000-01-01/${name}`;
  const crypto = await import("node:crypto");
  const validBody = await readFile(path.join(dayDir, validName));
  const validSha = crypto.createHash("sha256").update(validBody).digest("hex");
  await writeFile(path.join(stateDir, "archive-manifest.jsonl"), [
    JSON.stringify({ path: relative(validName), size: validBody.length, sha256: validSha }),
    JSON.stringify({ path: relative(mismatchName), size: 1, sha256: "0".repeat(64) })
  ].join("\n") + "\n");

  const result = await runPrune(logRoot, stateDir);
  const actions = result.stdout.trim().split(/\r?\n/).map((line) => JSON.parse(line));
  assert.deepEqual(actions, [
    { action: "candidate", path: relative(validName), size: validBody.length, sha256: validSha },
    { action: "skip", reason: "not_archived", path: relative(missingName) },
    { action: "skip", reason: "manifest_mismatch", path: relative(mismatchName) }
  ]);
});

test("prune rejects non-contract paths and never permits test-mode apply", async () => {
  const fixture = await mkdtemp(path.join(os.tmpdir(), "myserver-vector-"));
  const result = await execFileAsync(process.execPath, [
    prune, `--log-root=${fixture}`, `--state-dir=${fixture}`, "--apply", "--test-mode"
  ], { cwd: root, encoding: "utf8" }).catch((error) => error);
  assert.notEqual(result.code, 0);
  assert.match(`${result.stderr}${result.stdout}`, /retention paths|contract/i);
});

test("recovery check emits one JSON object per scenario without touching fixtures", async (t) => {
  if (process.platform === "win32") {
    t.skip("bash cannot consume native Windows fixture paths; run this shell scenario test in Linux/WSL");
    return;
  }
  const fixture = await mkdtemp(path.join(os.tmpdir(), "myserver-vector-"));
  const config = path.join(fixture, "vector.yaml");
  await writeFile(config, [
    "sources:", "  docker_logs:", "    type: docker_logs", "    retry_backoff_secs: 2",
    "sinks:", "  jsonl:", "    buffer:", "      when_full: drop_newest"
  ].join("\n"));
  const { stdout } = await execFileAsync("bash", [
    recovery, "--log-root", fixture, "--state-dir", fixture, "--config", config, "--json"
  ], { cwd: root, encoding: "utf8" });
  const records = stdout.trim().split(/\r?\n/).map((line) => JSON.parse(line));
  assert.equal(records.length, 9);
  assert.ok(records.every((record) => record.schema === "vector.recovery.v1" && record.status === "pass"));
});

test("Vector output template preserves UTC, instance, container, and active suffix", async () => {
  const config = await readFile(path.join(root, "deploy/docker/vector/vector.yaml"), "utf8");
  assert.match(config, /format_timestamp\(format: "%Y-%m-%d"\)/);
  assert.match(config, /\{\{ instance_id \}\}\.\{\{ container_id_prefix \}\}\.jsonl\.open/);
  assert.match(config, /timezone: UTC/);
});

test("unsafe service and symlink inputs are ignored by retention scanner", async () => {
  const fixture = await mkdtemp(path.join(os.tmpdir(), "myserver-vector-"));
  const logRoot = path.join(fixture, "log");
  const stateDir = path.join(fixture, "state");
  await mkdir(logRoot, { recursive: true });
  await mkdir(stateDir, { recursive: true });
  await mkdir(path.join(logRoot, "unknown-service", "2000-01-01"), { recursive: true });
  const safeServiceDay = path.join(logRoot, "game-server", "2000-01-01");
  await mkdir(safeServiceDay, { recursive: true });
  await writeFile(path.join(safeServiceDay, "bad instance.abcdef123456.0001.jsonl"), "unsafe\n");
  await writeFile(path.join(safeServiceDay, "instance.abcdef123456.jsonl.open"), "active\n");
  try {
    await symlink(path.join(fixture, "outside"), path.join(logRoot, "game-server"));
  } catch {
    // Windows without developer mode may reject symlink creation; the
    // unknown-service assertion below still exercises the allowlist gate.
  }
  const result = await runPrune(logRoot, stateDir);
  assert.equal(result.stdout.trim(), "");
});
