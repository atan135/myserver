export async function createApp() {
  const { register } = await import("node:module");
  const { fileURLToPath, pathToFileURL } = await import("node:url");
  process.env.TS_NODE_PROJECT ??= fileURLToPath(new URL("../tsconfig.json", import.meta.url));
  process.env.TS_NODE_TRANSPILE_ONLY ??= "true";
  register("ts-node/esm", pathToFileURL("./"));

  const { createNestApp, closeNestApp } = await import("./nest-app.ts");
  const {
    MAIL_CONFIG,
    MAIL_METRICS,
    MAIL_MYSQL_POOL,
    MAIL_NATS,
    MAIL_REDIS,
    MAIL_REGISTRY
  } = await import("./tokens.ts");

  const nestApp = await createNestApp();

  return {
    app: nestApp.getHttpAdapter().getInstance(),
    nestApp,
    config: nestApp.get(MAIL_CONFIG),
    redis: nestApp.get(MAIL_REDIS, { strict: false }),
    nats: nestApp.get(MAIL_NATS, { strict: false }),
    mysqlPool: nestApp.get(MAIL_MYSQL_POOL, { strict: false }),
    registryClient: nestApp.get(MAIL_REGISTRY, { strict: false }),
    metrics: nestApp.get(MAIL_METRICS, { strict: false }),
    close: () => closeNestApp(nestApp)
  };
}
