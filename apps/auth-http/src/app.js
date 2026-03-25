import express from "express";

import { AuthStore } from "./auth-store.js";
import { getConfig } from "./config.js";
import { configureLogger, log } from "./logger.js";
import { createRedisClient } from "./redis-client.js";
import { createRoutes } from "./routes.js";

export async function createApp() {
  const config = getConfig();
  configureLogger(config);
  const redis = await createRedisClient(config);
  const authStore = new AuthStore(config, redis);
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

  app.use(createRoutes(config, authStore));

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

  return { app, config, redis };
}
