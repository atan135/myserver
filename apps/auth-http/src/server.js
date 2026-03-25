import { createApp } from "./app.js";
import { log } from "./logger.js";

async function main() {
  const { app, config } = await createApp();

  app.listen(config.port, config.host, () => {
    log("info", "http.server_started", {
      host: config.host,
      port: config.port,
      logEnableConsole: config.logEnableConsole,
      logEnableFile: config.logEnableFile,
      logDir: config.logDir
    });
  });
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
