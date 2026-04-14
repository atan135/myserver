import express from "express";

import { getConfig } from "./config.js";
import { configureLogger, log } from "./logger.js";
import { createMySqlPool } from "./mysql-client.js";
import { AdminStore } from "./admin-store.js";
import { GameAdminClient } from "./game-admin-client.js";
import { createRedisClient } from "./redis-client.js";
import { createRoutes } from "./routes.js";
import { createMetricsCollector } from "./metrics.js";

export async function createApp() {
  const config = getConfig();
  configureLogger(config);

  const redis = await createRedisClient(config);
  const pool = await createMySqlPool(config);
  const adminStore = new AdminStore(pool);
  const gameAdminClient = new GameAdminClient(config);

  // Ensure initial admin exists
  await adminStore.ensureInitialAdmin(config);

  // Create and start metrics collector
  const metrics = createMetricsCollector(redis, "admin-api", config.redisKeyPrefix || "");

  const app = express();
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

  app.use(createRoutes(config, adminStore, gameAdminClient, redis, pool));

  app.use((req, res) => {
    res.status(404).json({ ok: false, error: "NOT_FOUND" });
  });

  app.use((err, _req, res, _next) => {
    log("error", "http.unhandled_error", { error: err.message });
    res.status(500).json({ ok: false, error: "INTERNAL_ERROR" });
  });

  return { app, config, pool, redis, metrics };
}
