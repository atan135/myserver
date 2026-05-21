import crypto from "node:crypto";

import express from "express";

import { AuthStore } from "./auth-store.js";
import { getConfig } from "./config.js";
import { GameAdminClient } from "./game-admin-client.js";
import { configureLogger, log, requestContext } from "./logger.js";
import { createMySqlPool } from "./mysql-client.js";
import { MySqlAuthStore } from "./mysql-store.js";
import { RateLimiter, AccountLockout } from "./rate-limiter.js";
import { createRedisClient } from "./redis-client.js";
import { createRoutes } from "./routes.js";
import { ServiceDiscovery } from "./service-discovery.js";
import { createMetricsCollector } from "./metrics.js";
import { createNatsClient } from "./nats-client.js";

export async function createApp() {
  const config = getConfig();
  configureLogger(config);
  const redis = await createRedisClient(config);
  let nats;
  try {
    nats = await createNatsClient(config);
  } catch (error) {
    await redis.quit();
    throw error;
  }
  let mysqlPool = null;

  try {
    mysqlPool = await createMySqlPool(config);
  } catch (error) {
    await nats.close();
    await redis.quit();
    throw error;
  }

  const mysqlStore = new MySqlAuthStore(mysqlPool);
  const authStore = new AuthStore(config, redis, mysqlStore, nats);
  const gameAdminClient = new GameAdminClient(config);
  const rateLimiter = new RateLimiter(redis, config);
  const accountLockout = new AccountLockout(redis, config);
  const serviceDiscovery = new ServiceDiscovery(redis, config);
  const app = express();

  // Create and start metrics collector
  const metrics = createMetricsCollector(
    redis,
    nats,
    "auth-http",
    config.redisKeyPrefix || "",
    config.serviceInstanceId
  );

  app.disable("x-powered-by");
  app.use(express.json({ limit: "64kb" }));

  // Request ID middleware: generate or read X-Request-Id, wrap in AsyncLocalStorage
  app.use((req, res, next) => {
    const requestId = req.headers["x-request-id"] || crypto.randomBytes(8).toString("hex");
    req.requestId = requestId;
    res.setHeader("X-Request-Id", requestId);

    requestContext.run({ requestId }, () => {
      log("info", "http.request", {
        method: req.method,
        path: req.path
      });
      next();
    });
  });

  // Metrics middleware - track QPS and latency
  app.use(metrics.middleware());

  app.use(
    createRoutes(
      config,
      authStore,
      gameAdminClient,
      rateLimiter,
      accountLockout,
      mysqlStore,
      serviceDiscovery
    )
  );

  app.use((req, res) => {
    res.status(404).json({
      ok: false,
      error: "NOT_FOUND",
      path: req.path
    });
  });

  app.use((err, _req, res, _next) => {
    log("error", "http.unhandled_error", {
      error: err.message
    });
    res.status(500).json({
      ok: false,
      error: "INTERNAL_ERROR"
    });
  });

  return { app, config, redis, nats, mysqlPool, metrics };
}
