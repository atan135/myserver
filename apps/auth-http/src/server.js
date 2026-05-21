import log4js from "log4js";

import { createApp } from "./app.js";
import { log } from "./logger.js";

async function main() {
  const { app, config, redis, nats, mysqlPool, metrics } = await createApp();

  let shuttingDown = false;

  const shutdown = async (signal) => {
    if (shuttingDown) return;
    shuttingDown = true;

    log("info", "shutdown.start", { signal });

    // 1. Stop accepting new connections, wait for in-flight requests
    try {
      await new Promise((resolve, reject) => {
        httpServer.close((err) => (err ? reject(err) : resolve()));
      });
      log("info", "shutdown.http_server_closed");
    } catch (error) {
      log("error", "shutdown.http_server_close_failed", { error: error.message });
    }

    // 2. Stop metrics reporter
    try {
      await metrics.stop();
    } catch (error) {
      log("error", "shutdown.metrics_stop_failed", { error: error.message });
    }

    // 3. Close NATS connection
    try {
      await nats.close();
      log("info", "shutdown.nats_closed");
    } catch (error) {
      log("error", "shutdown.nats_close_failed", { error: error.message });
    }

    // 4. Close Redis connection
    try {
      await redis.quit();
      log("info", "shutdown.redis_closed");
    } catch (error) {
      log("error", "shutdown.redis_close_failed", { error: error.message });
    }

    // 5. Close MySQL connection pool
    if (mysqlPool) {
      try {
        await mysqlPool.end();
        log("info", "shutdown.mysql_closed");
      } catch (error) {
        log("error", "shutdown.mysql_close_failed", { error: error.message });
      }
    }

    log("info", "shutdown.complete", { signal });
    await new Promise((resolve) => log4js.shutdown(resolve));
    process.exit(0);
  };

  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("SIGINT", () => shutdown("SIGINT"));

  const httpServer = app.listen(config.port, config.host, () => {
    log("info", "http.server_started", {
      host: config.host,
      port: config.port,
      logEnableConsole: config.logEnableConsole,
      logEnableFile: config.logEnableFile,
      logDir: config.logDir,
      mysqlEnabled: config.mysqlEnabled
    });
  });
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
