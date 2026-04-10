use mysql_async::{Opts, OptsBuilder, Pool, params, prelude::Queryable};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub msg_id: String,
    pub chat_type: i32, // 1=私聊, 2=群聊
    pub sender_id: String,
    pub content: String,
    pub created_at: i64,
    pub target_id: String, // 私聊时为对方用户ID
    pub group_id: String,  // 群聊时为群组ID
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatGroup {
    pub group_id: String,
    pub name: String,
    pub owner_id: String,
    pub created_at: i64,
}

#[derive(Clone)]
pub struct ChatStore {
    pool: Option<Pool>,
}

impl ChatStore {
    pub async fn new(mysql_url: &str, mysql_pool_size: u32) -> Result<Self, Box<dyn std::error::Error>> {
        let opts = Opts::from_url(mysql_url)?;
        let pool_opts = mysql_async::PoolOpts::default().with_constraints(
            mysql_async::PoolConstraints::new(1, mysql_pool_size.max(1) as usize).unwrap(),
        );
        let builder = OptsBuilder::from_opts(opts).pool_opts(Some(pool_opts));
        let pool = Pool::new(builder);
        let mut conn = pool.get_conn().await?;

        conn.query_drop("SELECT 1").await?;

        // 创建聊天消息表
        conn.query_drop(
            r#"CREATE TABLE IF NOT EXISTS chat_messages (
                id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
                msg_id VARCHAR(64) UNIQUE NOT NULL,
                chat_type TINYINT NOT NULL,
                sender_id VARCHAR(64) NOT NULL,
                content TEXT NOT NULL,
                created_at BIGINT NOT NULL,
                target_id VARCHAR(64) NULL,
                group_id VARCHAR(64) NULL,
                INDEX idx_sender (sender_id),
                INDEX idx_target (target_id),
                INDEX idx_group (group_id),
                INDEX idx_created (created_at)
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci"#,
        )
        .await?;

        // 创建群组表
        conn.query_drop(
            r#"CREATE TABLE IF NOT EXISTS chat_groups (
                id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
                group_id VARCHAR(64) UNIQUE NOT NULL,
                name VARCHAR(128) NOT NULL,
                owner_id VARCHAR(64) NOT NULL,
                created_at BIGINT NOT NULL,
                INDEX idx_owner (owner_id)
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci"#,
        )
        .await?;

        // 创建群组成员表
        conn.query_drop(
            r#"CREATE TABLE IF NOT EXISTS chat_group_members (
                id BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
                group_id VARCHAR(64) NOT NULL,
                player_id VARCHAR(64) NOT NULL,
                joined_at BIGINT NOT NULL,
                UNIQUE KEY uk_group_player (group_id, player_id),
                INDEX idx_player (player_id)
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci"#,
        )
        .await?;

        drop(conn);

        Ok(Self { pool: Some(pool) })
    }

    pub fn enabled(&self) -> bool {
        self.pool.is_some()
    }

    pub async fn close(&self) -> Result<(), mysql_async::Error> {
        if let Some(pool) = &self.pool {
            pool.clone().disconnect().await?;
        }
        Ok(())
    }

    // ============================================================
    // 消息存储
    // ============================================================

    pub async fn save_private_message(&self, msg: &ChatMessage) -> Result<(), mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };

        let mut conn = pool.get_conn().await?;
        conn.exec_drop(
            r#"INSERT INTO chat_messages (
                msg_id, chat_type, sender_id, content, created_at, target_id, group_id
            ) VALUES (:msg_id, :chat_type, :sender_id, :content, :created_at, :target_id, :group_id)"#,
            params! {
                "msg_id" => &msg.msg_id,
                "chat_type" => msg.chat_type,
                "sender_id" => &msg.sender_id,
                "content" => &msg.content,
                "created_at" => msg.created_at,
                "target_id" => &msg.target_id,
                "group_id" => &msg.group_id,
            },
        )
        .await?;
        Ok(())
    }

    pub async fn save_group_message(&self, msg: &ChatMessage) -> Result<(), mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };

        let mut conn = pool.get_conn().await?;
        conn.exec_drop(
            r#"INSERT INTO chat_messages (
                msg_id, chat_type, sender_id, content, created_at, target_id, group_id
            ) VALUES (:msg_id, :chat_type, :sender_id, :content, :created_at, :target_id, :group_id)"#,
            params! {
                "msg_id" => &msg.msg_id,
                "chat_type" => msg.chat_type,
                "sender_id" => &msg.sender_id,
                "content" => &msg.content,
                "created_at" => msg.created_at,
                "target_id" => &msg.target_id,
                "group_id" => &msg.group_id,
            },
        )
        .await?;
        Ok(())
    }

    pub async fn get_private_history(
        &self,
        player_a: &str,
        player_b: &str,
        before_time: i64,
        limit: i32,
    ) -> Result<Vec<ChatMessage>, mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(Vec::new());
        };

        let mut conn = pool.get_conn().await?;
        let rows: Vec<(String, i32, String, String, i64, String, String)> = conn.exec(
            r#"SELECT msg_id, chat_type, sender_id, content, created_at, target_id, group_id
               FROM chat_messages
               WHERE chat_type = 1
                 AND ((sender_id = :player_a AND target_id = :player_b) OR (sender_id = :player_b AND target_id = :player_a))
                 AND created_at < :before_time
               ORDER BY created_at DESC
               LIMIT :limit"#,
            params! {
                "player_a" => player_a,
                "player_b" => player_b,
                "before_time" => before_time,
                "limit" => limit,
            },
        )
        .await?;

        Ok(rows
            .into_iter()
            .map(|(msg_id, chat_type, sender_id, content, created_at, target_id, group_id)| ChatMessage {
                msg_id,
                chat_type,
                sender_id,
                content,
                created_at,
                target_id,
                group_id,
            })
            .collect())
    }

    pub async fn get_group_history(
        &self,
        group_id: &str,
        before_time: i64,
        limit: i32,
    ) -> Result<Vec<ChatMessage>, mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(Vec::new());
        };

        let mut conn = pool.get_conn().await?;
        let rows: Vec<(String, i32, String, String, i64, String, String)> = conn.exec(
            r#"SELECT msg_id, chat_type, sender_id, content, created_at, target_id, group_id
               FROM chat_messages
               WHERE chat_type = 2 AND group_id = :group_id AND created_at < :before_time
               ORDER BY created_at DESC
               LIMIT :limit"#,
            params! {
                "group_id" => group_id,
                "before_time" => before_time,
                "limit" => limit,
            },
        )
        .await?;

        Ok(rows
            .into_iter()
            .map(|(msg_id, chat_type, sender_id, content, created_at, target_id, group_id)| ChatMessage {
                msg_id,
                chat_type,
                sender_id,
                content,
                created_at,
                target_id,
                group_id,
            })
            .collect())
    }

    // ============================================================
    // 群组管理
    // ============================================================

    pub async fn create_group(&self, group: &ChatGroup) -> Result<(), mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };

        let mut conn = pool.get_conn().await?;

        conn.exec_drop(
            r#"INSERT INTO chat_groups (group_id, name, owner_id, created_at)
               VALUES (:group_id, :name, :owner_id, :created_at)"#,
            params! {
                "group_id" => &group.group_id,
                "name" => &group.name,
                "owner_id" => &group.owner_id,
                "created_at" => group.created_at,
            },
        )
        .await?;

        conn.exec_drop(
            r#"INSERT INTO chat_group_members (group_id, player_id, joined_at)
               VALUES (:group_id, :player_id, :joined_at)"#,
            params! {
                "group_id" => &group.group_id,
                "player_id" => &group.owner_id,
                "joined_at" => group.created_at,
            },
        )
        .await?;

        Ok(())
    }

    pub async fn get_group(&self, group_id: &str) -> Result<Option<ChatGroup>, mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(None);
        };

        let mut conn = pool.get_conn().await?;
        let row: Option<(String, String, String, i64)> = conn.exec_first(
            r#"SELECT group_id, name, owner_id, created_at FROM chat_groups WHERE group_id = :group_id"#,
            params! {
                "group_id" => group_id,
            },
        )
        .await?;

        Ok(row.map(|(group_id, name, owner_id, created_at)| ChatGroup {
            group_id,
            name,
            owner_id,
            created_at,
        }))
    }

    pub async fn delete_group(&self, group_id: &str) -> Result<(), mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };

        let mut conn = pool.get_conn().await?;

        conn.exec_drop(
            r#"DELETE FROM chat_group_members WHERE group_id = :group_id"#,
            params! {
                "group_id" => group_id,
            },
        )
        .await?;

        conn.exec_drop(
            r#"DELETE FROM chat_groups WHERE group_id = :group_id"#,
            params! {
                "group_id" => group_id,
            },
        )
        .await?;

        Ok(())
    }

    pub async fn add_group_member(&self, group_id: &str, player_id: &str, joined_at: i64) -> Result<(), mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };

        let mut conn = pool.get_conn().await?;
        conn.exec_drop(
            r#"INSERT IGNORE INTO chat_group_members (group_id, player_id, joined_at)
               VALUES (:group_id, :player_id, :joined_at)"#,
            params! {
                "group_id" => group_id,
                "player_id" => player_id,
                "joined_at" => joined_at,
            },
        )
        .await?;
        Ok(())
    }

    pub async fn remove_group_member(&self, group_id: &str, player_id: &str) -> Result<(), mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(());
        };

        let mut conn = pool.get_conn().await?;
        conn.exec_drop(
            r#"DELETE FROM chat_group_members WHERE group_id = :group_id AND player_id = :player_id"#,
            params! {
                "group_id" => group_id,
                "player_id" => player_id,
            },
        )
        .await?;
        Ok(())
    }

    pub async fn get_group_members(&self, group_id: &str) -> Result<Vec<String>, mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(Vec::new());
        };

        let mut conn = pool.get_conn().await?;
        let members: Vec<String> = conn.exec(
            r#"SELECT player_id FROM chat_group_members WHERE group_id = :group_id"#,
            params! {
                "group_id" => group_id,
            },
        )
        .await?;

        Ok(members)
    }

    pub async fn get_player_groups(&self, player_id: &str) -> Result<Vec<ChatGroup>, mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(Vec::new());
        };

        let mut conn = pool.get_conn().await?;
        let rows: Vec<(String, String, String, i64)> = conn.exec(
            r#"SELECT g.group_id, g.name, g.owner_id, g.created_at
               FROM chat_groups g
               INNER JOIN chat_group_members gm ON g.group_id = gm.group_id
               WHERE gm.player_id = :player_id"#,
            params! {
                "player_id" => player_id,
            },
        )
        .await?;

        Ok(rows
            .into_iter()
            .map(|(group_id, name, owner_id, created_at)| ChatGroup {
                group_id,
                name,
                owner_id,
                created_at,
            })
            .collect())
    }

    pub async fn get_group_member_count(&self, group_id: &str) -> Result<i32, mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(0);
        };

        let mut conn = pool.get_conn().await?;
        let count: Option<i32> = conn.exec_first(
            r#"SELECT COUNT(*) FROM chat_group_members WHERE group_id = :group_id"#,
            params! {
                "group_id" => group_id,
            },
        )
        .await?;

        Ok(count.unwrap_or(0))
    }

    pub async fn is_group_owner(&self, group_id: &str, player_id: &str) -> Result<bool, mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(false);
        };

        let mut conn = pool.get_conn().await?;
        let count: Option<i32> = conn.exec_first(
            r#"SELECT COUNT(*) FROM chat_groups WHERE group_id = :group_id AND owner_id = :player_id"#,
            params! {
                "group_id" => group_id,
                "player_id" => player_id,
            },
        )
        .await?;

        Ok(count.unwrap_or(0) > 0)
    }

    pub async fn is_group_member(&self, group_id: &str, player_id: &str) -> Result<bool, mysql_async::Error> {
        let Some(pool) = &self.pool else {
            return Ok(false);
        };

        let mut conn = pool.get_conn().await?;
        let count: Option<i32> = conn.exec_first(
            r#"SELECT COUNT(*) FROM chat_group_members WHERE group_id = :group_id AND player_id = :player_id"#,
            params! {
                "group_id" => group_id,
                "player_id" => player_id,
            },
        )
        .await?;

        Ok(count.unwrap_or(0) > 0)
    }
}
