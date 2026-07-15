import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { after, before, test } from "node:test";

import Redis from "ioredis";
import pg from "pg";

import {
  createServiceInstancePayload,
  registryHeartbeatKey,
  registryInstanceKey
} from "../../packages/service-registry/node/registry-schema.js";
import { discoverGameServerAdminEndpoints } from "../../apps/mail-service/src/registry-client.js";
import { TcpProtocolClient } from "../../tools/mock-client/src/client.js";
import { MESSAGE_TYPE } from "../../tools/mock-client/src/constants.js";
import { authenticateChatClient } from "../../tools/mock-client/src/scenarios/chat.js";
import { authenticateClient } from "../../tools/mock-client/src/scenarios/room.js";
import {
  findFreePort,
  randomId,
  startNatsServer
} from "../helpers/runtime.mjs";

const { Client, Pool } = pg;
const projectRoot = path.resolve(import.meta.dirname, "..", "..");
const databaseUrl = process.env.TEST_DATABASE_URL;
const redisServerBin = process.env.REDIS_SERVER_BIN || "redis-server";
const executableSuffix = process.platform === "win32" ? ".exe" : "";
const gameServerBin = path.join(projectRoot, "target", "debug", `game-server${executableSuffix}`);
const chatServerBin = path.join(projectRoot, "target", "debug", `chat-server${executableSuffix}`);
const ticketSecret = "mail-acceptance-ticket-secret-2026";
const mailServiceToken = "mail-acceptance-service-token-2026";
const gameAdminToken = "mail-acceptance-game-admin-token-2026";
const redisPrefix = `acceptance:mail:${randomId("run")}:`;
const registryPrefix = `${redisPrefix}registry:`;
const playerId = randomId("player");
const characterId = `chr_${crypto.randomBytes(7).toString("hex").slice(0, 13)}`;
const gameAId = randomId("game-a");
const gameBId = randomId("game-b");
const chatId = randomId("chat");

let redisPort;
let natsPort;
let dbProxyPort;
let gameAPort;
let gameAAdminPort;
let gameBPort;
let gameBAdminPort;
let gameAProxyPort;
let gameBProxyPort;
let chatPort;
let mailAPort;
let mailBPort;
let redisUrl;
let natsUrl;
let proxiedDatabaseUrl;
let ticket;
let directDb;
let redis;
let redisProcess;
let natsServer;
let dbProxy;
let gameAProxy;
let gameBProxy;
let gameAProcess;
let gameBProcess;
let chatProcess;
let mailAProcess;
let mailBProcess;
let gameClient;
let chatClient;
let workerIdCounter = 10;

function nextWorkerId() {
  return workerIdCounter++;
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function waitFor(check, { timeoutMs = 15000, intervalMs = 100, label = "condition" } = {}) {
  const deadline = Date.now() + timeoutMs;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await check();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await delay(intervalMs);
  }
  throw new Error(`timed out waiting for ${label}${lastError ? `: ${lastError.message}` : ""}`);
}

async function waitForPort(port, host = "127.0.0.1", processRef) {
  return waitFor(async () => {
    if (processRef?.child.exitCode !== null) {
      throw new Error(`${processRef.name} exited with ${processRef.child.exitCode}: ${processRef.stderr.join("").slice(-2000)}`);
    }
    return new Promise((resolve) => {
      const socket = net.createConnection({ host, port });
      socket.once("connect", () => {
        socket.destroy();
        resolve(true);
      });
      socket.once("error", () => resolve(false));
    });
  }, { timeoutMs: 60000, label: `${host}:${port}` });
}

function spawnManaged(name, command, args, { cwd = projectRoot, env = {} } = {}) {
  const stdout = [];
  const stderr = [];
  const child = spawn(command, args, {
    cwd,
    env: { ...process.env, ...env },
    stdio: ["ignore", "pipe", "pipe"]
  });
  const append = (target, chunk) => {
    target.push(chunk.toString());
    while (target.join("").length > 100_000) target.shift();
  };
  child.stdout.on("data", (chunk) => append(stdout, chunk));
  child.stderr.on("data", (chunk) => append(stderr, chunk));
  return { name, child, stdout, stderr };
}

async function stopManaged(processRef) {
  if (!processRef?.child || processRef.child.exitCode !== null) return;
  const exited = once(processRef.child, "close").catch(() => []);
  if (process.platform === "win32") {
    const killer = spawn("taskkill", ["/pid", String(processRef.child.pid), "/T", "/F"], { stdio: "ignore" });
    await once(killer, "close").catch(() => []);
  } else {
    processRef.child.kill("SIGKILL");
  }
  await Promise.race([exited, delay(5000)]);
}

class RestartableTcpProxy {
  constructor(name, listenPort, targetPort, targetHost = "127.0.0.1") {
    this.name = name;
    this.listenPort = listenPort;
    this.targetPort = targetPort;
    this.targetHost = targetHost;
    this.server = null;
    this.sockets = new Set();
    this.mode = "pass";
    this.onDroppedResponse = null;
  }

  async start() {
    if (this.server) return;
    this.server = net.createServer((downstream) => {
      this.sockets.add(downstream);
      downstream.once("close", () => this.sockets.delete(downstream));

      if (this.mode === "drop-before-once") {
        this.mode = "pass";
        downstream.once("data", () => downstream.destroy());
        return;
      }

      const upstream = net.createConnection({ host: this.targetHost, port: this.targetPort });
      this.sockets.add(upstream);
      upstream.once("close", () => this.sockets.delete(upstream));
      upstream.on("error", () => downstream.destroy());
      downstream.on("error", () => upstream.destroy());
      downstream.pipe(upstream);

      const dropResponse = this.mode === "drop-response-once";
      if (dropResponse) this.mode = "pass";
      upstream.on("data", async (chunk) => {
        if (!dropResponse) {
          downstream.write(chunk);
          return;
        }
        try {
          await this.onDroppedResponse?.();
        } finally {
          downstream.destroy();
          upstream.destroy();
        }
      });
    });
    await new Promise((resolve, reject) => {
      this.server.once("error", reject);
      this.server.listen(this.listenPort, "127.0.0.1", resolve);
    });
  }

  dropBeforeOnce() {
    this.mode = "drop-before-once";
  }

  dropResponseOnce(callback) {
    this.mode = "drop-response-once";
    this.onDroppedResponse = callback || null;
  }

  async stop() {
    if (!this.server) return;
    for (const socket of this.sockets) socket.destroy();
    this.sockets.clear();
    const server = this.server;
    this.server = null;
    await Promise.race([
      new Promise((resolve) => server.close(resolve)),
      delay(2000)
    ]);
  }
}

async function startRedis() {
  const processRef = spawnManaged("redis-acceptance", redisServerBin, [
    "--bind", "127.0.0.1",
    "--port", String(redisPort),
    "--save", "",
    "--appendonly", "no"
  ]);
  await waitForPort(redisPort, "127.0.0.1", processRef);
  return processRef;
}

function gameEnv(instanceId, gamePort, adminPort) {
  return {
    NODE_ENV: "development",
    APP_ENV: "local",
    SERVICE_NAME: "game-server",
    SERVICE_INSTANCE_ID: instanceId,
    SERVICE_BIND_HOST: "127.0.0.1",
    SERVICE_PUBLIC_HOST: "127.0.0.1",
    SERVICE_ADMIN_BIND_HOST: "127.0.0.1",
    SERVICE_ADMIN_ADVERTISED_HOST: "127.0.0.1",
    GAME_HOST: "127.0.0.1",
    GAME_PORT: String(gamePort),
    ADMIN_HOST: "127.0.0.1",
    ADMIN_PORT: String(adminPort),
    GAME_LOCAL_SOCKET_NAME: `${instanceId}.sock`,
    GAME_INTERNAL_SOCKET_NAME: `${instanceId}-internal.sock`,
    GAME_ADMIN_TOKEN: gameAdminToken,
    GAME_ADMIN_AUDIT_ENABLED: "false",
    GAME_ADMIN_AUDIT_REQUIRE_ACTOR: "true",
    REGISTRY_ENABLED: "false",
    DISCOVERY_REQUIRED: "false",
    DB_ENABLED: "true",
    DATABASE_URL: proxiedDatabaseUrl,
    DB_POOL_SIZE: "3",
    REDIS_URL: redisUrl,
    REDIS_KEY_PREFIX: redisPrefix,
    NATS_URL: natsUrl,
    TICKET_SECRET: ticketSecret,
    GLOBAL_ID_ORIGIN_ID: "31",
    GLOBAL_ID_WORKER_ID: String(nextWorkerId()),
    HEARTBEAT_TIMEOUT_SECS: "300",
    LOG_LEVEL: "error",
    LOG_ENABLE_CONSOLE: "false",
    LOG_ENABLE_FILE: "false"
  };
}

async function startGame(instanceId, gamePort, adminPort) {
  const processRef = spawnManaged(instanceId, gameServerBin, [], {
    cwd: path.join(projectRoot, "apps", "game-server"),
    env: gameEnv(instanceId, gamePort, adminPort)
  });
  await waitForPort(gamePort, "127.0.0.1", processRef);
  await waitForPort(adminPort, "127.0.0.1", processRef);
  return processRef;
}

async function startChat() {
  const processRef = spawnManaged(chatId, chatServerBin, [], {
    cwd: path.join(projectRoot, "apps", "chat-server"),
    env: {
      NODE_ENV: "test",
      APP_ENV: "test",
      SERVICE_NAME: "chat-server",
      SERVICE_INSTANCE_ID: chatId,
      SERVICE_BIND_HOST: "127.0.0.1",
      SERVICE_PUBLIC_HOST: "127.0.0.1",
      CHAT_BIND_ADDR: `127.0.0.1:${chatPort}`,
      CHAT_PUBLIC_HOST: "127.0.0.1",
      CHAT_MAIL_ACCEPT_LEGACY_PAYLOAD: "false",
      CHAT_MAIL_NOTIFY_RECONNECT_BASE_MS: "100",
      CHAT_MAIL_NOTIFY_RECONNECT_MAX_MS: "1000",
      REGISTRY_ENABLED: "true",
      DISCOVERY_REQUIRED: "true",
      REGISTRY_URL: redisUrl,
      REGISTRY_KEY_PREFIX: registryPrefix,
      DB_ENABLED: "false",
      REDIS_URL: redisUrl,
      REDIS_KEY_PREFIX: redisPrefix,
      NATS_URL: natsUrl,
      TICKET_SECRET: ticketSecret,
      GLOBAL_ID_ORIGIN_ID: "31",
      GLOBAL_ID_WORKER_ID: String(nextWorkerId()),
      LOG_LEVEL: "error",
      LOG_ENABLE_CONSOLE: "false",
      LOG_ENABLE_FILE: "false"
    }
  });
  await waitForPort(chatPort, "127.0.0.1", processRef);
  return processRef;
}

function mailEnv(instanceId, port, registryKeyPrefix = registryPrefix) {
  return {
    NODE_ENV: "test",
    APP_ENV: "test",
    SERVICE_NAME: "mail-service",
    SERVICE_INSTANCE_ID: instanceId,
    SERVICE_BIND_HOST: "127.0.0.1",
    SERVICE_ADVERTISED_HOST: "127.0.0.1",
    MAIL_PORT: String(port),
    REGISTRY_ENABLED: "true",
    DISCOVERY_REQUIRED: "true",
    DISALLOW_LEGACY_DIRECT_CONFIG: "true",
    REGISTRY_URL: redisUrl,
    REGISTRY_KEY_PREFIX: registryKeyPrefix,
    DB_ENABLED: "true",
    DATABASE_URL: proxiedDatabaseUrl,
    DB_POOL_SIZE: "3",
    REDIS_URL: redisUrl,
    REDIS_KEY_PREFIX: redisPrefix,
    NATS_URL: natsUrl,
    TICKET_SECRET: ticketSecret,
    MAIL_PLAYER_AUTH_REQUIRED: "true",
    MAIL_SERVICE_TOKEN: mailServiceToken,
    GAME_ADMIN_TOKEN: gameAdminToken,
    GAME_ADMIN_ACTOR: instanceId,
    GAME_ADMIN_CONNECT_TIMEOUT_MS: "500",
    GAME_ADMIN_WRITE_TIMEOUT_MS: "500",
    GAME_ADMIN_READ_TIMEOUT_MS: "1000",
    MAIL_OUTBOX_POLL_INTERVAL_MS: "100",
    MAIL_OUTBOX_LEASE_MS: "1000",
    MAIL_OUTBOX_BACKOFF_BASE_MS: "100",
    MAIL_OUTBOX_BACKOFF_MAX_MS: "1000",
    MAIL_OUTBOX_BACKOFF_JITTER_RATIO: "0",
    MAIL_CLAIM_LEASE_MS: "1000",
    MAIL_CLAIM_RECOVERY_POLL_INTERVAL_MS: "100",
    MAIL_CLAIM_RECOVERY_LEASE_MS: "5000",
    MAIL_CLAIM_RECOVERY_BACKOFF_BASE_MS: "100",
    MAIL_CLAIM_RECOVERY_BACKOFF_MAX_MS: "1000",
    MAIL_CLAIM_RECOVERY_MAX_ATTEMPTS: "50",
    GLOBAL_ID_ORIGIN_ID: "31",
    GLOBAL_ID_WORKER_ID: String(nextWorkerId()),
    LOG_LEVEL: "error",
    LOG_ENABLE_CONSOLE: "false",
    LOG_ENABLE_FILE: "false"
  };
}

async function startMail(instanceId, port, registryKeyPrefix = registryPrefix) {
  const processRef = spawnManaged(instanceId, process.execPath, ["src/server.js"], {
    cwd: path.join(projectRoot, "apps", "mail-service"),
    env: mailEnv(instanceId, port, registryKeyPrefix)
  });
  await waitForPort(port, "127.0.0.1", processRef);
  await waitFor(async () => {
    const response = await fetch(`http://127.0.0.1:${port}/healthz`).catch(() => null);
    return response?.ok;
  }, { timeoutMs: 60000, label: `${instanceId} health` });
  return processRef;
}

function createTicket() {
  const payload = {
    playerId,
    characterId,
    ver: 1,
    nonce: crypto.randomBytes(12).toString("hex"),
    exp: new Date(Date.now() + 30 * 60_000).toISOString()
  };
  const encoded = Buffer.from(JSON.stringify(payload)).toString("base64url");
  const signature = crypto.createHmac("sha256", ticketSecret).update(encoded).digest("base64url");
  return `${encoded}.${signature}`;
}

async function persistTicket() {
  const hash = crypto.createHash("sha256").update(ticket).digest("hex");
  await redis.set(`${redisPrefix}ticket:${hash}`, playerId, "EX", 1800);
  await redis.set(`${redisPrefix}player-ticket-version:${playerId}`, "1", "EX", 1800);
}

function gameRouteKey() {
  const digest = crypto.createHash("sha256").update(characterId).digest("hex");
  return `${redisPrefix}game:online-route:${digest}`;
}

async function connectGame(port, label) {
  const client = new TcpProtocolClient({ host: "127.0.0.1", port, timeoutMs: 5000 }, label);
  await client.connect();
  await authenticateClient(client, { timeoutMs: 5000 }, { ticket });
  await waitFor(async () => {
    const raw = await redis.get(gameRouteKey());
    if (!raw) return false;
    const route = JSON.parse(raw);
    return route.character_id === characterId && route.instance_id === (port === gameAPort ? gameAId : gameBId);
  }, { label: `${label} authoritative route` });
  return client;
}

async function connectChat() {
  const client = new TcpProtocolClient({ host: "127.0.0.1", port: chatPort, timeoutMs: 5000 }, "chat-client");
  await client.connect();
  await authenticateChatClient(client, { timeoutMs: 5000 }, { ticket });
  await waitFor(
    () => redis.get(`${redisPrefix}chat:online:${playerId}`).then((value) => value === chatId),
    { label: "chat online route" }
  );
  return client;
}

async function registerGameEndpoint(instanceId, gamePort, adminProxyPort) {
  const metadata = {
    service_name: "game-server",
    service_instance_id: instanceId,
    instance_id: instanceId,
    build_version: "mail-acceptance",
    zone: "test"
  };
  const payload = createServiceInstancePayload({
    id: instanceId,
    name: "game-server",
    host: "127.0.0.1",
    port: gamePort,
    admin_port: adminProxyPort,
    endpoints: [{
      name: "admin",
      protocol: "tcp",
      host: "127.0.0.1",
      port: adminProxyPort,
      socket: "",
      visibility: "admin",
      metadata,
      healthy: true
    }],
    tags: ["game", "admin", "mail-acceptance"],
    metadata,
    weight: 100
  });
  await redis.hset(registryInstanceKey(registryPrefix, "game-server", instanceId), "data", JSON.stringify(payload));
  await redis.set(registryHeartbeatKey(registryPrefix, "game-server", instanceId), "1", "EX", 1800);
}

async function registerGames() {
  await registerGameEndpoint(gameAId, gameAPort, gameAProxyPort);
  await registerGameEndpoint(gameBId, gameBPort, gameBProxyPort);
}

async function fetchJson(url, options = {}) {
  const response = await fetch(url, options);
  const text = await response.text();
  let payload = null;
  try {
    payload = text ? JSON.parse(text) : null;
  } catch {
    payload = { raw: text };
  }
  return { response, payload };
}

async function createMail(port, title, itemId = 1001, count = 1) {
  const result = await fetchJson(`http://127.0.0.1:${port}/api/v1/mails`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${mailServiceToken}`,
      "content-type": "application/json"
    },
    body: JSON.stringify({
      to_player_id: playerId,
      title,
      content: `acceptance ${title}`,
      attachments: [{ type: "item", item_id: itemId, count, binded: true }],
      mail_type: "system"
    })
  });
  assert.equal(result.response.status, 201, JSON.stringify(result.payload));
  assert.equal(result.payload.ok, true);
  return result.payload.mail_id;
}

function claimMail(port, mailId, body = {}) {
  return fetchJson(`http://127.0.0.1:${port}/api/v1/mails/${mailId}/claim`, {
    method: "POST",
    headers: {
      authorization: `Bearer ${ticket}`,
      "content-type": "application/json"
    },
    body: JSON.stringify(body)
  });
}

async function waitForWorkflow(mailId, expectedStatus = "claimed", timeoutMs = 20000) {
  return waitFor(async () => {
    const { rows } = await directDb.query("SELECT * FROM mail_claim_workflows WHERE mail_id = $1", [mailId]);
    return rows[0]?.status === expectedStatus ? rows[0] : false;
  }, { timeoutMs, label: `${mailId} workflow ${expectedStatus}` });
}

async function retryClaimUntilClaimed(port, mailId, timeoutMs = 15000) {
  const deadline = Date.now() + timeoutMs;
  let lastResult;
  while (Date.now() < deadline) {
    lastResult = await claimMail(port, mailId).catch(() => null);
    const { rows } = await directDb.query("SELECT status FROM mail_claim_workflows WHERE mail_id = $1", [mailId]);
    if (rows[0]?.status === "claimed") return lastResult;
    await delay(300);
  }
  throw new Error(`claim retry did not converge for ${mailId}: ${JSON.stringify(lastResult?.payload)}`);
}

async function waitForOutbox(mailId, expectedStatus = "sent", timeoutMs = 15000) {
  return waitFor(async () => {
    const { rows } = await directDb.query("SELECT * FROM mail_notification_outbox WHERE mail_id = $1", [mailId]);
    return rows[0]?.status === expectedStatus ? rows[0] : false;
  }, { timeoutMs, label: `${mailId} outbox ${expectedStatus}` });
}

async function grantCount(mailId) {
  const { rows } = await directDb.query(
    "SELECT count(*)::integer AS count FROM character_inventory_grants WHERE request_id = $1",
    [`mail_claim:${mailId}`]
  );
  return rows[0].count;
}

async function itemCount(itemId) {
  const { rows } = await directDb.query("SELECT inventory_data FROM character_inventory WHERE character_id = $1", [characterId]);
  const slots = rows[0]?.inventory_data?.slots || [];
  return slots.filter(Boolean).filter((item) => item.item_id === itemId).reduce((sum, item) => sum + item.count, 0);
}

async function expectMailPush(mailId, timeoutMs = 8000) {
  return chatClient.readUntil(
    timeoutMs,
    (packet, decoded) => packet.messageType === MESSAGE_TYPE.MAIL_NOTIFY_PUSH && decoded.mailId === mailId,
    "mail-notify"
  );
}

async function removeGameRegistry() {
  await redis.del(
    registryInstanceKey(registryPrefix, "game-server", gameAId),
    registryHeartbeatKey(registryPrefix, "game-server", gameAId),
    registryInstanceKey(registryPrefix, "game-server", gameBId),
    registryHeartbeatKey(registryPrefix, "game-server", gameBId)
  );
}

before(async () => {
  assert.ok(databaseUrl, "TEST_DATABASE_URL is required");
  assert.ok(fs.existsSync(gameServerBin), `missing ${gameServerBin}`);
  assert.ok(fs.existsSync(chatServerBin), `missing ${chatServerBin}`);
  const acceptanceDatabaseName = new URL(databaseUrl).pathname.replace(/^\//, "");
  assert.match(
    acceptanceDatabaseName,
    /^myserver_mail_acceptance(?:_[a-z0-9_]+)?$/,
    "TEST_DATABASE_URL must target a dedicated mail acceptance database"
  );

  [
    redisPort,
    natsPort,
    dbProxyPort,
    gameAPort,
    gameAAdminPort,
    gameBPort,
    gameBAdminPort,
    gameAProxyPort,
    gameBProxyPort,
    chatPort,
    mailAPort,
    mailBPort
  ] = await Promise.all(Array.from({ length: 12 }, () => findFreePort()));

  redisUrl = `redis://127.0.0.1:${redisPort}`;
  natsUrl = `nats://127.0.0.1:${natsPort}`;
  const directUrl = new URL(databaseUrl);
  const proxyUrl = new URL(databaseUrl);
  proxyUrl.hostname = "127.0.0.1";
  proxyUrl.port = String(dbProxyPort);
  proxiedDatabaseUrl = proxyUrl.toString();

  redisProcess = await startRedis();
  natsServer = await startNatsServer({ port: natsPort });
  dbProxy = new RestartableTcpProxy(
    "postgres-proxy",
    dbProxyPort,
    Number(directUrl.port || 5432),
    directUrl.hostname
  );
  await dbProxy.start();

  directDb = new Pool({ connectionString: databaseUrl, max: 3 });
  await directDb.query("SELECT 1");
  redis = new Redis(redisUrl);
  redis.on("error", () => {});
  ticket = createTicket();
  await persistTicket();

  gameAProcess = await startGame(gameAId, gameAPort, gameAAdminPort);
  gameBProcess = await startGame(gameBId, gameBPort, gameBAdminPort);
  gameAProxy = new RestartableTcpProxy("game-a-admin-proxy", gameAProxyPort, gameAAdminPort);
  gameBProxy = new RestartableTcpProxy("game-b-admin-proxy", gameBProxyPort, gameBAdminPort);
  await gameAProxy.start();
  await gameBProxy.start();
  await registerGames();

  chatProcess = await startChat();
  mailAProcess = await startMail(randomId("mail-a"), mailAPort);
  gameClient = await connectGame(gameAPort, "game-client-a");
  chatClient = await connectChat();
});

after(async () => {
  gameClient?.close();
  chatClient?.close();
  await Promise.allSettled([
    stopManaged(mailAProcess),
    stopManaged(mailBProcess),
    stopManaged(chatProcess),
    stopManaged(gameAProcess),
    stopManaged(gameBProcess)
  ]);
  await Promise.allSettled([gameAProxy?.stop(), gameBProxy?.stop(), dbProxy?.stop()]);
  await natsServer?.close().catch(() => {});
  await stopManaged(redisProcess);
  await redis?.quit().catch(() => {});
  await directDb?.end().catch(() => {});

  if (databaseUrl) {
    const url = new URL(databaseUrl);
    const databaseName = url.pathname.replace(/^\//, "");
    url.pathname = "/postgres";
    const admin = new Client({ connectionString: url.toString() });
    await admin.connect().catch(() => {});
    await admin.query("SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1 AND pid <> pg_backend_pid()", [databaseName]).catch(() => {});
    await admin.query(`DROP DATABASE IF EXISTS "${databaseName.replaceAll('"', '""')}"`).catch(() => {});
    await admin.end().catch(() => {});
  }
});

test("mail reliability fault drill with real PostgreSQL, game-server and chat-server", { timeout: 300000 }, async (t) => {
  await t.test("normal notification and claim use authoritative route", async () => {
    const beforeItems = await itemCount(1001);
    const mailId = await createMail(mailAPort, "normal", 1001, 2);
    const push = await expectMailPush(mailId);
    assert.equal(push.title, "normal");
    const claim = await claimMail(mailAPort, mailId);
    assert.equal(claim.response.status, 200, JSON.stringify(claim.payload));
    const workflow = await waitForWorkflow(mailId);
    assert.equal(workflow.game_instance_id, gameAId);
    assert.equal(await grantCount(mailId), 1);
    assert.equal(await itemCount(1001), beforeItems + 2);
  });

  await t.test("NATS outage retains outbox and republishes after recovery", async () => {
    await natsServer.close();
    natsServer = null;
    const mailId = await createMail(mailAPort, "nats-outage", 1001, 1);
    const pending = await waitFor(async () => {
      const { rows } = await directDb.query("SELECT * FROM mail_notification_outbox WHERE mail_id = $1", [mailId]);
      return ["pending", "sending"].includes(rows[0]?.status) ? rows[0] : false;
    }, { label: "outbox retained while NATS is down" });
    assert.notEqual(pending.status, "sent");
    natsServer = await startNatsServer({ port: natsPort });
    await waitForOutbox(mailId, "sent", 20000);
    const push = await expectMailPush(mailId, 20000);
    assert.equal(push.mailId, mailId);
  });

  await t.test("chat outage does not affect mail and does not replay history", async () => {
    chatClient.close();
    chatClient = null;
    await stopManaged(chatProcess);
    chatProcess = null;
    const mailId = await createMail(mailAPort, "chat-offline", 1001, 1);
    await waitForOutbox(mailId, "sent");
    const { rows } = await directDb.query("SELECT status FROM mails WHERE mail_id = $1", [mailId]);
    assert.equal(rows[0].status, "unread");

    chatProcess = await startChat();
    chatClient = await connectChat();
    await assert.rejects(() => chatClient.readNextPacket(1200), /Timed out waiting/);
    const liveMailId = await createMail(mailAPort, "chat-recovered", 1001, 1);
    assert.equal((await expectMailPush(liveMailId)).mailId, liveMailId);
  });

  await t.test("disconnect before grant recovers once with stable request id", async () => {
    const beforeItems = await itemCount(1001);
    const mailId = await createMail(mailAPort, "game-before", 1001, 3);
    gameClient.close();
    gameClient = null;
    await stopManaged(gameAProcess);
    gameAProcess = null;
    gameAProxy.dropBeforeOnce();
    const claim = await claimMail(mailAPort, mailId);
    assert.ok([202, 503].includes(claim.response.status), JSON.stringify(claim.payload));
    assert.equal(await grantCount(mailId), 0);

    gameAProcess = await startGame(gameAId, gameAPort, gameAAdminPort);
    gameClient = await connectGame(gameAPort, "game-client-a-restarted");
    await retryClaimUntilClaimed(mailAPort, mailId);
    const workflow = await waitForWorkflow(mailId, "claimed", 30000);
    assert.equal(workflow.claim_request_id, `mail_claim:${mailId}`);
    assert.equal(await grantCount(mailId), 1);
    assert.equal(await itemCount(1001), beforeItems + 3);
  });

  await t.test("lost game response reconciles without a second grant", async () => {
    const beforeItems = await itemCount(1001);
    const mailId = await createMail(mailAPort, "response-lost", 1001, 4);
    let responseDropped = false;
    gameAProxy.dropResponseOnce(() => {
      responseDropped = true;
    });
    const claim = await claimMail(mailAPort, mailId);
    assert.ok([202, 503].includes(claim.response.status), JSON.stringify(claim.payload));
    await waitFor(async () => {
      await claimMail(mailAPort, mailId).catch(() => null);
      return responseDropped;
    }, { timeoutMs: 15000, intervalMs: 300, label: "lost response fault trigger" });
    await waitForWorkflow(mailId, "claimed", 30000);
    assert.equal(responseDropped, true);
    assert.equal(await grantCount(mailId), 1);
    assert.equal(await itemCount(1001), beforeItems + 4);
  });

  await t.test("mail crash after game commit converges after restart", async () => {
    const beforeItems = await itemCount(1001);
    const mailId = await createMail(mailAPort, "mail-crash", 1001, 5);
    const crashingProcess = mailAProcess;
    let crashTriggered = false;
    gameAProxy.dropResponseOnce(async () => {
      crashTriggered = true;
      await stopManaged(crashingProcess);
    });
    await waitFor(async () => {
      await claimMail(mailAPort, mailId).catch(() => null);
      return crashTriggered;
    }, { timeoutMs: 15000, intervalMs: 300, label: "mail crash fault trigger" });
    await waitFor(() => crashTriggered, { timeoutMs: 15000, label: "mail crash fault trigger" });
    await waitFor(() => crashingProcess.child.exitCode !== null, { timeoutMs: 10000, label: "crashed mail process exit" });
    mailAProcess = null;
    try {
      await waitFor(() => grantCount(mailId).then((count) => count === 1), {
        timeoutMs: 10000,
        label: `${mailId} committed grant before mail restart`
      });
    } finally {
      mailAProcess = await startMail(randomId("mail-a-restarted"), mailAPort);
    }
    await waitForWorkflow(mailId, "claimed", 30000);
    assert.equal(await grantCount(mailId), 1);
    assert.equal(await itemCount(1001), beforeItems + 5);
  });

  await t.test("two mail instances and game authority switch remain idempotent", async () => {
    mailBProcess = await startMail(randomId("mail-b"), mailBPort);
    gameClient.close();
    await delay(500);
    gameClient = await connectGame(gameBPort, "game-client-b");
    const beforeItems = await itemCount(1001);
    const mailId = await createMail(mailAPort, "multi-instance", 1001, 6);

    const malicious = await claimMail(mailAPort, mailId, {
      target_instance_id: gameAId,
      attachments: [{ type: "item", item_id: 9999, count: 999 }]
    });
    assert.equal(malicious.response.status, 403);
    assert.equal(await grantCount(mailId), 0);

    const [first, second] = await Promise.all([
      claimMail(mailAPort, mailId),
      claimMail(mailBPort, mailId)
    ]);
    assert.ok([200, 202].includes(first.response.status), JSON.stringify(first.payload));
    assert.ok([200, 202].includes(second.response.status), JSON.stringify(second.payload));
    const workflow = await waitForWorkflow(mailId, "claimed", 30000);
    assert.equal(workflow.game_instance_id, gameBId);
    assert.equal(await grantCount(mailId), 1);
    assert.equal(await itemCount(1001), beforeItems + 6);
  });

  await t.test("registry loss and restoration recover the original workflow", async () => {
    const beforeItems = await itemCount(1001);
    const mailId = await createMail(mailAPort, "registry-outage", 1001, 2);
    await removeGameRegistry();
    assert.equal(await redis.exists(registryHeartbeatKey(registryPrefix, "game-server", gameAId)), 0);
    assert.equal(await redis.exists(registryHeartbeatKey(registryPrefix, "game-server", gameBId)), 0);
    await Promise.all([stopManaged(mailAProcess), stopManaged(mailBProcess)]);
    mailAProcess = await startMail(
      randomId("mail-after-registry-loss"),
      mailAPort,
      `${registryPrefix}unavailable:`
    );
    mailBProcess = null;
    const failed = await claimMail(mailAPort, mailId);
    assert.equal(failed.response.status, 503, JSON.stringify(failed.payload));
    assert.equal(await grantCount(mailId), 0);
    await stopManaged(mailAProcess);
    mailAProcess = null;
    await registerGames();
    const restoredEndpoints = await discoverGameServerAdminEndpoints(redis, registryPrefix);
    assert.deepEqual(
      restoredEndpoints.map((endpoint) => endpoint.instanceId).sort(),
      [gameAId, gameBId].sort()
    );
    let restoredRouteRaw = await redis.get(gameRouteKey());
    if (!restoredRouteRaw) {
      gameClient?.close();
      await delay(500);
      gameClient = await connectGame(gameBPort, "game-client-b-after-registry");
      restoredRouteRaw = await redis.get(gameRouteKey());
    }
    const restoredRoute = JSON.parse(restoredRouteRaw);
    assert.ok([gameAId, gameBId].includes(restoredRoute.instance_id));
    mailAProcess = await startMail(randomId("mail-after-registry-restore"), mailAPort);
    const retried = await claimMail(mailAPort, mailId);
    assert.ok([200, 202, 503].includes(retried.response.status), JSON.stringify(retried.payload));
    await retryClaimUntilClaimed(mailAPort, mailId, 20000);
    try {
      await waitForWorkflow(mailId, "claimed", 30000);
    } catch (error) {
      const { rows } = await directDb.query("SELECT * FROM mail_claim_workflows WHERE mail_id = $1", [mailId]);
      throw new Error(
        `${error.message}; retry=${JSON.stringify(retried.payload)}; ` +
        `workflow=${JSON.stringify(rows[0])}; route=${JSON.stringify(restoredRoute)}; ` +
        `endpoints=${JSON.stringify(restoredEndpoints.map((endpoint) => endpoint.instanceId))}`
      );
    }
    assert.equal(await grantCount(mailId), 1);
    assert.equal(await itemCount(1001), beforeItems + 2);
  });

  await t.test("Redis outage is classified and services recover after restart", async () => {
    const mailId = await createMail(mailAPort, "redis-outage", 1001, 2);
    await stopManaged(redisProcess);
    redisProcess = null;
    const failed = await claimMail(mailAPort, mailId);
    assert.equal(failed.response.status, 401, JSON.stringify(failed.payload));
    assert.equal(failed.payload.error?.code || failed.payload.error || failed.payload.code, "AUTH_BACKEND_UNAVAILABLE");
    assert.equal(await grantCount(mailId), 0);

    gameClient.close();
    chatClient.close();
    gameClient = null;
    chatClient = null;
    await Promise.all([
      stopManaged(mailAProcess),
      stopManaged(mailBProcess),
      stopManaged(chatProcess),
      stopManaged(gameAProcess),
      stopManaged(gameBProcess)
    ]);
    mailAProcess = mailBProcess = chatProcess = gameAProcess = gameBProcess = null;
    await redis.disconnect(false);
    redisProcess = await startRedis();
    redis = new Redis(redisUrl);
    redis.on("error", () => {});
    await persistTicket();
    gameAProcess = await startGame(gameAId, gameAPort, gameAAdminPort);
    gameBProcess = await startGame(gameBId, gameBPort, gameBAdminPort);
    await registerGames();
    chatProcess = await startChat();
    mailAProcess = await startMail(randomId("mail-after-redis"), mailAPort);
    gameClient = await connectGame(gameBPort, "game-client-b-after-redis");
    chatClient = await connectChat();
    const recovered = await claimMail(mailAPort, mailId);
    assert.ok([200, 202].includes(recovered.response.status), JSON.stringify(recovered.payload));
    await waitForWorkflow(mailId, "claimed", 30000);
    assert.equal(await grantCount(mailId), 1);
  });

  await t.test("PostgreSQL transport outage does not grant and recovers", async () => {
    const beforeItems = await itemCount(1001);
    const mailId = await createMail(mailAPort, "postgres-outage", 1001, 7);
    const before = await directDb.query("SELECT count(*)::integer AS count FROM mails");
    await dbProxy.stop();
    let createDuringOutage;
    try {
      createDuringOutage = await fetchJson(`http://127.0.0.1:${mailAPort}/api/v1/mails`, {
        method: "POST",
        headers: { authorization: `Bearer ${mailServiceToken}`, "content-type": "application/json" },
        body: JSON.stringify({ to_player_id: playerId, title: "must-fail", attachments: [] })
      });
    } catch (error) {
      throw new Error(
        `mail HTTP failed during PostgreSQL outage: ${error.message}; ` +
        `exit=${mailAProcess?.child.exitCode}; stderr=${mailAProcess?.stderr.join("").slice(-2000)}`
      );
    }
    assert.ok(createDuringOutage.response.status >= 500);
    const claimDuringOutage = await claimMail(mailAPort, mailId);
    assert.ok(claimDuringOutage.response.status >= 500);
    assert.equal(await grantCount(mailId), 0);

    await dbProxy.start();
    const after = await directDb.query("SELECT count(*)::integer AS count FROM mails");
    assert.equal(after.rows[0].count, before.rows[0].count);
    const recovered = await claimMail(mailAPort, mailId);
    assert.ok([200, 202, 503].includes(recovered.response.status), JSON.stringify(recovered.payload));
    await waitForWorkflow(mailId, "claimed", 30000);
    assert.equal(await grantCount(mailId), 1);
    assert.equal(await itemCount(1001), beforeItems + 7);
  });
});
