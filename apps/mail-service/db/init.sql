-- Mail Service Database Schema
-- Database: myserver_mail

CREATE DATABASE IF NOT EXISTS myserver_mail DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
USE myserver_mail;

CREATE TABLE IF NOT EXISTS mails (
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
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

-- Sample data for testing
INSERT INTO mails (mail_id, from_player_id, to_player_id, title, content, mail_type, status)
VALUES
  ('mail_test_001', 'system', 'player_001', 'Welcome to MyServer', 'Welcome to the game!', 'system', 'unread'),
  ('mail_test_002', 'admin', 'player_001', 'GM Notice', 'Server maintenance at 2:00 AM.', 'notice', 'unread');
