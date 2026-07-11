import { createNestApp, closeNestApp } from "./nest-app.js";
import { log } from "./logger.js";
import { closeHttpServer, createShutdownHandler } from "./main-shutdown.js";
import { ADMIN_CONFIG, ADMIN_REGISTRY, MYFORGE_GATEWAY } from "./tokens.js";

export async function bootstrap() {
  const app = await createNestApp();
  const config = app.get<any>(ADMIN_CONFIG);
  const httpServer = await app.listen(config.port, config.host);
  const registryClient = app.get<any>(ADMIN_REGISTRY, { strict: false });
  const myforgeGateway = app.get<any>(MYFORGE_GATEWAY, { strict: false });

  try {
    await registryClient.register();
    registryClient.startHeartbeat(10);
    registryClient.startDiscoveryRefresh?.();
  } catch (error: any) {
    log("error", "startup.registry_failed", { error: error.message });
  }

  const shutdown = createShutdownHandler({
    shutdownGateway: () => myforgeGateway?.shutdown?.(),
    closeHttp: () => closeHttpServer(httpServer),
    closeApplication: () => closeNestApp(app, { skipMyforgeShutdown: true }),
    exit: (code: number) => process.exit(code),
    info: (message: string) => console.log(message),
    error: (message: string, error: unknown) => console.error(message, error)
  });

  process.on("SIGTERM", () => { void shutdown("SIGTERM"); });
  process.on("SIGINT", () => { void shutdown("SIGINT"); });

  console.log(`admin-api listening on ${config.host}:${config.port}`);
  return { app, config, httpServer };
}

if (import.meta.url === `file://${process.argv[1]?.replaceAll("\\", "/")}`) {
  bootstrap().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
