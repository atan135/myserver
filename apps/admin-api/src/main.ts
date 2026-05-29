import { createNestApp, closeNestApp } from "./nest-app.js";
import { ADMIN_CONFIG } from "./tokens.js";

export async function bootstrap() {
  const app = await createNestApp();
  const config = app.get<any>(ADMIN_CONFIG);
  const httpServer = await app.listen(config.port, config.host);

  const shutdown = async (signal: string) => {
    console.log(`Shutdown signal: ${signal}`);

    try {
      if (typeof httpServer.close === "function") {
        await httpServer.close();
      }
    } catch (error) {
      console.error("httpServer.close error:", error);
    }

    await closeNestApp(app);

    console.log("Shutdown complete");
    process.exit(0);
  };

  process.on("SIGTERM", () => shutdown("SIGTERM"));
  process.on("SIGINT", () => shutdown("SIGINT"));

  console.log(`admin-api listening on ${config.host}:${config.port}`);
  return { app, config, httpServer };
}

if (import.meta.url === `file://${process.argv[1]?.replaceAll("\\", "/")}`) {
  bootstrap().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
