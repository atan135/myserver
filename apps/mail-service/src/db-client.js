import pg from "pg";

import { log } from "./logger.js";

const { Pool } = pg;

function createPoolOptions(config) {
  return {
    connectionString: config.databaseUrl,
    max: config.dbPoolSize,
    application_name: config.serviceName || "mail-service"
  };
}

export function attachPoolErrorHandler(pool, report = (errorCode) => {
  log("error", "database.pool_client_error", { errorCode });
}) {
  const handleError = (error) => {
    const errorCode = typeof error?.code === "string" && /^[A-Z][A-Z0-9_]{0,127}$/.test(error.code)
      ? error.code
      : "DATABASE_POOL_ERROR";
    try {
      report(errorCode);
    } catch {
      // Nest initializes providers before configuring log4js; pool listeners must never throw.
    }
  };
  pool.on("error", handleError);
  pool.on("connect", (client) => client.on("error", handleError));
  return pool;
}

export async function createDbPool(config) {
  if (!config.dbEnabled) {
    return null;
  }

  const pool = attachPoolErrorHandler(new Pool(createPoolOptions(config)));
  let client = null;

  try {
    client = await pool.connect();
    await client.query("SELECT 1");
  } catch (error) {
    client?.release();
    client = null;
    await pool.end();
    throw error;
  } finally {
    client?.release();
  }

  return pool;
}
