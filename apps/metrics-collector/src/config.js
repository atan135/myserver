import fs from "node:fs";
import path from "node:path";

import dotenv from "dotenv";

const envPath = path.resolve(process.cwd(), ".env");
if (fs.existsSync(envPath)) {
  dotenv.config({ path: envPath });
}

function parseBoolean(value, fallback) {
  if (value === undefined) return fallback;
  return value === "true" || value === "1";
}

export function getConfig() {
  return {
    serviceName: process.env.SERVICE_NAME || "metrics-collector",
    serviceInstanceId:
      process.env.SERVICE_INSTANCE_ID || "metrics-collector-001",
    serviceZone: process.env.SERVICE_ZONE || "local",
    serviceBuildVersion: process.env.SERVICE_BUILD_VERSION || "dev",
    registryKeyPrefix: process.env.REGISTRY_KEY_PREFIX ?? process.env.REDIS_KEY_PREFIX ?? "",
    serviceRegistryRegister: parseBoolean(
      process.env.SERVICE_REGISTRY_REGISTER,
      false
    ),
    natsUrl: process.env.NATS_URL || "nats://127.0.0.1:4222",
    redisUrl: process.env.REDIS_URL || "redis://127.0.0.1:6379",
    metricsSubject: process.env.METRICS_SUBJECT || "myserver.metrics.>",
    metricsTtlSeconds: Number.parseInt(
      process.env.METRICS_TTL_SECONDS || "604800",
      10
    ),
    heartbeatTtlSeconds: Number.parseInt(
      process.env.METRICS_HEARTBEAT_TTL_SECONDS || "30",
      10
    )
  };
}
