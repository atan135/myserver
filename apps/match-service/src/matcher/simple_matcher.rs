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
    CharacterMatchContext, CharacterMatchStatus, SharedCharacterState,
    new_character_state_store_with_runtime_store,
};

/// 简单撮合器
pub struct SimpleMatcher {
    pool: SharedMatchPool,
    character_state: SharedCharacterState,
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
        let character_state = new_character_state_store_with_runtime_store(runtime_store.clone());
        let pool = new_match_pool_with_modes_and_runtime_store(
            character_state.clone(),
            config.modes.clone(),
            runtime_store.clone(),
        );
        let game_server_client = GameServerClient::new(&config);

        Self {
            pool,
            character_state,
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

    pub fn character_state(&self) -> &SharedCharacterState {
        &self.character_state
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

        self.character_state.apply_snapshot(&snapshot).await;
        self.pool.apply_snapshot(&snapshot).await;

        info!(
            candidate_modes = snapshot.candidates_by_mode.len(),
            match_count = snapshot.matches.len(),
            character_context_count = snapshot.character_context.len(),
            latest_event_count = snapshot.latest_events.len(),
            "match runtime state recovered"
        );

        for task in snapshot.matches.values() {
            if let Some(room_id) = task.room_id.as_deref() {
                self.ensure_room_assignment(
                    &task.match_id,
                    &task.mode,
                    room_id,
                    &task.character_ids,
                )
                .await?;
            } else {
                self.resume_pending_room_create(&task.match_id, &task.mode, &task.character_ids)
                    .await?;
            }
        }

        for mode in self.config.modes.keys() {
            self.try_match_mode(mode).await?;
        }

        Ok(())
    }

    /// 开始匹配
    pub async fn start_match(
        &self,
        character_id: String,
        mode: String,
    ) -> Result<String, MatchError> {
        let mode_config = self
            .config
            .get_mode(&mode)
            .ok_or_else(|| MatchError::InvalidMode(mode.clone()))?;

        let current_status = self.character_state.get_status(&character_id).await;
        if current_status != CharacterMatchStatus::Idle {
            return Err(MatchError::AlreadyMatching(character_id));
        }

        let match_id = uuid::Uuid::new_v4().to_string();
        let timeout_at =
            std::time::Instant::now() + Duration::from_secs(mode_config.match_timeout_secs);
        let candidate = MatchCandidate::new(
            character_id.clone(),
            match_id.clone(),
            mode.clone(),
            timeout_at,
        );

        self.pool.add_candidate(candidate).await;
        self.character_state
            .set_status(&character_id, CharacterMatchStatus::Matching)
            .await;
        self.character_state
            .set_context(
                &character_id,
                CharacterMatchContext {
                    match_id: match_id.clone(),
                    mode: mode.clone(),
                    room_id: None,
                    token: None,
                },
            )
            .await;

        info!(
            character_id = %character_id,
            mode = %mode,
            match_id = %match_id,
            timeout_secs = mode_config.match_timeout_secs,
            required_characters = mode_config.total_size,
            "character started matching"
        );

        self.try_match_mode(&mode).await?;

        Ok(match_id)
    }

    /// 取消匹配
    pub async fn cancel_match(&self, character_id: &str, match_id: &str) -> Result<(), MatchError> {
        let ctx = self
            .character_state
            .get_context(character_id)
            .await
            .ok_or_else(|| MatchError::NotMatching(character_id.to_string()))?;

        if ctx.match_id != match_id {
            return Err(MatchError::MatchNotFound(match_id.to_string()));
        }

        let status = self.character_state.get_status(character_id).await;
        if status != CharacterMatchStatus::Matching {
            return Err(MatchError::NotMatching(character_id.to_string()));
        }

        self.pool.remove_candidate(character_id, &ctx.mode).await;
        self.character_state
            .set_status(character_id, CharacterMatchStatus::Idle)
            .await;
        self.character_state.clear_context(character_id).await;

        let event = MatchEvent {
            event: "match_cancelled".to_string(),
            match_id: match_id.to_string(),
            room_id: String::new(),
            token: String::new(),
            error_code: String::new(),
        };
        let _ = self.character_state.send_event(character_id, event).await;

        info!(
            character_id = %character_id,
            match_id = %match_id,
            mode = %ctx.mode,
            "match cancelled"
        );

        Ok(())
    }

    /// 查询匹配状态
    pub async fn get_status(&self, character_id: &str) -> Result<MatchStatusRes, MatchError> {
        let ctx = self.character_state.get_context(character_id).await;
        let status = self.character_state.get_status(character_id).await;

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
            CharacterMatchStatus::Idle => "idle",
            CharacterMatchStatus::Matching => "matching",
            CharacterMatchStatus::Matched => "matched",
            CharacterMatchStatus::InRoom => "in_room",
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
        character_ids: &[String],
        mode: &str,
    ) -> Result<(), MatchError> {
        if self.pool.get_match_task(match_id).await.is_none() {
            warn!(
                match_id = %match_id,
                room_id = %room_id,
                characters = ?character_ids,
                mode = %mode,
                "CreateRoomAndJoin received without existing match task, reconstructing task"
            );
            self.pool
                .create_match_task(
                    match_id.to_string(),
                    mode.to_string(),
                    character_ids.to_vec(),
                )
                .await;
        }

        let task = self
            .pool
            .get_match_task(match_id)
            .await
            .ok_or_else(|| MatchError::MatchNotFound(match_id.to_string()))?;

        if task.room_id.as_deref() == Some(room_id) {
            self.ensure_room_assignment(match_id, mode, room_id, character_ids)
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

        for character_id in character_ids {
            let token = uuid::Uuid::new_v4().to_string();
            self.update_character_to_matched(character_id, match_id, mode, room_id, Some(token))
                .await?;
        }

        info!(
            match_id = %match_id,
            room_id = %room_id,
            characters = ?character_ids,
            mode = %mode,
            "CreateRoomAndJoin applied to match state"
        );

        Ok(())
    }

    /// 角色进入房间回调
    pub async fn player_joined(
        &self,
        match_id: &str,
        character_id: &str,
        room_id: &str,
    ) -> Result<(), MatchError> {
        let ctx = self
            .character_state
            .get_context(character_id)
            .await
            .ok_or_else(|| MatchError::NotMatching(character_id.to_string()))?;

        if ctx.match_id != match_id {
            return Err(MatchError::MatchNotFound(match_id.to_string()));
        }

        let task = self
            .pool
            .mark_character_joined(match_id, character_id)
            .await
            .ok_or_else(|| MatchError::MatchNotFound(match_id.to_string()))?;

        self.character_state
            .set_status(character_id, CharacterMatchStatus::InRoom)
            .await;

        if let Some(existing_ctx) = self.character_state.get_context(character_id).await {
            self.character_state
                .set_context(
                    character_id,
                    CharacterMatchContext {
                        match_id: existing_ctx.match_id,
                        mode: existing_ctx.mode,
                        room_id: Some(room_id.to_string()),
                        token: existing_ctx.token,
                    },
                )
                .await;
        }

        info!(
            character_id = %character_id,
            match_id = %match_id,
            room_id = %room_id,
            joined_characters = task.joined_characters.len(),
            total_characters = task.character_ids.len(),
            "character joined room"
        );

        Ok(())
    }

    /// 角色离开回调
    pub async fn player_left(
        &self,
        match_id: &str,
        character_id: &str,
        reason: &str,
    ) -> Result<bool, MatchError> {
        let ctx = self
            .character_state
            .get_context(character_id)
            .await
            .ok_or_else(|| MatchError::NotMatching(character_id.to_string()))?;

        if ctx.match_id != match_id {
            return Err(MatchError::MatchNotFound(match_id.to_string()));
        }

        let task = self
            .pool
            .mark_character_left(match_id, character_id)
            .await
            .ok_or_else(|| MatchError::MatchNotFound(match_id.to_string()))?;

        self.character_state
            .set_status(character_id, CharacterMatchStatus::Matched)
            .await;

        let should_abort = task.active_characters.is_empty() && reason != "disconnect";

        info!(
            character_id = %character_id,
            match_id = %match_id,
            mode = %ctx.mode,
            reason = %reason,
            active_characters = task.active_characters.len(),
            joined_characters = task.joined_characters.len(),
            match_should_abort = should_abort,
            "character left match"
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
            let character_count = task.character_ids.len();

            for character_id in &task.character_ids {
                self.character_state
                    .set_status(character_id, CharacterMatchStatus::Idle)
                    .await;
                self.character_state.clear_context(character_id).await;
            }

            info!(
                match_id = %match_id,
                mode = %task.mode,
                room_id = %room_id,
                character_count = character_count,
                characters = ?task.character_ids,
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
                let character_ids: Vec<String> = candidates
                    .iter()
                    .map(|candidate| candidate.character_id.clone())
                    .collect();
                let match_id = uuid::Uuid::new_v4().to_string();

                info!(
                    mode = %mode,
                    match_id = %match_id,
                    character_count = character_ids.len(),
                    characters = ?character_ids,
                    "match formed, creating room via game-server"
                );

                self.pool
                    .create_match_task(match_id.clone(), mode.to_string(), character_ids.clone())
                    .await;
                self.prepare_characters_for_match(&character_ids, &match_id, mode)
                    .await?;
                self.resume_pending_room_create(&match_id, mode, &character_ids)
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
        character_ids: &[String],
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
            .create_matched_room(match_id, &expected_room_id, character_ids, mode)
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
                        characters = ?character_ids,
                        error = %error,
                        "room creation response failed after callback already applied"
                    );
                    expected_room_id
                } else {
                    error!(
                        mode = %mode,
                        match_id = %match_id,
                        characters = ?character_ids,
                        error = %error,
                        "failed to create matched room"
                    );
                    self.pool.remove_match_task(match_id).await;
                    self.fail_matched_characters(character_ids, match_id, "ROOM_CREATE_FAILED")
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
            self.create_room_and_join(match_id, &room_id, character_ids, mode)
                .await?;
        }

        info!(
            mode = %mode,
            match_id = %match_id,
            room_id = %room_id,
            characters = ?character_ids,
            "match created successfully"
        );

        Ok(())
    }

    /// 清理超时角色
    pub async fn cleanup_timeout(&self) -> Result<(), MatchError> {
        let timed_out = self.pool.cleanup_timeout().await;

        for candidate in timed_out {
            self.character_state
                .set_status(&candidate.character_id, CharacterMatchStatus::Idle)
                .await;
            self.character_state
                .clear_context(&candidate.character_id)
                .await;

            let event = MatchEvent {
                event: "match_failed".to_string(),
                match_id: candidate.match_id.clone(),
                room_id: String::new(),
                token: String::new(),
                error_code: "MATCH_TIMEOUT".to_string(),
            };
            let _ = self
                .character_state
                .send_event(&candidate.character_id, event)
                .await;

            info!(
                character_id = %candidate.character_id,
                match_id = %candidate.match_id,
                mode = %candidate.mode,
                "match candidate timed out"
            );
        }

        Ok(())
    }

    async fn prepare_characters_for_match(
        &self,
        character_ids: &[String],
        match_id: &str,
        mode: &str,
    ) -> Result<(), MatchError> {
        for character_id in character_ids {
            let ctx = self
                .character_state
                .get_context(character_id)
                .await
                .ok_or_else(|| MatchError::CharacterNotFound(character_id.clone()))?;

            self.character_state
                .set_context(
                    character_id,
                    CharacterMatchContext {
                        match_id: match_id.to_string(),
                        mode: ctx.mode,
                        room_id: None,
                        token: None,
                    },
                )
                .await;
            self.character_state
                .set_status(character_id, CharacterMatchStatus::Matched)
                .await;
        }

        info!(
            match_id = %match_id,
            mode = %mode,
            characters = ?character_ids,
            "characters promoted from matching queue into match task"
        );

        Ok(())
    }

    async fn update_character_to_matched(
        &self,
        character_id: &str,
        match_id: &str,
        mode: &str,
        room_id: &str,
        token_override: Option<String>,
    ) -> Result<(), MatchError> {
        let ctx = self.character_state.get_context(character_id).await;
        if let Some(existing_ctx) = ctx.as_ref() {
            if existing_ctx.match_id != match_id {
                warn!(
                    character_id = %character_id,
                    previous_match_id = %existing_ctx.match_id,
                    new_match_id = %match_id,
                    "character context match_id differed from room callback, overwriting with room callback"
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

        self.character_state
            .set_context(
                character_id,
                CharacterMatchContext {
                    match_id: match_id.to_string(),
                    mode: resolved_mode,
                    room_id: Some(room_id.to_string()),
                    token: Some(token.clone()),
                },
            )
            .await;
        self.character_state
            .set_status(character_id, CharacterMatchStatus::Matched)
            .await;

        let event = MatchEvent {
            event: "matched".to_string(),
            match_id: match_id.to_string(),
            room_id: room_id.to_string(),
            token,
            error_code: String::new(),
        };
        let _ = self.character_state.send_event(character_id, event).await;

        Ok(())
    }

    async fn ensure_room_assignment(
        &self,
        match_id: &str,
        mode: &str,
        room_id: &str,
        character_ids: &[String],
    ) -> Result<(), MatchError> {
        for character_id in character_ids {
            let existing_ctx = self.character_state.get_context(character_id).await;
            if let Some(ctx) = existing_ctx.as_ref() {
                if ctx.match_id != match_id {
                    warn!(
                        character_id = %character_id,
                        previous_match_id = %ctx.match_id,
                        repaired_match_id = %match_id,
                        "character context match_id differed while repairing recovered room assignment"
                    );
                }
            }

            let existing_event = self.character_state.latest_event(character_id).await;
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
                self.character_state
                    .set_context(
                        character_id,
                        CharacterMatchContext {
                            match_id: match_id.to_string(),
                            mode: resolved_mode.clone(),
                            room_id: Some(room_id.to_string()),
                            token: Some(token.clone()),
                        },
                    )
                    .await;
            }

            let status = self.character_state.get_status(character_id).await;
            let status_needs_repair = !matches!(
                status,
                CharacterMatchStatus::Matched | CharacterMatchStatus::InRoom
            );
            if status_needs_repair {
                self.character_state
                    .set_status(character_id, CharacterMatchStatus::Matched)
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
                let _ = self.character_state.send_event(character_id, event).await;
            }

            if context_needs_repair || status_needs_repair || event_needs_repair {
                info!(
                    character_id = %character_id,
                    match_id = %match_id,
                    room_id = %room_id,
                    repaired_context = context_needs_repair,
                    repaired_status = status_needs_repair,
                    repaired_event = event_needs_repair,
                    "repaired recovered room assignment for character"
                );
            }
        }

        Ok(())
    }

    fn match_lease_ttl(&self) -> Duration {
        Duration::from_secs(self.config.match_runtime_lease_ttl_secs.max(1))
    }

    async fn fail_matched_characters(
        &self,
        character_ids: &[String],
        match_id: &str,
        error_code: &str,
    ) {
        for character_id in character_ids {
            self.character_state
                .set_status(character_id, CharacterMatchStatus::Idle)
                .await;
            self.character_state.clear_context(character_id).await;

            let event = MatchEvent {
                event: "match_failed".to_string(),
                match_id: match_id.to_string(),
                room_id: String::new(),
                token: String::new(),
                error_code: error_code.to_string(),
            };
            let _ = self.character_state.send_event(character_id, event).await;
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
    use crate::config::ModeConfig;
    use crate::runtime_store::{
        StoredMatchCandidate, StoredMatchTask, new_memory_match_runtime_store,
    };
    use crate::state::{CharacterMatchContext, CharacterMatchStatus};

    fn test_config() -> Config {
        let mut modes = std::collections::HashMap::new();
        modes.insert(
            "1v1".to_string(),
            ModeConfig {
                team_size: 1,
                total_size: 2,
                match_timeout_secs: 30,
            },
        );

        Config {
            bind_addr: "0.0.0.0:9002".to_string(),
            public_host: "127.0.0.1".to_string(),
            port: 9002,
            match_timeout_secs: 30,
            max_concurrent_matches: 1000,
            modes,
            match_cleanup_interval_secs: 1,
            game_server_service_name: "game-server".to_string(),
            game_server_internal_socket_name: "fallback.sock".to_string(),
            local_discovery_fallback_enabled: true,
            game_server_discovery_cache_ttl_secs: 1,
            game_server_target_zone: String::new(),
            game_internal_token: "dev-only-change-this-game-internal-token".to_string(),
            log_level: "info".to_string(),
            log_enable_console: true,
            log_enable_file: false,
            log_dir: "logs".to_string(),
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: String::new(),
            global_id_origin_id: 0,
            global_id_worker_id: None,
            nats_url: "nats://127.0.0.1:4222".to_string(),
            registry_enabled: false,
            discovery_required: false,
            registry_url: "redis://127.0.0.1:1".to_string(),
            registry_key_prefix: String::new(),
            registry_heartbeat_interval_secs: 10,
            service_name: "match-service".to_string(),
            service_instance_id: "match-service-test".to_string(),
            service_zone: "local".to_string(),
            service_build_version: "dev".to_string(),
            match_runtime_store: "memory".to_string(),
            match_runtime_key_prefix: "myserver:".to_string(),
            match_runtime_lease_ttl_secs: 10,
            match_recovery_enabled: true,
            legacy_direct_config_warnings: Vec::new(),
        }
    }

    async fn seed_in_room_match(
        matcher: &SimpleMatcher,
        match_id: &str,
        room_id: &str,
        characters: &[&str],
        mode: &str,
    ) {
        matcher
            .pool()
            .create_match_task(
                match_id.to_string(),
                mode.to_string(),
                characters
                    .iter()
                    .map(|character| (*character).to_string())
                    .collect(),
            )
            .await;

        for character_id in characters {
            matcher
                .character_state()
                .set_context(
                    character_id,
                    CharacterMatchContext {
                        match_id: match_id.to_string(),
                        mode: mode.to_string(),
                        room_id: Some(room_id.to_string()),
                        token: Some(format!("token-{character_id}")),
                    },
                )
                .await;
            matcher
                .character_state()
                .set_status(character_id, CharacterMatchStatus::InRoom)
                .await;
        }
    }

    #[tokio::test]
    async fn disconnecting_last_active_character_does_not_abort_match() {
        let matcher = SimpleMatcher::new(test_config());
        seed_in_room_match(
            &matcher,
            "match-1",
            "room-1",
            &["character-a", "character-b"],
            "1v1",
        )
        .await;

        let left_a = matcher
            .player_left("match-1", "character-a", "disconnect")
            .await
            .unwrap();
        let left_b = matcher
            .player_left("match-1", "character-b", "disconnect")
            .await
            .unwrap();

        assert!(!left_a);
        assert!(!left_b);

        let task = matcher
            .pool()
            .get_match_task("match-1")
            .await
            .expect("match task should still exist");
        assert!(task.active_characters.is_empty());
    }

    #[tokio::test]
    async fn normal_leave_still_aborts_when_last_active_character_leaves() {
        let matcher = SimpleMatcher::new(test_config());
        seed_in_room_match(
            &matcher,
            "match-2",
            "room-2",
            &["character-a", "character-b"],
            "1v1",
        )
        .await;

        let left_a = matcher
            .player_left("match-2", "character-a", "normal")
            .await
            .unwrap();
        let left_b = matcher
            .player_left("match-2", "character-b", "normal")
            .await
            .unwrap();

        assert!(!left_a);
        assert!(left_b);
    }

    #[tokio::test]
    async fn different_character_ids_are_distinct_match_participants() {
        let matcher =
            SimpleMatcher::new_for_test(test_config(), new_memory_match_runtime_store(), false);

        let match_a = matcher
            .start_match("account-1:character-a".to_string(), "1v1".to_string())
            .await
            .unwrap();
        let match_b = matcher
            .start_match("account-1:character-b".to_string(), "1v1".to_string())
            .await
            .unwrap();

        assert_ne!(match_a, match_b);
        assert_eq!(
            matcher
                .character_state()
                .get_status("account-1:character-a")
                .await,
            CharacterMatchStatus::Matched
        );
        assert_eq!(
            matcher
                .character_state()
                .get_status("account-1:character-b")
                .await,
            CharacterMatchStatus::Matched
        );

        let ctx_a = matcher
            .character_state()
            .get_context("account-1:character-a")
            .await
            .expect("character-a should keep its own match context");
        let ctx_b = matcher
            .character_state()
            .get_context("account-1:character-b")
            .await
            .expect("character-b should keep its own match context");
        assert_eq!(ctx_a.match_id, ctx_b.match_id);

        let task = matcher
            .pool()
            .get_match_task(&ctx_a.match_id)
            .await
            .expect("match task should be created for both characters");
        assert_eq!(
            task.character_ids,
            vec![
                "account-1:character-a".to_string(),
                "account-1:character-b".to_string(),
            ]
        );
        assert_eq!(task.active_characters.len(), 2);
    }

    #[tokio::test]
    async fn recovery_restores_matching_candidate_state() {
        let store = new_memory_match_runtime_store();
        let config = test_config();
        let matcher = SimpleMatcher::new_for_test(config, store.clone(), false);
        let timeout_at = std::time::Instant::now() + Duration::from_secs(30);
        let candidate = MatchCandidate::new(
            "character-a".to_string(),
            "match-a".to_string(),
            "1v1".to_string(),
            timeout_at,
        );
        store
            .save_candidate(StoredMatchCandidate::from_candidate(&candidate))
            .await
            .unwrap();
        store
            .set_character_status("character-a", CharacterMatchStatus::Matching.into())
            .await
            .unwrap();
        store
            .set_character_context(
                "character-a",
                CharacterMatchContext {
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
            matcher.character_state().get_status("character-a").await,
            CharacterMatchStatus::Matching
        );
        assert_eq!(matcher.pool().candidate_count("1v1").await, 1);
    }

    #[tokio::test]
    async fn recovery_restores_pending_match_task() {
        let store = new_memory_match_runtime_store();
        let config = test_config();
        let matcher = SimpleMatcher::new_for_test(config, store.clone(), false);
        let task = crate::pool::MatchTask::new(
            "match-pending".to_string(),
            "1v1".to_string(),
            vec!["character-a".to_string(), "character-b".to_string()],
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
        assert_eq!(recovered.character_ids, task.character_ids);
        assert!(recovered.room_id.is_none());
    }

    #[tokio::test]
    async fn recovery_repairs_room_task_character_assignment_state() {
        let store = new_memory_match_runtime_store();
        let config = test_config();
        let matcher = SimpleMatcher::new_for_test(config, store.clone(), false);
        let mut task = crate::pool::MatchTask::new(
            "match-roomed".to_string(),
            "1v1".to_string(),
            vec!["character-a".to_string(), "character-b".to_string()],
        );
        task.room_id = Some("room-recovered".to_string());
        store
            .save_match_task(StoredMatchTask::from_task(&task))
            .await
            .unwrap();
        store
            .set_character_status("character-a", CharacterMatchStatus::Matched.into())
            .await
            .unwrap();
        store
            .set_character_context(
                "character-a",
                CharacterMatchContext {
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
            .character_state()
            .get_context("character-a")
            .await
            .expect("existing context should remain available");
        assert_eq!(ctx_a.room_id.as_deref(), Some("room-recovered"));
        assert_eq!(ctx_a.token.as_deref(), Some("token-existing"));
        let event_a = matcher
            .character_state()
            .latest_event("character-a")
            .await
            .expect("existing character should get recovered latest event");
        assert_eq!(event_a.event, "matched");
        assert_eq!(event_a.token, "token-existing");

        let ctx_b = matcher
            .character_state()
            .get_context("character-b")
            .await
            .expect("missing character context should be repaired");
        assert_eq!(ctx_b.match_id, "match-roomed");
        assert_eq!(ctx_b.room_id.as_deref(), Some("room-recovered"));
        assert!(ctx_b.token.as_ref().is_some_and(|token| !token.is_empty()));
        assert_eq!(
            matcher.character_state().get_status("character-b").await,
            CharacterMatchStatus::Matched
        );
        let event_b = matcher
            .character_state()
            .latest_event("character-b")
            .await
            .expect("missing character should get recovered latest event");
        assert_eq!(event_b.event, "matched");
        assert_eq!(event_b.match_id, "match-roomed");
        assert_eq!(event_b.room_id, "room-recovered");
        assert_eq!(event_b.token, ctx_b.token.unwrap());
    }

    #[tokio::test]
    async fn mode_lease_blocks_candidate_consumption_by_other_instance() {
        let store = new_memory_match_runtime_store();
        let mut config = test_config();
        config.service_instance_id = "instance-b".to_string();
        let matcher = SimpleMatcher::new_for_test(config, store.clone(), false);

        store
            .acquire_lease("match-mode:1v1", "instance-a", Duration::from_secs(30))
            .await
            .unwrap();

        for character_id in ["character-a", "character-b"] {
            let match_id = format!("match-{character_id}");
            let candidate = MatchCandidate::new(
                character_id.to_string(),
                match_id.clone(),
                "1v1".to_string(),
                std::time::Instant::now() + Duration::from_secs(30),
            );
            matcher.pool().add_candidate(candidate).await;
            matcher
                .character_state()
                .set_context(
                    character_id,
                    CharacterMatchContext {
                        match_id,
                        mode: "1v1".to_string(),
                        room_id: None,
                        token: None,
                    },
                )
                .await;
            matcher
                .character_state()
                .set_status(character_id, CharacterMatchStatus::Matching)
                .await;
        }

        matcher.try_match_mode("1v1").await.unwrap();

        assert_eq!(matcher.pool().candidate_count("1v1").await, 2);
        assert!(
            matcher
                .pool()
                .get_match_task("match-character-a")
                .await
                .is_none()
        );
    }
}
