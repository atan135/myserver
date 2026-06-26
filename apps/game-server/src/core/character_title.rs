#![allow(dead_code)]

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::Config;
use crate::core::config_table::ConfigTableRuntime;
use crate::csv_code::titletable::TitleTable;
use crate::session::AuthenticatedSessionIdentity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterTitle {
    pub character_id: String,
    pub title_id: String,
    pub source_type: String,
    pub source_id: Option<String>,
    pub is_equipped: bool,
    pub unlocked_at: String,
    pub expires_at: Option<String>,
    pub expired: bool,
}

impl CharacterTitle {
    fn snapshot_json(&self) -> Value {
        serde_json::json!({
            "character_id": self.character_id,
            "title_id": self.title_id,
            "source_type": self.source_type,
            "source_id": self.source_id,
            "is_equipped": self.is_equipped,
            "unlocked_at": self.unlocked_at,
            "expires_at": self.expires_at,
            "expired": self.expired,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleOperationContext {
    pub source_type: String,
    pub source_id: Option<String>,
    pub operator_type: Option<String>,
    pub operator_id: Option<String>,
    pub reason: Option<String>,
}

impl TitleOperationContext {
    pub fn new(source_type: impl Into<String>) -> Self {
        Self {
            source_type: source_type.into(),
            source_id: None,
            operator_type: None,
            operator_id: None,
            reason: None,
        }
    }

    pub fn with_source_id(mut self, source_id: impl Into<String>) -> Self {
        self.source_id = Some(source_id.into());
        self
    }

    pub fn with_operator(
        mut self,
        operator_type: impl Into<String>,
        operator_id: impl Into<String>,
    ) -> Self {
        self.operator_type = Some(operator_type.into());
        self.operator_id = Some(operator_id.into());
        self
    }

    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantTitleRequest {
    pub title_id: String,
    pub expires_at: Option<String>,
}

impl GrantTitleRequest {
    pub fn new(title_id: impl Into<String>) -> Self {
        Self {
            title_id: title_id.into(),
            expires_at: None,
        }
    }

    pub fn with_expires_at(mut self, expires_at: impl Into<String>) -> Self {
        self.expires_at = Some(expires_at.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EquipTitleOptions {
    pub allow_hidden: bool,
}

impl EquipTitleOptions {
    pub const fn visible_only() -> Self {
        Self {
            allow_hidden: false,
        }
    }

    pub const fn allow_hidden() -> Self {
        Self { allow_hidden: true }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantTitleStatus {
    Granted,
    AlreadyOwned,
    Renewed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantTitleResult {
    pub status: GrantTitleStatus,
    pub title: CharacterTitle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleError {
    TitleNotFound,
    TitleAlreadyOwned,
    TitleNotOwned,
    TitleExpired,
    TitleConfigNotFound,
    InvalidTitleAction,
    DbUnavailable,
    DbError { message: String },
}

impl TitleError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::TitleNotFound => "TITLE_NOT_FOUND",
            Self::TitleAlreadyOwned => "TITLE_ALREADY_OWNED",
            Self::TitleNotOwned => "TITLE_NOT_OWNED",
            Self::TitleExpired => "TITLE_EXPIRED",
            Self::TitleConfigNotFound => "TITLE_CONFIG_NOT_FOUND",
            Self::InvalidTitleAction => "INVALID_TITLE_ACTION",
            Self::DbUnavailable => "TITLE_DB_UNAVAILABLE",
            Self::DbError { .. } => "TITLE_DB_ERROR",
        }
    }
}

impl std::fmt::Display for TitleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TitleNotFound => write!(formatter, "title not found"),
            Self::TitleAlreadyOwned => write!(formatter, "title already owned"),
            Self::TitleNotOwned => write!(formatter, "title is not owned"),
            Self::TitleExpired => write!(formatter, "title is expired"),
            Self::TitleConfigNotFound => write!(formatter, "title config not found"),
            Self::InvalidTitleAction => write!(formatter, "invalid title action"),
            Self::DbUnavailable => write!(formatter, "title database is unavailable"),
            Self::DbError { message } => write!(formatter, "title database error: {message}"),
        }
    }
}

impl std::error::Error for TitleError {}

#[derive(Clone)]
pub struct TitleService {
    store: TitleStore,
    config_source: TitleConfigSource,
}

impl TitleService {
    pub fn new(store: PgTitleStore, config_tables: ConfigTableRuntime) -> Self {
        Self {
            store: TitleStore::Pg(store),
            config_source: TitleConfigSource::Runtime(config_tables),
        }
    }

    pub async fn list_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
        context: TitleOperationContext,
    ) -> Result<Vec<CharacterTitle>, TitleError> {
        self.list(&identity.character_id, context).await
    }

    pub async fn equipped_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
        context: TitleOperationContext,
    ) -> Result<Option<CharacterTitle>, TitleError> {
        self.equipped(&identity.character_id, context).await
    }

    pub async fn grant_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
        request: GrantTitleRequest,
        context: TitleOperationContext,
    ) -> Result<GrantTitleResult, TitleError> {
        self.grant(&identity.character_id, request, context).await
    }

    pub async fn revoke_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
        title_id: &str,
        context: TitleOperationContext,
    ) -> Result<(), TitleError> {
        self.revoke(&identity.character_id, title_id, context).await
    }

    pub async fn equip_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
        title_id: &str,
        options: EquipTitleOptions,
        context: TitleOperationContext,
    ) -> Result<CharacterTitle, TitleError> {
        self.equip(&identity.character_id, title_id, options, context)
            .await
    }

    pub async fn unequip_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
        context: TitleOperationContext,
    ) -> Result<Option<CharacterTitle>, TitleError> {
        self.unequip(&identity.character_id, context).await
    }

    pub async fn list(
        &self,
        character_id: &str,
        context: TitleOperationContext,
    ) -> Result<Vec<CharacterTitle>, TitleError> {
        validate_context(&context)?;
        self.process_expired(character_id, context.clone()).await?;
        match &self.store {
            TitleStore::Pg(store) => store.list(character_id).await,
            #[cfg(test)]
            TitleStore::Memory(store) => store.lock().await.list(character_id),
        }
    }

    pub async fn equipped(
        &self,
        character_id: &str,
        context: TitleOperationContext,
    ) -> Result<Option<CharacterTitle>, TitleError> {
        validate_context(&context)?;
        self.process_expired(character_id, context.clone()).await?;
        match &self.store {
            TitleStore::Pg(store) => store.equipped(character_id).await,
            #[cfg(test)]
            TitleStore::Memory(store) => store.lock().await.equipped(character_id),
        }
    }

    pub async fn grant(
        &self,
        character_id: &str,
        request: GrantTitleRequest,
        context: TitleOperationContext,
    ) -> Result<GrantTitleResult, TitleError> {
        validate_context(&context)?;
        let title_config = self.resolve_title_config(&request.title_id).await?;
        if title_config.limited != 0 && request.expires_at.is_none() {
            return Err(TitleError::InvalidTitleAction);
        }

        match &self.store {
            TitleStore::Pg(store) => store.grant(character_id, request, context).await,
            #[cfg(test)]
            TitleStore::Memory(store) => store.lock().await.grant(character_id, request, context),
        }
    }

    pub async fn revoke(
        &self,
        character_id: &str,
        title_id: &str,
        context: TitleOperationContext,
    ) -> Result<(), TitleError> {
        validate_context(&context)?;
        self.resolve_title_config(title_id).await?;
        match &self.store {
            TitleStore::Pg(store) => store.revoke(character_id, title_id, context).await,
            #[cfg(test)]
            TitleStore::Memory(store) => store.lock().await.revoke(character_id, title_id, context),
        }
    }

    pub async fn equip(
        &self,
        character_id: &str,
        title_id: &str,
        options: EquipTitleOptions,
        context: TitleOperationContext,
    ) -> Result<CharacterTitle, TitleError> {
        validate_context(&context)?;
        self.process_expired(character_id, context.clone()).await?;
        let title_config = self.resolve_title_config(title_id).await?;
        if title_config.hidden != 0 && !options.allow_hidden {
            return Err(TitleError::InvalidTitleAction);
        }

        match &self.store {
            TitleStore::Pg(store) => store.equip(character_id, title_id, context).await,
            #[cfg(test)]
            TitleStore::Memory(store) => store.lock().await.equip(character_id, title_id, context),
        }
    }

    pub async fn unequip(
        &self,
        character_id: &str,
        context: TitleOperationContext,
    ) -> Result<Option<CharacterTitle>, TitleError> {
        validate_context(&context)?;
        self.process_expired(character_id, context.clone()).await?;
        match &self.store {
            TitleStore::Pg(store) => store.unequip(character_id, context).await,
            #[cfg(test)]
            TitleStore::Memory(store) => store.lock().await.unequip(character_id, context),
        }
    }

    pub async fn process_expired(
        &self,
        character_id: &str,
        context: TitleOperationContext,
    ) -> Result<Vec<CharacterTitle>, TitleError> {
        validate_context(&context)?;
        match &self.store {
            TitleStore::Pg(store) => store.process_expired(character_id, context).await,
            #[cfg(test)]
            TitleStore::Memory(store) => store.lock().await.process_expired(character_id, context),
        }
    }

    pub async fn close(&self) {
        match &self.store {
            TitleStore::Pg(store) => store.close().await,
            #[cfg(test)]
            TitleStore::Memory(_) => {}
        }
    }

    async fn resolve_title_config(
        &self,
        title_id: &str,
    ) -> Result<crate::csv_code::titletable::TitleTableRow, TitleError> {
        let parsed_id = title_id
            .parse::<i32>()
            .map_err(|_| TitleError::TitleConfigNotFound)?;
        let table = self.config_source.title_table().await;
        table
            .get(parsed_id)
            .cloned()
            .ok_or(TitleError::TitleConfigNotFound)
    }

    #[cfg(test)]
    fn new_in_memory(title_table: Arc<TitleTable>) -> Self {
        Self {
            store: TitleStore::Memory(Arc::new(tokio::sync::Mutex::new(
                MemoryTitleStore::default(),
            ))),
            config_source: TitleConfigSource::Static(title_table),
        }
    }

    #[cfg(test)]
    async fn logs(&self) -> Vec<TitleLogEntry> {
        match &self.store {
            TitleStore::Memory(store) => store.lock().await.logs.clone(),
            TitleStore::Pg(_) => Vec::new(),
        }
    }
}

#[derive(Clone)]
enum TitleStore {
    Pg(PgTitleStore),
    #[cfg(test)]
    Memory(Arc<tokio::sync::Mutex<MemoryTitleStore>>),
}

#[derive(Clone)]
enum TitleConfigSource {
    Runtime(ConfigTableRuntime),
    #[cfg(test)]
    Static(Arc<TitleTable>),
}

impl TitleConfigSource {
    async fn title_table(&self) -> Arc<TitleTable> {
        match self {
            Self::Runtime(runtime) => runtime.tables_snapshot().await.titletable.clone(),
            #[cfg(test)]
            Self::Static(table) => table.clone(),
        }
    }
}

#[derive(Clone)]
pub struct PgTitleStore {
    pool: Option<PgPool>,
}

impl PgTitleStore {
    pub fn new_disabled() -> Self {
        Self { pool: None }
    }

    pub async fn new(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        if !config.db_enabled {
            return Ok(Self::new_disabled());
        }

        let pool = PgPoolOptions::new()
            .max_connections(config.db_pool_size.max(1))
            .connect(&config.database_url)
            .await?;

        if let Err(error) = sqlx::query("SELECT 1").execute(&pool).await {
            pool.close().await;
            return Err(Box::new(error));
        }

        Ok(Self { pool: Some(pool) })
    }

    pub async fn close(&self) {
        if let Some(pool) = &self.pool {
            pool.close().await;
        }
    }

    pub async fn list(&self, character_id: &str) -> Result<Vec<CharacterTitle>, TitleError> {
        let Some(pool) = &self.pool else {
            return Err(TitleError::DbUnavailable);
        };

        sqlx::query_as::<_, CharacterTitleRow>(LIST_TITLES_SQL)
            .bind(character_id)
            .fetch_all(pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(CharacterTitleRow::into_title)
                    .collect()
            })
            .map_err(map_db_error)
    }

    pub async fn equipped(&self, character_id: &str) -> Result<Option<CharacterTitle>, TitleError> {
        let Some(pool) = &self.pool else {
            return Err(TitleError::DbUnavailable);
        };

        sqlx::query_as::<_, CharacterTitleRow>(GET_EQUIPPED_TITLE_SQL)
            .bind(character_id)
            .fetch_optional(pool)
            .await
            .map(|row| row.map(CharacterTitleRow::into_title))
            .map_err(map_db_error)
    }

    pub async fn grant(
        &self,
        character_id: &str,
        request: GrantTitleRequest,
        context: TitleOperationContext,
    ) -> Result<GrantTitleResult, TitleError> {
        let Some(pool) = &self.pool else {
            return Err(TitleError::DbUnavailable);
        };

        let mut tx = pool.begin().await.map_err(map_db_error)?;
        let existing = sqlx::query_as::<_, CharacterTitleRow>(LOCK_TITLE_SQL)
            .bind(character_id)
            .bind(&request.title_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?
            .map(CharacterTitleRow::into_title);

        let (status, title, before_json) = if let Some(existing) = existing {
            if !existing.expired {
                let snapshot = existing.snapshot_json();
                insert_log(
                    &mut tx,
                    character_id,
                    &request.title_id,
                    "grant",
                    &context,
                    Some(snapshot.clone()),
                    Some(snapshot),
                )
                .await?;
                (GrantTitleStatus::AlreadyOwned, existing, None)
            } else {
                let before_json = existing.snapshot_json();
                let renewed = sqlx::query_as::<_, CharacterTitleRow>(UPDATE_TITLE_GRANT_SQL)
                    .bind(character_id)
                    .bind(&request.title_id)
                    .bind(&context.source_type)
                    .bind(context.source_id.as_deref())
                    .bind(request.expires_at.as_deref())
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(map_db_error)?
                    .into_title();
                (GrantTitleStatus::Renewed, renewed, Some(before_json))
            }
        } else {
            let inserted = sqlx::query_as::<_, CharacterTitleRow>(INSERT_TITLE_SQL)
                .bind(character_id)
                .bind(&request.title_id)
                .bind(&context.source_type)
                .bind(context.source_id.as_deref())
                .bind(request.expires_at.as_deref())
                .fetch_one(&mut *tx)
                .await
                .map_err(map_db_error)?
                .into_title();
            (GrantTitleStatus::Granted, inserted, None)
        };

        if !matches!(status, GrantTitleStatus::AlreadyOwned) {
            insert_log(
                &mut tx,
                character_id,
                &request.title_id,
                "grant",
                &context,
                before_json,
                Some(title.snapshot_json()),
            )
            .await?;
        }

        tx.commit().await.map_err(map_db_error)?;
        Ok(GrantTitleResult { status, title })
    }

    pub async fn revoke(
        &self,
        character_id: &str,
        title_id: &str,
        context: TitleOperationContext,
    ) -> Result<(), TitleError> {
        let Some(pool) = &self.pool else {
            return Err(TitleError::DbUnavailable);
        };

        let mut tx = pool.begin().await.map_err(map_db_error)?;
        let Some(existing) = sqlx::query_as::<_, CharacterTitleRow>(LOCK_TITLE_SQL)
            .bind(character_id)
            .bind(title_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?
            .map(CharacterTitleRow::into_title)
        else {
            tx.rollback().await.map_err(map_db_error)?;
            return Err(TitleError::TitleNotOwned);
        };

        sqlx::query(DELETE_TITLE_SQL)
            .bind(character_id)
            .bind(title_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        insert_log(
            &mut tx,
            character_id,
            title_id,
            "revoke",
            &context,
            Some(existing.snapshot_json()),
            None,
        )
        .await?;

        tx.commit().await.map_err(map_db_error)
    }

    pub async fn equip(
        &self,
        character_id: &str,
        title_id: &str,
        context: TitleOperationContext,
    ) -> Result<CharacterTitle, TitleError> {
        let Some(pool) = &self.pool else {
            return Err(TitleError::DbUnavailable);
        };

        let mut tx = pool.begin().await.map_err(map_db_error)?;
        let Some(target) = sqlx::query_as::<_, CharacterTitleRow>(LOCK_TITLE_SQL)
            .bind(character_id)
            .bind(title_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?
            .map(CharacterTitleRow::into_title)
        else {
            tx.rollback().await.map_err(map_db_error)?;
            return Err(TitleError::TitleNotOwned);
        };

        if target.expired {
            tx.rollback().await.map_err(map_db_error)?;
            return Err(TitleError::TitleExpired);
        }

        if target.is_equipped {
            let snapshot = target.snapshot_json();
            insert_log(
                &mut tx,
                character_id,
                title_id,
                "equip",
                &context,
                Some(snapshot.clone()),
                Some(snapshot),
            )
            .await?;
            tx.commit().await.map_err(map_db_error)?;
            return Ok(target);
        }

        let equipped_rows = sqlx::query_as::<_, CharacterTitleRow>(LOCK_EQUIPPED_TITLES_SQL)
            .bind(character_id)
            .bind(title_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(map_db_error)?;
        for row in equipped_rows {
            let before = row.into_title();
            let after = sqlx::query_as::<_, CharacterTitleRow>(SET_TITLE_EQUIPPED_SQL)
                .bind(character_id)
                .bind(&before.title_id)
                .bind(false)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_db_error)?
                .into_title();
            insert_log(
                &mut tx,
                character_id,
                &before.title_id,
                "unequip",
                &context,
                Some(before.snapshot_json()),
                Some(after.snapshot_json()),
            )
            .await?;
        }

        let after = sqlx::query_as::<_, CharacterTitleRow>(SET_TITLE_EQUIPPED_SQL)
            .bind(character_id)
            .bind(title_id)
            .bind(true)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?
            .into_title();
        insert_log(
            &mut tx,
            character_id,
            title_id,
            "equip",
            &context,
            Some(target.snapshot_json()),
            Some(after.snapshot_json()),
        )
        .await?;

        tx.commit().await.map_err(map_db_error)?;
        Ok(after)
    }

    pub async fn unequip(
        &self,
        character_id: &str,
        context: TitleOperationContext,
    ) -> Result<Option<CharacterTitle>, TitleError> {
        let Some(pool) = &self.pool else {
            return Err(TitleError::DbUnavailable);
        };

        let mut tx = pool.begin().await.map_err(map_db_error)?;
        let Some(before) = sqlx::query_as::<_, CharacterTitleRow>(LOCK_FIRST_EQUIPPED_TITLE_SQL)
            .bind(character_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?
            .map(CharacterTitleRow::into_title)
        else {
            tx.commit().await.map_err(map_db_error)?;
            return Ok(None);
        };

        let after = sqlx::query_as::<_, CharacterTitleRow>(SET_TITLE_EQUIPPED_SQL)
            .bind(character_id)
            .bind(&before.title_id)
            .bind(false)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?
            .into_title();
        insert_log(
            &mut tx,
            character_id,
            &before.title_id,
            "unequip",
            &context,
            Some(before.snapshot_json()),
            Some(after.snapshot_json()),
        )
        .await?;

        tx.commit().await.map_err(map_db_error)?;
        Ok(Some(after))
    }

    pub async fn process_expired(
        &self,
        character_id: &str,
        context: TitleOperationContext,
    ) -> Result<Vec<CharacterTitle>, TitleError> {
        let Some(pool) = &self.pool else {
            return Err(TitleError::DbUnavailable);
        };

        let mut tx = pool.begin().await.map_err(map_db_error)?;
        let expired_rows = sqlx::query_as::<_, CharacterTitleRow>(LOCK_EXPIRED_EQUIPPED_TITLES_SQL)
            .bind(character_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(map_db_error)?;

        let mut changed = Vec::new();
        for row in expired_rows {
            let before = row.into_title();
            let after = sqlx::query_as::<_, CharacterTitleRow>(SET_TITLE_EQUIPPED_SQL)
                .bind(character_id)
                .bind(&before.title_id)
                .bind(false)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_db_error)?
                .into_title();
            insert_log(
                &mut tx,
                character_id,
                &before.title_id,
                "expire",
                &context,
                Some(before.snapshot_json()),
                Some(after.snapshot_json()),
            )
            .await?;
            changed.push(after);
        }

        tx.commit().await.map_err(map_db_error)?;
        Ok(changed)
    }
}

const LIST_TITLES_SQL: &str = r#"SELECT
    character_id,
    title_id,
    source_type,
    source_id,
    is_equipped,
    unlocked_at::text AS unlocked_at,
    expires_at::text AS expires_at,
    (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired
FROM character_titles
WHERE character_id = $1
ORDER BY is_equipped DESC, unlocked_at DESC, title_id ASC"#;

const GET_EQUIPPED_TITLE_SQL: &str = r#"SELECT
    character_id,
    title_id,
    source_type,
    source_id,
    is_equipped,
    unlocked_at::text AS unlocked_at,
    expires_at::text AS expires_at,
    (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired
FROM character_titles
WHERE character_id = $1
  AND is_equipped = true
  AND (expires_at IS NULL OR expires_at > current_timestamp)
ORDER BY updated_at DESC
LIMIT 1"#;

const LOCK_TITLE_SQL: &str = r#"SELECT
    character_id,
    title_id,
    source_type,
    source_id,
    is_equipped,
    unlocked_at::text AS unlocked_at,
    expires_at::text AS expires_at,
    (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired
FROM character_titles
WHERE character_id = $1 AND title_id = $2
FOR UPDATE"#;

const LOCK_EQUIPPED_TITLES_SQL: &str = r#"SELECT
    character_id,
    title_id,
    source_type,
    source_id,
    is_equipped,
    unlocked_at::text AS unlocked_at,
    expires_at::text AS expires_at,
    (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired
FROM character_titles
WHERE character_id = $1 AND title_id <> $2 AND is_equipped = true
FOR UPDATE"#;

const LOCK_FIRST_EQUIPPED_TITLE_SQL: &str = r#"SELECT
    character_id,
    title_id,
    source_type,
    source_id,
    is_equipped,
    unlocked_at::text AS unlocked_at,
    expires_at::text AS expires_at,
    (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired
FROM character_titles
WHERE character_id = $1 AND is_equipped = true
ORDER BY updated_at DESC
LIMIT 1
FOR UPDATE"#;

const LOCK_EXPIRED_EQUIPPED_TITLES_SQL: &str = r#"SELECT
    character_id,
    title_id,
    source_type,
    source_id,
    is_equipped,
    unlocked_at::text AS unlocked_at,
    expires_at::text AS expires_at,
    (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired
FROM character_titles
WHERE character_id = $1
  AND is_equipped = true
  AND expires_at IS NOT NULL
  AND expires_at <= current_timestamp
FOR UPDATE"#;

const INSERT_TITLE_SQL: &str = r#"INSERT INTO character_titles (
    character_id,
    title_id,
    source_type,
    source_id,
    is_equipped,
    unlocked_at,
    expires_at,
    created_at,
    updated_at
) VALUES ($1, $2, $3, $4, false, current_timestamp, $5::timestamptz, current_timestamp, current_timestamp)
RETURNING
    character_id,
    title_id,
    source_type,
    source_id,
    is_equipped,
    unlocked_at::text AS unlocked_at,
    expires_at::text AS expires_at,
    (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired"#;

const UPDATE_TITLE_GRANT_SQL: &str = r#"UPDATE character_titles
SET
    source_type = $3,
    source_id = $4,
    is_equipped = false,
    unlocked_at = current_timestamp,
    expires_at = $5::timestamptz,
    updated_at = current_timestamp
WHERE character_id = $1 AND title_id = $2
RETURNING
    character_id,
    title_id,
    source_type,
    source_id,
    is_equipped,
    unlocked_at::text AS unlocked_at,
    expires_at::text AS expires_at,
    (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired"#;

const SET_TITLE_EQUIPPED_SQL: &str = r#"UPDATE character_titles
SET is_equipped = $3, updated_at = current_timestamp
WHERE character_id = $1 AND title_id = $2
RETURNING
    character_id,
    title_id,
    source_type,
    source_id,
    is_equipped,
    unlocked_at::text AS unlocked_at,
    expires_at::text AS expires_at,
    (expires_at IS NOT NULL AND expires_at <= current_timestamp) AS expired"#;

const DELETE_TITLE_SQL: &str = r#"DELETE FROM character_titles
WHERE character_id = $1 AND title_id = $2"#;

const INSERT_TITLE_LOG_SQL: &str = r#"INSERT INTO character_title_logs (
    character_id,
    title_id,
    action,
    source_type,
    source_id,
    operator_type,
    operator_id,
    before_json,
    after_json,
    reason,
    created_at
) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, current_timestamp)"#;

#[derive(sqlx::FromRow)]
struct CharacterTitleRow {
    character_id: String,
    title_id: String,
    source_type: String,
    source_id: Option<String>,
    is_equipped: bool,
    unlocked_at: String,
    expires_at: Option<String>,
    expired: bool,
}

impl CharacterTitleRow {
    fn into_title(self) -> CharacterTitle {
        CharacterTitle {
            character_id: self.character_id,
            title_id: self.title_id,
            source_type: self.source_type,
            source_id: self.source_id,
            is_equipped: self.is_equipped,
            unlocked_at: self.unlocked_at,
            expires_at: self.expires_at,
            expired: self.expired,
        }
    }
}

async fn insert_log(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    character_id: &str,
    title_id: &str,
    action: &str,
    context: &TitleOperationContext,
    before_json: Option<Value>,
    after_json: Option<Value>,
) -> Result<(), TitleError> {
    sqlx::query(INSERT_TITLE_LOG_SQL)
        .bind(character_id)
        .bind(title_id)
        .bind(action)
        .bind(Some(context.source_type.as_str()))
        .bind(context.source_id.as_deref())
        .bind(context.operator_type.as_deref())
        .bind(context.operator_id.as_deref())
        .bind(before_json)
        .bind(after_json)
        .bind(context.reason.as_deref())
        .execute(&mut **tx)
        .await
        .map(|_| ())
        .map_err(map_db_error)
}

fn validate_context(context: &TitleOperationContext) -> Result<(), TitleError> {
    if context.source_type.trim().is_empty() {
        return Err(TitleError::InvalidTitleAction);
    }
    Ok(())
}

fn map_db_error(error: sqlx::Error) -> TitleError {
    TitleError::DbError {
        message: error.to_string(),
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct TitleLogEntry {
    character_id: String,
    title_id: String,
    action: String,
    source_type: Option<String>,
    source_id: Option<String>,
    operator_type: Option<String>,
    operator_id: Option<String>,
    before_json: Option<Value>,
    after_json: Option<Value>,
    reason: Option<String>,
}

#[cfg(test)]
#[derive(Default)]
struct MemoryTitleStore {
    values: std::collections::BTreeMap<(String, String), CharacterTitle>,
    logs: Vec<TitleLogEntry>,
}

#[cfg(test)]
impl MemoryTitleStore {
    fn list(&self, character_id: &str) -> Result<Vec<CharacterTitle>, TitleError> {
        Ok(self
            .values
            .values()
            .filter(|value| value.character_id == character_id)
            .cloned()
            .collect())
    }

    fn equipped(&self, character_id: &str) -> Result<Option<CharacterTitle>, TitleError> {
        Ok(self
            .values
            .values()
            .find(|value| value.character_id == character_id && value.is_equipped && !value.expired)
            .cloned())
    }

    fn grant(
        &mut self,
        character_id: &str,
        request: GrantTitleRequest,
        context: TitleOperationContext,
    ) -> Result<GrantTitleResult, TitleError> {
        let key = (character_id.to_string(), request.title_id.clone());
        if let Some(existing) = self.values.get(&key).cloned() {
            if !existing.expired {
                let snapshot = existing.snapshot_json();
                self.push_log(
                    character_id,
                    &request.title_id,
                    "grant",
                    &context,
                    Some(snapshot.clone()),
                    Some(snapshot),
                );
                return Ok(GrantTitleResult {
                    status: GrantTitleStatus::AlreadyOwned,
                    title: existing,
                });
            }
            let before = existing.snapshot_json();
            let renewed = CharacterTitle {
                source_type: context.source_type.clone(),
                source_id: context.source_id.clone(),
                is_equipped: false,
                unlocked_at: "memory-now".to_string(),
                expires_at: request.expires_at.clone(),
                expired: false,
                ..existing
            };
            self.values.insert(key, renewed.clone());
            self.push_log(
                character_id,
                &request.title_id,
                "grant",
                &context,
                Some(before),
                Some(renewed.snapshot_json()),
            );
            return Ok(GrantTitleResult {
                status: GrantTitleStatus::Renewed,
                title: renewed,
            });
        }

        let title = CharacterTitle {
            character_id: character_id.to_string(),
            title_id: request.title_id.clone(),
            source_type: context.source_type.clone(),
            source_id: context.source_id.clone(),
            is_equipped: false,
            unlocked_at: "memory-now".to_string(),
            expires_at: request.expires_at,
            expired: false,
        };
        self.values.insert(key, title.clone());
        self.push_log(
            character_id,
            &title.title_id,
            "grant",
            &context,
            None,
            Some(title.snapshot_json()),
        );
        Ok(GrantTitleResult {
            status: GrantTitleStatus::Granted,
            title,
        })
    }

    fn revoke(
        &mut self,
        character_id: &str,
        title_id: &str,
        context: TitleOperationContext,
    ) -> Result<(), TitleError> {
        let key = (character_id.to_string(), title_id.to_string());
        let Some(existing) = self.values.remove(&key) else {
            return Err(TitleError::TitleNotOwned);
        };
        self.push_log(
            character_id,
            title_id,
            "revoke",
            &context,
            Some(existing.snapshot_json()),
            None,
        );
        Ok(())
    }

    fn equip(
        &mut self,
        character_id: &str,
        title_id: &str,
        context: TitleOperationContext,
    ) -> Result<CharacterTitle, TitleError> {
        let key = (character_id.to_string(), title_id.to_string());
        let Some(target) = self.values.get(&key).cloned() else {
            return Err(TitleError::TitleNotOwned);
        };
        if target.expired {
            return Err(TitleError::TitleExpired);
        }
        if target.is_equipped {
            let snapshot = target.snapshot_json();
            self.push_log(
                character_id,
                title_id,
                "equip",
                &context,
                Some(snapshot.clone()),
                Some(snapshot),
            );
            return Ok(target);
        }

        let equipped_keys = self
            .values
            .iter()
            .filter(|((stored_character_id, stored_title_id), value)| {
                stored_character_id == character_id
                    && stored_title_id != title_id
                    && value.is_equipped
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for equipped_key in equipped_keys {
            let before = self.values.get(&equipped_key).cloned().unwrap();
            let mut after = before.clone();
            after.is_equipped = false;
            self.values.insert(equipped_key, after.clone());
            self.push_log(
                character_id,
                &before.title_id,
                "unequip",
                &context,
                Some(before.snapshot_json()),
                Some(after.snapshot_json()),
            );
        }

        let before = self.values.get(&key).cloned().unwrap();
        let mut after = before.clone();
        after.is_equipped = true;
        self.values.insert(key, after.clone());
        self.push_log(
            character_id,
            title_id,
            "equip",
            &context,
            Some(before.snapshot_json()),
            Some(after.snapshot_json()),
        );
        Ok(after)
    }

    fn unequip(
        &mut self,
        character_id: &str,
        context: TitleOperationContext,
    ) -> Result<Option<CharacterTitle>, TitleError> {
        let equipped_key = self
            .values
            .iter()
            .find(|((stored_character_id, _), value)| {
                stored_character_id == character_id && value.is_equipped
            })
            .map(|(key, _)| key.clone());
        let Some(equipped_key) = equipped_key else {
            return Ok(None);
        };

        let before = self.values.get(&equipped_key).cloned().unwrap();
        let mut after = before.clone();
        after.is_equipped = false;
        self.values.insert(equipped_key, after.clone());
        self.push_log(
            character_id,
            &before.title_id,
            "unequip",
            &context,
            Some(before.snapshot_json()),
            Some(after.snapshot_json()),
        );
        Ok(Some(after))
    }

    fn process_expired(
        &mut self,
        character_id: &str,
        context: TitleOperationContext,
    ) -> Result<Vec<CharacterTitle>, TitleError> {
        let expired_keys = self
            .values
            .iter()
            .filter(|((stored_character_id, _), value)| {
                stored_character_id == character_id && value.is_equipped && value.expired
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let mut changed = Vec::new();
        for key in expired_keys {
            let before = self.values.get(&key).cloned().unwrap();
            let mut after = before.clone();
            after.is_equipped = false;
            self.values.insert(key, after.clone());
            self.push_log(
                character_id,
                &before.title_id,
                "expire",
                &context,
                Some(before.snapshot_json()),
                Some(after.snapshot_json()),
            );
            changed.push(after);
        }
        Ok(changed)
    }

    fn push_log(
        &mut self,
        character_id: &str,
        title_id: &str,
        action: &str,
        context: &TitleOperationContext,
        before_json: Option<Value>,
        after_json: Option<Value>,
    ) {
        self.logs.push(TitleLogEntry {
            character_id: character_id.to_string(),
            title_id: title_id.to_string(),
            action: action.to_string(),
            source_type: Some(context.source_type.clone()),
            source_id: context.source_id.clone(),
            operator_type: context.operator_type.clone(),
            operator_id: context.operator_id.clone(),
            before_json,
            after_json,
            reason: context.reason.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csv_code::titletable::{TitleTable, TitleTableRow};
    use std::collections::HashMap;

    fn title_table() -> Arc<TitleTable> {
        let rows = vec![
            TitleTableRow {
                titleid: 1001,
                hidden: 0,
                limited: 0,
                ..TitleTableRow::default()
            },
            TitleTableRow {
                titleid: 9001,
                hidden: 1,
                limited: 0,
                ..TitleTableRow::default()
            },
            TitleTableRow {
                titleid: 9101,
                hidden: 0,
                limited: 1,
                ..TitleTableRow::default()
            },
        ];
        let by_id = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.titleid, index))
            .collect();
        Arc::new(TitleTable {
            string_pool: HashMap::new(),
            rows,
            by_id,
        })
    }

    fn identity() -> AuthenticatedSessionIdentity {
        AuthenticatedSessionIdentity {
            account_player_id: "plr_0000000000001".to_string(),
            character_id: "chr_0000000000001".to_string(),
            world_id: Some(0),
        }
    }

    fn context() -> TitleOperationContext {
        TitleOperationContext::new("gm")
            .with_source_id("unit-test")
            .with_operator("account", "plr_0000000000001")
            .with_reason("test")
    }

    #[tokio::test]
    async fn grant_is_idempotent_and_logs_duplicate_attempt() {
        let service = TitleService::new_in_memory(title_table());
        let identity = identity();

        let first = service
            .grant_for_identity(&identity, GrantTitleRequest::new("1001"), context())
            .await
            .expect("first grant should work");
        assert_eq!(first.status, GrantTitleStatus::Granted);

        let second = service
            .grant_for_identity(&identity, GrantTitleRequest::new("1001"), context())
            .await
            .expect("duplicate grant should be idempotent");
        assert_eq!(second.status, GrantTitleStatus::AlreadyOwned);

        let titles = service
            .list_for_identity(&identity, context())
            .await
            .expect("list should work");
        assert_eq!(titles.len(), 1);

        let logs = service.logs().await;
        assert_eq!(logs.iter().filter(|log| log.action == "grant").count(), 2);
        assert!(
            logs.iter()
                .all(|log| log.source_type.as_deref() == Some("gm"))
        );
    }

    #[tokio::test]
    async fn equip_rejects_unowned_expired_and_hidden_titles() {
        let service = TitleService::new_in_memory(title_table());
        let identity = identity();

        let unowned = service
            .equip_for_identity(
                &identity,
                "1001",
                EquipTitleOptions::visible_only(),
                context(),
            )
            .await
            .unwrap_err();
        assert_eq!(unowned.error_code(), "TITLE_NOT_OWNED");

        service
            .grant_for_identity(&identity, GrantTitleRequest::new("9001"), context())
            .await
            .unwrap();
        let hidden = service
            .equip_for_identity(
                &identity,
                "9001",
                EquipTitleOptions::visible_only(),
                context(),
            )
            .await
            .unwrap_err();
        assert_eq!(hidden.error_code(), "INVALID_TITLE_ACTION");

        let equipped_hidden = service
            .equip_for_identity(
                &identity,
                "9001",
                EquipTitleOptions::allow_hidden(),
                context(),
            )
            .await
            .expect("explicit allow_hidden should permit equip");
        assert!(equipped_hidden.is_equipped);
    }

    #[tokio::test]
    async fn equip_switches_titles_and_revoke_clears_equipped_state() {
        let service = TitleService::new_in_memory(title_table());
        let identity = identity();

        service
            .grant_for_identity(&identity, GrantTitleRequest::new("1001"), context())
            .await
            .unwrap();
        service
            .grant_for_identity(&identity, GrantTitleRequest::new("9001"), context())
            .await
            .unwrap();
        service
            .equip_for_identity(
                &identity,
                "1001",
                EquipTitleOptions::visible_only(),
                context(),
            )
            .await
            .unwrap();
        service
            .equip_for_identity(
                &identity,
                "9001",
                EquipTitleOptions::allow_hidden(),
                context(),
            )
            .await
            .unwrap();

        let titles = service
            .list_for_identity(&identity, context())
            .await
            .unwrap();
        assert_eq!(
            titles
                .iter()
                .filter(|title| title.is_equipped)
                .map(|title| title.title_id.as_str())
                .collect::<Vec<_>>(),
            vec!["9001"]
        );

        service
            .revoke_for_identity(&identity, "9001", context())
            .await
            .unwrap();
        assert!(
            service
                .equipped_for_identity(&identity, context())
                .await
                .unwrap()
                .is_none()
        );

        let logs = service.logs().await;
        assert!(logs.iter().any(|log| log.action == "unequip"));
        assert!(logs.iter().any(|log| log.action == "revoke"));
    }

    #[tokio::test]
    async fn expired_equipped_title_is_unequipped_during_query_and_cannot_equip() {
        let service = TitleService::new_in_memory(title_table());
        let identity = identity();

        service
            .grant_for_identity(&identity, GrantTitleRequest::new("1001"), context())
            .await
            .unwrap();
        service
            .equip_for_identity(
                &identity,
                "1001",
                EquipTitleOptions::visible_only(),
                context(),
            )
            .await
            .unwrap();

        if let TitleStore::Memory(store) = &service.store {
            let mut store = store.lock().await;
            let title = store
                .values
                .get_mut(&("chr_0000000000001".to_string(), "1001".to_string()))
                .unwrap();
            title.expired = true;
            title.expires_at = Some("expired".to_string());
        }

        let equipped = service
            .equipped_for_identity(&identity, context())
            .await
            .expect("query should process expiry");
        assert!(equipped.is_none());

        let equip_error = service
            .equip_for_identity(
                &identity,
                "1001",
                EquipTitleOptions::visible_only(),
                context(),
            )
            .await
            .unwrap_err();
        assert_eq!(equip_error.error_code(), "TITLE_EXPIRED");

        let logs = service.logs().await;
        assert!(logs.iter().any(|log| log.action == "expire"));
    }

    #[tokio::test]
    async fn config_errors_are_stable_and_limited_title_requires_expiry() {
        let service = TitleService::new_in_memory(title_table());
        let identity = identity();

        let missing = service
            .grant_for_identity(&identity, GrantTitleRequest::new("404"), context())
            .await
            .unwrap_err();
        assert_eq!(missing.error_code(), "TITLE_CONFIG_NOT_FOUND");

        let limited_without_expiry = service
            .grant_for_identity(&identity, GrantTitleRequest::new("9101"), context())
            .await
            .unwrap_err();
        assert_eq!(limited_without_expiry.error_code(), "INVALID_TITLE_ACTION");

        let limited = service
            .grant_for_identity(
                &identity,
                GrantTitleRequest::new("9101").with_expires_at("2099-01-01T00:00:00Z"),
                context(),
            )
            .await
            .unwrap();
        assert_eq!(limited.status, GrantTitleStatus::Granted);
    }

    #[tokio::test]
    async fn disabled_store_returns_explicit_error() {
        let service = TitleService::new(
            PgTitleStore::new_disabled(),
            ConfigTableRuntime::load(std::path::Path::new("csv")).unwrap_or_else(|_| {
                panic!("test fixture csv should load when constructing disabled service")
            }),
        );
        let error = service
            .list("chr_0000000000001", context())
            .await
            .unwrap_err();
        assert_eq!(error.error_code(), "TITLE_DB_UNAVAILABLE");
    }
}
