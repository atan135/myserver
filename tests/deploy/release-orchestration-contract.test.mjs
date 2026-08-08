import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const read = (relativePath) => readFile(path.join(root, relativePath), "utf8");

function extractGameServerInstanceId(config) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [
      path.join(root, "scripts/docker/release-readiness-probe.mjs"),
      "--extract-game-server-instance-id"
    ], { stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("close", (code) => resolve({ code, stdout, stderr }));
    child.stdin.end(JSON.stringify(config));
  });
}

function serviceBlock(compose, serviceName) {
  const match = compose.match(new RegExp(
    `(?:^|\\r?\\n)  ${serviceName}:\\r?\\n([\\s\\S]*?)(?=\\r?\\n  [a-z][a-z0-9-]*:|\\r?\\nvolumes:)`
  ));
  assert.ok(match, `${serviceName} service must exist`);
  return match[1];
}

function dependencies(block) {
  const depends = block.match(/(?:^|\r?\n)    depends_on:\r?\n([\s\S]*?)(?=\r?\n    [a-z_]+:|$)/);
  if (!depends) return [];
  return [...depends[1].matchAll(/^      ([a-z][a-z0-9-]*):$/gm)].map((match) => match[1]);
}

test("production compose has no application-layer startup dependencies", async () => {
  const compose = await read("deploy/docker/compose.production.yml");
  const allowed = new Set(["postgres", "redis", "nats", "game-socket-init"]);
  const services = [
    "game-server", "match-service", "chat-server", "mail-service", "announce-service",
    "metrics-collector", "game-proxy", "auth-http", "admin-api", "caddy", "migration-runner"
  ];

  for (const service of services) {
    for (const dependency of dependencies(serviceBlock(compose, service))) {
      assert.ok(allowed.has(dependency), `${service} must not depend on application service ${dependency}`);
    }
  }
});

test("production mail service enables its required database adapter", async () => {
  const compose = await read("deploy/docker/compose.production.yml");
  assert.match(serviceBlock(compose, "mail-service"), /^      DB_ENABLED: "true"$/m);
});

test("release runner batch-starts applications and gates traffic on bounded convergence", async () => {
  const source = await read("scripts/docker/server-apply-release.sh");
  const logical = source.replace(/\\\r?\n\s*/g, " ");
  const batches = [...logical.matchAll(/^"\$\{compose\[@\]\}" up -d ([^\r\n]+)$/gm)]
    .map((match) => match[1])
    .filter((command) => command.includes("game-server"));
  assert.equal(batches.length, 1, "application services must use one compose up batch");
  for (const service of [
    "game-server", "match-service", "chat-server", "mail-service", "announce-service",
    "metrics-collector", "game-proxy", "auth-http", "admin-api"
  ]) {
    assert.match(batches[0], new RegExp(`(?:^|\\s)${service}(?:\\s|$)`));
  }
  assert.doesNotMatch(source, /stop game-server|rm -f .*registry|rm -f .*sock/);
  assert.match(source, /wait_for_release_readiness/);
  assert.match(source, /release_failure[\s\S]+READINESS_CONVERGENCE_TIMEOUT/);
  assert.match(source, /rollback_previous_release/);
  assert.ok(source.indexOf("wait_for_release_readiness") < source.lastIndexOf('up -d caddy'));
  assert.ok(source.lastIndexOf("postflight --environment production") < source.lastIndexOf('up -d caddy'));
  assert.ok(source.lastIndexOf('up -d caddy') < source.indexOf('ln -sfn "$release_dir"'));
});

test("all docker compose one-shot runners are disposable and dependency-free", async () => {
  const directory = path.join(root, "scripts/docker");
  const files = (await readdir(directory)).filter((file) => file.endsWith(".sh"));
  const commands = [];
  for (const file of files) {
    const source = (await readFile(path.join(directory, file), "utf8")).replace(/\\\r?\n\s*/g, " ");
    for (const line of source.split(/\r?\n/)) {
      if (/(?:release_compose_command|compose\[@\]|readiness_compose\[@\]).*\brun\b/.test(line)) commands.push(`${file}: ${line.trim()}`);
    }
  }
  assert.ok(commands.length >= 4);
  for (const command of commands) {
    assert.match(command, /\brun\s+--rm\s+--no-deps\b/);
  }
});

test("shared convergence covers registry TTL, stability and safe diagnostics", async () => {
  const [helper, probe, bundle, installer, common, restart, replace, retire, rollback, deploy, upload, render, environment, compose, releaseGuide, automationGuide, simplifiedGuide] = await Promise.all([
    read("scripts/docker/readiness-convergence.sh"),
    read("scripts/docker/release-readiness-probe.mjs"),
    read("scripts/docker/create-release-bundle.sh"),
    read("scripts/docker/install-ops-scripts.sh"),
    read("deploy/docker/scripts/ops-common.sh"),
    read("deploy/docker/scripts/ops-restart.sh"),
    read("deploy/docker/scripts/ops-replace.sh"),
    read("deploy/docker/scripts/ops-retire.sh"),
    read("deploy/docker/scripts/ops-rollback.sh"),
    read("deploy/docker/scripts/ops-deploy.sh"),
    read("scripts/docker/upload-release-bundle.sh"),
    read("scripts/docker/render-release-env.mjs"),
    read("deploy/docker/compose.production.env.example"),
    read("deploy/docker/compose.production.yml"),
    read("docs/后台与运维/Docker部署/正式Release上线说明.md"),
    read("docs/后台与运维/Docker部署/自动化发布脚本.md"),
    read("docs/后台与运维/Docker部署/简化版打包提交部署流程.md")
  ]);
  assert.match(helper, /RELEASE_REGISTRY_HEARTBEAT_TTL_SECONDS=30/);
  assert.match(helper, /--profile ops run --rm --no-deps --entrypoint node/);
  assert.match(helper, /MYSERVER_RELEASE_GAME_SERVER_INSTANCE_ID=.*game-server-1/);
  assert.match(helper, /RELEASE_READY_STABILITY_SECONDS_DEFAULT=10/);
  assert.match(helper, /validate_release_readiness_window\(\)[\s\S]+timeout_seconds <= required_stable_seconds[\s\S]+READINESS_WINDOW_INVALID/);
  assert.match(helper, /wait_for_release_readiness\(\)[\s\S]+validate_release_readiness_window/);
  assert.match(helper, /required_stable_seconds=\$\(\(registry_ttl_seconds \+ stability_seconds\)\)/);
  assert.match(helper, /timeout_seconds <= required_stable_seconds[\s\S]+READINESS_WINDOW_INVALID/);
  assert.match(helper, /readiness_timeout[\s\S]+release_readiness_diagnostics/);
  assert.match(helper, /RELEASE_READINESS_PROBE_FILE[\s\S]+--volume/);
  assert.match(probe, /"match-service"[\s\S]+7603\/readyz|MYSERVER_RELEASE_MATCH_SERVICE_READINESS_URL/);
  assert.match(probe, /dependencyState[\s\S]+errorCode[\s\S]+instanceId/);
  assert.doesNotMatch(probe, /\bendpoint\s*:/);
  assert.match(bundle, /readiness-convergence\.sh/);
  assert.match(bundle, /release-readiness-probe\.mjs/);
  assert.match(bundle, /install-ops-scripts\.sh/);
  assert.match(bundle, /server-apply-release\.sh/);
  for (const script of [
    "ops-common.sh", "ops-deploy.sh", "ops-disk-report.sh", "ops-health.sh", "ops-logs.sh",
    "ops-replace.sh", "ops-restart.sh", "ops-retire.sh", "ops-rollback.sh", "ops-status.sh"
  ]) {
    assert.match(bundle, new RegExp(script.replace(".", "\\.")), `bundle must contain ${script}`);
  }
  assert.match(common, /container_health[\s\S]+State\.Health/);
  assert.match(common, /wait_for_target_container[\s\S]+container_health/);
  assert.match(common, /readiness probe is unavailable/);
  assert.match(common, /acquire_mutating_lock[\s\S]+flock -n/);
  assert.match(common, /assert_no_pending_retire/);
  assert.match(common, /assert_no_pending_ops_install/);
  assert.match(installer, /source "\$source_dir\/ops-common\.sh"[\s\S]+acquire_mutating_lock/);
  assert.match(installer, /pending-ops-install[\s\S]+rollback_pending/);
  assert.match(installer, /allowed_target=\/home\/gameops\/script/);
  assert.match(retire, /--instance-id[\s\S]+--revision[\s\S]+--confirm/);
  assert.match(retire, /docker update --restart=no "\$container_id"/);
  assert.match(retire, /exit_code" == 0 && "\$oom_killed" == false/);
  assert.doesNotMatch(retire, /\bcurl\b|authorization:|--token|--nonce|access[_-]?token/i);
  for (const source of [restart, replace, rollback, deploy]) {
    assert.match(source, /acquire_mutating_lock[\s\S]+assert_no_pending_ops_install[\s\S]+assert_no_pending_retire/);
  }
  for (const [name, operation, source] of [
    ["restart", 'compose restart "$service"', restart],
    ["replace", 'compose up -d --no-deps "$service"', replace]
  ]) {
    const loadIndex = source.indexOf("load_release_readiness");
    const validationIndex = source.indexOf('validate_release_readiness_window "$timeout"');
    const operationIndex = source.indexOf(operation);
    const targetIndex = source.indexOf("wait_for_target_container");
    const convergenceIndex = source.indexOf("wait_for_release_readiness");
    assert.ok(loadIndex >= 0 && loadIndex < operationIndex, `${name} must reject an old bundle before changing the target`);
    assert.ok(loadIndex < validationIndex && validationIndex < operationIndex, `${name} must reject an invalid readiness window before changing the target`);
    assert.ok(operationIndex < targetIndex, `${name} must change the target before its container gate`);
    assert.ok(targetIndex < convergenceIndex, `${name} must gate the target before global convergence`);
  }
  assert.match(deploy, /rollback_db_compatible=false[\s\S]+--rollback-db-compatible[\s\S]+exec \/data\/myserver\/apply-release\.sh[\s\S]+--rollback-db-compatible/);
  assert.match(rollback, /exec \/data\/myserver\/apply-release\.sh[\s\S]+--rollback-db-compatible --rollback-attempt[\s\S]+--readiness-source-release/);
  assert.match(upload, /server_command=.*--rollback-db-compatible/);
  assert.match(upload, /"\$target\/scripts\/install-ops-scripts\.sh"\s+\\\s+--source "\$target\/scripts\/ops"\s+\\\s+--runner-source "\$target\/scripts\/server-apply-release\.sh"\s+\\\s+--target \/home\/gameops\/script/);
  assert.doesNotMatch(upload, /install[^\n]*ops-\*|cp[^\n]*ops-\*/);
  assert.doesNotMatch(upload, /sudo -n install -m 0755 "\$runner_source"/);
  assert.match(upload, /Existing release identity does not match[\s\S]+resuming_verified_release/);
  assert.ok(upload.indexOf("resuming_verified_release") < upload.indexOf('"$target/scripts/install-ops-scripts.sh"'));
  const releaseRunner = await read("scripts/docker/server-apply-release.sh");
  assert.match(releaseRunner, /verify_release_bundle\(\)[\s\S]+sha256sum --check --status SHA256SUMS/);
  const sourceVerificationIndex = releaseRunner.indexOf('verify_release_bundle "$readiness_source_dir" "$readiness_source_release_id"');
  const sourceHelperIndex = releaseRunner.indexOf('source "$readiness_source_dir/scripts/readiness-convergence.sh"');
  const sourceProbeIndex = releaseRunner.indexOf('export RELEASE_READINESS_PROBE_FILE="$readiness_source_dir/scripts/release-readiness-probe.mjs"');
  assert.ok(sourceVerificationIndex >= 0 && sourceVerificationIndex < sourceProbeIndex);
  assert.ok(sourceVerificationIndex < sourceHelperIndex, "source bundle checksum must pass before helper/probe use");
  assert.match(releaseRunner, /--readiness-source-release "\$release_id"/);
  assert.match(releaseRunner, /operations\.lock/);
  assert.match(releaseRunner, /pending-ops-install/);
  assert.match(releaseRunner, /pending-game-server-retire/);
  assert.match(releaseRunner, /rollback_attempt" == true && -z "\$readiness_source_release_id/);
  assert.match(releaseRunner, /if \[\[ "\$rollback_attempt" == false \]\][\s\S]+up -d postgres redis nats[\s\S]+else[\s\S]+database_migration_state=preserved/);
  assert.match(releaseRunner, /if \[\[ "\$rollback_attempt" == false \]\][\s\S]+migration-runner preflight[\s\S]+migration-runner apply/);
  assert.match(releaseRunner, /readiness_compose\[@\][\s\S]+postflight --environment production/);
  assert.match(releaseRunner, /SERVICE_INSTANCE_ID[\s\S]+export MYSERVER_RELEASE_GAME_SERVER_INSTANCE_ID/);
  assert.match(render, /\["RELEASE_ROOT", envValue\("release-root"\)\]/);
  assert.match(environment, /^RELEASE_ROOT=\/data\/myserver\/release\//m);
  assert.match(compose, /\$\{RELEASE_ROOT:\?set RELEASE_ROOT[^}]*\}\/scripts\/release-readiness-probe\.mjs/);
  assert.match(releaseGuide, /首次启动顺序[\s\S]+source \.\/scripts\/readiness-convergence\.sh[\s\S]+wait_for_release_readiness[\s\S]+postflight/);
  assert.match(automationGuide, /apply-release\.sh[\s\S]+--rollback-db-compatible/);
  assert.match(simplifiedGuide, /apply-release\.sh[\s\S]+--rollback-db-compatible/);
});

test("resolved Compose instance identity extraction is strict and redacted", async () => {
  const valid = await extractGameServerInstanceId({
    services: { "game-server": { environment: { SERVICE_INSTANCE_ID: "game-server-blue" } } }
  });
  assert.equal(valid.code, 0);
  assert.equal(valid.stdout.trim(), "game-server-blue");

  const secret = "must-not-be-echoed";
  const invalid = await extractGameServerInstanceId({
    services: { "game-server": { environment: { SERVICE_INSTANCE_ID: "invalid value", SECRET: secret } } }
  });
  assert.equal(invalid.code, 65);
  assert.doesNotMatch(`${invalid.stdout}${invalid.stderr}`, new RegExp(secret));
});
