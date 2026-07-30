import { acquireRedisWorkerLease, createGlobalIdGeneratorFromEnv } from "../../../packages/global-id/node/index.js";

let mailIdGenerator = null;
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
    serviceName: config.serviceName || config.appName || "mail-service",
    serviceInstanceId: config.serviceInstanceId || "mail-service",
    redisKeyPrefix: config.redisKeyPrefix || "",
    onLeaseLost: handleLeaseLost
  });
  mailIdGenerator = workerLease.createGenerator({ prefix: "mail" });
  return workerLease;
}

function handleLeaseLost(details) {
  if (typeof leaseLossHandler === "function") {
    return leaseLossHandler(details);
  }
  console.error("global id worker lease lost before mail-service shutdown handler was installed", details);
  process.exit(1);
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
