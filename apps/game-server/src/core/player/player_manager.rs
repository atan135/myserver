use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use super::db_player_store::{
    AssetLedgerContext, GrantRecord, GrantRecordLookup, PgPlayerStore, SaveGrantRecordError,
    SaveGrantRecordOutcome, SavePlayerError as StoreSavePlayerError,
};
use super::grant_contract::GrantResultSummary;
use crate::core::inventory::{EquipSlot, Item, ItemError, PlayerData};
use crate::csv_code::itemtable::ItemTable;
use crate::metrics::METRICS;

static PLAYER_ASSET_OPERATION_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct GrantItemsOutcome {
    pub applied: bool,
    pub player_data: Option<PlayerData>,
    pub granted_items: Vec<Item>,
    pub record: GrantRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantItemsError {
    pub error_code: &'static str,
    pub error_category: &'static str,
    pub result_state: &'static str,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerSaveError {
    TransactionFailed,
    VersionConflict,
    ResultUnknown,
}

/// Errors returned by the only runtime path allowed to mutate a player's item snapshot.
///
/// Protocol handlers intentionally receive this rather than a mutable `PlayerData`: the
/// character lock, revision check, durable save, and publish-after-commit ordering must remain
/// one operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerAssetMutationError {
    Item(ItemError),
    Persistence(PlayerSaveError),
    CrossStoreAtomicityUnavailable,
    MigrationDisabled,
}

impl PlayerAssetMutationError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::Item(error) => error.as_str(),
            Self::Persistence(error) => error.error_code(),
            Self::CrossStoreAtomicityUnavailable => "ASSET_CROSS_STORE_ATOMICITY_UNAVAILABLE",
            Self::MigrationDisabled => "ASSET_TRANSACTION_MIGRATION_DISABLED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarehouseAssetAction {
    Deposit,
    Withdraw,
}

#[derive(Debug, Clone)]
pub struct PlayerItemUseOutcome {
    pub player_data: PlayerData,
    pub hp_change: i64,
}

impl PlayerSaveError {
    pub const fn error_code(&self) -> &'static str {
        match self {
            Self::TransactionFailed => "INVENTORY_TRANSACTION_FAILED",
            Self::VersionConflict => "INVENTORY_VERSION_CONFLICT",
            Self::ResultUnknown => "INVENTORY_COMMIT_RESULT_UNKNOWN",
        }
    }
}

impl GrantItemsError {
    pub fn fingerprint_conflict() -> Self {
        Self {
            error_code: "REQUEST_FINGERPRINT_CONFLICT",
            error_category: "PERMANENT_FAILURE",
            result_state: "not_applied",
            retryable: false,
        }
    }

    pub fn result_unavailable() -> Self {
        Self {
            error_code: "GRANT_RESULT_UNAVAILABLE",
            error_category: "RESULT_UNKNOWN",
            result_state: "unknown",
            retryable: false,
        }
    }

    pub fn transaction_failed() -> Self {
        Self {
            error_code: "INVENTORY_TRANSACTION_FAILED",
            error_category: "RETRYABLE_FAILURE",
            result_state: "not_applied",
            retryable: true,
        }
    }

    pub fn result_query_failed() -> Self {
        Self {
            error_code: "GRANT_RESULT_QUERY_FAILED",
            error_category: "RESULT_UNKNOWN",
            result_state: "unknown",
            retryable: true,
        }
    }

    pub fn commit_result_unknown() -> Self {
        Self {
            error_code: "INVENTORY_COMMIT_RESULT_UNKNOWN",
            error_category: "RESULT_UNKNOWN",
            result_state: "unknown",
            retryable: true,
        }
    }

    pub fn item_failure(error: &ItemError) -> Self {
        Self {
            error_code: error.as_str(),
            error_category: "PERMANENT_FAILURE",
            result_state: "not_applied",
            retryable: false,
        }
    }

    fn from_player_save(error: PlayerSaveError) -> Self {
        match error {
            PlayerSaveError::TransactionFailed | PlayerSaveError::VersionConflict => {
                Self::transaction_failed()
            }
            PlayerSaveError::ResultUnknown => Self::commit_result_unknown(),
        }
    }
}

/// 玩家数据管理器
/// 负责管理所有在线角色的背包/属性数据
#[derive(Clone)]
pub struct PlayerManager {
    players: Arc<RwLock<HashMap<String, PlayerData>>>,
    store: PgPlayerStore,
    grant_records: Arc<RwLock<HashMap<String, GrantRecord>>>,
    grant_request_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    asset_character_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
}

impl PlayerManager {
    /// 创建新的 PlayerManager
    pub fn new(store: PgPlayerStore) -> Self {
        Self {
            players: Arc::new(RwLock::new(HashMap::new())),
            store,
            grant_records: Arc::new(RwLock::new(HashMap::new())),
            grant_request_locks: Arc::new(Mutex::new(HashMap::new())),
            asset_character_locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 获取角色玩法数据（如果不存在返回 None）
    pub async fn get_player(&self, character_id: &str) -> Option<PlayerData> {
        let players = self.players.read().await;
        players.get(character_id).cloned()
    }

    /// 获取或创建角色玩法数据
    pub async fn get_or_create_player(&self, character_id: &str) -> PlayerData {
        // 先尝试读取在线角色
        {
            let players = self.players.read().await;
            if let Some(data) = players.get(character_id) {
                return data.clone();
            }
        }

        // 同角色的首次加载也进入资产并发边界；不同角色不会相互等待数据库 IO。
        let character_lock = keyed_lock(&self.asset_character_locks, character_id).await;
        let _character_guard = character_lock.lock().await;

        if let Some(data) = self.players.read().await.get(character_id).cloned() {
            return data;
        }

        // 尝试从数据库加载
        match self.store.load(character_id).await {
            Ok(Some(data)) => {
                let player_data = data;
                let mut players = self.players.write().await;
                players.insert(character_id.to_string(), player_data.clone());
                return player_data;
            }
            Ok(None) => {
                // 数据库中没有，创建新角色数据
                info!(character_id = %character_id, "creating new character gameplay data");
            }
            Err(e) => {
                warn!(character_id = %character_id, error = %e, "failed to load character gameplay data from DB, creating new");
            }
        }

        // 不存在则创建新角色玩法数据
        let new_player = PlayerData::new(character_id.to_string());
        let mut players = self.players.write().await;
        players.insert(character_id.to_string(), new_player.clone());
        new_player
    }

    /// 保存角色玩法数据。只有持久化事务确认提交后才会发布新的在线快照。
    pub async fn save_player(
        &self,
        character_id: &str,
        mut data: PlayerData,
    ) -> Result<PlayerData, PlayerSaveError> {
        let character_lock = keyed_lock(&self.asset_character_locks, character_id).await;
        let _character_guard = character_lock.lock().await;

        if let Some(current) = self.players.read().await.get(character_id)
            && current.persistence_revision() != data.persistence_revision()
        {
            METRICS.record_asset_version_conflict();
            return Err(PlayerSaveError::VersionConflict);
        }

        if self.store.enabled() {
            let outcome = self.store.save(character_id, &data).await.map_err(|error| {
                warn!(character_id = %character_id, error = %error, "failed to persist character gameplay data");
                match error {
                    StoreSavePlayerError::NotApplied(_) => PlayerSaveError::TransactionFailed,
                    StoreSavePlayerError::VersionConflict => PlayerSaveError::VersionConflict,
                    StoreSavePlayerError::ResultUnknown(_) => PlayerSaveError::ResultUnknown,
                }
            })?;
            data.set_persistence_revision(outcome.revision);
        } else {
            // Explicit database-disabled development mode still serializes and versions online
            // snapshots so stale clones cannot overwrite a same-process asset mutation.
            data.set_persistence_revision(data.persistence_revision().saturating_add(1));
        }

        self.players
            .write()
            .await
            .insert(character_id.to_string(), data.clone());
        Ok(data)
    }

    /// Run one player-originated inventory operation under the same per-character transaction
    /// boundary used by grants. The closure is private to this module's named operations below;
    /// protocol services never receive a mutable snapshot or call `save_player` directly.
    async fn commit_asset_mutation<T, F>(
        &self,
        character_id: &str,
        operation: &'static str,
        mutate: F,
    ) -> Result<(PlayerData, T), PlayerAssetMutationError>
    where
        F: FnOnce(&mut PlayerData) -> Result<T, PlayerAssetMutationError>,
    {
        if !asset_player_operations_enabled() {
            return Err(PlayerAssetMutationError::MigrationDisabled);
        }
        let character_lock = keyed_lock(&self.asset_character_locks, character_id).await;
        let _character_guard = character_lock.lock().await;

        let mut player_data = self
            .load_or_create_player_for_asset_mutation(character_id)
            .await?;
        if let Some(current) = self.players.read().await.get(character_id)
            && current.persistence_revision() != player_data.persistence_revision()
        {
            METRICS.record_asset_version_conflict();
            return Err(PlayerAssetMutationError::Persistence(
                PlayerSaveError::VersionConflict,
            ));
        }

        let before = player_data.clone();
        let result = mutate(&mut player_data)?;
        if self.store.enabled() {
            let request_id = format!(
                "player-op:{}:{}",
                current_unix_ms(),
                PLAYER_ASSET_OPERATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            );
            let outcome = self
                .store
                .save_with_asset_ledger(
                    character_id,
                    &before,
                    &player_data,
                    &request_id,
                    "player-operation",
                    operation,
                    AssetLedgerContext::player_operation(operation, &request_id),
                )
                .await
                .map_err(|error| {
                    warn!(
                        character_id = %character_id,
                        error = %error,
                        "failed to persist player asset transaction"
                    );
                    PlayerAssetMutationError::Persistence(match error {
                        StoreSavePlayerError::NotApplied(_) => PlayerSaveError::TransactionFailed,
                        StoreSavePlayerError::VersionConflict => {
                            METRICS.record_asset_version_conflict();
                            PlayerSaveError::VersionConflict
                        }
                        StoreSavePlayerError::ResultUnknown(_) => PlayerSaveError::ResultUnknown,
                    })
                })?;
            player_data.set_persistence_revision(outcome.revision);
        } else {
            // DB-disabled local mode retains the same stale-snapshot protection as production.
            player_data
                .set_persistence_revision(player_data.persistence_revision().saturating_add(1));
        }

        self.players
            .write()
            .await
            .insert(character_id.to_string(), player_data.clone());
        Ok((player_data, result))
    }

    /// Equip an inventory item through the shared asset transaction boundary.
    pub async fn equip_item_in_asset_transaction(
        &self,
        character_id: &str,
        item_uid: u64,
        item_table: &ItemTable,
    ) -> Result<PlayerData, PlayerAssetMutationError> {
        self.commit_asset_mutation(character_id, "equip", |player_data| {
            player_data
                .equip_item(item_uid, item_table)
                .map_err(PlayerAssetMutationError::Item)
        })
        .await
        .map(|(player_data, ())| player_data)
    }

    /// Unequip an item through the shared asset transaction boundary. There is no public player
    /// packet for this legacy operation yet, but internal callers must use this method.
    pub async fn unequip_item_in_asset_transaction(
        &self,
        character_id: &str,
        slot: EquipSlot,
        item_table: &ItemTable,
    ) -> Result<(PlayerData, Option<Item>), PlayerAssetMutationError> {
        self.commit_asset_mutation(character_id, "unequip", |player_data| {
            player_data
                .unequip_item(slot, item_table)
                .map_err(PlayerAssetMutationError::Item)
        })
        .await
    }

    /// Consume a player item only after its effect has been proven to fit the same durable
    /// snapshot. Effects owned by another store remain rejected rather than risking a partial
    /// commit.
    pub async fn use_item_in_asset_transaction(
        &self,
        character_id: &str,
        item_uid: u64,
        item_table: &ItemTable,
    ) -> Result<PlayerItemUseOutcome, PlayerAssetMutationError> {
        let (player_data, hp_change) = self
            .commit_asset_mutation(character_id, "use", |player_data| {
                let hp_before = player_data.get_hp();
                let prepared = player_data
                    .prepare_item_use(item_uid, item_table)
                    .map_err(PlayerAssetMutationError::Item)?;
                if matches!(
                    prepared.effect,
                    crate::core::inventory::player_data::PreparedItemUseEffect::CharacterElementChange { .. }
                ) {
                    return Err(PlayerAssetMutationError::CrossStoreAtomicityUnavailable);
                }
                player_data
                    .finalize_prepared_item_use(&prepared, item_table)
                    .map_err(PlayerAssetMutationError::Item)?;
                Ok(player_data.get_hp() - hp_before)
            })
            .await?;

        Ok(PlayerItemUseOutcome {
            player_data,
            hp_change,
        })
    }

    /// Discard an inventory item through the shared asset transaction boundary.
    pub async fn discard_item_in_asset_transaction(
        &self,
        character_id: &str,
        item_uid: u64,
        count: u32,
    ) -> Result<PlayerData, PlayerAssetMutationError> {
        self.commit_asset_mutation(character_id, "discard", |player_data| {
            player_data
                .remove_item(item_uid, count)
                .map(|_| ())
                .map_err(PlayerAssetMutationError::Item)
        })
        .await
        .map(|(player_data, ())| player_data)
    }

    /// Move an item between inventory and warehouse without exposing either container to the
    /// protocol layer.
    pub async fn move_warehouse_item_in_asset_transaction<F>(
        &self,
        character_id: &str,
        action: WarehouseAssetAction,
        item_uid: u64,
        count: u32,
        item_table: &ItemTable,
        mut next_uid: F,
    ) -> Result<PlayerData, PlayerAssetMutationError>
    where
        F: FnMut() -> Result<u64, ItemError>,
    {
        let operation = match action {
            WarehouseAssetAction::Deposit => "warehouse_deposit",
            WarehouseAssetAction::Withdraw => "warehouse_withdraw",
        };
        self.commit_asset_mutation(character_id, operation, |player_data| {
            let mutation = match action {
                WarehouseAssetAction::Deposit => {
                    player_data.warehouse_deposit(item_uid, count, item_table, &mut next_uid)
                }
                WarehouseAssetAction::Withdraw => {
                    player_data.warehouse_withdraw(item_uid, count, item_table, &mut next_uid)
                }
            };
            mutation.map_err(PlayerAssetMutationError::Item)
        })
        .await
        .map(|(player_data, ())| player_data)
    }

    pub async fn grant_items(
        &self,
        character_id: &str,
        items: &[Item],
    ) -> Result<PlayerData, GrantItemsError> {
        let mut player_data = self.get_or_create_player(character_id).await;

        for item in items {
            player_data
                .add_item(item.clone())
                .map_err(|error| GrantItemsError::item_failure(&error))?;
        }

        self.save_player(character_id, player_data)
            .await
            .map_err(GrantItemsError::from_player_save)
    }

    pub async fn find_grant_record(&self, request_id: &str) -> Result<GrantRecordLookup, String> {
        if self.store.enabled() {
            self.store.find_grant_record(request_id).await
        } else {
            Ok(self
                .grant_records
                .read()
                .await
                .get(request_id)
                .cloned()
                .map_or(GrantRecordLookup::NotFound, GrantRecordLookup::Succeeded))
        }
    }

    pub async fn grant_items_with_request<F>(
        &self,
        character_id: &str,
        request_id: &str,
        request_fingerprint: &str,
        source: &str,
        reason: &str,
        result_summary: GrantResultSummary,
        build_items: F,
    ) -> Result<GrantItemsOutcome, GrantItemsError>
    where
        F: FnOnce() -> Result<Vec<Item>, GrantItemsError>,
    {
        let request_lock = keyed_lock(&self.grant_request_locks, request_id).await;
        let _request_guard = request_lock.lock().await;
        let character_lock = keyed_lock(&self.asset_character_locks, character_id).await;
        let _character_guard = character_lock.lock().await;

        match self
            .find_grant_record(request_id)
            .await
            .map_err(|_| GrantItemsError::result_query_failed())?
        {
            GrantRecordLookup::NotFound => {}
            GrantRecordLookup::Succeeded(record) => {
                return replay_or_conflict(record, character_id, request_fingerprint);
            }
            GrantRecordLookup::ResultUnavailable => {
                return Err(GrantItemsError::result_unavailable());
            }
        }

        let mut player_data = self.load_or_create_player_for_grant(character_id).await?;
        if let Some(current) = self.players.read().await.get(character_id)
            && current.persistence_revision() != player_data.persistence_revision()
        {
            METRICS.record_asset_version_conflict();
            return Err(GrantItemsError::transaction_failed());
        }
        let items = build_items()?;

        for item in &items {
            player_data
                .add_item(item.clone())
                .map_err(|error| GrantItemsError::item_failure(&error))?;
        }

        let save_outcome = if self.store.enabled() {
            self.store
                .save_with_grant_record(
                    character_id,
                    &player_data,
                    request_id,
                    request_fingerprint,
                    source,
                    reason,
                    &items,
                    &result_summary,
                )
                .await
                .map_err(|error| {
                    warn!(
                        request_id,
                        character_id,
                        error = %error,
                        "failed to persist inventory grant transaction"
                    );
                    match error {
                        SaveGrantRecordError::NotApplied(_) => {
                            GrantItemsError::transaction_failed()
                        }
                        SaveGrantRecordError::ResultUnknown(_) => {
                            GrantItemsError::commit_result_unknown()
                        }
                    }
                })?
        } else {
            let record = GrantRecord {
                request_id: request_id.to_string(),
                character_id: character_id.to_string(),
                request_fingerprint: request_fingerprint.to_string(),
                result_summary,
                created_at_ms: current_unix_ms(),
            };
            self.grant_records
                .write()
                .await
                .insert(request_id.to_string(), record.clone());
            SaveGrantRecordOutcome::Applied(record)
        };

        match save_outcome {
            SaveGrantRecordOutcome::Applied(record) => {
                player_data
                    .set_persistence_revision(player_data.persistence_revision().saturating_add(1));
                let mut players = self.players.write().await;
                players.insert(character_id.to_string(), player_data.clone());
                Ok(GrantItemsOutcome {
                    applied: true,
                    player_data: Some(player_data),
                    granted_items: items,
                    record,
                })
            }
            SaveGrantRecordOutcome::Existing(GrantRecordLookup::Succeeded(record)) => {
                replay_or_conflict(record, character_id, request_fingerprint)
            }
            SaveGrantRecordOutcome::Existing(GrantRecordLookup::ResultUnavailable) => {
                Err(GrantItemsError::result_unavailable())
            }
            SaveGrantRecordOutcome::Existing(GrantRecordLookup::NotFound) => {
                Err(GrantItemsError::transaction_failed())
            }
        }
    }

    /// Reward-delivery entry point. Unlike the legacy grant path retained for rolling migration,
    /// this performs one MaxStack capacity preflight for the complete reward before mutating the
    /// candidate snapshot. The same request, revision, ledger, and publish-after-commit rules
    /// are shared with `grant_items_with_request`.
    pub async fn grant_items_with_request_using_table<F, G>(
        &self,
        character_id: &str,
        request_id: &str,
        request_fingerprint: &str,
        source: &str,
        reason: &str,
        ledger_context: AssetLedgerContext,
        result_summary: GrantResultSummary,
        item_table: &ItemTable,
        build_items: F,
        mut next_uid: G,
    ) -> Result<GrantItemsOutcome, GrantItemsError>
    where
        F: FnOnce() -> Result<Vec<Item>, GrantItemsError>,
        G: FnMut() -> Result<u64, ItemError>,
    {
        let request_lock = keyed_lock(&self.grant_request_locks, request_id).await;
        let _request_guard = request_lock.lock().await;
        let character_lock = keyed_lock(&self.asset_character_locks, character_id).await;
        let _character_guard = character_lock.lock().await;

        match self
            .find_grant_record(request_id)
            .await
            .map_err(|_| GrantItemsError::result_query_failed())?
        {
            GrantRecordLookup::NotFound => {}
            GrantRecordLookup::Succeeded(record) => {
                return replay_or_conflict(record, character_id, request_fingerprint);
            }
            GrantRecordLookup::ResultUnavailable => {
                return Err(GrantItemsError::result_unavailable());
            }
        }

        let mut player_data = self.load_or_create_player_for_grant(character_id).await?;
        if let Some(current) = self.players.read().await.get(character_id)
            && current.persistence_revision() != player_data.persistence_revision()
        {
            METRICS.record_asset_version_conflict();
            return Err(GrantItemsError::transaction_failed());
        }
        let items = build_items()?;

        // `plan_add_items` clones the whole container and validates every split/merge before a
        // single slot is changed. UID allocation for split stacks only begins after that proof.
        let plan = player_data
            .inventory
            .plan_add_items(&items, item_table)
            .map_err(|error| GrantItemsError::item_failure(&error))?;
        player_data
            .inventory
            .apply_addition_plan(plan, &mut next_uid)
            .map_err(|error| GrantItemsError::item_failure(&error))?;
        player_data.set_data_dirty();

        let save_outcome = if self.store.enabled() {
            self.store
                .save_with_grant_record_and_ledger_context(
                    character_id,
                    &player_data,
                    request_id,
                    request_fingerprint,
                    source,
                    reason,
                    &items,
                    &result_summary,
                    ledger_context,
                )
                .await
                .map_err(|error| {
                    warn!(
                        request_id,
                        character_id,
                        error = %error,
                        "failed to persist capacity-checked inventory grant transaction"
                    );
                    match error {
                        SaveGrantRecordError::NotApplied(_) => {
                            GrantItemsError::transaction_failed()
                        }
                        SaveGrantRecordError::ResultUnknown(_) => {
                            GrantItemsError::commit_result_unknown()
                        }
                    }
                })?
        } else {
            let record = GrantRecord {
                request_id: request_id.to_string(),
                character_id: character_id.to_string(),
                request_fingerprint: request_fingerprint.to_string(),
                result_summary,
                created_at_ms: current_unix_ms(),
            };
            self.grant_records
                .write()
                .await
                .insert(request_id.to_string(), record.clone());
            SaveGrantRecordOutcome::Applied(record)
        };

        match save_outcome {
            SaveGrantRecordOutcome::Applied(record) => {
                player_data
                    .set_persistence_revision(player_data.persistence_revision().saturating_add(1));
                let mut players = self.players.write().await;
                players.insert(character_id.to_string(), player_data.clone());
                Ok(GrantItemsOutcome {
                    applied: true,
                    player_data: Some(player_data),
                    granted_items: items,
                    record,
                })
            }
            SaveGrantRecordOutcome::Existing(GrantRecordLookup::Succeeded(record)) => {
                replay_or_conflict(record, character_id, request_fingerprint)
            }
            SaveGrantRecordOutcome::Existing(GrantRecordLookup::ResultUnavailable) => {
                Err(GrantItemsError::result_unavailable())
            }
            SaveGrantRecordOutcome::Existing(GrantRecordLookup::NotFound) => {
                Err(GrantItemsError::transaction_failed())
            }
        }
    }

    /// 移除角色数据（离线）
    pub async fn remove_player(&self, character_id: &str) -> Option<PlayerData> {
        let mut players = self.players.write().await;
        players.remove(character_id)
    }

    /// 获取所有角色数据（用于批量保存）
    pub async fn get_all_dirty_players(&self) -> Vec<PlayerData> {
        let players = self.players.read().await;
        players
            .values()
            .filter(|p| p.is_data_dirty())
            .cloned()
            .collect()
    }

    /// 清空指定角色的脏标记
    pub async fn clear_dirty(&self, character_id: &str) {
        let mut players = self.players.write().await;
        if let Some(player) = players.get_mut(character_id) {
            player.clear_attr_dirty();
            player.clear_visual_dirty();
            player.clear_data_dirty();
        }
    }

    /// 获取当前在线角色玩法数据数量
    pub async fn online_count(&self) -> usize {
        let players = self.players.read().await;
        players.len()
    }

    /// 检查角色数据是否已加载
    pub async fn is_online(&self, character_id: &str) -> bool {
        let players = self.players.read().await;
        players.contains_key(character_id)
    }

    pub async fn close(&self) {
        self.store.close().await;
    }

    async fn load_or_create_player_for_grant(
        &self,
        character_id: &str,
    ) -> Result<PlayerData, GrantItemsError> {
        if let Some(player_data) = self.players.read().await.get(character_id).cloned() {
            return Ok(player_data);
        }

        if !self.store.enabled() {
            return Ok(PlayerData::new(character_id.to_string()));
        }

        match self.store.load(character_id).await {
            Ok(Some(player_data)) => Ok(player_data),
            Ok(None) => Ok(PlayerData::new(character_id.to_string())),
            Err(error) => {
                warn!(
                    character_id,
                    error = %error,
                    "failed to load character data for inventory grant"
                );
                Err(GrantItemsError::transaction_failed())
            }
        }
    }

    async fn load_or_create_player_for_asset_mutation(
        &self,
        character_id: &str,
    ) -> Result<PlayerData, PlayerAssetMutationError> {
        if let Some(player_data) = self.players.read().await.get(character_id).cloned() {
            return Ok(player_data);
        }

        if !self.store.enabled() {
            return Ok(PlayerData::new(character_id.to_string()));
        }

        match self.store.load(character_id).await {
            Ok(Some(player_data)) => Ok(player_data),
            Ok(None) => Ok(PlayerData::new(character_id.to_string())),
            Err(error) => {
                warn!(
                    character_id,
                    error = %error,
                    "failed to load player data for asset transaction"
                );
                Err(PlayerAssetMutationError::Persistence(
                    PlayerSaveError::TransactionFailed,
                ))
            }
        }
    }
}

async fn keyed_lock(locks: &Mutex<HashMap<String, Weak<Mutex<()>>>>, key: &str) -> Arc<Mutex<()>> {
    let mut locks = locks.lock().await;
    locks.retain(|_, lock| lock.strong_count() > 0);
    if let Some(lock) = locks.get(key).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(Mutex::new(()));
    locks.insert(key.to_string(), Arc::downgrade(&lock));
    lock
}

fn replay_or_conflict(
    record: GrantRecord,
    character_id: &str,
    request_fingerprint: &str,
) -> Result<GrantItemsOutcome, GrantItemsError> {
    if record.character_id != character_id || record.request_fingerprint != request_fingerprint {
        return Err(GrantItemsError::fingerprint_conflict());
    }
    Ok(GrantItemsOutcome {
        applied: false,
        player_data: None,
        granted_items: Vec::new(),
        record,
    })
}

fn asset_player_operations_enabled() -> bool {
    std::env::var("PLAYER_ASSET_TRANSACTIONS_ENABLED")
        .ok()
        .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(true)
}

fn current_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

impl Default for PlayerManager {
    fn default() -> Self {
        // 创建一个禁用的 store（用于测试）
        let store = PgPlayerStore::new_disabled();
        Self::new(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::inventory::{Buff, EquipSlot};
    use crate::core::player::grant_contract::GrantItemIntent;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn create_disabled_store() -> PgPlayerStore {
        PgPlayerStore::new_disabled()
    }

    #[tokio::test]
    async fn test_player_manager() {
        let manager = PlayerManager::new(create_disabled_store());

        // 测试获取不存在的角色
        let result = manager.get_player("chr_0000000000001").await;
        assert!(result.is_none());

        // 测试获取或创建
        let player = manager.get_or_create_player("chr_0000000000001").await;
        assert_eq!(player.character_id, "chr_0000000000001");

        // 再次获取应该返回同一个角色
        let player2 = manager.get_or_create_player("chr_0000000000001").await;
        assert_eq!(player.character_id, player2.character_id);

        // 保存后再次获取
        manager
            .save_player("chr_0000000000001", player.clone())
            .await
            .unwrap();
        let saved = manager.get_player("chr_0000000000001").await;
        assert!(saved.is_some());

        // 已加载角色数量
        assert_eq!(manager.online_count().await, 1);

        // 移除角色
        let removed = manager.remove_player("chr_0000000000001").await;
        assert!(removed.is_some());
        assert_eq!(manager.online_count().await, 0);
    }

    #[tokio::test]
    async fn player_discard_commits_through_asset_transaction_before_publishing_snapshot() {
        let manager = PlayerManager::new(create_disabled_store());
        let mut initial = manager.get_or_create_player("chr_asset_txn").await;
        initial.add_item(Item::new(90, 5001, 2, false)).unwrap();
        let initial = manager.save_player("chr_asset_txn", initial).await.unwrap();

        let committed = manager
            .discard_item_in_asset_transaction("chr_asset_txn", 90, 1)
            .await
            .unwrap();

        assert_eq!(
            committed.persistence_revision(),
            initial.persistence_revision() + 1
        );
        assert_eq!(
            committed.inventory.find_item(90).map(|item| item.count),
            Some(1)
        );
        let published = manager.get_player("chr_asset_txn").await.unwrap();
        assert_eq!(
            published.persistence_revision(),
            committed.persistence_revision()
        );
        assert_eq!(
            published.inventory.find_item(90).map(|item| item.count),
            Some(1)
        );
    }

    fn grant_summary(character_id: &str, count: u32) -> GrantResultSummary {
        GrantResultSummary {
            character_id: character_id.to_string(),
            source: "mail-claim".to_string(),
            items: vec![GrantItemIntent {
                item_id: 1001,
                count,
                binded: false,
            }],
        }
    }

    #[tokio::test]
    async fn grant_replay_returns_first_result_without_building_new_items() {
        let manager = PlayerManager::new(create_disabled_store());
        let first = manager
            .grant_items_with_request(
                "chr_1",
                "mail_claim:mail_1",
                "sha256:first",
                "mail-claim",
                "claim",
                grant_summary("chr_1", 2),
                || Ok(vec![Item::new(10, 1001, 2, false)]),
            )
            .await
            .unwrap();
        assert!(first.applied);

        let replay = manager
            .grant_items_with_request(
                "chr_1",
                "mail_claim:mail_1",
                "sha256:first",
                "mail-claim",
                "changed audit reason",
                grant_summary("chr_1", 2),
                || panic!("idempotent replay must not create another item uid"),
            )
            .await
            .unwrap();

        assert!(!replay.applied);
        assert!(replay.granted_items.is_empty());
        assert_eq!(replay.record, first.record);
        assert_eq!(
            manager
                .get_player("chr_1")
                .await
                .unwrap()
                .inventory
                .find_item(10)
                .unwrap()
                .count,
            2
        );
    }

    #[tokio::test]
    async fn grant_request_id_conflicts_across_fingerprint_or_character() {
        let manager = PlayerManager::new(create_disabled_store());
        manager
            .grant_items_with_request(
                "chr_1",
                "mail_claim:mail_1",
                "sha256:first",
                "mail-claim",
                "claim",
                grant_summary("chr_1", 2),
                || Ok(vec![Item::new(10, 1001, 2, false)]),
            )
            .await
            .unwrap();

        for (character_id, fingerprint) in [("chr_1", "sha256:changed"), ("chr_2", "sha256:first")]
        {
            let error = manager
                .grant_items_with_request(
                    character_id,
                    "mail_claim:mail_1",
                    fingerprint,
                    "mail-claim",
                    "claim",
                    grant_summary(character_id, 3),
                    || panic!("conflicting request must not create an item uid"),
                )
                .await
                .unwrap_err();
            assert_eq!(error, GrantItemsError::fingerprint_conflict());
        }
        assert!(manager.get_player("chr_2").await.is_none());
    }

    #[tokio::test]
    async fn concurrent_grant_replay_builds_items_once() {
        let manager = PlayerManager::new(create_disabled_store());
        let build_count = Arc::new(AtomicUsize::new(0));
        let mut tasks = Vec::new();
        for uid in [10, 11] {
            let manager = manager.clone();
            let build_count = build_count.clone();
            tasks.push(tokio::spawn(async move {
                manager
                    .grant_items_with_request(
                        "chr_1",
                        "mail_claim:mail_1",
                        "sha256:first",
                        "mail-claim",
                        "claim",
                        grant_summary("chr_1", 2),
                        || {
                            build_count.fetch_add(1, Ordering::Relaxed);
                            Ok(vec![Item::new(uid, 1001, 2, false)])
                        },
                    )
                    .await
                    .unwrap()
            }));
        }

        let mut applied = 0;
        for task in tasks {
            applied += usize::from(task.await.unwrap().applied);
        }
        assert_eq!(applied, 1);
        assert_eq!(build_count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn grant_load_error_does_not_build_items_or_attempt_save() {
        let store = PgPlayerStore::new_failing_load_for_test("temporary read failure");
        let store_probe = store.clone();
        let manager = PlayerManager::new(store);
        let build_count = Arc::new(AtomicUsize::new(0));
        let build_count_for_grant = build_count.clone();

        let error = manager
            .grant_items_with_request(
                "chr_1",
                "mail_claim:mail_1",
                "sha256:first",
                "mail-claim",
                "claim",
                grant_summary("chr_1", 2),
                || {
                    build_count_for_grant.fetch_add(1, Ordering::Relaxed);
                    Ok(vec![Item::new(10, 1001, 2, false)])
                },
            )
            .await
            .unwrap_err();

        assert_eq!(error, GrantItemsError::transaction_failed());
        assert_eq!(build_count.load(Ordering::Relaxed), 0);
        assert_eq!(store_probe.grant_save_attempts_for_test(), 0);
        assert!(manager.get_player("chr_1").await.is_none());
    }

    #[tokio::test]
    async fn grant_transaction_failure_does_not_publish_partial_inventory_or_record() {
        let store = PgPlayerStore::new_failing_grant_save_for_test("inventory upsert failed");
        let store_probe = store.clone();
        let manager = PlayerManager::new(store);

        let error = manager
            .grant_items_with_request(
                "chr_1",
                "mail_claim:mail_1",
                "sha256:first",
                "mail-claim",
                "claim",
                grant_summary("chr_1", 2),
                || Ok(vec![Item::new(10, 1001, 2, false)]),
            )
            .await
            .unwrap_err();

        assert_eq!(error, GrantItemsError::transaction_failed());
        assert_eq!(store_probe.grant_save_attempts_for_test(), 1);
        assert!(manager.get_player("chr_1").await.is_none());
        assert_eq!(
            manager
                .find_grant_record("mail_claim:mail_1")
                .await
                .unwrap(),
            GrantRecordLookup::NotFound
        );
    }

    #[tokio::test]
    async fn save_failure_does_not_publish_a_normal_player_mutation() {
        let store = PgPlayerStore::new_failing_save_for_test("inventory update failed");
        let store_probe = store.clone();
        let manager = PlayerManager::new(store);
        let mut candidate = PlayerData::new("chr_1".to_string());
        candidate.add_item(Item::new(10, 1001, 1, false)).unwrap();

        let error = manager.save_player("chr_1", candidate).await.unwrap_err();

        assert_eq!(error, PlayerSaveError::TransactionFailed);
        assert_eq!(store_probe.save_attempts_for_test(), 1);
        assert!(manager.get_player("chr_1").await.is_none());
    }

    #[tokio::test]
    async fn unknown_grant_commit_is_query_first_and_does_not_publish_memory() {
        let store = PgPlayerStore::new_unknown_grant_commit_for_test("connection dropped");
        let store_probe = store.clone();
        let manager = PlayerManager::new(store);

        let error = manager
            .grant_items_with_request(
                "chr_1",
                "mail_claim:mail_1",
                "sha256:first",
                "mail-claim",
                "claim",
                grant_summary("chr_1", 2),
                || Ok(vec![Item::new(10, 1001, 2, false)]),
            )
            .await
            .unwrap_err();

        assert_eq!(error, GrantItemsError::commit_result_unknown());
        assert_eq!(error.result_state, "unknown");
        assert_eq!(store_probe.grant_save_attempts_for_test(), 1);
        assert!(manager.get_player("chr_1").await.is_none());
    }

    #[tokio::test]
    async fn stale_player_snapshot_cannot_overwrite_a_committed_grant() {
        let manager = PlayerManager::new(create_disabled_store());
        let mut stale_player_operation = manager.get_or_create_player("chr_1").await;

        manager
            .grant_items_with_request(
                "chr_1",
                "mail_claim:mail_1",
                "sha256:first",
                "mail-claim",
                "claim",
                grant_summary("chr_1", 2),
                || Ok(vec![Item::new(10, 1001, 2, false)]),
            )
            .await
            .unwrap();

        stale_player_operation
            .add_item(Item::new(11, 1002, 1, false))
            .unwrap();
        let error = manager
            .save_player("chr_1", stale_player_operation)
            .await
            .unwrap_err();

        assert_eq!(error, PlayerSaveError::VersionConflict);
        let current = manager.get_player("chr_1").await.unwrap();
        assert_eq!(current.inventory.find_item(10).unwrap().count, 2);
        assert!(current.inventory.find_item(11).is_none());
    }

    #[tokio::test]
    async fn stale_player_save_returns_version_conflict_without_replacing_newer_snapshot() {
        let manager = PlayerManager::new(create_disabled_store());
        let initial = manager.get_or_create_player("chr_1").await;
        let mut first = initial.clone();
        first.add_item(Item::new(10, 1001, 1, false)).unwrap();
        manager.save_player("chr_1", first).await.unwrap();

        let mut stale = initial;
        stale.add_item(Item::new(11, 1002, 1, false)).unwrap();
        let error = manager.save_player("chr_1", stale).await.unwrap_err();

        assert_eq!(error, PlayerSaveError::VersionConflict);
        let current = manager.get_player("chr_1").await.unwrap();
        assert!(current.inventory.find_item(10).is_some());
        assert!(current.inventory.find_item(11).is_none());
    }

    #[tokio::test]
    async fn different_character_asset_locks_do_not_wait_for_each_other() {
        let manager = PlayerManager::new(create_disabled_store());
        let first_lock = keyed_lock(&manager.asset_character_locks, "chr_1").await;
        let _first_guard = first_lock.lock().await;
        let second_lock = keyed_lock(&manager.asset_character_locks, "chr_2").await;

        let second_guard =
            tokio::time::timeout(std::time::Duration::from_millis(50), second_lock.lock()).await;
        assert!(
            second_guard.is_ok(),
            "different characters must not share a write lock"
        );
    }

    #[tokio::test]
    async fn sequential_unique_grants_reclaim_dead_keyed_locks() {
        let manager = PlayerManager::new(create_disabled_store());
        for index in 0..100u64 {
            let character_id = format!("chr_{index}");
            manager
                .grant_items_with_request(
                    &character_id,
                    &format!("mail_claim:mail_{index}"),
                    &format!("sha256:{index}"),
                    "mail-claim",
                    "claim",
                    grant_summary(&character_id, 1),
                    || Ok(vec![Item::new(index + 1, 1001, 1, false)]),
                )
                .await
                .unwrap();
        }

        assert!(manager.grant_request_locks.lock().await.len() <= 1);
        assert!(manager.asset_character_locks.lock().await.len() <= 1);
    }

    #[tokio::test]
    async fn same_account_characters_keep_inventory_warehouse_equipment_and_buffs_isolated() {
        let manager = PlayerManager::new(create_disabled_store());
        let mut first_character = PlayerData::new("chr_0000000000001".to_string());
        first_character
            .add_item(Item::new(1, 1001, 3, false))
            .unwrap();
        first_character
            .warehouse
            .add_item(Item::new(2, 5001, 8, false))
            .unwrap();
        first_character
            .equipment
            .equip(EquipSlot::Weapon, Item::new(3, 1001, 1, false))
            .unwrap();
        first_character
            .buffs
            .push(Buff::new(4001, "test-buff".to_string(), 30_000));

        manager
            .save_player("chr_0000000000001", first_character)
            .await
            .unwrap();
        let other_character = manager.get_or_create_player("chr_0000000000002").await;
        let saved_first_character = manager.get_player("chr_0000000000001").await.unwrap();

        assert_eq!(saved_first_character.inventory.item_count(), 1);
        assert_eq!(saved_first_character.warehouse.item_count(), 1);
        assert_eq!(saved_first_character.equipment.equipped_count(), 1);
        assert_eq!(saved_first_character.buffs.len(), 1);
        assert_eq!(other_character.inventory.item_count(), 0);
        assert_eq!(other_character.warehouse.item_count(), 0);
        assert_eq!(other_character.equipment.equipped_count(), 0);
        assert_eq!(other_character.buffs.len(), 0);
        assert_eq!(other_character.character_id, "chr_0000000000002");
    }
}
