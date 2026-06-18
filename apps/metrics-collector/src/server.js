import Redis from "ioredis";
import { connect, StringCodec } from "nats";
import { pathToFileURL } from "node:url";

import { getConfig } from "./config.js";
import { maybeRegisterService } from "./registry-client.js";

const codec = StringCodec();
const DEFAULT_INSTANCE_ID = "default";

function normalizeMetricFields(metrics) {
  const fields = {};

  for (const [key, value] of Object.entries(metrics || {})) {
    if (value === undefined || value === null) {
      continue;
    }
    fields[key] = String(value);
  }

  return fields;
}

export function normalizeInstanceId(instanceId) {
  if (instanceId === undefined || instanceId === null || String(instanceId).trim() === "") {
    return DEFAULT_INSTANCE_ID;
  }

  return String(instanceId);
}

export function buildMetricsKey(serviceName, instanceId, bucket) {
  return `metrics:${serviceName}:${normalizeInstanceId(instanceId)}:${bucket}`;
}

export function isDirectRun(metaUrl = import.meta.url, argvPath = process.argv[1]) {
  return Boolean(argvPath) && metaUrl === pathToFileURL(argvPath).href;
}

export async function writeMetrics(redis, config, message) {
  const raw = codec.decode(message.data);
  const payload = JSON.parse(raw);

  if (!payload.service || !payload.bucket || !payload.metrics) {
    throw new Error("invalid metrics payload");
  }

  const serviceName = String(payload.service);
  const bucket = Number.parseInt(String(payload.bucket), 10);
  const timestamp = Number.parseInt(
    String(payload.timestamp || Math.floor(Date.now() / 1000)),
    10
  );
  const fields = normalizeMetricFields(payload.metrics);
  const instanceId = normalizeInstanceId(payload.instance_id);
  fields.instance_id = instanceId;

  if (!Number.isFinite(bucket) || Object.keys(fields).length === 0) {
    throw new Error("invalid metrics fields");
  }

  const metricsKey = buildMetricsKey(serviceName, instanceId, bucket);
  const heartbeatKey = `metrics:heartbeat:${serviceName}`;
  const instanceHeartbeatKey = `${heartbeatKey}:${instanceId}`;
  const pipe = redis.pipeline();

  pipe.hset(metricsKey, fields);
  pipe.expire(metricsKey, config.metricsTtlSeconds);
  pipe.set(heartbeatKey, String(timestamp), "EX", config.heartbeatTtlSeconds);
  pipe.set(instanceHeartbeatKey, String(timestamp), "EX", config.heartbeatTtlSeconds);
  await pipe.exec();
}

async function main() {
  const config = getConfig();
  const registryStatus = await maybeRegisterService(null, config);
  if (registryStatus.registered === false) {
    console.log(
      `[metrics-collector] service registry disabled: service=${registryStatus.service}, instance=${registryStatus.instance}, build=${registryStatus.build_version}; ${registryStatus.reason}`
    );
  }

  const redis = new Redis(config.redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 3
  });
  await redis.connect();

  const nats = await connect({
    servers: config.natsUrl,
    name: "metrics-collector"
  });

  nats.closed().then((error) => {
    if (error) {
      console.error("[metrics-collector] nats closed:", error.message);
    }
  });

  const subscription = nats.subscribe(config.metricsSubject);
  console.log(
    `metrics-collector subscribed to ${config.metricsSubject}, writing to Redis`
  );

  let shuttingDown = false;
  const shutdown = async (signal) => {
    if (shuttingDown) return;
    shuttingDown = true;
    console.log(`metrics-collector shutdown: ${signal}`);

    subscription.unsubscribe();
    try {
      await nats.drain();
    } catch {
      nats.close();
    }
    await redis.quit();
    process.exit(0);
  };

  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("SIGINT", () => shutdown("SIGINT"));

  for await (const message of subscription) {
    try {
      await writeMetrics(redis, config, message);
    } catch (error) {
      console.error("[metrics-collector] write failed:", error.message);
    }
  }
}

if (isDirectRun()) {
  main().catch((error) => {
    console.error("[metrics-collector] fatal:", error);
    process.exit(1);
  });
}
