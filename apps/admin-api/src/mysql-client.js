import mysql from "mysql2/promise";

export async function createMySqlPool(config) {
  const pool = mysql.createPool({
    uri: config.mysqlUrl,
    waitForConnections: true,
    connectionLimit: config.mysqlPoolSize || 10,
    maxIdle: config.mysqlPoolSize || 10,
    idleTimeout: 60000,
    enableKeepAlive: true,
    keepAliveInitialDelay: 10000
  });

  await pool.query("SELECT 1");
  await pool.query("ALTER TABLE player_accounts ADD COLUMN IF NOT EXISTS ban_expires_at DATETIME(3) NULL AFTER status");
  return pool;
}
