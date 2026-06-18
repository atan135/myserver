import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

import { scanDiscoveryConfig } from "../tools/check-discovery-config.js";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(__dirname, "..");

function createTempRepo() {
  const tempDir = fs.mkdtempSync(path.join(os.tmpdir(), "myserver-discovery-config-"));
  return tempDir;
}

function writeFile(rootDir, relativePath, content) {
  const filePath = path.join(rootDir, relativePath);
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, content, "utf8");
}

function violationVariables(result) {
  return result.violations.map((violation) => `${violation.file}:${violation.variable}`).sort();
}

function hasActiveConfigAssignment(content, name) {
  const escapedName = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const patterns = [
    new RegExp(`^(?:export\\s+)?${escapedName}\\s*=`),
    new RegExp(`^\\$env:${escapedName}\\s*=`, "i"),
    new RegExp(`^-?\\s*${escapedName}\\s*:`),
    new RegExp(`^"${escapedName}"\\s*:`)
  ];

  return content
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n")
    .split("\n")
    .some((line) => {
      const trimmed = line.trim();
      return (
        trimmed &&
        !trimmed.startsWith("#") &&
        !trimmed.startsWith("//") &&
        patterns.some((pattern) => pattern.test(trimmed))
      );
    });
}

test("repository discovery config scan passes current strict overlays and local fallback examples", () => {
  const result = scanDiscoveryConfig({ rootDir: projectRoot });

  assert.equal(result.ok, true);
  assert.equal(result.violations.length, 0);
  assert.ok(result.strictFiles.includes("apps/auth-http/.env.production.example"));
  assert.ok(result.strictFiles.includes("apps/game-proxy/.env.test.example"));
  assert.ok(result.localExampleFiles.includes("apps/auth-http/.env.example"));
  assert.ok(result.localExampleFiles.includes("apps/game-server/.env.example"));
  assert.ok(
    result.allowedLocalFallbacks.some(
      (item) => item.file === "apps/auth-http/.env.example" && item.variable === "GAME_PROXY_HOST"
    )
  );
  assert.ok(
    result.allowedLocalFallbacks.some(
      (item) => item.file === "apps/game-server/.env.example" && item.variable === "MATCH_SERVICE_ADDR"
    )
  );
});

test("repository game-server strict templates omit MATCH_SERVICE_ADDR while local fallback remains allowed", () => {
  const result = scanDiscoveryConfig({ rootDir: projectRoot });
  const gameServerStrictFiles = result.strictFiles.filter((file) =>
    file.startsWith("apps/game-server/")
  );

  assert.equal(result.ok, true);
  assert.ok(gameServerStrictFiles.includes("apps/game-server/.env.test.example"));
  assert.ok(gameServerStrictFiles.includes("apps/game-server/.env.production.example"));

  for (const file of gameServerStrictFiles) {
    const content = fs.readFileSync(path.join(projectRoot, file), "utf8");
    assert.equal(
      hasActiveConfigAssignment(content, "MATCH_SERVICE_ADDR"),
      false,
      `${file} must not define MATCH_SERVICE_ADDR`
    );
  }

  assert.ok(
    result.allowedLocalFallbacks.some(
      (item) =>
        item.file === "apps/game-server/.env.example" &&
        item.variable === "MATCH_SERVICE_ADDR" &&
        item.service === "game-server"
    )
  );
});

test("game-server MATCH_SERVICE_ADDR is forbidden in test production and staging templates", () => {
  const tempDir = createTempRepo();
  try {
    writeFile(
      tempDir,
      "apps/game-server/.env.example",
      [
        "APP_ENV=development",
        "REGISTRY_ENABLED=true",
        "DISCOVERY_REQUIRED=true",
        "# Local fallback only: used only when registry discovery is disabled.",
        "# Do not use for strict/test/production/staging discovery.",
        "MATCH_SERVICE_ADDR=http://127.0.0.1:9002"
      ].join("\n")
    );

    for (const [file, appEnv] of [
      ["apps/game-server/.env.test.example", "test"],
      ["apps/game-server/.env.production.example", "production"],
      ["apps/game-server/.env.staging.example", "staging"]
    ]) {
      writeFile(
        tempDir,
        file,
        [
          `APP_ENV=${appEnv}`,
          "REGISTRY_ENABLED=true",
          "DISCOVERY_REQUIRED=true",
          "DISALLOW_LEGACY_DIRECT_CONFIG=true",
          "MATCH_SERVICE_ADDR=http://10.0.0.22:9002"
        ].join("\n")
      );
    }

    const result = scanDiscoveryConfig({ rootDir: tempDir });

    assert.equal(result.ok, false);
    assert.deepEqual(violationVariables(result), [
      "apps/game-server/.env.production.example:MATCH_SERVICE_ADDR",
      "apps/game-server/.env.staging.example:MATCH_SERVICE_ADDR",
      "apps/game-server/.env.test.example:MATCH_SERVICE_ADDR"
    ]);
    assert.ok(
      result.allowedLocalFallbacks.some(
        (item) =>
          item.file === "apps/game-server/.env.example" &&
          item.variable === "MATCH_SERVICE_ADDR" &&
          item.service === "game-server"
      )
    );
    for (const violation of result.violations) {
      assert.equal(violation.service, "game-server");
      assert.equal(violation.rule, "strict_legacy_direct_config_forbidden");
      assert.match(violation.reason, /match-service\.grpc/);
      assert.match(violation.remediation, /Local fallback examples/);
      assert.ok(violation.strictContext.includes("strict path/name"));
    }
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("strict config scan rejects consumers using legacy direct target variables", () => {
  const tempDir = createTempRepo();
  try {
    writeFile(
      tempDir,
      "apps/admin-api/.env.production.example",
      [
        "NODE_ENV=production",
        "REGISTRY_ENABLED=true",
        "DISCOVERY_REQUIRED=true",
        "GAME_SERVER_ADMIN_HOST=10.0.0.20",
        "GAME_PROXY_ADMIN_PORT=17101"
      ].join("\n")
    );
    writeFile(
      tempDir,
      "apps/mail-service/.env.staging",
      [
        "NODE_ENV=staging",
        "REGISTRY_ENABLED=true",
        "DISCOVERY_REQUIRED=true",
        "GAME_SERVER_ADMIN_HOST=10.0.0.21"
      ].join("\n")
    );
    writeFile(
      tempDir,
      "apps/match-service/.env.test.example",
      [
        "APP_ENV=test",
        "REGISTRY_ENABLED=true",
        "DISCOVERY_REQUIRED=true",
        "GAME_SERVER_INTERNAL_SOCKET_NAME=myserver-game-server-internal.sock"
      ].join("\n")
    );
    writeFile(
      tempDir,
      "apps/game-proxy/.env.production.example",
      [
        "APP_ENV=production",
        "REGISTRY_ENABLED=true",
        "DISCOVERY_REQUIRED=true",
        "UPSTREAM_LOCAL_SOCKET_NAME=myserver-game-server.sock"
      ].join("\n")
    );

    const result = scanDiscoveryConfig({ rootDir: tempDir });

    assert.equal(result.ok, false);
    assert.deepEqual(violationVariables(result), [
      "apps/admin-api/.env.production.example:GAME_PROXY_ADMIN_PORT",
      "apps/admin-api/.env.production.example:GAME_SERVER_ADMIN_HOST",
      "apps/game-proxy/.env.production.example:UPSTREAM_LOCAL_SOCKET_NAME",
      "apps/mail-service/.env.staging:GAME_SERVER_ADMIN_HOST",
      "apps/match-service/.env.test.example:GAME_SERVER_INTERNAL_SOCKET_NAME"
    ]);
    for (const violation of result.violations) {
      assert.equal(violation.rule, "strict_legacy_direct_config_forbidden");
      assert.match(violation.reason, /registry|service registry|discover/i);
      assert.match(violation.remediation, /Remove this variable/i);
    }
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("discovery config CLI emits machine-readable JSON and fails on violations", () => {
  const tempDir = createTempRepo();
  try {
    writeFile(
      tempDir,
      "apps/auth-http/.env.production.example",
      [
        "NODE_ENV=production",
        "REGISTRY_ENABLED=true",
        "DISCOVERY_REQUIRED=true",
        "GAME_PROXY_HOST=10.0.0.30"
      ].join("\n")
    );

    const result = spawnSync(
      process.execPath,
      ["tools/check-discovery-config.js", "--root", tempDir, "--compact"],
      { cwd: projectRoot, encoding: "utf8" }
    );

    assert.equal(result.status, 1, `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
    assert.equal(result.stderr, "");
    const report = JSON.parse(result.stdout);

    assert.equal(report.ok, false);
    assert.equal(report.summary.violations, 1);
    assert.deepEqual(report.violations[0], {
      file: "apps/auth-http/.env.production.example",
      line: 4,
      variable: "GAME_PROXY_HOST",
      service: "auth-http",
      rule: "strict_legacy_direct_config_forbidden",
      severity: "error",
      reason: "auth-http must discover game-proxy.client from the service registry in strict environments",
      remediation:
        "Remove this variable from strict/test/production config and use Redis service registry endpoints. Local fallback examples belong only in development .env.example with local-only comments.",
      strictContext: ["strict path/name", "NODE_ENV=production", "DISCOVERY_REQUIRED=true"]
    });
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});
