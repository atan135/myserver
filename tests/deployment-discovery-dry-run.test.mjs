import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";

import { createServiceInstancePayload } from "../packages/service-registry/node/registry-schema.js";
import { checkDeploymentDiscoveryDryRun } from "../tools/check-deployment-discovery-dry-run.js";

const projectRoot = process.cwd();

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

function socketEndpoint(name, socket, visibility = "local", metadata = {}) {
  return {
    name,
    protocol: "local_socket",
    host: "",
    port: 0,
    socket,
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
    build_version: "deployment-dry-run-test",
    zone: "staging-a"
  };
}

function servicePayload({ id, name, host, port, endpoints, tags = [] }) {
  return createServiceInstancePayload({
    id,
    name,
    host,
    port,
    endpoints,
    tags,
    weight: 100,
    metadata: metadata(name, id)
  });
}

function registryFixture({ includeGameServerProxyLocal = true } = {}) {
  const gameServerMetadata = metadata("game-server", "game-server-a");
  const gameServerEndpoints = [
    endpoint("client", "tcp", "10.0.10.10", 7000, "internal", gameServerMetadata),
    endpoint("admin", "tcp", "10.0.10.11", 7500, "admin", gameServerMetadata),
    socketEndpoint("internal", "game-server-a-internal.sock", "local", gameServerMetadata)
  ];
  if (includeGameServerProxyLocal) {
    gameServerEndpoints.push(socketEndpoint("proxy-local", "game-server-a-proxy.sock", "local", gameServerMetadata));
  }

  const gameProxyMetadata = metadata("game-proxy", "game-proxy-a");
  const matchMetadata = metadata("match-service", "match-service-a");
  const authMetadata = metadata("auth-http", "auth-http-a");
  const adminMetadata = metadata("admin-api", "admin-api-a");
  const mailMetadata = metadata("mail-service", "mail-service-a");
  const announceMetadata = metadata("announce-service", "announce-service-a");

  return {
    services: {
      "game-server": [
        servicePayload({
          id: "game-server-a",
          name: "game-server",
          host: "10.0.10.10",
          port: 7000,
          endpoints: gameServerEndpoints,
          tags: ["game", "tcp"]
        })
      ],
      "game-proxy": [
        servicePayload({
          id: "game-proxy-a",
          name: "game-proxy",
          host: "10.0.20.10",
          port: 4000,
          endpoints: [
            endpoint("client", "kcp", "10.0.20.10", 4000, "public", gameProxyMetadata),
            endpoint("client-tcp-fallback", "tcp", "10.0.20.10", 14000, "public", gameProxyMetadata),
            endpoint("admin", "http", "10.0.20.11", 7101, "admin", gameProxyMetadata)
          ],
          tags: ["proxy", "kcp"]
        })
      ],
      "match-service": [
        servicePayload({
          id: "match-service-a",
          name: "match-service",
          host: "10.0.30.10",
          port: 9002,
          endpoints: [
            endpoint("grpc", "grpc", "10.0.30.10", 9002, "internal", matchMetadata)
          ],
          tags: ["match", "grpc"]
        })
      ],
      "auth-http": [
        servicePayload({
          id: "auth-http-a",
          name: "auth-http",
          host: "10.0.40.10",
          port: 3000,
          endpoints: [
            endpoint("http", "http", "10.0.40.10", 3000, "public", authMetadata),
            endpoint("internal", "http", "10.0.40.10", 3000, "internal", authMetadata)
          ],
          tags: ["auth", "http"]
        })
      ],
      "admin-api": [
        servicePayload({
          id: "admin-api-a",
          name: "admin-api",
          host: "10.0.50.10",
          port: 3001,
          endpoints: [
            endpoint("http", "http", "10.0.50.10", 3001, "admin", adminMetadata)
          ],
          tags: ["admin", "http"]
        })
      ],
      "mail-service": [
        servicePayload({
          id: "mail-service-a",
          name: "mail-service",
          host: "10.0.60.10",
          port: 9003,
          endpoints: [
            endpoint("http", "http", "10.0.60.10", 9003, "internal", mailMetadata)
          ],
          tags: ["mail", "http"]
        })
      ],
      "announce-service": [
        servicePayload({
          id: "announce-service-a",
          name: "announce-service",
          host: "10.0.70.10",
          port: 9004,
          endpoints: [
            endpoint("http", "http", "10.0.70.10", 9004, "internal", announceMetadata)
          ],
          tags: ["announce", "http"]
        })
      ]
    }
  };
}

function createTempDir(prefix) {
  return fs.mkdtempSync(path.join(os.tmpdir(), prefix));
}

function writeJson(filePath, value) {
  fs.writeFileSync(filePath, JSON.stringify(value, null, 2), "utf8");
}

function findCheck(report, consumer, targetService, endpointName) {
  const check = report.checks.find((item) =>
    item.consumer === consumer &&
    item.targetService === targetService &&
    item.endpointName === endpointName
  );
  assert.ok(check, `missing check ${consumer} -> ${targetService}.${endpointName}`);
  return check;
}

test("deployment discovery dry-run resolves required endpoints from a complete fixture", async () => {
  const tempDir = createTempDir("myserver-deployment-discovery-");
  try {
    const fixturePath = path.join(tempDir, "registry-fixture.json");
    writeJson(fixturePath, registryFixture());

    const report = await checkDeploymentDiscoveryDryRun({
      fixturePath,
      environment: "staging",
      registryEnabled: true,
      discoveryRequired: true,
      generatedAt: "2026-06-18T00:00:00.000Z"
    });

    assert.equal(report.ok, true);
    assert.equal(report.environment.strict, true);
    assert.equal(report.source, "fixture");
    assert.equal(report.summary.failed, 0);
    assert.equal(report.summary.fallbackUsed, false);
    assert.equal(report.serviceCounts["game-server"], 1);

    const proxyClient = findCheck(report, "auth-http", "game-proxy", "client");
    assert.equal(proxyClient.ok, true);
    assert.equal(proxyClient.source, "fixture");
    assert.equal(proxyClient.fallback, false);
    assert.deepEqual(proxyClient.resolvedEndpoints.map((endpoint) => ({
      instanceId: endpoint.instanceId,
      protocol: endpoint.protocol,
      visibility: endpoint.visibility,
      address: endpoint.address,
      source: endpoint.source,
      fallback: endpoint.fallback
    })), [
      {
        instanceId: "game-proxy-a",
        protocol: "kcp",
        visibility: "public",
        address: "10.0.20.10:4000",
        source: "fixture",
        fallback: false
      }
    ]);

    const proxyLocal = findCheck(report, "game-proxy", "game-server", "proxy-local");
    assert.deepEqual(proxyLocal.resolvedEndpoints.map((endpoint) => ({
      instanceId: endpoint.instanceId,
      protocol: endpoint.protocol,
      visibility: endpoint.visibility,
      address: endpoint.address
    })), [
      {
        instanceId: "game-server-a",
        protocol: "local_socket",
        visibility: "local",
        address: "game-server-a-proxy.sock"
      }
    ]);

    const matchGrpc = findCheck(report, "game-server", "match-service", "grpc");
    assert.equal(matchGrpc.resolvedEndpoints[0].address, "10.0.30.10:9002");
    assert.equal(matchGrpc.resolvedEndpoints[0].metadata.zone, "staging-a");

    const adminSelf = findCheck(report, "deployment-preflight", "admin-api", "http");
    assert.equal(adminSelf.resolvedEndpoints[0].visibility, "admin");
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});

test("deployment discovery dry-run CLI fails with clear JSON when a required endpoint is missing", () => {
  const tempDir = createTempDir("myserver-deployment-discovery-fail-");
  try {
    const fixturePath = path.join(tempDir, "registry-fixture.json");
    writeJson(fixturePath, registryFixture({ includeGameServerProxyLocal: false }));

    const result = spawnSync(
      process.execPath,
      [
        "tools/check-deployment-discovery-dry-run.js",
        "--fixture", fixturePath,
        "--environment", "production",
        "--registry-enabled", "true",
        "--discovery-required", "true",
        "--compact"
      ],
      { cwd: projectRoot, encoding: "utf8" }
    );

    assert.equal(result.status, 1, `stdout:\n${result.stdout}\nstderr:\n${result.stderr}`);
    assert.equal(result.stderr, "");
    const report = JSON.parse(result.stdout);
    assert.equal(report.ok, false);
    assert.equal(report.environment.strict, true);
    assert.equal(report.summary.failed, 1);

    const failed = findCheck(report, "game-proxy", "game-server", "proxy-local");
    assert.equal(failed.ok, false);
    assert.deepEqual(failed.resolvedEndpoints, []);
    assert.ok(failed.errors.some((error) =>
      error.code === "endpoint_missing" &&
      error.message.includes("game-server.proxy-local endpoint not found")
    ));
    assert.ok(report.errors.some((error) =>
      error.consumer === "game-proxy" &&
      error.targetService === "game-server" &&
      error.endpointName === "proxy-local" &&
      error.code === "fallback_forbidden"
    ));
  } finally {
    fs.rmSync(tempDir, { recursive: true, force: true });
  }
});
