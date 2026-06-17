//! 简单撮合器

use std::sync::Arc;
use std::time::Duration;

use global_id::GlobalIdGenerator;
use tokio::sync::oneshot;
use tracing::{error, info, warn};

use crate::config::Config;
use crate::error::MatchError;
use crate::game_server_client::GameServerClient;
use crate::pool::{MatchCandidate, SharedMatchPool, new_match_pool_with_modes_and_runtime_store};
use crate::proto::myserver::matchservice::{MatchEvent, MatchStatusRes};
use crate::runtime_store::{
    LeaseAcquireResult, SharedMatchRuntimeStore, new_memory_match_runtime_store,
};
use crate::state::{
    PlayerMatchContext, PlayerMatchStatus, SharedPlayerState,
    new_player_state_store_with_runtime_store,
};

/// 简单撮合器
pub struct SimpleMatcher {
    pool: SharedMatchPool,
    player_state: SharedPlayerState,
    game_server_client: GameServerClient,
    config: Config,
    runtime_store: SharedMatchRuntimeStore,
    auto_create_rooms: bool,
    room_id_generator: Arc<GlobalIdGenerator>,
}

impl SimpleMatcher {
    pub fn new(config: Config) -> Self {
        Self::with_runtime_store(config, new_memory_match_runtime_store())
    }

    pub fn with_runtime_store(config: Config, runtime_store: SharedMatchRuntimeStore) -> Self {
        Self::with_runtime_store_and_room_id_generator(
            config,
            runtime_store,
            Arc::new(GlobalIdGenerator::new(0, 0).expect("test room id generator config")),
        )
    }

    pub fn with_runtime_store_and_room_id_generator(
        config: Config,
        runtime_store: SharedMatchRuntimeStore,
        room_id_generator: Arc<GlobalIdGenerator>,
    ) -> Self {
        let player_state = new_player_state_store_with_runtime_store(runtime_store.clone());
        let pool = new_match_pool_with_modes_and_runtime_store(
            player_state.clone(),
            config.modes.clone(),
            runtime_store.clone(),
        );
        let game_server_client = GameServerClient::new(&config);

        Self {
            pool,
            player_state,
            game_server_client,
            config,
            runtime_store,
            auto_create_rooms: true,
            room_id_generator,
        }
    }

    #[cfg(test)]
    fn new_for_test(
        config: Config,
        runtime_store: SharedMatchRuntimeStore,
        auto_create_rooms: bool,
    ) -> Self {
        let mut matcher = Self::with_runtime_store(config, runtime_store);
        matcher.auto_create_rooms = auto_create_rooms;
        matcher
    }

    pub fn pool(&self) -> &SharedMatchPool {
        &self.pool
    }

    pub fn player_state(&self) -> &SharedPlayerState {
        &self.player_state
    }

    pub async fn recover_runtime_state(&self) -> Result<(), MatchError> {
        if !self.config.match_recovery_enabled {
            return Ok(());
        }

        let snapshot = self
            .runtime_store
            .load_snapshot()
            .await
            .map_err(|error| MatchError::Internal(error.to_string()))?;

        self.player_state.apply_snapshot(&snapshot).await;
        self.pool.apply_snapshot(&snapshot).await;

        info!(
            candidate_modes = snapshot.candidates_by_mode.len(),
            match_count = snapshot.matches.len(),
            player_context_count = snapshot.player_context.len(),
            latest_event_count = snapshot.latest_events.len(),
            "match runtime state recovered"
        );

        for task in snapshot.matches.values() {
            if let Some(room_id) = task.room_id.as_deref() {
                self.ensure_room_assignment(&task.match_id, &task.mode, room_id, &task.players)
                    .await?;
            } else {
                self.resume_pending_room_create(&task.match_id, &task.mode, &task.players)
                    .await?;
            }
        }

        for mode in self.config.modes.keys() {
            self.try_match_mode(mode).await?;
        }

        Ok(())
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
        let timeout_at =
            std::time::Instant::now() + Duration::from_secs(mode_config.match_timeout_secs);
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
        self.player_state
            .set_status(player_id, PlayerMatchStatus::Idle)
            .await;
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
            self.ensure_room_assignment(match_id, mode, room_id, player_ids)
                .await?;
            info!(
                match_id = %match_id,
                room_id = %room_id,
                "CreateRoomAndJoin reconciled because room_id was already applied"
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
                self.player_state
                    .set_status(player_id, PlayerMatchStatus::Idle)
                    .await;
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
        let lease_scope = format!("match-mode:{mode}");
        let lease_ttl = self.match_lease_ttl();
        match self
            .runtime_store
            .acquire_lease(&lease_scope, &self.config.service_instance_id, lease_ttl)
            .await
            .map_err(|error| MatchError::Internal(error.to_string()))?
        {
            LeaseAcquireResult::Acquired | LeaseAcquireResult::AlreadyOwned => {}
            LeaseAcquireResult::Busy { owner_instance_id } => {
                info!(
                    mode,
                    owner_instance_id,
                    "skip match attempt because mode lease is owned by another instance"
                );
                return Ok(());
            }
        }
        let lease_guard = MatchModeLeaseGuard::new(
            self.runtime_store.clone(),
            lease_scope.clone(),
            self.config.service_instance_id.clone(),
            lease_ttl,
        );

        let result = async {
            while let Some(candidates) = self.pool.try_match(mode).await {
                let player_ids: Vec<String> =
                    candidates.iter().map(|c| c.player_id.clone()).collect();
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
                self.prepare_players_for_match(&player_ids, &match_id, mode)
                    .await?;
                self.resume_pending_room_create(&match_id, mode, &player_ids)
                    .await?;
            }

            Ok(())
        }
        .await;
        lease_guard.release().await;
        result
    }

    async fn resume_pending_room_create(
        &self,
        match_id: &str,
        mode: &str,
        player_ids: &[String],
    ) -> Result<(), MatchError> {
        if !self.auto_create_rooms {
            return Ok(());
        }

        let expected_room_id = self
            .room_id_generator
            .generate_string("room")
            .map_err(|error| MatchError::Internal(error.to_string()))?;
        let room_id = match self
            .game_server_client
            .create_matched_room(match_id, &expected_room_id, player_ids, mode)
            .await
        {
            Ok(room_id) => room_id,
            Err(error) => {
                let callback_applied = self
                    .pool
                    .get_match_task(match_id)
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
                    self.pool.remove_match_task(match_id).await;
                    self.fail_matched_players(player_ids, match_id, "ROOM_CREATE_FAILED")
                        .await;
                    return Ok(());
                }
            }
        };

        let callback_applied = self
            .pool
            .get_match_task(match_id)
            .await
            .and_then(|task| task.room_id)
            .is_some();

        if !callback_applied {
            self.create_room_and_join(match_id, &room_id, player_ids, mode)
                .await?;
        }

        info!(
            mode = %mode,
            match_id = %match_id,
            room_id = %room_id,
            players = ?player_ids,
            "match created successfully"
        );

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
            let _ = self
                .player_state
                .send_event(&candidate.player_id, event)
                .await;

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

    async fn ensure_room_assignment(
        &self,
        match_id: &str,
        mode: &str,
        room_id: &str,
        player_ids: &[String],
    ) -> Result<(), MatchError> {
        for player_id in player_ids {
            let existing_ctx = self.player_state.get_context(player_id).await;
            if let Some(ctx) = existing_ctx.as_ref() {
                if ctx.match_id != match_id {
                    warn!(
                        player_id = %player_id,
                        previous_match_id = %ctx.match_id,
                        repaired_match_id = %match_id,
                        "player context match_id differed while repairing recovered room assignment"
                    );
                }
            }

            let existing_event = self.player_state.latest_event(player_id).await;
            let token = existing_ctx
                .as_ref()
                .filter(|ctx| {
                    ctx.match_id == match_id
                        && ctx.room_id.as_deref() == Some(room_id)
                        && ctx.token.as_ref().is_some_and(|value| !value.is_empty())
                })
                .and_then(|ctx| ctx.token.clone())
                .or_else(|| {
                    existing_event
                        .as_ref()
                        .filter(|event| {
                            event.event == "matched"
                                && event.match_id == match_id
                                && event.room_id == room_id
                                && !event.token.is_empty()
                        })
                        .map(|event| event.token.clone())
                })
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

            let resolved_mode = existing_ctx
                .as_ref()
                .filter(|ctx| ctx.match_id == match_id && !ctx.mode.is_empty())
                .map(|ctx| ctx.mode.clone())
                .unwrap_or_else(|| mode.to_string());

            let context_needs_repair = existing_ctx
                .as_ref()
                .map(|ctx| {
                    ctx.match_id != match_id
                        || ctx.mode != resolved_mode
                        || ctx.room_id.as_deref() != Some(room_id)
                        || ctx.token.as_deref() != Some(token.as_str())
                })
                .unwrap_or(true);
            if context_needs_repair {
                self.player_state
                    .set_context(
                        player_id,
                        PlayerMatchContext {
                            match_id: match_id.to_string(),
                            mode: resolved_mode.clone(),
                            room_id: Some(room_id.to_string()),
                            token: Some(token.clone()),
                        },
                    )
                    .await;
            }

            let status = self.player_state.get_status(player_id).await;
            let status_needs_repair = !matches!(
                status,
                PlayerMatchStatus::Matched | PlayerMatchStatus::InRoom
            );
            if status_needs_repair {
                self.player_state
                    .set_status(player_id, PlayerMatchStatus::Matched)
                    .await;
            }

            let event_needs_repair = existing_event
                .as_ref()
                .map(|event| {
                    event.event != "matched"
                        || event.match_id != match_id
                        || event.room_id != room_id
                        || event.token != token
                })
                .unwrap_or(true);
            if event_needs_repair {
                let event = MatchEvent {
                    event: "matched".to_string(),
                    match_id: match_id.to_string(),
                    room_id: room_id.to_string(),
                    token: token.clone(),
                    error_code: String::new(),
                };
                let _ = self.player_state.send_event(player_id, event).await;
            }

            if context_needs_repair || status_needs_repair || event_needs_repair {
                info!(
                    player_id = %player_id,
                    match_id = %match_id,
                    room_id = %room_id,
                    repaired_context = context_needs_repair,
                    repaired_status = status_needs_repair,
                    repaired_event = event_needs_repair,
                    "repaired recovered room assignment for player"
                );
            }
        }

        Ok(())
    }

    fn match_lease_ttl(&self) -> Duration {
        Duration::from_secs(self.config.match_runtime_lease_ttl_secs.max(1))
    }

    async fn fail_matched_players(&self, player_ids: &[String], match_id: &str, error_code: &str) {
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

struct MatchModeLeaseGuard {
    runtime_store: SharedMatchRuntimeStore,
    scope: String,
    owner_instance_id: String,
    stop_tx: Option<oneshot::Sender<()>>,
    renew_task: Option<tokio::task::JoinHandle<()>>,
}

impl MatchModeLeaseGuard {
    fn new(
        runtime_store: SharedMatchRuntimeStore,
        scope: String,
        owner_instance_id: String,
        ttl: Duration,
    ) -> Self {
        let (stop_tx, stop_rx) = oneshot::channel();
        let renew_task = tokio::spawn(Self::renew_loop(
            runtime_store.clone(),
            scope.clone(),
            owner_instance_id.clone(),
            ttl,
            stop_rx,
        ));
        Self {
            runtime_store,
            scope,
            owner_instance_id,
            stop_tx: Some(stop_tx),
            renew_task: Some(renew_task),
        }
    }

    async fn renew_loop(
        runtime_store: SharedMatchRuntimeStore,
        scope: String,
        owner_instance_id: String,
        ttl: Duration,
        mut stop_rx: oneshot::Receiver<()>,
    ) {
        let renew_interval = lease_renew_interval(ttl);
        let mut ticker = tokio::time::interval(renew_interval);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    match runtime_store.acquire_lease(&scope, &owner_instance_id, ttl).await {
                        Ok(LeaseAcquireResult::AlreadyOwned | LeaseAcquireResult::Acquired) => {}
                        Ok(LeaseAcquireResult::Busy { owner_instance_id: current_owner }) => {
                            warn!(
                                scope = %scope,
                                expected_owner = %owner_instance_id,
                                current_owner = %current_owner,
                                "match mode lease renew skipped because lease owner changed"
                            );
                        }
                        Err(error) => {
                            warn!(
                                scope = %scope,
                                owner_instance_id = %owner_instance_id,
                                error = %error,
                                "failed to renew match mode lease"
                            );
                        }
                    }
                }
                _ = &mut stop_rx => {
                    break;
                }
            }
        }
    }

    async fn release(mut self) {
        self.stop_and_wait().await;
        if let Err(error) = self
            .runtime_store
            .release_lease(&self.scope, &self.owner_instance_id)
            .await
        {
            warn!(
                scope = %self.scope,
                owner_instance_id = %self.owner_instance_id,
                error = %error,
                "failed to release match mode lease"
            );
        }
    }

    async fn stop_and_wait(&mut self) {
        self.stop();
        if let Some(renew_task) = self.renew_task.take() {
            if let Err(error) = renew_task.await {
                warn!(
                    scope = %self.scope,
                    owner_instance_id = %self.owner_instance_id,
                    error = %error,
                    "match mode lease renew task ended unexpectedly"
                );
            }
        }
    }

    fn stop(&mut self) {
        if let Some(stop_tx) = self.stop_tx.take() {
            let _ = stop_tx.send(());
        }
    }
}

impl Drop for MatchModeLeaseGuard {
    fn drop(&mut self) {
        self.stop();
    }
}

fn lease_renew_interval(ttl: Duration) -> Duration {
    let ttl_ms = ttl.as_millis().max(1);
    let renew_ms = (ttl_ms / 3).max(100).min(ttl_ms as u128) as u64;
    Duration::from_millis(renew_ms)
}

pub fn new_simple_matcher(
    config: Config,
    room_id_generator: Arc<GlobalIdGenerator>,
) -> SharedSimpleMatcher {
    Arc::new(SimpleMatcher::with_runtime_store_and_room_id_generator(
        config,
        new_memory_match_runtime_store(),
        room_id_generator,
    ))
}

pub fn new_simple_matcher_with_runtime_store(
    config: Config,
    runtime_store: SharedMatchRuntimeStore,
    room_id_generator: Arc<GlobalIdGenerator>,
) -> SharedSimpleMatcher {
    Arc::new(SimpleMatcher::with_runtime_store_and_room_id_generator(
        config,
        runtime_store,
        room_id_generator,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_store::{
        StoredMatchCandidate, StoredMatchTask, new_memory_match_runtime_store,
    };
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
        seed_in_room_match(
            &matcher,
            "match-1",
            "room-1",
            &["player-a", "player-b"],
            "1v1",
        )
        .await;

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
        seed_in_room_match(
            &matcher,
            "match-2",
            "room-2",
            &["player-a", "player-b"],
            "1v1",
        )
        .await;

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

    #[tokio::test]
    async fn recovery_restores_matching_candidate_state() {
        let store = new_memory_match_runtime_store();
        let config = Config::from_env();
        let matcher = SimpleMatcher::new_for_test(config, store.clone(), false);
        let timeout_at = std::time::Instant::now() + Duration::from_secs(30);
        let candidate = MatchCandidate::new(
            "player-a".to_string(),
            "match-a".to_string(),
            "1v1".to_string(),
            timeout_at,
        );
        store
            .save_candidate(StoredMatchCandidate::from_candidate(&candidate))
            .await
            .unwrap();
        store
            .set_player_status("player-a", PlayerMatchStatus::Matching.into())
            .await
            .unwrap();
        store
            .set_player_context(
                "player-a",
                PlayerMatchContext {
                    match_id: "match-a".to_string(),
                    mode: "1v1".to_string(),
                    room_id: None,
                    token: None,
                }
                .into(),
            )
            .await
            .unwrap();

        matcher.recover_runtime_state().await.unwrap();

        assert_eq!(
            matcher.player_state().get_status("player-a").await,
            PlayerMatchStatus::Matching
        );
        assert_eq!(matcher.pool().candidate_count("1v1").await, 1);
    }

    #[tokio::test]
    async fn recovery_restores_pending_match_task() {
        let store = new_memory_match_runtime_store();
        let config = Config::from_env();
        let matcher = SimpleMatcher::new_for_test(config, store.clone(), false);
        let task = crate::pool::MatchTask::new(
            "match-pending".to_string(),
            "1v1".to_string(),
            vec!["player-a".to_string(), "player-b".to_string()],
        );
        store
            .save_match_task(StoredMatchTask::from_task(&task))
            .await
            .unwrap();

        matcher.recover_runtime_state().await.unwrap();

        let recovered = matcher
            .pool()
            .get_match_task("match-pending")
            .await
            .expect("pending match task should be recovered");
        assert_eq!(recovered.players, task.players);
        assert!(recovered.room_id.is_none());
    }

    #[tokio::test]
    async fn recovery_repairs_room_task_player_assignment_state() {
        let store = new_memory_match_runtime_store();
        let config = Config::from_env();
        let matcher = SimpleMatcher::new_for_test(config, store.clone(), false);
        let mut task = crate::pool::MatchTask::new(
            "match-roomed".to_string(),
            "1v1".to_string(),
            vec!["player-a".to_string(), "player-b".to_string()],
        );
        task.room_id = Some("room-recovered".to_string());
        store
            .save_match_task(StoredMatchTask::from_task(&task))
            .await
            .unwrap();
        store
            .set_player_status("player-a", PlayerMatchStatus::Matched.into())
            .await
            .unwrap();
        store
            .set_player_context(
                "player-a",
                PlayerMatchContext {
                    match_id: "match-roomed".to_string(),
                    mode: "1v1".to_string(),
                    room_id: Some("room-recovered".to_string()),
                    token: Some("token-existing".to_string()),
                }
                .into(),
            )
            .await
            .unwrap();

        matcher.recover_runtime_state().await.unwrap();

        let ctx_a = matcher
            .player_state()
            .get_context("player-a")
            .await
            .expect("existing context should remain available");
        assert_eq!(ctx_a.room_id.as_deref(), Some("room-recovered"));
        assert_eq!(ctx_a.token.as_deref(), Some("token-existing"));
        let event_a = matcher
            .player_state()
            .latest_event("player-a")
            .await
            .expect("existing player should get recovered latest event");
        assert_eq!(event_a.event, "matched");
        assert_eq!(event_a.token, "token-existing");

        let ctx_b = matcher
            .player_state()
            .get_context("player-b")
            .await
            .expect("missing player context should be repaired");
        assert_eq!(ctx_b.match_id, "match-roomed");
        assert_eq!(ctx_b.room_id.as_deref(), Some("room-recovered"));
        assert!(ctx_b.token.as_ref().is_some_and(|token| !token.is_empty()));
        assert_eq!(
            matcher.player_state().get_status("player-b").await,
            PlayerMatchStatus::Matched
        );
        let event_b = matcher
            .player_state()
            .latest_event("player-b")
            .await
            .expect("missing player should get recovered latest event");
        assert_eq!(event_b.event, "matched");
        assert_eq!(event_b.match_id, "match-roomed");
        assert_eq!(event_b.room_id, "room-recovered");
        assert_eq!(event_b.token, ctx_b.token.unwrap());
    }

    #[tokio::test]
    async fn mode_lease_blocks_candidate_consumption_by_other_instance() {
        let store = new_memory_match_runtime_store();
        let mut config = Config::from_env();
        config.service_instance_id = "instance-b".to_string();
        let matcher = SimpleMatcher::new_for_test(config, store.clone(), false);

        store
            .acquire_lease("match-mode:1v1", "instance-a", Duration::from_secs(30))
            .await
            .unwrap();

        for player_id in ["player-a", "player-b"] {
            let match_id = format!("match-{player_id}");
            let candidate = MatchCandidate::new(
                player_id.to_string(),
                match_id.clone(),
                "1v1".to_string(),
                std::time::Instant::now() + Duration::from_secs(30),
            );
            matcher.pool().add_candidate(candidate).await;
            matcher
                .player_state()
                .set_context(
                    player_id,
                    PlayerMatchContext {
                        match_id,
                        mode: "1v1".to_string(),
                        room_id: None,
                        token: None,
                    },
                )
                .await;
            matcher
                .player_state()
                .set_status(player_id, PlayerMatchStatus::Matching)
                .await;
        }

        matcher.try_match_mode("1v1").await.unwrap();

        assert_eq!(matcher.pool().candidate_count("1v1").await, 2);
        assert!(
            matcher
                .pool()
                .get_match_task("match-player-a")
                .await
                .is_none()
        );
    }
}
