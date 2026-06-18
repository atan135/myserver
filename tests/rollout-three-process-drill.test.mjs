import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

import { createServiceInstancePayload } from "../packages/service-registry/node/registry-schema.js";

const projectRoot = process.cwd();
const powershellCommand = process.env.POWERSHELL_BIN || (process.platform === "win32" ? "powershell" : "pwsh");
const powershellProbe = spawnSync(
  powershellCommand,
  ["-NoProfile", "-Command", "$PSVersionTable.PSVersion.ToString()"],
  { cwd: projectRoot, encoding: "utf8" }
);
const powershellAvailable = !powershellProbe.error && powershellProbe.status === 0;
const powershellSkip = powershellAvailable
  ? false
  : `PowerShell is not available: ${powershellProbe.error?.message || powershellProbe.stderr || "probe failed"}`;

function endpoint(name, protocol, host, port, visibility, metadata = {}) {
  return {
    name,
    protocol,
    host,
    port,
    socket: "",
    visibility,
    metadata,
    healthy: true
  };
}

function metadata(serviceName, instanceId) {
  return {
    service_name: serviceName,
    service_instance_id: instanceId,
    instance_id: instanceId,
    build_version: "rollout-drill-test",
    zone: "test"
  };
}

function servicePayload({ id, name, host, port, endpoints }) {
  return createServiceInstancePayload({
    id,
    name,
    host,
    port,
    endpoints,
    tags: ["rollout-drill-test"],
    weight: 100,
    metadata: metadata(name, id)
  });
}

function registryFixture({ includeAuthInternal = true } = {}) {
  const oldId = "game-server-old";
  const newId = "game-server-new";
  const proxyId = "game-proxy-rollout";
  const authId = "auth-http-rollout";
  const payloads = [
    servicePayload({
      id: oldId,
      name: "game-server",
      host: "127.0.0.10",
      port: 17000,
      endpoints: [
        endpoint("admin", "tcp", "127.0.0.10", 17500, "admin", metadata("game-server", oldId))
      ]
    }),
    servicePayload({
      id: newId,
      name: "game-server",
      host: "127.0.0.11",
      port: 17001,
      endpoints: [
        endpoint("admin", "tcp", "127.0.0.11", 17501, "admin", metadata("game-server", newId))
      ]
    }),
    servicePayload({
      id: proxyId,
      name: "game-proxy",
      host: "127.0.0.12",
      port: 14000,
      endpoints: [
        endpoint("admin", "http", "127.0.0.12", 17101, "admin", metadata("game-proxy", proxyId))
      ]
    })
  ];

  const authEndpoints = includeAuthInternal
    ? [endpoint("internal", "http", "127.0.0.13", 13080, "internal", metadata("auth-http", authId))]
    : [endpoint("http", "http", "127.0.0.13", 13000, "public", metadata("auth-http", authId))];
  payloads.push(servicePayload({
    id: authId,
    name: "auth-http",
    host: "127.0.0.13",
    port: 13000,
    endpoints: authEndpoints
  }));

  return {
    services: {
      "game-server": payloads.filter((payload) => payload.name === "game-server"),
      "game-proxy": payloads.filter((payload) => payload.name === "game-proxy"),
      "auth-http": payloads.filter((payload) => payload.name === "auth-http")
    }
  };
}

function createTempDir(prefix) {
  const tmpRoot = path.join(projectRoot, ".tmp");
  fs.mkdirSync(tmpRoot, { recursive: true });
  return fs.mkdtempSync(path.join(tmpRoot, prefix));
}

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, JSON.stringify(value, null, 2), "utf8");
}

function readJson(filePath) {
  const text = fs.readFileSync(filePath, "utf8").replace(/^\uFEFF/, "");
  return JSON.parse(text);
}

function scriptArgs(args) {
  const common = ["-NoProfile"];
  if (process.platform === "win32") {
    common.push("-ExecutionPolicy", "Bypass");
  }
  return [
    ...common,
    "-File",
    path.join(projectRoot, "scripts", "rollout-three-process-drill.ps1"),
    ...args
  ];
}

function runDrill(args) {
  return spawnSync(powershellCommand, scriptArgs(args), {
    cwd: projectRoot,
    encoding: "utf8",
    env: {
      ...process.env,
      LOG_ENABLE_CONSOLE: "false",
      LOG_ENABLE_FILE: "false"
    },
    timeout: 60000
  });
}

test("rollout three-process drill strict dry-run resolves every control endpoint from registry fixture", { skip: powershellSkip }, () => {
  const tempDir = createTempDir("rollout-drill-strict-");
  try {
    const fixturePath = path.join(tempDir, "registry-fixture.json");
    const reportPath = path.join(tempDir, "report.json");
    writeJson(fixturePath, registryFixture());

    const result = runDrill([
      "-SkipPortProbe",
      "-EnvironmentName", "test",
      "-RegistryEnabled", "true",
      "-DiscoveryRequired", "true",
      "-RegistryFixturePath", fixturePath,
      "-ReportPath", reportPath,
      "-RoomId", "room-rollout-test",
      "-RolloutEpoch", "rollout-test",
      "-OldServerId", "game-server-old",
      "-NewServerId", "game-server-new"
    ]);

    assert.equal(result.status, 0, `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
    const report = readJson(reportPath);

    assert.equal(report.ok, true);
    assert.equal(report.mode, "dry-run");
    assert.equal(report.inputs.environmentName, "test");
    assert.equal(report.inputs.registryEnabled, true);
    assert.equal(report.inputs.discoveryRequired, true);
    assert.equal(report.discovery.mode, "registry");
    assert.equal(report.discovery.source, "fixture");
    assert.equal(report.resolvedEndpoints.source, "registry");
    assert.equal(report.resolvedEndpoints.discoverySource, "fixture");

    assert.deepEqual(report.resolvedEndpoints.endpointSources, {
      oldGameServerAdmin: "registry",
      newGameServerAdmin: "registry",
      authHttp: "registry",
      gameProxyAdmin: "registry"
    });

    assert.deepEqual(report.resolvedEndpoints.endpoints.oldGameServerAdmin, {
      instanceId: "game-server-old",
      endpointName: "admin",
      protocol: "tcp",
      host: "127.0.0.10",
      port: 17500,
      url: "",
      source: "registry",
      registrySource: "fixture"
    });
    assert.deepEqual(report.resolvedEndpoints.endpoints.newGameServerAdmin, {
      instanceId: "game-server-new",
      endpointName: "admin",
      protocol: "tcp",
      host: "127.0.0.11",
      port: 17501,
      url: "",
      source: "registry",
      registrySource: "fixture"
    });
    assert.deepEqual(report.resolvedEndpoints.endpoints.gameProxyAdmin, {
      instanceId: "game-proxy-rollout",
      endpointName: "admin",
      protocol: "http",
      host: "127.0.0.12",
      port: 17101,
      url: "http://127.0.0.12:17101",
      source: "registry",
      registrySource: "fixture"
    });
    assert.deepEqual(report.resolvedEndpoints.endpoints.authHttp, {
      instanceId: "auth-http-rollout",
      endpointName: "internal",
      protocol: "http",
      host: "127.0.0.13",
      port: 13080,
      url: "http://127.0.0.13:13080",
      source: "registry",
      registrySource: "fixture"
    });

    assert.equal(report.transfer.ok, true);
    assert.equal(report.transfer.mode, "transfer-dry-run");
    assert.equal(report.transfer.plan.endpoints.oldGameServerAdmin.endpoint, "127.0.0.10:17500");
    assert.equal(report.transfer.plan.endpoints.newGameServerAdmin.endpoint, "127.0.0.11:17501");
    assert.equal(report.transfer.plan.endpoints.gameProxyAdmin.url, "http://127.0.0.12:17101");
    assert.equal(JSON.stringify(report.resolvedEndpoints).includes("fallback"), false);
    assert.match(result.stdout, /Mode: registry/);
    assert.match(result.stdout, /Source: fixture/);
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("rollout three-process drill refuses auth local fallback in strict test discovery", { skip: powershellSkip }, () => {
  const tempDir = createTempDir("rollout-drill-strict-fail-");
  try {
    const fixturePath = path.join(tempDir, "registry-fixture.json");
    const reportPath = path.join(tempDir, "report.json");
    writeJson(fixturePath, registryFixture({ includeAuthInternal: false }));

    const result = runDrill([
      "-SkipPortProbe",
      "-EnvironmentName", "test",
      "-RegistryEnabled", "true",
      "-DiscoveryRequired", "false",
      "-RegistryFixturePath", fixturePath,
      "-ReportPath", reportPath,
      "-AuthBaseUrl", "http://127.0.0.1:3000",
      "-RoomId", "room-rollout-test",
      "-RolloutEpoch", "rollout-test",
      "-OldServerId", "game-server-old",
      "-NewServerId", "game-server-new"
    ]);

    assert.notEqual(result.status, 0, "strict discovery unexpectedly allowed auth-http local fallback");
    assert.match(`${result.stdout}\n${result.stderr}`, /auth-http\.internal endpoint not found in registry/);

    const report = readJson(reportPath);
    assert.equal(report.ok, false);
    assert.equal(report.inputs.environmentName, "test");
    assert.equal(report.inputs.registryEnabled, true);
    assert.equal(report.inputs.discoveryRequired, true);
    assert.equal(report.discovery, null);
    assert.equal(report.resolvedEndpoints, null);
    assert.equal(JSON.stringify(report).includes("fallback-auth-base-url"), false);
    assert.ok(report.stages.some((stage) => stage.stage === "script" && stage.status === "failed"));
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});
