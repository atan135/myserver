import { createApp } from "./app.js";
import { log } from "./logger.js";

async function main() {
  let appContext;

  try {
    appContext = await createApp();
  } catch (error) {
    console.error("Failed to start mail-service:", error);
    process.exit(1);
  }

  const { app, config, redis, mysqlPool, registryClient } = appContext;

  // Register service to Redis
  try {
    await registryClient.register();
    registryClient.startHeartbeat(10);
  } catch (error) {
    log("error", "startup.registry_failed", { error: error.message });
  }

  // Register shutdown handlers
  const shutdown = async (signal) => {
    log("info", "shutdown.start", { signal });

    registryClient.stopHeartbeat();

    try {
      await registryClient.deregister();
    } catch (error) {
      log("error", "shutdown.deregister_failed", { error: error.message });
    }

    try {
      await redis.quit();
    } catch (error) {
      log("error", "shutdown.redis_close_failed", { error: error.message });
    }

    try {
      if (mysqlPool) {
        await mysqlPool.end();
      }
    } catch (error) {
      log("error", "shutdown.mysql_close_failed", { error: error.message });
    }

    log("info", "shutdown.complete", { signal });
    process.exit(0);
  };

  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("SIGINT", () => shutdown("SIGINT"));

  // Start HTTP server
  const server = app.listen(config.port, config.host, () => {
    log("info", "server.started", {
      host: config.host,
      port: config.port,
      env: config.env
    });
    console.log(`Mail service listening on ${config.host}:${config.port}`);
  });

  server.on("error", (error) => {
    log("error", "server.error", { error: error.message });
    console.error("Server error:", error);
    process.exit(1);
  });
}

main();
