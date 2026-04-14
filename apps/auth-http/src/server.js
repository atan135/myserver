import { createApp } from "./app.js";
import { log } from "./logger.js";

async function main() {
  const { app, config, metrics } = await createApp();

  // Register shutdown handler
  const shutdown = async (signal) => {
    log("info", "shutdown.start", { signal });

    try {
      await metrics.stop();
    } catch (error) {
      log("error", "shutdown.metrics_stop_failed", { error: error.message });
    }

    log("info", "shutdown.complete", { signal });
    process.exit(0);
  };

  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("SIGINT", () => shutdown("SIGINT"));

  app.listen(config.port, config.host, () => {
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
