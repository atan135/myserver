use mysql_async::{Opts, OptsBuilder, Pool, params, prelude::Queryable};
use serde_json::Value;

use crate::config::Config;

#[derive(Clone)]
pub struct MySqlAuditStore {
    pool: Option<Pool>,
}

impl MySqlAuditStore {
    pub async fn new(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        if !config.mysql_enabled {
            return Ok(Self { pool: None });
        }

        let opts = Opts::from_url(&config.mysql_url)?;
        let pool_opts = mysql_async::PoolOpts::default().with_constraints(
            mysql_async::PoolConstraints::new(1, config.mysql_pool_size.max(1)).unwrap(),
        );
        let builder = OptsBuilder::from_opts(opts).pool_opts(Some(pool_opts));
        let pool = Pool::new(builder);
        let mut conn = pool.get_conn().await?;

        conn.query_drop("SELECT 1").await?;
        conn.query_drop(
            r#"CREATE TABLE IF NOT EXISTS game_connection_audit_logs (
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
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci"#,
        )
        .await?;
        conn.query_drop(
            r#"CREATE TABLE IF NOT EXISTS room_event_logs (
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

    pub async fn append_connection_event(
        &self,
        session_id: u64,
        player_id: Option<&str>,
        peer_addr: Option<&str>,
        event_type: &str,
        details: Option<Value>,
    ) {
        let Some(pool) = &self.pool else {
            return;
        };

        let Ok(mut conn) = pool.get_conn().await else {
            return;
        };

        let details_json = details.map(|value| value.to_string());
        let _ = conn
            .exec_drop(
                r#"INSERT INTO game_connection_audit_logs (
                    session_id,
                    player_id,
                    peer_addr,
                    event_type,
                    details_json,
                    created_at
                ) VALUES (:session_id, :player_id, :peer_addr, :event_type, :details_json, CURRENT_TIMESTAMP(3))"#,
                params! {
                    "session_id" => session_id,
                    "player_id" => player_id,
                    "peer_addr" => peer_addr,
                    "event_type" => event_type,
                    "details_json" => details_json,
                },
            )
            .await;
    }

    pub async fn append_room_event(
        &self,
        room_id: &str,
        player_id: Option<&str>,
        owner_player_id: Option<&str>,
        event_type: &str,
        room_state: Option<&str>,
        member_count: usize,
        details: Option<Value>,
    ) {
        let Some(pool) = &self.pool else {
            return;
        };

        let Ok(mut conn) = pool.get_conn().await else {
            return;
        };

        let details_json = details.map(|value| value.to_string());
        let _ = conn
            .exec_drop(
                r#"INSERT INTO room_event_logs (
                    room_id,
                    player_id,
                    owner_player_id,
                    event_type,
                    room_state,
                    member_count,
                    details_json,
                    created_at
                ) VALUES (:room_id, :player_id, :owner_player_id, :event_type, :room_state, :member_count, :details_json, CURRENT_TIMESTAMP(3))"#,
                params! {
                    "room_id" => room_id,
                    "player_id" => player_id,
                    "owner_player_id" => owner_player_id,
                    "event_type" => event_type,
                    "room_state" => room_state,
                    "member_count" => member_count as u32,
                    "details_json" => details_json,
                },
            )
            .await;
    }
}
