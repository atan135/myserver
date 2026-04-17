import mysql from "mysql2/promise";

const ANNOUNCEMENT_SCHEMA_STATEMENTS = [
  `CREATE TABLE IF NOT EXISTS announcements (
    id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    announce_id VARCHAR(64) NOT NULL,
    locale VARCHAR(32) NOT NULL DEFAULT 'default',
    title VARCHAR(256) NOT NULL,
    content TEXT NOT NULL,
    priority INT NOT NULL DEFAULT 0,
    announce_type VARCHAR(32) NOT NULL DEFAULT 'banner',
    target_group VARCHAR(128) NOT NULL DEFAULT 'all',
    start_time DATETIME(3) NOT NULL,
    end_time DATETIME(3) NOT NULL,
    created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    updated_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
    INDEX idx_announcements_locale (locale),
    INDEX idx_announcements_priority (priority),
    INDEX idx_announcements_time (start_time, end_time),
    UNIQUE KEY uk_announcements_announce_id (announce_id)
  ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci`,
  `ALTER TABLE announcements ADD COLUMN IF NOT EXISTS locale VARCHAR(32) NOT NULL DEFAULT 'default' AFTER announce_id`,
  `ALTER TABLE announcements ADD COLUMN IF NOT EXISTS priority INT NOT NULL DEFAULT 0 AFTER content`,
  `ALTER TABLE announcements ADD COLUMN IF NOT EXISTS announce_type VARCHAR(32) NOT NULL DEFAULT 'banner' AFTER priority`,
  `ALTER TABLE announcements ADD COLUMN IF NOT EXISTS target_group VARCHAR(128) NOT NULL DEFAULT 'all' AFTER announce_type`,
  `ALTER TABLE announcements ADD COLUMN IF NOT EXISTS updated_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3) AFTER created_at`
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

    for (const statement of ANNOUNCEMENT_SCHEMA_STATEMENTS) {
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
