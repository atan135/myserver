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

test("production game services share absolute local socket paths", async () => {
  const compose = await read("deploy/docker/compose.production.yml");
  const gameServer = serviceBlock(compose, "game-server");
  const gameProxy = serviceBlock(compose, "game-proxy");
  const matchService = serviceBlock(compose, "match-service");

  assert.match(
    gameServer,
    /^      GAME_LOCAL_SOCKET_NAME: \/run\/myserver\/myserver-game-server\.sock$/m
  );
  assert.match(
    gameServer,
    /^      GAME_INTERNAL_SOCKET_NAME: \/run\/myserver\/myserver-game-server-internal\.sock$/m
  );

  for (const service of [gameServer, gameProxy, matchService]) {
    assert.match(service, /^      - game-sockets:\/run\/myserver$/m);
  }
});
