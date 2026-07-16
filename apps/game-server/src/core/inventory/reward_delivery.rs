use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::sync::Mutex;

use crate::core::config_table::ConfigTableRuntime;
use crate::core::global_id::ItemUidGenerator;
use crate::core::player::db_player_store::GrantRecordLookup;
use crate::core::player::grant_contract::{GrantItemIntent, GrantResultSummary};
use crate::core::player::{PlayerManager, player_manager::GrantItemsError};
use crate::csv_code::itemtable::ItemTable;

use super::{
    AssetBinding, AssetCommandErrorCode, AssetCommandResult, AssetContainer, AssetContainerVersion,
    AssetDeliveryMethod, AssetDeliveryReceipt, AssetFallbackReason, AssetQuantityDelta,
    AssetRequestFingerprint, AssetResultState, AssetType, Item, ItemError, RewardDeliveryPolicy,
    RewardDeliveryResult, RewardOrder,
};

/// Runtime schema for the reward-delivery control plane. `character_asset_requests` remains the
/// authoritative inventory request record; these tables retain the delivery decision and the
/// durable mail-create intent around it.
pub const REWARD_DELIVERY_SCHEMA_STATEMENTS: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS reward_delivery_records (
        request_id varchar(128) PRIMARY KEY,
        character_id varchar(128) NOT NULL,
        request_fingerprint varchar(71) NOT NULL,
        result_json jsonb NOT NULL,
        created_at timestamptz NOT NULL DEFAULT current_timestamp
    )"#,
    "CREATE INDEX IF NOT EXISTS idx_reward_delivery_records_character_id ON reward_delivery_records (character_id, created_at DESC)",
    r#"CREATE TABLE IF NOT EXISTS reward_mail_outbox (
        delivery_request_id varchar(128) PRIMARY KEY,
        reward_request_id varchar(128) NOT NULL,
        mail_id varchar(64) NOT NULL,
        character_id varchar(128) NOT NULL,
        request_fingerprint varchar(71) NOT NULL,
        origin_type varchar(32) NOT NULL,
        origin_id varchar(128) NOT NULL,
        delivery_policy varchar(32) NOT NULL,
        items_json jsonb NOT NULL,
        reason varchar(512) NOT NULL,
        operator_json jsonb NOT NULL,
        status varchar(32) NOT NULL DEFAULT 'pending',
        created_at timestamptz NOT NULL DEFAULT current_timestamp,
        CONSTRAINT uk_reward_mail_outbox_reward_request UNIQUE (reward_request_id),
        CONSTRAINT uk_reward_mail_outbox_mail_id UNIQUE (mail_id)
    )"#,
    "ALTER TABLE reward_mail_outbox ADD COLUMN IF NOT EXISTS delivery_policy varchar(32) NOT NULL DEFAULT 'MAIL_ONLY'",
    "CREATE INDEX IF NOT EXISTS idx_reward_mail_outbox_pending ON reward_mail_outbox (status, created_at)",
    "CREATE INDEX IF NOT EXISTS idx_reward_mail_outbox_character_id ON reward_mail_outbox (character_id, created_at DESC)",
];

/// The only input trusted reward sources may submit. They never choose item UIDs, JSONB
/// snapshots, a direct inventory mutation, or a concrete mail id.
pub trait RewardInventoryPort: Send + Sync {
    async fn execute_reward(
        &self,
        order: &RewardOrder,
    ) -> Result<AssetCommandResult, RewardInventoryPortError>;

    /// Query the original inventory request after an uncertain execution result. Returning
    /// `None` means it is still not safe to infer a delivery result.
    async fn query_reward(
        &self,
        request_id: &str,
        request_fingerprint: &AssetRequestFingerprint,
    ) -> Result<Option<AssetCommandResult>, RewardInventoryPortError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewardInventoryPortError {
    pub message: String,
}

impl RewardInventoryPortError {
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Production bridge from a trusted reward order into the existing character asset transaction.
/// It owns item materialization and UID allocation, so sources never receive an `Item` or a
/// mutable `PlayerData` snapshot. Callers construct this once from the runtime dependencies and
/// pass it to [`RewardDeliveryService`]; source migration itself remains a later phase.
#[derive(Clone)]
pub struct PlayerManagerRewardInventoryPort {
    player_manager: PlayerManager,
    config_tables: ConfigTableRuntime,
    item_uid_generator: ItemUidGenerator,
}

impl PlayerManagerRewardInventoryPort {
    pub fn new(
        player_manager: PlayerManager,
        config_tables: ConfigTableRuntime,
        item_uid_generator: ItemUidGenerator,
    ) -> Self {
        Self {
            player_manager,
            config_tables,
            item_uid_generator,
        }
    }

    fn result_from_outcome(
        &self,
        order: &RewardOrder,
        outcome: crate::core::player::player_manager::GrantItemsOutcome,
    ) -> Result<AssetCommandResult, RewardInventoryPortError> {
        if outcome.record.character_id != order.character_id
            || outcome.record.request_fingerprint != order.request_fingerprint().as_str()
        {
            return AssetCommandResult::not_applied(
                &order.request_id,
                order.request_fingerprint(),
                AssetCommandErrorCode::RequestFingerprintConflict,
            )
            .map_err(invalid_inventory_result);
        }

        let actual_deltas = reward_item_deltas(order);
        let container_versions = outcome
            .player_data
            .as_ref()
            .map(|player_data| {
                vec![AssetContainerVersion {
                    container: AssetContainer::Inventory,
                    version: player_data.persistence_revision(),
                }]
            })
            .unwrap_or_default();

        // The current ledger schema uses generated numeric row ids and the stage-three grant
        // API does not return them. The durable request id remains the stable correlation key;
        // stage nine can expose the concrete ledger-row ids without weakening delivery safety.
        AssetCommandResult::applied(
            &order.request_id,
            order.request_fingerprint(),
            actual_deltas,
            container_versions,
            Vec::new(),
            None,
        )
        .map_err(invalid_inventory_result)
    }

    fn result_from_grant_error(
        &self,
        order: &RewardOrder,
        error: GrantItemsError,
    ) -> Result<AssetCommandResult, RewardInventoryPortError> {
        match error.error_code {
            // This is the sole inventory error proven by `plan_add_items` to be a fully
            // uncommitted capacity failure. The delivery service may mail-fallback only here.
            "INVENTORY_FULL" => AssetCommandResult::not_applied(
                &order.request_id,
                order.request_fingerprint(),
                AssetCommandErrorCode::InventoryCapacityFull,
            )
            .map_err(invalid_inventory_result),
            "REQUEST_FINGERPRINT_CONFLICT" => AssetCommandResult::not_applied(
                &order.request_id,
                order.request_fingerprint(),
                AssetCommandErrorCode::RequestFingerprintConflict,
            )
            .map_err(invalid_inventory_result),
            // The manager already treats failed commit/result reads as uncertain. Keep that
            // state explicit so `RewardDeliveryService` queries the original request before it
            // considers any retry or fallback.
            _ if error.result_state == "unknown" => {
                AssetCommandResult::unknown(&order.request_id, order.request_fingerprint())
                    .map_err(invalid_inventory_result)
            }
            _ => Err(RewardInventoryPortError::unavailable(format!(
                "reward inventory transaction was not applied: {}",
                error.error_code
            ))),
        }
    }
}

impl RewardInventoryPort for PlayerManagerRewardInventoryPort {
    async fn execute_reward(
        &self,
        order: &RewardOrder,
    ) -> Result<AssetCommandResult, RewardInventoryPortError> {
        let item_table = self
            .config_tables
            .current_snapshot()
            .tables
            .item_table
            .clone();
        let materializer_table = item_table.clone();
        let result_summary = reward_grant_summary(order);
        let materializer_uid_generator = self.item_uid_generator.clone();
        let split_uid_generator = self.item_uid_generator.clone();
        let order_for_items = order.clone();

        match self
            .player_manager
            .grant_items_with_request_using_table(
                &order.character_id,
                &order.request_id,
                order.request_fingerprint().as_str(),
                order.origin.origin_type.as_str(),
                &order.reason,
                result_summary,
                item_table.as_ref(),
                move || {
                    materialize_reward_items(
                        &order_for_items,
                        materializer_table.as_ref(),
                        &materializer_uid_generator,
                    )
                },
                move || split_uid_generator.next().map_err(|_| ItemError::Unknown),
            )
            .await
        {
            Ok(outcome) => self.result_from_outcome(order, outcome),
            Err(error) => self.result_from_grant_error(order, error),
        }
    }

    async fn query_reward(
        &self,
        request_id: &str,
        request_fingerprint: &AssetRequestFingerprint,
    ) -> Result<Option<AssetCommandResult>, RewardInventoryPortError> {
        match self
            .player_manager
            .find_grant_record(request_id)
            .await
            .map_err(RewardInventoryPortError::unavailable)?
        {
            GrantRecordLookup::NotFound => Ok(None),
            GrantRecordLookup::ResultUnavailable => {
                AssetCommandResult::unknown(request_id, request_fingerprint.clone())
                    .map(Some)
                    .map_err(invalid_inventory_result)
            }
            GrantRecordLookup::Succeeded(record) => {
                if record.request_fingerprint != request_fingerprint.as_str() {
                    return AssetCommandResult::not_applied(
                        request_id,
                        request_fingerprint.clone(),
                        AssetCommandErrorCode::RequestFingerprintConflict,
                    )
                    .map(Some)
                    .map_err(invalid_inventory_result);
                }

                let actual_deltas = record
                    .result_summary
                    .items
                    .into_iter()
                    .map(|item| AssetQuantityDelta {
                        asset_type: AssetType::Item,
                        item_id: item.item_id,
                        binding: if item.binded {
                            AssetBinding::CharacterBound {
                                character_id: record.character_id.clone(),
                            }
                        } else {
                            AssetBinding::Unbound
                        },
                        delta: i64::from(item.count),
                    })
                    .collect();
                AssetCommandResult::applied(
                    request_id,
                    request_fingerprint.clone(),
                    actual_deltas,
                    Vec::new(),
                    Vec::new(),
                    None,
                )
                .map(Some)
                .map_err(invalid_inventory_result)
            }
        }
    }
}

fn reward_grant_summary(order: &RewardOrder) -> GrantResultSummary {
    GrantResultSummary {
        character_id: order.character_id.clone(),
        source: order.origin.origin_type.as_str().to_string(),
        items: order
            .items
            .iter()
            .map(|item| GrantItemIntent {
                item_id: item.item_id,
                count: item.count,
                binded: matches!(item.binding, AssetBinding::CharacterBound { .. }),
            })
            .collect(),
    }
}

fn materialize_reward_items(
    order: &RewardOrder,
    item_table: &ItemTable,
    item_uid_generator: &ItemUidGenerator,
) -> Result<Vec<Item>, GrantItemsError> {
    order
        .items
        .iter()
        .map(|intent| {
            let row = item_table
                .get(intent.item_id)
                .ok_or_else(|| GrantItemsError::item_failure(&ItemError::InvalidItemConfig))?;
            let binded = matches!(intent.binding, AssetBinding::CharacterBound { .. });
            let item = Item::from_config(
                item_uid_generator
                    .next()
                    .map_err(|_| GrantItemsError::item_failure(&ItemError::Unknown))?,
                intent.item_id,
                intent.count,
                binded,
                Some(&order.character_id),
                row,
                item_table,
            );
            // An ItemTable `Pickup` binding rule is authoritative as well. Reject an order that
            // claims a different final binding before capacity planning or snapshot mutation.
            if AssetBinding::from_item(&item) != intent.binding {
                return Err(GrantItemsError::item_failure(&ItemError::InvalidBinding));
            }
            Ok(item)
        })
        .collect()
}

fn reward_item_deltas(order: &RewardOrder) -> Vec<AssetQuantityDelta> {
    order
        .items
        .iter()
        .map(|item| AssetQuantityDelta {
            asset_type: AssetType::Item,
            item_id: item.item_id,
            binding: item.binding.clone(),
            delta: i64::from(item.count),
        })
        .collect()
}

fn invalid_inventory_result(code: AssetCommandErrorCode) -> RewardInventoryPortError {
    RewardInventoryPortError::unavailable(format!("invalid reward inventory result: {code:?}"))
}

/// Durable intent consumed by the future game-server -> mail-service dispatcher. The mail id is
/// deterministic, so a restart cannot produce a second reward mail for one reward request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewardMailOutboxEntry {
    pub delivery_request_id: String,
    pub mail_id: String,
    pub request_id: String,
    pub character_id: String,
    pub request_fingerprint: AssetRequestFingerprint,
    pub order: RewardOrder,
}

impl RewardMailOutboxEntry {
    pub fn for_order(order: &RewardOrder) -> Self {
        let digest = format!("{:x}", Sha256::digest(order.request_id.as_bytes()));
        // `mail_id` is limited to 64 bytes by both the current mail schema and its v1 contract.
        // 244 bits of the request-id digest remains far beyond the collision budget here.
        let mail_id = format!("rw_{}", &digest[..61]);
        let delivery_request_id = format!("reward_mail:{digest}");
        Self {
            delivery_request_id,
            mail_id,
            request_id: order.request_id.clone(),
            character_id: order.character_id.clone(),
            request_fingerprint: order.request_fingerprint(),
            order: order.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewardDeliveryRecord {
    pub request_id: String,
    pub character_id: String,
    pub request_fingerprint: AssetRequestFingerprint,
    pub result: RewardDeliveryResult,
}

impl RewardDeliveryRecord {
    fn for_order(order: &RewardOrder, result: RewardDeliveryResult) -> Self {
        Self {
            request_id: order.request_id.clone(),
            character_id: order.character_id.clone(),
            request_fingerprint: order.request_fingerprint(),
            result,
        }
    }

    fn matches(&self, order: &RewardOrder) -> bool {
        self.character_id == order.character_id
            && self.request_fingerprint == order.request_fingerprint()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewardDeliveryRecordWrite {
    Created(RewardDeliveryRecord),
    Existing(RewardDeliveryRecord),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewardMailOutboxWrite {
    Created(RewardMailOutboxEntry),
    Existing(RewardMailOutboxEntry),
}

/// The outbox and delivery record must survive process restarts. A real implementation backs
/// this port with PostgreSQL; the in-memory implementation exists only for offline tests.
pub trait RewardDeliveryStore: Send + Sync {
    async fn find_delivery(&self, request_id: &str)
    -> Result<Option<RewardDeliveryRecord>, String>;

    async fn persist_delivery(
        &self,
        record: RewardDeliveryRecord,
    ) -> Result<RewardDeliveryRecordWrite, String>;

    async fn persist_reward_mail(
        &self,
        entry: RewardMailOutboxEntry,
    ) -> Result<RewardMailOutboxWrite, String>;
}

/// Notification is deliberately outside every durable transaction. It may send inventory and
/// item-obtain pushes for direct delivery, or a new-mail hint for mail delivery; failure never
/// rolls back an already committed record.
pub trait RewardDeliveryNotifier: Send + Sync {
    async fn notify_committed(
        &self,
        order: &RewardOrder,
        result: &RewardDeliveryResult,
    ) -> Result<(), String>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRewardDeliveryNotifier;

impl RewardDeliveryNotifier for NoopRewardDeliveryNotifier {
    async fn notify_committed(
        &self,
        _order: &RewardOrder,
        _result: &RewardDeliveryResult,
    ) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewardDeliveryError {
    InvalidOrder(AssetCommandErrorCode),
    InventoryUnavailable(String),
    DeliveryRecordUnavailable(String),
    RewardMailOutboxUnavailable(String),
    InvalidInventoryResult(AssetCommandErrorCode),
}

impl std::fmt::Display for RewardDeliveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidOrder(code) => write!(formatter, "invalid reward order: {code:?}"),
            Self::InventoryUnavailable(error) => {
                write!(formatter, "inventory unavailable: {error}")
            }
            Self::DeliveryRecordUnavailable(error) => {
                write!(formatter, "reward delivery record unavailable: {error}")
            }
            Self::RewardMailOutboxUnavailable(error) => {
                write!(formatter, "reward mail outbox unavailable: {error}")
            }
            Self::InvalidInventoryResult(code) => {
                write!(formatter, "invalid inventory delivery result: {code:?}")
            }
        }
    }
}

impl std::error::Error for RewardDeliveryError {}

/// Orchestrates direct inventory settlement, durable reward-mail fallback, and post-commit
/// notification. It never treats a transport failure or `unknown` result as a reason to mail.
pub struct RewardDeliveryService<I, S, N> {
    inventory: I,
    store: S,
    notifier: N,
}

impl<I, S, N> RewardDeliveryService<I, S, N>
where
    I: RewardInventoryPort,
    S: RewardDeliveryStore,
    N: RewardDeliveryNotifier,
{
    pub fn new(inventory: I, store: S, notifier: N) -> Self {
        Self {
            inventory,
            store,
            notifier,
        }
    }

    pub async fn deliver(
        &self,
        order: RewardOrder,
    ) -> Result<RewardDeliveryResult, RewardDeliveryError> {
        order
            .validate()
            .map_err(RewardDeliveryError::InvalidOrder)?;

        if let Some(existing) = self
            .store
            .find_delivery(&order.request_id)
            .await
            .map_err(RewardDeliveryError::DeliveryRecordUnavailable)?
        {
            return self.replay_delivery(&order, existing).await;
        }

        match order.delivery_policy {
            RewardDeliveryPolicy::MailOnly => self.persist_reward_mail(&order, None).await,
            RewardDeliveryPolicy::PreferInventory | RewardDeliveryPolicy::InventoryRequired => {
                self.deliver_inventory_first(&order).await
            }
        }
    }

    async fn deliver_inventory_first(
        &self,
        order: &RewardOrder,
    ) -> Result<RewardDeliveryResult, RewardDeliveryError> {
        let attempted = self
            .inventory
            .execute_reward(order)
            .await
            .map_err(|error| RewardDeliveryError::InventoryUnavailable(error.message))?;
        let result = self.query_before_decision(order, attempted).await?;
        self.validate_inventory_result(order, &result)?;

        match result.result_state {
            AssetResultState::Applied => {
                let direct = self.with_direct_delivery(order, result)?;
                self.record_and_notify(order, direct).await
            }
            AssetResultState::NotApplied
                if result.permits_reward_mail_fallback(order.delivery_policy) =>
            {
                self.persist_reward_mail(order, Some(AssetFallbackReason::InventoryCapacityFull))
                    .await
            }
            // INVENTORY_REQUIRED deliberately returns a definite capacity failure without a
            // record or an outbox entry, so the source cannot commit its exchange side effects.
            AssetResultState::NotApplied | AssetResultState::Unknown => Ok(result),
        }
    }

    async fn query_before_decision(
        &self,
        order: &RewardOrder,
        attempted: AssetCommandResult,
    ) -> Result<AssetCommandResult, RewardDeliveryError> {
        if !attempted.result_state.requires_query_first() {
            return Ok(attempted);
        }

        // A query failure remains unknown too. In particular it cannot be reclassified as a
        // capacity failure and cannot create a second mail-backed delivery.
        match self
            .inventory
            .query_reward(&order.request_id, &order.request_fingerprint())
            .await
        {
            Ok(Some(result)) => Ok(result),
            Ok(None) | Err(_) => Ok(attempted),
        }
    }

    fn validate_inventory_result(
        &self,
        order: &RewardOrder,
        result: &AssetCommandResult,
    ) -> Result<(), RewardDeliveryError> {
        if result.request_id != order.request_id
            || result.request_fingerprint != order.request_fingerprint()
        {
            return Err(RewardDeliveryError::InvalidInventoryResult(
                AssetCommandErrorCode::InvalidResultContract,
            ));
        }
        result
            .validate()
            .map_err(RewardDeliveryError::InvalidInventoryResult)
    }

    fn with_direct_delivery(
        &self,
        order: &RewardOrder,
        mut result: AssetCommandResult,
    ) -> Result<RewardDeliveryResult, RewardDeliveryError> {
        match result.delivery.as_ref() {
            Some(receipt) if receipt.semantics.delivery_method != AssetDeliveryMethod::Direct => {
                return Err(RewardDeliveryError::InvalidInventoryResult(
                    AssetCommandErrorCode::InvalidResultContract,
                ));
            }
            Some(_) => {}
            None => {
                result.delivery = Some(
                    AssetDeliveryReceipt::direct(&order.request_id)
                        .map_err(RewardDeliveryError::InvalidInventoryResult)?,
                );
            }
        }
        result
            .validate()
            .map_err(RewardDeliveryError::InvalidInventoryResult)?;
        Ok(result)
    }

    async fn persist_reward_mail(
        &self,
        order: &RewardOrder,
        fallback_reason: Option<AssetFallbackReason>,
    ) -> Result<RewardDeliveryResult, RewardDeliveryError> {
        let entry = RewardMailOutboxEntry::for_order(order);
        let entry = match self
            .store
            .persist_reward_mail(entry)
            .await
            .map_err(RewardDeliveryError::RewardMailOutboxUnavailable)?
        {
            RewardMailOutboxWrite::Created(entry) | RewardMailOutboxWrite::Existing(entry) => {
                if entry.character_id != order.character_id
                    || entry.request_fingerprint != order.request_fingerprint()
                {
                    return self.request_conflict(order);
                }
                entry
            }
        };

        let result = AssetCommandResult::applied(
            &order.request_id,
            order.request_fingerprint(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Some(
                AssetDeliveryReceipt::mail(entry.mail_id, fallback_reason)
                    .map_err(RewardDeliveryError::InvalidInventoryResult)?,
            ),
        )
        .map_err(RewardDeliveryError::InvalidInventoryResult)?;
        self.record_and_notify(order, result).await
    }

    async fn record_and_notify(
        &self,
        order: &RewardOrder,
        result: RewardDeliveryResult,
    ) -> Result<RewardDeliveryResult, RewardDeliveryError> {
        let record = RewardDeliveryRecord::for_order(order, result);
        let result = match self
            .store
            .persist_delivery(record)
            .await
            .map_err(RewardDeliveryError::DeliveryRecordUnavailable)?
        {
            RewardDeliveryRecordWrite::Created(record)
            | RewardDeliveryRecordWrite::Existing(record) => {
                if !record.matches(order) {
                    return self.request_conflict(order);
                }
                record.result
            }
        };

        // A failed push is intentionally ignored. The persisted direct delivery / reward mail is
        // still the source of truth and a replay can issue another best-effort notification.
        let _ = self.notifier.notify_committed(order, &result).await;
        Ok(result)
    }

    async fn replay_delivery(
        &self,
        order: &RewardOrder,
        record: RewardDeliveryRecord,
    ) -> Result<RewardDeliveryResult, RewardDeliveryError> {
        if !record.matches(order) {
            return self.request_conflict(order);
        }
        let _ = self.notifier.notify_committed(order, &record.result).await;
        Ok(record.result)
    }

    fn request_conflict(
        &self,
        order: &RewardOrder,
    ) -> Result<RewardDeliveryResult, RewardDeliveryError> {
        AssetCommandResult::not_applied(
            &order.request_id,
            order.request_fingerprint(),
            AssetCommandErrorCode::RequestFingerprintConflict,
        )
        .map_err(RewardDeliveryError::InvalidInventoryResult)
    }
}

#[derive(Debug, Default, Clone)]
pub struct InMemoryRewardDeliveryStore {
    state: Arc<Mutex<InMemoryRewardDeliveryState>>,
}

#[derive(Debug, Default)]
struct InMemoryRewardDeliveryState {
    deliveries: HashMap<String, RewardDeliveryRecord>,
    mails: HashMap<String, RewardMailOutboxEntry>,
    fail_delivery_writes: usize,
    fail_mail_writes: usize,
}

impl InMemoryRewardDeliveryStore {
    pub async fn fail_next_delivery_writes(&self, count: usize) {
        self.state.lock().await.fail_delivery_writes = count;
    }

    pub async fn fail_next_mail_writes(&self, count: usize) {
        self.state.lock().await.fail_mail_writes = count;
    }

    #[cfg(test)]
    async fn mail_count(&self) -> usize {
        self.state.lock().await.mails.len()
    }

    #[cfg(test)]
    async fn delivery_count(&self) -> usize {
        self.state.lock().await.deliveries.len()
    }
}

impl RewardDeliveryStore for InMemoryRewardDeliveryStore {
    async fn find_delivery(
        &self,
        request_id: &str,
    ) -> Result<Option<RewardDeliveryRecord>, String> {
        Ok(self.state.lock().await.deliveries.get(request_id).cloned())
    }

    async fn persist_delivery(
        &self,
        record: RewardDeliveryRecord,
    ) -> Result<RewardDeliveryRecordWrite, String> {
        let mut state = self.state.lock().await;
        if state.fail_delivery_writes > 0 {
            state.fail_delivery_writes -= 1;
            return Err("configured delivery-record write failure".to_string());
        }
        if let Some(existing) = state.deliveries.get(&record.request_id) {
            return Ok(RewardDeliveryRecordWrite::Existing(existing.clone()));
        }
        state
            .deliveries
            .insert(record.request_id.clone(), record.clone());
        Ok(RewardDeliveryRecordWrite::Created(record))
    }

    async fn persist_reward_mail(
        &self,
        entry: RewardMailOutboxEntry,
    ) -> Result<RewardMailOutboxWrite, String> {
        let mut state = self.state.lock().await;
        if state.fail_mail_writes > 0 {
            state.fail_mail_writes -= 1;
            return Err("configured reward-mail outbox write failure".to_string());
        }
        if let Some(existing) = state.mails.get(&entry.request_id) {
            return Ok(RewardMailOutboxWrite::Existing(existing.clone()));
        }
        state.mails.insert(entry.request_id.clone(), entry.clone());
        Ok(RewardMailOutboxWrite::Created(entry))
    }
}

/// PostgreSQL-backed durable store. Dispatching the pending outbox into mail-service is
/// intentionally deferred to the mail workflow phase; persisting this row is the handoff that
/// allows a source to complete safely today.
#[derive(Debug, Clone)]
pub struct PgRewardDeliveryStore {
    pool: PgPool,
}

impl PgRewardDeliveryStore {
    pub async fn from_pool(pool: PgPool) -> Result<Self, String> {
        for statement in REWARD_DELIVERY_SCHEMA_STATEMENTS {
            sqlx::query(statement)
                .execute(&pool)
                .await
                .map_err(|error| error.to_string())?;
        }
        Ok(Self { pool })
    }
}

impl RewardDeliveryStore for PgRewardDeliveryStore {
    async fn find_delivery(
        &self,
        request_id: &str,
    ) -> Result<Option<RewardDeliveryRecord>, String> {
        let row = sqlx::query_as::<_, RewardDeliveryRecordRow>(
            r#"SELECT request_id, character_id, request_fingerprint, result_json
            FROM reward_delivery_records WHERE request_id = $1"#,
        )
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| error.to_string())?;
        row.map(RewardDeliveryRecordRow::into_record).transpose()
    }

    async fn persist_delivery(
        &self,
        record: RewardDeliveryRecord,
    ) -> Result<RewardDeliveryRecordWrite, String> {
        let result_json =
            serde_json::to_value(&record.result).map_err(|error| error.to_string())?;
        let inserted = sqlx::query_scalar::<_, String>(
            r#"INSERT INTO reward_delivery_records (
                request_id, character_id, request_fingerprint, result_json
            ) VALUES ($1, $2, $3, $4)
            ON CONFLICT (request_id) DO NOTHING
            RETURNING request_id"#,
        )
        .bind(&record.request_id)
        .bind(&record.character_id)
        .bind(record.request_fingerprint.as_str())
        .bind(result_json)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| error.to_string())?;
        if inserted.is_some() {
            return Ok(RewardDeliveryRecordWrite::Created(record));
        }
        let existing = self
            .find_delivery(&record.request_id)
            .await?
            .ok_or_else(|| "delivery record disappeared after conflict".to_string())?;
        Ok(RewardDeliveryRecordWrite::Existing(existing))
    }

    async fn persist_reward_mail(
        &self,
        entry: RewardMailOutboxEntry,
    ) -> Result<RewardMailOutboxWrite, String> {
        let items_json =
            serde_json::to_value(&entry.order.items).map_err(|error| error.to_string())?;
        let operator_json =
            serde_json::to_value(&entry.order.operator).map_err(|error| error.to_string())?;
        let inserted = sqlx::query_scalar::<_, String>(
            r#"INSERT INTO reward_mail_outbox (
                delivery_request_id, reward_request_id, mail_id, character_id,
                request_fingerprint, origin_type, origin_id, delivery_policy, items_json, reason, operator_json
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            ON CONFLICT (reward_request_id) DO NOTHING
            RETURNING reward_request_id"#,
        )
        .bind(&entry.delivery_request_id)
        .bind(&entry.request_id)
        .bind(&entry.mail_id)
        .bind(&entry.character_id)
        .bind(entry.request_fingerprint.as_str())
        .bind(entry.order.origin.origin_type.as_str())
        .bind(&entry.order.origin.origin_id)
        .bind(match entry.order.delivery_policy {
            RewardDeliveryPolicy::MailOnly => "MAIL_ONLY",
            RewardDeliveryPolicy::PreferInventory => "PREFER_INVENTORY",
            RewardDeliveryPolicy::InventoryRequired => "INVENTORY_REQUIRED",
        })
        .bind(items_json)
        .bind(&entry.order.reason)
        .bind(operator_json)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| error.to_string())?;
        if inserted.is_some() {
            return Ok(RewardMailOutboxWrite::Created(entry));
        }
        let existing = self
            .find_mail(&entry.request_id)
            .await?
            .ok_or_else(|| "reward mail outbox row disappeared after conflict".to_string())?;
        Ok(RewardMailOutboxWrite::Existing(existing))
    }
}

impl PgRewardDeliveryStore {
    async fn find_mail(&self, request_id: &str) -> Result<Option<RewardMailOutboxEntry>, String> {
        let row = sqlx::query_as::<_, RewardMailOutboxRow>(
            r#"SELECT
                delivery_request_id,
                mail_id,
                reward_request_id,
                character_id,
                request_fingerprint,
                origin_type,
                origin_id,
                delivery_policy,
                items_json,
                reason,
                operator_json
            FROM reward_mail_outbox
            WHERE reward_request_id = $1"#,
        )
        .bind(request_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| error.to_string())?;
        row.map(RewardMailOutboxRow::into_entry).transpose()
    }
}

#[derive(sqlx::FromRow)]
struct RewardDeliveryRecordRow {
    request_id: String,
    character_id: String,
    request_fingerprint: String,
    result_json: serde_json::Value,
}

impl RewardDeliveryRecordRow {
    fn into_record(self) -> Result<RewardDeliveryRecord, String> {
        let request_fingerprint = AssetRequestFingerprint::parse(self.request_fingerprint)
            .map_err(|error| format!("invalid stored reward fingerprint: {error:?}"))?;
        let result: RewardDeliveryResult =
            serde_json::from_value(self.result_json).map_err(|error| error.to_string())?;
        result
            .validate()
            .map_err(|error| format!("invalid stored reward result: {error:?}"))?;
        Ok(RewardDeliveryRecord {
            request_id: self.request_id,
            character_id: self.character_id,
            request_fingerprint,
            result,
        })
    }
}

#[derive(sqlx::FromRow)]
struct RewardMailOutboxRow {
    delivery_request_id: String,
    mail_id: String,
    reward_request_id: String,
    character_id: String,
    request_fingerprint: String,
    origin_type: String,
    origin_id: String,
    delivery_policy: String,
    items_json: serde_json::Value,
    reason: String,
    operator_json: serde_json::Value,
}

impl RewardMailOutboxRow {
    fn into_entry(self) -> Result<RewardMailOutboxEntry, String> {
        use super::{AssetOperator, AssetOrigin, AssetOriginType, NormalizedAssetItem};

        let origin_type = match self.origin_type.as_str() {
            "achievement" => AssetOriginType::Achievement,
            "quest" => AssetOriginType::Quest,
            "battle" => AssetOriginType::Battle,
            "scene_pickup" => AssetOriginType::ScenePickup,
            "activity" => AssetOriginType::Activity,
            "ranking" => AssetOriginType::Ranking,
            "world_event" => AssetOriginType::WorldEvent,
            "gm" => AssetOriginType::Gm,
            "mail_claim" => AssetOriginType::MailClaim,
            "player_operation" => AssetOriginType::PlayerOperation,
            "system" => AssetOriginType::System,
            _ => return Err("invalid stored reward origin type".to_string()),
        };
        let delivery_policy = match self.delivery_policy.as_str() {
            "MAIL_ONLY" => RewardDeliveryPolicy::MailOnly,
            "PREFER_INVENTORY" => RewardDeliveryPolicy::PreferInventory,
            "INVENTORY_REQUIRED" => RewardDeliveryPolicy::InventoryRequired,
            _ => return Err("invalid stored reward delivery policy".to_string()),
        };
        let request_fingerprint = AssetRequestFingerprint::parse(self.request_fingerprint)
            .map_err(|error| format!("invalid stored reward fingerprint: {error:?}"))?;
        let items: Vec<NormalizedAssetItem> =
            serde_json::from_value(self.items_json).map_err(|error| error.to_string())?;
        let operator: AssetOperator =
            serde_json::from_value(self.operator_json).map_err(|error| error.to_string())?;
        let order = RewardOrder::new(
            self.reward_request_id.clone(),
            self.character_id.clone(),
            AssetOrigin::new(origin_type, self.origin_id).map_err(|error| error.to_string())?,
            delivery_policy,
            &items,
            self.reason,
            operator,
        )
        .map_err(|error| format!("invalid stored reward mail order: {error:?}"))?;
        if order.request_fingerprint() != request_fingerprint {
            return Err("stored reward mail fingerprint does not match order".to_string());
        }
        Ok(RewardMailOutboxEntry {
            delivery_request_id: self.delivery_request_id,
            mail_id: self.mail_id,
            request_id: self.reward_request_id,
            character_id: self.character_id,
            request_fingerprint,
            order,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::core::inventory::{
        AssetBinding, AssetOperator, AssetOperatorType, AssetOrigin, AssetOriginType,
        AssetPermission, NormalizedAssetItem, PlayerData,
    };

    #[derive(Clone, Default)]
    struct FakeInventory {
        execute_results: Arc<Mutex<VecDeque<AssetCommandResult>>>,
        query_results: Arc<Mutex<VecDeque<Option<AssetCommandResult>>>>,
        execute_count: Arc<AtomicUsize>,
        query_count: Arc<AtomicUsize>,
    }

    impl FakeInventory {
        fn with_execute(result: AssetCommandResult) -> Self {
            Self {
                execute_results: Arc::new(Mutex::new(VecDeque::from([result]))),
                ..Self::default()
            }
        }

        async fn push_execute(&self, result: AssetCommandResult) {
            self.execute_results.lock().await.push_back(result);
        }

        async fn push_query(&self, result: Option<AssetCommandResult>) {
            self.query_results.lock().await.push_back(result);
        }
    }

    impl RewardInventoryPort for FakeInventory {
        async fn execute_reward(
            &self,
            _order: &RewardOrder,
        ) -> Result<AssetCommandResult, RewardInventoryPortError> {
            self.execute_count.fetch_add(1, Ordering::Relaxed);
            self.execute_results
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| RewardInventoryPortError::unavailable("missing execute result"))
        }

        async fn query_reward(
            &self,
            _request_id: &str,
            _request_fingerprint: &AssetRequestFingerprint,
        ) -> Result<Option<AssetCommandResult>, RewardInventoryPortError> {
            self.query_count.fetch_add(1, Ordering::Relaxed);
            Ok(self.query_results.lock().await.pop_front().flatten())
        }
    }

    #[derive(Clone, Default)]
    struct RecordingNotifier {
        calls: Arc<Mutex<Vec<RewardDeliveryResult>>>,
    }

    impl RewardDeliveryNotifier for RecordingNotifier {
        async fn notify_committed(
            &self,
            _order: &RewardOrder,
            result: &RewardDeliveryResult,
        ) -> Result<(), String> {
            self.calls.lock().await.push(result.clone());
            Ok(())
        }
    }

    fn order(policy: RewardDeliveryPolicy) -> RewardOrder {
        RewardOrder::new(
            "reward:achievement:42",
            "chr_42",
            AssetOrigin::new(AssetOriginType::Achievement, "achievement:42").unwrap(),
            policy,
            &[NormalizedAssetItem::new(1001, 2, AssetBinding::Unbound).unwrap()],
            "achievement reward",
            AssetOperator::new(
                AssetOperatorType::Service,
                "achievement-service",
                [AssetPermission::Grant],
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn applied(order: &RewardOrder) -> AssetCommandResult {
        AssetCommandResult::applied(
            &order.request_id,
            order.request_fingerprint(),
            Vec::new(),
            Vec::new(),
            vec!["ledger_1".to_string()],
            None,
        )
        .unwrap()
    }

    fn capacity_full(order: &RewardOrder) -> AssetCommandResult {
        AssetCommandResult::not_applied(
            &order.request_id,
            order.request_fingerprint(),
            AssetCommandErrorCode::InventoryCapacityFull,
        )
        .unwrap()
    }

    fn unknown(order: &RewardOrder) -> AssetCommandResult {
        AssetCommandResult::unknown(&order.request_id, order.request_fingerprint()).unwrap()
    }

    fn production_inventory_port(manager: PlayerManager) -> PlayerManagerRewardInventoryPort {
        let csv_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("csv");
        let config_tables = ConfigTableRuntime::load(&csv_dir)
            .expect("game-server CSV fixtures should build a reward inventory port");
        PlayerManagerRewardInventoryPort::new(
            manager,
            config_tables,
            ItemUidGenerator::new_for_test(10_000),
        )
    }

    #[tokio::test]
    async fn direct_success_records_then_notifies_the_committed_inventory_result() {
        let reward = order(RewardDeliveryPolicy::PreferInventory);
        let inventory = FakeInventory::with_execute(applied(&reward));
        let store = InMemoryRewardDeliveryStore::default();
        let notifier = RecordingNotifier::default();
        let service =
            RewardDeliveryService::new(inventory.clone(), store.clone(), notifier.clone());

        let result = service.deliver(reward.clone()).await.unwrap();

        assert_eq!(result.result_state, AssetResultState::Applied);
        assert_eq!(
            result.delivery.unwrap().semantics.delivery_method,
            AssetDeliveryMethod::Direct
        );
        assert_eq!(store.delivery_count().await, 1);
        assert_eq!(store.mail_count().await, 0);
        assert_eq!(inventory.execute_count.load(Ordering::Relaxed), 1);
        assert_eq!(notifier.calls.lock().await.len(), 1);
    }

    #[tokio::test]
    async fn capacity_fallback_persists_one_deterministic_reward_mail() {
        let reward = order(RewardDeliveryPolicy::PreferInventory);
        let inventory = FakeInventory::with_execute(capacity_full(&reward));
        let store = InMemoryRewardDeliveryStore::default();
        let notifier = RecordingNotifier::default();
        let service = RewardDeliveryService::new(inventory, store.clone(), notifier);

        let result = service.deliver(reward.clone()).await.unwrap();
        let receipt = result.delivery.unwrap();

        assert_eq!(receipt.semantics.delivery_method, AssetDeliveryMethod::Mail);
        assert_eq!(
            receipt.fallback_reason,
            Some(AssetFallbackReason::InventoryCapacityFull)
        );
        assert_eq!(receipt.delivery_id.len(), 64);
        assert_eq!(store.mail_count().await, 1);
        assert_eq!(store.delivery_count().await, 1);
    }

    #[tokio::test]
    async fn mail_outbox_failure_leaves_the_source_retryable_until_the_mail_is_persisted() {
        let reward = order(RewardDeliveryPolicy::PreferInventory);
        let inventory = FakeInventory::with_execute(capacity_full(&reward));
        inventory.push_execute(capacity_full(&reward)).await;
        let store = InMemoryRewardDeliveryStore::default();
        store.fail_next_mail_writes(1).await;
        let service = RewardDeliveryService::new(
            inventory.clone(),
            store.clone(),
            NoopRewardDeliveryNotifier,
        );

        let first = service.deliver(reward.clone()).await.unwrap_err();
        assert!(matches!(
            first,
            RewardDeliveryError::RewardMailOutboxUnavailable(_)
        ));
        assert_eq!(store.mail_count().await, 0);
        assert_eq!(store.delivery_count().await, 0);

        let second = service.deliver(reward).await.unwrap();
        assert_eq!(second.result_state, AssetResultState::Applied);
        assert_eq!(store.mail_count().await, 1);
        assert_eq!(inventory.execute_count.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn unknown_is_queried_before_any_fallback_and_remains_uncommitted_when_unresolved() {
        let reward = order(RewardDeliveryPolicy::PreferInventory);
        let inventory = FakeInventory::with_execute(unknown(&reward));
        inventory.push_query(Some(unknown(&reward))).await;
        let store = InMemoryRewardDeliveryStore::default();
        let service = RewardDeliveryService::new(
            inventory.clone(),
            store.clone(),
            NoopRewardDeliveryNotifier,
        );

        let result = service.deliver(reward).await.unwrap();

        assert_eq!(result.result_state, AssetResultState::Unknown);
        assert_eq!(inventory.query_count.load(Ordering::Relaxed), 1);
        assert_eq!(store.mail_count().await, 0);
        assert_eq!(store.delivery_count().await, 0);
    }

    #[tokio::test]
    async fn duplicate_mail_only_order_replays_the_persisted_delivery_without_creating_another_mail()
     {
        let reward = order(RewardDeliveryPolicy::MailOnly);
        let inventory = FakeInventory::default();
        let store = InMemoryRewardDeliveryStore::default();
        let notifier = RecordingNotifier::default();
        let service =
            RewardDeliveryService::new(inventory.clone(), store.clone(), notifier.clone());

        let first = service.deliver(reward.clone()).await.unwrap();
        let second = service.deliver(reward).await.unwrap();

        assert_eq!(first, second);
        assert_eq!(store.mail_count().await, 1);
        assert_eq!(store.delivery_count().await, 1);
        assert_eq!(inventory.execute_count.load(Ordering::Relaxed), 0);
        assert_eq!(notifier.calls.lock().await.len(), 2);
    }

    #[tokio::test]
    async fn interrupted_after_inventory_commit_recovers_from_the_idempotent_inventory_request() {
        let reward = order(RewardDeliveryPolicy::PreferInventory);
        let inventory = FakeInventory::with_execute(applied(&reward));
        inventory.push_execute(applied(&reward)).await;
        let store = InMemoryRewardDeliveryStore::default();
        store.fail_next_delivery_writes(1).await;

        let first_process = RewardDeliveryService::new(
            inventory.clone(),
            store.clone(),
            NoopRewardDeliveryNotifier,
        );
        let first = first_process.deliver(reward.clone()).await.unwrap_err();
        assert!(matches!(
            first,
            RewardDeliveryError::DeliveryRecordUnavailable(_)
        ));
        assert_eq!(store.delivery_count().await, 0);

        let second_process = RewardDeliveryService::new(
            inventory.clone(),
            store.clone(),
            NoopRewardDeliveryNotifier,
        );
        let recovered = second_process.deliver(reward).await.unwrap();
        assert_eq!(recovered.result_state, AssetResultState::Applied);
        assert_eq!(store.delivery_count().await, 1);
        assert_eq!(store.mail_count().await, 0);
        assert_eq!(inventory.execute_count.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn inventory_required_capacity_failure_does_not_create_a_mail_or_completion_record() {
        let reward = order(RewardDeliveryPolicy::InventoryRequired);
        let inventory = FakeInventory::with_execute(capacity_full(&reward));
        let store = InMemoryRewardDeliveryStore::default();
        let service =
            RewardDeliveryService::new(inventory, store.clone(), NoopRewardDeliveryNotifier);

        let result = service.deliver(reward).await.unwrap();

        assert_eq!(result.result_state, AssetResultState::NotApplied);
        assert_eq!(
            result.error_code,
            Some(AssetCommandErrorCode::InventoryCapacityFull)
        );
        assert_eq!(store.mail_count().await, 0);
        assert_eq!(store.delivery_count().await, 0);
    }

    #[tokio::test]
    async fn player_manager_port_uses_the_transactional_grant_and_replays_its_record() {
        let reward = order(RewardDeliveryPolicy::PreferInventory);
        let manager = PlayerManager::default();
        let port = production_inventory_port(manager.clone());

        let first = port.execute_reward(&reward).await.unwrap();
        let replay = port.execute_reward(&reward).await.unwrap();
        let queried = port
            .query_reward(&reward.request_id, &reward.request_fingerprint())
            .await
            .unwrap()
            .expect("committed reward must be queryable by request id");

        assert_eq!(first.result_state, AssetResultState::Applied);
        assert_eq!(
            first.actual_deltas,
            vec![AssetQuantityDelta {
                asset_type: AssetType::Item,
                item_id: 1001,
                binding: AssetBinding::Unbound,
                delta: 2,
            }]
        );
        assert_eq!(first.container_versions.len(), 1);
        assert!(replay.container_versions.is_empty());
        assert_eq!(queried.actual_deltas, first.actual_deltas);
        assert_eq!(
            manager
                .get_player(&reward.character_id)
                .await
                .unwrap()
                .get_inventory_items()
                .into_iter()
                .filter(|item| item.item_id == 1001)
                .map(|item| item.count)
                .sum::<u32>(),
            2
        );
    }

    #[tokio::test]
    async fn player_manager_port_recovers_after_inventory_commit_before_delivery_record() {
        let reward = order(RewardDeliveryPolicy::PreferInventory);
        let manager = PlayerManager::default();
        let port = production_inventory_port(manager.clone());
        let store = InMemoryRewardDeliveryStore::default();
        store.fail_next_delivery_writes(1).await;

        let first_process =
            RewardDeliveryService::new(port.clone(), store.clone(), NoopRewardDeliveryNotifier);
        assert!(matches!(
            first_process.deliver(reward.clone()).await,
            Err(RewardDeliveryError::DeliveryRecordUnavailable(_))
        ));

        let second_process =
            RewardDeliveryService::new(port, store.clone(), NoopRewardDeliveryNotifier);
        let recovered = second_process.deliver(reward.clone()).await.unwrap();

        assert_eq!(recovered.result_state, AssetResultState::Applied);
        assert_eq!(store.delivery_count().await, 1);
        assert_eq!(store.mail_count().await, 0);
        assert_eq!(
            manager
                .get_player(&reward.character_id)
                .await
                .unwrap()
                .get_inventory_items()
                .into_iter()
                .filter(|item| item.item_id == 1001)
                .map(|item| item.count)
                .sum::<u32>(),
            2
        );
    }

    #[tokio::test]
    async fn player_manager_port_returns_definite_capacity_failure_without_mutating_inventory() {
        let manager = PlayerManager::default();
        let mut full = PlayerData::with_capacity("chr_42".to_string(), 1, 1);
        full.inventory
            .add_item(Item::new(99, 1002, 1, false))
            .unwrap();
        manager.save_player("chr_42", full).await.unwrap();
        let port = production_inventory_port(manager.clone());
        let reward = order(RewardDeliveryPolicy::PreferInventory);

        let result = port.execute_reward(&reward).await.unwrap();

        assert_eq!(result.result_state, AssetResultState::NotApplied);
        assert_eq!(
            result.error_code,
            Some(AssetCommandErrorCode::InventoryCapacityFull)
        );
        let player = manager.get_player("chr_42").await.unwrap();
        assert_eq!(player.inventory.item_count(), 1);
        assert_eq!(player.inventory.find_item(99).unwrap().count, 1);
        assert!(player.inventory.find_item(10_000).is_none());
    }
}
