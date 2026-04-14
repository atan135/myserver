//! 匹配池实现

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::ModeConfig;
use crate::state::{new_player_state_store, SharedPlayerState};

use super::candidate::MatchCandidate;

/// 匹配任务
#[derive(Clone)]
pub struct MatchTask {
    pub match_id: String,
    pub mode: String,
    pub players: Vec<String>,
    pub room_id: Option<String>,
}

impl MatchTask {
    pub fn new(match_id: String, mode: String, players: Vec<String>) -> Self {
        Self {
            match_id,
            mode,
            players,
            room_id: None,
        }
    }
}

/// 单个模式的匹配池
struct ModePool {
    mode: String,
    config: ModeConfig,
    candidates: Vec<MatchCandidate>,
}

/// 匹配池
pub struct MatchPool {
    /// 模式 -> 匹配池
    pools: RwLock<HashMap<String, ModePool>>,
    /// 活跃的匹配任务
    matches: RwLock<HashMap<String, MatchTask>>,
    /// 玩家状态管理
    player_state: SharedPlayerState,
}

impl Default for MatchPool {
    fn default() -> Self {
        Self::new(new_player_state_store())
    }
}

impl MatchPool {
    pub fn new(player_state: SharedPlayerState) -> Self {
        Self {
            pools: RwLock::new(HashMap::new()),
            matches: RwLock::new(HashMap::new()),
            player_state,
        }
    }

    /// 使用指定模式创建匹配池
    pub fn with_modes(player_state: SharedPlayerState, modes: HashMap<String, ModeConfig>) -> Self {
        let pools = modes
            .into_iter()
            .map(|(mode, config)| {
                (
                    mode.clone(),
                    ModePool {
                        mode,
                        config,
                        candidates: Vec::new(),
                    },
                )
            })
            .collect();
        Self {
            pools: RwLock::new(pools),
            matches: RwLock::new(HashMap::new()),
            player_state,
        }
    }

    /// 注册一个模式
    pub async fn register_mode(&self, mode: String, config: ModeConfig) {
        let mut pools = self.pools.write().await;
        pools.insert(
            mode.clone(),
            ModePool {
                mode,
                config,
                candidates: Vec::new(),
            },
        );
    }

    /// 添加候选人到匹配池
    pub async fn add_candidate(&self, candidate: MatchCandidate) {
        let mut pools = self.pools.write().await;
        if let Some(pool) = pools.get_mut(&candidate.mode) {
            let count_before = pool.candidates.len();
            pool.candidates.push(candidate.clone());
            let count_after = pool.candidates.len();
            tracing::info!(
                mode = %candidate.mode,
                player_id = %candidate.player_id,
                match_id = %candidate.match_id,
                count_before = count_before,
                count_after = count_after,
                required = pool.config.total_size,
                "candidate added to pool"
            );
        }
    }

    /// 从匹配池移除候选人
    pub async fn remove_candidate(&self, player_id: &str, mode: &str) -> Option<MatchCandidate> {
        let mut pools = self.pools.write().await;
        if let Some(pool) = pools.get_mut(mode) {
            if let Some(pos) = pool.candidates.iter().position(|c| c.player_id == *player_id) {
                return Some(pool.candidates.remove(pos));
            }
        }
        None
    }

    /// 尝试撮合
    /// 返回匹配的候选人列表，如果人数不够返回 None
    pub async fn try_match(&self, mode: &str) -> Option<Vec<MatchCandidate>> {
        let mut pools = self.pools.write().await;
        if let Some(pool) = pools.get_mut(mode) {
            let total_size = pool.config.total_size;

            // 按等待时间排序
            pool.candidates.sort_by_key(|c| c.created_at);

            // 检查是否有足够的候选人
            if pool.candidates.len() >= total_size {
                // 取出足够的候选人
                let matched: Vec<MatchCandidate> = pool.candidates.drain(..total_size).collect();
                return Some(matched);
            }
        }
        None
    }

    /// 创建匹配任务
    pub async fn create_match_task(
        &self,
        match_id: String,
        mode: String,
        players: Vec<String>,
    ) {
        let mut matches = self.matches.write().await;
        matches.insert(match_id.clone(), MatchTask::new(match_id, mode, players));
    }

    /// 获取匹配任务
    pub async fn get_match_task(&self, match_id: &str) -> Option<MatchTask> {
        self.matches.read().await.get(match_id).cloned()
    }

    /// 更新匹配任务的 room_id
    pub async fn update_match_room(&self, match_id: &str, room_id: String) {
        let mut matches = self.matches.write().await;
        if let Some(task) = matches.get_mut(match_id) {
            task.room_id = Some(room_id);
        }
    }

    /// 删除匹配任务
    pub async fn remove_match_task(&self, match_id: &str) -> Option<MatchTask> {
        self.matches.write().await.remove(match_id)
    }

    /// 获取玩家状态管理
    pub fn player_state(&self) -> &SharedPlayerState {
        &self.player_state
    }

    /// 获取匹配池中某模式的候选人数
    pub async fn candidate_count(&self, mode: &str) -> usize {
        let pools = self.pools.read().await;
        pools.get(mode).map(|p| p.candidates.len()).unwrap_or(0)
    }

    /// 清理超时的候选人
    pub async fn cleanup_timeout(&self) -> Vec<MatchCandidate> {
        let mut removed = Vec::new();
        let mut pools = self.pools.write().await;

        for pool in pools.values_mut() {
            let to_remove: Vec<MatchCandidate> = pool.candidates.iter()
                .filter(|c| c.is_timeout())
                .cloned()
                .collect();
            pool.candidates.retain(|c| !c.is_timeout());
            removed.extend(to_remove);
        }

        removed
    }
}

pub type SharedMatchPool = Arc<MatchPool>;

pub fn new_match_pool(player_state: SharedPlayerState) -> SharedMatchPool {
    Arc::new(MatchPool::new(player_state))
}

pub fn new_match_pool_with_modes(
    player_state: SharedPlayerState,
    modes: std::collections::HashMap<String, ModeConfig>,
) -> SharedMatchPool {
    Arc::new(MatchPool::with_modes(player_state, modes))
}
