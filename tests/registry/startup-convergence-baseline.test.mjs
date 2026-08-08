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

test("game-server owns its worker lease before listeners and keeps match convergence recoverable", () => {
  const server = source("apps/game-server/src/server.rs");
  const matchClient = source("apps/game-server/src/match_client.rs");
  const entry = scenario("game-server-match-not-registered");

  const socketPair = indexOfOrFail(server, "let local_socket_listener = crate::local_socket::create_owned_listener(", "first owned local socket bind");
  const lease = indexOfOrFail(server, "WorkerLease::acquire_redis(", "worker lease acquisition");
  const matchConfig = indexOfOrFail(server, "MatchClientConfig::from_env().await", "match config parsing");
  const convergence = indexOfOrFail(server, "spawn_match_client_rediscovery(", "match convergence task");

  assert.ok(lease < socketPair);
  assert.ok(socketPair < matchConfig);
  assert.ok(matchConfig < convergence);
  assert.doesNotMatch(matchClient, /fn require_initial_match_discovery/);
  assert.match(matchClient, /spawn_convergence\(convergence_config/);
  assert.match(matchClient, /RegistryClient::new_lazy/);
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

test("game-server lease wait and cleanup are bounded, cancellable, and ownership ordered", () => {
  const startup = source("apps/game-server/src/startup.rs");
  const server = source("apps/game-server/src/server.rs");

  assert.match(startup, /GLOBAL_ID_WORKER_LEASE_WAIT_TIMEOUT_SECS/);
  assert.match(startup, /tokio::time::sleep_until\(deadline\)/);
  assert.match(startup, /LeaseWaitError::Cancelled/);
  assert.match(startup, /OwnedResource::NetworkListeners \| OwnedResource::LocalSockets/);
  assert.match(startup, /CleanupStep::StopBackgroundTasks[\s\S]+CleanupStep::CloseStores/);
  assert.match(startup, /let run_result = run\.await;[\s\S]+let cleanup_report = run_cleanup\(executor\)\.await/);
  assert.match(server, /run_then_cleanup\(std::future::ready\(run_result\), &mut resources\)\.await/);
  assert.match(server, /WorkerLease::acquire_redis[\s\S]+TcpListener::bind/);
  assert.match(server, /match \(run_result, cleanup_report\.failures\.is_empty\(\)\)/);
});

test("socket pair source preserves lease-first local-before-internal bootstrap order", () => {
  const server = source("apps/game-server/src/server.rs");
  const localSocket = source("apps/game-server/src/local_socket.rs");
  const productionLocalSocket = localSocket.slice(0, indexOfOrFail(localSocket, "#[cfg(test)]", "test module"));
  const entry = scenario("game-server-two-residual-local-sockets");

  assert.match(localSocket, /ListenerOptions::new\(\)\.name\(to_name\(name\)\?\)\.create_tokio\(\)/);
  assert.match(productionLocalSocket, /std::fs::remove_file\(&path\)/);
  assert.doesNotMatch(productionLocalSocket, /remove_dir_all/i);
  const lease = indexOfOrFail(server, "WorkerLease::acquire_redis(", "worker lease acquisition");
  const localCreate = indexOfOrFail(server, "let local_socket_listener = crate::local_socket::create_owned_listener(", "local socket create");
  const localTrack = indexOfOrFail(server, "capture_owned_socket(\n                &config.local_socket_name", "local socket ownership registration");
  const internalCreate = indexOfOrFail(server, "let internal_socket_listener = crate::local_socket::create_owned_listener(", "internal socket create");
  const internalTrack = indexOfOrFail(server, "capture_owned_socket(\n                &config.internal_socket_name", "internal socket ownership registration");
  assert.ok(lease < localCreate);
  assert.ok(localCreate < localTrack);
  assert.ok(localTrack < internalCreate);
  assert.ok(internalCreate < internalTrack);
  assert.match(productionLocalSocket, /worker lease is required before socket reclaim/);
  assert.match(productionLocalSocket, /socket path is not the current instance owned target/);
  assert.match(productionLocalSocket, /symlink_metadata\(&path\)/);
  assert.match(productionLocalSocket, /is_socket\(\)/);
  assert.match(productionLocalSocket, /refusing to reclaim socket \{path\} after probe timeout/);
  assert.match(productionLocalSocket, /ConnectionRefused \| io::ErrorKind::NotFound/);
  assert.match(server, /CleanupStep::ReleaseListenersAndSockets[\s\S]+verify_worker_lease_ownership\(\)[\s\S]+remove_owned_socket_path\(&socket\)/);
  assert.doesNotMatch(productionLocalSocket, /pub fn remove_socket_path\(/);
  assert.equal(entry.currentBehavior.createMode, "create_without_reclaim");
});

test("phase 5 derives per-instance sockets and proxy consumes only healthy published paths", () => {
  const config = source("apps/game-server/src/config.rs");
  const main = source("apps/game-server/src/main.rs");
  const server = source("apps/game-server/src/server.rs");
  const proxy = source("apps/game-proxy/src/proxy_server.rs");
  const compose = source("deploy/docker/compose.production.yml");

  assert.match(config, /build_instance_socket_names\([\s\S]+&service_instance_id/);
  assert.match(config, /GAME_SOCKET_ROOT/);
  assert.match(config, /GAME_SOCKET_BASENAME/);
  assert.doesNotMatch(config, /env::var\("GAME_(?:LOCAL|INTERNAL)_SOCKET_NAME"\)/);
  assert.match(proxy, /endpoint\.name == "proxy-local" && endpoint\.healthy && endpoint\.is_valid\(\)/);
  assert.match(proxy, /let socket = endpoint\.socket\.trim\(\)/);
  assert.match(proxy, /route_store\.sync_discovered_routes\(routes\)\.await/);
  assert.match(main, /name: "proxy-local"\.to_string\(\)[\s\S]+socket: config\.local_socket_name\.clone\(\)/);
  assert.match(compose, /SERVICE_INSTANCE_ID: \$\{GAME_SERVER_INSTANCE_ID:-game-server-1\}/);
  assert.match(compose, /GLOBAL_ID_WORKER_ID: \$\{GAME_SERVER_WORKER_ID:-5\}/);
  assert.doesNotMatch(compose, /rm -f \/run\/myserver\/myserver-game-server/);
});

test("drain keeps reconnect transport while rejecting new sessions and driving unhealthy publication", () => {
  const server = source("apps/game-server/src/server.rs");
  const coreService = source("apps/game-server/src/core/service/core_service.rs");
  const runtime = source("apps/game-server/src/admin_server/runtime_config.rs");
  const shutdown = source("apps/game-server/src/admin_server/rollout_status.rs");
  const proxy = source("apps/game-proxy/src/proxy_server.rs");

  assert.match(runtime, /drain_state_tx\.send_replace\(parsed\)/);
  assert.match(server, /result = tcp_listener\.accept\(\) => Some\(result\)/);
  assert.match(server, /socket = listener\.accept\(\) => socket\?/);
  assert.match(coreService, /drain_mode_enabled[\s\S]+find_room_by_offline_character[\s\S]+SERVER_DRAINING_REJECT_NEW_SESSION/);
  assert.match(server, /mark_degraded\([\s\S]+"server-listeners"[\s\S]+StartupErrorCode::DependencyPending/);
  assert.match(proxy, /endpoint\.name == "proxy-local" && endpoint\.healthy && endpoint\.is_valid\(\)/);
  assert.match(server, /run_drain_shutdown_monitor/);
  assert.match(server, /arm_rx\.recv\(\)\.await/);
  assert.match(server, /DrainShutdownDecision::TimedOut[\s\S]+active sessions and rooms remain protected/);
  assert.doesNotMatch(runtime, /connection_tasks\.(?:abort|clear)/);
  assert.match(shutdown, /!runtime\.drain_mode_enabled[\s\S]+connection_count != 0[\s\S]+owned_room_count != 0[\s\S]+migrating_room_count != 0/);
  assert.match(server, /run_then_cleanup\(std::future::ready\(run_result\), &mut resources\)\.await/);
});

test("proxy binds frontends before starting recoverable upstream convergence", () => {
  const proxy = source("apps/game-proxy/src/proxy_server.rs");
  const entry = scenario("game-proxy-upstream-endpoint-missing");

  const kcpBind = indexOfOrFail(proxy, "KcpFrontend::bind", "KCP frontend bind");
  const tcpBind = indexOfOrFail(proxy, "TcpFrontend::bind", "TCP frontend bind");
  const convergence = indexOfOrFail(proxy, "spawn_upstream_discovery(", "upstream convergence");
  assert.ok(kcpBind < convergence);
  assert.ok(tcpBind < convergence);
  assert.doesNotMatch(proxy, /fn validate_initial_upstream_routes/);
  assert.match(proxy, /ConvergenceAttempt::Retry\(StartupErrorCode::DependencyPending\)/);
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

test("target services publish unhealthy until stable readiness and recover registration", () => {
  const registry = source("packages/service-registry/src/client.rs");
  const publication = source("packages/service-registry/src/publication.rs");
  const convergence = source("packages/service-registry/src/convergence.rs");
  assert.match(registry, /pub fn new_lazy/);
  assert.match(publication, /PublicationAction::Register\(false\)/);
  assert.match(publication, /let desired_healthy = health_state\.snapshot\(\)\.ready/);
  assert.match(publication, /StartupErrorCode::RegistryUnavailable/);
  assert.match(convergence, /bounded_exponential_delay/);
  assert.match(convergence, /jitter\s*\.apply/);

  for (const entries of [
    ["apps/game-server/src/main.rs", "apps/game-server/src/server.rs"],
    ["apps/game-proxy/src/main.rs"],
    ["apps/match-service/src/main.rs"]
  ]) {
    const contents = entries.map(source).join("\n");
    assert.match(contents, /RegistryClient::new_lazy/);
    assert.match(contents, /spawn_registry_publication/);
    assert.doesNotMatch(contents, /start_heartbeat_task_with_observer/);
    assert.match(
      contents,
      /HealthState::try_from_env|HealthConfig::try_from_env[\s\S]+HealthState::new/
    );
  }
});

test("lease-owning production services handle Docker SIGTERM gracefully", () => {
  for (const relativePath of [
    "apps/match-service/src/server.rs",
    "apps/chat-server/src/chat_server.rs"
  ]) {
    const contents = source(relativePath);
    assert.match(contents, /async fn shutdown_signal\(\)/);
    assert.match(contents, /SignalKind::terminate\(\)/);
    assert.match(contents, /tokio::signal::ctrl_c\(\)/);
    assert.ok((contents.match(/shutdown_signal\(\)/g) ?? []).length >= 2);
  }
});
