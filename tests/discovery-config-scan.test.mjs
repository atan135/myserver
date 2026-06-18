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

function hasCommentedConfigAssignment(content, name) {
  const escapedName = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const pattern = new RegExp(`^#\\s*${escapedName}\\s*=`);

  return content
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "\n")
    .split("\n")
    .some((line) => pattern.test(line.trim()));
}

function commentedConfigAssignmentContext(content, name) {
  const escapedName = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const pattern = new RegExp(`^#\\s*${escapedName}\\s*=`);
  const lines = content.replace(/\r\n/g, "\n").replace(/\r/g, "\n").split("\n");
  const index = lines.findIndex((line) => pattern.test(line.trim()));
  assert.notEqual(index, -1, `${name} should be present as a commented example`);

  const start = Math.max(0, index - 12);
  const end = Math.min(lines.length, index + 4);
  return lines.slice(start, end).join("\n");
}

function assertCommentedLocalDebugFallback(content, name, file) {
  assert.equal(hasActiveConfigAssignment(content, name), false, `${file} must not enable ${name}`);
  assert.equal(
    hasCommentedConfigAssignment(content, name),
    true,
    `${file} should keep ${name} as a commented local fallback example`
  );

  const context = commentedConfigAssignmentContext(content, name);
  assert.match(context, /Local debug fallback/, `${file} ${name} should be in a local debug fallback section`);
  assert.match(context, /Local fallback only/, `${file} ${name} should be marked local fallback only`);
  assert.match(context, /REGISTRY_ENABLED=false/, `${file} ${name} should require registry discovery disabled`);
  assert.match(context, /DISCOVERY_REQUIRED=false/, `${file} ${name} should require non-strict discovery`);
  assert.match(context, /NODE_ENV=development/, `${file} ${name} should mention NODE_ENV=development`);
  assert.match(context, /APP_ENV=local/, `${file} ${name} should mention APP_ENV=local`);
  assert.match(context, /strict\/test\/production/i, `${file} ${name} should mention strict/test/production`);
  assert.match(context, /registry/i, `${file} ${name} should direct strict routing to registry discovery`);
}

test("repository discovery config scan passes current strict overlays and commented local fallback examples", () => {
  const result = scanDiscoveryConfig({ rootDir: projectRoot });

  assert.equal(result.ok, true);
  assert.equal(result.violations.length, 0);
  assert.ok(result.strictFiles.includes("apps/auth-http/.env.production.example"));
  assert.ok(result.strictFiles.includes("apps/game-proxy/.env.test.example"));
  assert.ok(result.localExampleFiles.includes("apps/auth-http/.env.example"));
  assert.ok(result.localExampleFiles.includes("apps/game-server/.env.example"));
  assert.deepEqual(result.allowedLocalFallbacks, []);
});

test("repository game-server strict templates omit MATCH_SERVICE_ADDR while commented local fallback remains non-active", () => {
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

  const gameServerExample = fs.readFileSync(path.join(projectRoot, "apps/game-server/.env.example"), "utf8");
  assertCommentedLocalDebugFallback(
    gameServerExample,
    "MATCH_SERVICE_ADDR",
    "apps/game-server/.env.example"
  );
  assert.equal(
    result.allowedLocalFallbacks.some(
      (item) => item.file === "apps/game-server/.env.example" && item.variable === "MATCH_SERVICE_ADDR"
    ),
    false
  );
});

test("repository auth-http strict templates omit GAME_PROXY direct config while commented local fallback remains non-active", () => {
  const result = scanDiscoveryConfig({ rootDir: projectRoot });
  const authHttpStrictFiles = result.strictFiles.filter((file) => file.startsWith("apps/auth-http/"));

  assert.equal(result.ok, true);
  assert.ok(authHttpStrictFiles.includes("apps/auth-http/.env.test.example"));
  assert.ok(authHttpStrictFiles.includes("apps/auth-http/.env.production.example"));

  for (const file of authHttpStrictFiles) {
    const content = fs.readFileSync(path.join(projectRoot, file), "utf8");
    for (const variable of ["GAME_PROXY_HOST", "GAME_PROXY_PORT"]) {
      assert.equal(
        hasActiveConfigAssignment(content, variable),
        false,
        `${file} must not define ${variable}`
      );
    }
  }

  const authHttpExample = fs.readFileSync(path.join(projectRoot, "apps/auth-http/.env.example"), "utf8");
  for (const variable of ["GAME_PROXY_HOST", "GAME_PROXY_PORT"]) {
    assertCommentedLocalDebugFallback(authHttpExample, variable, "apps/auth-http/.env.example");
    assert.equal(
      result.allowedLocalFallbacks.some(
        (item) => item.file === "apps/auth-http/.env.example" && item.variable === variable
      ),
      false
    );
  }
});

test("repository control-plane strict templates omit GAME_SERVER_ADMIN direct config while commented local fallback remains non-active", () => {
  const result = scanDiscoveryConfig({ rootDir: projectRoot });
  const services = ["auth-http", "admin-api", "mail-service"];
  const variables = ["GAME_SERVER_ADMIN_HOST", "GAME_SERVER_ADMIN_PORT"];

  assert.equal(result.ok, true);

  for (const service of services) {
    const strictFiles = result.strictFiles.filter((file) => file.startsWith(`apps/${service}/`));
    assert.ok(strictFiles.includes(`apps/${service}/.env.test.example`));
    assert.ok(strictFiles.includes(`apps/${service}/.env.production.example`));

    for (const file of strictFiles) {
      const content = fs.readFileSync(path.join(projectRoot, file), "utf8");
      for (const variable of variables) {
        assert.equal(
          hasActiveConfigAssignment(content, variable),
          false,
          `${file} must not define ${variable}`
        );
      }
    }
  }

  for (const service of services) {
    const example = fs.readFileSync(path.join(projectRoot, `apps/${service}/.env.example`), "utf8");
    for (const variable of variables) {
      assertCommentedLocalDebugFallback(example, variable, `apps/${service}/.env.example`);
      assert.equal(
        result.allowedLocalFallbacks.some(
          (item) => item.file === `apps/${service}/.env.example` && item.variable === variable
        ),
        false
      );
    }
  }
});

test("repository admin-api strict templates omit GAME_PROXY_ADMIN direct config while commented local fallback remains allowed", () => {
  const result = scanDiscoveryConfig({ rootDir: projectRoot });
  const adminApiStrictFiles = result.strictFiles.filter((file) =>
    file.startsWith("apps/admin-api/")
  );
  const variables = ["GAME_PROXY_ADMIN_HOST", "GAME_PROXY_ADMIN_PORT"];

  assert.equal(result.ok, true);
  assert.ok(adminApiStrictFiles.includes("apps/admin-api/.env.test.example"));
  assert.ok(adminApiStrictFiles.includes("apps/admin-api/.env.production.example"));

  for (const file of adminApiStrictFiles) {
    const content = fs.readFileSync(path.join(projectRoot, file), "utf8");
    for (const variable of variables) {
      assert.equal(
        hasActiveConfigAssignment(content, variable),
        false,
        `${file} must not define ${variable}`
      );
    }
  }

  const adminApiExample = fs.readFileSync(path.join(projectRoot, "apps/admin-api/.env.example"), "utf8");
  for (const variable of variables) {
    assertCommentedLocalDebugFallback(adminApiExample, variable, "apps/admin-api/.env.example");
    assert.equal(
      result.allowedLocalFallbacks.some(
        (item) =>
          item.file === "apps/admin-api/.env.example" &&
          item.variable === variable &&
          item.service === "admin-api"
      ),
      false,
      `${variable} is only a commented local fallback example in apps/admin-api/.env.example`
    );
  }
});

test("repository game-proxy strict templates omit UPSTREAM direct config while commented local fallback remains non-active", () => {
  const result = scanDiscoveryConfig({ rootDir: projectRoot });
  const gameProxyStrictFiles = result.strictFiles.filter((file) =>
    file.startsWith("apps/game-proxy/")
  );
  const variables = ["UPSTREAM_SERVER_ID", "UPSTREAM_LOCAL_SOCKET_NAME"];

  assert.equal(result.ok, true);
  assert.ok(gameProxyStrictFiles.includes("apps/game-proxy/.env.test.example"));
  assert.ok(gameProxyStrictFiles.includes("apps/game-proxy/.env.production.example"));

  for (const file of gameProxyStrictFiles) {
    const content = fs.readFileSync(path.join(projectRoot, file), "utf8");
    for (const variable of variables) {
      assert.equal(
        hasActiveConfigAssignment(content, variable),
        false,
        `${file} must not define ${variable}`
      );
    }
  }

  const gameProxyExample = fs.readFileSync(path.join(projectRoot, "apps/game-proxy/.env.example"), "utf8");
  for (const variable of variables) {
    assertCommentedLocalDebugFallback(gameProxyExample, variable, "apps/game-proxy/.env.example");
    assert.equal(
      result.allowedLocalFallbacks.some(
        (item) =>
          item.file === "apps/game-proxy/.env.example" &&
          item.variable === variable &&
          item.service === "game-proxy"
      ),
      false,
      `${variable} is only a commented local fallback example in apps/game-proxy/.env.example`
    );
  }
});

test("repository match-service strict templates omit game-server internal socket fallback while comments remain non-active", () => {
  const result = scanDiscoveryConfig({ rootDir: projectRoot });
  const matchServiceStrictFiles = result.strictFiles.filter((file) =>
    file.startsWith("apps/match-service/")
  );
  const variables = ["GAME_SERVER_INTERNAL_SOCKET_NAME", "GAME_INTERNAL_SOCKET_NAME"];

  assert.equal(result.ok, true);
  assert.ok(matchServiceStrictFiles.includes("apps/match-service/.env.test.example"));
  assert.ok(matchServiceStrictFiles.includes("apps/match-service/.env.production.example"));

  for (const file of matchServiceStrictFiles) {
    const content = fs.readFileSync(path.join(projectRoot, file), "utf8");
    for (const variable of variables) {
      assert.equal(
        hasActiveConfigAssignment(content, variable),
        false,
        `${file} must not define ${variable}`
      );
    }
  }

  const matchServiceExample = fs.readFileSync(
    path.join(projectRoot, "apps/match-service/.env.example"),
    "utf8"
  );
  for (const variable of variables) {
    assertCommentedLocalDebugFallback(matchServiceExample, variable, "apps/match-service/.env.example");
    assert.equal(
      result.allowedLocalFallbacks.some(
        (item) =>
          item.file === "apps/match-service/.env.example" &&
          item.variable === variable &&
          item.service === "match-service"
      ),
      false,
      `${variable} is only a commented local fallback example in apps/match-service/.env.example`
    );
  }

  const gameServerExample = fs.readFileSync(path.join(projectRoot, "apps/game-server/.env.example"), "utf8");
  assert.equal(hasActiveConfigAssignment(gameServerExample, "GAME_INTERNAL_SOCKET_NAME"), true);
  assert.equal(
    result.allowedLocalFallbacks.some(
      (item) =>
        item.file === "apps/game-server/.env.example" &&
        item.variable === "GAME_INTERNAL_SOCKET_NAME"
    ),
    false,
    "apps/game-server/.env.example GAME_INTERNAL_SOCKET_NAME is the game-server self bind config"
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

test("match-service game-server internal socket fallback is forbidden in test production and staging templates", () => {
  const tempDir = createTempRepo();
  try {
    writeFile(
      tempDir,
      "apps/match-service/.env.example",
      [
        "APP_ENV=development",
        "REGISTRY_ENABLED=true",
        "DISCOVERY_REQUIRED=true",
        "GAME_SERVER_SERVICE_NAME=game-server",
        "# Local fallback only: used only when registry discovery is disabled and discovery is not strict.",
        "# Rejected in strict/test/production discovery; use service registry for game-server.internal endpoints there.",
        "# GAME_SERVER_INTERNAL_SOCKET_NAME=myserver-game-server-internal.sock",
        "# Legacy local fallback alias kept for compatibility with old local scripts only.",
        "# GAME_INTERNAL_SOCKET_NAME=myserver-game-server-internal.sock"
      ].join("\n")
    );

    for (const [file, appEnv] of [
      ["apps/match-service/.env.test.example", "test"],
      ["apps/match-service/.env.production.example", "production"],
      ["apps/match-service/.env.staging.example", "staging"]
    ]) {
      writeFile(
        tempDir,
        file,
        [
          `APP_ENV=${appEnv}`,
          "REGISTRY_ENABLED=true",
          "DISCOVERY_REQUIRED=true",
          "DISALLOW_LEGACY_DIRECT_CONFIG=true",
          "GAME_SERVER_INTERNAL_SOCKET_NAME=myserver-game-server-internal.sock",
          "GAME_INTERNAL_SOCKET_NAME=myserver-game-server-internal.sock"
        ].join("\n")
      );
    }

    const result = scanDiscoveryConfig({ rootDir: tempDir });

    assert.equal(result.ok, false);
    assert.deepEqual(violationVariables(result), [
      "apps/match-service/.env.production.example:GAME_INTERNAL_SOCKET_NAME",
      "apps/match-service/.env.production.example:GAME_SERVER_INTERNAL_SOCKET_NAME",
      "apps/match-service/.env.staging.example:GAME_INTERNAL_SOCKET_NAME",
      "apps/match-service/.env.staging.example:GAME_SERVER_INTERNAL_SOCKET_NAME",
      "apps/match-service/.env.test.example:GAME_INTERNAL_SOCKET_NAME",
      "apps/match-service/.env.test.example:GAME_SERVER_INTERNAL_SOCKET_NAME"
    ]);
    for (const violation of result.violations) {
      assert.equal(violation.service, "match-service");
      assert.equal(violation.rule, "strict_legacy_direct_config_forbidden");
      assert.match(violation.reason, /game-server\.internal|legacy internal socket alias/);
      assert.match(violation.remediation, /Local fallback examples/);
      assert.ok(violation.strictContext.includes("strict path/name"));
    }
    for (const variable of ["GAME_SERVER_INTERNAL_SOCKET_NAME", "GAME_INTERNAL_SOCKET_NAME"]) {
      assert.equal(
        result.allowedLocalFallbacks.some(
          (item) =>
            item.file === "apps/match-service/.env.example" &&
            item.variable === variable &&
            item.service === "match-service"
        ),
        false,
        `${variable} is only a commented local fallback example in apps/match-service/.env.example`
      );
    }
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("game-proxy UPSTREAM direct config is forbidden in test production and staging templates", () => {
  const tempDir = createTempRepo();
  try {
    writeFile(
      tempDir,
      "apps/game-proxy/.env.example",
      [
        "APP_ENV=development",
        "REGISTRY_ENABLED=true",
        "DISCOVERY_REQUIRED=true",
        "# Local fallback only: uncomment only for local development when registry discovery is disabled",
        "# and discovery is not strict. Strict/test/production/staging must use registry discovery.",
        "# UPSTREAM_SERVER_ID=game-server-1",
        "# UPSTREAM_LOCAL_SOCKET_NAME=myserver-game-server.sock"
      ].join("\n")
    );

    for (const [file, appEnv] of [
      ["apps/game-proxy/.env.test.example", "test"],
      ["apps/game-proxy/.env.production.example", "production"],
      ["apps/game-proxy/.env.staging.example", "staging"]
    ]) {
      writeFile(
        tempDir,
        file,
        [
          `APP_ENV=${appEnv}`,
          "REGISTRY_ENABLED=true",
          "DISCOVERY_REQUIRED=true",
          "DISALLOW_LEGACY_DIRECT_CONFIG=true",
          "UPSTREAM_SERVER_ID=game-server-1",
          "UPSTREAM_LOCAL_SOCKET_NAME=myserver-game-server.sock"
        ].join("\n")
      );
    }

    const result = scanDiscoveryConfig({ rootDir: tempDir });

    assert.equal(result.ok, false);
    assert.deepEqual(violationVariables(result), [
      "apps/game-proxy/.env.production.example:UPSTREAM_LOCAL_SOCKET_NAME",
      "apps/game-proxy/.env.production.example:UPSTREAM_SERVER_ID",
      "apps/game-proxy/.env.staging.example:UPSTREAM_LOCAL_SOCKET_NAME",
      "apps/game-proxy/.env.staging.example:UPSTREAM_SERVER_ID",
      "apps/game-proxy/.env.test.example:UPSTREAM_LOCAL_SOCKET_NAME",
      "apps/game-proxy/.env.test.example:UPSTREAM_SERVER_ID"
    ]);
    for (const violation of result.violations) {
      assert.equal(violation.service, "game-proxy");
      assert.equal(violation.rule, "strict_legacy_direct_config_forbidden");
      assert.match(violation.reason, /game-server\.proxy-local/);
      assert.match(violation.remediation, /Local fallback examples/);
      assert.ok(violation.strictContext.includes("strict path/name"));
    }
    for (const variable of ["UPSTREAM_SERVER_ID", "UPSTREAM_LOCAL_SOCKET_NAME"]) {
      assert.equal(
        result.allowedLocalFallbacks.some(
          (item) =>
            item.file === "apps/game-proxy/.env.example" &&
            item.variable === variable &&
            item.service === "game-proxy"
        ),
        false,
        `${variable} is only a commented local fallback example in apps/game-proxy/.env.example`
      );
    }
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("auth-http GAME_PROXY direct config is forbidden in test production and staging templates", () => {
  const tempDir = createTempRepo();
  try {
    writeFile(
      tempDir,
      "apps/auth-http/.env.example",
      [
        "NODE_ENV=development",
        "REGISTRY_ENABLED=true",
        "DISCOVERY_REQUIRED=true",
        "# Local fallback only: used only when registry discovery is disabled.",
        "# Do not use for strict/test/production/staging discovery.",
        "GAME_PROXY_HOST=127.0.0.1",
        "GAME_PROXY_PORT=4000"
      ].join("\n")
    );

    for (const [file, nodeEnv] of [
      ["apps/auth-http/.env.test.example", "test"],
      ["apps/auth-http/.env.production.example", "production"],
      ["apps/auth-http/.env.staging.example", "staging"]
    ]) {
      writeFile(
        tempDir,
        file,
        [
          `NODE_ENV=${nodeEnv}`,
          "REGISTRY_ENABLED=true",
          "DISCOVERY_REQUIRED=true",
          "DISALLOW_LEGACY_DIRECT_CONFIG=true",
          "GAME_PROXY_HOST=10.0.0.30",
          "GAME_PROXY_PORT=4000"
        ].join("\n")
      );
    }

    const result = scanDiscoveryConfig({ rootDir: tempDir });

    assert.equal(result.ok, false);
    assert.deepEqual(violationVariables(result), [
      "apps/auth-http/.env.production.example:GAME_PROXY_HOST",
      "apps/auth-http/.env.production.example:GAME_PROXY_PORT",
      "apps/auth-http/.env.staging.example:GAME_PROXY_HOST",
      "apps/auth-http/.env.staging.example:GAME_PROXY_PORT",
      "apps/auth-http/.env.test.example:GAME_PROXY_HOST",
      "apps/auth-http/.env.test.example:GAME_PROXY_PORT"
    ]);
    for (const variable of ["GAME_PROXY_HOST", "GAME_PROXY_PORT"]) {
      assert.ok(
        result.allowedLocalFallbacks.some(
          (item) =>
            item.file === "apps/auth-http/.env.example" &&
            item.variable === variable &&
            item.service === "auth-http"
        )
      );
    }
    for (const violation of result.violations) {
      assert.equal(violation.service, "auth-http");
      assert.equal(violation.rule, "strict_legacy_direct_config_forbidden");
      assert.match(violation.reason, /game-proxy\.client/);
      assert.match(violation.remediation, /Local fallback examples/);
      assert.ok(violation.strictContext.includes("strict path/name"));
    }
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("control-plane GAME_SERVER_ADMIN direct config is forbidden in test production and staging templates", () => {
  const tempDir = createTempRepo();
  try {
    for (const service of ["auth-http", "admin-api", "mail-service"]) {
      writeFile(
        tempDir,
        `apps/${service}/.env.example`,
        [
          "NODE_ENV=development",
          "REGISTRY_ENABLED=true",
          "DISCOVERY_REQUIRED=true",
          "# Local fallback only: used only when registry discovery is disabled.",
          "# Do not use for strict/test/production/staging discovery.",
          "GAME_SERVER_ADMIN_HOST=127.0.0.1",
          "GAME_SERVER_ADMIN_PORT=7500"
        ].join("\n")
      );

      for (const [fileSuffix, nodeEnv] of [
        [".env.test.example", "test"],
        [".env.production.example", "production"],
        [".env.staging.example", "staging"]
      ]) {
        writeFile(
          tempDir,
          `apps/${service}/${fileSuffix}`,
          [
            `NODE_ENV=${nodeEnv}`,
            "REGISTRY_ENABLED=true",
            "DISCOVERY_REQUIRED=true",
            "DISALLOW_LEGACY_DIRECT_CONFIG=true",
            "GAME_SERVER_ADMIN_HOST=10.0.0.20",
            "GAME_SERVER_ADMIN_PORT=17500"
          ].join("\n")
        );
      }
    }

    const result = scanDiscoveryConfig({ rootDir: tempDir });

    assert.equal(result.ok, false);
    assert.deepEqual(violationVariables(result), [
      "apps/admin-api/.env.production.example:GAME_SERVER_ADMIN_HOST",
      "apps/admin-api/.env.production.example:GAME_SERVER_ADMIN_PORT",
      "apps/admin-api/.env.staging.example:GAME_SERVER_ADMIN_HOST",
      "apps/admin-api/.env.staging.example:GAME_SERVER_ADMIN_PORT",
      "apps/admin-api/.env.test.example:GAME_SERVER_ADMIN_HOST",
      "apps/admin-api/.env.test.example:GAME_SERVER_ADMIN_PORT",
      "apps/auth-http/.env.production.example:GAME_SERVER_ADMIN_HOST",
      "apps/auth-http/.env.production.example:GAME_SERVER_ADMIN_PORT",
      "apps/auth-http/.env.staging.example:GAME_SERVER_ADMIN_HOST",
      "apps/auth-http/.env.staging.example:GAME_SERVER_ADMIN_PORT",
      "apps/auth-http/.env.test.example:GAME_SERVER_ADMIN_HOST",
      "apps/auth-http/.env.test.example:GAME_SERVER_ADMIN_PORT",
      "apps/mail-service/.env.production.example:GAME_SERVER_ADMIN_HOST",
      "apps/mail-service/.env.production.example:GAME_SERVER_ADMIN_PORT",
      "apps/mail-service/.env.staging.example:GAME_SERVER_ADMIN_HOST",
      "apps/mail-service/.env.staging.example:GAME_SERVER_ADMIN_PORT",
      "apps/mail-service/.env.test.example:GAME_SERVER_ADMIN_HOST",
      "apps/mail-service/.env.test.example:GAME_SERVER_ADMIN_PORT"
    ]);
    for (const violation of result.violations) {
      assert.equal(violation.rule, "strict_legacy_direct_config_forbidden");
      assert.match(violation.reason, /game-server\.admin/);
      assert.match(violation.remediation, /Local fallback examples/);
      assert.ok(violation.strictContext.includes("strict path/name"));
    }
    for (const service of ["auth-http", "admin-api", "mail-service"]) {
      for (const variable of ["GAME_SERVER_ADMIN_HOST", "GAME_SERVER_ADMIN_PORT"]) {
        assert.ok(
          result.allowedLocalFallbacks.some(
            (item) =>
              item.file === `apps/${service}/.env.example` &&
              item.variable === variable &&
              item.service === service
          )
        );
      }
    }
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("admin-api GAME_PROXY_ADMIN direct config is forbidden in test production and staging templates", () => {
  const tempDir = createTempRepo();
  try {
    writeFile(
      tempDir,
      "apps/admin-api/.env.example",
      [
        "NODE_ENV=development",
        "REGISTRY_ENABLED=true",
        "DISCOVERY_REQUIRED=true",
        "# Local fallback only: used only when registry discovery is disabled.",
        "# Do not use for strict/test/production/staging discovery.",
        "GAME_PROXY_ADMIN_HOST=127.0.0.1",
        "GAME_PROXY_ADMIN_PORT=7101"
      ].join("\n")
    );

    for (const [file, nodeEnv] of [
      ["apps/admin-api/.env.test.example", "test"],
      ["apps/admin-api/.env.production.example", "production"],
      ["apps/admin-api/.env.staging.example", "staging"]
    ]) {
      writeFile(
        tempDir,
        file,
        [
          `NODE_ENV=${nodeEnv}`,
          "REGISTRY_ENABLED=true",
          "DISCOVERY_REQUIRED=true",
          "DISALLOW_LEGACY_DIRECT_CONFIG=true",
          "GAME_PROXY_ADMIN_HOST=10.0.0.31",
          "GAME_PROXY_ADMIN_PORT=17101"
        ].join("\n")
      );
    }

    const result = scanDiscoveryConfig({ rootDir: tempDir });

    assert.equal(result.ok, false);
    assert.deepEqual(violationVariables(result), [
      "apps/admin-api/.env.production.example:GAME_PROXY_ADMIN_HOST",
      "apps/admin-api/.env.production.example:GAME_PROXY_ADMIN_PORT",
      "apps/admin-api/.env.staging.example:GAME_PROXY_ADMIN_HOST",
      "apps/admin-api/.env.staging.example:GAME_PROXY_ADMIN_PORT",
      "apps/admin-api/.env.test.example:GAME_PROXY_ADMIN_HOST",
      "apps/admin-api/.env.test.example:GAME_PROXY_ADMIN_PORT"
    ]);
    for (const variable of ["GAME_PROXY_ADMIN_HOST", "GAME_PROXY_ADMIN_PORT"]) {
      assert.ok(
        result.allowedLocalFallbacks.some(
          (item) =>
            item.file === "apps/admin-api/.env.example" &&
            item.variable === variable &&
            item.service === "admin-api"
        )
      );
    }
    for (const violation of result.violations) {
      assert.equal(violation.service, "admin-api");
      assert.equal(violation.rule, "strict_legacy_direct_config_forbidden");
      assert.match(violation.reason, /game-proxy\.admin/);
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

test("script scan rejects rollout fixed default control targets and unmarked direct endpoints", () => {
  const tempDir = createTempRepo();
  try {
    writeFile(
      tempDir,
      "tools/mock-client/src/rollout-transfer-cli.js",
      [
        "const options = {",
        "  oldAdminPort: 7500,",
        "  newAdminPort: 7501,",
        "  proxyAdminUrl: process.env.MYSERVER_PROXY_ADMIN_URL || \"http://127.0.0.1:7101\"",
        "};"
      ].join("\n")
    );
    writeFile(
      tempDir,
      "tools/mock-client/help_rollout.txt",
      [
        "node tools/mock-client/src/rollout-transfer-cli.js ^",
        "  --rollout-epoch rollout-test ^",
        "  --old-admin-host 127.0.0.1 --old-admin-port 7500 ^",
        "  --proxy-admin-url http://127.0.0.1:7101"
      ].join("\n")
    );

    const result = scanDiscoveryConfig({ rootDir: tempDir });

    assert.equal(result.ok, false);
    assert.equal(result.summary.scriptFixedTargetViolations, 4);
    assert.deepEqual(
      result.scriptFixedTargetViolations.map((violation) => violation.rule).sort(),
      [
        "script_direct_control_target_requires_marker",
        "script_fixed_control_target_default_forbidden",
        "script_fixed_control_target_default_forbidden",
        "script_fixed_control_target_default_forbidden"
      ]
    );
    for (const violation of result.scriptFixedTargetViolations) {
      assert.equal(violation.service, "rollout-script");
      assert.match(violation.remediation, /registry target semantics|--resolved-control-targets|--local-debug-targets/);
    }
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("script scan allows pre-resolved registry and local debug rollout direct endpoints", () => {
  const tempDir = createTempRepo();
  try {
    writeFile(
      tempDir,
      "scripts/rollout-three-process-drill.ps1",
      [
        "$transferArgs = @(",
        "  $TransferCli,",
        "  \"--resolved-control-targets\",",
        "  \"--old-admin-host\", $OldAdminHost,",
        "  \"--old-admin-port\", [string]$OldAdminPort,",
        "  \"--new-admin-host\", $NewAdminHost,",
        "  \"--new-admin-port\", [string]$NewAdminPort,",
        "  \"--proxy-admin-url\", $ProxyAdminUrl",
        ")"
      ].join("\n")
    );
    writeFile(
      tempDir,
      "tools/mock-client/help_rollout.txt",
      [
        "node tools/mock-client/src/rollout-transfer-cli.js ^",
        "  --local-debug-targets ^",
        "  --old-admin-host 127.0.0.1 --old-admin-port 7500 ^",
        "  --proxy-admin-url http://127.0.0.1:7101"
      ].join("\n")
    );

    const result = scanDiscoveryConfig({ rootDir: tempDir });

    assert.equal(result.ok, true);
    assert.deepEqual(result.scriptFixedTargetViolations, []);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("document scan rejects test production internal direct-connect guidance", () => {
  const tempDir = createTempRepo();
  try {
    writeFile(
      tempDir,
      "docs/direct-targets.md",
      [
        "测试环境可以临时直连 `game-server:7000`、`match-service:9002` 定位问题。",
        "生产可以直接访问 127.0.0.1:7500 调 game-server admin。",
        "线上可用 GAME_SERVER_ADMIN_HOST / GAME_SERVER_ADMIN_PORT 固定端口访问内部服务。"
      ].join("\n")
    );

    const result = scanDiscoveryConfig({ rootDir: tempDir });

    assert.equal(result.ok, false);
    assert.equal(result.summary.documentPolicyViolations, 3);
    assert.deepEqual(
      result.documentPolicyViolations.map((violation) => violation.rule),
      [
        "document_strict_internal_direct_target_forbidden",
        "document_strict_internal_direct_target_forbidden",
        "document_strict_internal_direct_target_forbidden"
      ]
    );
    for (const violation of result.documentPolicyViolations) {
      assert.equal(violation.service, "docs");
      assert.match(violation.remediation, /registry endpoint or instance-id/);
    }
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("document scan allows local-only examples and negative strict guidance", () => {
  const tempDir = createTempRepo();
  try {
    writeFile(
      tempDir,
      "README.md",
      [
        "固定 host/port 只能作为本地开发、manual fallback 或故障排查时的显式临时参数使用。",
        "测试、预发和线上不能依赖本地默认 host/port 或 127.0.0.1:7500 跑通链路。",
        "本地示例：mock-client 可通过 http://127.0.0.1:9003 调试 mail-service。",
        "测试/线上必须通过 registry endpoint 或 instance id 解析 game-server.admin。"
      ].join("\n")
    );
    writeFile(
      tempDir,
      "tools/mock-client/help.txt",
      [
        "# ========== 邮件系统测试 (内部联调地址；本地示例通过 --mail-base-url 9003) ==========",
        "node tools/mock-client/src/index.js --scenario mail-list --mail-base-url http://127.0.0.1:9003"
      ].join("\n")
    );

    const result = scanDiscoveryConfig({ rootDir: tempDir });

    assert.equal(result.ok, true);
    assert.deepEqual(result.documentPolicyViolations, []);
    assert.deepEqual(result.documentPolicyFiles, ["README.md", "tools/mock-client/help.txt"]);
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
