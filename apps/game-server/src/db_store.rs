use serde_json::Value;
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::Config;

#[derive(Clone)]
pub struct PgAuditStore {
    pool: Option<PgPool>,
}

impl PgAuditStore {
    pub async fn new(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        if !config.db_enabled {
            return Ok(Self { pool: None });
        }

        let pool = PgPoolOptions::new()
            .max_connections(config.db_pool_size.max(1))
            .connect(&config.database_url)
            .await?;

        Ok(Self { pool: Some(pool) })
    }

    pub fn enabled(&self) -> bool {
        self.pool.is_some()
    }

    pub async fn close(&self) {
        if let Some(pool) = &self.pool {
            pool.close().await;
        }
    }

    pub async fn append_connection_event(
        &self,
        session_id: u64,
        player_id: Option<&str>,
        peer_addr: Option<&str>,
        event_type: &str,
        details: Option<Value>,
    ) {
        self.append_connection_event_with_identity(
            session_id, player_id, player_id, None, peer_addr, event_type, details,
        )
        .await;
    }

    pub async fn append_connection_event_with_identity(
        &self,
        session_id: u64,
        player_id: Option<&str>,
        account_player_id: Option<&str>,
        character_id: Option<&str>,
        peer_addr: Option<&str>,
        event_type: &str,
        details: Option<Value>,
    ) {
        let Some(pool) = &self.pool else {
            return;
        };

        let session_id = i64::try_from(session_id).unwrap_or(i64::MAX);
        let _ = sqlx::query(
            r#"INSERT INTO game_connection_audit_logs (
                session_id,
                player_id,
                account_player_id,
                character_id,
                peer_addr,
                event_type,
                details_json,
                created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, current_timestamp)"#,
        )
        .bind(session_id)
        .bind(player_id)
        .bind(account_player_id)
        .bind(character_id)
        .bind(peer_addr)
        .bind(event_type)
        .bind(details)
        .execute(pool)
        .await;
    }

    pub async fn append_room_event(
        &self,
        room_id: &str,
        room_subject_id: Option<&str>,
        owner_character_id: Option<&str>,
        event_type: &str,
        room_state: Option<&str>,
        member_count: usize,
        details: Option<Value>,
    ) {
        self.append_room_event_with_identity(
            room_id,
            room_subject_id,
            None,
            None,
            owner_character_id,
            event_type,
            room_state,
            member_count,
            details,
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn append_room_event_with_identity(
        &self,
        room_id: &str,
        room_subject_id: Option<&str>,
        account_player_id: Option<&str>,
        character_id: Option<&str>,
        owner_character_id: Option<&str>,
        event_type: &str,
        room_state: Option<&str>,
        member_count: usize,
        details: Option<Value>,
    ) {
        let Some(pool) = &self.pool else {
            return;
        };

        let member_count = i32::try_from(member_count).unwrap_or(i32::MAX);
        let _ = sqlx::query(
            r#"INSERT INTO room_event_logs (
                room_id,
                room_subject_id,
                account_player_id,
                character_id,
                owner_character_id,
                event_type,
                room_state,
                member_count,
                details_json,
                created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, current_timestamp)"#,
        )
        .bind(room_id)
        .bind(room_subject_id)
        .bind(account_player_id)
        .bind(character_id)
        .bind(owner_character_id)
        .bind(event_type)
        .bind(room_state)
        .bind(member_count)
        .bind(details)
        .execute(pool)
        .await;
    }

    /// Find the room_id where a character is currently offline.
    /// Returns the most recent room_id where the character had a
    /// 'member_disconnected' event but no subsequent reconnect/leave event.
    pub async fn find_room_by_offline_character(&self, character_id: &str) -> Option<String> {
        let Some(pool) = &self.pool else {
            return None;
        };

        sqlx::query_scalar::<_, String>(
            r#"SELECT room_id FROM room_event_logs
               WHERE (character_id = $1 OR room_subject_id = $1)
               AND event_type = 'member_disconnected'
               AND id > COALESCE(
                   (SELECT MAX(id) FROM room_event_logs
                    WHERE (character_id = $2 OR room_subject_id = $2)
                    AND event_type IN ('player_reconnected', 'room_left', 'room_disbanded')),
                   0
               )
               ORDER BY id DESC LIMIT 1"#,
        )
        .bind(character_id)
        .bind(character_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
    }
}
