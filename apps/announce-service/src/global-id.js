import { acquireRedisWorkerLease, createGlobalIdGeneratorFromEnv } from "../../../packages/global-id/node/index.js";

let announcementIdGenerator = null;
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
    serviceName: config.serviceName || config.appName || "announce-service",
    serviceInstanceId: config.serviceInstanceId || "announce-service",
    redisKeyPrefix: config.redisKeyPrefix || "",
    onLeaseLost: handleLeaseLost
  });
  announcementIdGenerator = workerLease.createGenerator({ prefix: "ann" });
  return workerLease;
}

function handleLeaseLost(details) {
  if (typeof leaseLossHandler === "function") {
    return leaseLossHandler(details);
  }
  console.error("global id worker lease lost before announce-service shutdown handler was installed", details);
  process.exit(1);
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
