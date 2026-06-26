#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::Config;
use crate::session::AuthenticatedSessionIdentity;

const VALID_DISCIPLINE_TIERS: &[&str] = &[
    "novice",
    "apprentice",
    "adept",
    "expert",
    "master",
    "grandmaster",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterDiscipline {
    pub character_id: String,
    pub discipline_id: String,
    pub points: i64,
    pub tier: String,
    pub active: bool,
    pub learned_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisciplineUpsert {
    pub discipline_id: String,
    pub points: i64,
    pub tier: String,
    pub active: bool,
}

impl DisciplineUpsert {
    pub fn new(
        discipline_id: impl Into<String>,
        points: i64,
        tier: impl Into<String>,
        active: bool,
    ) -> Self {
        Self {
            discipline_id: discipline_id.into(),
            points,
            tier: tier.into(),
            active,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisciplineError {
    DisciplineNotFound,
    InvalidDisciplineId,
    InvalidPoints,
    InvalidDisciplineTier,
    DbUnavailable,
    DbError { message: String },
}

impl DisciplineError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::DisciplineNotFound => "DISCIPLINE_NOT_FOUND",
            Self::InvalidDisciplineId => "INVALID_DISCIPLINE_ID",
            Self::InvalidPoints => "INVALID_DISCIPLINE_POINTS",
            Self::InvalidDisciplineTier => "INVALID_DISCIPLINE_TIER",
            Self::DbUnavailable => "DISCIPLINE_DB_UNAVAILABLE",
            Self::DbError { .. } => "DISCIPLINE_DB_ERROR",
        }
    }
}

impl std::fmt::Display for DisciplineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DisciplineNotFound => write!(formatter, "discipline not found"),
            Self::InvalidDisciplineId => write!(formatter, "discipline_id must not be empty"),
            Self::InvalidPoints => write!(formatter, "discipline points must be non-negative"),
            Self::InvalidDisciplineTier => write!(formatter, "unknown discipline tier"),
            Self::DbUnavailable => write!(formatter, "discipline database is unavailable"),
            Self::DbError { message } => write!(formatter, "discipline database error: {message}"),
        }
    }
}

impl std::error::Error for DisciplineError {}

#[derive(Clone)]
pub struct DisciplineService {
    store: DisciplineStore,
}

impl DisciplineService {
    pub fn new(store: PgDisciplineStore) -> Self {
        Self {
            store: DisciplineStore::Pg(store),
        }
    }

    pub async fn list_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
    ) -> Result<Vec<CharacterDiscipline>, DisciplineError> {
        self.list(&identity.character_id).await
    }

    pub async fn get_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
        discipline_id: &str,
    ) -> Result<CharacterDiscipline, DisciplineError> {
        self.get(&identity.character_id, discipline_id).await
    }

    pub async fn upsert_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
        upsert: DisciplineUpsert,
    ) -> Result<CharacterDiscipline, DisciplineError> {
        self.upsert(&identity.character_id, upsert).await
    }

    pub async fn list(
        &self,
        character_id: &str,
    ) -> Result<Vec<CharacterDiscipline>, DisciplineError> {
        match &self.store {
            DisciplineStore::Pg(store) => store.list(character_id).await,
            #[cfg(test)]
            DisciplineStore::Memory(store) => store.lock().await.list(character_id),
        }
    }

    pub async fn get(
        &self,
        character_id: &str,
        discipline_id: &str,
    ) -> Result<CharacterDiscipline, DisciplineError> {
        validate_discipline_id(discipline_id)?;
        match &self.store {
            DisciplineStore::Pg(store) => store.get(character_id, discipline_id).await,
            #[cfg(test)]
            DisciplineStore::Memory(store) => store.lock().await.get(character_id, discipline_id),
        }
    }

    pub async fn upsert(
        &self,
        character_id: &str,
        upsert: DisciplineUpsert,
    ) -> Result<CharacterDiscipline, DisciplineError> {
        validate_upsert(&upsert)?;
        match &self.store {
            DisciplineStore::Pg(store) => store.upsert(character_id, upsert).await,
            #[cfg(test)]
            DisciplineStore::Memory(store) => store.lock().await.upsert(character_id, upsert),
        }
    }

    pub async fn close(&self) {
        match &self.store {
            DisciplineStore::Pg(store) => store.close().await,
            #[cfg(test)]
            DisciplineStore::Memory(_) => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn new_in_memory() -> Self {
        Self {
            store: DisciplineStore::Memory(std::sync::Arc::new(tokio::sync::Mutex::new(
                MemoryDisciplineStore::default(),
            ))),
        }
    }
}

#[derive(Clone)]
enum DisciplineStore {
    Pg(PgDisciplineStore),
    #[cfg(test)]
    Memory(std::sync::Arc<tokio::sync::Mutex<MemoryDisciplineStore>>),
}

#[derive(Clone)]
pub struct PgDisciplineStore {
    pool: Option<PgPool>,
}

impl PgDisciplineStore {
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

    pub async fn list(
        &self,
        character_id: &str,
    ) -> Result<Vec<CharacterDiscipline>, DisciplineError> {
        let Some(pool) = &self.pool else {
            return Err(DisciplineError::DbUnavailable);
        };

        sqlx::query_as::<_, CharacterDisciplineRow>(LIST_DISCIPLINES_SQL)
            .bind(character_id)
            .fetch_all(pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(CharacterDisciplineRow::into_discipline)
                    .collect()
            })
            .map_err(map_db_error)
    }

    pub async fn get(
        &self,
        character_id: &str,
        discipline_id: &str,
    ) -> Result<CharacterDiscipline, DisciplineError> {
        let Some(pool) = &self.pool else {
            return Err(DisciplineError::DbUnavailable);
        };

        sqlx::query_as::<_, CharacterDisciplineRow>(GET_DISCIPLINE_SQL)
            .bind(character_id)
            .bind(discipline_id)
            .fetch_optional(pool)
            .await
            .map_err(map_db_error)?
            .map(CharacterDisciplineRow::into_discipline)
            .ok_or(DisciplineError::DisciplineNotFound)
    }

    pub async fn upsert(
        &self,
        character_id: &str,
        upsert: DisciplineUpsert,
    ) -> Result<CharacterDiscipline, DisciplineError> {
        let Some(pool) = &self.pool else {
            return Err(DisciplineError::DbUnavailable);
        };

        sqlx::query_as::<_, CharacterDisciplineRow>(UPSERT_DISCIPLINE_SQL)
            .bind(character_id)
            .bind(&upsert.discipline_id)
            .bind(upsert.points)
            .bind(&upsert.tier)
            .bind(upsert.active)
            .fetch_one(pool)
            .await
            .map(CharacterDisciplineRow::into_discipline)
            .map_err(map_db_error)
    }
}

const LIST_DISCIPLINES_SQL: &str = r#"SELECT
    character_id,
    discipline_id,
    points,
    tier,
    active,
    learned_at::text AS learned_at,
    updated_at::text AS updated_at
FROM character_disciplines
WHERE character_id = $1
ORDER BY active DESC, updated_at DESC, discipline_id ASC"#;

const GET_DISCIPLINE_SQL: &str = r#"SELECT
    character_id,
    discipline_id,
    points,
    tier,
    active,
    learned_at::text AS learned_at,
    updated_at::text AS updated_at
FROM character_disciplines
WHERE character_id = $1 AND discipline_id = $2"#;

const UPSERT_DISCIPLINE_SQL: &str = r#"INSERT INTO character_disciplines (
    character_id,
    discipline_id,
    points,
    tier,
    active,
    learned_at,
    updated_at
) VALUES ($1, $2, $3, $4, $5, current_timestamp, current_timestamp)
ON CONFLICT (character_id, discipline_id)
DO UPDATE SET
    points = EXCLUDED.points,
    tier = EXCLUDED.tier,
    active = EXCLUDED.active,
    updated_at = current_timestamp
RETURNING
    character_id,
    discipline_id,
    points,
    tier,
    active,
    learned_at::text AS learned_at,
    updated_at::text AS updated_at"#;

#[derive(sqlx::FromRow)]
struct CharacterDisciplineRow {
    character_id: String,
    discipline_id: String,
    points: i64,
    tier: String,
    active: bool,
    learned_at: String,
    updated_at: String,
}

impl CharacterDisciplineRow {
    fn into_discipline(self) -> CharacterDiscipline {
        CharacterDiscipline {
            character_id: self.character_id,
            discipline_id: self.discipline_id,
            points: self.points,
            tier: self.tier,
            active: self.active,
            learned_at: self.learned_at,
            updated_at: self.updated_at,
        }
    }
}

fn validate_upsert(upsert: &DisciplineUpsert) -> Result<(), DisciplineError> {
    validate_discipline_id(&upsert.discipline_id)?;
    if upsert.points < 0 {
        return Err(DisciplineError::InvalidPoints);
    }
    if !VALID_DISCIPLINE_TIERS.contains(&upsert.tier.as_str()) {
        return Err(DisciplineError::InvalidDisciplineTier);
    }
    Ok(())
}

fn validate_discipline_id(discipline_id: &str) -> Result<(), DisciplineError> {
    if discipline_id.trim().is_empty() {
        return Err(DisciplineError::InvalidDisciplineId);
    }
    Ok(())
}

fn map_db_error(error: sqlx::Error) -> DisciplineError {
    DisciplineError::DbError {
        message: error.to_string(),
    }
}

#[cfg(test)]
#[derive(Default)]
struct MemoryDisciplineStore {
    values: std::collections::BTreeMap<(String, String), CharacterDiscipline>,
}

#[cfg(test)]
impl MemoryDisciplineStore {
    fn list(&self, character_id: &str) -> Result<Vec<CharacterDiscipline>, DisciplineError> {
        Ok(self
            .values
            .values()
            .filter(|value| value.character_id == character_id)
            .cloned()
            .collect())
    }

    fn get(
        &self,
        character_id: &str,
        discipline_id: &str,
    ) -> Result<CharacterDiscipline, DisciplineError> {
        self.values
            .get(&(character_id.to_string(), discipline_id.to_string()))
            .cloned()
            .ok_or(DisciplineError::DisciplineNotFound)
    }

    fn upsert(
        &mut self,
        character_id: &str,
        upsert: DisciplineUpsert,
    ) -> Result<CharacterDiscipline, DisciplineError> {
        let key = (character_id.to_string(), upsert.discipline_id.clone());
        let value = CharacterDiscipline {
            character_id: character_id.to_string(),
            discipline_id: upsert.discipline_id,
            points: upsert.points,
            tier: upsert.tier,
            active: upsert.active,
            learned_at: "memory-now".to_string(),
            updated_at: "memory-now".to_string(),
        };
        self.values.insert(key, value.clone());
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> AuthenticatedSessionIdentity {
        AuthenticatedSessionIdentity {
            account_player_id: "plr_0000000000001".to_string(),
            character_id: "chr_0000000000001".to_string(),
            world_id: Some(0),
        }
    }

    #[tokio::test]
    async fn upsert_validates_id_points_and_tier() {
        let service = DisciplineService::new_in_memory();
        let identity = identity();

        let empty_id = service
            .upsert_for_identity(&identity, DisciplineUpsert::new(" ", 0, "novice", false))
            .await
            .unwrap_err();
        assert_eq!(empty_id.error_code(), "INVALID_DISCIPLINE_ID");

        let negative_points = service
            .upsert_for_identity(
                &identity,
                DisciplineUpsert::new("forging", -1, "novice", false),
            )
            .await
            .unwrap_err();
        assert_eq!(negative_points.error_code(), "INVALID_DISCIPLINE_POINTS");

        let unknown_tier = service
            .upsert_for_identity(
                &identity,
                DisciplineUpsert::new("forging", 0, "unknown", false),
            )
            .await
            .unwrap_err();
        assert_eq!(unknown_tier.error_code(), "INVALID_DISCIPLINE_TIER");
    }

    #[tokio::test]
    async fn upsert_and_query_are_scoped_to_identity_character() {
        let service = DisciplineService::new_in_memory();
        let identity = identity();

        let saved = service
            .upsert_for_identity(
                &identity,
                DisciplineUpsert::new("forging", 120, "apprentice", true),
            )
            .await
            .expect("valid discipline should save");

        assert_eq!(saved.character_id, identity.character_id);
        assert_eq!(saved.discipline_id, "forging");
        assert_eq!(saved.points, 120);
        assert_eq!(saved.tier, "apprentice");
        assert!(saved.active);

        let listed = service
            .list_for_identity(&identity)
            .await
            .expect("list should work");
        assert_eq!(listed.len(), 1);

        let fetched = service
            .get_for_identity(&identity, "forging")
            .await
            .expect("discipline should be found");
        assert_eq!(fetched, saved);
    }

    #[tokio::test]
    async fn disabled_store_returns_explicit_error() {
        let service = DisciplineService::new(PgDisciplineStore::new_disabled());
        let error = service.list("chr_0000000000001").await.unwrap_err();
        assert_eq!(error.error_code(), "DISCIPLINE_DB_UNAVAILABLE");
    }
}
