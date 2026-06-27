#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::postgres::{PgPool, PgPoolOptions};

use crate::config::Config;
use crate::core::character_element::{
    CharacterElementError, CharacterElementService, CharacterElements, ElementValues,
};
use crate::core::character_title::{
    CharacterTitle, TitleError, TitleOperationContext, TitleService,
};
use crate::core::inventory::{ItemError, PlayerData};
use crate::csv_code::disciplinetable::{DisciplineTable, DisciplineTableRow};
use crate::csv_code::itemtable::ItemTable;
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

impl CharacterDiscipline {
    fn snapshot_json(&self) -> Value {
        serde_json::json!({
            "character_id": self.character_id,
            "discipline_id": self.discipline_id,
            "points": self.points,
            "tier": self.tier,
            "active": self.active,
            "learned_at": self.learned_at,
            "updated_at": self.updated_at,
        })
    }
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
pub struct DisciplineOperationContext {
    pub source_type: String,
    pub source_id: Option<String>,
    pub operator_type: Option<String>,
    pub operator_id: Option<String>,
    pub reason: Option<String>,
}

impl DisciplineOperationContext {
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
pub struct LearnDisciplineRequest {
    pub discipline_id: String,
}

impl LearnDisciplineRequest {
    pub fn new(discipline_id: impl Into<String>) -> Self {
        Self {
            discipline_id: discipline_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnDisciplinePlan {
    pub upsert: DisciplineUpsert,
    pub consumed_items: Vec<DisciplineItemCost>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnDisciplineResult {
    pub discipline: CharacterDiscipline,
    pub consumed_items: Vec<DisciplineItemCost>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisciplineItemCost {
    pub item_uid: u64,
    pub item_id: i32,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisciplineDefinitionSummary {
    pub discipline_id: String,
    pub name: String,
    pub description: String,
    pub initial_tier: String,
    pub initial_points: i64,
    pub skill_pool: Vec<String>,
    pub interaction_permissions: Vec<String>,
    pub display_fields_json: String,
}

impl DisciplineDefinitionSummary {
    pub fn from_row(table: &DisciplineTable, row: &DisciplineTableRow) -> Self {
        let tier_rules = parse_tier_rules(resolve_string(table, row.tierrules).as_deref())
            .unwrap_or_else(|_| DisciplineTierRules::default());
        Self {
            discipline_id: resolve_string(table, row.disciplineid).unwrap_or_default(),
            name: resolve_string(table, row.name).unwrap_or_default(),
            description: resolve_string(table, row.description).unwrap_or_default(),
            initial_tier: tier_rules.initial_tier,
            initial_points: tier_rules.initial_points,
            skill_pool: row
                .skillpool
                .iter()
                .filter_map(|key| table.resolve_string(*key).map(ToString::to_string))
                .collect(),
            interaction_permissions: row
                .interactionpermissions
                .iter()
                .filter_map(|key| table.resolve_string(*key).map(ToString::to_string))
                .collect(),
            display_fields_json: resolve_string(table, row.displayfields).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisciplineError {
    DisciplineNotFound,
    DisciplineConfigNotFound,
    DisciplineAlreadyLearned,
    DisciplineLearnConditionNotMet { reason: String },
    UnsupportedLearnCondition { condition_type: String },
    InvalidDisciplineId,
    InvalidPoints,
    InvalidDisciplineTier,
    InvalidDisciplineAction,
    InvalidDisciplineConfig { message: String },
    CharacterElement(CharacterElementError),
    Title(TitleError),
    Item(ItemError),
    DbUnavailable,
    DbError { message: String },
}

impl DisciplineError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::DisciplineNotFound => "DISCIPLINE_NOT_FOUND",
            Self::DisciplineConfigNotFound => "DISCIPLINE_CONFIG_NOT_FOUND",
            Self::DisciplineAlreadyLearned => "DISCIPLINE_ALREADY_LEARNED",
            Self::DisciplineLearnConditionNotMet { .. } => "DISCIPLINE_LEARN_CONDITION_NOT_MET",
            Self::UnsupportedLearnCondition { .. } => "UNSUPPORTED_DISCIPLINE_LEARN_CONDITION",
            Self::InvalidDisciplineId => "INVALID_DISCIPLINE_ID",
            Self::InvalidPoints => "INVALID_DISCIPLINE_POINTS",
            Self::InvalidDisciplineTier => "INVALID_DISCIPLINE_TIER",
            Self::InvalidDisciplineAction => "INVALID_DISCIPLINE_ACTION",
            Self::InvalidDisciplineConfig { .. } => "INVALID_DISCIPLINE_CONFIG",
            Self::CharacterElement(error) => error.error_code(),
            Self::Title(error) => error.error_code(),
            Self::Item(error) => error.as_str(),
            Self::DbUnavailable => "DISCIPLINE_DB_UNAVAILABLE",
            Self::DbError { .. } => "DISCIPLINE_DB_ERROR",
        }
    }
}

impl std::fmt::Display for DisciplineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DisciplineNotFound => write!(formatter, "discipline not found"),
            Self::DisciplineConfigNotFound => write!(formatter, "discipline config not found"),
            Self::DisciplineAlreadyLearned => write!(formatter, "discipline already learned"),
            Self::DisciplineLearnConditionNotMet { reason } => {
                write!(formatter, "discipline learn condition not met: {reason}")
            }
            Self::UnsupportedLearnCondition { condition_type } => {
                write!(
                    formatter,
                    "unsupported discipline learn condition: {condition_type}"
                )
            }
            Self::InvalidDisciplineId => write!(formatter, "discipline_id must not be empty"),
            Self::InvalidPoints => write!(formatter, "discipline points must be non-negative"),
            Self::InvalidDisciplineTier => write!(formatter, "unknown discipline tier"),
            Self::InvalidDisciplineAction => write!(formatter, "invalid discipline action"),
            Self::InvalidDisciplineConfig { message } => {
                write!(formatter, "invalid discipline config: {message}")
            }
            Self::CharacterElement(error) => write!(formatter, "character element error: {error}"),
            Self::Title(error) => write!(formatter, "title error: {error}"),
            Self::Item(error) => write!(formatter, "item error: {}", error.as_str()),
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
        self.upsert_with_context(
            &identity.character_id,
            upsert,
            DisciplineOperationContext::new("system")
                .with_source_id("discipline_service")
                .with_reason("discipline upsert"),
        )
        .await
    }

    pub async fn upsert_for_identity_with_context(
        &self,
        identity: &AuthenticatedSessionIdentity,
        upsert: DisciplineUpsert,
        context: DisciplineOperationContext,
    ) -> Result<CharacterDiscipline, DisciplineError> {
        self.upsert_with_context(&identity.character_id, upsert, context)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn learn_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
        request: LearnDisciplineRequest,
        discipline_table: &DisciplineTable,
        item_table: &ItemTable,
        character_element_service: &CharacterElementService,
        title_service: &TitleService,
        player_data: &mut PlayerData,
        context: DisciplineOperationContext,
    ) -> Result<LearnDisciplineResult, DisciplineError> {
        validate_context(&context)?;
        validate_discipline_id(&request.discipline_id)?;

        let row = resolve_discipline_row(discipline_table, &request.discipline_id)?;
        match self
            .get_for_identity(identity, &request.discipline_id)
            .await
        {
            Ok(_) => return Err(DisciplineError::DisciplineAlreadyLearned),
            Err(DisciplineError::DisciplineNotFound) => {}
            Err(error) => return Err(error),
        }

        let tier_rules =
            parse_tier_rules(resolve_string(discipline_table, row.tierrules).as_deref())?;
        let raw_conditions = resolve_string(discipline_table, row.learnconditions);
        let condition = parse_learn_condition(raw_conditions.as_deref())?;
        let current_disciplines = self.list_for_identity(identity).await?;

        let elements = if condition.requires_elements() {
            Some(
                character_element_service
                    .get_elements_for_identity(identity)
                    .await
                    .map_err(DisciplineError::CharacterElement)?,
            )
        } else {
            None
        };
        let titles = if condition.requires_titles() {
            Some(
                title_service
                    .list_for_identity(identity, title_context_from_discipline_context(&context))
                    .await
                    .map_err(DisciplineError::Title)?,
            )
        } else {
            None
        };

        let consumed_items = evaluate_learn_condition(
            &condition,
            &current_disciplines,
            elements.as_ref(),
            titles.as_deref(),
            player_data,
        )?;
        let upsert = DisciplineUpsert::new(
            request.discipline_id.clone(),
            tier_rules.initial_points,
            tier_rules.initial_tier.clone(),
            false,
        );

        let mut next_player_data = player_data.clone();
        consume_item_costs(&mut next_player_data, &consumed_items, item_table)?;

        let discipline = self
            .upsert_with_context(&identity.character_id, upsert, context)
            .await?;
        *player_data = next_player_data;

        Ok(LearnDisciplineResult {
            discipline,
            consumed_items,
        })
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
        self.upsert_with_context(
            character_id,
            upsert,
            DisciplineOperationContext::new("system")
                .with_source_id("discipline_service")
                .with_reason("discipline upsert"),
        )
        .await
    }

    pub async fn upsert_with_context(
        &self,
        character_id: &str,
        upsert: DisciplineUpsert,
        context: DisciplineOperationContext,
    ) -> Result<CharacterDiscipline, DisciplineError> {
        validate_upsert(&upsert)?;
        validate_context(&context)?;
        match &self.store {
            DisciplineStore::Pg(store) => store.upsert(character_id, upsert, context).await,
            #[cfg(test)]
            DisciplineStore::Memory(store) => {
                store.lock().await.upsert(character_id, upsert, context)
            }
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
            store: DisciplineStore::Memory(Arc::new(tokio::sync::Mutex::new(
                MemoryDisciplineStore::default(),
            ))),
        }
    }

    #[cfg(test)]
    pub(crate) async fn logs(&self) -> Vec<DisciplineLogEntry> {
        match &self.store {
            DisciplineStore::Memory(store) => store.lock().await.logs.clone(),
            DisciplineStore::Pg(_) => Vec::new(),
        }
    }
}

#[derive(Clone)]
enum DisciplineStore {
    Pg(PgDisciplineStore),
    #[cfg(test)]
    Memory(Arc<tokio::sync::Mutex<MemoryDisciplineStore>>),
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
        context: DisciplineOperationContext,
    ) -> Result<CharacterDiscipline, DisciplineError> {
        let Some(pool) = &self.pool else {
            return Err(DisciplineError::DbUnavailable);
        };

        let mut tx = pool.begin().await.map_err(map_db_error)?;
        let before = sqlx::query_as::<_, CharacterDisciplineRow>(LOCK_DISCIPLINE_SQL)
            .bind(character_id)
            .bind(&upsert.discipline_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?
            .map(CharacterDisciplineRow::into_discipline);
        let action = action_for_upsert(before.as_ref(), &upsert);

        let after = sqlx::query_as::<_, CharacterDisciplineRow>(UPSERT_DISCIPLINE_SQL)
            .bind(character_id)
            .bind(&upsert.discipline_id)
            .bind(upsert.points)
            .bind(&upsert.tier)
            .bind(upsert.active)
            .fetch_one(&mut *tx)
            .await
            .map(CharacterDisciplineRow::into_discipline)
            .map_err(map_db_error)?;

        insert_discipline_log(
            &mut tx,
            character_id,
            &after.discipline_id,
            action,
            &context,
            before.map(|value| value.snapshot_json()),
            Some(after.snapshot_json()),
        )
        .await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(after)
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

const LOCK_DISCIPLINE_SQL: &str = r#"SELECT
    character_id,
    discipline_id,
    points,
    tier,
    active,
    learned_at::text AS learned_at,
    updated_at::text AS updated_at
FROM character_disciplines
WHERE character_id = $1 AND discipline_id = $2
FOR UPDATE"#;

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

const INSERT_DISCIPLINE_LOG_SQL: &str = r#"INSERT INTO character_discipline_logs (
    character_id,
    discipline_id,
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

async fn insert_discipline_log(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    character_id: &str,
    discipline_id: &str,
    action: &str,
    context: &DisciplineOperationContext,
    before_json: Option<Value>,
    after_json: Option<Value>,
) -> Result<(), DisciplineError> {
    sqlx::query(INSERT_DISCIPLINE_LOG_SQL)
        .bind(character_id)
        .bind(discipline_id)
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

fn action_for_upsert(
    before: Option<&CharacterDiscipline>,
    upsert: &DisciplineUpsert,
) -> &'static str {
    let Some(before) = before else {
        return "learn";
    };
    match tier_compare(&upsert.tier, &before.tier) {
        std::cmp::Ordering::Greater => "upgrade",
        std::cmp::Ordering::Less => "downgrade",
        std::cmp::Ordering::Equal => {
            if upsert.active != before.active || upsert.points != before.points {
                "update"
            } else {
                "grant"
            }
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

fn validate_context(context: &DisciplineOperationContext) -> Result<(), DisciplineError> {
    if context.source_type.trim().is_empty() {
        return Err(DisciplineError::InvalidDisciplineAction);
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct DisciplineTierRules {
    initial_tier: String,
    initial_points: i64,
}

impl Default for DisciplineTierRules {
    fn default() -> Self {
        Self {
            initial_tier: "novice".to_string(),
            initial_points: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LearnCondition {
    AllOf(Vec<LearnCondition>),
    AnyOf(Vec<LearnCondition>),
    Affinity {
        element: ElementKind,
        min: i32,
    },
    Mastery {
        element: ElementKind,
        min: i32,
    },
    DisciplineTier {
        discipline_id: String,
        tier: String,
    },
    Title {
        title_id: String,
    },
    Item {
        item_id: i32,
        count: u32,
        consume: bool,
    },
    Unsupported {
        condition_type: String,
    },
}

impl LearnCondition {
    fn requires_elements(&self) -> bool {
        match self {
            Self::Affinity { .. } | Self::Mastery { .. } => true,
            Self::AllOf(conditions) | Self::AnyOf(conditions) => {
                conditions.iter().any(Self::requires_elements)
            }
            Self::DisciplineTier { .. }
            | Self::Title { .. }
            | Self::Item { .. }
            | Self::Unsupported { .. } => false,
        }
    }

    fn requires_titles(&self) -> bool {
        match self {
            Self::Title { .. } => true,
            Self::AllOf(conditions) | Self::AnyOf(conditions) => {
                conditions.iter().any(Self::requires_titles)
            }
            Self::Affinity { .. }
            | Self::Mastery { .. }
            | Self::DisciplineTier { .. }
            | Self::Item { .. }
            | Self::Unsupported { .. } => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementKind {
    Earth,
    Fire,
    Water,
    Wind,
}

impl ElementKind {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "earth" => Some(Self::Earth),
            "fire" => Some(Self::Fire),
            "water" => Some(Self::Water),
            "wind" => Some(Self::Wind),
            _ => None,
        }
    }

    fn value_from(self, values: ElementValues) -> i32 {
        match self {
            Self::Earth => values.earth,
            Self::Fire => values.fire,
            Self::Water => values.water,
            Self::Wind => values.wind,
        }
    }
}

fn resolve_discipline_row<'a>(
    table: &'a DisciplineTable,
    discipline_id: &str,
) -> Result<&'a DisciplineTableRow, DisciplineError> {
    table
        .all()
        .iter()
        .find(|row| {
            resolve_string(table, row.disciplineid)
                .as_deref()
                .is_some_and(|value| value == discipline_id)
        })
        .ok_or(DisciplineError::DisciplineConfigNotFound)
}

fn parse_tier_rules(raw: Option<&str>) -> Result<DisciplineTierRules, DisciplineError> {
    let Some(raw) = raw else {
        return Err(invalid_config("missing TierRules"));
    };
    let value = serde_json::from_str::<Value>(raw.trim())
        .map_err(|error| invalid_config(format!("invalid TierRules JSON: {error}")))?;
    let initial_tier = string_field(&value, &["initial_tier", "initialTier"])
        .unwrap_or_else(|| "novice".to_string());
    if !VALID_DISCIPLINE_TIERS.contains(&initial_tier.as_str()) {
        return Err(DisciplineError::InvalidDisciplineTier);
    }
    let initial_points =
        i64::from(number_field(&value, &["initial_points", "initialPoints"]).unwrap_or(0));
    if initial_points < 0 {
        return Err(DisciplineError::InvalidPoints);
    }
    Ok(DisciplineTierRules {
        initial_tier,
        initial_points,
    })
}

fn parse_learn_condition(raw: Option<&str>) -> Result<LearnCondition, DisciplineError> {
    let Some(raw) = raw else {
        return Err(invalid_config("missing LearnConditions"));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_config("empty LearnConditions"));
    }
    let value = serde_json::from_str::<Value>(trimmed)
        .map_err(|error| invalid_config(format!("invalid LearnConditions JSON: {error}")))?;
    parse_condition_value(&value)
}

fn parse_condition_value(value: &Value) -> Result<LearnCondition, DisciplineError> {
    match value {
        Value::Object(map) => {
            if let Some(all_of) = map.get("all_of").or_else(|| map.get("allOf")) {
                return parse_condition_array(all_of).map(LearnCondition::AllOf);
            }
            if let Some(any_of) = map.get("any_of").or_else(|| map.get("anyOf")) {
                return parse_condition_array(any_of).map(LearnCondition::AnyOf);
            }

            let condition_type = string_field(value, &["type", "kind", "rule"])
                .ok_or_else(|| invalid_config("condition requires type/all_of/any_of"))?;
            match condition_type.as_str() {
                "affinity" => parse_element_threshold(value, true),
                "mastery" => parse_element_threshold(value, false),
                "discipline_tier" => {
                    let discipline_id = string_field(value, &["discipline_id", "discipline"])
                        .ok_or_else(|| invalid_config("discipline_tier requires discipline_id"))?;
                    let tier = string_field(value, &["tier", "min_tier"])
                        .ok_or_else(|| invalid_config("discipline_tier requires tier"))?;
                    Ok(LearnCondition::DisciplineTier {
                        discipline_id,
                        tier,
                    })
                }
                "title" => {
                    let title_id = string_field(value, &["title_id", "title"])
                        .ok_or_else(|| invalid_config("title condition requires title_id"))?;
                    Ok(LearnCondition::Title { title_id })
                }
                "item" => {
                    let item_id = number_field(value, &["item_id", "itemId"])
                        .ok_or_else(|| invalid_config("item condition requires item_id"))?;
                    let count = number_field(value, &["count", "amount"]).unwrap_or(1);
                    if item_id <= 0 || count <= 0 {
                        return Err(invalid_config(
                            "item condition item_id/count must be positive",
                        ));
                    }
                    Ok(LearnCondition::Item {
                        item_id,
                        count: count as u32,
                        consume: bool_field(value, &["consume"]).unwrap_or(false),
                    })
                }
                "quest" | "event" | "npc_affection" | "organization" | "scene_location"
                | "world_state" | "world_status" | "world_flag" => {
                    Ok(LearnCondition::Unsupported { condition_type })
                }
                other => Ok(LearnCondition::Unsupported {
                    condition_type: other.to_string(),
                }),
            }
        }
        Value::Array(_) => parse_condition_array(value).map(LearnCondition::AllOf),
        _ => Err(invalid_config("condition must be object or array")),
    }
}

fn parse_condition_array(value: &Value) -> Result<Vec<LearnCondition>, DisciplineError> {
    let values = value
        .as_array()
        .ok_or_else(|| invalid_config("condition group requires array"))?;
    if values.is_empty() {
        return Err(invalid_config("condition group must not be empty"));
    }
    values.iter().map(parse_condition_value).collect()
}

fn parse_element_threshold(
    value: &Value,
    affinity: bool,
) -> Result<LearnCondition, DisciplineError> {
    let element = string_field(value, &["element"])
        .and_then(|value| ElementKind::parse(&value))
        .ok_or_else(|| invalid_config("element condition requires earth/fire/water/wind"))?;
    let min = number_field(value, &["min", "threshold", "value", "required"])
        .ok_or_else(|| invalid_config("element condition requires min"))?;
    if min < 0 {
        return Err(invalid_config("element condition min must be non-negative"));
    }
    if affinity {
        Ok(LearnCondition::Affinity { element, min })
    } else {
        Ok(LearnCondition::Mastery { element, min })
    }
}

fn evaluate_learn_condition(
    condition: &LearnCondition,
    disciplines: &[CharacterDiscipline],
    elements: Option<&CharacterElements>,
    titles: Option<&[CharacterTitle]>,
    player_data: &PlayerData,
) -> Result<Vec<DisciplineItemCost>, DisciplineError> {
    match condition {
        LearnCondition::AllOf(conditions) => {
            let mut costs = Vec::new();
            for nested in conditions {
                costs.extend(evaluate_learn_condition(
                    nested,
                    disciplines,
                    elements,
                    titles,
                    player_data,
                )?);
            }
            Ok(costs)
        }
        LearnCondition::AnyOf(conditions) => {
            let mut last_error = None;
            for nested in conditions {
                match evaluate_learn_condition(nested, disciplines, elements, titles, player_data) {
                    Ok(costs) => return Ok(costs),
                    Err(error) => last_error = Some(error),
                }
            }
            Err(last_error.unwrap_or_else(|| condition_not_met("any_of has no matched rule")))
        }
        LearnCondition::Affinity { element, min } => {
            let current = elements
                .map(|elements| element.value_from(elements.affinity))
                .unwrap_or_default();
            if current >= *min {
                Ok(Vec::new())
            } else {
                Err(condition_not_met(format!("affinity {current} < {min}")))
            }
        }
        LearnCondition::Mastery { element, min } => {
            let current = elements
                .map(|elements| element.value_from(elements.mastery))
                .unwrap_or_default();
            if current >= *min {
                Ok(Vec::new())
            } else {
                Err(condition_not_met(format!("mastery {current} < {min}")))
            }
        }
        LearnCondition::DisciplineTier {
            discipline_id,
            tier,
        } => {
            let matched = disciplines
                .iter()
                .find(|discipline| discipline.discipline_id == *discipline_id)
                .is_some_and(|discipline| discipline_tier_satisfies(&discipline.tier, tier));
            if matched {
                Ok(Vec::new())
            } else {
                Err(condition_not_met(format!(
                    "discipline {discipline_id} tier < {tier}"
                )))
            }
        }
        LearnCondition::Title { title_id } => {
            let matched = titles
                .unwrap_or_default()
                .iter()
                .any(|title| title.title_id == *title_id && !title.expired);
            if matched {
                Ok(Vec::new())
            } else {
                Err(condition_not_met(format!("title {title_id} not owned")))
            }
        }
        LearnCondition::Item {
            item_id,
            count,
            consume,
        } => {
            let costs = collect_item_costs(player_data, *item_id, *count)?;
            if *consume { Ok(costs) } else { Ok(Vec::new()) }
        }
        LearnCondition::Unsupported { condition_type } => {
            Err(DisciplineError::UnsupportedLearnCondition {
                condition_type: condition_type.clone(),
            })
        }
    }
}

fn collect_item_costs(
    player_data: &PlayerData,
    item_id: i32,
    count: u32,
) -> Result<Vec<DisciplineItemCost>, DisciplineError> {
    let mut remaining = count;
    let mut costs = Vec::new();
    for item in player_data.get_inventory_items() {
        if item.item_id != item_id || item.is_bound_to_other_character(&player_data.character_id) {
            continue;
        }
        let take = item.count.min(remaining);
        if take > 0 {
            costs.push(DisciplineItemCost {
                item_uid: item.uid,
                item_id,
                count: take,
            });
            remaining -= take;
            if remaining == 0 {
                return Ok(costs);
            }
        }
    }
    Err(condition_not_met(format!(
        "item {item_id} count not enough"
    )))
}

fn consume_item_costs(
    player_data: &mut PlayerData,
    costs: &[DisciplineItemCost],
    item_table: &ItemTable,
) -> Result<(), DisciplineError> {
    for cost in costs {
        let Some(item) = player_data.inventory.find_item(cost.item_uid) else {
            return Err(DisciplineError::Item(ItemError::ItemNotFound));
        };
        if item.item_id != cost.item_id {
            return Err(DisciplineError::Item(ItemError::ItemNotFound));
        }
        if item_table.get(cost.item_id).is_none() {
            return Err(DisciplineError::Item(ItemError::ItemNotFound));
        }
        player_data
            .remove_item(cost.item_uid, cost.count)
            .map_err(DisciplineError::Item)?;
    }
    Ok(())
}

fn discipline_tier_satisfies(current: &str, required: &str) -> bool {
    tier_compare(current, required) != std::cmp::Ordering::Less
}

fn tier_compare(left: &str, right: &str) -> std::cmp::Ordering {
    let left = left.trim().to_ascii_lowercase();
    let right = right.trim().to_ascii_lowercase();
    match (
        VALID_DISCIPLINE_TIERS.iter().position(|tier| *tier == left),
        VALID_DISCIPLINE_TIERS
            .iter()
            .position(|tier| *tier == right),
    ) {
        (Some(left), Some(right)) => left.cmp(&right),
        _ => left.cmp(&right),
    }
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(value) = map.get(*key).or_else(|| map.get(&to_camel_case(key))) {
            if let Some(text) = value.as_str() {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_ascii_lowercase());
                }
            } else if let Some(number) = value.as_i64() {
                return Some(number.to_string());
            }
        }
    }
    None
}

fn number_field(value: &Value, keys: &[&str]) -> Option<i32> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(value) = map.get(*key).or_else(|| map.get(&to_camel_case(key))) {
            if let Some(number) = value.as_i64() {
                return i32::try_from(number).ok();
            }
            if let Some(text) = value.as_str() {
                if let Ok(parsed) = text.trim().parse::<i32>() {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

fn bool_field(value: &Value, keys: &[&str]) -> Option<bool> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(value) = map.get(*key).or_else(|| map.get(&to_camel_case(key))) {
            if let Some(flag) = value.as_bool() {
                return Some(flag);
            }
            if let Some(text) = value.as_str() {
                match text.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" => return Some(true),
                    "false" | "0" | "no" => return Some(false),
                    _ => {}
                }
            }
        }
    }
    None
}

fn to_camel_case(value: &str) -> String {
    let mut result = String::new();
    let mut uppercase_next = false;
    for ch in value.chars() {
        if ch == '_' {
            uppercase_next = true;
        } else if uppercase_next {
            result.push(ch.to_ascii_uppercase());
            uppercase_next = false;
        } else {
            result.push(ch);
        }
    }
    result
}

fn title_context_from_discipline_context(
    context: &DisciplineOperationContext,
) -> TitleOperationContext {
    let mut title_context = TitleOperationContext::new("discipline")
        .with_source_id("discipline_learn_condition")
        .with_reason("discipline learn title condition check");
    if let Some(operator_id) = context.operator_id.as_ref() {
        title_context = title_context.with_operator(
            context.operator_type.as_deref().unwrap_or("player"),
            operator_id.clone(),
        );
    }
    title_context
}

fn condition_not_met(reason: impl Into<String>) -> DisciplineError {
    DisciplineError::DisciplineLearnConditionNotMet {
        reason: reason.into(),
    }
}

fn invalid_config(message: impl Into<String>) -> DisciplineError {
    DisciplineError::InvalidDisciplineConfig {
        message: message.into(),
    }
}

fn resolve_string(table: &DisciplineTable, key: u32) -> Option<String> {
    table.resolve_string(key).map(ToString::to_string)
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DisciplineLogEntry {
    pub(crate) character_id: String,
    pub(crate) discipline_id: String,
    pub(crate) action: String,
    pub(crate) source_type: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) operator_type: Option<String>,
    pub(crate) operator_id: Option<String>,
    pub(crate) before_json: Option<Value>,
    pub(crate) after_json: Option<Value>,
    pub(crate) reason: Option<String>,
}

#[cfg(test)]
#[derive(Default)]
struct MemoryDisciplineStore {
    values: BTreeMap<(String, String), CharacterDiscipline>,
    logs: Vec<DisciplineLogEntry>,
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
        context: DisciplineOperationContext,
    ) -> Result<CharacterDiscipline, DisciplineError> {
        let key = (character_id.to_string(), upsert.discipline_id.clone());
        let before = self.values.get(&key).cloned();
        let action = action_for_upsert(before.as_ref(), &upsert);
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
        self.logs.push(DisciplineLogEntry {
            character_id: character_id.to_string(),
            discipline_id: value.discipline_id.clone(),
            action: action.to_string(),
            source_type: Some(context.source_type),
            source_id: context.source_id,
            operator_type: context.operator_type,
            operator_id: context.operator_id,
            before_json: before.map(|value| value.snapshot_json()),
            after_json: Some(value.snapshot_json()),
            reason: context.reason,
        });
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::character_element::{CharacterElementService, CharacterElements};
    use crate::core::character_title::TitleService;
    use crate::core::inventory::Item;
    use crate::csv_code::disciplinetable::{DisciplineTable, DisciplineTableRow, StringKey};
    use crate::csv_code::itemtable::ItemTableRow;
    use crate::csv_code::titletable::{TitleTable, TitleTableRow};
    use std::collections::HashMap;

    fn identity() -> AuthenticatedSessionIdentity {
        AuthenticatedSessionIdentity {
            account_player_id: "plr_0000000000001".to_string(),
            character_id: "chr_0000000000001".to_string(),
            world_id: Some(0),
        }
    }

    fn context() -> DisciplineOperationContext {
        DisciplineOperationContext::new("player")
            .with_source_id("discipline_learn_protocol")
            .with_operator("player", "plr_0000000000001")
            .with_reason("unit-test")
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

    #[test]
    fn learn_condition_parses_supported_and_unsupported_sources() {
        let rule = parse_learn_condition(Some(
            r#"{"all_of":[{"type":"affinity","element":"fire","min":2000},{"type":"mastery","element":"fire","min":10},{"type":"item","item_id":4101,"count":1,"consume":true},{"type":"event","event_id":"wind"}]}"#,
        ))
        .expect("rule should parse");

        assert!(rule.requires_elements());
        assert!(!rule.requires_titles());

        let mut player_data = PlayerData::new("chr_0000000000001".to_string());
        player_data.add_item(Item::new(7, 4101, 1, false)).unwrap();

        let unsupported = evaluate_learn_condition(
            &rule,
            &[],
            Some(&CharacterElements {
                character_id: "chr_0000000000001".to_string(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::new(0, 10, 0, 0),
            }),
            None,
            &player_data,
        )
        .unwrap_err();
        assert_eq!(
            unsupported.error_code(),
            "UNSUPPORTED_DISCIPLINE_LEARN_CONDITION"
        );
    }

    #[tokio::test]
    async fn formal_learn_uses_identity_character_consumes_items_and_logs() {
        let identity = identity();
        let service = DisciplineService::new_in_memory();
        let element_service = CharacterElementService::new_in_memory();
        element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::new(0, 10, 0, 0),
            })
            .await;
        service
            .upsert_for_identity(
                &identity,
                DisciplineUpsert::new("forging", 1, "novice", true),
            )
            .await
            .unwrap();

        let title_service = TitleService::new_in_memory(title_table());
        let table = discipline_table();
        let item_table = item_table();
        let mut player_data = PlayerData::new(identity.character_id.clone());
        player_data.add_item(Item::new(7, 4101, 1, false)).unwrap();

        let result = service
            .learn_for_identity(
                &identity,
                LearnDisciplineRequest::new("fire_art"),
                &table,
                &item_table,
                &element_service,
                &title_service,
                &mut player_data,
                context(),
            )
            .await
            .expect("formal learn should succeed");

        assert_eq!(result.discipline.character_id, identity.character_id);
        assert_eq!(result.discipline.discipline_id, "fire_art");
        assert_eq!(result.discipline.tier, "novice");
        assert_eq!(result.consumed_items.len(), 1);
        assert!(player_data.inventory.find_item(7).is_none());

        let logs = service.logs().await;
        let learn_log = logs
            .iter()
            .find(|log| log.discipline_id == "fire_art")
            .expect("learn log should be written");
        assert_eq!(learn_log.action, "learn");
        assert_eq!(learn_log.source_type.as_deref(), Some("player"));
        assert_eq!(learn_log.operator_id.as_deref(), Some("plr_0000000000001"));
        assert!(learn_log.before_json.is_none());
        assert!(learn_log.after_json.is_some());
    }

    #[tokio::test]
    async fn failed_learn_does_not_consume_items() {
        let identity = identity();
        let service = DisciplineService::new_in_memory();
        let element_service = CharacterElementService::new_in_memory();
        element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::zero(),
            })
            .await;

        let title_service = TitleService::new_in_memory(title_table());
        let table = discipline_table();
        let item_table = item_table();
        let mut player_data = PlayerData::new(identity.character_id.clone());
        player_data.add_item(Item::new(7, 4101, 1, false)).unwrap();

        let error = service
            .learn_for_identity(
                &identity,
                LearnDisciplineRequest::new("fire_art"),
                &table,
                &item_table,
                &element_service,
                &title_service,
                &mut player_data,
                context(),
            )
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), "DISCIPLINE_LEARN_CONDITION_NOT_MET");
        assert_eq!(player_data.inventory.find_item(7).unwrap().count, 1);
        assert!(service.logs().await.is_empty());
    }

    #[tokio::test]
    async fn duplicate_learn_returns_stable_error() {
        let identity = identity();
        let service = DisciplineService::new_in_memory();
        service
            .upsert_for_identity(
                &identity,
                DisciplineUpsert::new("forging", 0, "novice", false),
            )
            .await
            .unwrap();
        let element_service = CharacterElementService::new_in_memory();
        element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::zero(),
            })
            .await;
        let title_service = TitleService::new_in_memory(title_table());
        let mut player_data = PlayerData::new(identity.character_id.clone());

        let error = service
            .learn_for_identity(
                &identity,
                LearnDisciplineRequest::new("forging"),
                &discipline_table(),
                &item_table(),
                &element_service,
                &title_service,
                &mut player_data,
                context(),
            )
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), "DISCIPLINE_ALREADY_LEARNED");
    }

    #[test]
    fn action_for_upsert_classifies_upgrade_downgrade_and_grant() {
        let before = CharacterDiscipline {
            character_id: "chr".to_string(),
            discipline_id: "forging".to_string(),
            points: 10,
            tier: "adept".to_string(),
            active: false,
            learned_at: "now".to_string(),
            updated_at: "now".to_string(),
        };
        assert_eq!(
            action_for_upsert(
                Some(&before),
                &DisciplineUpsert::new("forging", 20, "expert", false)
            ),
            "upgrade"
        );
        assert_eq!(
            action_for_upsert(
                Some(&before),
                &DisciplineUpsert::new("forging", 0, "novice", false)
            ),
            "downgrade"
        );
        assert_eq!(
            action_for_upsert(
                Some(&before),
                &DisciplineUpsert::new("forging", 10, "adept", false)
            ),
            "grant"
        );
    }

    fn discipline_table() -> DisciplineTable {
        let mut builder = DisciplineTableBuilder::new();
        builder.add(
            1,
            "forging",
            r#"{"type":"affinity","element":"fire","min":2000}"#,
            r#"{"initial_tier":"novice","initial_points":0,"tiers":[{"tier":"novice","min_points":0}]}"#,
        );
        builder.add(
            2,
            "fire_art",
            r#"{"all_of":[{"type":"mastery","element":"fire","min":10},{"type":"discipline_tier","discipline_id":"forging","tier":"novice"},{"type":"item","item_id":4101,"count":1,"consume":true}]}"#,
            r#"{"initial_tier":"novice","initial_points":0,"tiers":[{"tier":"novice","min_points":0}]}"#,
        );
        builder.finish()
    }

    fn item_table() -> ItemTable {
        let mut string_pool = HashMap::new();
        string_pool.insert(1, "item".to_string());
        let rows = vec![ItemTableRow {
            id: 4101,
            code: 1,
            name: 1,
            maxstack: 99,
            ..ItemTableRow::default()
        }];
        let by_id = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id, index))
            .collect();
        ItemTable {
            string_pool,
            rows,
            by_id,
        }
    }

    fn title_table() -> Arc<TitleTable> {
        let rows = vec![TitleTableRow {
            titleid: 1001,
            ..TitleTableRow::default()
        }];
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

    struct DisciplineTableBuilder {
        string_pool: HashMap<StringKey, String>,
        rows: Vec<DisciplineTableRow>,
        by_id: HashMap<i32, usize>,
        next_key: StringKey,
    }

    impl DisciplineTableBuilder {
        fn new() -> Self {
            Self {
                string_pool: HashMap::new(),
                rows: Vec::new(),
                by_id: HashMap::new(),
                next_key: 1,
            }
        }

        fn key(&mut self, value: &str) -> StringKey {
            if let Some((&key, _)) = self
                .string_pool
                .iter()
                .find(|(_, existing)| existing.as_str() == value)
            {
                return key;
            }
            let key = self.next_key;
            self.next_key += 1;
            self.string_pool.insert(key, value.to_string());
            key
        }

        fn add(&mut self, id: i32, discipline_id: &str, conditions: &str, tier_rules: &str) {
            let row = DisciplineTableRow {
                id,
                disciplineid: self.key(discipline_id),
                name: self.key(discipline_id),
                description: self.key("desc"),
                learnconditions: self.key(conditions),
                tierrules: self.key(tier_rules),
                skillpool: vec![self.key("skill")],
                interactionpermissions: vec![self.key("learn")],
                displayfields: self.key(r#"{"icon":"x"}"#),
            };
            self.by_id.insert(id, self.rows.len());
            self.rows.push(row);
        }

        fn finish(self) -> DisciplineTable {
            DisciplineTable {
                string_pool: self.string_pool,
                rows: self.rows,
                by_id: self.by_id,
            }
        }
    }
}
