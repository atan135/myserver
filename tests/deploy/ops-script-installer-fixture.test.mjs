import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { chmod, copyFile, mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const installer = path.join(root, "scripts/docker/install-ops-scripts.sh");
const bashExecutable = process.platform === "win32"
  ? ["C:\\Program Files\\Git\\bin\\bash.exe", "C:\\Program Files\\Git\\usr\\bin\\bash.exe"].find(existsSync)
  : "bash";
const scripts = [
  "ops-common.sh", "ops-deploy.sh", "ops-disk-report.sh", "ops-health.sh", "ops-logs.sh",
  "ops-replace.sh", "ops-restart.sh", "ops-retire.sh", "ops-rollback.sh", "ops-status.sh"
];

function bashPath(value) {
  if (process.platform !== "win32") return value;
  return value.replace(/^([A-Za-z]):\\/, (_, drive) => `/${drive.toLowerCase()}/`).replaceAll("\\", "/");
}

async function fixture(t) {
  const directory = await mkdtemp(path.join(os.tmpdir(), "myserver-ops-installer-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const source = path.join(directory, "source");
  const bin = path.join(directory, "bin");
  const target = path.join(directory, "home", "gameops", "script");
  const runnerSource = path.join(directory, "server-apply-release.sh");
  const runnerTarget = path.join(directory, "data", "myserver", "apply-release.sh");
  await mkdir(source, { recursive: true });
  await mkdir(bin, { recursive: true });
  await mkdir(target, { recursive: true });
  await mkdir(path.dirname(runnerTarget), { recursive: true });
  for (const script of scripts) {
    await copyFile(path.join(root, "deploy/docker/scripts", script), path.join(source, script));
    await chmod(path.join(source, script), 0o755);
  }
  await writeFile(path.join(target, "old-only.sh"), "old ops generation\n");
  await writeFile(runnerSource, "#!/usr/bin/env bash\necho new runner\n");
  await writeFile(runnerTarget, "#!/usr/bin/env bash\necho old runner\n");
  await chmod(runnerSource, 0o755);
  await chmod(runnerTarget, 0o755);
  const commands = new Map([
    ["flock", "#!/usr/bin/env bash\nexit 0\n"],
    ["chmod", "#!/usr/bin/env bash\nexit 0\n"],
    ["sync", "#!/usr/bin/env bash\nexit 0\n"],
    ["stat", "#!/usr/bin/env bash\nprintf '600\\n'\n"],
    ["install", `#!/usr/bin/env bash
set -euo pipefail
directory=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    -d) directory=true; shift ;;
    -m) shift 2 ;;
    *) break ;;
  esac
done
if [[ "$directory" == true ]]; then
  mkdir -p "$@"
else
  source_file="$1"; destination="$2"
  cp "$source_file" "$destination"
fi
`]
  ]);
  for (const [name, body] of commands) {
    const executable = path.join(bin, name);
    await writeFile(executable, body);
    await chmod(executable, 0o755);
  }
  return { directory, source, target, runnerSource, runnerTarget, bin };
}

function run(f, extra = [], target = f.target) {
  assert.ok(bashExecutable, "Git Bash is required for the installer fixture on Windows");
  return spawnSync(bashExecutable, [
    "-c", 'export PATH="$MYSERVER_FIXTURE_BIN:$PATH"; exec bash "$@"', "fixture",
    bashPath(installer), "--source", bashPath(f.source),
    "--runner-source", bashPath(f.runnerSource), "--target", bashPath(target),
    "--test-root", bashPath(f.directory), ...extra
  ], { cwd: root, encoding: "utf8", env: { ...process.env, MYSERVER_FIXTURE_BIN: bashPath(f.bin) } });
}

test("installer replaces the complete ops and runner generation", async (t) => {
  const f = await fixture(t);
  const result = run(f);
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual((await readdir(f.target)).sort(), [...scripts].sort());
  assert.match(await readFile(f.target + "/ops-retire.sh", "utf8"), /Coordinates Docker desired state/);
  assert.match(await readFile(f.runnerTarget, "utf8"), /new runner/);
});

for (const [name, mutate] of [
  ["missing", (f) => rm(path.join(f.source, "ops-retire.sh"))],
  ["extra file", (f) => writeFile(path.join(f.source, "unexpected.sh"), "unexpected\n")],
  ["extra directory", (f) => mkdir(path.join(f.source, "unexpected"))]
]) {
  test(`installer rejects ${name} before cutover`, async (t) => {
    const f = await fixture(t);
    await mutate(f);
    const result = run(f);
    assert.notEqual(result.status, 0);
    assert.deepEqual(await readdir(f.target), ["old-only.sh"]);
    assert.match(await readFile(f.runnerTarget, "utf8"), /old runner/);
  });
}

test("installer rejects any target outside the exact test-root path", async (t) => {
  const f = await fixture(t);
  for (const target of ["/", f.directory, path.join(f.directory, "home/gameops/other")]) {
    const result = run(f, [], target);
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /Ops target must exactly match/);
  }
});

test("installer rejects a symlinked exact target", async (t) => {
  const f = await fixture(t);
  const redirected = path.join(f.directory, "redirected");
  await mkdir(redirected);
  await rm(f.target, { recursive: true });
  await symlink(redirected, f.target, "junction");
  const result = run(f);
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /Ops target must exactly match/);
});

test("runner switch failure restores old ops and old runner", async (t) => {
  const f = await fixture(t);
  const result = run(f, ["--test-fail-after", "ops-switched"]);
  assert.notEqual(result.status, 0);
  assert.deepEqual(await readdir(f.target), ["old-only.sh"]);
  assert.match(await readFile(f.runnerTarget, "utf8"), /old runner/);
  assert.equal(existsSync(path.join(f.directory, "data/myserver/run/pending-ops-install")), false);
});

test("next install identity-fences and recovers a SIGKILL journal", async (t) => {
  const f = await fixture(t);
  const crashed = run(f, ["--test-crash-after", "ops-switched"]);
  assert.notEqual(crashed.status, 0);
  assert.equal(existsSync(path.join(f.directory, "data/myserver/run/pending-ops-install")), true);

  const recovered = run(f);
  assert.equal(recovered.status, 0, recovered.stderr);
  assert.match(recovered.stdout, /recovered_pending_ops_install=true/);
  assert.deepEqual((await readdir(f.target)).sort(), [...scripts].sort());
  assert.match(await readFile(f.runnerTarget, "utf8"), /new runner/);
});

test("SIGKILL recovery preserves a mismatched replacement generation", async (t) => {
  const f = await fixture(t);
  const crashed = run(f, ["--test-crash-after", "ops-switched"]);
  assert.notEqual(crashed.status, 0);
  const journal = path.join(f.directory, "data/myserver/run/pending-ops-install");
  await writeFile(path.join(f.target, "ops-retire.sh"), "replacement generation\n");

  const refused = run(f);
  assert.notEqual(refused.status, 0);
  assert.match(refused.stderr, /cannot be recovered safely/);
  assert.equal(await readFile(path.join(f.target, "ops-retire.sh"), "utf8"), "replacement generation\n");
  assert.equal(existsSync(journal), true);
});
