export async function createApp() {
  const { register } = await import("node:module");
  const { fileURLToPath, pathToFileURL } = await import("node:url");
  process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../tsconfig.json", import.meta.url));
  process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
  register("ts-node/esm", pathToFileURL("./"));

  const { createNestApp, closeNestApp } = await import("./nest-app.ts");
  const {
    AUTH_CONFIG,
    AUTH_METRICS,
    AUTH_MYSQL_POOL,
    AUTH_NATS,
    AUTH_REDIS
  } = await import("./tokens.ts");

  const nestApp = await createNestApp();

  return {
    app: nestApp.getHttpAdapter().getInstance(),
    nestApp,
    config: nestApp.get(AUTH_CONFIG),
    redis: nestApp.get(AUTH_REDIS, { strict: false }),
    nats: nestApp.get(AUTH_NATS, { strict: false }),
    mysqlPool: nestApp.get(AUTH_MYSQL_POOL, { strict: false }),
    metrics: nestApp.get(AUTH_METRICS, { strict: false }),
    close: () => closeNestApp(nestApp)
  };
}
