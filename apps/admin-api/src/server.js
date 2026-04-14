import { createApp } from "./app.js";

const { app, config, pool, redis, metrics } = await createApp();

// Register shutdown handler
const shutdown = async (signal) => {
  console.log(`Shutdown signal: ${signal}`);

  try {
    await metrics.stop();
  } catch (error) {
    console.error("metrics.stop error:", error);
  }

  try {
    await redis.quit();
  } catch (error) {
    console.error("redis.quit error:", error);
  }

  try {
    await pool.end();
  } catch (error) {
    console.error("pool.end error:", error);
  }

  console.log("Shutdown complete");
  process.exit(0);
};

process.on("SIGTERM", () => shutdown("SIGTERM"));
process.on("SIGINT", () => shutdown("SIGINT"));

app.listen(config.port, config.host, () => {
  console.log(`admin-api listening on ${config.host}:${config.port}`);
});
