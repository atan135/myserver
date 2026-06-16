export async function createApp() {
  const { register } = await import("node:module");
  const { fileURLToPath, pathToFileURL } = await import("node:url");
  process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../tsconfig.json", import.meta.url));
  process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
  register("ts-node/esm", pathToFileURL("./"));

  const { createNestApp, closeNestApp } = await import("./nest-app.ts");
  const {
    ANNOUNCE_CONFIG,
    ANNOUNCE_DB_POOL,
    ANNOUNCE_METRICS,
    ANNOUNCE_NATS,
    ANNOUNCE_REDIS,
    ANNOUNCE_REGISTRY
  } = await import("./tokens.ts");

  const nestApp = await createNestApp();

  return {
    app: nestApp.getHttpAdapter().getInstance(),
    nestApp,
    config: nestApp.get(ANNOUNCE_CONFIG),
    redis: nestApp.get(ANNOUNCE_REDIS, { strict: false }),
    nats: nestApp.get(ANNOUNCE_NATS, { strict: false }),
    dbPool: nestApp.get(ANNOUNCE_DB_POOL, { strict: false }),
    registryClient: nestApp.get(ANNOUNCE_REGISTRY, { strict: false }),
    metrics: nestApp.get(ANNOUNCE_METRICS, { strict: false }),
    close: () => closeNestApp(nestApp)
  };
}
