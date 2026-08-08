import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";

const projectRoot = process.cwd();
const fixture = JSON.parse(fs.readFileSync(
  path.join(projectRoot, "tests/fixtures/startup-convergence-baseline.json"),
  "utf8"
));

function source(relativePath) {
  return fs.readFileSync(path.join(projectRoot, relativePath), "utf8");
}

function scenario(id) {
  const value = fixture.scenarios.find((entry) => entry.id === id);
  assert.ok(value, `missing startup fixture: ${id}`);
  return value;
}

function indexOfOrFail(contents, marker, label) {
  const index = contents.indexOf(marker);
  assert.notEqual(index, -1, `missing ${label}: ${marker}`);
  return index;
}

function assertRequiredDependencyGraphIsAcyclic(contracts) {
  const graph = new Map();
  for (const entry of contracts.filter((value) => value.requirement === "required")) {
    const dependencies = graph.get(entry.consumer) ?? [];
    dependencies.push(entry.dependency);
    graph.set(entry.consumer, dependencies);
  }

  const visiting = new Set();
  const visited = new Set();
  function visit(service) {
    assert.equal(visiting.has(service), false, `required dependency cycle includes ${service}`);
    if (visited.has(service)) {
      return;
    }
    visiting.add(service);
    for (const dependency of graph.get(service) ?? []) {
      visit(dependency);
    }
    visiting.delete(service);
    visited.add(service);
  }

  for (const service of graph.keys()) {
    visit(service);
  }
}

test("startup fixture covers the four reproducible baseline failures", () => {
  assert.equal(fixture.schemaVersion, 1);
  assert.deepEqual(
    fixture.scenarios.map((entry) => entry.id),
    [
      "game-server-match-not-registered",
      "game-server-worker-lease-unexpired",
      "game-server-two-residual-local-sockets",
      "game-proxy-upstream-endpoint-missing"
    ]
  );
  assert.deepEqual(fixture.dependencyContracts, [
    {
      consumer: "game-server",
      dependency: "match-service",
      endpoint: "grpc",
      requirement: "required"
    },
    {
      consumer: "match-service",
      dependency: "game-server",
      endpoint: "internal",
      requirement: "optional_capability"
    },
    {
      consumer: "game-proxy",
      dependency: "game-server",
      endpoint: "proxy-local",
      requirement: "required"
    }
  ]);

  assertRequiredDependencyGraphIsAcyclic(fixture.dependencyContracts);

  const sockets = scenario("game-server-two-residual-local-sockets");
  assert.equal(sockets.condition.socketPaths.length, 2);
  assert.equal(new Set(sockets.condition.socketPaths).size, 2);

  for (const entry of fixture.scenarios) {
    assert.match(entry.currentBehavior.errorCode, /^[A-Z][A-Z_]+$/);
    assert.equal(entry.targetBehavior.ready, false);
  }
});

test("source order places initial match discovery after socket pair and lease acquisition", () => {
  const server = source("apps/game-server/src/server.rs");
  const matchClient = source("apps/game-server/src/match_client.rs");
  const entry = scenario("game-server-match-not-registered");

  const socketPair = indexOfOrFail(server, "create_listener_pair(", "local socket pair bind");
  const lease = indexOfOrFail(server, "WorkerLease::acquire_redis(", "worker lease acquisition");
  const initialMatchConfig = indexOfOrFail(server, "MatchClientConfig::from_env().await", "initial match discovery");
  const caughtConnectError = indexOfOrFail(server, "if let Err(e) = init_match_client", "non-fatal match connection error");

  assert.ok(socketPair < lease);
  assert.ok(lease < initialMatchConfig);
  assert.ok(initialMatchConfig < caughtConnectError);
  assert.match(matchClient, /fn require_initial_match_discovery[\s\S]+?result\.unwrap_or_else\(\|error\|\s*\{\s*panic!\(/);
  assert.match(matchClient, /match-service grpc endpoint not found/);
  assert.equal(entry.currentBehavior.processOutcome, "panic");
  assert.deepEqual(entry.currentBehavior.resourceStateAtFailure, [
    "player_listener_bound",
    "admin_listener_bound",
    "two_local_sockets_bound",
    "worker_lease_acquired"
  ]);
});

test("worker lease source routes SET NX results through the tested classifier", () => {
  const globalId = source("packages/global-id/src/lib.rs");
  const entry = scenario("game-server-worker-lease-unexpired");

  assert.match(globalId, /\.arg\("NX"\)/);
  assert.match(globalId, /classify_worker_lease_set_result\(result, origin_id, worker_id\)\?/);
  assert.equal(entry.currentBehavior.acquireMode, "redis_set_nx_once");
  assert.ok(entry.condition.ttlSecondsRemaining > 0);
});

test("socket pair source preserves local-before-internal bootstrap order", () => {
  const server = source("apps/game-server/src/server.rs");
  const localSocket = source("apps/game-server/src/local_socket.rs");
  const productionLocalSocket = localSocket.slice(0, indexOfOrFail(localSocket, "#[cfg(test)]", "test module"));
  const entry = scenario("game-server-two-residual-local-sockets");

  assert.match(localSocket, /ListenerOptions::new\(\)\.name\(to_name\(name\)\?\)\.create_tokio\(\)/);
  assert.doesNotMatch(productionLocalSocket, /remove_file|reclaim|stale/i);
  assert.match(server, /create_listener_pair\(\s*&config\.local_socket_name,\s*&config\.internal_socket_name/s);
  const localCreate = indexOfOrFail(localSocket, "let local_listener = create(local_name)?", "local socket create");
  const internalCreate = indexOfOrFail(localSocket, "let internal_listener = create(internal_name)?", "internal socket create");
  assert.ok(localCreate < internalCreate);
  assert.equal(entry.currentBehavior.createMode, "create_without_reclaim");
});

test("proxy source routes initial endpoint count through the tested validator", () => {
  const proxy = source("apps/game-proxy/src/proxy_server.rs");
  const entry = scenario("game-proxy-upstream-endpoint-missing");

  assert.match(proxy, /validate_initial_upstream_routes\(&service_name, initial_routes\)\?/);
  assert.match(proxy, /fn validate_initial_upstream_routes[\s\S]+?if initial_routes == 0/);
  assert.match(proxy, /required upstream discovery failed: \{\}\.proxy-local endpoint not found/);
  assert.equal(entry.currentBehavior.processOutcome, "error_return");
  assert.deepEqual(entry.condition.eligibleEndpoints, []);
});

test("stage 2 health contract keeps the required graph acyclic and diagnostics address-free", () => {
  const health = source("packages/service-registry/src/health.rs");
  const game = source("apps/game-server/src/main.rs");
  const proxy = source("apps/game-proxy/src/main.rs");
  const match = source("apps/match-service/src/main.rs");

  assert.match(game, /DependencySpec::required\("match-service", "grpc"\)/);
  assert.match(proxy, /DependencySpec::required\("game-server", "proxy-local"\)/);
  assert.match(match, /DependencySpec::optional\("game-server", "internal"\)/);
  assert.match(health, /last_error_code: Option<StartupErrorCode>/);
  assert.doesNotMatch(health, /pub (?:host|port|socket|url|token|password|credential):/i);
});

test("production config gives all dependency-aware services bounded health windows", () => {
  const compose = source("deploy/docker/compose.production.yml");
  const services = [
    ["game-server", "7600"],
    ["match-service", "7603"],
    ["game-proxy", "7601"]
  ];

  for (const [service, port] of services) {
    const start = indexOfOrFail(compose, `  ${service}:`, `${service} compose block`);
    const remainder = compose.slice(start + 3);
    const nextOffset = remainder.search(/^  [a-z0-9][a-z0-9-]*:\r?$/m);
    const next = nextOffset === -1 ? compose.length : start + 3 + nextOffset;
    const block = compose.slice(start, next);
    assert.match(block, new RegExp(`MYSERVER_HEALTH_BIND_ADDR: 0\\.0\\.0\\.0:${port}`));
    assert.match(block, /MYSERVER_STARTUP_CONVERGENCE_WINDOW_SECS: "120"/);
    assert.match(block, /MYSERVER_READY_STABILITY_WINDOW_SECS: "10"/);
    assert.match(block, /MYSERVER_DEPENDENCY_STALE_WINDOW_SECS: "60"/);
  }
});

test("target services observe heartbeat outcomes without exposing raw errors", () => {
  const registry = source("packages/service-registry/src/client.rs");
  assert.match(registry, /pub enum HeartbeatOutcome\s*\{\s*Succeeded,\s*Failed/);
  assert.match(registry, /pub fn start_heartbeat_task\(&self\)[\s\S]+?start_heartbeat_task_with_observer/);

  for (const entry of [
    "apps/game-server/src/main.rs",
    "apps/game-proxy/src/main.rs",
    "apps/match-service/src/main.rs"
  ]) {
    const contents = source(entry);
    assert.match(contents, /start_heartbeat_task_with_observer/);
    assert.match(contents, /HeartbeatOutcome::Failed[\s\S]+?StartupErrorCode::RegistryUnavailable/);
    assert.match(contents, /HeartbeatOutcome::Succeeded[\s\S]+?mark_ready\("service-registry", "self-registration"\)/);
    assert.match(contents, /HealthState::try_from_env/);
  }
});
