import log4js from "log4js";

import { createNestApp, closeNestApp } from "./nest-app.js";
import { log } from "./logger.js";
import { AUTH_CONFIG } from "./tokens.js";

export async function bootstrap() {
  const app = await createNestApp();
  const config = app.get<any>(AUTH_CONFIG);
  const httpServer = await app.listen(config.port, config.host);
  let shuttingDown = false;

  const shutdown = async (signal: string) => {
    if (shuttingDown) return;
    shuttingDown = true;

    log("info", "shutdown.start", { signal });

    try {
      await new Promise<void>((resolve, reject) => {
        httpServer.close((err: Error | undefined) => (err ? reject(err) : resolve()));
      });
      log("info", "shutdown.http_server_closed");
    } catch (error: any) {
      log("error", "shutdown.http_server_close_failed", { error: error.message });
    }

    await closeNestApp(app);
    log("info", "shutdown.complete", { signal });
    await new Promise((resolve) => log4js.shutdown(resolve));
    process.exit(0);
  };

  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("SIGINT", () => shutdown("SIGINT"));

  log("info", "http.server_started", {
    host: config.host,
    port: config.port,
    logEnableConsole: config.logEnableConsole,
    logEnableFile: config.logEnableFile,
    logDir: config.logDir,
    mysqlEnabled: config.mysqlEnabled
  });

  return { app, config, httpServer };
}

if (import.meta.url === `file://${process.argv[1]?.replaceAll("\\", "/")}`) {
  bootstrap().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
