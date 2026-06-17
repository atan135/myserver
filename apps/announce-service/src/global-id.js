import { acquireRedisWorkerLease, createGlobalIdGeneratorFromEnv } from "../../../packages/global-id/node/index.js";

let announcementIdGenerator = null;
let workerLease = null;

export async function initializeGlobalIdLease(config, redis) {
  if (workerLease) {
    return workerLease;
  }

  workerLease = await acquireRedisWorkerLease({
    redis,
    originId: config.globalIdOriginId,
    workerId: config.globalIdWorkerId,
    serviceName: config.serviceName || config.appName || "announce-service",
    serviceInstanceId: config.serviceInstanceId || "announce-service",
    redisKeyPrefix: config.redisKeyPrefix || ""
  });
  announcementIdGenerator = workerLease.createGenerator({ prefix: "ann" });
  return workerLease;
}

export async function releaseGlobalIdLease() {
  const lease = workerLease;
  workerLease = null;
  announcementIdGenerator = null;
  await lease?.release?.();
}

function getAnnouncementIdGenerator() {
  announcementIdGenerator ??= createGlobalIdGeneratorFromEnv({ prefix: "ann" });
  return announcementIdGenerator;
}

export function generateAnnouncementId() {
  return getAnnouncementIdGenerator().generateString();
}
