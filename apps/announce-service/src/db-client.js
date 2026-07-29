import pg from "pg";

const { Pool } = pg;

function createPoolOptions(config) {
  return {
    connectionString: config.databaseUrl,
    max: config.dbPoolSize,
    application_name: config.serviceName || "announce-service"
  };
}

export async function createDbPool(config) {
  if (!config.dbEnabled) {
    return null;
  }

  const pool = new Pool(createPoolOptions(config));
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
