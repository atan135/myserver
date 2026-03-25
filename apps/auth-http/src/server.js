import { createApp } from "./app.js";
import { log } from "./logger.js";

async function main() {
  const { app, config } = await createApp();

  app.listen(config.port, config.host, () => {
    log("info", "http.server_started", {
      host: config.host,
      port: config.port
    });
  });
}

main().catch((error) => {
  log("error", "http.server_start_failed", {
    error: error.message
  });
  process.exitCode = 1;
});
