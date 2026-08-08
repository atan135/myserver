import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const read = (path) => readFile(new URL(`../../${path}`, import.meta.url), "utf8");

function serviceBlock(compose, serviceName) {
  const pattern = new RegExp(
    `(?:^|\\r?\\n)  ${serviceName}:\\r?\\n([\\s\\S]*?)(?=\\r?\\n  [a-z][a-z0-9-]*:|\\r?\\nvolumes:)`
  );
  const match = compose.match(pattern);
  assert.ok(match, `${serviceName} service must exist`);
  return match[1];
}

test("production game services share instance-specific absolute local socket paths", async () => {
  const compose = await read("deploy/docker/compose.production.yml");
  const applyScript = await read("scripts/docker/server-apply-release.sh");
  const socketInit = serviceBlock(compose, "game-socket-init");
  const gameServer = serviceBlock(compose, "game-server");
  const gameProxy = serviceBlock(compose, "game-proxy");
  const matchService = serviceBlock(compose, "match-service");

  assert.match(
    gameServer,
    /^      GAME_SOCKET_ROOT: \/run\/myserver$/m
  );
  assert.match(
    gameServer,
    /^      GAME_SOCKET_BASENAME: game-server$/m
  );
  assert.match(gameServer, /^      SERVICE_INSTANCE_ID: \$\{GAME_SERVER_INSTANCE_ID:-game-server-1\}$/m);
  assert.match(gameServer, /^      GLOBAL_ID_WORKER_ID: \$\{GAME_SERVER_WORKER_ID:-5\}$/m);
  assert.match(socketInit, /^    user: "0:0"$/m);
  assert.match(
    socketInit,
    /^    command: \["install -d -o 10001 -g 10001 -m 0770 \/run\/myserver"\]$/m
  );
  assert.match(socketInit, /^      - game-sockets:\/run\/myserver$/m);
  assert.doesNotMatch(compose, /^  game-socket-clean:$/m);
  assert.doesNotMatch(applyScript, /game-socket-clean/);
  assert.doesNotMatch(applyScript, /stop game-server/);
  assert.match(applyScript, /up -d game-server match-service chat-server/);
  assert.match(
    gameServer,
    /^      game-socket-init:\r?\n        condition: service_completed_successfully$/m
  );

  for (const service of [gameServer, gameProxy, matchService]) {
    assert.match(service, /^      - game-sockets:\/run\/myserver$/m);
  }
});
