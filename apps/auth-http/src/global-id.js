import { acquireRedisWorkerLease, createGlobalIdGeneratorFromEnv } from "../../../packages/global-id/node/index.js";

let playerIdGenerator = null;
let workerLease = null;
let leaseLossHandler = null;

export function setGlobalIdLeaseLossHandler(handler) {
  leaseLossHandler = handler;
}

export async function initializeGlobalIdLease(config, redis) {
  if (workerLease) {
    return workerLease;
  }

  workerLease = await acquireRedisWorkerLease({
    redis,
    originId: config.globalIdOriginId,
    workerId: config.globalIdWorkerId,
    serviceName: config.appName || "auth-http",
    serviceInstanceId: config.serviceInstanceId || "auth-http",
    redisKeyPrefix: config.redisKeyPrefix || "",
    onLeaseLost: handleLeaseLost
  });
  playerIdGenerator = workerLease.createGenerator({ prefix: "plr" });
  return workerLease;
}

function handleLeaseLost(details) {
  if (typeof leaseLossHandler === "function") {
    return leaseLossHandler(details);
  }
  console.error("global id worker lease lost before auth-http shutdown handler was installed", details);
  process.exit(1);
}

export async function releaseGlobalIdLease() {
  const lease = workerLease;
  workerLease = null;
  playerIdGenerator = null;
  await lease?.release?.();
}

function getPlayerIdGenerator() {
  playerIdGenerator ??= createGlobalIdGeneratorFromEnv({ prefix: "plr" });
  return playerIdGenerator;
}

export function generatePlayerId() {
  return getPlayerIdGenerator().generateString();
}

export function generateCharacterId() {
  return getPlayerIdGenerator().generateString("chr");
}
