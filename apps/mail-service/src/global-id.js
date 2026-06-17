import { acquireRedisWorkerLease, createGlobalIdGeneratorFromEnv } from "../../../packages/global-id/node/index.js";

let mailIdGenerator = null;
let workerLease = null;

export async function initializeGlobalIdLease(config, redis) {
  if (workerLease) {
    return workerLease;
  }

  workerLease = await acquireRedisWorkerLease({
    redis,
    originId: config.globalIdOriginId,
    workerId: config.globalIdWorkerId,
    serviceName: config.serviceName || config.appName || "mail-service",
    serviceInstanceId: config.serviceInstanceId || "mail-service",
    redisKeyPrefix: config.redisKeyPrefix || ""
  });
  mailIdGenerator = workerLease.createGenerator({ prefix: "mail" });
  return workerLease;
}

export async function releaseGlobalIdLease() {
  const lease = workerLease;
  workerLease = null;
  mailIdGenerator = null;
  await lease?.release?.();
}

function getMailIdGenerator() {
  mailIdGenerator ??= createGlobalIdGeneratorFromEnv({ prefix: "mail" });
  return mailIdGenerator;
}

export function generateMailId() {
  return getMailIdGenerator().generateString();
}
