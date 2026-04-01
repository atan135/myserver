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
