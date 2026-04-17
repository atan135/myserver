import express from "express";

import { getConfig } from "./config.js";
import { configureLogger, log } from "./logger.js";
import { createMetricsCollector } from "./metrics.js";
import { createMySqlPool } from "./mysql-client.js";
import { AnnouncementStore } from "./mysql-store.js";
import { createRedisClient } from "./redis-client.js";
import { RegistryClient } from "./registry-client.js";
import { createRoutes } from "./routes.js";

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

  const announcementStore = new AnnouncementStore(mysqlPool);
  const registryClient = new RegistryClient(redis, config);
  const metrics = createMetricsCollector(redis, "announce-service");

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

  app.use(metrics.middleware());
  app.use(createRoutes(config, announcementStore));

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

  return { app, config, redis, mysqlPool, registryClient, metrics };
}
