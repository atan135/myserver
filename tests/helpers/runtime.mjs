import assert from "node:assert/strict";
import fs from "node:fs";
import { spawn } from "node:child_process";
import { once } from "node:events";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

import Redis from "ioredis";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(__dirname, "..", "..");

export function randomId(prefix = "test") {
  return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2, 10)}`;
}

export async function findFreePort(host = "127.0.0.1") {
  const server = net.createServer();
  server.listen(0, host);
  await once(server, "listening");
  const address = server.address();
  server.close();
  await once(server, "close");
  assert.ok(address && typeof address === "object", "failed to allocate a tcp port");
  return address.port;
}

export async function cleanupRedisPrefix(redisUrl, prefix) {
  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });

  await redis.connect();

  try {
    const keys = await redis.keys(`${prefix}*`);
    if (keys.length > 0) {
      await redis.del(...keys);
    }
  } finally {
    await redis.quit();
  }
}

export async function cleanupRegistryInstances(redisUrl, instances) {
  const redis = new Redis(redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });

  await redis.connect();

  try {
    const keys = [];
    for (const { serviceName, instanceId } of instances) {
      keys.push(`service:${serviceName}:instances:${instanceId}`);
      keys.push(`heartbeat:${serviceName}:${instanceId}`);
    }

    if (keys.length > 0) {
      await redis.del(...keys);
    }
  } finally {
    await redis.quit();
  }
}

function setEnvVars(nextEnv) {
  const previous = new Map();

  for (const [key, value] of Object.entries(nextEnv)) {
    previous.set(key, process.env[key]);
    process.env[key] = value;
  }

  return () => {
    for (const [key, value] of previous.entries()) {
      if (value === undefined) {
        delete process.env[key];
      } else {
        process.env[key] = value;
      }
    }
  };
}

export async function startAuthHttpServer({
  host = "127.0.0.1",
  port,
  ticketSecret,
  redisUrl,
  redisKeyPrefix,
  gameServerAdminHost = "127.0.0.1",
  gameServerAdminPort = 7001,
  envOverrides = {}
}) {
  const restoreEnv = setEnvVars({
    NODE_ENV: "test",
    HOST: host,
    PORT: String(port),
    LOG_LEVEL: "error",
    LOG_ENABLE_CONSOLE: "false",
    LOG_ENABLE_FILE: "false",
    LOG_DIR: "logs/test-auth-http",
    REDIS_URL: redisUrl,
    REDIS_KEY_PREFIX: redisKeyPrefix,
    DB_ENABLED: "false",
    TICKET_SECRET: ticketSecret,
    SESSION_TTL_SECONDS: "600",
    TICKET_TTL_SECONDS: "300",
    GAME_SERVER_ADMIN_HOST: gameServerAdminHost,
    GAME_SERVER_ADMIN_PORT: String(gameServerAdminPort),
    ...envOverrides
  });

  let context;

  try {
    const { createApp } = await import(pathToFileURL(path.join(projectRoot, "apps", "auth-http", "src", "app.js")));
    context = await createApp();
    const { app, config, redis, dbPool, nestApp } = context;

    let httpServer;
    if (nestApp) {
      await nestApp.listen(config.port, config.host);
      httpServer = nestApp.getHttpServer();
    } else {
      httpServer = await new Promise((resolve, reject) => {
        const instance = app.listen(config.port, config.host, () => resolve(instance));
        instance.once("error", reject);
      });
    }

    const address = httpServer.address();
    assert.ok(address && typeof address === "object", "http server did not expose an address");

    return {
      host,
      port: address.port,
      baseUrl: `http://${host}:${address.port}`,
      redisUrl,
      redisKeyPrefix,
      async close() {
        if (context.close) {
          await context.close();
        } else {
          await new Promise((resolve, reject) => {
            httpServer.close((error) => {
              if (error) {
                reject(error);
                return;
              }
              resolve();
            });
          });
          await redis.quit();
          if (dbPool) {
            await dbPool.end();
          }
        }
        restoreEnv();
      }
    };
  } catch (error) {
    if (context?.close) {
      await context.close().catch(() => {});
    }
    restoreEnv();
    throw error;
  }
}

export function resolveCargoBin() {
  if (process.env.CARGO_BIN) {
    return process.env.CARGO_BIN;
  }

  if (process.platform === "win32") {
    return path.join(os.homedir(), ".cargo", "bin", "cargo.exe");
  }

  return "cargo";
}

function resolveNatsServerBin() {
  const envBin = process.env.NATS_SERVER_BIN;
  if (envBin) {
    return envBin;
  }

  const localBin = path.join(
    projectRoot,
    "bin",
    process.platform === "win32" ? "nats-server.exe" : "nats-server"
  );
  if (fs.existsSync(localBin)) {
    return localBin;
  }

  return process.platform === "win32" ? "nats-server.exe" : "nats-server";
}

async function waitForTcpPort({ host, port, timeoutMs = 60000, onTick }) {
  const startedAt = Date.now();

  while (Date.now() - startedAt < timeoutMs) {
    if (onTick) {
      onTick();
    }

    try {
      await new Promise((resolve, reject) => {
        const socket = net.createConnection({ host, port });
        socket.once("connect", () => {
          socket.end();
          resolve();
        });
        socket.once("error", reject);
      });
      return;
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 250));
    }
  }

  throw new Error(`timed out waiting for tcp ${host}:${port}`);
}

function waitForChildExit(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve(true);
  }

  return new Promise((resolve) => {
    let settled = false;
    const finish = (value) => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      child.off("close", onClose);
      child.off("exit", onExit);
      resolve(value);
    };
    const onClose = () => finish(true);
    const onExit = () => finish(true);
    const timer = setTimeout(() => finish(false), timeoutMs);

    child.once("close", onClose);
    child.once("exit", onExit);
  });
}

async function killProcessTree(pid, timeoutMs) {
  if (process.platform !== "win32" || !pid) {
    return;
  }

  const killer = spawn("taskkill", ["/pid", String(pid), "/T", "/F"], {
    stdio: "ignore"
  });

  await new Promise((resolve) => {
    let settled = false;
    const finish = () => {
      if (settled) {
        return;
      }
      settled = true;
      clearTimeout(timer);
      killer.off("close", finish);
      killer.off("error", finish);
      resolve();
    };
    const timer = setTimeout(() => {
      killer.kill();
      finish();
    }, timeoutMs);

    killer.once("close", finish);
    killer.once("error", finish);
  });
}

async function terminateChild(child, { name = "process", timeoutMs = 5000 } = {}) {
  if (child.exitCode !== null || child.signalCode !== null) {
    child.stdout?.destroy();
    child.stderr?.destroy();
    return;
  }

  const pid = child.pid;
  const exitPromise = waitForChildExit(child, timeoutMs);
  let killError = null;

  if (process.platform === "win32") {
    await killProcessTree(pid, timeoutMs);
  } else {
    try {
      child.kill();
    } catch (error) {
      killError = error;
    }
  }

  if (await exitPromise) {
    child.stdout?.destroy();
    child.stderr?.destroy();
    return;
  }

  if (process.platform === "win32") {
    try {
      child.kill();
    } catch (error) {
      killError ??= error;
    }
  } else {
    try {
      child.kill("SIGKILL");
    } catch (error) {
      killError ??= error;
    }
  }

  if (await waitForChildExit(child, timeoutMs)) {
    child.stdout?.destroy();
    child.stderr?.destroy();
    return;
  }

  child.stdout?.destroy();
  child.stderr?.destroy();
  const detail = killError ? `: ${killError.message}` : "";
  throw new Error(`timed out terminating ${name}${pid ? ` pid ${pid}` : ""}${detail}`);
}

export async function startNatsServer({ host = "127.0.0.1", port } = {}) {
  const stdout = [];
  const stderr = [];
  const natsPort = port || await findFreePort(host);
  const binaryPath = resolveNatsServerBin();
  let spawnError = null;

  const child = spawn(binaryPath, ["-a", host, "-p", String(natsPort)], {
    cwd: projectRoot,
    stdio: ["ignore", "pipe", "pipe"]
  });

  child.once("error", (error) => {
    spawnError = error;
  });
  child.stdout.on("data", (chunk) => {
    stdout.push(chunk.toString());
  });
  child.stderr.on("data", (chunk) => {
    stderr.push(chunk.toString());
  });

  try {
    await waitForTcpPort({
      host,
      port: natsPort,
      onTick: () => {
        if (spawnError) {
          throw spawnError;
        }
        if (child.exitCode !== null) {
          throw new Error(`nats-server exited early with code ${child.exitCode}`);
        }
      }
    });
  } catch (error) {
    await terminateChild(child, { name: "nats-server" }).catch(() => {});
    throw new Error(`${error.message}\n[nats-server stdout]\n${stdout.join("")}\n[nats-server stderr]\n${stderr.join("")}`);
  }

  return {
    host,
    port: natsPort,
    url: `nats://${host}:${natsPort}`,
    stdout,
    stderr,
    async close() {
      await terminateChild(child, { name: "nats-server" });
    }
  };
}

async function runProcess({ command, args, cwd, env, timeoutMs = 240000 }) {
  const child = spawn(command, args, {
    cwd,
    env,
    stdio: ["ignore", "pipe", "pipe"]
  });

  let stdout = "";
  let stderr = "";
  let spawnError = null;

  child.once("error", (error) => {
    spawnError = error;
  });
  child.stdout.on("data", (chunk) => {
    stdout += chunk.toString();
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk.toString();
  });

  const exitPromise = once(child, "close");
  let timer;
  const timeoutPromise = new Promise((_, reject) => {
    timer = setTimeout(() => {
      reject(new Error(`process timeout after ${timeoutMs}ms`));
    }, timeoutMs);
  });

  try {
    const [code] = await Promise.race([exitPromise, timeoutPromise]);
    clearTimeout(timer);
    if (spawnError) {
      throw spawnError;
    }
    if (code !== 0) {
      throw new Error(`process exited with code ${code}`);
    }
    return { stdout, stderr };
  } catch (error) {
    clearTimeout(timer);
    await terminateChild(child, { name: command }).catch(() => {});
    throw new Error(`${error.message}\n[stdout]\n${stdout}\n[stderr]\n${stderr}`);
  }
}

export async function startGameServer({
  host = "127.0.0.1",
  port,
  adminPort,
  localSocketName = "myserver-game-server.sock",
  ticketSecret,
  redisUrl,
  redisKeyPrefix,
  envOverrides = {}
}) {
  const stdout = [];
  const stderr = [];
  const cargoBin = resolveCargoBin();
  const cargoTargetDir = path.join(projectRoot, ".tmp", "cargo-target", "integration");
  let spawnError = null;
  const cargoEnv = {
    ...process.env,
    GAME_HOST: host,
    GAME_PORT: String(port),
    ADMIN_HOST: host,
    ADMIN_PORT: String(adminPort),
    GAME_LOCAL_SOCKET_NAME: localSocketName,
    LOG_LEVEL: "error",
    LOG_ENABLE_CONSOLE: "false",
    LOG_ENABLE_FILE: "false",
    LOG_DIR: "logs/test-game-server",
    REDIS_URL: redisUrl,
    REDIS_KEY_PREFIX: redisKeyPrefix,
    DB_ENABLED: "false",
    CARGO_TARGET_DIR: cargoTargetDir,
    TICKET_SECRET: ticketSecret,
    HEARTBEAT_TIMEOUT_SECS: "10",
    MAX_BODY_LEN: "4096",
    ...envOverrides
  };

  await runProcess({
    command: cargoBin,
    args: ["build", "--quiet"],
    cwd: path.join(projectRoot, "apps", "game-server"),
    env: cargoEnv
  });

  const binaryPath = path.join(
    cargoTargetDir,
    "debug",
    process.platform === "win32" ? "game-server.exe" : "game-server"
  );

  const child = spawn(binaryPath, [], {
    cwd: path.join(projectRoot, "apps", "game-server"),
    env: cargoEnv,
    stdio: ["ignore", "pipe", "pipe"]
  });

  child.once("error", (error) => {
    spawnError = error;
  });
  child.stdout.on("data", (chunk) => {
    stdout.push(chunk.toString());
  });
  child.stderr.on("data", (chunk) => {
    stderr.push(chunk.toString());
  });

  try {
    await waitForTcpPort({
      host,
      port,
      onTick: () => {
        if (spawnError) {
          throw spawnError;
        }
        if (child.exitCode !== null) {
          throw new Error(`game-server exited early with code ${child.exitCode}`);
        }
      }
    });
  } catch (error) {
    await terminateChild(child, { name: "game-server" }).catch(() => {});
    throw new Error(`${error.message}\n[game-server stdout]\n${stdout.join("")}\n[game-server stderr]\n${stderr.join("")}`);
  }

  await waitForTcpPort({ host, port: adminPort });

  return {
    host,
    port,
    adminPort,
    stdout,
    stderr,
    async close() {
      await terminateChild(child, { name: "game-server" });
    }
  };
}

export async function runMockClientScenario({
  scenario,
  httpBaseUrl,
  host,
  port,
  roomId,
  guestId,
  loginName,
  password,
  loginNameA,
  passwordA,
  loginNameB,
  passwordB,
  timeoutMs = 5000,
  maxBodyLen = 4096,
  gameHost,
  noServiceDiscovery = false,
  processTimeoutMs = 120000
}) {
  const args = [
    path.join(projectRoot, "tools", "mock-client", "src", "index.js"),
    "--scenario",
    scenario,
    "--http-base-url",
    httpBaseUrl,
    "--host",
    host,
    "--port",
    String(port),
    "--timeout-ms",
    String(timeoutMs),
    "--max-body-len",
    String(maxBodyLen)
  ];

  if (gameHost) {
    args.push("--game-host", gameHost);
  }

  if (noServiceDiscovery) {
    args.push("--no-service-discovery");
  }

  if (roomId) {
    args.push("--room-id", roomId);
  }

  if (guestId) {
    args.push("--guest-id", guestId);
  }

  if (loginName) {
    args.push("--login-name", loginName);
  }

  if (password) {
    args.push("--password", password);
  }

  if (loginNameA) {
    args.push("--login-name-a", loginNameA);
  }

  if (passwordA) {
    args.push("--password-a", passwordA);
  }

  if (loginNameB) {
    args.push("--login-name-b", loginNameB);
  }

  if (passwordB) {
    args.push("--password-b", passwordB);
  }

  const child = spawn(process.execPath, args, {
    cwd: projectRoot,
    stdio: ["ignore", "pipe", "pipe"]
  });

  let stdout = "";
  let stderr = "";

  child.stdout.on("data", (chunk) => {
    stdout += chunk.toString();
  });
  child.stderr.on("data", (chunk) => {
    stderr += chunk.toString();
  });

  let timer;
  const exitPromise = once(child, "close");
  const timeoutPromise = new Promise((resolve) => {
    timer = setTimeout(() => {
      resolve(["timeout"]);
    }, processTimeoutMs);
  });

  const [code] = await Promise.race([exitPromise, timeoutPromise]);
  clearTimeout(timer);
  if (code === "timeout") {
    await terminateChild(child, { name: `mock-client ${scenario}` }).catch(() => {});
    throw new Error(`mock-client scenario ${scenario} timed out after ${processTimeoutMs}ms\n[stdout]\n${stdout}\n[stderr]\n${stderr}`);
  }
  if (code !== 0) {
    throw new Error(`mock-client scenario ${scenario} failed with exit ${code}\n[stdout]\n${stdout}\n[stderr]\n${stderr}`);
  }

  return { stdout, stderr };
}

export async function startGameProxy({
  host = "127.0.0.1",
  port,
  adminPort,
  tcpFallbackPort = port + 10000,
  upstreamLocalSocketName = "myserver-game-server.sock",
  envOverrides = {}
}) {
  const stdout = [];
  const stderr = [];
  const cargoBin = resolveCargoBin();
  const cargoTargetDir = path.join(projectRoot, ".tmp", "cargo-target", "integration-proxy");
  let spawnError = null;
  const cargoEnv = {
    ...process.env,
    PROXY_HOST: host,
    PROXY_PORT: String(port),
    PROXY_ADMIN_HOST: host,
    PROXY_ADMIN_PORT: String(adminPort),
    PROXY_TCP_FALLBACK_HOST: host,
    PROXY_TCP_FALLBACK_PORT: String(tcpFallbackPort),
    LOG_LEVEL: "error",
    LOG_ENABLE_CONSOLE: "false",
    LOG_ENABLE_FILE: "false",
    LOG_DIR: "logs/test-game-proxy",
    UPSTREAM_SERVER_ID: "game-server-1",
    UPSTREAM_LOCAL_SOCKET_NAME: upstreamLocalSocketName,
    CARGO_TARGET_DIR: cargoTargetDir,
    ...envOverrides
  };

  await runProcess({
    command: cargoBin,
    args: ["build", "--quiet"],
    cwd: path.join(projectRoot, "apps", "game-proxy"),
    env: cargoEnv
  });

  const binaryPath = path.join(
    cargoTargetDir,
    "debug",
    process.platform === "win32" ? "game-proxy.exe" : "game-proxy"
  );

  const child = spawn(binaryPath, [], {
    cwd: path.join(projectRoot, "apps", "game-proxy"),
    env: cargoEnv,
    stdio: ["ignore", "pipe", "pipe"]
  });

  child.once("error", (error) => {
    spawnError = error;
  });
  child.stdout.on("data", (chunk) => {
    stdout.push(chunk.toString());
  });
  child.stderr.on("data", (chunk) => {
    stderr.push(chunk.toString());
  });

  try {
    await waitForTcpPort({
      host,
      port: adminPort,
      onTick: () => {
        if (spawnError) {
          throw spawnError;
        }
        if (child.exitCode !== null) {
          throw new Error("game-proxy exited early with code " + child.exitCode);
        }
      }
    });
  } catch (error) {
    await terminateChild(child, { name: "game-proxy" }).catch(() => {});
    throw new Error(error.message + "\n[game-proxy stdout]\n" + stdout.join("") + "\n[game-proxy stderr]\n" + stderr.join(""));
  }

  return {
    host,
    port,
    adminPort,
    tcpFallbackPort,
    stdout,
    stderr,
    async close() {
      await terminateChild(child, { name: "game-proxy" });
    }
  };
}


