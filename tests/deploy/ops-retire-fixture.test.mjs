import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { chmod, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const retire = path.join(root, "deploy/docker/scripts/ops-retire.sh");
const replace = path.join(root, "deploy/docker/scripts/ops-replace.sh");
const containerId = "a".repeat(64);
const replacementId = "c".repeat(64);
const revision = "b".repeat(40);
const bashExecutable = process.platform === "win32"
  ? ["C:\\Program Files\\Git\\bin\\bash.exe", "C:\\Program Files\\Git\\usr\\bin\\bash.exe"].find(existsSync)
  : "bash";

function bashPath(value) {
  if (process.platform !== "win32") return value;
  return value.replace(/^([A-Za-z]):\\/, (_, drive) => `/${drive.toLowerCase()}/`).replaceAll("\\", "/");
}

async function fixture(t, options = {}) {
  const directory = await mkdtemp(path.join(os.tmpdir(), "myserver-ops-retire-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const bin = path.join(directory, "bin");
  const releaseRoot = path.join(directory, "release");
  const current = path.join(releaseRoot, "v-test");
  const stateRoot = path.join(directory, "run");
  const state = path.join(directory, "docker-state");
  const log = path.join(directory, "docker-operations");
  await mkdir(bin, { recursive: true });
  await mkdir(current, { recursive: true });
  await mkdir(stateRoot, { recursive: true });
  await writeFile(path.join(current, "compose.production.yml"), "name: myserver\nservices: {}\n");
  await writeFile(path.join(current, "compose.production.env"), "RELEASE_ID=v-test\n");
  await symlink(current, path.join(releaseRoot, "current"), "junction");
  await writeFile(state, `policy=${options.policy ?? "unless-stopped"}\nstatus=${options.status ?? "running"}\ncompose_id=${containerId}\n`);
  await writeFile(log, "");
  const docker = path.join(bin, "docker");
  await writeFile(docker, `#!/usr/bin/env bash
set -euo pipefail
state_file="\${FAKE_DOCKER_STATE:?}"
log_file="\${FAKE_DOCKER_LOG:?}"
value() { awk -F= -v key="$1" '$1 == key { print $2 }' "$state_file"; }
set_value() { awk -F= -v key="$1" -v value="$2" 'BEGIN { found=0 } $1 == key { print key "=" value; found=1; next } { print } END { if (!found) print key "=" value }' "$state_file" > "$state_file.tmp"; mv "$state_file.tmp" "$state_file"; }
if [[ "$1" == compose ]]; then
  shift
  lookup_project=''
  while [[ $# -gt 0 && "$1" != ps ]]; do
    if [[ "$1" == --project-name ]]; then lookup_project="$2"; shift 2; else shift; fi
  done
  [[ "$1" == ps ]] || exit 2
  [[ -z "\${FAKE_EXPECTED_LOOKUP_PROJECT:-}" || "$lookup_project" == "$FAKE_EXPECTED_LOOKUP_PROJECT" ]] || exit 87
  value compose_id
  exit 0
fi
case "$1" in
  inspect)
    if [[ "$2" != --format ]]; then [[ "$2" == '${containerId}' ]]; exit; fi
    format="$3"
    case "$format" in
      *com.docker.compose.service*) printf '%s\\n' "\${FAKE_COMPOSE_SERVICE:-game-server}" ;;
      *com.docker.compose.project*) printf '%s\\n' "\${FAKE_COMPOSE_PROJECT:-myserver}" ;;
      *org.opencontainers.image.revision*) printf '%s\\n' "\${FAKE_REVISION:-${revision}}" ;;
      *Config.Env*) printf 'SERVICE_INSTANCE_ID=%s\\n' "\${FAKE_INSTANCE_ID:-game-server-old}" ;;
      *HostConfig.RestartPolicy.Name*) value policy ;;
      *State.Status*) value status ;;
      *State.ExitCode*) printf '%s\\n' "\${FAKE_EXIT_CODE:-0}" ;;
      *State.OOMKilled*) printf '%s\\n' "\${FAKE_OOM_KILLED:-false}" ;;
      *) exit 2 ;;
    esac
    ;;
  update)
    printf 'update %s %s\\n' "$2" "$3" >> "$log_file"
    [[ "$3" == '${containerId}' ]] || exit 88
    policy="\${2#--restart=}"
    set_value policy "$policy"
    if [[ "\${FAKE_EXIT_ON_UPDATE:-}" == 1 && "$policy" == no ]]; then set_value status exited; fi
    if [[ "\${FAKE_DRIFT_AFTER_UPDATE:-}" == 1 && "$policy" == no ]]; then set_value compose_id '${replacementId}'; fi
    ;;
  start)
    printf 'start %s\\n' "$2" >> "$log_file"
    [[ "$2" == '${containerId}' ]] || exit 88
    set_value status running
    ;;
  *) exit 2 ;;
esac
`);
  await chmod(docker, 0o755);
  for (const [name, source] of [
    ["flock", "#!/usr/bin/env bash\nexit 0\n"],
    ["install", "#!/usr/bin/env bash\nset -e\nwhile [[ $# -gt 0 ]]; do case \"$1\" in -d) shift ;; -m) shift 2 ;; *) mkdir -p \"$1\"; shift ;; esac; done\n"],
    ["sync", "#!/usr/bin/env bash\nexit 0\n"],
    ["stat", "#!/usr/bin/env bash\nprintf '600\\n'\n"]
  ]) {
    const executable = path.join(bin, name);
    await writeFile(executable, source);
    await chmod(executable, 0o755);
  }
  return {
    state,
    log,
    stateRoot,
    env: {
      ...process.env,
      MYSERVER_FIXTURE_BIN: bashPath(bin),
      FAKE_DOCKER_STATE: bashPath(state),
      FAKE_DOCKER_LOG: bashPath(log),
      MYSERVER_RELEASE_ROOT: bashPath(releaseRoot),
      MYSERVER_OPS_STATE_ROOT: bashPath(stateRoot)
    }
  };
}

function run(script, args, env) {
  assert.ok(bashExecutable, "Windows Git Bash is required for the retire fixture");
  return spawnSync(bashExecutable, [
    "-c", 'export PATH="$MYSERVER_FIXTURE_BIN:$PATH"; exec bash "$@"',
    "fixture", bashPath(script), ...args
  ], { cwd: root, env, encoding: "utf8" });
}

const retireArgs = (extra = []) => [
  "game-server", "--instance-id", "game-server-old", "--revision", revision,
  "--confirm", `game-server-old@${revision}@myserver`, ...extra
];

async function writeJournal(f) {
  await writeFile(path.join(f.stateRoot, "pending-game-server-retire"), [
    "schema=1", "service=game-server", "project=myserver", `container_id=${containerId}`,
    "instance_id=game-server-old", `revision=${revision}`,
    "original_policy=unless-stopped", "original_running=true", "phase=waiting", ""
  ].join("\n"), { mode: 0o600 });
}

test("retire keeps restart disabled only after an exact clean exit", async (t) => {
  const f = await fixture(t);
  const result = run(retire, retireArgs(["--timeout", "2"]), { ...f.env, FAKE_EXIT_ON_UPDATE: "1" });
  assert.equal(result.status, 0, result.stderr);
  assert.match(await readFile(f.state, "utf8"), /policy=no/);
  assert.match(await readFile(f.state, "utf8"), /status=exited/);
  assert.doesNotMatch(result.stdout + result.stderr, /Bearer|nonce|access[_-]?token/i);
});

test("retire timeout restores the original running desired state", async (t) => {
  const f = await fixture(t);
  const result = run(retire, retireArgs(["--timeout", "1"]), f.env);
  assert.notEqual(result.status, 0);
  assert.match(await readFile(f.state, "utf8"), /policy=unless-stopped/);
  assert.match(await readFile(f.state, "utf8"), /status=running/);
});

for (const [name, args, extraEnv, error] of [
  ["confirmation", ["game-server", "--instance-id", "game-server-old", "--revision", revision, "--confirm", "wrong"], {}, /confirmation must exactly match/],
  ["instance identity", retireArgs(), { FAKE_INSTANCE_ID: "game-server-other" }, /instance identity does not match/],
  ["image revision", retireArgs(), { FAKE_REVISION: "d".repeat(40) }, /image revision does not match/],
  ["Compose service label", retireArgs(), { FAKE_COMPOSE_SERVICE: "auth-http" }, /Compose service does not match/],
  ["Compose project label", retireArgs(), { FAKE_COMPOSE_PROJECT: "other" }, /Compose project does not match/]
]) {
  test(`wrong ${name} is rejected before restart policy changes`, async (t) => {
    const f = await fixture(t);
    const result = run(retire, args, { ...f.env, ...extraEnv });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, error);
    assert.equal(await readFile(f.log, "utf8"), "");
  });
}

for (const [name, extraEnv] of [
  ["nonzero exit", { FAKE_EXIT_CODE: "23" }],
  ["OOM exit", { FAKE_OOM_KILLED: "true" }]
]) {
  test(`${name} restores unless-stopped and the running state`, async (t) => {
    const f = await fixture(t);
    const result = run(retire, retireArgs(["--timeout", "2"]), {
      ...f.env, FAKE_EXIT_ON_UPDATE: "1", ...extraEnv
    });
    assert.notEqual(result.status, 0);
    assert.match(await readFile(f.state, "utf8"), /policy=unless-stopped/);
    assert.match(await readFile(f.state, "utf8"), /status=running/);
    assert.match(await readFile(f.log, "utf8"), /update --restart=unless-stopped/);
    assert.match(await readFile(f.log, "utf8"), /start a{64}/);
  });
}

test("identity drift never updates or starts the replacement and preserves recovery journal", async (t) => {
  const f = await fixture(t);
  const result = run(retire, retireArgs(["--timeout", "2"]), { ...f.env, FAKE_DRIFT_AFTER_UPDATE: "1" });
  assert.notEqual(result.status, 0);
  assert.equal(await readFile(f.log, "utf8"), `update --restart=no ${containerId}\n`);
  assert.equal(existsSync(path.join(f.stateRoot, "pending-game-server-retire")), true);
});

test("explicit isolated Compose project is part of lookup, label fence and confirmation", async (t) => {
  const f = await fixture(t);
  const project = "myserver-phase7-b54acdd-b";
  const args = [
    "game-server", "--instance-id", "game-server-old", "--revision", revision,
    "--project", project, "--confirm", `game-server-old@${revision}@${project}`, "--timeout", "2"
  ];
  const result = run(retire, args, {
    ...f.env, FAKE_COMPOSE_PROJECT: project, FAKE_EXPECTED_LOOKUP_PROJECT: project, FAKE_EXIT_ON_UPDATE: "1"
  });
  assert.equal(result.status, 0, result.stderr);
});

test("pending retire blocks replace and explicit recovery is identity fenced", async (t) => {
  const f = await fixture(t);
  await writeJournal(f);
  const blocked = run(replace, ["game-server", "--confirm", "game-server"], f.env);
  assert.notEqual(blocked.status, 0);
  assert.match(blocked.stderr, /pending game-server retire recovery is required/);
  const recovered = run(retire, retireArgs(["--recover"]), f.env);
  assert.equal(recovered.status, 0, recovered.stderr);
  assert.match(await readFile(f.state, "utf8"), /policy=unless-stopped/);
});

test("pending ops install journal blocks mutating wrappers", async (t) => {
  const f = await fixture(t);
  await writeFile(path.join(f.stateRoot, "pending-ops-install"), "schema=1\n", { mode: 0o600 });
  const blocked = run(replace, ["game-server", "--confirm", "game-server"], f.env);
  assert.notEqual(blocked.status, 0);
  assert.match(blocked.stderr, /pending ops install recovery is required/);
  assert.equal(await readFile(f.log, "utf8"), "");
});

test("recovering a clean-exited old container restarts it and ends the retire cycle", async (t) => {
  const f = await fixture(t, { policy: "no", status: "exited" });
  await writeJournal(f);
  const recovered = run(retire, retireArgs(["--recover"]), f.env);
  assert.equal(recovered.status, 0, recovered.stderr);
  assert.match(await readFile(f.state, "utf8"), /policy=unless-stopped/);
  assert.match(await readFile(f.state, "utf8"), /status=running/);
  assert.equal(existsSync(path.join(f.stateRoot, "pending-game-server-retire")), false);
  assert.match(await readFile(f.log, "utf8"), /start a{64}/);

  const newCycle = run(retire, retireArgs(["--timeout", "1"]), f.env);
  assert.notEqual(newCycle.status, 0);
  assert.match(newCycle.stdout, /awaiting_control_plane_shutdown/);
});
