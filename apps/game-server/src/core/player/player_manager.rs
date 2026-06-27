use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use super::db_player_store::PgPlayerStore;
use crate::core::inventory::{Item, ItemError, PlayerData};

#[derive(Debug, Clone)]
pub struct GrantItemsOutcome {
    pub applied: bool,
    pub player_data: PlayerData,
}

/// 玩家数据管理器
/// 负责管理所有在线角色的背包/属性数据
#[derive(Clone)]
pub struct PlayerManager {
    players: Arc<RwLock<HashMap<String, PlayerData>>>,
    store: PgPlayerStore,
}

impl PlayerManager {
    /// 创建新的 PlayerManager
    pub fn new(store: PgPlayerStore) -> Self {
        Self {
            players: Arc::new(RwLock::new(HashMap::new())),
            store,
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

    pub async fn grant_items_with_request(
        &self,
        character_id: &str,
        items: &[Item],
        request_id: &str,
        source: &str,
        reason: &str,
    ) -> Result<GrantItemsOutcome, String> {
        let mut player_data = self.get_or_create_player(character_id).await;

        for item in items {
            player_data
                .add_item(item.clone())
                .map_err(|error| error.as_str().to_string())?;
        }

        let applied = if self.store.enabled() {
            self.store
                .save_with_grant_record(
                    character_id,
                    &player_data,
                    request_id,
                    source,
                    reason,
                    items,
                )
                .await?
        } else {
            true
        };

        if applied {
            let mut players = self.players.write().await;
            players.insert(character_id.to_string(), player_data.clone());
        }

        Ok(GrantItemsOutcome {
            applied,
            player_data,
        })
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
