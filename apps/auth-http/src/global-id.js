import { acquireRedisWorkerLease, createGlobalIdGeneratorFromEnv } from "../../../packages/global-id/node/index.js";

let playerIdGenerator = null;
let workerLease = null;

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
    redisKeyPrefix: config.redisKeyPrefix || ""
  });
  playerIdGenerator = workerLease.createGenerator({ prefix: "plr" });
  return workerLease;
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
