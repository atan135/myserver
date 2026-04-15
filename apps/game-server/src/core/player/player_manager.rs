use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{info, warn};

use super::mysql_player_store::MySqlPlayerStore;
use crate::core::inventory::PlayerData;

/// 玩家数据管理器
/// 负责管理所有在线玩家的背包/属性数据
#[derive(Clone)]
pub struct PlayerManager {
    players: Arc<RwLock<HashMap<String, PlayerData>>>,
    store: MySqlPlayerStore,
}

impl PlayerManager {
    /// 创建新的 PlayerManager
    pub fn new(store: MySqlPlayerStore) -> Self {
        Self {
            players: Arc::new(RwLock::new(HashMap::new())),
            store,
        }
    }

    /// 获取玩家数据（如果不存在返回 None）
    pub async fn get_player(&self, player_id: &str) -> Option<PlayerData> {
        let players = self.players.read().await;
        players.get(player_id).cloned()
    }

    /// 获取或创建玩家数据
    pub async fn get_or_create_player(&self, player_id: &str) -> PlayerData {
        // 先尝试读取在线玩家
        {
            let players = self.players.read().await;
            if let Some(data) = players.get(player_id) {
                return data.clone();
            }
        }

        // 尝试从数据库加载
        match self.store.load(player_id).await {
            Ok(Some(data)) => {
                let player_data = data;
                let mut players = self.players.write().await;
                players.insert(player_id.to_string(), player_data.clone());
                return player_data;
            }
            Ok(None) => {
                // 数据库中没有，创建新玩家
                info!(player_id = %player_id, "creating new player");
            }
            Err(e) => {
                warn!(player_id = %player_id, error = %e, "failed to load player from DB, creating new");
            }
        }

        // 不存在则创建新玩家数据
        let new_player = PlayerData::new(player_id.to_string());
        let mut players = self.players.write().await;
        players.insert(player_id.to_string(), new_player.clone());
        new_player
    }

    /// 保存玩家数据
    pub async fn save_player(&self, player_id: &str, data: PlayerData) {
        // 先更新内存
        let mut players = self.players.write().await;
        players.insert(player_id.to_string(), data.clone());

        // 异步持久化到数据库
        if let Err(e) = self.store.save(player_id, &data).await {
            warn!(player_id = %player_id, error = %e, "failed to persist player data");
        }
    }

    /// 移除玩家数据（离线）
    pub async fn remove_player(&self, player_id: &str) -> Option<PlayerData> {
        let mut players = self.players.write().await;
        players.remove(player_id)
    }

    /// 获取所有玩家数据（用于批量保存）
    pub async fn get_all_dirty_players(&self) -> Vec<PlayerData> {
        let players = self.players.read().await;
        players
            .values()
            .filter(|p| p.is_data_dirty())
            .cloned()
            .collect()
    }

    /// 清空指定玩家的脏标记
    pub async fn clear_dirty(&self, player_id: &str) {
        let mut players = self.players.write().await;
        if let Some(player) = players.get_mut(player_id) {
            player.clear_attr_dirty();
            player.clear_visual_dirty();
            player.clear_data_dirty();
        }
    }

    /// 获取当前在线玩家数量
    pub async fn online_count(&self) -> usize {
        let players = self.players.read().await;
        players.len()
    }

    /// 检查玩家是否在线
    pub async fn is_online(&self, player_id: &str) -> bool {
        let players = self.players.read().await;
        players.contains_key(player_id)
    }
}

impl Default for PlayerManager {
    fn default() -> Self {
        // 创建一个禁用的 store（用于测试）
        let store = MySqlPlayerStore::new_disabled();
        Self::new(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_disabled_store() -> MySqlPlayerStore {
        MySqlPlayerStore {
            pool: None,
        }
    }

    #[tokio::test]
    async fn test_player_manager() {
        let manager = PlayerManager::new(create_disabled_store());

        // 测试获取不存在的玩家
        let result = manager.get_player("player1").await;
        assert!(result.is_none());

        // 测试获取或创建
        let player = manager.get_or_create_player("player1").await;
        assert_eq!(player.player_id, "player1");

        // 再次获取应该返回同一个玩家
        let player2 = manager.get_or_create_player("player1").await;
        assert_eq!(player.player_id, player2.player_id);

        // 保存后再次获取
        manager.save_player("player1", player.clone()).await;
        let saved = manager.get_player("player1").await;
        assert!(saved.is_some());

        // 在线数量
        assert_eq!(manager.online_count().await, 1);

        // 移除玩家
        let removed = manager.remove_player("player1").await;
        assert!(removed.is_some());
        assert_eq!(manager.online_count().await, 0);
    }
}
