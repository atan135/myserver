use sqlx::postgres::{PgPool, PgPoolOptions};
use tracing::info;

#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::config::Config;
use crate::core::inventory::Item;
use crate::core::inventory::player_data::PlayerData;
use crate::core::inventory::{
    AttrPanel, Buff, EquipmentSlots, ItemContainer, PlayerAttr, PlayerVisual,
};
use crate::core::player::grant_contract::GrantResultSummary;

/// Correlation metadata recorded beside every durable grant ledger row.  It is intentionally
/// compact: the ledger must explain a settlement without retaining an unbounded item snapshot
/// or mail attachment body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetLedgerContext {
    pub origin_type: String,
    pub origin_id: String,
    pub delivery_method: String,
    pub delivery_id: Option<String>,
    pub mail_id: Option<String>,
    pub fallback_reason: Option<String>,
    pub operator_id: Option<String>,
}

impl AssetLedgerContext {
    pub fn direct(source: &str, request_id: &str) -> Self {
        Self {
            origin_type: source.to_string(),
            origin_id: request_id.to_string(),
            delivery_method: "direct".to_string(),
            delivery_id: Some(request_id.to_string()),
            mail_id: None,
            fallback_reason: None,
            operator_id: None,
        }
    }

    pub fn mail_claim(source: &str, _request_id: &str, mail_id: &str) -> Self {
        Self {
            origin_type: source.replace('-', "_"),
            origin_id: mail_id.to_string(),
            delivery_method: "mail".to_string(),
            delivery_id: Some(mail_id.to_string()),
            mail_id: Some(mail_id.to_string()),
            fallback_reason: None,
            operator_id: None,
        }
    }

    pub fn player_operation(operation: &str, request_id: &str) -> Self {
        Self {
            origin_type: "player_operation".to_string(),
            origin_id: request_id.to_string(),
            delivery_method: "direct".to_string(),
            delivery_id: Some(request_id.to_string()),
            mail_id: None,
            fallback_reason: None,
            operator_id: Some(operation.to_string()),
        }
    }

    fn normalized(&self, source: &str, request_id: &str) -> Self {
        let mut context = self.clone();
        if context.origin_type.trim().is_empty() {
            context.origin_type = source.to_string();
        }
        if context.origin_id.trim().is_empty() {
            context.origin_id = request_id.to_string();
        }
        if context.delivery_method.trim().is_empty() {
            context.delivery_method = "direct".to_string();
        }
        context
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantRecord {
    pub request_id: String,
    pub character_id: String,
    pub request_fingerprint: String,
    pub result_summary: GrantResultSummary,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantRecordLookup {
    NotFound,
    Succeeded(GrantRecord),
    ResultUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveGrantRecordOutcome {
    Applied(GrantRecord),
    Existing(GrantRecordLookup),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SavePlayerOutcome {
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SavePlayerError {
    NotApplied(String),
    VersionConflict,
    ResultUnknown(String),
}

impl std::fmt::Display for SavePlayerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotApplied(error) => write!(formatter, "player save not applied: {error}"),
            Self::VersionConflict => write!(formatter, "player snapshot revision conflict"),
            Self::ResultUnknown(error) => write!(formatter, "player save result unknown: {error}"),
        }
    }
}

impl std::error::Error for SavePlayerError {}

#[derive(Debug)]
pub enum SaveGrantRecordError {
    NotApplied(String),
    ResultUnknown(String),
}

impl std::fmt::Display for SaveGrantRecordError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotApplied(error) => write!(formatter, "grant transaction not applied: {error}"),
            Self::ResultUnknown(error) => {
                write!(formatter, "grant transaction result unknown: {error}")
            }
        }
    }
}

impl std::error::Error for SaveGrantRecordError {}

/// PostgreSQL Character Inventory Store.
/// 负责角色背包数据的持久化。
#[derive(Clone)]
pub struct PgPlayerStore {
    pool: Option<PgPool>,
    #[cfg(test)]
    test_behavior: Option<TestStoreBehavior>,
}

#[cfg(test)]
#[derive(Clone)]
struct TestStoreBehavior {
    load_error: Option<String>,
    save_error: Option<String>,
    save_result_unknown: Option<String>,
    grant_save_error: Option<String>,
    grant_save_result_unknown: Option<String>,
    save_attempts: Arc<AtomicUsize>,
    grant_save_attempts: Arc<AtomicUsize>,
}

impl PgPlayerStore {
    /// 创建一个禁用的 store（用于测试和本地无数据库模式）。
    pub fn new_disabled() -> Self {
        Self {
            pool: None,
            #[cfg(test)]
            test_behavior: None,
        }
    }

    #[cfg(test)]
    pub fn new_failing_load_for_test(error: impl Into<String>) -> Self {
        Self {
            pool: None,
            test_behavior: Some(TestStoreBehavior {
                load_error: Some(error.into()),
                save_error: None,
                save_result_unknown: None,
                grant_save_error: None,
                grant_save_result_unknown: None,
                save_attempts: Arc::new(AtomicUsize::new(0)),
                grant_save_attempts: Arc::new(AtomicUsize::new(0)),
            }),
        }
    }

    #[cfg(test)]
    pub fn new_failing_save_for_test(error: impl Into<String>) -> Self {
        Self {
            pool: None,
            test_behavior: Some(TestStoreBehavior {
                load_error: None,
                save_error: Some(error.into()),
                save_result_unknown: None,
                grant_save_error: None,
                grant_save_result_unknown: None,
                save_attempts: Arc::new(AtomicUsize::new(0)),
                grant_save_attempts: Arc::new(AtomicUsize::new(0)),
            }),
        }
    }

    #[cfg(test)]
    pub fn new_failing_grant_save_for_test(error: impl Into<String>) -> Self {
        Self {
            pool: None,
            test_behavior: Some(TestStoreBehavior {
                load_error: None,
                save_error: None,
                save_result_unknown: None,
                grant_save_error: Some(error.into()),
                grant_save_result_unknown: None,
                save_attempts: Arc::new(AtomicUsize::new(0)),
                grant_save_attempts: Arc::new(AtomicUsize::new(0)),
            }),
        }
    }

    #[cfg(test)]
    pub fn new_unknown_grant_commit_for_test(error: impl Into<String>) -> Self {
        Self {
            pool: None,
            test_behavior: Some(TestStoreBehavior {
                load_error: None,
                save_error: None,
                save_result_unknown: None,
                grant_save_error: None,
                grant_save_result_unknown: Some(error.into()),
                save_attempts: Arc::new(AtomicUsize::new(0)),
                grant_save_attempts: Arc::new(AtomicUsize::new(0)),
            }),
        }
    }

    #[cfg(test)]
    pub fn grant_save_attempts_for_test(&self) -> usize {
        self.test_behavior
            .as_ref()
            .map(|behavior| behavior.grant_save_attempts.load(Ordering::Relaxed))
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub fn save_attempts_for_test(&self) -> usize {
        self.test_behavior
            .as_ref()
            .map(|behavior| behavior.save_attempts.load(Ordering::Relaxed))
            .unwrap_or_default()
    }

    pub async fn new(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        if !config.db_enabled {
            return Ok(Self {
                pool: None,
                #[cfg(test)]
                test_behavior: None,
            });
        }

        let pool = PgPoolOptions::new()
            .max_connections(config.db_pool_size.max(1))
            .connect(&config.database_url)
            .await?;

        info!("PgPlayerStore initialized");
        Ok(Self {
            pool: Some(pool),
            #[cfg(test)]
            test_behavior: None,
        })
    }

    pub fn enabled(&self) -> bool {
        #[cfg(test)]
        if self.test_behavior.is_some() {
            return true;
        }
        self.pool.is_some()
    }

    pub async fn close(&self) {
        if let Some(pool) = &self.pool {
            pool.close().await;
        }
    }

    /// Persist a complete gameplay snapshot with an optimistic revision check.
    ///
    /// An error returned by `commit` is intentionally `ResultUnknown`: callers must query the
    /// original request before deciding whether an asset action may be retried or fall back.
    pub async fn save(
        &self,
        character_id: &str,
        data: &PlayerData,
    ) -> Result<SavePlayerOutcome, SavePlayerError> {
        #[cfg(test)]
        if let Some(behavior) = &self.test_behavior {
            behavior.save_attempts.fetch_add(1, Ordering::Relaxed);
            if let Some(error) = &behavior.save_error {
                return Err(SavePlayerError::NotApplied(error.clone()));
            }
            if let Some(error) = &behavior.save_result_unknown {
                return Err(SavePlayerError::ResultUnknown(error.clone()));
            }
            return Ok(SavePlayerOutcome {
                revision: data.persistence_revision().saturating_add(1),
            });
        }

        let Some(pool) = &self.pool else {
            return Err(SavePlayerError::NotApplied(
                "database not enabled".to_string(),
            ));
        };

        let json = serialize_player_data(data).map_err(SavePlayerError::NotApplied)?;
        let mut tx = pool
            .begin()
            .await
            .map_err(|error| SavePlayerError::NotApplied(error.to_string()))?;
        let revision = match save_snapshot_with_revision(
            &mut tx,
            character_id,
            data.persistence_revision(),
            data.get_hp(),
            &json,
        )
        .await
        {
            Ok(Some(revision)) => revision,
            Ok(None) => {
                let _ = tx.rollback().await;
                return Err(SavePlayerError::VersionConflict);
            }
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(SavePlayerError::NotApplied(error.to_string()));
            }
        };

        tx.commit()
            .await
            .map_err(|error| SavePlayerError::ResultUnknown(error.to_string()))?;

        info!(character_id = %character_id, revision, "character inventory saved");
        Ok(SavePlayerOutcome { revision })
    }

    pub async fn save_with_grant_record(
        &self,
        character_id: &str,
        data: &PlayerData,
        request_id: &str,
        request_fingerprint: &str,
        source: &str,
        reason: &str,
        items: &[Item],
        result_summary: &GrantResultSummary,
    ) -> Result<SaveGrantRecordOutcome, SaveGrantRecordError> {
        self.save_with_grant_record_and_ledger_context(
            character_id,
            data,
            request_id,
            request_fingerprint,
            source,
            reason,
            items,
            result_summary,
            AssetLedgerContext::direct(source, request_id),
        )
        .await
    }

    /// Persist a player-originated asset mutation and its append-only quantity deltas in one
    /// PostgreSQL transaction. The caller supplies snapshots before and after mutation; this
    /// method never accepts a caller-provided ledger row or JSONB replacement.
    pub async fn save_with_asset_ledger(
        &self,
        character_id: &str,
        before: &PlayerData,
        after: &PlayerData,
        request_id: &str,
        source: &str,
        reason: &str,
        ledger_context: AssetLedgerContext,
    ) -> Result<SavePlayerOutcome, SavePlayerError> {
        #[cfg(test)]
        if let Some(behavior) = &self.test_behavior {
            behavior.save_attempts.fetch_add(1, Ordering::Relaxed);
            if let Some(error) = &behavior.save_error {
                return Err(SavePlayerError::NotApplied(error.clone()));
            }
            if let Some(error) = &behavior.save_result_unknown {
                return Err(SavePlayerError::ResultUnknown(error.clone()));
            }
            return Ok(SavePlayerOutcome {
                revision: after.persistence_revision().saturating_add(1),
            });
        }

        let Some(pool) = &self.pool else {
            return Err(SavePlayerError::NotApplied(
                "database not enabled".to_string(),
            ));
        };
        let json = serialize_player_data(after).map_err(SavePlayerError::NotApplied)?;
        let mut tx = pool
            .begin()
            .await
            .map_err(|error| SavePlayerError::NotApplied(error.to_string()))?;
        let request_inserted = sqlx::query_scalar::<_, i64>(
            r#"INSERT INTO character_asset_requests (
                request_id,
                character_id,
                request_fingerprint,
                result_json
            ) VALUES ($1, $2, 'sha256:player-operation', $3)
            ON CONFLICT (request_id) DO NOTHING
            RETURNING id"#,
        )
        .bind(request_id)
        .bind(character_id)
        .bind(serde_json::json!({ "operation": source, "state": "applied" }))
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| SavePlayerError::NotApplied(error.to_string()))?;
        if request_inserted.is_none() {
            let _ = tx.rollback().await;
            return Err(SavePlayerError::NotApplied(
                "player asset request id already exists".to_string(),
            ));
        }
        let revision = match save_snapshot_with_revision(
            &mut tx,
            character_id,
            after.persistence_revision(),
            after.get_hp(),
            &json,
        )
        .await
        {
            Ok(Some(revision)) => revision,
            Ok(None) => {
                let _ = tx.rollback().await;
                return Err(SavePlayerError::VersionConflict);
            }
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(SavePlayerError::NotApplied(error.to_string()));
            }
        };

        insert_player_mutation_ledger_entries(
            &mut tx,
            character_id,
            request_id,
            source,
            reason,
            revision,
            before,
            after,
            &ledger_context.normalized(source, request_id),
        )
        .await
        .map_err(|error| SavePlayerError::NotApplied(error.to_string()))?;

        tx.commit()
            .await
            .map_err(|error| SavePlayerError::ResultUnknown(error.to_string()))?;
        Ok(SavePlayerOutcome { revision })
    }

    pub async fn save_with_grant_record_and_ledger_context(
        &self,
        character_id: &str,
        data: &PlayerData,
        request_id: &str,
        request_fingerprint: &str,
        source: &str,
        reason: &str,
        items: &[Item],
        result_summary: &GrantResultSummary,
        ledger_context: AssetLedgerContext,
    ) -> Result<SaveGrantRecordOutcome, SaveGrantRecordError> {
        #[cfg(test)]
        if let Some(behavior) = &self.test_behavior {
            behavior.grant_save_attempts.fetch_add(1, Ordering::Relaxed);
            if let Some(error) = &behavior.grant_save_error {
                return Err(SaveGrantRecordError::NotApplied(error.clone()));
            }
            if let Some(error) = &behavior.grant_save_result_unknown {
                return Err(SaveGrantRecordError::ResultUnknown(error.clone()));
            }
        }
        let Some(pool) = &self.pool else {
            return Err(SaveGrantRecordError::NotApplied(
                "database not enabled".to_string(),
            ));
        };

        let json = serialize_player_data(data).map_err(SaveGrantRecordError::NotApplied)?;
        let items_json = serde_json::to_value(items)
            .map_err(|error| SaveGrantRecordError::NotApplied(error.to_string()))?;
        let result_json = serde_json::to_value(result_summary)
            .map_err(|error| SaveGrantRecordError::NotApplied(error.to_string()))?;
        let mut tx = pool
            .begin()
            .await
            .map_err(|error| SaveGrantRecordError::NotApplied(error.to_string()))?;

        let inserted_at_ms = match sqlx::query_scalar::<_, i64>(
            r#"INSERT INTO character_inventory_grants (
                request_id,
                character_id,
                source,
                items_json,
                reason,
                request_fingerprint,
                result_json
            ) VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (request_id) DO NOTHING
            RETURNING (extract(epoch from created_at) * 1000)::bigint"#,
        )
        .bind(request_id)
        .bind(character_id)
        .bind(source)
        .bind(items_json)
        .bind(reason)
        .bind(request_fingerprint)
        .bind(&result_json)
        .fetch_optional(&mut *tx)
        .await
        {
            Ok(created_at_ms) => created_at_ms,
            Err(error) => {
                let _ = tx.rollback().await;
                return Err(SaveGrantRecordError::NotApplied(error.to_string()));
            }
        };

        let Some(created_at_ms) = inserted_at_ms else {
            let existing = find_grant_record_with_executor(&mut *tx, request_id)
                .await
                .map_err(SaveGrantRecordError::ResultUnknown)?;
            tx.rollback()
                .await
                .map_err(|error| SaveGrantRecordError::ResultUnknown(error.to_string()))?;
            return Ok(SaveGrantRecordOutcome::Existing(existing));
        };

        let generic_request_inserted = sqlx::query_scalar::<_, i64>(
            r#"INSERT INTO character_asset_requests (
                request_id,
                character_id,
                request_fingerprint,
                result_json
            ) VALUES ($1, $2, $3, $4)
            ON CONFLICT (request_id) DO NOTHING
            RETURNING (extract(epoch from created_at) * 1000)::bigint"#,
        )
        .bind(request_id)
        .bind(character_id)
        .bind(request_fingerprint)
        .bind(&result_json)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| SaveGrantRecordError::NotApplied(error.to_string()))?;

        if generic_request_inserted.is_none() {
            let existing = find_asset_request_with_executor(&mut *tx, request_id)
                .await
                .map_err(SaveGrantRecordError::ResultUnknown)?;
            tx.rollback()
                .await
                .map_err(|error| SaveGrantRecordError::ResultUnknown(error.to_string()))?;
            return Ok(SaveGrantRecordOutcome::Existing(existing));
        }

        let Some(revision) = save_snapshot_with_revision(
            &mut tx,
            character_id,
            data.persistence_revision(),
            data.get_hp(),
            &json,
        )
        .await
        .map_err(|error| SaveGrantRecordError::NotApplied(error.to_string()))?
        else {
            let _ = tx.rollback().await;
            return Err(SaveGrantRecordError::NotApplied(
                "character inventory revision conflict".to_string(),
            ));
        };

        insert_grant_ledger_entries(
            &mut tx,
            character_id,
            request_id,
            request_fingerprint,
            source,
            reason,
            revision,
            data,
            items,
            &ledger_context.normalized(source, request_id),
        )
        .await
        .map_err(|error| SaveGrantRecordError::NotApplied(error.to_string()))?;

        tx.commit()
            .await
            .map_err(|error| SaveGrantRecordError::ResultUnknown(error.to_string()))?;
        info!(character_id = %character_id, request_id = %request_id, "character inventory grant saved");
        Ok(SaveGrantRecordOutcome::Applied(GrantRecord {
            request_id: request_id.to_string(),
            character_id: character_id.to_string(),
            request_fingerprint: request_fingerprint.to_string(),
            result_summary: result_summary.clone(),
            created_at_ms,
        }))
    }

    pub async fn find_grant_record(&self, request_id: &str) -> Result<GrantRecordLookup, String> {
        #[cfg(test)]
        if self.test_behavior.is_some() {
            return Ok(GrantRecordLookup::NotFound);
        }
        let Some(pool) = &self.pool else {
            return Err("database not enabled".to_string());
        };
        match find_grant_record_with_executor(pool, request_id).await? {
            GrantRecordLookup::NotFound => find_asset_request_with_executor(pool, request_id).await,
            existing => Ok(existing),
        }
    }

    pub async fn load(&self, character_id: &str) -> Result<Option<PlayerData>, String> {
        #[cfg(test)]
        if let Some(behavior) = &self.test_behavior {
            return match &behavior.load_error {
                Some(error) => Err(error.clone()),
                None => Ok(None),
            };
        }
        let Some(pool) = &self.pool else {
            return Err("database not enabled".to_string());
        };

        let row = sqlx::query_as::<_, CharacterInventoryRow>(
            r#"SELECT
                hp,
                inventory_data,
                warehouse_data,
                equipment_data,
                attr_base_data,
                visual_data,
                buffs_data,
                progress_data,
                asset_revision
            FROM character_inventory
            WHERE character_id = $1"#,
        )
        .bind(character_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;

        row.map(|row| row.into_player_data(character_id))
            .transpose()
    }

    pub async fn delete(&self, character_id: &str) -> Result<(), String> {
        let Some(pool) = &self.pool else {
            return Err("database not enabled".to_string());
        };

        sqlx::query(r#"DELETE FROM character_inventory WHERE character_id = $1"#)
            .bind(character_id)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;

        info!(character_id = %character_id, "character inventory deleted");
        Ok(())
    }
}

struct SerializedPlayerData {
    inventory: serde_json::Value,
    warehouse: serde_json::Value,
    equipment: serde_json::Value,
    attr_base: serde_json::Value,
    visual: serde_json::Value,
    buffs: serde_json::Value,
    progress: serde_json::Value,
}

fn serialize_player_data(data: &PlayerData) -> Result<SerializedPlayerData, String> {
    Ok(SerializedPlayerData {
        inventory: serde_json::to_value(&data.inventory).map_err(|e| e.to_string())?,
        warehouse: serde_json::to_value(&data.warehouse).map_err(|e| e.to_string())?,
        equipment: serde_json::to_value(&data.equipment).map_err(|e| e.to_string())?,
        attr_base: serde_json::to_value(&data.attr.base).map_err(|e| e.to_string())?,
        visual: serde_json::to_value(&data.visual).map_err(|e| e.to_string())?,
        buffs: serde_json::to_value(&data.buffs).map_err(|e| e.to_string())?,
        progress: serde_json::to_value(&data.progress).map_err(|e| e.to_string())?,
    })
}

/// Insert the first snapshot or conditionally replace the exact revision that the caller read.
/// `None` is a definite optimistic-concurrency conflict and never a successful save.
async fn save_snapshot_with_revision(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    character_id: &str,
    expected_revision: u64,
    hp: i64,
    json: &SerializedPlayerData,
) -> Result<Option<u64>, sqlx::Error> {
    if expected_revision == 0 {
        let inserted = sqlx::query_scalar::<_, i64>(
            r#"INSERT INTO character_inventory (
                character_id,
                hp,
                inventory_data,
                warehouse_data,
                equipment_data,
                attr_base_data,
                visual_data,
                buffs_data,
                progress_data,
                asset_revision
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 1)
            ON CONFLICT (character_id) DO NOTHING
            RETURNING asset_revision"#,
        )
        .bind(character_id)
        .bind(hp)
        .bind(&json.inventory)
        .bind(&json.warehouse)
        .bind(&json.equipment)
        .bind(&json.attr_base)
        .bind(&json.visual)
        .bind(&json.buffs)
        .bind(&json.progress)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(revision) = inserted {
            return Ok(u64::try_from(revision).ok());
        }
    }

    let revision = sqlx::query_scalar::<_, i64>(
        r#"UPDATE character_inventory
        SET
            hp = $1,
            inventory_data = $2,
            warehouse_data = $3,
            equipment_data = $4,
            attr_base_data = $5,
            visual_data = $6,
            buffs_data = $7,
            progress_data = $8,
            asset_revision = asset_revision + 1,
            updated_at = current_timestamp
        WHERE character_id = $9 AND asset_revision = $10
        RETURNING asset_revision"#,
    )
    .bind(hp)
    .bind(&json.inventory)
    .bind(&json.warehouse)
    .bind(&json.equipment)
    .bind(&json.attr_base)
    .bind(&json.visual)
    .bind(&json.buffs)
    .bind(&json.progress)
    .bind(character_id)
    .bind(i64::try_from(expected_revision).unwrap_or(i64::MAX))
    .fetch_optional(&mut **tx)
    .await?;

    Ok(revision.and_then(|value| u64::try_from(value).ok()))
}

/// The compatibility grant record and this ledger are written in the same transaction as the
/// conditional JSONB snapshot update. Stage nine will expose query and audit surfaces over it.
async fn insert_grant_ledger_entries(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    character_id: &str,
    request_id: &str,
    request_fingerprint: &str,
    source: &str,
    reason: &str,
    revision: u64,
    after: &PlayerData,
    granted_items: &[Item],
    ledger_context: &AssetLedgerContext,
) -> Result<(), sqlx::Error> {
    use std::collections::BTreeMap;

    let mut deltas = BTreeMap::<(i32, bool, Option<String>, String), i64>::new();
    for item in granted_items {
        let delta = deltas
            .entry((
                item.item_id,
                item.binded,
                item.bound_character_id.clone(),
                ledger_rule_version(item),
            ))
            .or_default();
        *delta = delta.saturating_add(i64::from(item.count));
    }

    for ((item_id, binded, bound_character_id, rule_version), delta) in deltas {
        let quantity_after = after
            .get_inventory_items()
            .into_iter()
            .filter(|item| {
                item.item_id == item_id
                    && item.binded == binded
                    && item.bound_character_id == bound_character_id
            })
            .map(|item| i64::from(item.count))
            .sum::<i64>();
        let quantity_before = quantity_after.saturating_sub(delta);
        let binding_json = serde_json::json!({
            "binded": binded,
            "bound_character_id": bound_character_id,
        });

        sqlx::query(
            r#"INSERT INTO character_asset_ledger (
                request_id,
                character_id,
                request_fingerprint,
                asset_type,
                item_id,
                binding_json,
                quantity_before,
                quantity_after,
                quantity_delta,
                container,
                source,
                reason,
                snapshot_revision,
                origin_type,
                origin_id,
                delivery_method,
                delivery_id,
                mail_id,
                fallback_reason,
                rule_version,
                operator_id
            ) VALUES ($1, $2, $3, 'item', $4, $5, $6, $7, $8, 'inventory', $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)"#,
        )
        .bind(request_id)
        .bind(character_id)
        .bind(request_fingerprint)
        .bind(item_id)
        .bind(binding_json)
        .bind(quantity_before)
        .bind(quantity_after)
        .bind(delta)
        .bind(source)
        .bind(reason)
        .bind(i64::try_from(revision).unwrap_or(i64::MAX))
        .bind(&ledger_context.origin_type)
        .bind(&ledger_context.origin_id)
        .bind(&ledger_context.delivery_method)
        .bind(&ledger_context.delivery_id)
        .bind(&ledger_context.mail_id)
        .bind(&ledger_context.fallback_reason)
        .bind(rule_version)
        .bind(&ledger_context.operator_id)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

fn ledger_rule_version(item: &Item) -> String {
    serde_json::to_string(&item.config_version)
        .unwrap_or_else(|_| "\"unavailable\"".to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PlayerAssetQuantityKey {
    item_id: i32,
    binded: bool,
    bound_character_id: Option<String>,
    container: &'static str,
    rule_version: String,
}

fn player_asset_quantities(data: &PlayerData) -> std::collections::BTreeMap<PlayerAssetQuantityKey, i64> {
    let mut quantities = std::collections::BTreeMap::new();
    record_player_asset_quantities(&mut quantities, "inventory", data.get_inventory_items());
    record_player_asset_quantities(&mut quantities, "warehouse", data.get_warehouse_items());
    for (_, item) in data.get_equipped_items() {
        record_player_asset_quantities(&mut quantities, "equipment", vec![item]);
    }
    quantities
}

fn record_player_asset_quantities(
    quantities: &mut std::collections::BTreeMap<PlayerAssetQuantityKey, i64>,
    container: &'static str,
    items: Vec<&Item>,
) {
    for item in items {
        let key = PlayerAssetQuantityKey {
            item_id: item.item_id,
            binded: item.binded,
            bound_character_id: item.bound_character_id.clone(),
            container,
            rule_version: ledger_rule_version(item),
        };
        *quantities.entry(key).or_default() += i64::from(item.count);
    }
}

async fn insert_player_mutation_ledger_entries(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    character_id: &str,
    request_id: &str,
    source: &str,
    reason: &str,
    revision: u64,
    before: &PlayerData,
    after: &PlayerData,
    ledger_context: &AssetLedgerContext,
) -> Result<(), sqlx::Error> {
    let before_quantities = player_asset_quantities(before);
    let after_quantities = player_asset_quantities(after);
    let keys = before_quantities
        .keys()
        .chain(after_quantities.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();

    for key in keys {
        let quantity_before = before_quantities.get(&key).copied().unwrap_or_default();
        let quantity_after = after_quantities.get(&key).copied().unwrap_or_default();
        let quantity_delta = quantity_after.saturating_sub(quantity_before);
        if quantity_delta == 0 {
            continue;
        }
        let binding_json = serde_json::json!({
            "binded": key.binded,
            "bound_character_id": key.bound_character_id.clone(),
        });
        sqlx::query(
            r#"INSERT INTO character_asset_ledger (
                request_id,
                character_id,
                request_fingerprint,
                asset_type,
                item_id,
                binding_json,
                quantity_before,
                quantity_after,
                quantity_delta,
                container,
                source,
                reason,
                snapshot_revision,
                origin_type,
                origin_id,
                delivery_method,
                delivery_id,
                mail_id,
                fallback_reason,
                rule_version,
                operator_id
            ) VALUES ($1, $2, 'sha256:player-operation', 'item', $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)"#,
        )
        .bind(request_id)
        .bind(character_id)
        .bind(key.item_id)
        .bind(binding_json)
        .bind(quantity_before)
        .bind(quantity_after)
        .bind(quantity_delta)
        .bind(key.container)
        .bind(source)
        .bind(reason)
        .bind(i64::try_from(revision).unwrap_or(i64::MAX))
        .bind(&ledger_context.origin_type)
        .bind(&ledger_context.origin_id)
        .bind(&ledger_context.delivery_method)
        .bind(&ledger_context.delivery_id)
        .bind(&ledger_context.mail_id)
        .bind(&ledger_context.fallback_reason)
        .bind(&key.rule_version)
        .bind(&ledger_context.operator_id)
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

#[derive(sqlx::FromRow)]
struct CharacterInventoryRow {
    hp: i64,
    inventory_data: serde_json::Value,
    warehouse_data: serde_json::Value,
    equipment_data: serde_json::Value,
    attr_base_data: serde_json::Value,
    visual_data: serde_json::Value,
    buffs_data: serde_json::Value,
    progress_data: serde_json::Value,
    asset_revision: i64,
}

impl CharacterInventoryRow {
    fn into_player_data(self, character_id: &str) -> Result<PlayerData, String> {
        let inventory: ItemContainer =
            serde_json::from_value(self.inventory_data).map_err(|e| e.to_string())?;
        let warehouse: ItemContainer =
            serde_json::from_value(self.warehouse_data).map_err(|e| e.to_string())?;
        let equipment: EquipmentSlots =
            serde_json::from_value(self.equipment_data).map_err(|e| e.to_string())?;
        let attr_base: AttrPanel =
            serde_json::from_value(self.attr_base_data).map_err(|e| e.to_string())?;
        let visual: PlayerVisual =
            serde_json::from_value(self.visual_data).map_err(|e| e.to_string())?;
        let buffs: Vec<Buff> =
            serde_json::from_value(self.buffs_data).map_err(|e| e.to_string())?;
        let progress = serde_json::from_value(self.progress_data).map_err(|e| e.to_string())?;

        let mut attr = PlayerAttr::new();
        attr.set_base(attr_base);

        let mut player_data = PlayerData::with_capacity(
            character_id.to_string(),
            inventory.capacity(),
            warehouse.capacity(),
        );
        player_data.inventory = inventory;
        player_data.warehouse = warehouse;
        player_data.equipment = equipment;
        player_data.attr = attr;
        player_data.visual = visual;
        player_data.buffs = buffs;
        player_data.progress = progress;
        player_data.set_hp(self.hp);
        player_data.set_persistence_revision(
            u64::try_from(self.asset_revision).map_err(|_| "invalid asset revision".to_string())?,
        );

        player_data.clear_attr_dirty();
        player_data.clear_visual_dirty();
        player_data.clear_data_dirty();

        info!(character_id = %character_id, "character inventory loaded");
        Ok(player_data)
    }
}

#[derive(sqlx::FromRow)]
struct GrantRecordRow {
    request_id: String,
    character_id: String,
    request_fingerprint: Option<String>,
    result_json: Option<serde_json::Value>,
    created_at_ms: i64,
}

impl GrantRecordRow {
    fn into_lookup(self) -> GrantRecordLookup {
        let (Some(request_fingerprint), Some(result_json)) =
            (self.request_fingerprint, self.result_json)
        else {
            return GrantRecordLookup::ResultUnavailable;
        };
        let Ok(result_summary) = serde_json::from_value(result_json) else {
            return GrantRecordLookup::ResultUnavailable;
        };
        GrantRecordLookup::Succeeded(GrantRecord {
            request_id: self.request_id,
            character_id: self.character_id,
            request_fingerprint,
            result_summary,
            created_at_ms: self.created_at_ms,
        })
    }
}

async fn find_grant_record_with_executor<'e, E>(
    executor: E,
    request_id: &str,
) -> Result<GrantRecordLookup, String>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let row = sqlx::query_as::<_, GrantRecordRow>(
        r#"SELECT
            request_id,
            character_id,
            request_fingerprint,
            result_json,
            (extract(epoch from created_at) * 1000)::bigint AS created_at_ms
        FROM character_inventory_grants
        WHERE request_id = $1"#,
    )
    .bind(request_id)
    .fetch_optional(executor)
    .await
    .map_err(|error| error.to_string())?;

    Ok(row.map_or(GrantRecordLookup::NotFound, GrantRecordRow::into_lookup))
}

/// New transaction participants share this request namespace. Existing grant rows remain the
/// first lookup during rolling upgrade, so mail-service and GM callers retain their v1 contract.
async fn find_asset_request_with_executor<'e, E>(
    executor: E,
    request_id: &str,
) -> Result<GrantRecordLookup, String>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let row = sqlx::query_as::<_, GrantRecordRow>(
        r#"SELECT
            request_id,
            character_id,
            request_fingerprint,
            result_json,
            (extract(epoch from created_at) * 1000)::bigint AS created_at_ms
        FROM character_asset_requests
        WHERE request_id = $1"#,
    )
    .bind(request_id)
    .fetch_optional(executor)
    .await
    .map_err(|error| error.to_string())?;

    Ok(row.map_or(GrantRecordLookup::NotFound, GrantRecordRow::into_lookup))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_asset_quantity_snapshot_preserves_container_before_after_for_moves() {
        let mut before = PlayerData::new("chr_ledger".to_string());
        before.inventory.add_item(Item::new(1, 1001, 2, false)).unwrap();
        let mut after = before.clone();
        let moved = after.inventory.remove_item(1, 2).unwrap();
        after.warehouse.add_item(moved).unwrap();

        let before_quantities = player_asset_quantities(&before);
        let after_quantities = player_asset_quantities(&after);
        let inventory = before_quantities
            .iter()
            .find(|(key, _)| key.item_id == 1001 && key.container == "inventory")
            .unwrap();
        let warehouse = after_quantities
            .iter()
            .find(|(key, _)| key.item_id == 1001 && key.container == "warehouse")
            .unwrap();

        assert_eq!(*inventory.1, 2);
        assert_eq!(after_quantities.get(inventory.0).copied().unwrap_or_default(), 0);
        assert_eq!(before_quantities.get(warehouse.0).copied().unwrap_or_default(), 0);
        assert_eq!(*warehouse.1, 2);
    }
}
