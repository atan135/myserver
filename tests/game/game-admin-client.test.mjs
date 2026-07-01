import assert from "node:assert/strict";
import net from "node:net";
import { test } from "node:test";

import { GameAdminClient } from "../../apps/auth-http/src/game-admin-client.js";

const MAGIC = 0xcafe;
const VERSION = 1;
const HEADER_LEN = 14;

function listen(server) {
  return new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      server.off("error", reject);
      resolve(server.address().port);
    });
  });
}

function trackSockets(server) {
  const sockets = new Set();
  server.on("connection", (socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
  });
  return sockets;
}

function close(server, sockets) {
  for (const socket of sockets) {
    socket.destroy();
  }
  return new Promise((resolve) => server.close(resolve));
}

function createConfig(port, overrides = {}) {
  return {
    gameServerAdminHost: "127.0.0.1",
    gameServerAdminPort: port,
    registryDiscoveryEnabled: false,
    registryDiscoveryRequired: false,
    localDiscoveryFallbackEnabled: true,
    gameAdminConnectTimeoutMs: 200,
    gameAdminReadTimeoutMs: 100,
    gameAdminWriteTimeoutMs: 200,
    gameAdminMaxResponseBytes: 64,
    ...overrides
  };
}

function oversizedHeader() {
  const header = Buffer.alloc(HEADER_LEN);
  header.writeUInt16BE(MAGIC, 0);
  header.writeUInt8(VERSION, 2);
  header.writeUInt8(0, 3);
  header.writeUInt16BE(2002, 4);
  header.writeUInt32BE(1, 6);
  header.writeUInt32BE(1024, 10);
  return header;
}

test("GameAdminClient rejects read timeout", async () => {
  const server = net.createServer(() => {});
  const sockets = trackSockets(server);
  const port = await listen(server);

  try {
    const client = new GameAdminClient(createConfig(port));
    await assert.rejects(
      () => client.getServerStatus(),
      (error) => error.code === "GAME_ADMIN_READ_TIMEOUT"
    );
  } finally {
    await close(server, sockets);
  }
});

test("GameAdminClient rejects oversized response", async () => {
  const server = net.createServer((socket) => {
    socket.once("data", () => {
      socket.write(oversizedHeader());
    });
  });
  const sockets = trackSockets(server);
  const port = await listen(server);

  try {
    const client = new GameAdminClient(createConfig(port));
    await assert.rejects(
      () => client.getServerStatus(),
      (error) => error.code === "GAME_ADMIN_RESPONSE_TOO_LARGE"
    );
  } finally {
    await close(server, sockets);
  }
});
