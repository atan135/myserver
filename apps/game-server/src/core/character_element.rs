use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::Config;
use crate::session::AuthenticatedSessionIdentity;

/// Temporary compatibility exports for callers that have not yet migrated to
/// `business::character_element`. Remove this forwarding layer in stage 8.
pub use crate::business::character_element::{
    CharacterElementApplyResult, CharacterElementChange, CharacterElementChangeSource,
    CharacterElementError, CharacterElements, ElementDeltas, ElementValues,
};

#[derive(Clone)]
pub struct CharacterElementService {
    store: CharacterElementStore,
}

impl CharacterElementService {
    pub fn new(store: PgCharacterElementStore) -> Self {
        Self {
            store: CharacterElementStore::Pg(store),
        }
    }

    pub async fn get_elements_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
    ) -> Result<CharacterElements, CharacterElementError> {
        match &self.store {
            CharacterElementStore::Pg(store) => store.get(&identity.character_id).await,
            #[cfg(test)]
            CharacterElementStore::Memory(store) => store.lock().await.get(&identity.character_id),
        }
    }

    pub async fn apply_change(
        &self,
        character_id: &str,
        change: CharacterElementChange,
        source: CharacterElementChangeSource,
        reason: Option<&str>,
    ) -> Result<CharacterElementApplyResult, CharacterElementError> {
        match &self.store {
            CharacterElementStore::Pg(store) => {
                store
                    .apply_change(character_id, change, source, reason)
                    .await
            }
            #[cfg(test)]
            CharacterElementStore::Memory(store) => {
                store
                    .lock()
                    .await
                    .apply_change(character_id, change, source, reason)
            }
        }
    }

    pub async fn close(&self) {
        match &self.store {
            CharacterElementStore::Pg(store) => store.close().await,
            #[cfg(test)]
            CharacterElementStore::Memory(_) => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn new_in_memory() -> Self {
        Self {
            store: CharacterElementStore::Memory(std::sync::Arc::new(tokio::sync::Mutex::new(
                MemoryCharacterElementStore::default(),
            ))),
        }
    }

    #[cfg(test)]
    pub(crate) async fn set_elements(&self, elements: CharacterElements) {
        if let CharacterElementStore::Memory(store) = &self.store {
            store.lock().await.set(elements);
        }
    }

    #[cfg(test)]
    pub(crate) async fn applied_change_logs(&self) -> Vec<MemoryCharacterElementLog> {
        match &self.store {
            CharacterElementStore::Memory(store) => store.lock().await.logs.clone(),
            CharacterElementStore::Pg(_) => Vec::new(),
        }
    }
}

#[derive(Clone)]
enum CharacterElementStore {
    Pg(PgCharacterElementStore),
    #[cfg(test)]
    Memory(std::sync::Arc<tokio::sync::Mutex<MemoryCharacterElementStore>>),
}

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

    pub fn enabled(&self) -> bool {
        self.pool.is_some()
    }

    pub async fn close(&self) {
        if let Some(pool) = &self.pool {
            pool.close().await;
        }
    }

    pub async fn get(
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

    pub async fn apply_change(
        &self,
        character_id: &str,
        change: CharacterElementChange,
        source: CharacterElementChangeSource,
        reason: Option<&str>,
    ) -> Result<CharacterElementApplyResult, CharacterElementError> {
        let Some(pool) = &self.pool else {
            return Err(CharacterElementError::DbUnavailable);
        };

        let mut tx = pool.begin().await.map_err(map_db_error)?;

        let row = sqlx::query_as::<_, CharacterElementsRow>(LOCK_CHARACTER_ELEMENTS_SQL)
            .bind(character_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?;

        let Some(row) = row else {
            tx.rollback().await.map_err(map_db_error)?;
            return Err(CharacterElementError::CharacterNotFound);
        };

        let before = row.into_elements();
        let after = match before.apply_change(change) {
            Ok(after) => after,
            Err(error) => {
                tx.rollback().await.map_err(map_db_error)?;
                return Err(error);
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
            .bind(character_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;

        sqlx::query(INSERT_CHARACTER_ELEMENT_LOG_SQL)
            .bind(character_id)
            .bind(&source.source_type)
            .bind(source.source_id.as_deref())
            .bind(source.operator_type.as_deref())
            .bind(source.operator_id.as_deref())
            .bind(change.affinity.earth)
            .bind(change.affinity.fire)
            .bind(change.affinity.water)
            .bind(change.affinity.wind)
            .bind(change.mastery.earth)
            .bind(change.mastery.fire)
            .bind(change.mastery.water)
            .bind(change.mastery.wind)
            .bind(snapshot_json(&before))
            .bind(snapshot_json(&after))
            .bind(reason)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;

        tx.commit().await.map_err(map_db_error)?;

        Ok(CharacterElementApplyResult {
            character_id: character_id.to_string(),
            before,
            after,
        })
    }
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
#[derive(Default)]
struct MemoryCharacterElementStore {
    values: std::collections::BTreeMap<String, CharacterElements>,
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
impl MemoryCharacterElementStore {
    fn set(&mut self, elements: CharacterElements) {
        self.values.insert(elements.character_id.clone(), elements);
    }

    fn get(&self, character_id: &str) -> Result<CharacterElements, CharacterElementError> {
        self.values
            .get(character_id)
            .cloned()
            .ok_or(CharacterElementError::CharacterNotFound)
    }

    fn apply_change(
        &mut self,
        character_id: &str,
        change: CharacterElementChange,
        source: CharacterElementChangeSource,
        reason: Option<&str>,
    ) -> Result<CharacterElementApplyResult, CharacterElementError> {
        let before = self.get(character_id)?;
        let after = before.apply_change(change)?;
        self.values.insert(character_id.to_string(), after.clone());
        self.logs.push(MemoryCharacterElementLog {
            character_id: character_id.to_string(),
            change,
            source,
            reason: reason.map(str::to_string),
            before: before.clone(),
            after: after.clone(),
        });
        Ok(CharacterElementApplyResult {
            character_id: character_id.to_string(),
            before,
            after,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_elements() -> CharacterElements {
        CharacterElements {
            character_id: "chr_0000000000001".to_string(),
            affinity: ElementValues::new(2500, 2500, 2500, 2500),
            mastery: ElementValues::new(10, 20, 30, 40),
        }
    }

    fn identity(character_id: &str) -> AuthenticatedSessionIdentity {
        AuthenticatedSessionIdentity {
            account_player_id: "plr_0000000000001".to_string(),
            character_id: character_id.to_string(),
            world_id: Some(0),
        }
    }

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
    async fn identity_query_reads_only_the_authenticated_characters_elements() {
        let service = CharacterElementService::new_in_memory();
        service.set_elements(base_elements()).await;
        service
            .set_elements(CharacterElements {
                character_id: "chr_0000000000002".to_string(),
                affinity: ElementValues::new(1000, 2000, 3000, 4000),
                mastery: ElementValues::new(1, 2, 3, 4),
            })
            .await;

        let selected = service
            .get_elements_for_identity(&identity("chr_0000000000002"))
            .await
            .expect("current authenticated character should be queried");
        assert_eq!(selected.character_id, "chr_0000000000002");
        assert_eq!(
            selected.affinity,
            ElementValues::new(1000, 2000, 3000, 4000)
        );

        let error = service
            .get_elements_for_identity(&identity("chr_0000000000003"))
            .await
            .expect_err(
                "unknown authenticated character should not read another characters values",
            );
        assert_eq!(error, CharacterElementError::CharacterNotFound);
    }

    #[tokio::test]
    async fn apply_change_returns_snapshots_and_records_complete_audit_context() {
        let service = CharacterElementService::new_in_memory();
        let before = base_elements();
        service.set_elements(before.clone()).await;
        let change = CharacterElementChange::new(
            ElementDeltas::new(-100, 100, 0, 0),
            ElementDeltas::new(0, 5, -10, 0),
        );
        let source = CharacterElementChangeSource::new("quest")
            .with_source_id("quest_0001")
            .with_operator("player", "plr_0000000000001");

        let result = service
            .apply_change(
                &before.character_id,
                change,
                source.clone(),
                Some("quest reward"),
            )
            .await
            .expect("valid change should be persisted");

        let expected_after = CharacterElements {
            character_id: before.character_id.clone(),
            affinity: ElementValues::new(2400, 2600, 2500, 2500),
            mastery: ElementValues::new(10, 25, 20, 40),
        };
        assert_eq!(result.character_id, before.character_id);
        assert_eq!(result.before, before);
        assert_eq!(result.after, expected_after);

        let logs = service.applied_change_logs().await;
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].character_id, result.character_id);
        assert_eq!(logs[0].change, change);
        assert_eq!(logs[0].source, source);
        assert_eq!(logs[0].reason.as_deref(), Some("quest reward"));
        assert_eq!(logs[0].before, result.before);
        assert_eq!(logs[0].after, result.after);
    }

    #[tokio::test]
    async fn rejected_or_missing_character_change_leaves_memory_state_and_logs_unchanged() {
        let service = CharacterElementService::new_in_memory();
        let before = base_elements();
        service.set_elements(before.clone()).await;

        let invalid_error = service
            .apply_change(
                &before.character_id,
                CharacterElementChange::new(ElementDeltas::new(1, 0, 0, 0), ElementDeltas::zero()),
                CharacterElementChangeSource::new("quest"),
                Some("invalid affinity total"),
            )
            .await
            .expect_err("unbalanced affinity change should fail");
        assert_eq!(invalid_error.error_code(), "INVALID_AFFINITY_TOTAL");
        assert_eq!(
            service
                .get_elements_for_identity(&identity(&before.character_id))
                .await
                .expect("rejected change must not mutate current values"),
            before
        );

        let missing_error = service
            .apply_change(
                "chr_missing",
                CharacterElementChange::zero(),
                CharacterElementChangeSource::new("quest"),
                None,
            )
            .await
            .expect_err("missing character should fail before a log is written");
        assert_eq!(missing_error, CharacterElementError::CharacterNotFound);
        assert!(service.applied_change_logs().await.is_empty());
    }

    #[tokio::test]
    async fn disabled_store_returns_explicit_db_unavailable_error() {
        let service = CharacterElementService::new(PgCharacterElementStore::new_disabled());
        let identity = AuthenticatedSessionIdentity {
            account_player_id: "plr_0000000000001".to_string(),
            character_id: "chr_0000000000001".to_string(),
            world_id: Some(0),
        };

        let get_error = service
            .get_elements_for_identity(&identity)
            .await
            .unwrap_err();
        assert_eq!(get_error.error_code(), "CHARACTER_ELEMENTS_DB_UNAVAILABLE");

        let apply_error = service
            .apply_change(
                &identity.character_id,
                CharacterElementChange::zero(),
                CharacterElementChangeSource::new("system"),
                Some("unit-test"),
            )
            .await
            .unwrap_err();
        assert_eq!(
            apply_error.error_code(),
            "CHARACTER_ELEMENTS_DB_UNAVAILABLE"
        );
    }
}
