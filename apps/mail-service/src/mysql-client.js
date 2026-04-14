import mysql from "mysql2/promise";

const MAIL_SCHEMA_STATEMENTS = [
  `CREATE TABLE IF NOT EXISTS mails (
    id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    mail_id VARCHAR(64) NOT NULL,
    from_player_id VARCHAR(64) NOT NULL,
    to_player_id VARCHAR(64) NOT NULL,
    title VARCHAR(256) NOT NULL,
    content TEXT,
    attachments JSON,
    mail_type VARCHAR(32) NOT NULL DEFAULT 'system',
    status VARCHAR(32) NOT NULL DEFAULT 'unread',
    created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    read_at DATETIME(3) NULL,
    expires_at DATETIME(3) NULL,
    INDEX idx_mails_to_player_id (to_player_id),
    INDEX idx_mails_status (to_player_id, status),
    INDEX idx_mails_created_at (created_at),
    UNIQUE KEY uk_mails_mail_id (mail_id)
  ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci`
];

function createPoolOptions(config) {
  const url = new URL(config.mysqlUrl);

  return {
    host: url.hostname,
    port: url.port ? Number.parseInt(url.port, 10) : 3306,
    user: decodeURIComponent(url.username),
    password: decodeURIComponent(url.password),
    database: url.pathname.replace(/^\//, ""),
    waitForConnections: true,
    connectionLimit: config.mysqlPoolSize,
    maxIdle: config.mysqlPoolSize,
    idleTimeout: 60000,
    queueLimit: 0,
    enableKeepAlive: true,
    keepAliveInitialDelay: 0,
    charset: "utf8mb4"
  };
}

export async function createMySqlPool(config) {
  if (!config.mysqlEnabled) {
    return null;
  }

  const pool = mysql.createPool(createPoolOptions(config));

  const connection = await pool.getConnection();
  try {
    await connection.query("SELECT 1");

    for (const statement of MAIL_SCHEMA_STATEMENTS) {
      await connection.query(statement);
    }
  } catch (error) {
    await pool.end();
    throw error;
  } finally {
    connection.release();
  }

  return pool;
}
