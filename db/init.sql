CREATE DATABASE IF NOT EXISTS myserver_auth CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE DATABASE IF NOT EXISTS myserver_game CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;

USE myserver_auth;

CREATE TABLE IF NOT EXISTS player_accounts (
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
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS auth_audit_logs (
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
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS security_audit_logs (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  event_type VARCHAR(64) NOT NULL,
  target_type VARCHAR(32) NULL COMMENT 'ip, account, ticket, etc.',
  target_value VARCHAR(256) NULL COMMENT 'ip address, login_name, etc.',
  client_ip VARCHAR(64) NULL,
  severity VARCHAR(16) NOT NULL DEFAULT 'warning' COMMENT 'info, warning, critical',
  details_json JSON NULL,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  KEY idx_security_audit_logs_event_type (event_type),
  KEY idx_security_audit_logs_target (target_type, target_value),
  KEY idx_security_audit_logs_client_ip (client_ip),
  KEY idx_security_audit_logs_severity (severity),
  KEY idx_security_audit_logs_created_at (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS admin_accounts (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  username VARCHAR(64) NOT NULL UNIQUE,
  display_name VARCHAR(64) NULL,
  password_algo VARCHAR(32) NOT NULL DEFAULT 'scrypt',
  password_salt VARCHAR(128) NOT NULL,
  password_hash VARCHAR(256) NOT NULL,
  role VARCHAR(32) NOT NULL DEFAULT 'viewer',
  status VARCHAR(32) NOT NULL DEFAULT 'active',
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  last_login_at DATETIME(3) NULL,
  UNIQUE KEY uk_admin_accounts_username (username),
  KEY idx_admin_accounts_role (role),
  KEY idx_admin_accounts_status (status)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS admin_audit_logs (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  admin_id BIGINT UNSIGNED NULL,
  admin_username VARCHAR(64) NULL,
  action VARCHAR(64) NOT NULL,
  target_type VARCHAR(32) NULL,
  target_value VARCHAR(256) NULL,
  details_json JSON NULL,
  ip VARCHAR(64) NULL,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  KEY idx_admin_audit_logs_admin_id (admin_id),
  KEY idx_admin_audit_logs_action (action),
  KEY idx_admin_audit_logs_target (target_type, target_value),
  KEY idx_admin_audit_logs_created_at (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

USE myserver_game;

CREATE TABLE IF NOT EXISTS game_connection_audit_logs (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  session_id BIGINT UNSIGNED NOT NULL,
  player_id VARCHAR(64) NULL,
  peer_addr VARCHAR(128) NULL,
  event_type VARCHAR(32) NOT NULL,
  details_json JSON NULL,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  KEY idx_game_connection_audit_logs_player_id (player_id),
  KEY idx_game_connection_audit_logs_event_type (event_type),
  KEY idx_game_connection_audit_logs_created_at (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS room_event_logs (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  room_id VARCHAR(64) NOT NULL,
  player_id VARCHAR(64) NULL,
  owner_player_id VARCHAR(64) NULL,
  event_type VARCHAR(32) NOT NULL,
  room_state VARCHAR(32) NULL,
  member_count INT UNSIGNED NOT NULL DEFAULT 0,
  details_json JSON NULL,
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  KEY idx_room_event_logs_room_id (room_id),
  KEY idx_room_event_logs_player_id (player_id),
  KEY idx_room_event_logs_event_type (event_type),
  KEY idx_room_event_logs_created_at (created_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

CREATE TABLE IF NOT EXISTS metrics_archive (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  service_name VARCHAR(64) NOT NULL,
  bucket_time INT UNSIGNED NOT NULL COMMENT '5秒桶时间戳（秒级）',
  qps INT UNSIGNED NOT NULL DEFAULT 0,
  latency_ms INT UNSIGNED NOT NULL DEFAULT 0,
  online_value INT UNSIGNED NOT NULL DEFAULT 0 COMMENT 'online_sessions / online_players / connections 等',
  extra JSON NULL COMMENT '服务特有字段',
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  INDEX idx_metrics_archive_service_time (service_name, bucket_time)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- 玩家背包数据表
CREATE TABLE IF NOT EXISTS player_inventory (
  id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  player_id VARCHAR(64) NOT NULL,
  level INT NOT NULL DEFAULT 1,
  hp BIGINT NOT NULL DEFAULT 0,
  inventory_data JSON NOT NULL COMMENT '背包物品 JSON',
  warehouse_data JSON NOT NULL COMMENT '仓库物品 JSON',
  equipment_data JSON NOT NULL COMMENT '装备栏 JSON',
  attr_base_data JSON NOT NULL COMMENT '基础属性 JSON',
  visual_data JSON NOT NULL COMMENT '外观 JSON',
  buffs_data JSON NOT NULL COMMENT 'Buff列表 JSON',
  updated_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3) ON UPDATE CURRENT_TIMESTAMP(3),
  created_at DATETIME(3) NOT NULL DEFAULT CURRENT_TIMESTAMP(3),
  UNIQUE KEY uk_player_inventory_player_id (player_id),
  KEY idx_player_inventory_updated_at (updated_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
