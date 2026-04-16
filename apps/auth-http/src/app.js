import express from "express";

import { AuthStore } from "./auth-store.js";
import { getConfig } from "./config.js";
import { GameAdminClient } from "./game-admin-client.js";
import { configureLogger, log } from "./logger.js";
import { createMySqlPool } from "./mysql-client.js";
import { MySqlAuthStore } from "./mysql-store.js";
import { RateLimiter, AccountLockout } from "./rate-limiter.js";
import { createRedisClient } from "./redis-client.js";
import { createRoutes } from "./routes.js";
import { ServiceDiscovery } from "./service-discovery.js";
import { createMetricsCollector } from "./metrics.js";

export async function createApp() {
  const config = getConfig();
  configureLogger(config);
  const redis = await createRedisClient(config);
  let mysqlPool = null;

  try {
    mysqlPool = await createMySqlPool(config);
  } catch (error) {
    await redis.quit();
    throw error;
  }

  const mysqlStore = new MySqlAuthStore(mysqlPool);
  const authStore = new AuthStore(config, redis, mysqlStore);
  const gameAdminClient = new GameAdminClient(config);
  const rateLimiter = new RateLimiter(redis, config);
  const accountLockout = new AccountLockout(redis, config);
  const serviceDiscovery = new ServiceDiscovery(redis, config);
  const app = express();

  // Create and start metrics collector
  const metrics = createMetricsCollector(redis, "auth-http", config.redisKeyPrefix || "");

  app.disable("x-powered-by");
  app.use(express.json({ limit: "64kb" }));

  app.use((req, _res, next) => {
    log("info", "http.request", {
      method: req.method,
      path: req.path
    });
    next();
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

  return { app, config, redis, mysqlPool, metrics };
}
