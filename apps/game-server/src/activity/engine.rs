use super::cache::ActivityCache;
use super::domain::ActivityStatus;
use super::repository::{
    ActivityRepository, InMemoryActivityRepository, PgActivityRepository, PublishedActivitySnapshot,
};
use super::settlement::{
    ActivityClaimCoordinator, ActivityRewardDelivery, ClaimStatus, PgActivityClaimStore,
    build_reward_order,
};
use super::types::{
    ActivityTypeRegistry, GameEntryEvent, InMemoryLoginRewardProgressRepository, LoginRewardConfig,
    LoginRewardProgressError, LoginRewardProgressRepository, LoginRewardProgressResult,
    LotteryConfig, LotteryDrawCost, LotterySelection, LotteryState,
    PgLoginRewardProgressRepository, PlayerContext, TransactionContext, apply_game_entry,
    build_lottery_voucher_exchange, draw_lottery_item, eligible_stage_numbers,
    evaluate_lottery_draw, login_reward_claim_key, lottery_period_key, normalize_lottery_state,
};
use crate::core::config_table::ConfigTableRuntime;
use crate::core::global_id::ItemUidGenerator;
use crate::core::inventory::{
    AssetBinding, AssetOperation, AssetRequestFingerprint, AssetResultState, Item, ItemError,
    NormalizedAssetItem, PlayerManagerRewardInventoryPort, RewardDeliveryPolicy,
    RewardInventoryPort,
};
use crate::core::player::db_player_store::AssetCommandRecordLookup;
use crate::core::player::player_manager::PlayerManager;
use crate::metrics::{ActivityActionMetric, ActivityCacheFailureMetric, METRICS, MetricsCollector};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LotteryVoucher {
    pub(crate) item_id: i32,
    pub(crate) asset_uid: u64,
    pub(crate) quantity: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LotteryAssetApplyError {
    Unknown,
    RetryableNotApplied,
    PermanentNotApplied,
}

/// The real implementation must resolve the voucher from the authoritative
/// inventory and execute the supplied all-or-nothing exchange. Tests and local
/// harnesses can install a fake implementation through the builder below.
pub(crate) trait LotteryAssetGateway: Send + Sync {
    fn find_voucher<'a>(
        &'a self,
        character_id: &'a str,
        item_id: i32,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<LotteryVoucher>, String>> + Send + 'a>,
    >;
    fn apply_draw<'a>(
        &'a self,
        character_id: &'a str,
        request_id: &'a str,
        exchange: Option<crate::core::reward_source::InventoryRequiredExchange>,
        reward_order: &'a crate::core::inventory::RewardOrder,
        selection: &'a LotterySelection,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<AssetResultState, LotteryAssetApplyError>>
                + Send
                + 'a,
        >,
    >;
    fn query_draw<'a>(
        &'a self,
        request_id: &'a str,
        request_fingerprint: Option<&'a str>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<AssetResultState>, String>> + Send + 'a>,
    >;
}

struct UnavailableLotteryAssetGateway;

impl LotteryAssetGateway for UnavailableLotteryAssetGateway {
    fn find_voucher<'a>(
        &'a self,
        _character_id: &'a str,
        _item_id: i32,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<LotteryVoucher>, String>> + Send + 'a>,
    > {
        Box::pin(async { Err("lottery inventory lookup is unavailable".into()) })
    }

    fn apply_draw<'a>(
        &'a self,
        _character_id: &'a str,
        _request_id: &'a str,
        _exchange: Option<crate::core::reward_source::InventoryRequiredExchange>,
        _reward_order: &'a crate::core::inventory::RewardOrder,
        _selection: &'a LotterySelection,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<AssetResultState, LotteryAssetApplyError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async { Err(LotteryAssetApplyError::Unknown) })
    }

    fn query_draw<'a>(
        &'a self,
        _request_id: &'a str,
        _request_fingerprint: Option<&'a str>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<AssetResultState>, String>> + Send + 'a>,
    > {
        Box::pin(async { Err("lottery asset query is unavailable".into()) })
    }
}

#[derive(Clone)]
pub(crate) struct PlayerManagerLotteryAssetGateway {
    player_manager: PlayerManager,
    inventory: PlayerManagerRewardInventoryPort,
    config_tables: ConfigTableRuntime,
    item_uid_generator: ItemUidGenerator,
    reward_delivery: Arc<dyn ActivityRewardDelivery>,
}

impl PlayerManagerLotteryAssetGateway {
    pub(crate) fn new(
        player_manager: PlayerManager,
        inventory: PlayerManagerRewardInventoryPort,
        config_tables: ConfigTableRuntime,
        item_uid_generator: ItemUidGenerator,
        reward_delivery: Arc<dyn ActivityRewardDelivery>,
    ) -> Self {
        Self {
            player_manager,
            inventory,
            config_tables,
            item_uid_generator,
            reward_delivery,
        }
    }

    fn exchange_items(
        &self,
        character_id: &str,
        exchange: &crate::core::reward_source::InventoryRequiredExchange,
    ) -> Result<Vec<Item>, String> {
        let grants = match exchange.command.operations.as_slice() {
            [
                AssetOperation::Consume { .. },
                AssetOperation::Grant { items },
            ] => items,
            _ => return Err("lottery exchange operation contract is invalid".to_string()),
        };
        let item_table = self
            .config_tables
            .current_snapshot()
            .tables
            .item_table
            .clone();
        grants
            .iter()
            .map(|intent| {
                let row = item_table
                    .get(intent.item_id)
                    .ok_or_else(|| "lottery reward item config is unavailable".to_string())?;
                let binded = matches!(intent.binding, AssetBinding::CharacterBound { .. });
                let item = Item::from_config(
                    self.item_uid_generator
                        .next()
                        .map_err(|_| "lottery reward uid allocation failed".to_string())?,
                    intent.item_id,
                    intent.count,
                    binded,
                    Some(character_id),
                    row,
                    item_table.as_ref(),
                );
                if AssetBinding::from_item(&item) != intent.binding {
                    return Err("lottery reward binding does not match item config".to_string());
                }
                Ok(item)
            })
            .collect()
    }
}

impl LotteryAssetGateway for PlayerManagerLotteryAssetGateway {
    fn find_voucher<'a>(
        &'a self,
        character_id: &'a str,
        item_id: i32,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<LotteryVoucher>, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            self.player_manager
                .find_inventory_item_by_config(character_id, item_id)
                .await
                .map(|item| {
                    item.map(|item| LotteryVoucher {
                        item_id: item.item_id,
                        asset_uid: item.uid,
                        quantity: item.count,
                    })
                })
                .map_err(|error| error.error_code.to_string())
        })
    }

    fn apply_draw<'a>(
        &'a self,
        character_id: &'a str,
        _request_id: &'a str,
        exchange: Option<crate::core::reward_source::InventoryRequiredExchange>,
        reward_order: &'a crate::core::inventory::RewardOrder,
        _selection: &'a LotterySelection,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<AssetResultState, LotteryAssetApplyError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            if let Some(exchange) = exchange {
                let item_table = self
                    .config_tables
                    .current_snapshot()
                    .tables
                    .item_table
                    .clone();
                let produced_items = self
                    .exchange_items(character_id, &exchange)
                    .map_err(|_| LotteryAssetApplyError::PermanentNotApplied)?;
                let split_uid_generator = self.item_uid_generator.clone();
                return match self
                    .player_manager
                    .execute_inventory_required_exchange(
                        &exchange,
                        produced_items,
                        item_table.as_ref(),
                        move || split_uid_generator.next().map_err(|_| ItemError::Unknown),
                    )
                    .await
                {
                    Ok(result) => Ok(result.result_state),
                    Err(error) if error.result_state == "unknown" => {
                        Err(LotteryAssetApplyError::Unknown)
                    }
                    Err(error) if error.result_state == "not_applied" && error.retryable => {
                        Err(LotteryAssetApplyError::RetryableNotApplied)
                    }
                    Err(error) if error.result_state == "not_applied" => {
                        Err(LotteryAssetApplyError::PermanentNotApplied)
                    }
                    Err(_) => Err(LotteryAssetApplyError::Unknown),
                };
            }
            self.reward_delivery
                .deliver(reward_order.clone())
                .await
                .map(|outcome| outcome.result.result_state)
                .map_err(|_| LotteryAssetApplyError::Unknown)
        })
    }

    fn query_draw<'a>(
        &'a self,
        request_id: &'a str,
        request_fingerprint: Option<&'a str>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<AssetResultState>, String>> + Send + 'a>,
    > {
        Box::pin(async move {
            let fingerprint = request_fingerprint
                .ok_or_else(|| "lottery asset fingerprint is missing".to_string())?;
            let fingerprint = AssetRequestFingerprint::parse(fingerprint.to_string())
                .map_err(|_| "lottery asset fingerprint is invalid".to_string())?;
            if request_id.starts_with("exchange:") {
                return match self.player_manager.query_asset_command(request_id).await? {
                    AssetCommandRecordLookup::NotFound => Ok(None),
                    AssetCommandRecordLookup::ResultUnavailable => {
                        Ok(Some(AssetResultState::Unknown))
                    }
                    AssetCommandRecordLookup::Succeeded(result) => {
                        if result.request_fingerprint != fingerprint {
                            Ok(Some(AssetResultState::NotApplied))
                        } else {
                            Ok(Some(result.result_state))
                        }
                    }
                };
            }
            self.inventory
                .query_reward(request_id, &fingerprint)
                .await
                .map(|result| result.map(|result| result.result_state))
                .map_err(|error| error.message)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LotteryDrawStatus {
    Processing,
    Granted,
    RetryableFailure,
    ReconciliationPending,
    ManualReview,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct LotteryDrawRecord {
    character_id: String,
    activity_id: String,
    version: i32,
    semantic_key: String,
    draw_request_id: String,
    status: LotteryDrawStatus,
    previous_state: LotteryState,
    next_state: LotteryState,
    selection: LotterySelection,
    exchange: Option<crate::core::reward_source::InventoryRequiredExchange>,
    reward_order: crate::core::inventory::RewardOrder,
    reward_request_id: String,
    reward_fingerprint: Option<String>,
    asset_request_id: String,
    asset_fingerprint: String,
    notification_failed: bool,
}

fn lottery_identity_key(character_id: &str, activity_id: &str, version: i64) -> String {
    format!(
        "{}:{character_id}:{}:{activity_id}:{version}",
        character_id.len(),
        activity_id.len()
    )
}

fn lottery_record_key(
    character_id: &str,
    activity_id: &str,
    version: i64,
    client_request_id: &str,
) -> String {
    let identity = lottery_identity_key(character_id, activity_id, version);
    let digest = Sha256::digest(format!(
        "{}:{identity}:{}:{client_request_id}",
        identity.len(),
        client_request_id.len()
    ));
    format!("activity-lottery:sha256:{digest:x}")
}

type LotteryStoreFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone)]
enum LotteryRecordCreate {
    Created,
    Existing(LotteryDrawRecord),
    Busy,
    Conflict,
    ManualReview,
}

trait LotteryRuntimeStore: Send + Sync {
    fn state<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
    ) -> LotteryStoreFuture<'a, Result<LotteryState, String>>;
    fn record<'a>(
        &'a self,
        record_key: &'a str,
    ) -> LotteryStoreFuture<'a, Result<Option<LotteryDrawRecord>, String>>;
    fn create<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        record_key: &'a str,
        record: LotteryDrawRecord,
    ) -> LotteryStoreFuture<'a, Result<LotteryRecordCreate, String>>;
    fn save<'a>(
        &'a self,
        record_key: &'a str,
        record: LotteryDrawRecord,
    ) -> LotteryStoreFuture<'a, Result<(), String>>;
    fn grant<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        record_key: &'a str,
        record: LotteryDrawRecord,
    ) -> LotteryStoreFuture<'a, Result<(), String>>;
}

#[derive(Clone, Default)]
struct InMemoryLotteryRuntimeStore {
    state: Arc<Mutex<InMemoryLotteryRuntimeState>>,
}

#[derive(Default)]
struct InMemoryLotteryRuntimeState {
    states: HashMap<String, LotteryState>,
    records: HashMap<String, LotteryDrawRecord>,
    active: HashMap<String, String>,
}

impl LotteryRuntimeStore for InMemoryLotteryRuntimeStore {
    fn state<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
    ) -> LotteryStoreFuture<'a, Result<LotteryState, String>> {
        Box::pin(async move {
            let state_key = format!("{character_id}\0{activity_id}\0{version}");
            Ok(self
                .state
                .lock()
                .await
                .states
                .get(&state_key)
                .cloned()
                .unwrap_or_default())
        })
    }

    fn record<'a>(
        &'a self,
        record_key: &'a str,
    ) -> LotteryStoreFuture<'a, Result<Option<LotteryDrawRecord>, String>> {
        Box::pin(async move { Ok(self.state.lock().await.records.get(record_key).cloned()) })
    }

    fn create<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        record_key: &'a str,
        record: LotteryDrawRecord,
    ) -> LotteryStoreFuture<'a, Result<LotteryRecordCreate, String>> {
        Box::pin(async move {
            let state_key = format!("{character_id}\0{activity_id}\0{version}");
            let mut state = self.state.lock().await;
            if let Some(existing) = state.records.get(record_key).cloned() {
                return Ok(LotteryRecordCreate::Existing(existing));
            }
            if state.records.iter().any(|(existing_key, existing)| {
                existing.character_id == character_id
                    && existing.draw_request_id == record.draw_request_id
                    && existing_key.as_str() != record_key
            }) {
                return Ok(LotteryRecordCreate::Conflict);
            }
            if state.active.contains_key(&state_key) {
                return Ok(LotteryRecordCreate::Busy);
            }
            if state.states.get(&state_key).cloned().unwrap_or_default() != record.previous_state {
                return Ok(LotteryRecordCreate::Busy);
            }
            state.active.insert(state_key, record_key.to_string());
            state.records.insert(record_key.to_string(), record);
            Ok(LotteryRecordCreate::Created)
        })
    }

    fn save<'a>(
        &'a self,
        record_key: &'a str,
        record: LotteryDrawRecord,
    ) -> LotteryStoreFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.state
                .lock()
                .await
                .records
                .insert(record_key.to_string(), record);
            Ok(())
        })
    }

    fn grant<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        record_key: &'a str,
        record: LotteryDrawRecord,
    ) -> LotteryStoreFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let state_key = format!("{character_id}\0{activity_id}\0{version}");
            let mut state = self.state.lock().await;
            state
                .states
                .insert(state_key.clone(), record.next_state.clone());
            state.records.insert(record_key.to_string(), record);
            state.active.remove(&state_key);
            Ok(())
        })
    }
}

#[derive(Clone)]
struct PgLotteryRuntimeStore {
    pool: PgPool,
}

impl PgLotteryRuntimeStore {
    fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    fn status(status: LotteryDrawStatus) -> &'static str {
        match status {
            LotteryDrawStatus::Processing => "processing",
            LotteryDrawStatus::Granted => "granted",
            LotteryDrawStatus::RetryableFailure => "retryable_failure",
            LotteryDrawStatus::ReconciliationPending => "reconciliation_pending",
            LotteryDrawStatus::ManualReview => "manual_review",
        }
    }

    async fn load_record(&self, runtime_key: &str) -> Result<Option<LotteryDrawRecord>, String> {
        let snapshot = sqlx::query_scalar::<_, serde_json::Value>(
            r#"SELECT d.result_json
            FROM activity_claim_record c
            JOIN activity_draw_result d ON d.claim_id = c.id
            WHERE c.runtime_key = $1"#,
        )
        .bind(runtime_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| error.to_string())?;
        snapshot
            .map(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
            .transpose()
    }
}

impl LotteryRuntimeStore for PgLotteryRuntimeStore {
    fn state<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
    ) -> LotteryStoreFuture<'a, Result<LotteryState, String>> {
        Box::pin(async move {
            let value = sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT type_state_json FROM player_activity_state WHERE character_id = $1 AND activity_id = $2 AND version_no = $3",
            )
            .bind(character_id)
            .bind(activity_id)
            .bind(version)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| error.to_string())?;
            value
                .map(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
                .transpose()
                .map(|value| value.unwrap_or_default())
        })
    }

    fn record<'a>(
        &'a self,
        record_key: &'a str,
    ) -> LotteryStoreFuture<'a, Result<Option<LotteryDrawRecord>, String>> {
        Box::pin(async move { self.load_record(record_key).await })
    }

    fn create<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        record_key: &'a str,
        record: LotteryDrawRecord,
    ) -> LotteryStoreFuture<'a, Result<LotteryRecordCreate, String>> {
        Box::pin(async move {
            let reward_snapshot = serde_json::to_value(&record.reward_order.items)
                .map_err(|error| error.to_string())?;
            let cost_snapshot = match &record.exchange {
                Some(exchange) => serde_json::json!([exchange]),
                None => serde_json::json!([]),
            };
            let order_snapshot =
                serde_json::to_value(&record.reward_order).map_err(|error| error.to_string())?;
            let runtime_snapshot =
                serde_json::to_value(&record).map_err(|error| error.to_string())?;
            let mut transaction = self.pool.begin().await.map_err(|error| error.to_string())?;
            let identity_key = lottery_identity_key(character_id, activity_id, i64::from(version));
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(identity_key)
                .execute(&mut *transaction)
                .await
                .map_err(|error| error.to_string())?;

            let existing_snapshot = sqlx::query_scalar::<_, serde_json::Value>(
                r#"SELECT d.result_json
                FROM activity_claim_record c
                JOIN activity_draw_result d ON d.claim_id = c.id
                WHERE c.runtime_key = $1"#,
            )
            .bind(record_key)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
            if let Some(snapshot) = existing_snapshot {
                let existing =
                    serde_json::from_value(snapshot).map_err(|error| error.to_string())?;
                transaction
                    .commit()
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(LotteryRecordCreate::Existing(existing));
            }

            let active_exists = sqlx::query_scalar::<_, bool>(
                r#"SELECT EXISTS (
                    SELECT 1 FROM activity_claim_record
                    WHERE character_id = $1 AND activity_id = $2 AND version_no = $3
                      AND action_type = 'draw'
                      AND status IN ('processing', 'retryable_failure', 'reconciliation_pending')
                )"#,
            )
            .bind(character_id)
            .bind(activity_id)
            .bind(version)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
            let persisted_state: LotteryState = sqlx::query_scalar::<_, serde_json::Value>(
                "SELECT type_state_json FROM player_activity_state WHERE character_id = $1 AND activity_id = $2 AND version_no = $3",
            )
            .bind(character_id)
            .bind(activity_id)
            .bind(version)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?
            .map(|value| serde_json::from_value(value).map_err(|error| error.to_string()))
            .transpose()?
            .unwrap_or_default();
            if active_exists || persisted_state != record.previous_state {
                transaction
                    .commit()
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(LotteryRecordCreate::Busy);
            }

            let claim_id = sqlx::query_scalar::<_, i64>(
                r#"INSERT INTO activity_claim_record (
                    character_id, activity_id, version_no, activity_type, action_type,
                    period_key, semantic_claim_key, client_request_id, runtime_key,
                    status, reward_snapshot_json, cost_snapshot_json,
                    reward_request_id, order_snapshot_json, result_json,
                    notification_failed, attempt_count
                ) VALUES ($1, $2, $3, 'lottery', 'draw', $4, $5, $4, $6,
                    'processing', $7, $8, $9, $10, 'null'::jsonb, false, 1)
                ON CONFLICT DO NOTHING RETURNING id"#,
            )
            .bind(&record.character_id)
            .bind(&record.activity_id)
            .bind(record.version)
            .bind(&record.draw_request_id)
            .bind(&record.semantic_key)
            .bind(record_key)
            .bind(reward_snapshot)
            .bind(cost_snapshot)
            .bind(&record.reward_request_id)
            .bind(order_snapshot.clone())
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
            if let Some(claim_id) = claim_id {
                sqlx::query(
                    r#"INSERT INTO activity_draw_result (
                        claim_id, character_id, activity_id, version_no, reward_group_key,
                        pool_digest, random_algorithm_version, selected_item_id, result_json
                    ) VALUES ($1, $2, $3, $4, 'lottery_pool', $5, $6, $7, $8)"#,
                )
                .bind(claim_id)
                .bind(&record.character_id)
                .bind(&record.activity_id)
                .bind(record.version)
                .bind(&record.selection.metadata.weight_digest)
                .bind(&record.selection.metadata.random_algorithm_version)
                .bind(i64::from(record.selection.item_id))
                .bind(runtime_snapshot)
                .execute(&mut *transaction)
                .await
                .map_err(|error| error.to_string())?;
                transaction
                    .commit()
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(LotteryRecordCreate::Created);
            }
            let request_binding = sqlx::query_as::<_, (i64, String, i32, String, Option<String>)>(
                r#"SELECT id, activity_id, version_no, action_type, runtime_key
                FROM activity_claim_record
                WHERE character_id = $1 AND client_request_id = $2
                LIMIT 1"#,
            )
            .bind(character_id)
            .bind(&record.draw_request_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
            if let Some((claim_id, bound_activity, bound_version, bound_action, bound_runtime)) =
                request_binding
            {
                let is_same_binding = bound_activity == activity_id
                    && bound_version == version
                    && bound_action == "draw"
                    && bound_runtime.as_deref() == Some(record_key);
                if is_same_binding {
                    let snapshot = sqlx::query_scalar::<_, serde_json::Value>(
                        "SELECT result_json FROM activity_draw_result WHERE claim_id = $1",
                    )
                    .bind(claim_id)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(|error| error.to_string())?;
                    if let Some(snapshot) = snapshot {
                        let existing =
                            serde_json::from_value(snapshot).map_err(|error| error.to_string())?;
                        transaction
                            .commit()
                            .await
                            .map_err(|error| error.to_string())?;
                        return Ok(LotteryRecordCreate::Existing(existing));
                    }
                }

                let reason_code = if is_same_binding {
                    "ACTIVITY_LOTTERY_RESULT_MISSING"
                } else {
                    "REQUEST_FINGERPRINT_CONFLICT"
                };
                sqlx::query(
                    r#"INSERT INTO activity_claim_review (
                        character_id, activity_id, version_no, semantic_claim_key,
                        client_request_id, reason_code, order_snapshot_json
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                    ON CONFLICT DO NOTHING"#,
                )
                .bind(character_id)
                .bind(activity_id)
                .bind(version)
                .bind(&record.semantic_key)
                .bind(&record.draw_request_id)
                .bind(reason_code)
                .bind(order_snapshot)
                .execute(&mut *transaction)
                .await
                .map_err(|error| error.to_string())?;
                transaction
                    .commit()
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(if is_same_binding {
                    LotteryRecordCreate::ManualReview
                } else {
                    LotteryRecordCreate::Conflict
                });
            }

            transaction
                .commit()
                .await
                .map_err(|error| error.to_string())?;
            Ok(LotteryRecordCreate::Busy)
        })
    }

    fn save<'a>(
        &'a self,
        record_key: &'a str,
        record: LotteryDrawRecord,
    ) -> LotteryStoreFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let runtime_snapshot =
                serde_json::to_value(&record).map_err(|error| error.to_string())?;
            let changed = sqlx::query(
                r#"WITH updated_claim AS (
                    UPDATE activity_claim_record SET status = $2,
                        notification_failed = $3, updated_at = current_timestamp,
                        last_retry_at = CASE WHEN $2 = 'retryable_failure' THEN current_timestamp ELSE last_retry_at END
                    WHERE runtime_key = $1 RETURNING id
                )
                UPDATE activity_draw_result d SET result_json = $4
                FROM updated_claim c WHERE d.claim_id = c.id"#,
            )
            .bind(record_key)
            .bind(Self::status(record.status))
            .bind(record.notification_failed)
            .bind(runtime_snapshot)
            .execute(&self.pool)
            .await
            .map_err(|error| error.to_string())?;
            if changed.rows_affected() == 1 {
                Ok(())
            } else {
                Err("lottery draw record disappeared".to_string())
            }
        })
    }

    fn grant<'a>(
        &'a self,
        _character_id: &'a str,
        _activity_id: &'a str,
        _version: i32,
        record_key: &'a str,
        record: LotteryDrawRecord,
    ) -> LotteryStoreFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let progress = serde_json::json!({
                "daily_draw_count": record.next_state.daily_draw_count,
                "total_draw_count": record.next_state.total_draw_count,
                "last_draw_period_key": record.next_state.last_draw_period_key,
            });
            let state_snapshot =
                serde_json::to_value(&record.next_state).map_err(|error| error.to_string())?;
            let runtime_snapshot =
                serde_json::to_value(&record).map_err(|error| error.to_string())?;
            let result_snapshot = serde_json::json!({
                "selection": record.selection,
                "asset_request_id": record.asset_request_id,
                "asset_fingerprint": record.asset_fingerprint,
            });
            let mut transaction = self.pool.begin().await.map_err(|error| error.to_string())?;
            let identity_key = lottery_identity_key(
                &record.character_id,
                &record.activity_id,
                i64::from(record.version),
            );
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(identity_key)
                .execute(&mut *transaction)
                .await
                .map_err(|error| error.to_string())?;
            sqlx::query(
                r#"INSERT INTO player_activity_state (
                    character_id, activity_id, version_no, current_stage_id,
                    progress_json, type_state_json, state_revision, last_event_key
                ) VALUES ($1, $2, $3, $4, $5, $6, 1, $4)
                ON CONFLICT (character_id, activity_id, version_no) DO UPDATE SET
                    current_stage_id = EXCLUDED.current_stage_id,
                    progress_json = EXCLUDED.progress_json,
                    type_state_json = EXCLUDED.type_state_json,
                    state_revision = player_activity_state.state_revision + 1,
                    last_event_key = EXCLUDED.last_event_key,
                    updated_at = current_timestamp"#,
            )
            .bind(&record.character_id)
            .bind(&record.activity_id)
            .bind(record.version)
            .bind(&record.draw_request_id)
            .bind(progress)
            .bind(state_snapshot)
            .execute(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
            let claim_id = sqlx::query_scalar::<_, i64>(
                r#"UPDATE activity_claim_record SET status = 'granted', result_json = $2,
                    notification_failed = $3, completed_at = current_timestamp,
                    updated_at = current_timestamp
                WHERE runtime_key = $1 RETURNING id"#,
            )
            .bind(record_key)
            .bind(result_snapshot)
            .bind(record.notification_failed)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "lottery draw claim disappeared".to_string())?;
            sqlx::query("UPDATE activity_draw_result SET result_json = $2 WHERE claim_id = $1")
                .bind(claim_id)
                .bind(runtime_snapshot)
                .execute(&mut *transaction)
                .await
                .map_err(|error| error.to_string())?;
            transaction
                .commit()
                .await
                .map_err(|error| error.to_string())
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityEngineError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl ActivityEngineError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityActionRequest {
    pub(crate) activity_id: String,
    pub(crate) version: u32,
    pub(crate) stage_id: String,
    pub(crate) action_type: String,
    pub(crate) client_request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityActionResponse {
    pub(crate) ok: bool,
    pub(crate) error_code: Option<&'static str>,
    pub(crate) activity_id: String,
    pub(crate) version: u32,
    pub(crate) stage_id: String,
    pub(crate) action_type: String,
    pub(crate) client_request_id: String,
    pub(crate) processing: bool,
    pub(crate) duplicate: bool,
    pub(crate) state_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityRequestContext {
    pub(crate) character_id: String,
    pub(crate) account_player_id: Option<String>,
    pub(crate) source_ip: Option<String>,
    pub(crate) credential_id: Option<String>,
    pub(crate) device_subject: Option<String>,
}

impl ActivityRequestContext {
    pub(crate) fn character_only(character_id: &str) -> Self {
        Self {
            character_id: character_id.to_string(),
            account_player_id: None,
            source_ip: None,
            credential_id: None,
            device_subject: None,
        }
    }

    pub(crate) fn authenticated(
        character_id: &str,
        account_player_id: &str,
        peer_addr: &str,
        credential_id: Option<&str>,
        device_subject: Option<&str>,
    ) -> Self {
        let source_ip = peer_addr
            .parse::<SocketAddr>()
            .map(|address| address.ip().to_string())
            .unwrap_or_else(|_| peer_addr.trim().to_string());
        Self {
            character_id: character_id.to_string(),
            account_player_id: Some(account_player_id.to_string()),
            source_ip: (!source_ip.is_empty()).then_some(source_ip),
            credential_id: credential_id
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            device_subject: device_subject
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ActivityRateLimitPolicy {
    window: Duration,
    character_max: u64,
    account_max: u64,
    source_max: u64,
    credential_max: u64,
    device_max: u64,
    activity_max: u64,
    action_max: u64,
}

impl Default for ActivityRateLimitPolicy {
    fn default() -> Self {
        Self {
            window: Duration::from_millis(100),
            character_max: 16,
            account_max: 16,
            // game-server commonly sees the proxy/local-socket source rather than
            // the real client IP. Keep this dimension available but disabled here;
            // the public proxy owns the enforceable IP boundary.
            source_max: 0,
            credential_max: 16,
            device_max: 16,
            activity_max: 8,
            action_max: 1,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ActivityEngine {
    repository: Arc<dyn ActivityRepository>,
    registry: Arc<ActivityTypeRegistry>,
    request_state: Arc<Mutex<RequestState>>,
    login_progress: Arc<dyn LoginRewardProgressRepository>,
    enabled: bool,
    claim_coordinator: Option<ActivityClaimCoordinator>,
    lottery_states: Arc<dyn LotteryRuntimeStore>,
    lottery_assets: Arc<dyn LotteryAssetGateway>,
    lottery_notifier: Arc<dyn LotteryResultNotifier>,
    cache: Option<Arc<dyn ActivityCache>>,
    rate_limit_policy: ActivityRateLimitPolicy,
    metrics: &'static MetricsCollector,
}

pub(crate) trait LotteryResultNotifier: Send + Sync {
    fn notify<'a>(
        &'a self,
        record: &'a LotteryDrawRecord,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>;
}

struct NoopLotteryResultNotifier;

impl LotteryResultNotifier for NoopLotteryResultNotifier {
    fn notify<'a>(
        &'a self,
        _record: &'a LotteryDrawRecord,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone)]
struct SeenRequest {
    fingerprint: String,
    response: Option<ActivityActionResponse>,
    processing: bool,
}

struct RateLimitWindow {
    started_at: Instant,
    count: u64,
}

#[derive(Default)]
struct RequestState {
    seen: HashMap<String, SeenRequest>,
    rate_limits: HashMap<String, RateLimitWindow>,
}

impl ActivityEngine {
    pub(crate) fn new(repository: Arc<dyn ActivityRepository>) -> Self {
        Self {
            repository,
            registry: Arc::new(ActivityTypeRegistry::with_defaults()),
            request_state: Arc::new(Mutex::new(RequestState::default())),
            login_progress: Arc::new(InMemoryLoginRewardProgressRepository::default()),
            enabled: true,
            claim_coordinator: None,
            lottery_states: Arc::new(InMemoryLotteryRuntimeStore::default()),
            lottery_assets: Arc::new(UnavailableLotteryAssetGateway),
            lottery_notifier: Arc::new(NoopLotteryResultNotifier),
            cache: None,
            rate_limit_policy: ActivityRateLimitPolicy::default(),
            metrics: &METRICS,
        }
    }

    pub(crate) fn in_memory() -> Self {
        Self::new(Arc::new(InMemoryActivityRepository::default()))
    }

    pub(crate) fn postgres(pool: PgPool, reward_delivery: Arc<dyn ActivityRewardDelivery>) -> Self {
        let claim_store = Arc::new(PgActivityClaimStore::new(pool.clone()));
        Self::new(Arc::new(PgActivityRepository::from_pool(pool.clone())))
            .with_login_reward_progress_repository(Arc::new(PgLoginRewardProgressRepository::new(
                pool.clone(),
            )))
            .with_claim_coordinator(ActivityClaimCoordinator::with_store(
                reward_delivery,
                claim_store,
            ))
            .with_lottery_runtime_store(Arc::new(PgLotteryRuntimeStore::new(pool)))
    }

    pub(crate) fn disabled() -> Self {
        let mut engine = Self::in_memory();
        engine.enabled = false;
        engine
    }

    pub(crate) fn with_claim_coordinator(mut self, coordinator: ActivityClaimCoordinator) -> Self {
        self.claim_coordinator = Some(coordinator);
        self
    }

    pub(crate) fn with_login_reward_progress_repository(
        mut self,
        repository: Arc<dyn LoginRewardProgressRepository>,
    ) -> Self {
        self.login_progress = repository;
        self
    }

    pub(crate) fn with_lottery_asset_gateway(
        mut self,
        gateway: Arc<dyn LotteryAssetGateway>,
    ) -> Self {
        self.lottery_assets = gateway;
        self
    }

    fn with_lottery_runtime_store(mut self, store: Arc<dyn LotteryRuntimeStore>) -> Self {
        self.lottery_states = store;
        self
    }

    pub(crate) fn with_lottery_result_notifier(
        mut self,
        notifier: Arc<dyn LotteryResultNotifier>,
    ) -> Self {
        self.lottery_notifier = notifier;
        self
    }

    pub(crate) fn with_cache(mut self, cache: Arc<dyn ActivityCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_metrics(mut self, metrics: &'static MetricsCollector) -> Self {
        self.metrics = metrics;
        self
    }

    /// Records a server-trusted game entry for a login-reward activity.
    ///
    /// The character identity and occurrence time come from the server entry
    /// context; no client-supplied period key is accepted here.
    pub(crate) async fn on_game_entry(
        &self,
        character_id: &str,
        activity_id: &str,
        version: u32,
        occurred_at: DateTime<Utc>,
    ) -> Result<LoginRewardProgressResult, ActivityEngineError> {
        let action = ActivityActionMetric::GameEntry;
        self.metrics.record_activity_request(action);
        let result = self
            .on_game_entry_inner(character_id, activity_id, version, occurred_at)
            .await;
        self.metrics.record_activity_response(
            action,
            result.is_ok(),
            false,
            result.as_ref().err().map(|error| error.code),
        );
        result
    }

    async fn on_game_entry_inner(
        &self,
        character_id: &str,
        activity_id: &str,
        version: u32,
        occurred_at: DateTime<Utc>,
    ) -> Result<LoginRewardProgressResult, ActivityEngineError> {
        if !self.enabled {
            return Err(Self::unavailable_error());
        }
        if character_id.trim().is_empty() {
            return Err(Self::auth_error());
        }
        if activity_id.trim().is_empty() || version == 0 {
            return Err(ActivityEngineError::new(
                "ACTIVITY_INVALID_REQUEST",
                "activity id and version are required",
            ));
        }

        let snapshot = self.load_detail(activity_id, occurred_at).await?;
        if snapshot.version.version_no != version as i32 {
            return Err(ActivityEngineError::new(
                "ACTIVITY_INVALID_VERSION",
                "requested activity version is not current",
            ));
        }

        // Evaluate lifecycle at the event time so a delayed event cannot be
        // accepted outside the activity's effective running window.
        Self::validate_read_status(&snapshot, occurred_at)?;
        if snapshot.activity.activity_type.as_str() != "login_reward" {
            return Err(ActivityEngineError::new(
                "ACTIVITY_INVALID_TYPE",
                "activity type does not support game entry progress",
            ));
        }
        self.registry
            .validate_config(
                snapshot.activity.activity_type.as_str(),
                &snapshot.version.type_config,
            )
            .map_err(|error| ActivityEngineError::new("ACTIVITY_INVALID_CONFIG", error.message))?;
        let config: LoginRewardConfig =
            serde_json::from_value(snapshot.version.type_config.clone()).map_err(|error| {
                ActivityEngineError::new("ACTIVITY_INVALID_CONFIG", error.to_string())
            })?;
        let event = GameEntryEvent {
            character_id: character_id.to_string(),
            activity_id: activity_id.to_string(),
            version_no: version as i32,
            occurred_at,
        };
        let result = apply_game_entry(
            &config,
            snapshot.activity.effective_status(occurred_at),
            snapshot.activity.start_at,
            snapshot.activity.end_at,
            &snapshot.activity.timezone,
            &event,
            self.login_progress.as_ref(),
        )
        .await
        .map_err(Self::map_login_progress_error)?;
        if config.claim_mode == "automatic" {
            for stage_no in eligible_stage_numbers(&config, &result.state) {
                let request = ActivityActionRequest {
                    activity_id: activity_id.to_string(),
                    version,
                    stage_id: stage_no.to_string(),
                    action_type: "claim".to_string(),
                    client_request_id: format!("auto:{}:{}", result.period_key, stage_no),
                };
                let base = ActivityActionResponse {
                    ok: false,
                    error_code: None,
                    activity_id: activity_id.to_string(),
                    version,
                    stage_id: request.stage_id.clone(),
                    action_type: request.action_type.clone(),
                    client_request_id: request.client_request_id.clone(),
                    processing: false,
                    duplicate: false,
                    state_revision: result.state_revision as u64,
                };
                let response = self
                    .claim_login_reward(character_id, &request, &snapshot, base, true)
                    .await;
                if !response.ok && !response.duplicate {
                    return Err(ActivityEngineError::new(
                        response.error_code.unwrap_or("ACTIVITY_RETRYABLE_FAILURE"),
                        "automatic login reward delivery did not complete",
                    ));
                }
            }
        }
        Ok(result)
    }

    fn map_login_progress_error(error: LoginRewardProgressError) -> ActivityEngineError {
        match error {
            LoginRewardProgressError::InvalidEvent(message) => {
                ActivityEngineError::new("ACTIVITY_INVALID_REQUEST", message)
            }
            LoginRewardProgressError::InvalidConfig(message) => {
                ActivityEngineError::new("ACTIVITY_INVALID_CONFIG", message)
            }
            LoginRewardProgressError::ActivityNotActive => {
                ActivityEngineError::new("ACTIVITY_NOT_STARTED", "activity is not active")
            }
            LoginRewardProgressError::VersionConflict => ActivityEngineError::new(
                "ACTIVITY_VERSION_CONFLICT",
                "login progress changed concurrently",
            ),
            LoginRewardProgressError::StorageUnavailable => ActivityEngineError::new(
                "ACTIVITY_STORAGE_UNAVAILABLE",
                "activity storage unavailable",
            ),
            LoginRewardProgressError::NotQualified => ActivityEngineError::new(
                "ACTIVITY_QUALIFICATION_NOT_MET",
                "login reward qualification is not met",
            ),
            LoginRewardProgressError::AlreadyClaimed => ActivityEngineError::new(
                "ACTIVITY_ALREADY_CLAIMED",
                "login reward stage has already been claimed",
            ),
        }
    }

    async fn claim_login_reward(
        &self,
        character_id: &str,
        request: &ActivityActionRequest,
        snapshot: &PublishedActivitySnapshot,
        base: ActivityActionResponse,
        automatic: bool,
    ) -> ActivityActionResponse {
        let config: LoginRewardConfig =
            match serde_json::from_value(snapshot.version.type_config.clone()) {
                Ok(config) => config,
                Err(_) => return Self::failed(base, "ACTIVITY_INVALID_CONFIG"),
            };
        if (automatic && config.claim_mode != "automatic")
            || (!automatic && config.claim_mode != "manual")
        {
            return Self::failed(base, "ACTIVITY_INVALID_ACTION");
        }
        let stage = match request.stage_id.parse::<u32>().ok().and_then(|stage_no| {
            config
                .stages
                .iter()
                .find(|stage| stage.stage_no == stage_no)
        }) {
            Some(stage) => stage,
            None => return Self::failed(base, "ACTIVITY_QUALIFICATION_NOT_MET"),
        };
        let (state, revision) = match self
            .login_progress
            .load(
                character_id,
                &request.activity_id,
                snapshot.version.version_no,
            )
            .await
        {
            Ok(value) => value,
            Err(error) => return Self::failed(base, Self::map_login_progress_error(error).code),
        };
        let period_key = match state.last_period_key.clone() {
            Some(period_key) => period_key,
            None => return Self::failed(base, "ACTIVITY_QUALIFICATION_NOT_MET"),
        };
        let count = if config.progression == "cumulative" {
            state.cumulative_count
        } else {
            state.consecutive_count
        };
        if count < stage.required_count {
            return Self::failed(base, "ACTIVITY_QUALIFICATION_NOT_MET");
        }
        let semantic_claim_key =
            login_reward_claim_key(&request.stage_id, &period_key, snapshot.version.version_no);
        if state
            .claimed_stage_ids
            .iter()
            .any(|value| value == &semantic_claim_key)
        {
            let mut response = ActivityActionResponse { ok: true, ..base };
            response.duplicate = true;
            response.state_revision = revision as u64;
            return response;
        }
        let items = match Self::reward_items(
            &snapshot.version.public_config,
            &stage.reward_group_key,
            character_id,
        ) {
            Ok(items) if !items.is_empty() => items,
            _ => return Self::failed(base, "ACTIVITY_MANUAL_REVIEW"),
        };
        let Some(coordinator) = &self.claim_coordinator else {
            return Self::failed(base, "ACTIVITY_MANUAL_REVIEW");
        };
        let order = match build_reward_order(
            character_id,
            &request.activity_id,
            snapshot.version.version_no,
            &semantic_claim_key,
            &items,
            RewardDeliveryPolicy::PreferInventory,
        ) {
            Ok(order) => order,
            Err(_) => return Self::failed(base, "ACTIVITY_MANUAL_REVIEW"),
        };
        let settlement = coordinator
            .settle(
                character_id,
                &request.activity_id,
                snapshot.version.version_no,
                &semantic_claim_key,
                &request.client_request_id,
                order,
            )
            .await;
        match settlement.status {
            ClaimStatus::Granted => {
                let current_stage_id = Some(request.stage_id.clone());
                let marked = self
                    .login_progress
                    .claim_stage(
                        character_id,
                        &request.activity_id,
                        snapshot.version.version_no,
                        revision,
                        &semantic_claim_key,
                        current_stage_id,
                    )
                    .await;
                match marked {
                    Ok(next_revision) => ActivityActionResponse {
                        ok: true,
                        duplicate: settlement.duplicate,
                        state_revision: next_revision as u64,
                        ..base
                    },
                    Err(LoginRewardProgressError::AlreadyClaimed) => ActivityActionResponse {
                        ok: true,
                        duplicate: true,
                        state_revision: revision as u64,
                        ..base
                    },
                    Err(LoginRewardProgressError::VersionConflict) => {
                        let latest = self
                            .login_progress
                            .load(
                                character_id,
                                &request.activity_id,
                                snapshot.version.version_no,
                            )
                            .await;
                        if latest.ok().is_some_and(|(state, _)| {
                            state
                                .claimed_stage_ids
                                .iter()
                                .any(|value| value == &semantic_claim_key)
                        }) {
                            ActivityActionResponse {
                                ok: true,
                                duplicate: true,
                                ..base
                            }
                        } else {
                            Self::failed(base, "ACTIVITY_RETRYABLE_FAILURE")
                        }
                    }
                    Err(error) => Self::failed(base, Self::map_login_progress_error(error).code),
                }
            }
            ClaimStatus::Processing => {
                let mut response = Self::failed(base, "ACTIVITY_PROCESSING");
                response.processing = true;
                response.duplicate = settlement.duplicate;
                response
            }
            ClaimStatus::RetryableFailure => Self::failed(base, "ACTIVITY_RETRYABLE_FAILURE"),
            ClaimStatus::ReconciliationPending => {
                Self::failed(base, "ACTIVITY_RECONCILIATION_PENDING")
            }
            ClaimStatus::BlockedCapacity => Self::failed(base, "INVENTORY_FULL"),
            ClaimStatus::ManualReview => Self::failed(base, "ACTIVITY_MANUAL_REVIEW"),
        }
    }

    fn reward_items(
        public_config: &serde_json::Value,
        reward_group_key: &str,
        character_id: &str,
    ) -> Result<Vec<NormalizedAssetItem>, ()> {
        let groups = public_config.get("reward_groups").ok_or(())?;
        let group = if let Some(groups) = groups.as_array() {
            groups
                .iter()
                .find(|group| {
                    group.get("key").and_then(|value| value.as_str()) == Some(reward_group_key)
                        || group
                            .get("reward_group_key")
                            .and_then(|value| value.as_str())
                            == Some(reward_group_key)
                })
                .ok_or(())?
        } else {
            groups.get(reward_group_key).ok_or(())?
        };
        let items = group
            .get("items")
            .and_then(|value| value.as_array())
            .ok_or(())?;
        items
            .iter()
            .map(|item| {
                let item_id = item
                    .get("item_id")
                    .or_else(|| item.get("asset_id"))
                    .and_then(|value| value.as_i64())
                    .ok_or(())? as i32;
                let count = item
                    .get("count")
                    .or_else(|| item.get("quantity"))
                    .and_then(|value| value.as_u64())
                    .ok_or(())? as u32;
                let binding = match item.get("binding").and_then(|value| value.as_str()) {
                    Some("character_bound") => AssetBinding::CharacterBound {
                        character_id: character_id.to_string(),
                    },
                    _ => AssetBinding::Unbound,
                };
                NormalizedAssetItem::new(item_id, count, binding).map_err(|_| ())
            })
            .collect()
    }

    pub(crate) async fn list(
        &self,
        character_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<PublishedActivitySnapshot>, ActivityEngineError> {
        self.list_with_context(&ActivityRequestContext::character_only(character_id), now)
            .await
    }

    pub(crate) async fn list_with_context(
        &self,
        context: &ActivityRequestContext,
        now: DateTime<Utc>,
    ) -> Result<Vec<PublishedActivitySnapshot>, ActivityEngineError> {
        let action = ActivityActionMetric::List;
        self.metrics.record_activity_request(action);
        let result = self.list_with_context_inner(context, now).await;
        self.metrics.record_activity_response(
            action,
            result.is_ok(),
            false,
            result.as_ref().err().map(|error| error.code),
        );
        result
    }

    async fn list_with_context_inner(
        &self,
        context: &ActivityRequestContext,
        now: DateTime<Utc>,
    ) -> Result<Vec<PublishedActivitySnapshot>, ActivityEngineError> {
        if !self.enabled {
            return Err(Self::unavailable_error());
        }
        if context.character_id.trim().is_empty() {
            return Err(Self::auth_error());
        }
        if self.check_rate_limit(context, "*", "read:list").await {
            return Err(Self::rate_limited_error());
        }
        let snapshots = self.repository.list_published(now).await.map_err(|_| {
            ActivityEngineError::new(
                "ACTIVITY_STORAGE_UNAVAILABLE",
                "activity storage unavailable",
            )
        })?;
        if let Some(cache) = &self.cache {
            for snapshot in &snapshots {
                self.refresh_cached_version(cache.as_ref(), &snapshot.version)
                    .await;
            }
            let ids = snapshots
                .iter()
                .map(|snapshot| snapshot.activity.id.clone())
                .collect::<Vec<_>>();
            if cache.put_activity_list(&ids).await.is_err() {
                self.metrics
                    .record_activity_cache_failure(ActivityCacheFailureMetric::Write);
            }
        }
        Ok(snapshots)
    }

    pub(crate) async fn detail(
        &self,
        character_id: &str,
        activity_id: &str,
        version: u32,
        now: DateTime<Utc>,
    ) -> Result<PublishedActivitySnapshot, ActivityEngineError> {
        self.detail_with_context(
            &ActivityRequestContext::character_only(character_id),
            activity_id,
            version,
            now,
        )
        .await
    }

    pub(crate) async fn detail_with_context(
        &self,
        context: &ActivityRequestContext,
        activity_id: &str,
        version: u32,
        now: DateTime<Utc>,
    ) -> Result<PublishedActivitySnapshot, ActivityEngineError> {
        let action = ActivityActionMetric::Detail;
        self.metrics.record_activity_request(action);
        let result = self
            .detail_with_context_inner(context, activity_id, version, now)
            .await;
        self.metrics.record_activity_response(
            action,
            result.is_ok(),
            false,
            result.as_ref().err().map(|error| error.code),
        );
        result
    }

    pub(crate) async fn progress_with_context(
        &self,
        context: &ActivityRequestContext,
        activity_id: &str,
        version: u32,
        now: DateTime<Utc>,
    ) -> Result<PublishedActivitySnapshot, ActivityEngineError> {
        let action = ActivityActionMetric::Progress;
        self.metrics.record_activity_request(action);
        let result = self
            .detail_with_context_inner(context, activity_id, version, now)
            .await;
        self.metrics.record_activity_response(
            action,
            result.is_ok(),
            false,
            result.as_ref().err().map(|error| error.code),
        );
        result
    }

    async fn detail_with_context_inner(
        &self,
        context: &ActivityRequestContext,
        activity_id: &str,
        version: u32,
        now: DateTime<Utc>,
    ) -> Result<PublishedActivitySnapshot, ActivityEngineError> {
        if !self.enabled {
            return Err(Self::unavailable_error());
        }
        if context.character_id.trim().is_empty() {
            return Err(Self::auth_error());
        }
        if self
            .check_rate_limit(context, activity_id, "read:detail")
            .await
        {
            return Err(Self::rate_limited_error());
        }
        let snapshot = self.load_detail(activity_id, now).await?;
        if version != 0 && snapshot.version.version_no != version as i32 {
            return Err(ActivityEngineError::new(
                "ACTIVITY_INVALID_VERSION",
                "requested activity version is not current",
            ));
        }
        Self::validate_read_status(&snapshot, now)?;
        Ok(snapshot)
    }

    pub(crate) async fn player_view_json(
        &self,
        character_id: &str,
        snapshot: &PublishedActivitySnapshot,
        now: DateTime<Utc>,
    ) -> Result<serde_json::Value, ActivityEngineError> {
        let player_state = if snapshot.activity.activity_type.as_str() == "login_reward" {
            self.login_progress
                .load_activity_state(
                    character_id,
                    &snapshot.activity.id,
                    snapshot.version.version_no,
                )
                .await
                .map_err(Self::map_login_progress_error)?
        } else if snapshot.activity.activity_type.as_str() == "lottery" {
            let state = self
                .lottery_states
                .state(
                    character_id,
                    &snapshot.activity.id,
                    snapshot.version.version_no,
                )
                .await
                .map_err(|_| {
                    ActivityEngineError::new(
                        "ACTIVITY_STORAGE_UNAVAILABLE",
                        "lottery runtime storage unavailable",
                    )
                })?;
            Some(super::domain::PlayerActivityState {
                character_id: character_id.to_string(),
                activity_id: snapshot.activity.id.clone(),
                version_no: snapshot.version.version_no,
                current_stage_id: state.draw_request_id.clone(),
                progress: serde_json::json!({
                    "daily_draw_count": state.daily_draw_count,
                    "total_draw_count": state.total_draw_count,
                    "last_draw_period_key": state.last_draw_period_key,
                }),
                type_state: serde_json::to_value(&state).unwrap_or_else(|_| serde_json::json!({})),
                state_revision: state.total_draw_count as i64,
            })
        } else {
            None
        };
        let mut view = self
            .registry
            .build_player_view(&snapshot.activity, &snapshot.version, player_state.as_ref())
            .map_err(|error| ActivityEngineError::new("ACTIVITY_INVALID_CONFIG", error.message))?;
        if snapshot.activity.activity_type.as_str() == "login_reward" {
            if let Some(last_period_key) =
                view.get("last_period_key").and_then(|value| value.as_str())
            {
                let current_period =
                    super::types::login_period_key(now, &snapshot.activity.timezone)
                        .map_err(Self::map_login_progress_error)?;
                view["today_status"] = serde_json::json!(if last_period_key == current_period {
                    "logged_in"
                } else {
                    "not_logged_in"
                });
            }
        }
        Ok(view)
    }

    async fn load_detail(
        &self,
        activity_id: &str,
        now: DateTime<Utc>,
    ) -> Result<PublishedActivitySnapshot, ActivityEngineError> {
        let snapshot = self
            .repository
            .get_published_for_detail(activity_id, now)
            .await
            .map_err(|_| {
                ActivityEngineError::new(
                    "ACTIVITY_STORAGE_UNAVAILABLE",
                    "activity storage unavailable",
                )
            })?
            .ok_or_else(|| {
                ActivityEngineError::new("ACTIVITY_NOT_FOUND", "published activity was not found")
            })?;
        if let Some(cache) = &self.cache {
            self.refresh_cached_version(cache.as_ref(), &snapshot.version)
                .await;
        }
        Ok(snapshot)
    }

    async fn refresh_cached_version(
        &self,
        cache: &dyn ActivityCache,
        version: &super::domain::ActivityVersion,
    ) {
        match cache
            .get_version(&version.activity_id, version.version_no)
            .await
        {
            Ok(Some(cached)) if cached.config_digest == version.config_digest => return,
            Ok(_) => {}
            Err(_) => self
                .metrics
                .record_activity_cache_failure(ActivityCacheFailureMetric::Read),
        }
        if cache.put_version(version).await.is_err() {
            self.metrics
                .record_activity_cache_failure(ActivityCacheFailureMetric::Refresh);
        }
    }

    pub(crate) async fn dispatch_action(
        &self,
        character_id: &str,
        request: ActivityActionRequest,
        now: DateTime<Utc>,
    ) -> ActivityActionResponse {
        self.dispatch_action_with_context(
            &ActivityRequestContext::character_only(character_id),
            request,
            now,
        )
        .await
    }

    pub(crate) async fn dispatch_action_with_context(
        &self,
        context: &ActivityRequestContext,
        request: ActivityActionRequest,
        now: DateTime<Utc>,
    ) -> ActivityActionResponse {
        let action = ActivityActionMetric::from_action(&request.action_type);
        self.metrics.record_activity_request(action);
        let response = self
            .dispatch_action_with_context_inner(context, request, now)
            .await;
        self.metrics.record_activity_response(
            action,
            response.ok,
            response.duplicate,
            response.error_code,
        );
        response
    }

    async fn dispatch_action_with_context_inner(
        &self,
        context: &ActivityRequestContext,
        request: ActivityActionRequest,
        now: DateTime<Utc>,
    ) -> ActivityActionResponse {
        let character_id = context.character_id.as_str();
        let base = ActivityActionResponse {
            ok: false,
            error_code: None,
            activity_id: request.activity_id.clone(),
            version: request.version,
            stage_id: request.stage_id.clone(),
            action_type: request.action_type.clone(),
            client_request_id: request.client_request_id.clone(),
            processing: false,
            duplicate: false,
            state_revision: 0,
        };
        if !self.enabled {
            return Self::failed(base, Self::unavailable_error().code);
        }
        if character_id.trim().is_empty() {
            return Self::failed(base, Self::auth_error().code);
        }
        if request.client_request_id.trim().is_empty() || request.client_request_id.len() > 128 {
            return Self::failed(base, "ACTIVITY_INVALID_REQUEST");
        }
        let request_key = format!("{character_id}:{}", request.client_request_id);
        let lottery_record_key = lottery_record_key(
            character_id,
            &request.activity_id,
            i64::from(request.version),
            &request.client_request_id,
        );
        let request_fingerprint = format!(
            "{}\0{}\0{}\0{}",
            request.activity_id, request.version, request.action_type, request.stage_id
        );
        let is_new_request = {
            let mut state = self.request_state.lock().await;
            if let Some(previous) = state.seen.get(&request_key) {
                if previous.fingerprint != request_fingerprint {
                    return Self::failed(base, "REQUEST_FINGERPRINT_CONFLICT");
                }
                false
            } else {
                state.seen.insert(
                    request_key.clone(),
                    SeenRequest {
                        fingerprint: request_fingerprint.clone(),
                        response: None,
                        processing: true,
                    },
                );
                true
            }
        };
        // PostgreSQL may be slow or unavailable. Never retain the process-wide request mutex
        // while querying a durable lottery record, otherwise one draw stalls unrelated players.
        let lottery_record_exists = request.action_type == "draw"
            && self
                .lottery_states
                .record(&lottery_record_key)
                .await
                .ok()
                .flatten()
                .is_some();
        if !lottery_record_exists {
            if !is_new_request {
                let mut state = self.request_state.lock().await;
                let Some(previous) = state.seen.get_mut(&request_key) else {
                    state.seen.insert(
                        request_key.clone(),
                        SeenRequest {
                            fingerprint: request_fingerprint.clone(),
                            response: None,
                            processing: true,
                        },
                    );
                    drop(state);
                    return Self::failed(base, "ACTIVITY_PROCESSING");
                };
                if previous.fingerprint != request_fingerprint {
                    return Self::failed(base, "REQUEST_FINGERPRINT_CONFLICT");
                }
                if let Some(mut response) = previous.response.clone() {
                    response.duplicate = true;
                    return response;
                }
                if previous.processing {
                    let mut response = Self::failed(base, "ACTIVITY_PROCESSING");
                    response.processing = true;
                    response.duplicate = true;
                    return response;
                }
                previous.processing = true;
            }
            let action_limit_key = format!("write:{}", request.action_type);
            if self
                .check_rate_limit(context, &request.activity_id, &action_limit_key)
                .await
            {
                let response = Self::failed(base, "ACTIVITY_RATE_LIMITED");
                return self
                    .finish_request(&request_key, &request_fingerprint, response)
                    .await;
            }
        }

        let snapshot = match self.load_detail(&request.activity_id, now).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let response = Self::failed(base, error.code);
                return self
                    .finish_request(&request_key, &request_fingerprint, response)
                    .await;
            }
        };
        if request.version != 0 && snapshot.version.version_no != request.version as i32 {
            let response = Self::failed(base, "ACTIVITY_INVALID_VERSION");
            return self
                .finish_request(&request_key, &request_fingerprint, response)
                .await;
        }
        if let Err(error) = Self::validate_read_status(&snapshot, now) {
            let ended_claim_window = snapshot.activity.activity_type.as_str() == "login_reward"
                && request.action_type == "claim"
                && error.code == "ACTIVITY_ENDED"
                && now < snapshot.activity.claim_deadline
                && self
                    .login_progress
                    .load(
                        character_id,
                        &request.activity_id,
                        snapshot.version.version_no,
                    )
                    .await
                    .ok()
                    .is_some_and(|(state, _)| state.last_period_key.is_some());
            if !ended_claim_window {
                let response = Self::failed(base, error.code);
                return self
                    .finish_request(&request_key, &request_fingerprint, response)
                    .await;
            }
        }
        if request.action_type == "claim" && request.stage_id.trim().is_empty() {
            let response = Self::failed(base, "ACTIVITY_INVALID_REQUEST");
            return self
                .finish_request(&request_key, &request_fingerprint, response)
                .await;
        }
        if snapshot.activity.activity_type.as_str() == "login_reward"
            && request.action_type == "claim"
        {
            let response = self
                .claim_login_reward(character_id, &request, &snapshot, base.clone(), false)
                .await;
            return self
                .finish_request(&request_key, &request_fingerprint, response)
                .await;
        }
        if snapshot.activity.activity_type.as_str() == "lottery" && request.action_type == "draw" {
            let response = self
                .draw_lottery(character_id, &request, &snapshot, base.clone(), now)
                .await;
            return self
                .finish_request(&request_key, &request_fingerprint, response)
                .await;
        }
        let mut transaction = TransactionContext {
            request_id: request.client_request_id.clone(),
        };
        let outcome = self.registry.dispatch_action(
            &snapshot.activity,
            &snapshot.version,
            &request.action_type,
            &PlayerContext {
                character_id: character_id.to_string(),
            },
            None,
            &mut transaction,
        );
        let response = match outcome {
            Ok(outcome) if outcome.applied => {
                if request.action_type == "claim" {
                    if let Some(coordinator) = &self.claim_coordinator {
                        let items: Vec<NormalizedAssetItem> = outcome
                            .result
                            .get("reward_items")
                            .and_then(|items| serde_json::from_value(items.clone()).ok())
                            .unwrap_or_default();
                        if items.is_empty() {
                            let response = Self::failed(base, "ACTIVITY_MANUAL_REVIEW");
                            return self
                                .finish_request(&request_key, &request_fingerprint, response)
                                .await;
                        }
                        let semantic_claim_key = request.stage_id.clone();
                        let order = match build_reward_order(
                            character_id,
                            &request.activity_id,
                            snapshot.version.version_no,
                            &semantic_claim_key,
                            &items,
                            RewardDeliveryPolicy::PreferInventory,
                        ) {
                            Ok(order) => order,
                            Err(_) => {
                                let response = Self::failed(base, "ACTIVITY_MANUAL_REVIEW");
                                return self
                                    .finish_request(&request_key, &request_fingerprint, response)
                                    .await;
                            }
                        };
                        let settlement = coordinator
                            .settle(
                                character_id,
                                &request.activity_id,
                                snapshot.version.version_no,
                                &semantic_claim_key,
                                &request.client_request_id,
                                order,
                            )
                            .await;
                        let duplicate = settlement.duplicate;
                        match settlement.status {
                            ClaimStatus::Granted => ActivityActionResponse {
                                ok: true,
                                duplicate,
                                ..base
                            },
                            ClaimStatus::Processing => {
                                let mut response = Self::failed(base, "ACTIVITY_PROCESSING");
                                response.processing = true;
                                response.duplicate = duplicate;
                                response
                            }
                            ClaimStatus::RetryableFailure => {
                                let mut response = Self::failed(base, "ACTIVITY_RETRYABLE_FAILURE");
                                response.duplicate = duplicate;
                                response
                            }
                            ClaimStatus::ReconciliationPending => {
                                let mut response =
                                    Self::failed(base, "ACTIVITY_RECONCILIATION_PENDING");
                                response.duplicate = duplicate;
                                response
                            }
                            ClaimStatus::BlockedCapacity => {
                                let mut response = Self::failed(base, "INVENTORY_FULL");
                                response.duplicate = duplicate;
                                response
                            }
                            ClaimStatus::ManualReview => {
                                let mut response = Self::failed(base, "ACTIVITY_MANUAL_REVIEW");
                                response.duplicate = duplicate;
                                response
                            }
                        }
                    } else {
                        ActivityActionResponse { ok: true, ..base }
                    }
                } else {
                    ActivityActionResponse { ok: true, ..base }
                }
            }
            Ok(_) => Self::failed(base, "ACTIVITY_QUALIFICATION_NOT_MET"),
            Err(error) => Self::failed(base, error.code.as_str()),
        };
        self.finish_request(&request_key, &request_fingerprint, response)
            .await
    }

    async fn draw_lottery(
        &self,
        character_id: &str,
        request: &ActivityActionRequest,
        snapshot: &PublishedActivitySnapshot,
        base: ActivityActionResponse,
        now: DateTime<Utc>,
    ) -> ActivityActionResponse {
        let config: LotteryConfig =
            match serde_json::from_value(snapshot.version.type_config.clone()) {
                Ok(config) => config,
                Err(_) => return Self::failed(base, "ACTIVITY_INVALID_CONFIG"),
            };
        let record_key = lottery_record_key(
            character_id,
            &request.activity_id,
            i64::from(request.version),
            &request.client_request_id,
        );
        if let Ok(Some(existing)) = self.lottery_states.record(&record_key).await {
            return self
                .resolve_lottery_record(existing, record_key, base, false)
                .await;
        }
        let current = match self
            .lottery_states
            .state(
                character_id,
                &snapshot.activity.id,
                snapshot.version.version_no,
            )
            .await
        {
            Ok(state) => state,
            Err(_) => return Self::failed(base, "ACTIVITY_STORAGE_UNAVAILABLE"),
        };
        let voucher_item_id = config.voucher_item_id;
        let needs_voucher = lottery_period_key(now, &snapshot.activity.timezone)
            .ok()
            .map(|period| {
                normalize_lottery_state(&config, &current, &period).free_draws_remaining == 0
            })
            .unwrap_or(true);
        let voucher = if needs_voucher {
            match voucher_item_id {
                Some(item_id) => match self
                    .lottery_assets
                    .find_voucher(character_id, item_id)
                    .await
                {
                    Ok(value) => value,
                    Err(_) => return Self::failed(base, "ACTIVITY_RETRYABLE_FAILURE"),
                },
                None => None,
            }
        } else {
            None
        };
        let voucher_quantity = voucher.map(|value| value.quantity).unwrap_or(0);
        let decision = match evaluate_lottery_draw(
            &snapshot.activity,
            &config,
            &current,
            voucher_quantity,
            now,
        ) {
            Ok(decision) => decision,
            Err(error) => {
                let code = match error.code {
                    super::types::ActivityTypeErrorCode::HandlerRejected => {
                        if error.message.contains("not running") {
                            "ACTIVITY_ENDED"
                        } else {
                            "ACTIVITY_QUALIFICATION_NOT_MET"
                        }
                    }
                    _ => "ACTIVITY_INVALID_CONFIG",
                };
                return Self::failed(base, code);
            }
        };
        let selection = match draw_lottery_item(&config) {
            Ok(selection) => selection,
            Err(_) => return Self::failed(base, "ACTIVITY_RETRYABLE_FAILURE"),
        };
        let exchange = match decision.cost {
            LotteryDrawCost::Free => None,
            LotteryDrawCost::Voucher { item_id, .. } => {
                let Some(voucher) = voucher else {
                    return Self::failed(base, "ACTIVITY_QUALIFICATION_NOT_MET");
                };
                if voucher.item_id != item_id || voucher.quantity == 0 {
                    return Self::failed(base, "ACTIVITY_QUALIFICATION_NOT_MET");
                }
                match build_lottery_voucher_exchange(
                    character_id,
                    &format!("{}:{}", request.activity_id, request.client_request_id),
                    voucher.asset_uid,
                    item_id,
                    &selection,
                ) {
                    Ok(exchange) => Some(exchange),
                    Err(_) => return Self::failed(base, "ACTIVITY_MANUAL_REVIEW"),
                }
            }
        };
        let mut next_state = decision.next_state;
        next_state.draw_request_id = Some(request.client_request_id.clone());
        next_state.result_item_id = Some(selection.item_id);
        let semantic_key = record_key.clone();
        let reward_item =
            NormalizedAssetItem::new(selection.item_id, selection.quantity, AssetBinding::Unbound)
                .map_err(|_| ())
                .ok();
        let Some(reward_item) = reward_item else {
            return Self::failed(base, "ACTIVITY_MANUAL_REVIEW");
        };
        let reward_order = match build_reward_order(
            character_id,
            &request.activity_id,
            snapshot.version.version_no,
            &semantic_key,
            &[reward_item],
            RewardDeliveryPolicy::InventoryRequired,
        ) {
            Ok(order) => order,
            Err(_) => return Self::failed(base, "ACTIVITY_MANUAL_REVIEW"),
        };
        let asset_request_id = exchange
            .as_ref()
            .map(|value| value.command.request_id.clone())
            .unwrap_or_else(|| reward_order.request_id.clone());
        let asset_fingerprint = exchange
            .as_ref()
            .map(|value| value.command.request_fingerprint().as_str().to_string())
            .unwrap_or_else(|| reward_order.request_fingerprint().as_str().to_string());
        let reward_request_id = reward_order.request_id.clone();
        let reward_fingerprint = Some(reward_order.request_fingerprint().as_str().to_string());
        let record = LotteryDrawRecord {
            character_id: character_id.to_string(),
            activity_id: request.activity_id.clone(),
            version: snapshot.version.version_no,
            semantic_key,
            draw_request_id: request.client_request_id.clone(),
            status: LotteryDrawStatus::Processing,
            previous_state: current,
            next_state,
            selection,
            exchange,
            reward_order,
            reward_request_id,
            reward_fingerprint,
            asset_request_id,
            asset_fingerprint,
            notification_failed: false,
        };
        match self
            .lottery_states
            .create(
                character_id,
                &snapshot.activity.id,
                snapshot.version.version_no,
                &record_key,
                record.clone(),
            )
            .await
        {
            Ok(LotteryRecordCreate::Created) => {
                self.resolve_lottery_record(record, record_key, base, true)
                    .await
            }
            Ok(LotteryRecordCreate::Existing(existing)) => {
                self.resolve_lottery_record(existing, record_key, base, false)
                    .await
            }
            Ok(LotteryRecordCreate::Busy) => Self::failed(base, "ACTIVITY_PROCESSING"),
            Ok(LotteryRecordCreate::Conflict) => Self::failed(base, "REQUEST_FINGERPRINT_CONFLICT"),
            Ok(LotteryRecordCreate::ManualReview) => Self::failed(base, "ACTIVITY_MANUAL_REVIEW"),
            Err(_) => Self::failed(base, "ACTIVITY_STORAGE_UNAVAILABLE"),
        }
    }

    async fn resolve_lottery_record(
        &self,
        mut record: LotteryDrawRecord,
        record_key: String,
        base: ActivityActionResponse,
        initial_attempt: bool,
    ) -> ActivityActionResponse {
        if record.status == LotteryDrawStatus::Granted {
            return Self::lottery_record_response(base, &record, true);
        }
        if record.status == LotteryDrawStatus::ManualReview {
            return Self::failed(base, "ACTIVITY_MANUAL_REVIEW");
        }
        if record.status == LotteryDrawStatus::ReconciliationPending
            || (record.status == LotteryDrawStatus::Processing && !initial_attempt)
        {
            match self
                .lottery_assets
                .query_draw(
                    &record.asset_request_id,
                    Some(record.asset_fingerprint.as_str()),
                )
                .await
            {
                Ok(Some(result)) => {
                    if result == AssetResultState::Unknown {
                        return Self::failed(base, "ACTIVITY_RECONCILIATION_PENDING");
                    }
                    if result == AssetResultState::NotApplied {
                        record.status = LotteryDrawStatus::RetryableFailure;
                    } else {
                        return self.finish_lottery_granted(record, record_key, base).await;
                    }
                }
                Ok(None) => {
                    if record.status == LotteryDrawStatus::ReconciliationPending {
                        return Self::failed(base, "ACTIVITY_RECONCILIATION_PENDING");
                    }
                }
                Err(_) => return Self::failed(base, "ACTIVITY_RECONCILIATION_PENDING"),
            }
        }
        let delivery_started_at = Instant::now();
        let delivery_result = self
            .lottery_assets
            .apply_draw(
                &record.character_id,
                &record.draw_request_id,
                record.exchange.clone(),
                &record.reward_order,
                &record.selection,
            )
            .await;
        self.metrics.record_activity_reward_delivery_duration(
            u64::try_from(delivery_started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        );
        let result = match delivery_result {
            Ok(result) => result,
            // A transport/storage error does not prove the asset transaction rolled back.
            // Persist unknown and query the original request on retry.
            Err(LotteryAssetApplyError::Unknown) => AssetResultState::Unknown,
            Err(LotteryAssetApplyError::RetryableNotApplied) => AssetResultState::NotApplied,
            Err(LotteryAssetApplyError::PermanentNotApplied) => {
                record.status = LotteryDrawStatus::ManualReview;
                let _ = self.lottery_states.save(&record_key, record).await;
                return Self::failed(base, "ACTIVITY_MANUAL_REVIEW");
            }
        };
        match result {
            AssetResultState::Applied => {
                self.finish_lottery_granted(record, record_key, base).await
            }
            AssetResultState::Unknown => {
                record.status = LotteryDrawStatus::ReconciliationPending;
                let _ = self.lottery_states.save(&record_key, record).await;
                Self::failed(base, "ACTIVITY_RECONCILIATION_PENDING")
            }
            AssetResultState::NotApplied => {
                record.status = LotteryDrawStatus::RetryableFailure;
                let _ = self.lottery_states.save(&record_key, record).await;
                Self::failed(base, "ACTIVITY_RETRYABLE_FAILURE")
            }
        }
    }

    async fn finish_lottery_granted(
        &self,
        mut record: LotteryDrawRecord,
        record_key: String,
        base: ActivityActionResponse,
    ) -> ActivityActionResponse {
        record.status = LotteryDrawStatus::Granted;
        if self
            .lottery_states
            .grant(
                &record.character_id,
                &record.activity_id,
                record.version,
                &record_key,
                record.clone(),
            )
            .await
            .is_err()
        {
            return Self::failed(base, "ACTIVITY_RECONCILIATION_PENDING");
        }
        if self.lottery_notifier.notify(&record).await.is_err() {
            record.notification_failed = true;
            let _ = self.lottery_states.save(&record_key, record.clone()).await;
        }
        Self::lottery_record_response(base, &record, false)
    }

    fn lottery_record_response(
        mut base: ActivityActionResponse,
        record: &LotteryDrawRecord,
        duplicate: bool,
    ) -> ActivityActionResponse {
        base.duplicate = duplicate;
        base.state_revision = record.next_state.total_draw_count as u64;
        match record.status {
            LotteryDrawStatus::Granted => base.ok = true,
            LotteryDrawStatus::Processing => {
                base.error_code = Some("ACTIVITY_PROCESSING");
                base.processing = true;
            }
            LotteryDrawStatus::RetryableFailure => {
                base.error_code = Some("ACTIVITY_RETRYABLE_FAILURE")
            }
            LotteryDrawStatus::ReconciliationPending => {
                base.error_code = Some("ACTIVITY_RECONCILIATION_PENDING")
            }
            LotteryDrawStatus::ManualReview => base.error_code = Some("ACTIVITY_MANUAL_REVIEW"),
        }
        base
    }

    fn validate_read_status(
        snapshot: &PublishedActivitySnapshot,
        now: DateTime<Utc>,
    ) -> Result<(), ActivityEngineError> {
        match snapshot.activity.effective_status(now) {
            ActivityStatus::Published if now < snapshot.activity.start_at => Err(
                ActivityEngineError::new("ACTIVITY_NOT_STARTED", "activity has not started"),
            ),
            ActivityStatus::Ended => Err(ActivityEngineError::new(
                "ACTIVITY_ENDED",
                "activity has ended",
            )),
            ActivityStatus::Offline => Err(ActivityEngineError::new(
                "ACTIVITY_OFFLINE",
                "activity is offline",
            )),
            _ => Ok(()),
        }
    }

    fn auth_error() -> ActivityEngineError {
        ActivityEngineError::new(
            "ACTIVITY_AUTH_REQUIRED",
            "character-bound authentication is required",
        )
    }

    fn unavailable_error() -> ActivityEngineError {
        ActivityEngineError::new(
            "ACTIVITY_ENGINE_UNAVAILABLE",
            "activity engine is not enabled in this server",
        )
    }

    fn rate_limited_error() -> ActivityEngineError {
        ActivityEngineError::new("ACTIVITY_RATE_LIMITED", "activity request rate limited")
    }

    async fn check_rate_limit(
        &self,
        context: &ActivityRequestContext,
        activity_id: &str,
        action: &str,
    ) -> bool {
        let policy = self.rate_limit_policy;
        let character_id = context.character_id.as_str();
        let mut keys = vec![
            (format!("character:{character_id}"), policy.character_max),
            (
                format!("activity:{character_id}:{activity_id}"),
                policy.activity_max,
            ),
            (
                format!("action:{character_id}:{activity_id}:{action}"),
                policy.action_max,
            ),
        ];
        if let Some(account_player_id) = context.account_player_id.as_deref() {
            keys.push((format!("account:{account_player_id}"), policy.account_max));
        }
        if let Some(source_ip) = context.source_ip.as_deref() {
            keys.push((format!("source:{source_ip}"), policy.source_max));
        }
        if let Some(credential_id) = context.credential_id.as_deref() {
            keys.push((format!("credential:{credential_id}"), policy.credential_max));
        }
        if let Some(device_subject) = context.device_subject.as_deref() {
            keys.push((format!("device:{device_subject}"), policy.device_max));
        }

        let now = Instant::now();
        let mut state = self.request_state.lock().await;
        state
            .rate_limits
            .retain(|_, window| now.saturating_duration_since(window.started_at) < policy.window);
        if keys.iter().any(|(key, maximum)| {
            *maximum > 0
                && state
                    .rate_limits
                    .get(key)
                    .is_some_and(|window| window.count >= *maximum)
        }) {
            return true;
        }
        for (key, maximum) in keys {
            if maximum == 0 {
                continue;
            }
            let window = state.rate_limits.entry(key).or_insert(RateLimitWindow {
                started_at: now,
                count: 0,
            });
            window.count = window.count.saturating_add(1);
        }
        false
    }

    async fn finish_request(
        &self,
        request_key: &str,
        request_fingerprint: &str,
        response: ActivityActionResponse,
    ) -> ActivityActionResponse {
        let mut state = self.request_state.lock().await;
        let transient = matches!(
            response.error_code,
            Some(
                "ACTIVITY_PROCESSING"
                    | "ACTIVITY_RETRYABLE_FAILURE"
                    | "ACTIVITY_RECONCILIATION_PENDING"
                    | "ACTIVITY_STORAGE_UNAVAILABLE"
                    | "ACTIVITY_RATE_LIMITED"
            )
        );
        match state.seen.get_mut(request_key) {
            Some(seen) if seen.fingerprint == request_fingerprint => {
                seen.processing = false;
                seen.response = (!transient).then(|| response.clone());
            }
            Some(_) => {}
            None => {
                state.seen.insert(
                    request_key.to_string(),
                    SeenRequest {
                        fingerprint: request_fingerprint.to_string(),
                        response: (!transient).then(|| response.clone()),
                        processing: false,
                    },
                );
            }
        }
        response
    }

    fn failed(mut response: ActivityActionResponse, code: &'static str) -> ActivityActionResponse {
        response.error_code = Some(code);
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{
        Activity, ActivityCache, ActivityCacheError, ActivityScope, ActivityType, ActivityVersion,
    };
    use crate::core::inventory::AssetOperation;
    use chrono::{Duration as ChronoDuration, TimeZone};
    use serde_json::json;

    #[test]
    fn reward_items_accept_admin_control_plane_catalog() {
        let public_config = json!({
            "title": "Summer",
            "reward_groups": [{
                "key": "g1",
                "selection_mode": "fixed",
                "items": [{"item_id": 1001, "quantity": 2}]
            }]
        });

        let items = ActivityEngine::reward_items(&public_config, "g1", "character-1").unwrap();
        assert_eq!(
            items,
            vec![NormalizedAssetItem::new(1001, 2, AssetBinding::Unbound).unwrap()]
        );
    }

    struct FailingCache;

    impl ActivityCache for FailingCache {
        fn get_version<'a>(
            &'a self,
            _activity_id: &'a str,
            _version_no: i32,
        ) -> super::super::cache::CacheFuture<'a, Result<Option<ActivityVersion>, ActivityCacheError>>
        {
            Box::pin(async { Err(ActivityCacheError::Unavailable("offline".into())) })
        }

        fn put_version<'a>(
            &'a self,
            _version: &'a ActivityVersion,
        ) -> super::super::cache::CacheFuture<'a, Result<(), ActivityCacheError>> {
            Box::pin(async { Err(ActivityCacheError::Unavailable("offline".into())) })
        }

        fn invalidate_version<'a>(
            &'a self,
            _activity_id: &'a str,
            _version_no: i32,
        ) -> super::super::cache::CacheFuture<'a, Result<(), ActivityCacheError>> {
            Box::pin(async { Err(ActivityCacheError::Unavailable("offline".into())) })
        }

        fn put_activity_list<'a>(
            &'a self,
            _activity_ids: &'a [String],
        ) -> super::super::cache::CacheFuture<'a, Result<(), ActivityCacheError>> {
            Box::pin(async { Err(ActivityCacheError::Unavailable("offline".into())) })
        }

        fn publish_refresh<'a>(
            &'a self,
            _activity_id: &'a str,
            _version_no: i32,
        ) -> super::super::cache::CacheFuture<'a, Result<(), ActivityCacheError>> {
            Box::pin(async { Err(ActivityCacheError::Unavailable("offline".into())) })
        }
    }

    #[derive(Clone)]
    struct BlockingRecordLotteryStore {
        inner: InMemoryLotteryRuntimeStore,
        blocked_key: String,
        block_once: Arc<std::sync::atomic::AtomicBool>,
        entered: Arc<tokio::sync::Barrier>,
        release: Arc<tokio::sync::Notify>,
    }

    impl LotteryRuntimeStore for BlockingRecordLotteryStore {
        fn state<'a>(
            &'a self,
            character_id: &'a str,
            activity_id: &'a str,
            version: i32,
        ) -> LotteryStoreFuture<'a, Result<LotteryState, String>> {
            self.inner.state(character_id, activity_id, version)
        }

        fn record<'a>(
            &'a self,
            record_key: &'a str,
        ) -> LotteryStoreFuture<'a, Result<Option<LotteryDrawRecord>, String>> {
            Box::pin(async move {
                if record_key == self.blocked_key
                    && self
                        .block_once
                        .swap(false, std::sync::atomic::Ordering::SeqCst)
                {
                    self.entered.wait().await;
                    self.release.notified().await;
                }
                self.inner.record(record_key).await
            })
        }

        fn create<'a>(
            &'a self,
            character_id: &'a str,
            activity_id: &'a str,
            version: i32,
            record_key: &'a str,
            record: LotteryDrawRecord,
        ) -> LotteryStoreFuture<'a, Result<LotteryRecordCreate, String>> {
            self.inner
                .create(character_id, activity_id, version, record_key, record)
        }

        fn save<'a>(
            &'a self,
            record_key: &'a str,
            record: LotteryDrawRecord,
        ) -> LotteryStoreFuture<'a, Result<(), String>> {
            self.inner.save(record_key, record)
        }

        fn grant<'a>(
            &'a self,
            character_id: &'a str,
            activity_id: &'a str,
            version: i32,
            record_key: &'a str,
            record: LotteryDrawRecord,
        ) -> LotteryStoreFuture<'a, Result<(), String>> {
            self.inner
                .grant(character_id, activity_id, version, record_key, record)
        }
    }

    #[derive(Clone)]
    struct FakeLotteryGateway {
        voucher: Arc<Mutex<Option<LotteryVoucher>>>,
        result: AssetResultState,
        query_result: Option<AssetResultState>,
        applied: Arc<Mutex<u32>>,
    }

    impl FakeLotteryGateway {
        fn free() -> Self {
            Self {
                voucher: Arc::new(Mutex::new(None)),
                result: AssetResultState::Applied,
                query_result: Some(AssetResultState::Applied),
                applied: Arc::new(Mutex::new(0)),
            }
        }

        fn voucher() -> Self {
            Self {
                voucher: Arc::new(Mutex::new(Some(LotteryVoucher {
                    item_id: 9001,
                    asset_uid: 42,
                    quantity: 10,
                }))),
                result: AssetResultState::Applied,
                query_result: Some(AssetResultState::Applied),
                applied: Arc::new(Mutex::new(0)),
            }
        }
    }

    impl LotteryAssetGateway for FakeLotteryGateway {
        fn find_voucher<'a>(
            &'a self,
            _character_id: &'a str,
            _item_id: i32,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<LotteryVoucher>, String>>
                    + Send
                    + 'a,
            >,
        > {
            let voucher = self.voucher.clone();
            Box::pin(async move { Ok(voucher.lock().await.clone()) })
        }

        fn apply_draw<'a>(
            &'a self,
            _character_id: &'a str,
            _request_id: &'a str,
            exchange: Option<crate::core::reward_source::InventoryRequiredExchange>,
            reward_order: &'a crate::core::inventory::RewardOrder,
            _selection: &'a LotterySelection,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<AssetResultState, LotteryAssetApplyError>>
                    + Send
                    + 'a,
            >,
        > {
            let result = self.result;
            let applied = self.applied.clone();
            Box::pin(async move {
                assert!(reward_order.request_id.starts_with("activity_claim:"));
                assert!(
                    reward_order
                        .request_fingerprint()
                        .as_str()
                        .starts_with("sha256:")
                );
                if let Some(exchange) = exchange {
                    assert!(matches!(
                        exchange.command.operations[0],
                        AssetOperation::Consume { .. }
                    ));
                    assert!(matches!(
                        exchange.command.operations[1],
                        AssetOperation::Grant { .. }
                    ));
                }
                *applied.lock().await += 1;
                Ok(result)
            })
        }

        fn query_draw<'a>(
            &'a self,
            _request_id: &'a str,
            _request_fingerprint: Option<&'a str>,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<AssetResultState>, String>>
                    + Send
                    + 'a,
            >,
        > {
            let result = self.query_result;
            Box::pin(async move { Ok(result) })
        }
    }

    struct FakeLotteryNotifier {
        fail: bool,
        calls: Arc<Mutex<u32>>,
    }

    impl LotteryResultNotifier for FakeLotteryNotifier {
        fn notify<'a>(
            &'a self,
            _record: &'a LotteryDrawRecord,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>
        {
            let fail = self.fail;
            let calls = self.calls.clone();
            Box::pin(async move {
                *calls.lock().await += 1;
                if fail {
                    Err("push unavailable".into())
                } else {
                    Ok(())
                }
            })
        }
    }

    async fn lottery_fixture(
        now: DateTime<Utc>,
        config: serde_json::Value,
        gateway: Arc<dyn LotteryAssetGateway>,
    ) -> ActivityEngine {
        let repo = Arc::new(InMemoryActivityRepository::default());
        publish_lottery(repo.as_ref(), "lottery-1", now, config).await;
        ActivityEngine::new(repo).with_lottery_asset_gateway(gateway)
    }

    async fn publish_lottery(
        repo: &dyn ActivityRepository,
        activity_id: &str,
        now: DateTime<Utc>,
        config: serde_json::Value,
    ) {
        let activity = Activity::new(
            activity_id,
            activity_id,
            ActivityType::new("lottery").unwrap(),
            ActivityScope::Character,
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(1),
            now + ChronoDuration::hours(2),
            "UTC",
        )
        .unwrap();
        let version = ActivityVersion::draft(
            activity.id.clone(),
            1,
            json!({}),
            config,
            activity.start_at,
            activity.end_at,
            activity.claim_deadline,
            "UTC",
        )
        .unwrap();
        repo.save_draft(activity.clone(), version).await.unwrap();
        repo.publish(&activity.id, 1, None).await.unwrap();
    }

    fn lottery_config(free: u32, daily: u32, total: u32) -> serde_json::Value {
        json!({
            "schema_version": 1,
            "draw_source": "player_action",
            "pool_version": 1,
            "free_draw_count": free,
            "voucher_item_id": 9001,
            "daily_draw_limit": daily,
            "total_draw_limit": total,
            "pool_items": [{"item_id": 1001, "quantity": 1, "weight": 1}]
        })
    }

    async fn fixture_with_window(
        now: DateTime<Utc>,
        start_at: DateTime<Utc>,
        end_at: DateTime<Utc>,
    ) -> (ActivityEngine, Arc<InMemoryActivityRepository>) {
        let repo = Arc::new(InMemoryActivityRepository::default());
        let activity = Activity::new(
            "a1",
            "a1",
            ActivityType::new("login_reward").unwrap(),
            ActivityScope::Character,
            start_at,
            end_at,
            now + ChronoDuration::hours(2),
            "UTC",
        )
        .unwrap();
        let version = ActivityVersion::draft(
            activity.id.clone(),
            1,
            json!({}),
            json!({
                "schema_version": 1,
                "event_source": "game_entry",
                "cycle_unit": "natural_day",
                "progression": "consecutive",
                "miss_policy": "reset",
                "claim_mode": "manual",
                "stages": [{"stage_no": 1, "required_count": 1, "reward_group_key": "login-day-1"}]
            }),
            activity.start_at,
            activity.end_at,
            activity.claim_deadline,
            "UTC",
        )
        .unwrap();
        repo.save_draft(activity.clone(), version).await.unwrap();
        repo.publish(&activity.id, 1, None).await.unwrap();
        (ActivityEngine::new(repo.clone()), repo)
    }

    async fn fixture(now: DateTime<Utc>) -> ActivityEngine {
        fixture_with_window(
            now,
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(1),
        )
        .await
        .0
    }

    #[tokio::test]
    async fn list_detail_and_action_apply_server_context_and_idempotency() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let engine = fixture(now).await;
        assert_eq!(engine.list("character-1", now).await.unwrap().len(), 1);
        let detail = engine.detail("character-1", "a1", 1, now).await.unwrap();
        assert_eq!(detail.activity.id, "a1");
        let request = ActivityActionRequest {
            activity_id: "a1".into(),
            version: 1,
            stage_id: "stage-1".into(),
            action_type: "detail".into(),
            client_request_id: "req-1".into(),
        };
        let first = engine
            .dispatch_action("character-1", request.clone(), now)
            .await;
        assert_eq!(first.error_code, Some("ACTIVITY_QUALIFICATION_NOT_MET"));
        let second = engine.dispatch_action("character-1", request, now).await;
        assert!(second.duplicate);
    }

    #[tokio::test]
    async fn identical_action_replay_bypasses_action_rate_limit() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let mut engine = fixture(now).await;
        engine.rate_limit_policy = ActivityRateLimitPolicy {
            window: Duration::from_secs(1),
            character_max: 100,
            account_max: 100,
            source_max: 100,
            credential_max: 100,
            device_max: 100,
            activity_max: 100,
            action_max: 1,
        };
        let request = ActivityActionRequest {
            activity_id: "a1".into(),
            version: 1,
            stage_id: "stage-1".into(),
            action_type: "detail".into(),
            client_request_id: "idempotent-replay".into(),
        };

        let first = engine
            .dispatch_action("character-1", request.clone(), now)
            .await;
        assert_eq!(first.error_code, Some("ACTIVITY_QUALIFICATION_NOT_MET"));

        let replay = engine
            .dispatch_action("character-1", request.clone(), now)
            .await;
        assert!(replay.duplicate);
        assert_eq!(replay.error_code, first.error_code);

        let different_request = engine
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    client_request_id: "new-request".into(),
                    ..request
                },
                now,
            )
            .await;
        assert_eq!(different_request.error_code, Some("ACTIVITY_RATE_LIMITED"));
    }

    #[tokio::test]
    async fn rejects_auth_version_and_rate_limit_boundaries() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let engine = fixture(now).await;
        assert_eq!(
            engine.list("", now).await.unwrap_err().code,
            "ACTIVITY_AUTH_REQUIRED"
        );
        assert_eq!(
            engine
                .detail("character-1", "not-owned-activity", 0, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_NOT_FOUND"
        );
        let unauthorized_action = engine
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "not-owned-activity".into(),
                    version: 0,
                    stage_id: "stage-1".into(),
                    action_type: "claim".into(),
                    client_request_id: "unauthorized-activity".into(),
                },
                now,
            )
            .await;
        assert_eq!(unauthorized_action.error_code, Some("ACTIVITY_NOT_FOUND"));
        let request = ActivityActionRequest {
            activity_id: "a1".into(),
            version: 1,
            stage_id: "".into(),
            action_type: "detail".into(),
            client_request_id: "req-2".into(),
        };
        let first = engine.dispatch_action("character-1", request, now).await;
        assert_eq!(first.error_code, Some("ACTIVITY_QUALIFICATION_NOT_MET"));
        engine.list("character-1", now).await.unwrap();
        assert_eq!(
            engine.list("character-1", now).await.unwrap_err().code,
            "ACTIVITY_RATE_LIMITED"
        );
        engine.detail("character-1", "a1", 1, now).await.unwrap();
        assert_eq!(
            engine
                .detail("character-1", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_RATE_LIMITED"
        );
    }

    #[tokio::test]
    async fn enforces_lifecycle_and_character_boundaries() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let (not_started, _) = fixture_with_window(
            now,
            now + ChronoDuration::hours(1),
            now + ChronoDuration::hours(2),
        )
        .await;
        assert_eq!(
            not_started
                .detail("character-1", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_NOT_STARTED"
        );
        let not_started_action = not_started
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 1,
                    stage_id: "stage-1".into(),
                    action_type: "claim".into(),
                    client_request_id: "not-started-action".into(),
                },
                now,
            )
            .await;
        assert_eq!(not_started_action.error_code, Some("ACTIVITY_NOT_STARTED"));
        let (ended, _) = fixture_with_window(
            now,
            now - ChronoDuration::hours(2),
            now - ChronoDuration::hours(1),
        )
        .await;
        assert_eq!(
            ended
                .detail("character-1", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_ENDED"
        );
        let ended_action = ended
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 1,
                    stage_id: "stage-1".into(),
                    action_type: "claim".into(),
                    client_request_id: "ended-action".into(),
                },
                now,
            )
            .await;
        assert_eq!(ended_action.error_code, Some("ACTIVITY_ENDED"));
        let (offline, repo) = fixture_with_window(
            now,
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(1),
        )
        .await;
        repo.offline("a1", 1).await.unwrap();
        assert_eq!(
            offline
                .detail("character-1", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_OFFLINE"
        );
        let offline_action = offline
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 1,
                    stage_id: "stage-1".into(),
                    action_type: "claim".into(),
                    client_request_id: "offline-action".into(),
                },
                now,
            )
            .await;
        assert_eq!(offline_action.error_code, Some("ACTIVITY_OFFLINE"));

        let request = ActivityActionRequest {
            activity_id: "a1".into(),
            version: 1,
            stage_id: "stage-1".into(),
            action_type: "detail".into(),
            client_request_id: "same-request".into(),
        };
        let (engine, _) = fixture_with_window(
            now,
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(1),
        )
        .await;
        let invalid_version = engine
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 9,
                    stage_id: "stage-1".into(),
                    action_type: "version-check".into(),
                    client_request_id: "invalid-version-action".into(),
                },
                now,
            )
            .await;
        assert_eq!(invalid_version.error_code, Some("ACTIVITY_INVALID_VERSION"));
        let invalid_version_replay = engine
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 9,
                    stage_id: "stage-1".into(),
                    action_type: "version-check".into(),
                    client_request_id: "invalid-version-action".into(),
                },
                now,
            )
            .await;
        assert!(invalid_version_replay.duplicate);
        assert_eq!(
            invalid_version_replay.error_code,
            Some("ACTIVITY_INVALID_VERSION")
        );
        let corrected_with_same_request_id = engine
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 1,
                    stage_id: "stage-1".into(),
                    action_type: "version-check".into(),
                    client_request_id: "invalid-version-action".into(),
                },
                now,
            )
            .await;
        assert_eq!(
            corrected_with_same_request_id.error_code,
            Some("REQUEST_FINGERPRINT_CONFLICT")
        );
        let first = engine
            .dispatch_action("character-1", request.clone(), now)
            .await;
        let other_character = engine
            .dispatch_action("character-2", request.clone(), now)
            .await;
        let duplicate = engine.dispatch_action("character-1", request, now).await;
        assert!(!first.duplicate);
        assert!(!other_character.duplicate);
        assert!(duplicate.duplicate);

        let fingerprint_conflict = engine
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 1,
                    stage_id: "forged-stage".into(),
                    action_type: "claim".into(),
                    client_request_id: "same-request".into(),
                },
                now,
            )
            .await;
        assert_eq!(
            fingerprint_conflict.error_code,
            Some("REQUEST_FINGERPRINT_CONFLICT")
        );
    }

    #[tokio::test]
    async fn activity_rate_limit_covers_identity_source_credential_activity_and_action_keys() {
        fn context(
            character: &str,
            account: &str,
            source: &str,
            credential: &str,
            device: &str,
        ) -> ActivityRequestContext {
            ActivityRequestContext::authenticated(
                character,
                account,
                source,
                Some(credential),
                Some(device),
            )
        }

        fn policy() -> ActivityRateLimitPolicy {
            ActivityRateLimitPolicy {
                window: Duration::from_secs(1),
                character_max: 100,
                account_max: 100,
                source_max: 100,
                credential_max: 100,
                device_max: 100,
                activity_max: 100,
                action_max: 100,
            }
        }

        let dimensions = [
            (
                "character",
                ActivityRateLimitPolicy {
                    character_max: 1,
                    ..policy()
                },
                context("c1", "a1", "127.0.0.1:1001", "t1", "d1"),
                context("c1", "a2", "127.0.0.2:1002", "t2", "d2"),
                ("activity-1", "list"),
                ("activity-2", "detail"),
            ),
            (
                "account",
                ActivityRateLimitPolicy {
                    account_max: 1,
                    ..policy()
                },
                context("c1", "account", "127.0.0.1:1001", "t1", "d1"),
                context("c2", "account", "127.0.0.2:1002", "t2", "d2"),
                ("activity-1", "list"),
                ("activity-2", "detail"),
            ),
            (
                "source",
                ActivityRateLimitPolicy {
                    source_max: 1,
                    ..policy()
                },
                context("c1", "a1", "127.0.0.1:1001", "t1", "d1"),
                context("c2", "a2", "127.0.0.1:2002", "t2", "d2"),
                ("activity-1", "list"),
                ("activity-2", "detail"),
            ),
            (
                "credential",
                ActivityRateLimitPolicy {
                    credential_max: 1,
                    ..policy()
                },
                context("c1", "a1", "127.0.0.1:1001", "ticket", "d1"),
                context("c2", "a2", "127.0.0.2:1002", "ticket", "d2"),
                ("activity-1", "list"),
                ("activity-2", "detail"),
            ),
            (
                "device",
                ActivityRateLimitPolicy {
                    device_max: 1,
                    ..policy()
                },
                context("c1", "a1", "127.0.0.1:1001", "t1", "device"),
                context("c2", "a2", "127.0.0.2:1002", "t2", "device"),
                ("activity-1", "list"),
                ("activity-2", "detail"),
            ),
            (
                "activity",
                ActivityRateLimitPolicy {
                    activity_max: 1,
                    ..policy()
                },
                context("c1", "a1", "127.0.0.1:1001", "t1", "d1"),
                context("c1", "a2", "127.0.0.2:1002", "t2", "d2"),
                ("activity-1", "list"),
                ("activity-1", "detail"),
            ),
            (
                "action",
                ActivityRateLimitPolicy {
                    action_max: 1,
                    ..policy()
                },
                context("c1", "a1", "127.0.0.1:1001", "t1", "d1"),
                context("c1", "a2", "127.0.0.2:1002", "t2", "d2"),
                ("activity-1", "claim"),
                ("activity-1", "claim"),
            ),
        ];

        for (dimension, rate_limit_policy, first, second, first_key, second_key) in dimensions {
            let mut engine = ActivityEngine::in_memory();
            engine.rate_limit_policy = rate_limit_policy;
            assert!(
                !engine
                    .check_rate_limit(&first, first_key.0, first_key.1)
                    .await,
                "first {dimension} request must be admitted"
            );
            assert!(
                engine
                    .check_rate_limit(&second, second_key.0, second_key.1)
                    .await,
                "second {dimension} request must be limited"
            );
        }
    }

    #[tokio::test]
    async fn disabled_engine_returns_explicit_unavailable_error() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let engine = ActivityEngine::disabled();
        assert_eq!(
            engine.list("character-1", now).await.unwrap_err().code,
            "ACTIVITY_ENGINE_UNAVAILABLE"
        );
        let response = engine
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 1,
                    stage_id: "stage-1".into(),
                    action_type: "detail".into(),
                    client_request_id: "req-1".into(),
                },
                now,
            )
            .await;
        assert_eq!(response.error_code, Some("ACTIVITY_ENGINE_UNAVAILABLE"));
        assert_eq!(
            engine
                .on_game_entry("character-1", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_ENGINE_UNAVAILABLE"
        );
    }

    #[tokio::test]
    async fn unavailable_cache_falls_back_to_repository_truth() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let engine = fixture(now).await.with_cache(Arc::new(FailingCache));

        let list = engine.list("character-1", now).await.unwrap();
        let detail = engine.detail("character-1", "a1", 1, now).await.unwrap();

        assert_eq!(list.len(), 1);
        assert_eq!(detail.activity.id, "a1");
        assert_eq!(detail.version.version_no, 1);
    }

    #[tokio::test]
    async fn lottery_draw_consumes_free_once_and_replays_same_request() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let gateway = Arc::new(FakeLotteryGateway::free());
        let metrics = Box::leak(Box::new(MetricsCollector::new()));
        let engine = lottery_fixture(now, lottery_config(1, 2, 2), gateway.clone())
            .await
            .with_metrics(metrics);
        let request = ActivityActionRequest {
            activity_id: "lottery-1".into(),
            version: 1,
            stage_id: String::new(),
            action_type: "draw".into(),
            client_request_id: "draw-free-1".into(),
        };
        let first = engine
            .dispatch_action("character-1", request.clone(), now)
            .await;
        assert!(first.ok);
        assert_eq!(*gateway.applied.lock().await, 1);
        let record = engine
            .lottery_states
            .record(&lottery_record_key(
                "character-1",
                "lottery-1",
                1,
                "draw-free-1",
            ))
            .await
            .unwrap()
            .unwrap();
        assert!(
            record
                .reward_order
                .request_id
                .starts_with("activity_claim:")
        );
        assert!(
            record
                .reward_order
                .request_fingerprint()
                .as_str()
                .starts_with("sha256:")
        );
        let duplicate = engine.dispatch_action("character-1", request, now).await;
        assert!(duplicate.ok);
        assert!(duplicate.duplicate);
        assert_eq!(*gateway.applied.lock().await, 1);
        let metric_fields = metrics
            .drain_activity_fields()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(
            metric_fields
                .get("activity_reward_delivery_count")
                .map(String::as_str),
            Some("1")
        );
    }

    #[tokio::test]
    async fn lottery_request_id_is_character_scoped_across_activities() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let gateway = Arc::new(FakeLotteryGateway::free());
        let engine = lottery_fixture(now, lottery_config(1, 2, 2), gateway.clone()).await;
        publish_lottery(
            engine.repository.as_ref(),
            "lottery-2",
            now,
            lottery_config(1, 2, 2),
        )
        .await;
        let request = |activity_id: &str| ActivityActionRequest {
            activity_id: activity_id.into(),
            version: 1,
            stage_id: String::new(),
            action_type: "draw".into(),
            client_request_id: "shared-request-id".into(),
        };

        let first = engine
            .dispatch_action("character-1", request("lottery-1"), now)
            .await;
        assert!(first.ok);

        let cross_activity = engine
            .dispatch_action("character-1", request("lottery-2"), now)
            .await;
        assert_eq!(
            cross_activity.error_code,
            Some("REQUEST_FINGERPRINT_CONFLICT")
        );

        let other_character = engine
            .dispatch_action("character-2", request("lottery-2"), now)
            .await;
        assert!(other_character.ok);
        assert!(!other_character.duplicate);
        assert_eq!(*gateway.applied.lock().await, 2);
    }

    #[tokio::test]
    async fn shared_runtime_rejects_cross_instance_request_binding_conflict() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let repository = Arc::new(InMemoryActivityRepository::default());
        publish_lottery(
            repository.as_ref(),
            "lottery-1",
            now,
            lottery_config(1, 2, 2),
        )
        .await;
        publish_lottery(
            repository.as_ref(),
            "lottery-2",
            now,
            lottery_config(1, 2, 2),
        )
        .await;
        let runtime = Arc::new(InMemoryLotteryRuntimeStore::default());
        let gateway = Arc::new(FakeLotteryGateway::free());
        let engine_a = ActivityEngine::new(repository.clone())
            .with_lottery_runtime_store(runtime.clone())
            .with_lottery_asset_gateway(gateway.clone());
        let engine_b = ActivityEngine::new(repository)
            .with_lottery_runtime_store(runtime)
            .with_lottery_asset_gateway(gateway.clone());
        let request = |activity_id: &str| ActivityActionRequest {
            activity_id: activity_id.into(),
            version: 1,
            stage_id: String::new(),
            action_type: "draw".into(),
            client_request_id: "cross-instance-conflict".into(),
        };

        let first = engine_a
            .dispatch_action("character-1", request("lottery-1"), now)
            .await;
        let conflict = engine_b
            .dispatch_action("character-1", request("lottery-2"), now)
            .await;
        let replayed_conflict = engine_b
            .dispatch_action("character-1", request("lottery-2"), now)
            .await;

        assert!(first.ok);
        assert_eq!(conflict.error_code, Some("REQUEST_FINGERPRINT_CONFLICT"));
        assert_eq!(
            replayed_conflict.error_code,
            Some("REQUEST_FINGERPRINT_CONFLICT")
        );
        assert_eq!(*gateway.applied.lock().await, 1);
    }

    #[tokio::test]
    async fn slow_lottery_record_lookup_does_not_hold_global_request_lock() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let (engine, repository) = fixture_with_window(
            now,
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(1),
        )
        .await;
        publish_lottery(
            repository.as_ref(),
            "lottery-1",
            now,
            lottery_config(1, 2, 2),
        )
        .await;
        let entered = Arc::new(tokio::sync::Barrier::new(2));
        let release = Arc::new(tokio::sync::Notify::new());
        let runtime = Arc::new(BlockingRecordLotteryStore {
            inner: InMemoryLotteryRuntimeStore::default(),
            blocked_key: lottery_record_key("blocked-character", "lottery-1", 1, "blocked-draw"),
            block_once: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            entered: entered.clone(),
            release: release.clone(),
        });
        let engine = Arc::new(
            engine
                .with_lottery_runtime_store(runtime)
                .with_lottery_asset_gateway(Arc::new(FakeLotteryGateway::free())),
        );
        let blocked_engine = engine.clone();
        let blocked = tokio::spawn(async move {
            blocked_engine
                .dispatch_action(
                    "blocked-character",
                    ActivityActionRequest {
                        activity_id: "lottery-1".into(),
                        version: 1,
                        stage_id: String::new(),
                        action_type: "draw".into(),
                        client_request_id: "blocked-draw".into(),
                    },
                    now,
                )
                .await
        });
        entered.wait().await;

        let independent = tokio::time::timeout(
            Duration::from_secs(1),
            engine.dispatch_action(
                "independent-character",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 1,
                    stage_id: "stage-1".into(),
                    action_type: "detail".into(),
                    client_request_id: "independent-request".into(),
                },
                now,
            ),
        )
        .await
        .expect("independent request must not wait for lottery storage");
        release.notify_one();
        let blocked = blocked.await.unwrap();

        assert_eq!(
            independent.error_code,
            Some("ACTIVITY_QUALIFICATION_NOT_MET")
        );
        assert!(blocked.ok);
    }

    #[tokio::test]
    async fn lottery_voucher_path_uses_atomic_exchange_and_limit() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let gateway = Arc::new(FakeLotteryGateway::voucher());
        let engine = lottery_fixture(now, lottery_config(0, 1, 1), gateway.clone()).await;
        let request = |id: &str| ActivityActionRequest {
            activity_id: "lottery-1".into(),
            version: 1,
            stage_id: String::new(),
            action_type: "draw".into(),
            client_request_id: id.into(),
        };
        let first = engine
            .dispatch_action("character-1", request("draw-voucher-1"), now)
            .await;
        assert!(first.ok);
        assert_eq!(*gateway.applied.lock().await, 1);
        tokio::time::sleep(std::time::Duration::from_millis(110)).await;
        let limited = engine
            .dispatch_action("character-1", request("draw-voucher-2"), now)
            .await;
        assert_eq!(limited.error_code, Some("ACTIVITY_QUALIFICATION_NOT_MET"));
        assert_eq!(*gateway.applied.lock().await, 1);
    }

    #[tokio::test]
    async fn lottery_distinct_concurrent_requests_cannot_bypass_limits() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let gateway = Arc::new(FakeLotteryGateway::free());
        let engine = lottery_fixture(now, lottery_config(1, 1, 1), gateway.clone()).await;
        let request = |id: &str| ActivityActionRequest {
            activity_id: "lottery-1".into(),
            version: 1,
            stage_id: String::new(),
            action_type: "draw".into(),
            client_request_id: id.into(),
        };
        let (first, second) = tokio::join!(
            engine.dispatch_action("character-1", request("concurrent-a"), now),
            engine.dispatch_action("character-1", request("concurrent-b"), now)
        );
        assert_eq!((first.ok as usize) + (second.ok as usize), 1);
        assert!(first.error_code.is_some() || second.error_code.is_some());
        assert_eq!(*gateway.applied.lock().await, 1);
    }

    #[tokio::test]
    async fn two_engines_share_lottery_fact_store_and_deliver_once() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let repository = Arc::new(InMemoryActivityRepository::default());
        publish_lottery(
            repository.as_ref(),
            "lottery-1",
            now,
            lottery_config(1, 1, 1),
        )
        .await;
        let runtime = Arc::new(InMemoryLotteryRuntimeStore::default());
        let gateway = Arc::new(FakeLotteryGateway::free());
        let engine_a = ActivityEngine::new(repository.clone())
            .with_lottery_runtime_store(runtime.clone())
            .with_lottery_asset_gateway(gateway.clone());
        let engine_b = ActivityEngine::new(repository)
            .with_lottery_runtime_store(runtime)
            .with_lottery_asset_gateway(gateway.clone());
        let request = ActivityActionRequest {
            activity_id: "lottery-1".into(),
            version: 1,
            stage_id: String::new(),
            action_type: "draw".into(),
            client_request_id: "cross-instance-replay".into(),
        };

        let first = engine_a
            .dispatch_action("character-1", request.clone(), now)
            .await;
        let replay = engine_b.dispatch_action("character-1", request, now).await;

        assert!(first.ok);
        assert!(replay.ok);
        assert!(replay.duplicate);
        assert_eq!(*gateway.applied.lock().await, 1);
    }

    #[tokio::test]
    async fn two_engines_cannot_bypass_shared_lottery_limit() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let repository = Arc::new(InMemoryActivityRepository::default());
        publish_lottery(
            repository.as_ref(),
            "lottery-1",
            now,
            lottery_config(1, 1, 1),
        )
        .await;
        let runtime = Arc::new(InMemoryLotteryRuntimeStore::default());
        let gateway = Arc::new(FakeLotteryGateway::free());
        let engine_a = ActivityEngine::new(repository.clone())
            .with_lottery_runtime_store(runtime.clone())
            .with_lottery_asset_gateway(gateway.clone());
        let engine_b = ActivityEngine::new(repository)
            .with_lottery_runtime_store(runtime)
            .with_lottery_asset_gateway(gateway.clone());
        let request = |id: &str| ActivityActionRequest {
            activity_id: "lottery-1".into(),
            version: 1,
            stage_id: String::new(),
            action_type: "draw".into(),
            client_request_id: id.into(),
        };

        let (first, second) = tokio::join!(
            engine_a.dispatch_action("character-1", request("instance-a"), now),
            engine_b.dispatch_action("character-1", request("instance-b"), now),
        );

        assert_eq!(usize::from(first.ok) + usize::from(second.ok), 1);
        assert_eq!(*gateway.applied.lock().await, 1);
    }

    #[tokio::test]
    async fn restarted_engine_queries_original_lottery_request_without_redraw() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let repository = Arc::new(InMemoryActivityRepository::default());
        publish_lottery(
            repository.as_ref(),
            "lottery-1",
            now,
            lottery_config(1, 2, 2),
        )
        .await;
        let runtime = Arc::new(InMemoryLotteryRuntimeStore::default());
        let gateway = Arc::new(FakeLotteryGateway {
            voucher: Arc::new(Mutex::new(None)),
            result: AssetResultState::Unknown,
            query_result: Some(AssetResultState::Applied),
            applied: Arc::new(Mutex::new(0)),
        });
        let first_engine = ActivityEngine::new(repository.clone())
            .with_lottery_runtime_store(runtime.clone())
            .with_lottery_asset_gateway(gateway.clone());
        let request = ActivityActionRequest {
            activity_id: "lottery-1".into(),
            version: 1,
            stage_id: String::new(),
            action_type: "draw".into(),
            client_request_id: "lost-response".into(),
        };
        let pending = first_engine
            .dispatch_action("character-1", request.clone(), now)
            .await;
        let key = lottery_record_key("character-1", "lottery-1", 1, "lost-response");
        let original = runtime.record(&key).await.unwrap().unwrap();

        let restarted = ActivityEngine::new(repository)
            .with_lottery_runtime_store(runtime.clone())
            .with_lottery_asset_gateway(gateway.clone());
        let recovered = restarted.dispatch_action("character-1", request, now).await;
        let final_record = runtime.record(&key).await.unwrap().unwrap();

        assert_eq!(pending.error_code, Some("ACTIVITY_RECONCILIATION_PENDING"));
        assert!(recovered.ok);
        assert_eq!(*gateway.applied.lock().await, 1);
        assert_eq!(final_record.selection, original.selection);
        assert_eq!(final_record.reward_order, original.reward_order);
        assert_eq!(final_record.asset_request_id, original.asset_request_id);
    }

    #[tokio::test]
    async fn lottery_retryable_reuses_original_selection_without_randomizing() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let gateway = Arc::new(FakeLotteryGateway {
            voucher: Arc::new(Mutex::new(None)),
            result: AssetResultState::NotApplied,
            query_result: None,
            applied: Arc::new(Mutex::new(0)),
        });
        let engine = lottery_fixture(now, lottery_config(1, 2, 2), gateway.clone()).await;
        let request = ActivityActionRequest {
            activity_id: "lottery-1".into(),
            version: 1,
            stage_id: String::new(),
            action_type: "draw".into(),
            client_request_id: "draw-retryable".into(),
        };
        let first = engine
            .dispatch_action("character-1", request.clone(), now)
            .await;
        assert_eq!(first.error_code, Some("ACTIVITY_RETRYABLE_FAILURE"));
        let key = lottery_record_key("character-1", "lottery-1", 1, "draw-retryable");
        let selection = engine
            .lottery_states
            .record(&key)
            .await
            .unwrap()
            .unwrap()
            .selection;
        let second = engine.dispatch_action("character-1", request, now).await;
        assert_eq!(second.error_code, Some("ACTIVITY_RETRYABLE_FAILURE"));
        assert_eq!(
            engine
                .lottery_states
                .record(&key)
                .await
                .unwrap()
                .unwrap()
                .selection,
            selection
        );
        assert_eq!(*gateway.applied.lock().await, 2);
    }

    #[tokio::test]
    async fn lottery_unknown_queries_original_request_and_push_failure_keeps_granted() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let gateway = Arc::new(FakeLotteryGateway {
            voucher: Arc::new(Mutex::new(None)),
            result: AssetResultState::Unknown,
            query_result: Some(AssetResultState::Applied),
            applied: Arc::new(Mutex::new(0)),
        });
        let notifier = Arc::new(FakeLotteryNotifier {
            fail: true,
            calls: Arc::new(Mutex::new(0)),
        });
        let engine = lottery_fixture(now, lottery_config(1, 2, 2), gateway.clone())
            .await
            .with_lottery_result_notifier(notifier.clone());
        let request = ActivityActionRequest {
            activity_id: "lottery-1".into(),
            version: 1,
            stage_id: String::new(),
            action_type: "draw".into(),
            client_request_id: "draw-unknown".into(),
        };
        let pending = engine
            .dispatch_action("character-1", request.clone(), now)
            .await;
        assert_eq!(pending.error_code, Some("ACTIVITY_RECONCILIATION_PENDING"));
        let granted = engine.dispatch_action("character-1", request, now).await;
        assert!(granted.ok);
        assert_eq!(*gateway.applied.lock().await, 1);
        assert_eq!(*notifier.calls.lock().await, 1);
        let record = engine
            .lottery_states
            .record(&lottery_record_key(
                "character-1",
                "lottery-1",
                1,
                "draw-unknown",
            ))
            .await
            .unwrap()
            .unwrap();
        assert!(record.notification_failed);
        assert_eq!(record.status, LotteryDrawStatus::Granted);
    }

    #[test]
    fn lottery_record_keys_do_not_alias_delimiter_containing_ids() {
        let first = lottery_record_key("character:one", "lottery", 1, "draw:one");
        let second = lottery_record_key("character", "one:lottery", 1, "draw:one");
        let third = lottery_record_key("character:one", "lottery", 1, "draw:one:extra");

        assert_ne!(first, second);
        assert_ne!(first, third);
        assert!(first.starts_with("activity-lottery:sha256:"));
    }

    #[tokio::test]
    async fn lottery_draw_rejects_ended_and_offline_activity() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let gateway = Arc::new(FakeLotteryGateway::free());
        let ended = lottery_fixture(
            now - ChronoDuration::hours(3),
            lottery_config(1, 1, 1),
            gateway.clone(),
        )
        .await;
        let request = ActivityActionRequest {
            activity_id: "lottery-1".into(),
            version: 1,
            stage_id: String::new(),
            action_type: "draw".into(),
            client_request_id: "draw-ended".into(),
        };
        let ended_response = ended
            .dispatch_action("character-1", request.clone(), now)
            .await;
        assert_eq!(ended_response.error_code, Some("ACTIVITY_ENDED"));

        let offline = lottery_fixture(now, lottery_config(1, 1, 1), gateway.clone()).await;
        // The published snapshot is running at `now`; taking the activity
        // offline exercises the lifecycle gate before lottery qualification.
        offline.repository.offline("lottery-1", 1).await.unwrap();
        let offline_response = offline.dispatch_action("character-1", request, now).await;
        assert_eq!(offline_response.error_code, Some("ACTIVITY_OFFLINE"));
        assert_eq!(*gateway.applied.lock().await, 0);
    }

    #[tokio::test]
    async fn trusted_game_entry_updates_login_progress_and_is_idempotent() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let engine = fixture(now).await;
        let first = engine
            .on_game_entry("character-1", "a1", 1, now)
            .await
            .unwrap();
        assert!(!first.duplicate);
        assert_eq!(first.state.cumulative_count, 1);
        assert_eq!(first.state.consecutive_count, 1);
        assert_eq!(first.current_stage_no, Some(1));

        let duplicate = engine
            .on_game_entry("character-1", "a1", 1, now + ChronoDuration::minutes(10))
            .await
            .unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.state.cumulative_count, 1);
        assert_eq!(duplicate.state_revision, first.state_revision);
    }

    #[tokio::test]
    async fn concurrent_trusted_game_entry_advances_progress_once() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let engine = fixture(now).await;
        let (left, right) = tokio::join!(
            engine.on_game_entry("character-1", "a1", 1, now),
            engine.on_game_entry("character-1", "a1", 1, now + ChronoDuration::minutes(1))
        );
        let left = left.unwrap();
        let right = right.unwrap();
        assert_eq!((left.duplicate as u8) + (right.duplicate as u8), 1);
        let (state, revision) = engine
            .login_progress
            .load("character-1", "a1", 1)
            .await
            .unwrap();
        assert_eq!(state.cumulative_count, 1);
        assert_eq!(state.consecutive_count, 1);
        assert_eq!(revision, 1);
    }

    #[tokio::test]
    async fn trusted_game_entry_rejects_version_lifecycle_and_identity_boundaries() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let engine = fixture(now).await;
        assert_eq!(
            engine
                .on_game_entry("character-1", "a1", 9, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_INVALID_VERSION"
        );
        assert_eq!(
            engine
                .on_game_entry("", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_AUTH_REQUIRED"
        );

        let (ended, _) = fixture_with_window(
            now,
            now - ChronoDuration::hours(2),
            now - ChronoDuration::hours(1),
        )
        .await;
        assert_eq!(
            ended
                .on_game_entry("character-1", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_ENDED"
        );

        let (offline, repo) = fixture_with_window(
            now,
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(1),
        )
        .await;
        repo.offline("a1", 1).await.unwrap();
        assert_eq!(
            offline
                .on_game_entry("character-1", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_OFFLINE"
        );
    }
}
