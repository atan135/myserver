use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::Config;
use crate::session::AuthenticatedSessionIdentity;

pub const AFFINITY_TOTAL: i32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementValues {
    pub earth: i32,
    pub fire: i32,
    pub water: i32,
    pub wind: i32,
}

impl ElementValues {
    pub const fn new(earth: i32, fire: i32, water: i32, wind: i32) -> Self {
        Self {
            earth,
            fire,
            water,
            wind,
        }
    }

    pub const fn zero() -> Self {
        Self::new(0, 0, 0, 0)
    }

    pub fn total(self) -> i64 {
        i64::from(self.earth) + i64::from(self.fire) + i64::from(self.water) + i64::from(self.wind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementDeltas {
    pub earth: i32,
    pub fire: i32,
    pub water: i32,
    pub wind: i32,
}

impl ElementDeltas {
    pub const fn new(earth: i32, fire: i32, water: i32, wind: i32) -> Self {
        Self {
            earth,
            fire,
            water,
            wind,
        }
    }

    pub const fn zero() -> Self {
        Self::new(0, 0, 0, 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterElements {
    pub character_id: String,
    pub affinity: ElementValues,
    pub mastery: ElementValues,
}

impl CharacterElements {
    pub fn apply_change(
        &self,
        change: CharacterElementChange,
    ) -> Result<Self, CharacterElementError> {
        let after = Self {
            character_id: self.character_id.clone(),
            affinity: apply_deltas("affinity", self.affinity, change.affinity)?,
            mastery: apply_deltas("mastery", self.mastery, change.mastery)?,
        };

        validate_elements(&after)?;
        Ok(after)
    }

    fn snapshot_json(&self) -> serde_json::Value {
        serde_json::json!({
            "affinity": self.affinity,
            "mastery": self.mastery
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterElementChange {
    pub affinity: ElementDeltas,
    pub mastery: ElementDeltas,
}

impl CharacterElementChange {
    pub const fn new(affinity: ElementDeltas, mastery: ElementDeltas) -> Self {
        Self { affinity, mastery }
    }

    pub const fn zero() -> Self {
        Self::new(ElementDeltas::zero(), ElementDeltas::zero())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterElementChangeSource {
    pub source_type: String,
    pub source_id: Option<String>,
    pub operator_type: Option<String>,
    pub operator_id: Option<String>,
}

impl CharacterElementChangeSource {
    pub fn new(source_type: impl Into<String>) -> Self {
        Self {
            source_type: source_type.into(),
            source_id: None,
            operator_type: None,
            operator_id: None,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterElementApplyResult {
    pub character_id: String,
    pub before: CharacterElements,
    pub after: CharacterElements,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharacterElementError {
    CharacterNotFound,
    InvalidAffinityTotal { total: i64 },
    NegativeAffinity { element: &'static str, value: i64 },
    NegativeMastery { element: &'static str, value: i64 },
    ValueOutOfRange { field: &'static str, value: i64 },
    DbUnavailable,
    DbError { message: String },
}

impl CharacterElementError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::CharacterNotFound => "CHARACTER_NOT_FOUND",
            Self::InvalidAffinityTotal { .. } => "INVALID_AFFINITY_TOTAL",
            Self::NegativeAffinity { .. } => "NEGATIVE_AFFINITY",
            Self::NegativeMastery { .. } => "NEGATIVE_MASTERY",
            Self::ValueOutOfRange { .. } => "CHARACTER_ELEMENTS_VALUE_OUT_OF_RANGE",
            Self::DbUnavailable => "CHARACTER_ELEMENTS_DB_UNAVAILABLE",
            Self::DbError { .. } => "CHARACTER_ELEMENTS_DB_ERROR",
        }
    }
}

impl std::fmt::Display for CharacterElementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CharacterNotFound => write!(formatter, "character not found"),
            Self::InvalidAffinityTotal { total } => {
                write!(
                    formatter,
                    "affinity total must be {AFFINITY_TOTAL}, got {total}"
                )
            }
            Self::NegativeAffinity { element, value } => {
                write!(
                    formatter,
                    "affinity {element} must be non-negative, got {value}"
                )
            }
            Self::NegativeMastery { element, value } => {
                write!(
                    formatter,
                    "mastery {element} must be non-negative, got {value}"
                )
            }
            Self::ValueOutOfRange { field, value } => {
                write!(formatter, "{field} value is out of range: {value}")
            }
            Self::DbUnavailable => write!(formatter, "character elements database is unavailable"),
            Self::DbError { message } => {
                write!(formatter, "character elements database error: {message}")
            }
        }
    }
}

impl std::error::Error for CharacterElementError {}

#[derive(Clone)]
pub struct CharacterElementService {
    store: PgCharacterElementStore,
}

impl CharacterElementService {
    pub fn new(store: PgCharacterElementStore) -> Self {
        Self { store }
    }

    pub async fn get_elements_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
    ) -> Result<CharacterElements, CharacterElementError> {
        self.store.get(&identity.character_id).await
    }

    pub async fn apply_change(
        &self,
        character_id: &str,
        change: CharacterElementChange,
        source: CharacterElementChangeSource,
        reason: Option<&str>,
    ) -> Result<CharacterElementApplyResult, CharacterElementError> {
        self.store
            .apply_change(character_id, change, source, reason)
            .await
    }

    pub async fn close(&self) {
        self.store.close().await;
    }
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
            .bind(before.snapshot_json())
            .bind(after.snapshot_json())
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

fn apply_deltas(
    group: &'static str,
    current: ElementValues,
    delta: ElementDeltas,
) -> Result<ElementValues, CharacterElementError> {
    Ok(ElementValues::new(
        checked_add(group, "earth", current.earth, delta.earth)?,
        checked_add(group, "fire", current.fire, delta.fire)?,
        checked_add(group, "water", current.water, delta.water)?,
        checked_add(group, "wind", current.wind, delta.wind)?,
    ))
}

fn checked_add(
    group: &'static str,
    element: &'static str,
    current: i32,
    delta: i32,
) -> Result<i32, CharacterElementError> {
    let value = i64::from(current) + i64::from(delta);
    if value < i64::from(i32::MIN) || value > i64::from(i32::MAX) {
        return Err(CharacterElementError::ValueOutOfRange {
            field: match group {
                "affinity" => match element {
                    "earth" => "affinity_earth",
                    "fire" => "affinity_fire",
                    "water" => "affinity_water",
                    "wind" => "affinity_wind",
                    _ => "affinity",
                },
                "mastery" => match element {
                    "earth" => "mastery_earth",
                    "fire" => "mastery_fire",
                    "water" => "mastery_water",
                    "wind" => "mastery_wind",
                    _ => "mastery",
                },
                _ => "element",
            },
            value,
        });
    }

    Ok(value as i32)
}

fn validate_elements(elements: &CharacterElements) -> Result<(), CharacterElementError> {
    validate_affinity(elements.affinity)?;
    validate_mastery(elements.mastery)
}

fn validate_affinity(affinity: ElementValues) -> Result<(), CharacterElementError> {
    let total = affinity.total();
    if total != i64::from(AFFINITY_TOTAL) {
        return Err(CharacterElementError::InvalidAffinityTotal { total });
    }

    for (element, value) in [
        ("earth", affinity.earth),
        ("fire", affinity.fire),
        ("water", affinity.water),
        ("wind", affinity.wind),
    ] {
        if value < 0 {
            return Err(CharacterElementError::NegativeAffinity {
                element,
                value: i64::from(value),
            });
        }
    }

    Ok(())
}

fn validate_mastery(mastery: ElementValues) -> Result<(), CharacterElementError> {
    for (element, value) in [
        ("earth", mastery.earth),
        ("fire", mastery.fire),
        ("water", mastery.water),
        ("wind", mastery.wind),
    ] {
        if value < 0 {
            return Err(CharacterElementError::NegativeMastery {
                element,
                value: i64::from(value),
            });
        }
    }

    Ok(())
}

fn map_db_error(error: sqlx::Error) -> CharacterElementError {
    CharacterElementError::DbError {
        message: error.to_string(),
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

    #[test]
    fn apply_change_accepts_balanced_affinity_and_non_negative_mastery() {
        let before = base_elements();
        let change = CharacterElementChange::new(
            ElementDeltas::new(-500, 500, 0, 0),
            ElementDeltas::new(5, 0, -10, 20),
        );

        let after = before
            .apply_change(change)
            .expect("valid element change should apply");

        assert_eq!(after.affinity, ElementValues::new(2000, 3000, 2500, 2500));
        assert_eq!(after.affinity.total(), i64::from(AFFINITY_TOTAL));
        assert_eq!(after.mastery, ElementValues::new(15, 20, 20, 60));
    }

    #[test]
    fn apply_change_rejects_invalid_affinity_total() {
        let before = base_elements();
        let change =
            CharacterElementChange::new(ElementDeltas::new(1, 0, 0, 0), ElementDeltas::zero());

        let error = before.apply_change(change).unwrap_err();

        assert_eq!(error.error_code(), "INVALID_AFFINITY_TOTAL");
        assert_eq!(
            error,
            CharacterElementError::InvalidAffinityTotal { total: 10001 }
        );
    }

    #[test]
    fn apply_change_rejects_negative_mastery() {
        let before = base_elements();
        let change =
            CharacterElementChange::new(ElementDeltas::zero(), ElementDeltas::new(0, 0, -31, 0));

        let error = before.apply_change(change).unwrap_err();

        assert_eq!(error.error_code(), "NEGATIVE_MASTERY");
        assert_eq!(
            error,
            CharacterElementError::NegativeMastery {
                element: "water",
                value: -1
            }
        );
    }

    #[test]
    fn apply_change_rejects_negative_affinity_even_when_total_stays_balanced() {
        let before = base_elements();
        let change = CharacterElementChange::new(
            ElementDeltas::new(-3000, 3000, 0, 0),
            ElementDeltas::zero(),
        );

        let error = before.apply_change(change).unwrap_err();

        assert_eq!(error.error_code(), "NEGATIVE_AFFINITY");
        assert_eq!(
            error,
            CharacterElementError::NegativeAffinity {
                element: "earth",
                value: -500
            }
        );
    }

    #[test]
    fn apply_change_rejects_integer_overflow_before_db_update() {
        let before = CharacterElements {
            mastery: ElementValues::new(i32::MAX, 0, 0, 0),
            ..base_elements()
        };
        let change =
            CharacterElementChange::new(ElementDeltas::zero(), ElementDeltas::new(1, 0, 0, 0));

        let error = before.apply_change(change).unwrap_err();

        assert_eq!(error.error_code(), "CHARACTER_ELEMENTS_VALUE_OUT_OF_RANGE");
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
