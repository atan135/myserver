import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { chmod, copyFile, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const common = path.join(root, "deploy/docker/scripts/ops-common.sh");
const installer = path.join(root, "scripts/docker/install-ops-scripts.sh");
const opsScripts = [
  "ops-common.sh", "ops-deploy.sh", "ops-disk-report.sh", "ops-health.sh", "ops-logs.sh",
  "ops-replace.sh", "ops-restart.sh", "ops-retire.sh", "ops-rollback.sh", "ops-status.sh"
];
const linuxOnly = { skip: process.platform === "win32" ? "requires Linux /proc and real flock" : false };

function run(script, stateRoot) {
  return spawnSync("bash", ["-c", script, "fixture", common], {
    cwd: root,
    env: { ...process.env, MYSERVER_OPS_STATE_ROOT: stateRoot },
    encoding: "utf8"
  });
}

async function installerFixture(t) {
  const testRoot = await mkdtemp(path.join(os.tmpdir(), "myserver-installer-lock-"));
  t.after(() => rm(testRoot, { recursive: true, force: true }));
  const source = path.join(testRoot, "source");
  const target = path.join(testRoot, "home/gameops/script");
  const runner = path.join(testRoot, "runner.sh");
  await mkdir(source, { recursive: true });
  for (const script of opsScripts) {
    await copyFile(path.join(root, "deploy/docker/scripts", script), path.join(source, script));
    await chmod(path.join(source, script), 0o755);
  }
  await writeFile(runner, "#!/usr/bin/env bash\nexit 0\n");
  await chmod(runner, 0o755);
  return { testRoot, source, target, runner, stateRoot: path.join(testRoot, "data/myserver/run") };
}

function installerArgs(f) {
  return [installer, "--source", f.source, "--runner-source", f.runner, "--target", f.target, "--test-root", f.testRoot];
}

test("real flock excludes a concurrent mutating operation", linuxOnly, async (t) => {
  const stateRoot = await mkdtemp(path.join(os.tmpdir(), "myserver-ops-lock-"));
  t.after(() => rm(stateRoot, { recursive: true, force: true }));
  const holder = spawn("bash", ["-c", 'source "$1"; acquire_mutating_lock; echo locked; read -r _', "fixture", common], {
    cwd: root,
    env: { ...process.env, MYSERVER_OPS_STATE_ROOT: stateRoot },
    stdio: ["pipe", "pipe", "pipe"]
  });
  t.after(() => holder.kill());
  await new Promise((resolve, reject) => {
    holder.stdout.once("data", resolve);
    holder.once("error", reject);
    holder.once("exit", (code) => reject(new Error(`lock holder exited early: ${code}`)));
  });

  const contender = run('source "$1"; acquire_mutating_lock', stateRoot);
  assert.notEqual(contender.status, 0);
  assert.match(contender.stderr, /another mutating operation is already running/);
  holder.stdin.end("release\n");
  await new Promise((resolve) => holder.once("exit", resolve));
});

test("real lock permits same-process reentry", linuxOnly, async (t) => {
  const stateRoot = await mkdtemp(path.join(os.tmpdir(), "myserver-ops-lock-"));
  t.after(() => rm(stateRoot, { recursive: true, force: true }));
  const result = run('source "$1"; acquire_mutating_lock; acquire_mutating_lock', stateRoot);
  assert.equal(result.status, 0, result.stderr);
});

test("real lock descriptor is inherited by the automatic rollback child", linuxOnly, async (t) => {
  const stateRoot = await mkdtemp(path.join(os.tmpdir(), "myserver-ops-lock-"));
  t.after(() => rm(stateRoot, { recursive: true, force: true }));
  const result = run(
    'source "$1"; acquire_mutating_lock; bash -c \'source "$1"; acquire_mutating_lock\' child "$1"',
    stateRoot
  );
  assert.equal(result.status, 0, result.stderr);
});

test("installer is excluded by a real concurrent operations lock", linuxOnly, async (t) => {
  const f = await installerFixture(t);
  await mkdir(f.stateRoot, { recursive: true });
  const holder = spawn("bash", ["-c", 'source "$1"; acquire_mutating_lock; echo locked; read -r _', "fixture", common], {
    cwd: root,
    env: { ...process.env, MYSERVER_OPS_STATE_ROOT: f.stateRoot },
    stdio: ["pipe", "pipe", "pipe"]
  });
  t.after(() => holder.kill());
  await new Promise((resolve, reject) => {
    holder.stdout.once("data", resolve);
    holder.once("error", reject);
    holder.once("exit", (code) => reject(new Error(`lock holder exited early: ${code}`)));
  });
  const contender = spawnSync("bash", installerArgs(f), { cwd: root, encoding: "utf8" });
  assert.notEqual(contender.status, 0);
  assert.match(contender.stderr, /another mutating operation is already running/);
  holder.stdin.end("release\n");
  await new Promise((resolve) => holder.once("exit", resolve));
});

test("installer accepts only a legitimately inherited FD9 lock", linuxOnly, async (t) => {
  const f = await installerFixture(t);
  await mkdir(f.stateRoot, { recursive: true });
  const args = installerArgs(f);
  const result = spawnSync("bash", [
    "-c", 'source "$1"; acquire_mutating_lock; shift; exec bash "$@"',
    "fixture", common, ...args
  ], { cwd: root, env: { ...process.env, MYSERVER_OPS_STATE_ROOT: f.stateRoot }, encoding: "utf8" });
  assert.equal(result.status, 0, result.stderr);
});
