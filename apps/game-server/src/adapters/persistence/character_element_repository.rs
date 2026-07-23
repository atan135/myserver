use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::business::character_element::application::ports::{
    ApplyCharacterElementChangeInTransaction, CharacterElementRepository,
    CharacterElementRepositoryApplyError, CharacterElementRepositoryReadError,
    CharacterElementsRead, RepositoryFuture,
};
use crate::business::character_element::{
    CharacterElementApplyResult, CharacterElementError, CharacterElements, ElementValues,
};
use crate::config::Config;

#[cfg(test)]
use crate::business::character_element::{CharacterElementChange, CharacterElementChangeSource};

/// PostgreSQL implementation of permanent character element persistence.
///
/// Configuration and SQLx are deliberately contained in this adapter. The
/// business module uses it only through `CharacterElementRepository`.
#[derive(Clone)]
pub struct PgCharacterElementStore {
    pool: Option<PgPool>,
}

impl PgCharacterElementStore {
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

    #[allow(dead_code)]
    pub fn enabled(&self) -> bool {
        self.pool.is_some()
    }

    pub async fn close(&self) {
        if let Some(pool) = &self.pool {
            pool.close().await;
        }
    }

    async fn get_elements(
        &self,
        character_id: &str,
    ) -> Result<CharacterElements, CharacterElementError> {
        let Some(pool) = &self.pool else {
            return Err(CharacterElementError::DbUnavailable);
        };

        let row = sqlx::query_as::<_, CharacterElementsRow>(SELECT_CHARACTER_ELEMENTS_SQL)
            .bind(character_id)
            .fetch_optional(pool)
            .await
            .map_err(map_db_error)?;

        row.map(CharacterElementsRow::into_elements)
            .ok_or(CharacterElementError::CharacterNotFound)
    }

    async fn apply_elements_change(
        &self,
        request: ApplyCharacterElementChangeInTransaction,
    ) -> Result<CharacterElementApplyResult, PgCharacterElementApplyError> {
        let Some(pool) = &self.pool else {
            return Err(PgCharacterElementApplyError::Rejected(
                CharacterElementError::DbUnavailable,
            ));
        };

        let mut tx = pool
            .begin()
            .await
            .map_err(|error| PgCharacterElementApplyError::Failure(map_db_error(error)))?;

        let row = sqlx::query_as::<_, CharacterElementsRow>(LOCK_CHARACTER_ELEMENTS_SQL)
            .bind(request.character_id())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|error| PgCharacterElementApplyError::Failure(map_db_error(error)))?;

        let Some(row) = row else {
            tx.rollback()
                .await
                .map_err(|error| PgCharacterElementApplyError::Failure(map_db_error(error)))?;
            return Err(PgCharacterElementApplyError::Rejected(
                CharacterElementError::CharacterNotFound,
            ));
        };

        let before = row.into_elements();
        let after = match before.apply_change(request.change()) {
            Ok(after) => after,
            Err(error) => {
                tx.rollback().await.map_err(|rollback_error| {
                    PgCharacterElementApplyError::Failure(map_db_error(rollback_error))
                })?;
                return Err(PgCharacterElementApplyError::Rejected(error));
            }
        };

        sqlx::query(UPDATE_CHARACTER_ELEMENTS_SQL)
            .bind(after.affinity.earth)
            .bind(after.affinity.fire)
            .bind(after.affinity.water)
            .bind(after.affinity.wind)
            .bind(after.mastery.earth)
            .bind(after.mastery.fire)
            .bind(after.mastery.water)
            .bind(after.mastery.wind)
            .bind(request.character_id())
            .execute(&mut *tx)
            .await
            .map_err(|error| PgCharacterElementApplyError::Failure(map_db_error(error)))?;

        sqlx::query(INSERT_CHARACTER_ELEMENT_LOG_SQL)
            .bind(request.character_id())
            .bind(&request.source().source_type)
            .bind(request.source().source_id.as_deref())
            .bind(request.source().operator_type.as_deref())
            .bind(request.source().operator_id.as_deref())
            .bind(request.change().affinity.earth)
            .bind(request.change().affinity.fire)
            .bind(request.change().affinity.water)
            .bind(request.change().affinity.wind)
            .bind(request.change().mastery.earth)
            .bind(request.change().mastery.fire)
            .bind(request.change().mastery.water)
            .bind(request.change().mastery.wind)
            .bind(snapshot_json(&before))
            .bind(snapshot_json(&after))
            .bind(request.reason())
            .execute(&mut *tx)
            .await
            .map_err(|error| PgCharacterElementApplyError::Failure(map_db_error(error)))?;

        match tx.commit().await {
            Ok(()) => {}
            Err(error) if error.as_database_error().is_some() => {
                return Err(PgCharacterElementApplyError::Failure(map_db_error(error)));
            }
            Err(_) => {
                // Once COMMIT has been sent, a transport failure cannot prove
                // that PostgreSQL did not commit the transaction.
                return Err(PgCharacterElementApplyError::OutcomeUnknown);
            }
        }

        Ok(CharacterElementApplyResult {
            character_id: request.character_id().to_string(),
            before,
            after,
        })
    }
}

impl CharacterElementRepository for PgCharacterElementStore {
    fn get<'a>(
        &'a self,
        character_id: &'a str,
    ) -> RepositoryFuture<'a, Result<CharacterElementsRead, CharacterElementRepositoryReadError>>
    {
        Box::pin(async move {
            match self.get_elements(character_id).await {
                Ok(elements) => Ok(CharacterElementsRead::Found(elements)),
                Err(CharacterElementError::CharacterNotFound) => Ok(CharacterElementsRead::Missing),
                Err(CharacterElementError::DbUnavailable) => {
                    Err(CharacterElementRepositoryReadError::Unavailable)
                }
                Err(CharacterElementError::DbError { .. }) => {
                    Err(CharacterElementRepositoryReadError::Failure)
                }
                Err(_) => Err(CharacterElementRepositoryReadError::Failure),
            }
        })
    }

    fn apply_change<'a>(
        &'a self,
        request: ApplyCharacterElementChangeInTransaction,
    ) -> RepositoryFuture<
        'a,
        Result<CharacterElementApplyResult, CharacterElementRepositoryApplyError>,
    > {
        Box::pin(async move {
            match self.apply_elements_change(request).await {
                Ok(result) => Ok(result),
                Err(PgCharacterElementApplyError::Rejected(
                    CharacterElementError::DbUnavailable,
                )) => Err(CharacterElementRepositoryApplyError::Unavailable),
                Err(PgCharacterElementApplyError::OutcomeUnknown) => {
                    Err(CharacterElementRepositoryApplyError::OutcomeUnknown)
                }
                Err(PgCharacterElementApplyError::Failure(error)) => {
                    let _ = error.error_code();
                    Err(CharacterElementRepositoryApplyError::Failure)
                }
                Err(PgCharacterElementApplyError::Rejected(error)) => {
                    Err(CharacterElementRepositoryApplyError::Rejected(error))
                }
            }
        })
    }
}

enum PgCharacterElementApplyError {
    Rejected(CharacterElementError),
    Failure(CharacterElementError),
    OutcomeUnknown,
}

const SELECT_CHARACTER_ELEMENTS_SQL: &str = r#"SELECT
    character_id,
    affinity_earth,
    affinity_fire,
    affinity_water,
    affinity_wind,
    mastery_earth,
    mastery_fire,
    mastery_water,
    mastery_wind
FROM characters
WHERE character_id = $1 AND deleted_at IS NULL"#;

const LOCK_CHARACTER_ELEMENTS_SQL: &str = r#"SELECT
    character_id,
    affinity_earth,
    affinity_fire,
    affinity_water,
    affinity_wind,
    mastery_earth,
    mastery_fire,
    mastery_water,
    mastery_wind
FROM characters
WHERE character_id = $1 AND deleted_at IS NULL
FOR UPDATE"#;

const UPDATE_CHARACTER_ELEMENTS_SQL: &str = r#"UPDATE characters
SET
    affinity_earth = $1,
    affinity_fire = $2,
    affinity_water = $3,
    affinity_wind = $4,
    mastery_earth = $5,
    mastery_fire = $6,
    mastery_water = $7,
    mastery_wind = $8
WHERE character_id = $9"#;

const INSERT_CHARACTER_ELEMENT_LOG_SQL: &str = r#"INSERT INTO character_element_logs (
    character_id,
    source_type,
    source_id,
    operator_type,
    operator_id,
    affinity_earth_delta,
    affinity_fire_delta,
    affinity_water_delta,
    affinity_wind_delta,
    mastery_earth_delta,
    mastery_fire_delta,
    mastery_water_delta,
    mastery_wind_delta,
    before_json,
    after_json,
    reason,
    created_at
) VALUES (
    $1, $2, $3, $4, $5,
    $6, $7, $8, $9,
    $10, $11, $12, $13,
    $14, $15, $16,
    current_timestamp
)"#;

#[derive(sqlx::FromRow)]
struct CharacterElementsRow {
    character_id: String,
    affinity_earth: i32,
    affinity_fire: i32,
    affinity_water: i32,
    affinity_wind: i32,
    mastery_earth: i32,
    mastery_fire: i32,
    mastery_water: i32,
    mastery_wind: i32,
}

impl CharacterElementsRow {
    fn into_elements(self) -> CharacterElements {
        CharacterElements {
            character_id: self.character_id,
            affinity: ElementValues::new(
                self.affinity_earth,
                self.affinity_fire,
                self.affinity_water,
                self.affinity_wind,
            ),
            mastery: ElementValues::new(
                self.mastery_earth,
                self.mastery_fire,
                self.mastery_water,
                self.mastery_wind,
            ),
        }
    }
}

fn snapshot_json(elements: &CharacterElements) -> serde_json::Value {
    serde_json::json!({
        "affinity": elements.affinity,
        "mastery": elements.mastery
    })
}

fn map_db_error(error: sqlx::Error) -> CharacterElementError {
    CharacterElementError::DbError {
        message: error.to_string(),
    }
}

#[cfg(test)]
use std::collections::BTreeMap;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct InMemoryCharacterElementRepository {
    store: Arc<tokio::sync::Mutex<MemoryCharacterElementStore>>,
}

#[cfg(test)]
#[derive(Default)]
struct MemoryCharacterElementStore {
    values: BTreeMap<String, CharacterElements>,
    logs: Vec<MemoryCharacterElementLog>,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemoryCharacterElementLog {
    pub character_id: String,
    pub change: CharacterElementChange,
    pub source: CharacterElementChangeSource,
    pub reason: Option<String>,
    pub before: CharacterElements,
    pub after: CharacterElements,
}

#[cfg(test)]
impl InMemoryCharacterElementRepository {
    pub(crate) async fn set_elements(&self, elements: CharacterElements) {
        self.store
            .lock()
            .await
            .values
            .insert(elements.character_id.clone(), elements);
    }

    pub(crate) async fn applied_change_logs(&self) -> Vec<MemoryCharacterElementLog> {
        self.store.lock().await.logs.clone()
    }
}

#[cfg(test)]
impl CharacterElementRepository for InMemoryCharacterElementRepository {
    fn get<'a>(
        &'a self,
        character_id: &'a str,
    ) -> RepositoryFuture<'a, Result<CharacterElementsRead, CharacterElementRepositoryReadError>>
    {
        Box::pin(async move {
            Ok(self
                .store
                .lock()
                .await
                .values
                .get(character_id)
                .cloned()
                .map(CharacterElementsRead::Found)
                .unwrap_or(CharacterElementsRead::Missing))
        })
    }

    fn apply_change<'a>(
        &'a self,
        request: ApplyCharacterElementChangeInTransaction,
    ) -> RepositoryFuture<
        'a,
        Result<CharacterElementApplyResult, CharacterElementRepositoryApplyError>,
    > {
        Box::pin(async move {
            let mut store = self.store.lock().await;
            let before = store.values.get(request.character_id()).cloned().ok_or(
                CharacterElementRepositoryApplyError::Rejected(
                    CharacterElementError::CharacterNotFound,
                ),
            )?;
            let after = before
                .apply_change(request.change())
                .map_err(CharacterElementRepositoryApplyError::Rejected)?;

            store
                .values
                .insert(after.character_id.clone(), after.clone());
            store.logs.push(MemoryCharacterElementLog {
                character_id: request.character_id().to_string(),
                change: request.change(),
                source: request.source().clone(),
                reason: request.reason().map(str::to_string),
                before: before.clone(),
                after: after.clone(),
            });

            Ok(CharacterElementApplyResult {
                character_id: request.character_id().to_string(),
                before,
                after,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_element_row_preserves_persisted_default_values() {
        let elements = CharacterElementsRow {
            character_id: "chr_0000000000001".to_string(),
            affinity_earth: 2500,
            affinity_fire: 2500,
            affinity_water: 2500,
            affinity_wind: 2500,
            mastery_earth: 0,
            mastery_fire: 0,
            mastery_water: 0,
            mastery_wind: 0,
        }
        .into_elements();

        assert_eq!(elements.character_id, "chr_0000000000001");
        assert_eq!(
            elements.affinity,
            ElementValues::new(2500, 2500, 2500, 2500)
        );
        assert_eq!(elements.mastery, ElementValues::zero());
    }

    #[tokio::test]
    async fn disabled_postgres_adapter_reports_repository_unavailable() {
        let repository = PgCharacterElementStore::new_disabled();

        assert_eq!(
            repository.get("chr_0000000000001").await,
            Err(CharacterElementRepositoryReadError::Unavailable)
        );
        assert_eq!(
            repository
                .apply_change(ApplyCharacterElementChangeInTransaction::new(
                    "chr_0000000000001".to_string(),
                    CharacterElementChange::zero(),
                    CharacterElementChangeSource::new("system"),
                    None,
                ))
                .await,
            Err(CharacterElementRepositoryApplyError::Unavailable)
        );
    }
}
