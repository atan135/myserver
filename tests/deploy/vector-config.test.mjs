import assert from "node:assert/strict";
import { access, readFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import path from "node:path";
import test from "node:test";

const root = path.resolve(import.meta.dirname, "../..");
const vectorDir = path.join(root, "deploy/docker/vector");
const scriptDir = path.join(root, "scripts/docker");

test("Vector bundle has the frozen source, sink, and metadata contract", async () => {
  const config = await readFile(path.join(vectorDir, "vector.yaml"), "utf8");
  const unit = await readFile(path.join(vectorDir, "vector.service"), "utf8");
  const compose = await readFile(path.join(root, "deploy/docker/compose.production.yml"), "utf8");
  const services = [
    "game-server",
    "game-proxy",
    "auth-http",
    "admin-api",
    "chat-server",
    "match-service",
    "mail-service",
    "announce-service",
    "metrics-collector"
  ];

  assert.match(config, /type:\s*docker_logs/);
  assert.match(config, /type:\s*file/);
  assert.match(config, /codec:\s*json/);
  assert.match(config, /method:\s*newline_delimited/);
  assert.match(config, /data_dir:\s*\/var\/lib\/vector/);
  assert.match(config, /\/data\/myserver\/log\/\{\{ service \}\}\/\{\{ captured_at \| format_timestamp\(format: "%(?:Y-%m-%d|F)"\) \}\}\/\{\{ instance_id \}\}\.\{\{ container_id_prefix \}\}\.jsonl\.open/);
  assert.match(config, /timezone:\s*UTC/);
  assert.match(config, /idle_timeout_secs:\s*30/);
  assert.match(config, /when_full:\s*drop_newest/);
  assert.match(config, /auto_partial_merge:\s*true/);
  assert.match(config, /max_size:\s*1073741824/);
  assert.match(config, /com\.docker\.compose\.service/);
  assert.match(config, /com\.myserver\.service-instance-id/);
  assert.match(config, /com\.myserver\.release-id/);
  assert.match(config, /\.timestamp \?\? null/);
  assert.match(config, /strlen\(\.message\) > 1048576/);
  assert.match(config, /message_sha256/);
  assert.match(config, /parse_regex\(\.message/);
  for (const service of services) assert.match(config, new RegExp(`"${service}"`));
  for (const field of [
    "service",
    "container_id",
    "container_id_prefix",
    "instance_id",
    "release_id",
    "captured_at",
    "event_time",
    "level",
    "stream",
    "message",
    "parse_status"
  ]) assert.match(config, new RegExp(`\\.${field}\\b`));
  assert.doesNotMatch(config, /\/var\/lib\/docker\/containers|docker-json\.log|\/run\/secrets/);
  assert.doesNotMatch(config, /(?:PASSWORD|TOKEN|DSN|DATABASE_URL)/);
  for (const service of services) {
    const section = compose.match(new RegExp(`  ${service}:[\\s\\S]*?(?=\\n  [a-z0-9-]+:|\\nvolumes:)`))?.[0] ?? "";
    assert.match(section, /com\.myserver\.service-instance-id:/, `${service} must expose instance label`);
    assert.match(section, /com\.myserver\.release-id:/, `${service} must expose release label`);
  }

  assert.match(unit, /^User=vector$/m);
  assert.match(unit, /^SupplementaryGroups=docker$/m);
  assert.match(unit, /^ExecStart=\/usr\/bin\/vector --config \/etc\/vector\/vector\.yaml$/m);
  assert.match(unit, /^ProtectSystem=strict$/m);
  assert.match(unit, /^ReadWritePaths=\/var\/lib\/vector \/data\/myserver\/log$/m);
  await access(path.join(vectorDir, "vector.yaml"));
  await access(path.join(vectorDir, "vector.service"));
});

test("Vector installation and diagnostics scripts are present and shell-parseable when bash is available", async (t) => {
  const scripts = ["install-vector.sh", "verify-vector.sh", "vector-status.sh", "rotate-vector-files.sh"];
  for (const name of scripts) await access(path.join(scriptDir, name));
  if (process.platform === "win32") {
    t.skip("Windows Node cannot pass native drive paths to the available bash shim");
    return;
  }
  const bash = spawnSync("bash", ["--version"], { cwd: root, encoding: "utf8" });
  if (bash.error?.code === "ENOENT") {
    t.skip("bash is not installed on this Windows workspace");
    return;
  }
  assert.equal(bash.status, 0);
  for (const name of scripts) {
    const result = spawnSync("bash", ["-n", path.join(scriptDir, name)], { cwd: root, encoding: "utf8" });
    assert.equal(result.status, 0, `${name} failed bash -n: ${result.stderr}`);
  }
  await access(path.join(scriptDir, "prune-vector-files.mjs"));
});

test("release bundle copies the Vector assets", async () => {
  const bundler = await readFile(path.join(scriptDir, "create-release-bundle.sh"), "utf8");
  const rotation = await readFile(path.join(scriptDir, "rotate-vector-files.sh"), "utf8");
  const retention = await readFile(path.join(scriptDir, "prune-vector-files.mjs"), "utf8");
  assert.match(bundler, /deploy\/docker\/vector\/vector\.yaml/);
  assert.match(bundler, /deploy\/docker\/vector\/vector\.service/);
  for (const name of ["install-vector.sh", "verify-vector.sh", "vector-status.sh", "rotate-vector-files.sh", "prune-vector-files.mjs"]) {
    assert.match(bundler, new RegExp(`scripts/docker/${name.replace(".", "\\.")}`));
  }
  assert.match(rotation, /268435456/);
  assert.match(rotation, /systemctl stop vector\.service/);
  assert.match(rotation, /sync -f/);
  assert.match(rotation, /\.jsonl\.open/);
  assert.match(rotation, /\.jsonl/);
  assert.match(retention, /archive-manifest\.jsonl/);
  assert.match(retention, /vector-retention-v2/);
  assert.match(retention, /retention-actions\.jsonl/);
  assert.match(retention, /not_archived/);
});
