//! 匹配池实现

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::ModeConfig;
use crate::metrics::METRICS;
use crate::runtime_store::{
    MatchRuntimeSnapshot, SharedMatchRuntimeStore, StoredMatchCandidate, StoredMatchTask,
};
use crate::state::{SharedPlayerState, new_player_state_store};

use super::candidate::MatchCandidate;

/// 匹配任务
#[derive(Clone)]
pub struct MatchTask {
    pub match_id: String,
    pub mode: String,
    pub players: Vec<String>,
    pub room_id: Option<String>,
    pub joined_players: HashSet<String>,
    pub active_players: HashSet<String>,
}

impl MatchTask {
    pub fn new(match_id: String, mode: String, players: Vec<String>) -> Self {
        let active_players = players.iter().cloned().collect();
        Self {
            match_id,
            mode,
            players,
            room_id: None,
            joined_players: HashSet::new(),
            active_players,
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
    /// 可选运行时持久化存储
    runtime_store: Option<SharedMatchRuntimeStore>,
}

impl Default for MatchPool {
    fn default() -> Self {
        Self::new(new_player_state_store())
    }
}

impl MatchPool {
    fn total_candidates_from_pools(pools: &HashMap<String, ModePool>) -> u64 {
        pools
            .values()
            .map(|pool| pool.candidates.len() as u64)
            .sum()
    }

    pub fn new(player_state: SharedPlayerState) -> Self {
        Self {
            pools: RwLock::new(HashMap::new()),
            matches: RwLock::new(HashMap::new()),
            player_state,
            runtime_store: None,
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
            runtime_store: None,
        }
    }

    pub fn with_modes_and_runtime_store(
        player_state: SharedPlayerState,
        modes: HashMap<String, ModeConfig>,
        runtime_store: SharedMatchRuntimeStore,
    ) -> Self {
        let pool = Self::with_modes(player_state, modes);
        Self {
            runtime_store: Some(runtime_store),
            ..pool
        }
    }

    pub async fn apply_snapshot(&self, snapshot: &MatchRuntimeSnapshot) {
        {
            let mut pools = self.pools.write().await;
            for (mode, candidates) in &snapshot.candidates_by_mode {
                if let Some(pool) = pools.get_mut(mode) {
                    pool.candidates = candidates
                        .iter()
                        .cloned()
                        .map(StoredMatchCandidate::into_candidate)
                        .filter(|candidate| !candidate.is_timeout())
                        .collect();
                    pool.candidates
                        .sort_by_key(|candidate| candidate.created_at);
                }
            }
            METRICS.set_pool_size(Self::total_candidates_from_pools(&pools));
        }

        {
            let mut matches = self.matches.write().await;
            matches.clear();
            matches.extend(
                snapshot
                    .matches
                    .iter()
                    .map(|(match_id, task)| (match_id.clone(), task.clone().into_task())),
            );
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
        METRICS.set_pool_size(Self::total_candidates_from_pools(&pools));
    }

    /// 添加候选人到匹配池
    pub async fn add_candidate(&self, candidate: MatchCandidate) {
        let updated = {
            let mut pools = self.pools.write().await;
            let mut updated = None;
            if let Some(pool) = pools.get_mut(&candidate.mode) {
                let count_before = pool.candidates.len();
                pool.candidates.push(candidate.clone());
                let count_after = pool.candidates.len();
                updated = Some((
                    count_before,
                    count_after,
                    pool.config.total_size,
                    Self::total_candidates_from_pools(&pools),
                ));
            }
            updated
        };

        if let Some((count_before, count_after, required, pool_size)) = updated {
            METRICS.set_pool_size(pool_size);
            if let Some(runtime_store) = &self.runtime_store {
                if let Err(error) = runtime_store
                    .save_candidate(StoredMatchCandidate::from_candidate(&candidate))
                    .await
                {
                    tracing::warn!(
                        player_id = %candidate.player_id,
                        match_id = %candidate.match_id,
                        mode = %candidate.mode,
                        error = %error,
                        "failed to persist match candidate"
                    );
                }
            }
            tracing::info!(
                mode = %candidate.mode,
                player_id = %candidate.player_id,
                match_id = %candidate.match_id,
                count_before = count_before,
                count_after = count_after,
                required = required,
                "candidate added to pool"
            );
        }
    }

    /// 从匹配池移除候选人
    pub async fn remove_candidate(&self, player_id: &str, mode: &str) -> Option<MatchCandidate> {
        let (removed, pool_size) = {
            let mut pools = self.pools.write().await;
            let mut removed = None;
            if let Some(pool) = pools.get_mut(mode) {
                if let Some(pos) = pool
                    .candidates
                    .iter()
                    .position(|c| c.player_id == *player_id)
                {
                    removed = Some(pool.candidates.remove(pos));
                }
            }
            let pool_size = Self::total_candidates_from_pools(&pools);
            (removed, pool_size)
        };
        if removed.is_some() {
            METRICS.set_pool_size(pool_size);
            if let Some(runtime_store) = &self.runtime_store {
                if let Err(error) = runtime_store.remove_candidate(player_id, mode).await {
                    tracing::warn!(
                        player_id,
                        mode,
                        error = %error,
                        "failed to remove persisted match candidate"
                    );
                }
            }
        }
        removed
    }

    /// 尝试撮合
    /// 返回匹配的候选人列表，如果人数不够返回 None
    pub async fn try_match(&self, mode: &str) -> Option<Vec<MatchCandidate>> {
        let (matched, pool_size) = {
            let mut pools = self.pools.write().await;
            let mut matched: Option<Vec<MatchCandidate>> = None;
            if let Some(pool) = pools.get_mut(mode) {
                let total_size = pool.config.total_size;

                // 按等待时间排序
                pool.candidates.sort_by_key(|c| c.created_at);

                // 检查是否有足够的候选人
                if pool.candidates.len() >= total_size {
                    // 取出足够的候选人
                    matched = Some(pool.candidates.drain(..total_size).collect());
                }
            }
            let pool_size = Self::total_candidates_from_pools(&pools);
            (matched, pool_size)
        };
        if matched.is_some() {
            METRICS.set_pool_size(pool_size);
            if let Some(runtime_store) = &self.runtime_store {
                if let Some(candidates) = matched.as_ref() {
                    for candidate in candidates {
                        if let Err(error) = runtime_store
                            .remove_candidate(&candidate.player_id, &candidate.mode)
                            .await
                        {
                            tracing::warn!(
                                player_id = %candidate.player_id,
                                mode = %candidate.mode,
                                error = %error,
                                "failed to remove persisted candidate after match"
                            );
                        }
                    }
                }
            }
        }
        matched
    }

    /// 创建匹配任务
    pub async fn create_match_task(&self, match_id: String, mode: String, players: Vec<String>) {
        let task = MatchTask::new(match_id.clone(), mode, players);
        {
            let mut matches = self.matches.write().await;
            matches.insert(match_id.clone(), task.clone());
        }
        if let Some(runtime_store) = &self.runtime_store {
            if let Err(error) = runtime_store
                .save_match_task(StoredMatchTask::from_task(&task))
                .await
            {
                tracing::warn!(
                    match_id,
                    error = %error,
                    "failed to persist match task"
                );
            }
        }
    }

    /// 获取匹配任务
    pub async fn get_match_task(&self, match_id: &str) -> Option<MatchTask> {
        self.matches.read().await.get(match_id).cloned()
    }

    /// 更新匹配任务的 room_id
    pub async fn update_match_room(&self, match_id: &str, room_id: String) {
        let updated_task = {
            let mut matches = self.matches.write().await;
            matches.get_mut(match_id).map(|task| {
                task.room_id = Some(room_id);
                task.clone()
            })
        };

        if let Some(task) = updated_task.as_ref() {
            if let Some(runtime_store) = &self.runtime_store {
                if let Err(error) = runtime_store
                    .save_match_task(StoredMatchTask::from_task(task))
                    .await
                {
                    tracing::warn!(
                        match_id,
                        error = %error,
                        "failed to persist match task room"
                    );
                }
            }
        }
    }

    /// 标记玩家已进入房间
    pub async fn mark_player_joined(&self, match_id: &str, player_id: &str) -> Option<MatchTask> {
        let updated_task = {
            let mut matches = self.matches.write().await;
            let task = matches.get_mut(match_id)?;
            if task.players.iter().any(|player| player == player_id) {
                task.joined_players.insert(player_id.to_string());
                task.active_players.insert(player_id.to_string());
            }
            task.clone()
        };

        if let Some(runtime_store) = &self.runtime_store {
            if let Err(error) = runtime_store
                .save_match_task(StoredMatchTask::from_task(&updated_task))
                .await
            {
                tracing::warn!(
                    match_id,
                    player_id,
                    error = %error,
                    "failed to persist joined match task"
                );
            }
        }
        Some(updated_task)
    }

    /// 标记玩家已离开房间
    pub async fn mark_player_left(&self, match_id: &str, player_id: &str) -> Option<MatchTask> {
        let updated_task = {
            let mut matches = self.matches.write().await;
            let task = matches.get_mut(match_id)?;
            task.active_players.remove(player_id);
            task.clone()
        };

        if let Some(runtime_store) = &self.runtime_store {
            if let Err(error) = runtime_store
                .save_match_task(StoredMatchTask::from_task(&updated_task))
                .await
            {
                tracing::warn!(
                    match_id,
                    player_id,
                    error = %error,
                    "failed to persist left match task"
                );
            }
        }
        Some(updated_task)
    }

    /// 删除匹配任务
    pub async fn remove_match_task(&self, match_id: &str) -> Option<MatchTask> {
        let removed = {
            let mut matches = self.matches.write().await;
            matches.remove(match_id)
        };
        if removed.is_some() {
            if let Some(runtime_store) = &self.runtime_store {
                if let Err(error) = runtime_store.remove_match_task(match_id).await {
                    tracing::warn!(
                        match_id,
                        error = %error,
                        "failed to remove persisted match task"
                    );
                }
            }
        }
        removed
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
        let (removed, pool_size) = {
            let mut removed = Vec::new();
            let mut pools = self.pools.write().await;

            for pool in pools.values_mut() {
                let to_remove: Vec<MatchCandidate> = pool
                    .candidates
                    .iter()
                    .filter(|c| c.is_timeout())
                    .cloned()
                    .collect();
                pool.candidates.retain(|c| !c.is_timeout());
                removed.extend(to_remove);
            }
            let pool_size = Self::total_candidates_from_pools(&pools);
            (removed, pool_size)
        };
        METRICS.set_pool_size(pool_size);
        if let Some(runtime_store) = &self.runtime_store {
            for candidate in &removed {
                if let Err(error) = runtime_store
                    .remove_candidate(&candidate.player_id, &candidate.mode)
                    .await
                {
                    tracing::warn!(
                        player_id = %candidate.player_id,
                        mode = %candidate.mode,
                        error = %error,
                        "failed to remove timed out persisted candidate"
                    );
                }
            }
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

pub fn new_match_pool_with_modes_and_runtime_store(
    player_state: SharedPlayerState,
    modes: std::collections::HashMap<String, ModeConfig>,
    runtime_store: SharedMatchRuntimeStore,
) -> SharedMatchPool {
    Arc::new(MatchPool::with_modes_and_runtime_store(
        player_state,
        modes,
        runtime_store,
    ))
}
