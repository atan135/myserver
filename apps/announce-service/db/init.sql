-- Announce Service Database Schema
-- Database: myserver_announce

CREATE DATABASE IF NOT EXISTS myserver_announce DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
USE myserver_announce;

CREATE TABLE IF NOT EXISTS announcements (
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
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;

INSERT INTO announcements (
  announce_id,
  locale,
  title,
  content,
  priority,
  announce_type,
  target_group,
  start_time,
  end_time
)
VALUES
  (
    'announce_test_001',
    'default',
    'Welcome to MyServer',
    'This is the default announcement for all players.',
    10,
    'banner',
    'all',
    DATE_SUB(NOW(3), INTERVAL 1 HOUR),
    DATE_ADD(NOW(3), INTERVAL 7 DAY)
  ),
  (
    'announce_test_002',
    'zh-CN',
    '版本维护通知',
    '今晚 20:00 开始进行版本维护，请提前下线。',
    20,
    'popup',
    'all',
    DATE_SUB(NOW(3), INTERVAL 30 MINUTE),
    DATE_ADD(NOW(3), INTERVAL 1 DAY)
  );
