import pg from "pg";

const { Pool } = pg;

function createPoolOptions(config) {
  return {
    connectionString: config.databaseUrl,
    max: config.dbPoolSize || 10
  };
}

function createGamePoolOptions(config) {
  return {
    connectionString: config.gameDatabaseUrl,
    max: config.gameDbPoolSize || config.dbPoolSize || 10
  };
}

async function verifyConnection(pool) {
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
}

export async function createDbPool(config) {
  const pool = new Pool(createPoolOptions(config));
  await verifyConnection(pool);
  return pool;
}

export async function createGameDbPool(config) {
  const pool = new Pool(createGamePoolOptions(config));
  await verifyConnection(pool);
  return pool;
}
