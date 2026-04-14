//! 简单撮合器

use std::sync::Arc;
use std::time::Duration;
use tracing::info;

use crate::config::Config;
use crate::proto::myserver::matchservice::MatchEvent;
use crate::pool::{new_match_pool_with_modes, MatchCandidate, SharedMatchPool};
use crate::state::{new_player_state_store, PlayerMatchStatus, SharedPlayerState};

/// 简单撮合器
pub struct SimpleMatcher {
    pool: SharedMatchPool,
    player_state: SharedPlayerState,
    config: Config,
}

impl SimpleMatcher {
    pub fn new(config: Config) -> Self {
        let player_state = new_player_state_store();
        let pool = new_match_pool_with_modes(player_state.clone(), config.modes.clone());

        Self {
            pool,
            player_state,
            config,
        }
    }

    pub fn pool(&self) -> &SharedMatchPool {
        &self.pool
    }

    pub fn player_state(&self) -> &SharedPlayerState {
        &self.player_state
    }

    /// 开始匹配
    pub async fn start_match(
        &self,
        player_id: String,
        mode: String,
    ) -> Result<String, crate::error::MatchError> {
        // 检查模式是否有效
        let mode_config = self
            .config
            .get_mode(&mode)
            .ok_or_else(|| crate::error::MatchError::InvalidMode(mode.clone()))?;

        // 检查玩家当前状态
        let current_status = self.player_state.get_status(&player_id).await;
        if current_status != PlayerMatchStatus::Idle {
            return Err(crate::error::MatchError::AlreadyMatching(player_id));
        }

        // 生成 match_id
        let match_id = uuid::Uuid::new_v4().to_string();

        // 创建候选人
        let timeout_at = std::time::Instant::now()
            + Duration::from_secs(mode_config.match_timeout_secs);
        let candidate = MatchCandidate::new(
            player_id.clone(),
            match_id.clone(),
            mode.clone(),
            timeout_at,
        );

        // 添加到匹配池
        self.pool.add_candidate(candidate).await;

        // 更新玩家状态
        self.player_state
            .set_status(&player_id, PlayerMatchStatus::Matching)
            .await;
        self.player_state
            .set_context(
                &player_id,
                crate::state::PlayerMatchContext {
                    match_id: match_id.clone(),
                    mode: mode.clone(),
                    room_id: None,
                    token: None,
                },
            )
            .await;

        info!(
            player_id = %player_id,
            mode = %mode,
            match_id = %match_id,
            timeout_secs = mode_config.match_timeout_secs,
            required_players = mode_config.total_size,
            "player started matching"
        );

        Ok(match_id)
    }

    /// 取消匹配
    pub async fn cancel_match(
        &self,
        player_id: &str,
        match_id: &str,
    ) -> Result<(), crate::error::MatchError> {
        let ctx = self.player_state.get_context(player_id).await;

        let ctx = match ctx {
            Some(c) => c,
            None => {
                return Err(crate::error::MatchError::NotMatching(player_id.to_string()));
            }
        };

        if ctx.match_id != match_id {
            return Err(crate::error::MatchError::MatchNotFound(match_id.to_string()));
        }

        let status = self.player_state.get_status(player_id).await;
        if status != PlayerMatchStatus::Matching {
            return Err(crate::error::MatchError::NotMatching(player_id.to_string()));
        }

        // 从匹配池移除
        self.pool.remove_candidate(player_id, &ctx.mode).await;

        // 重置状态
        self.player_state.set_status(player_id, PlayerMatchStatus::Idle).await;
        self.player_state.clear_context(player_id).await;

        // 发送取消事件
        let event = MatchEvent {
            event: "match_cancelled".to_string(),
            match_id: match_id.to_string(),
            room_id: String::new(),
            token: String::new(),
            error_code: String::new(),
        };
        let _ = self.player_state.send_event(player_id, event).await;

        info!(
            player_id = %player_id,
            match_id = %match_id,
            mode = %ctx.mode,
            "match cancelled"
        );

        Ok(())
    }

    /// 查询匹配状态
    pub async fn get_status(
        &self,
        player_id: &str,
    ) -> Result<crate::proto::myserver::matchservice::MatchStatusRes, crate::error::MatchError> {
        let ctx = self.player_state.get_context(player_id).await;

        let status = self.player_state.get_status(player_id).await;

        let (match_id, room_id, token, estimated_wait) = match ctx {
            Some(c) => (
                c.match_id,
                c.room_id.unwrap_or_default(),
                c.token.unwrap_or_default(),
                0,
            ),
            None => (String::new(), String::new(), String::new(), 0),
        };

        let status_str = match status {
            PlayerMatchStatus::Idle => "idle",
            PlayerMatchStatus::Matching => "matching",
            PlayerMatchStatus::Matched => "matched",
            PlayerMatchStatus::InRoom => "in_room",
        };

        Ok(crate::proto::myserver::matchservice::MatchStatusRes {
            status: status_str.to_string(),
            match_id,
            room_id,
            token,
            estimated_wait_secs: estimated_wait,
        })
    }

    /// 玩家进入房间回调
    pub async fn player_joined(
        &self,
        match_id: &str,
        player_id: &str,
        room_id: &str,
    ) -> Result<(), crate::error::MatchError> {
        let ctx = self.player_state.get_context(player_id).await;

        let ctx = match ctx {
            Some(c) => c,
            None => {
                return Err(crate::error::MatchError::NotMatching(player_id.to_string()));
            }
        };

        if ctx.match_id != match_id {
            return Err(crate::error::MatchError::MatchNotFound(match_id.to_string()));
        }

        // 更新状态
        self.player_state.set_status(player_id, PlayerMatchStatus::InRoom).await;

        // 获取匹配任务查看其他玩家
        let task = self.pool.get_match_task(match_id).await;
        let player_count = task.as_ref().map(|t| t.players.len()).unwrap_or(1);

        info!(
            player_id = %player_id,
            match_id = %match_id,
            room_id = %room_id,
            mode = %ctx.mode,
            total_players = player_count,
            "player joined room"
        );

        Ok(())
    }

    /// 玩家离开回调
    pub async fn player_left(
        &self,
        match_id: &str,
        player_id: &str,
        reason: &str,
    ) -> Result<bool, crate::error::MatchError> {
        let ctx = self.player_state.get_context(player_id).await;

        let ctx = match ctx {
            Some(c) => c,
            None => {
                return Err(crate::error::MatchError::NotMatching(player_id.to_string()));
            }
        };

        if ctx.match_id != match_id {
            return Err(crate::error::MatchError::MatchNotFound(match_id.to_string()));
        }

        // 获取匹配任务查看剩余玩家
        let task = self.pool.get_match_task(match_id).await;
        let remaining_players = task.as_ref().map(|t| t.players.len()).unwrap_or(0);

        // 重置状态
        self.player_state.set_status(player_id, PlayerMatchStatus::Idle).await;
        self.player_state.clear_context(player_id).await;

        info!(
            player_id = %player_id,
            match_id = %match_id,
            mode = %ctx.mode,
            reason = %reason,
            remaining_players = remaining_players,
            "player left match"
        );

        // TODO: 检查是否所有人都离开了
        Ok(false)
    }

    /// 对局结束
    pub async fn match_end(
        &self,
        match_id: &str,
        room_id: &str,
        reason: &str,
    ) -> Result<(), crate::error::MatchError> {
        let task = self.pool.remove_match_task(match_id).await;

        if let Some(task) = task {
            let player_count = task.players.len();

            // 重置所有玩家状态
            for player_id in &task.players {
                self.player_state.set_status(player_id, PlayerMatchStatus::Idle).await;
                self.player_state.clear_context(player_id).await;
            }

            info!(
                match_id = %match_id,
                mode = %task.mode,
                room_id = %room_id,
                player_count = player_count,
                players = ?task.players,
                reason = %reason,
                "match ended"
            );
        } else {
            info!(
                match_id = %match_id,
                room_id = %room_id,
                reason = %reason,
                "match ended (task not found)"
            );
        }

        Ok(())
    }

    /// 尝试撮合某个模式
    pub async fn try_match_mode(&self, mode: &str) {
        let candidates = self.pool.try_match(mode).await;

        if let Some(candidates) = candidates {
            let player_ids: Vec<String> = candidates.iter().map(|c| c.player_id.clone()).collect();
            let match_id = candidates.first().map(|c| c.match_id.clone()).unwrap_or_default();

            info!(
                mode = %mode,
                match_id = %match_id,
                player_count = player_ids.len(),
                players = ?player_ids,
                "match formed, notifying game-server to create room"
            );

            // 创建匹配任务
            self.pool.create_match_task(match_id.clone(), mode.to_string(), player_ids.clone()).await;

            // TODO: 调用 game-server 创建房间
            // 目前先模拟房间创建
            let room_id = format!("room_{}", uuid::Uuid::new_v4());

            // 更新匹配任务的 room_id
            self.pool.update_match_room(&match_id, room_id.clone()).await;

            // 生成 token 并更新玩家上下文
            for player_id in &player_ids {
                let token = uuid::Uuid::new_v4().to_string();
                if let Some(ctx) = self.player_state.get_context(player_id).await {
                    self.player_state.set_context(
                        player_id,
                        crate::state::PlayerMatchContext {
                            match_id: ctx.match_id,
                            mode: ctx.mode,
                            room_id: Some(room_id.clone()),
                            token: Some(token.clone()),
                        },
                    ).await;
                }

                // 更新状态为 matched
                self.player_state.set_status(player_id, PlayerMatchStatus::Matched).await;

                // 发送匹配成功事件
                let event = MatchEvent {
                    event: "matched".to_string(),
                    match_id: match_id.clone(),
                    room_id: room_id.clone(),
                    token,
                    error_code: String::new(),
                };
                let _ = self.player_state.send_event(player_id, event).await;
            }

            info!(
                mode = %mode,
                match_id = %match_id,
                room_id = %room_id,
                players = ?player_ids,
                "match created successfully"
            );
        }
    }
}

pub type SharedSimpleMatcher = Arc<SimpleMatcher>;

pub fn new_simple_matcher(config: Config) -> SharedSimpleMatcher {
    Arc::new(SimpleMatcher::new(config))
}
