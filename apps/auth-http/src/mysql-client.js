import mysql from "mysql2/promise";

const AUTH_SCHEMA_STATEMENTS = [
  `CREATE TABLE IF NOT EXISTS player_accounts (
    id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    player_id VARCHAR(64) NOT NULL,
    guest_id VARCHAR(128) NULL,
    login_name VARCHAR(64) NULL,
    display_name VARCHAR(64) NULL,
    account_type VARCHAR(32) NOT NULL DEFAULT 'guest',
    status VARCHAR(32) NOT NULL DEFAULT 'active',
    password_algo VARCHAR(32) NULL,
    password_salt VARCHAR(128) NULL,
    password_hash CHAR(128) NULL,
    created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    last_login_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    UNIQUE KEY uk_player_accounts_player_id (player_id),
    UNIQUE KEY uk_player_accounts_guest_id (guest_id),
    UNIQUE KEY uk_player_accounts_login_name (login_name),
    KEY idx_player_accounts_last_login_at (last_login_at)
  ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci`,
  `ALTER TABLE player_accounts ADD COLUMN IF NOT EXISTS login_name VARCHAR(64) NULL AFTER guest_id`,
  `ALTER TABLE player_accounts ADD COLUMN IF NOT EXISTS display_name VARCHAR(64) NULL AFTER login_name`,
  `ALTER TABLE player_accounts ADD COLUMN IF NOT EXISTS password_algo VARCHAR(32) NULL AFTER status`,
  `ALTER TABLE player_accounts ADD COLUMN IF NOT EXISTS password_salt VARCHAR(128) NULL AFTER password_algo`,
  `ALTER TABLE player_accounts ADD COLUMN IF NOT EXISTS password_hash CHAR(128) NULL AFTER password_salt`,
  `CREATE TABLE IF NOT EXISTS auth_audit_logs (
    id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
    player_id VARCHAR(64) NULL,
    guest_id VARCHAR(128) NULL,
    event_type VARCHAR(32) NOT NULL,
    access_token_hash CHAR(64) NULL,
    ticket_hash CHAR(64) NULL,
    client_ip VARCHAR(64) NULL,
    details_json JSON NULL,
    created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
    KEY idx_auth_audit_logs_player_id (player_id),
    KEY idx_auth_audit_logs_guest_id (guest_id),
    KEY idx_auth_audit_logs_event_type (event_type),
    KEY idx_auth_audit_logs_created_at (created_at)
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

    for (const statement of AUTH_SCHEMA_STATEMENTS) {
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
