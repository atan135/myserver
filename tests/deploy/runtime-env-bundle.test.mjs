import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const renderer = path.join(root, "scripts/docker/render-release-env.mjs");
const read = (relativePath) => readFile(path.join(root, relativePath), "utf8");
const runtimeReference = "${MYSERVER_RUNTIME_ENV:?set MYSERVER_RUNTIME_ENV to production or test}";

function render(output, runtimeEnv) {
  const args = [
    renderer,
    "--lock", path.join(root, "deploy/docker/images.lock.json"),
    "--template", path.join(root, "deploy/docker/compose.production.env.example"),
    "--output", output,
    "--release-root", "/data/myserver/release/v-runtime-env-test",
    "--caddy-landing-host", "landing.test.example",
    "--caddy-auth-host", "auth.test.example",
    "--caddy-admin-host", "admin.test.example",
    "--caddy-chat-host", "chat.test.example",
    "--caddy-email", "ops@test.example",
    "--game-proxy-advertised-host", "game.test.example"
  ];
  if (runtimeEnv !== undefined) args.push("--runtime-env", runtimeEnv);
  return spawnSync(process.execPath, args, { encoding: "utf8" });
}

async function withTemporaryDirectory(callback) {
  const directory = await mkdtemp(path.join(os.tmpdir(), "myserver-runtime-env-"));
  try {
    return await callback(directory);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

function serviceBlock(compose, serviceName) {
  const match = compose.match(new RegExp(
    `(?:^|\\r?\\n)  ${serviceName}:\\r?\\n([\\s\\S]*?)(?=\\r?\\n  [a-z][a-z0-9-]*:|\\r?\\nvolumes:)`
  ));
  assert.ok(match, `${serviceName} service must exist`);
  return match[1];
}

test("release environment renderer defaults to production and accepts test", async () => {
  await withTemporaryDirectory(async (directory) => {
    const defaultOutput = path.join(directory, "default.env");
    const defaultResult = render(defaultOutput);
    assert.equal(defaultResult.status, 0, defaultResult.stderr);
    assert.match(await readFile(defaultOutput, "utf8"), /^MYSERVER_RUNTIME_ENV=production$/m);

    const testOutput = path.join(directory, "test.env");
    const testResult = render(testOutput, "test");
    assert.equal(testResult.status, 0, testResult.stderr);
    assert.match(await readFile(testOutput, "utf8"), /^MYSERVER_RUNTIME_ENV=test$/m);
  });
});

test("release environment renderer rejects unsupported runtime values", async () => {
  await withTemporaryDirectory(async (directory) => {
    const result = render(path.join(directory, "invalid.env"), "staging");
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /--runtime-env must be production or test/);
  });
});

test("production compose propagates the rendered runtime identity without lowering hardening", async () => {
  const compose = await read("deploy/docker/compose.production.yml");
  for (const [service, key] of [
    ["game-server", "APP_ENV"],
    ["match-service", "APP_ENV"],
    ["chat-server", "APP_ENV"],
    ["mail-service", "NODE_ENV"],
    ["announce-service", "NODE_ENV"],
    ["metrics-collector", "NODE_ENV"],
    ["game-proxy", "APP_ENV"],
    ["auth-http", "NODE_ENV"],
    ["admin-api", "NODE_ENV"],
    ["migration-runner", "NODE_ENV"]
  ]) {
    const expected = `      ${key}: ${runtimeReference}`;
    assert.ok(
      serviceBlock(compose, service).split(/\r?\n/).includes(expected),
      `${service} must obtain ${key} from MYSERVER_RUNTIME_ENV`
    );
  }

  for (const service of [
    "game-server", "match-service", "chat-server", "mail-service", "announce-service", "game-proxy", "auth-http", "admin-api"
  ]) {
    const block = serviceBlock(compose, service);
    assert.match(block, /^      REGISTRY_ENABLED: "true"$/m);
    assert.match(block, /^      DISCOVERY_REQUIRED: "true"$/m);
    assert.match(block, /^      DISALLOW_LEGACY_DIRECT_CONFIG: "true"$/m);
  }

  const auth = serviceBlock(compose, "auth-http");
  assert.match(auth, /^      AUTH_REQUIRE_TLS: "true"$/m);
  assert.match(auth, /^      AUTH_STRICT_SECURITY: "true"$/m);
});

test("bundle and upload scripts pass a validated runtime environment end to end", async () => {
  const [create, upload, rendererSource, template] = await Promise.all([
    read("scripts/docker/create-release-bundle.sh"),
    read("scripts/docker/upload-release-bundle.sh"),
    read("scripts/docker/render-release-env.mjs"),
    read("deploy/docker/compose.production.env.example")
  ]);

  assert.match(create, /runtime_env="\$\{MYSERVER_RUNTIME_ENV:-production\}"/);
  assert.match(create, /--runtime-env\)\r?\n      runtime_env=/);
  assert.match(create, /case "\$runtime_env" in\r?\n  production\|test\) ;;/);
  assert.match(create, /--runtime-env "\$runtime_env"/);
  assert.match(upload, /runtime_env="\$\{MYSERVER_RUNTIME_ENV:-production\}"/);
  assert.match(upload, /--runtime-env\) runtime_env=/);
  assert.match(upload, /case "\$runtime_env" in production\|test\) ;;/);
  assert.match(upload, /--runtime-env "\$runtime_env"/);
  assert.match(rendererSource, /const runtimeEnv = options\.has\("runtime-env"\) \? envValue\("runtime-env"\) : "production"/);
  assert.match(template, /^MYSERVER_RUNTIME_ENV=production$/m);
});
