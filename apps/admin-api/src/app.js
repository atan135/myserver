export async function createApp() {
  const { register } = await import("node:module");
  const { fileURLToPath, pathToFileURL } = await import("node:url");
  process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../tsconfig.json", import.meta.url));
  process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
  register("ts-node/esm", pathToFileURL("./"));

  const { createNestApp } = await import("./nest-app.ts");
  const {
    ADMIN_CONFIG,
    ADMIN_DB_POOL,
    ADMIN_METRICS,
    ADMIN_NATS,
    ADMIN_REDIS
  } = await import("./tokens.ts");

  const nestApp = await createNestApp();

  return {
    app: nestApp.getHttpAdapter().getInstance(),
    nestApp,
    config: nestApp.get(ADMIN_CONFIG),
    pool: nestApp.get(ADMIN_DB_POOL, { strict: false }),
    redis: nestApp.get(ADMIN_REDIS, { strict: false }),
    nats: nestApp.get(ADMIN_NATS, { strict: false }),
    metrics: nestApp.get(ADMIN_METRICS, { strict: false })
  };
}
