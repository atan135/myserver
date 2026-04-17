//! 简单撮合器

use std::sync::Arc;
use std::time::Duration;

use tracing::{error, info, warn};

use crate::config::Config;
use crate::error::MatchError;
use crate::game_server_client::GameServerClient;
use crate::proto::myserver::matchservice::{MatchEvent, MatchStatusRes};
use crate::pool::{MatchCandidate, SharedMatchPool, new_match_pool_with_modes};
use crate::state::{
    PlayerMatchContext, PlayerMatchStatus, SharedPlayerState, new_player_state_store,
};

/// 简单撮合器
pub struct SimpleMatcher {
    pool: SharedMatchPool,
    player_state: SharedPlayerState,
    game_server_client: GameServerClient,
    config: Config,
}

impl SimpleMatcher {
    pub fn new(config: Config) -> Self {
        let player_state = new_player_state_store();
        let pool = new_match_pool_with_modes(player_state.clone(), config.modes.clone());
        let game_server_client = GameServerClient::new(&config);

        Self {
            pool,
            player_state,
            game_server_client,
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
    pub async fn start_match(&self, player_id: String, mode: String) -> Result<String, MatchError> {
        let mode_config = self
            .config
            .get_mode(&mode)
            .ok_or_else(|| MatchError::InvalidMode(mode.clone()))?;

        let current_status = self.player_state.get_status(&player_id).await;
        if current_status != PlayerMatchStatus::Idle {
            return Err(MatchError::AlreadyMatching(player_id));
        }

        let match_id = uuid::Uuid::new_v4().to_string();
        let timeout_at = std::time::Instant::now() + Duration::from_secs(mode_config.match_timeout_secs);
        let candidate = MatchCandidate::new(
            player_id.clone(),
            match_id.clone(),
            mode.clone(),
            timeout_at,
        );

        self.pool.add_candidate(candidate).await;
        self.player_state
            .set_status(&player_id, PlayerMatchStatus::Matching)
            .await;
        self.player_state
            .set_context(
                &player_id,
                PlayerMatchContext {
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

        self.try_match_mode(&mode).await?;

        Ok(match_id)
    }

    /// 取消匹配
    pub async fn cancel_match(&self, player_id: &str, match_id: &str) -> Result<(), MatchError> {
        let ctx = self
            .player_state
            .get_context(player_id)
            .await
            .ok_or_else(|| MatchError::NotMatching(player_id.to_string()))?;

        if ctx.match_id != match_id {
            return Err(MatchError::MatchNotFound(match_id.to_string()));
        }

        let status = self.player_state.get_status(player_id).await;
        if status != PlayerMatchStatus::Matching {
            return Err(MatchError::NotMatching(player_id.to_string()));
        }

        self.pool.remove_candidate(player_id, &ctx.mode).await;
        self.player_state.set_status(player_id, PlayerMatchStatus::Idle).await;
        self.player_state.clear_context(player_id).await;

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
    pub async fn get_status(&self, player_id: &str) -> Result<MatchStatusRes, MatchError> {
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

        Ok(MatchStatusRes {
            status: status_str.to_string(),
            match_id,
            room_id,
            token,
            estimated_wait_secs: estimated_wait,
        })
    }

    /// GameServer 创建房间回调
    pub async fn create_room_and_join(
        &self,
        match_id: &str,
        room_id: &str,
        player_ids: &[String],
        mode: &str,
    ) -> Result<(), MatchError> {
        if self.pool.get_match_task(match_id).await.is_none() {
            warn!(
                match_id = %match_id,
                room_id = %room_id,
                players = ?player_ids,
                mode = %mode,
                "CreateRoomAndJoin received without existing match task, reconstructing task"
            );
            self.pool
                .create_match_task(match_id.to_string(), mode.to_string(), player_ids.to_vec())
                .await;
        }

        let task = self
            .pool
            .get_match_task(match_id)
            .await
            .ok_or_else(|| MatchError::MatchNotFound(match_id.to_string()))?;

        if task.room_id.as_deref() == Some(room_id) {
            info!(
                match_id = %match_id,
                room_id = %room_id,
                "CreateRoomAndJoin ignored because room_id already applied"
            );
            return Ok(());
        }

        if task.mode != mode {
            warn!(
                match_id = %match_id,
                expected_mode = %task.mode,
                actual_mode = %mode,
                "CreateRoomAndJoin mode mismatch"
            );
        }

        self.pool
            .update_match_room(match_id, room_id.to_string())
            .await;

        for player_id in player_ids {
            let token = uuid::Uuid::new_v4().to_string();
            self.update_player_to_matched(player_id, match_id, mode, room_id, Some(token))
                .await?;
        }

        info!(
            match_id = %match_id,
            room_id = %room_id,
            players = ?player_ids,
            mode = %mode,
            "CreateRoomAndJoin applied to match state"
        );

        Ok(())
    }

    /// 玩家进入房间回调
    pub async fn player_joined(
        &self,
        match_id: &str,
        player_id: &str,
        room_id: &str,
    ) -> Result<(), MatchError> {
        let ctx = self
            .player_state
            .get_context(player_id)
            .await
            .ok_or_else(|| MatchError::NotMatching(player_id.to_string()))?;

        if ctx.match_id != match_id {
            return Err(MatchError::MatchNotFound(match_id.to_string()));
        }

        let task = self
            .pool
            .mark_player_joined(match_id, player_id)
            .await
            .ok_or_else(|| MatchError::MatchNotFound(match_id.to_string()))?;

        self.player_state
            .set_status(player_id, PlayerMatchStatus::InRoom)
            .await;

        if let Some(existing_ctx) = self.player_state.get_context(player_id).await {
            self.player_state
                .set_context(
                    player_id,
                    PlayerMatchContext {
                        match_id: existing_ctx.match_id,
                        mode: existing_ctx.mode,
                        room_id: Some(room_id.to_string()),
                        token: existing_ctx.token,
                    },
                )
                .await;
        }

        info!(
            player_id = %player_id,
            match_id = %match_id,
            room_id = %room_id,
            joined_players = task.joined_players.len(),
            total_players = task.players.len(),
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
    ) -> Result<bool, MatchError> {
        let ctx = self
            .player_state
            .get_context(player_id)
            .await
            .ok_or_else(|| MatchError::NotMatching(player_id.to_string()))?;

        if ctx.match_id != match_id {
            return Err(MatchError::MatchNotFound(match_id.to_string()));
        }

        let task = self
            .pool
            .mark_player_left(match_id, player_id)
            .await
            .ok_or_else(|| MatchError::MatchNotFound(match_id.to_string()))?;

        self.player_state
            .set_status(player_id, PlayerMatchStatus::Matched)
            .await;

        let should_abort = task.active_players.is_empty() && reason != "disconnect";

        info!(
            player_id = %player_id,
            match_id = %match_id,
            mode = %ctx.mode,
            reason = %reason,
            active_players = task.active_players.len(),
            joined_players = task.joined_players.len(),
            match_should_abort = should_abort,
            "player left match"
        );

        Ok(should_abort)
    }

    /// 对局结束
    pub async fn match_end(
        &self,
        match_id: &str,
        room_id: &str,
        reason: &str,
    ) -> Result<(), MatchError> {
        let task = self.pool.remove_match_task(match_id).await;

        if let Some(task) = task {
            let player_count = task.players.len();

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
    pub async fn try_match_mode(&self, mode: &str) -> Result<(), MatchError> {
        while let Some(candidates) = self.pool.try_match(mode).await {
            let player_ids: Vec<String> = candidates.iter().map(|c| c.player_id.clone()).collect();
            let match_id = uuid::Uuid::new_v4().to_string();

            info!(
                mode = %mode,
                match_id = %match_id,
                player_count = player_ids.len(),
                players = ?player_ids,
                "match formed, creating room via game-server"
            );

            self.pool
                .create_match_task(match_id.clone(), mode.to_string(), player_ids.clone())
                .await;
            self.prepare_players_for_match(&player_ids, &match_id, mode).await?;

            let expected_room_id = format!("room_{}", uuid::Uuid::new_v4());
            let room_id = match self
                .game_server_client
                .create_matched_room(&match_id, &expected_room_id, &player_ids, mode)
                .await
            {
                Ok(room_id) => room_id,
                Err(error) => {
                    let callback_applied = self
                        .pool
                        .get_match_task(&match_id)
                        .await
                        .and_then(|task| task.room_id)
                        .is_some();
                    if callback_applied {
                        warn!(
                            mode = %mode,
                            match_id = %match_id,
                            players = ?player_ids,
                            error = %error,
                            "room creation response failed after callback already applied"
                        );
                        expected_room_id
                    } else {
                        error!(
                            mode = %mode,
                            match_id = %match_id,
                            players = ?player_ids,
                            error = %error,
                            "failed to create matched room"
                        );
                        self.pool.remove_match_task(&match_id).await;
                        self.fail_matched_players(&player_ids, &match_id, "ROOM_CREATE_FAILED")
                            .await;
                        continue;
                    }
                }
            };

            let callback_applied = self
                .pool
                .get_match_task(&match_id)
                .await
                .and_then(|task| task.room_id)
                .is_some();

            if !callback_applied {
                self.create_room_and_join(&match_id, &room_id, &player_ids, mode)
                    .await?;
            }

            info!(
                mode = %mode,
                match_id = %match_id,
                room_id = %room_id,
                players = ?player_ids,
                "match created successfully"
            );
        }

        Ok(())
    }

    /// 清理超时玩家
    pub async fn cleanup_timeout(&self) -> Result<(), MatchError> {
        let timed_out = self.pool.cleanup_timeout().await;

        for candidate in timed_out {
            self.player_state
                .set_status(&candidate.player_id, PlayerMatchStatus::Idle)
                .await;
            self.player_state.clear_context(&candidate.player_id).await;

            let event = MatchEvent {
                event: "match_failed".to_string(),
                match_id: candidate.match_id.clone(),
                room_id: String::new(),
                token: String::new(),
                error_code: "MATCH_TIMEOUT".to_string(),
            };
            let _ = self.player_state.send_event(&candidate.player_id, event).await;

            info!(
                player_id = %candidate.player_id,
                match_id = %candidate.match_id,
                mode = %candidate.mode,
                "match candidate timed out"
            );
        }

        Ok(())
    }

    async fn prepare_players_for_match(
        &self,
        player_ids: &[String],
        match_id: &str,
        mode: &str,
    ) -> Result<(), MatchError> {
        for player_id in player_ids {
            let ctx = self
                .player_state
                .get_context(player_id)
                .await
                .ok_or_else(|| MatchError::PlayerNotFound(player_id.clone()))?;

            self.player_state
                .set_context(
                    player_id,
                    PlayerMatchContext {
                        match_id: match_id.to_string(),
                        mode: ctx.mode,
                        room_id: None,
                        token: None,
                    },
                )
                .await;
            self.player_state
                .set_status(player_id, PlayerMatchStatus::Matched)
                .await;
        }

        info!(
            match_id = %match_id,
            mode = %mode,
            players = ?player_ids,
            "players promoted from matching queue into match task"
        );

        Ok(())
    }

    async fn update_player_to_matched(
        &self,
        player_id: &str,
        match_id: &str,
        mode: &str,
        room_id: &str,
        token_override: Option<String>,
    ) -> Result<(), MatchError> {
        let ctx = self.player_state.get_context(player_id).await;
        if let Some(existing_ctx) = ctx.as_ref() {
            if existing_ctx.match_id != match_id {
                warn!(
                    player_id = %player_id,
                    previous_match_id = %existing_ctx.match_id,
                    new_match_id = %match_id,
                    "player context match_id differed from room callback, overwriting with room callback"
                );
            }
        }

        let resolved_mode = ctx
            .as_ref()
            .map(|existing_ctx| existing_ctx.mode.clone())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| mode.to_string());
        let token = token_override.unwrap_or_else(|| {
            ctx.as_ref()
                .and_then(|existing_ctx| existing_ctx.token.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
        });

        self.player_state
            .set_context(
                player_id,
                PlayerMatchContext {
                    match_id: match_id.to_string(),
                    mode: resolved_mode,
                    room_id: Some(room_id.to_string()),
                    token: Some(token.clone()),
                },
            )
            .await;
        self.player_state
            .set_status(player_id, PlayerMatchStatus::Matched)
            .await;

        let event = MatchEvent {
            event: "matched".to_string(),
            match_id: match_id.to_string(),
            room_id: room_id.to_string(),
            token,
            error_code: String::new(),
        };
        let _ = self.player_state.send_event(player_id, event).await;

        Ok(())
    }

    async fn fail_matched_players(
        &self,
        player_ids: &[String],
        match_id: &str,
        error_code: &str,
    ) {
        for player_id in player_ids {
            self.player_state
                .set_status(player_id, PlayerMatchStatus::Idle)
                .await;
            self.player_state.clear_context(player_id).await;

            let event = MatchEvent {
                event: "match_failed".to_string(),
                match_id: match_id.to_string(),
                room_id: String::new(),
                token: String::new(),
                error_code: error_code.to_string(),
            };
            let _ = self.player_state.send_event(player_id, event).await;
        }
    }
}

pub type SharedSimpleMatcher = Arc<SimpleMatcher>;

pub fn new_simple_matcher(config: Config) -> SharedSimpleMatcher {
    Arc::new(SimpleMatcher::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{PlayerMatchContext, PlayerMatchStatus};

    async fn seed_in_room_match(
        matcher: &SimpleMatcher,
        match_id: &str,
        room_id: &str,
        players: &[&str],
        mode: &str,
    ) {
        matcher
            .pool()
            .create_match_task(
                match_id.to_string(),
                mode.to_string(),
                players.iter().map(|player| (*player).to_string()).collect(),
            )
            .await;

        for player_id in players {
            matcher
                .player_state()
                .set_context(
                    player_id,
                    PlayerMatchContext {
                        match_id: match_id.to_string(),
                        mode: mode.to_string(),
                        room_id: Some(room_id.to_string()),
                        token: Some(format!("token-{player_id}")),
                    },
                )
                .await;
            matcher
                .player_state()
                .set_status(player_id, PlayerMatchStatus::InRoom)
                .await;
        }
    }

    #[tokio::test]
    async fn disconnecting_last_active_player_does_not_abort_match() {
        let matcher = SimpleMatcher::new(Config::from_env());
        seed_in_room_match(&matcher, "match-1", "room-1", &["player-a", "player-b"], "1v1").await;

        let left_a = matcher
            .player_left("match-1", "player-a", "disconnect")
            .await
            .unwrap();
        let left_b = matcher
            .player_left("match-1", "player-b", "disconnect")
            .await
            .unwrap();

        assert!(!left_a);
        assert!(!left_b);

        let task = matcher
            .pool()
            .get_match_task("match-1")
            .await
            .expect("match task should still exist");
        assert!(task.active_players.is_empty());
    }

    #[tokio::test]
    async fn normal_leave_still_aborts_when_last_active_player_leaves() {
        let matcher = SimpleMatcher::new(Config::from_env());
        seed_in_room_match(&matcher, "match-2", "room-2", &["player-a", "player-b"], "1v1").await;

        let left_a = matcher
            .player_left("match-2", "player-a", "normal")
            .await
            .unwrap();
        let left_b = matcher
            .player_left("match-2", "player-b", "normal")
            .await
            .unwrap();

        assert!(!left_a);
        assert!(left_b);
    }
}
