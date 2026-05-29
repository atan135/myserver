import log4js from "log4js";

import { createNestApp, closeNestApp } from "./nest-app.js";
import { log } from "./logger.js";
import { ANNOUNCE_CONFIG, ANNOUNCE_REGISTRY } from "./tokens.js";

export async function bootstrap() {
  const app = await createNestApp();
  const config = app.get<any>(ANNOUNCE_CONFIG);
  const registryClient = app.get<any>(ANNOUNCE_REGISTRY, { strict: false });

  try {
    await registryClient.register();
    registryClient.startHeartbeat(10);
  } catch (error: any) {
    log("error", "startup.registry_failed", { error: error.message });
  }

  const httpServer = await app.listen(config.port, config.host);
  let shuttingDown = false;

  const shutdown = async (signal: string) => {
    if (shuttingDown) return;
    shuttingDown = true;

    log("info", "shutdown.start", { signal });

    try {
      if (typeof httpServer.close === "function") {
        await httpServer.close();
      }
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

  log("info", "server.started", {
    host: config.host,
    port: config.port,
    env: config.env
  });
  console.log(`Announce service listening on ${config.host}:${config.port}`);

  return { app, config, httpServer };
}

if (import.meta.url === `file://${process.argv[1]?.replaceAll("\\", "/")}`) {
  bootstrap().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
