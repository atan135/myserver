use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use super::db_player_store::{
    GrantRecord, GrantRecordLookup, PgPlayerStore, SaveGrantRecordError, SaveGrantRecordOutcome,
};
use super::grant_contract::GrantResultSummary;
use crate::core::inventory::{Item, ItemError, PlayerData};

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
}

/// 玩家数据管理器
/// 负责管理所有在线角色的背包/属性数据
#[derive(Clone)]
pub struct PlayerManager {
    players: Arc<RwLock<HashMap<String, PlayerData>>>,
    store: PgPlayerStore,
    grant_records: Arc<RwLock<HashMap<String, GrantRecord>>>,
    grant_request_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    grant_character_locks: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
}

impl PlayerManager {
    /// 创建新的 PlayerManager
    pub fn new(store: PgPlayerStore) -> Self {
        Self {
            players: Arc::new(RwLock::new(HashMap::new())),
            store,
            grant_records: Arc::new(RwLock::new(HashMap::new())),
            grant_request_locks: Arc::new(Mutex::new(HashMap::new())),
            grant_character_locks: Arc::new(Mutex::new(HashMap::new())),
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

    /// 保存角色玩法数据
    pub async fn save_player(&self, character_id: &str, data: PlayerData) {
        // 先更新内存
        let mut players = self.players.write().await;
        players.insert(character_id.to_string(), data.clone());

        // 异步持久化到数据库
        if let Err(e) = self.store.save(character_id, &data).await {
            warn!(character_id = %character_id, error = %e, "failed to persist character gameplay data");
        }
    }

    pub async fn grant_items(
        &self,
        character_id: &str,
        items: &[Item],
    ) -> Result<PlayerData, ItemError> {
        let mut player_data = self.get_or_create_player(character_id).await;

        for item in items {
            player_data.add_item(item.clone())?;
        }

        self.save_player(character_id, player_data.clone()).await;
        Ok(player_data)
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
        let character_lock = keyed_lock(&self.grant_character_locks, character_id).await;
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
            .await;
        let saved = manager.get_player("chr_0000000000001").await;
        assert!(saved.is_some());

        // 已加载角色数量
        assert_eq!(manager.online_count().await, 1);

        // 移除角色
        let removed = manager.remove_player("chr_0000000000001").await;
        assert!(removed.is_some());
        assert_eq!(manager.online_count().await, 0);
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
        assert!(manager.grant_character_locks.lock().await.len() <= 1);
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
            .await;
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
