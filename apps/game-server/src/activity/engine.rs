use super::domain::ActivityStatus;
use super::repository::{
    ActivityRepository, InMemoryActivityRepository, PublishedActivitySnapshot,
};
use super::settlement::{ActivityClaimCoordinator, ClaimStatus, build_reward_order};
use super::types::{
    apply_game_entry, ActivityTypeRegistry, GameEntryEvent,
    InMemoryLoginRewardProgressRepository, LoginRewardConfig, LoginRewardProgressError,
    LoginRewardProgressRepository, LoginRewardProgressResult, PlayerContext, TransactionContext,
    eligible_stage_numbers, login_reward_claim_key,
};
use crate::core::inventory::{AssetBinding, NormalizedAssetItem, RewardDeliveryPolicy};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityEngineError {
    pub(crate) code: &'static str,
    pub(crate) message: String,
}

impl ActivityEngineError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityActionRequest {
    pub(crate) activity_id: String,
    pub(crate) version: u32,
    pub(crate) stage_id: String,
    pub(crate) action_type: String,
    pub(crate) client_request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityActionResponse {
    pub(crate) ok: bool,
    pub(crate) error_code: Option<&'static str>,
    pub(crate) activity_id: String,
    pub(crate) version: u32,
    pub(crate) stage_id: String,
    pub(crate) action_type: String,
    pub(crate) client_request_id: String,
    pub(crate) processing: bool,
    pub(crate) duplicate: bool,
    pub(crate) state_revision: u64,
}

#[derive(Clone)]
pub(crate) struct ActivityEngine {
    repository: Arc<InMemoryActivityRepository>,
    registry: Arc<ActivityTypeRegistry>,
    request_state: Arc<Mutex<RequestState>>,
    login_progress: Arc<dyn LoginRewardProgressRepository>,
    enabled: bool,
    claim_coordinator: Option<ActivityClaimCoordinator>,
}

#[derive(Default)]
struct RequestState {
    seen: HashMap<String, ActivityActionResponse>,
    rate_limits: HashMap<String, Instant>,
}

impl ActivityEngine {
    pub(crate) fn new(repository: Arc<InMemoryActivityRepository>) -> Self {
        Self {
            repository,
            registry: Arc::new(ActivityTypeRegistry::with_defaults()),
            request_state: Arc::new(Mutex::new(RequestState::default())),
            login_progress: Arc::new(InMemoryLoginRewardProgressRepository::default()),
            enabled: true,
            claim_coordinator: None,
        }
    }

    pub(crate) fn in_memory() -> Self {
        Self::new(Arc::new(InMemoryActivityRepository::default()))
    }

    pub(crate) fn disabled() -> Self {
        let mut engine = Self::in_memory();
        engine.enabled = false;
        engine
    }

    pub(crate) fn with_claim_coordinator(mut self, coordinator: ActivityClaimCoordinator) -> Self {
        self.claim_coordinator = Some(coordinator);
        self
    }

    pub(crate) fn with_login_reward_progress_repository(
        mut self,
        repository: Arc<dyn LoginRewardProgressRepository>,
    ) -> Self {
        self.login_progress = repository;
        self
    }

    /// Records a server-trusted game entry for a login-reward activity.
    ///
    /// The character identity and occurrence time come from the server entry
    /// context; no client-supplied period key is accepted here.
    pub(crate) async fn on_game_entry(
        &self,
        character_id: &str,
        activity_id: &str,
        version: u32,
        occurred_at: DateTime<Utc>,
    ) -> Result<LoginRewardProgressResult, ActivityEngineError> {
        if !self.enabled {
            return Err(Self::unavailable_error());
        }
        if character_id.trim().is_empty() {
            return Err(Self::auth_error());
        }
        if activity_id.trim().is_empty() || version == 0 {
            return Err(ActivityEngineError::new(
                "ACTIVITY_INVALID_REQUEST",
                "activity id and version are required",
            ));
        }

        let snapshot = self.load_detail(activity_id, occurred_at).await?;
        if snapshot.version.version_no != version as i32 {
            return Err(ActivityEngineError::new(
                "ACTIVITY_INVALID_VERSION",
                "requested activity version is not current",
            ));
        }

        // Evaluate lifecycle at the event time so a delayed event cannot be
        // accepted outside the activity's effective running window.
        Self::validate_read_status(&snapshot, occurred_at)?;
        if snapshot.activity.activity_type.as_str() != "login_reward" {
            return Err(ActivityEngineError::new(
                "ACTIVITY_INVALID_TYPE",
                "activity type does not support game entry progress",
            ));
        }
        self.registry
            .validate_config(snapshot.activity.activity_type.as_str(), &snapshot.version.type_config)
            .map_err(|error| ActivityEngineError::new("ACTIVITY_INVALID_CONFIG", error.message))?;
        let config: LoginRewardConfig = serde_json::from_value(snapshot.version.type_config.clone())
            .map_err(|error| ActivityEngineError::new("ACTIVITY_INVALID_CONFIG", error.to_string()))?;
        let event = GameEntryEvent {
            character_id: character_id.to_string(),
            activity_id: activity_id.to_string(),
            version_no: version as i32,
            occurred_at,
        };
        let result = apply_game_entry(
            &config,
            snapshot.activity.effective_status(occurred_at),
            snapshot.activity.start_at,
            snapshot.activity.end_at,
            &snapshot.activity.timezone,
            &event,
            self.login_progress.as_ref(),
        )
        .map_err(Self::map_login_progress_error)?;
        if config.claim_mode == "automatic" {
            for stage_no in eligible_stage_numbers(&config, &result.state) {
                let request = ActivityActionRequest {
                    activity_id: activity_id.to_string(),
                    version,
                    stage_id: stage_no.to_string(),
                    action_type: "claim".to_string(),
                    client_request_id: format!("auto:{}:{}", result.period_key, stage_no),
                };
                let base = ActivityActionResponse {
                    ok: false,
                    error_code: None,
                    activity_id: activity_id.to_string(),
                    version,
                    stage_id: request.stage_id.clone(),
                    action_type: request.action_type.clone(),
                    client_request_id: request.client_request_id.clone(),
                    processing: false,
                    duplicate: false,
                    state_revision: result.state_revision as u64,
                };
                let response = self
                    .claim_login_reward(character_id, &request, &snapshot, base, true)
                    .await;
                if !response.ok && !response.duplicate {
                    return Err(ActivityEngineError::new(
                        response.error_code.unwrap_or("ACTIVITY_RETRYABLE_FAILURE"),
                        "automatic login reward delivery did not complete",
                    ));
                }
            }
        }
        Ok(result)
    }

    fn map_login_progress_error(error: LoginRewardProgressError) -> ActivityEngineError {
        match error {
            LoginRewardProgressError::InvalidEvent(message) =>
                ActivityEngineError::new("ACTIVITY_INVALID_REQUEST", message),
            LoginRewardProgressError::InvalidConfig(message) =>
                ActivityEngineError::new("ACTIVITY_INVALID_CONFIG", message),
            LoginRewardProgressError::ActivityNotActive =>
                ActivityEngineError::new("ACTIVITY_NOT_STARTED", "activity is not active"),
            LoginRewardProgressError::VersionConflict =>
                ActivityEngineError::new("ACTIVITY_VERSION_CONFLICT", "login progress changed concurrently"),
            LoginRewardProgressError::StorageUnavailable =>
                ActivityEngineError::new("ACTIVITY_STORAGE_UNAVAILABLE", "activity storage unavailable"),
            LoginRewardProgressError::NotQualified =>
                ActivityEngineError::new("ACTIVITY_QUALIFICATION_NOT_MET", "login reward qualification is not met"),
            LoginRewardProgressError::AlreadyClaimed =>
                ActivityEngineError::new("ACTIVITY_ALREADY_CLAIMED", "login reward stage has already been claimed"),
        }
    }

    async fn claim_login_reward(
        &self,
        character_id: &str,
        request: &ActivityActionRequest,
        snapshot: &PublishedActivitySnapshot,
        base: ActivityActionResponse,
        automatic: bool,
    ) -> ActivityActionResponse {
        let config: LoginRewardConfig = match serde_json::from_value(snapshot.version.type_config.clone()) {
            Ok(config) => config,
            Err(_) => return Self::failed(base, "ACTIVITY_INVALID_CONFIG"),
        };
        if (automatic && config.claim_mode != "automatic") || (!automatic && config.claim_mode != "manual") {
            return Self::failed(base, "ACTIVITY_INVALID_ACTION");
        }
        let stage = match request.stage_id.parse::<u32>().ok().and_then(|stage_no| {
            config.stages.iter().find(|stage| stage.stage_no == stage_no)
        }) {
            Some(stage) => stage,
            None => return Self::failed(base, "ACTIVITY_QUALIFICATION_NOT_MET"),
        };
        let (state, revision) = match self.login_progress.load(
            character_id,
            &request.activity_id,
            snapshot.version.version_no,
        ) {
            Ok(value) => value,
            Err(error) => return Self::failed(base, Self::map_login_progress_error(error).code),
        };
        let period_key = match state.last_period_key.clone() {
            Some(period_key) => period_key,
            None => return Self::failed(base, "ACTIVITY_QUALIFICATION_NOT_MET"),
        };
        let count = if config.progression == "cumulative" {
            state.cumulative_count
        } else {
            state.consecutive_count
        };
        if count < stage.required_count {
            return Self::failed(base, "ACTIVITY_QUALIFICATION_NOT_MET");
        }
        let semantic_claim_key = login_reward_claim_key(
            &request.stage_id,
            &period_key,
            snapshot.version.version_no,
        );
        if state.claimed_stage_ids.iter().any(|value| value == &semantic_claim_key) {
            let mut response = ActivityActionResponse { ok: true, ..base };
            response.duplicate = true;
            response.state_revision = revision as u64;
            return response;
        }
        let items = match Self::reward_items(&snapshot.version.public_config, &stage.reward_group_key, character_id) {
            Ok(items) if !items.is_empty() => items,
            _ => return Self::failed(base, "ACTIVITY_MANUAL_REVIEW"),
        };
        let Some(coordinator) = &self.claim_coordinator else {
            return Self::failed(base, "ACTIVITY_MANUAL_REVIEW");
        };
        let order = match build_reward_order(
            character_id,
            &request.activity_id,
            snapshot.version.version_no,
            &semantic_claim_key,
            &items,
            RewardDeliveryPolicy::PreferInventory,
        ) {
            Ok(order) => order,
            Err(_) => return Self::failed(base, "ACTIVITY_MANUAL_REVIEW"),
        };
        let settlement = coordinator
            .settle(
                character_id,
                &request.activity_id,
                snapshot.version.version_no,
                &semantic_claim_key,
                &request.client_request_id,
                order,
            )
            .await;
        match settlement.status {
            ClaimStatus::Granted => {
                let current_stage_id = Some(request.stage_id.clone());
                let marked = self.login_progress.claim_stage(
                    character_id,
                    &request.activity_id,
                    snapshot.version.version_no,
                    revision,
                    &semantic_claim_key,
                    current_stage_id,
                );
                match marked {
                    Ok(next_revision) => ActivityActionResponse {
                        ok: true,
                        duplicate: settlement.duplicate,
                        state_revision: next_revision as u64,
                        ..base
                    },
                    Err(LoginRewardProgressError::AlreadyClaimed) => ActivityActionResponse {
                        ok: true,
                        duplicate: true,
                        state_revision: revision as u64,
                        ..base
                    },
                    Err(LoginRewardProgressError::VersionConflict) => {
                        let latest = self.login_progress.load(character_id, &request.activity_id, snapshot.version.version_no);
                        if latest.ok().is_some_and(|(state, _)| state.claimed_stage_ids.iter().any(|value| value == &semantic_claim_key)) {
                            ActivityActionResponse { ok: true, duplicate: true, ..base }
                        } else {
                            Self::failed(base, "ACTIVITY_RETRYABLE_FAILURE")
                        }
                    }
                    Err(error) => Self::failed(base, Self::map_login_progress_error(error).code),
                }
            }
            ClaimStatus::Processing => {
                let mut response = Self::failed(base, "ACTIVITY_PROCESSING");
                response.processing = true;
                response.duplicate = settlement.duplicate;
                response
            }
            ClaimStatus::RetryableFailure => Self::failed(base, "ACTIVITY_RETRYABLE_FAILURE"),
            ClaimStatus::ReconciliationPending => Self::failed(base, "ACTIVITY_RECONCILIATION_PENDING"),
            ClaimStatus::ManualReview => Self::failed(base, "ACTIVITY_MANUAL_REVIEW"),
        }
    }

    fn reward_items(
        public_config: &serde_json::Value,
        reward_group_key: &str,
        character_id: &str,
    ) -> Result<Vec<NormalizedAssetItem>, ()> {
        let groups = public_config.get("reward_groups").ok_or(())?;
        let group = if let Some(groups) = groups.as_array() {
            groups.iter().find(|group| {
                group.get("key").and_then(|value| value.as_str()) == Some(reward_group_key)
                    || group.get("reward_group_key").and_then(|value| value.as_str()) == Some(reward_group_key)
            }).ok_or(())?
        } else {
            groups.get(reward_group_key).ok_or(())?
        };
        let items = group.get("items").and_then(|value| value.as_array()).ok_or(())?;
        items.iter().map(|item| {
            let item_id = item.get("item_id").or_else(|| item.get("asset_id")).and_then(|value| value.as_i64()).ok_or(())? as i32;
            let count = item.get("count").or_else(|| item.get("quantity")).and_then(|value| value.as_u64()).ok_or(())? as u32;
            let binding = match item.get("binding").and_then(|value| value.as_str()) {
                Some("character_bound") => AssetBinding::CharacterBound { character_id: character_id.to_string() },
                _ => AssetBinding::Unbound,
            };
            NormalizedAssetItem::new(item_id, count, binding).map_err(|_| ())
        }).collect()
    }

    pub(crate) async fn list(
        &self,
        character_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<PublishedActivitySnapshot>, ActivityEngineError> {
        if !self.enabled {
            return Err(Self::unavailable_error());
        }
        if character_id.trim().is_empty() {
            return Err(Self::auth_error());
        }
        if self.check_rate_limit(character_id, "*", "list").await {
            return Err(Self::rate_limited_error());
        }
        self.repository.list_published(now).await.map_err(|_| {
            ActivityEngineError::new(
                "ACTIVITY_STORAGE_UNAVAILABLE",
                "activity storage unavailable",
            )
        })
    }

    pub(crate) async fn detail(
        &self,
        character_id: &str,
        activity_id: &str,
        version: u32,
        now: DateTime<Utc>,
    ) -> Result<PublishedActivitySnapshot, ActivityEngineError> {
        if !self.enabled {
            return Err(Self::unavailable_error());
        }
        if character_id.trim().is_empty() {
            return Err(Self::auth_error());
        }
        if self
            .check_rate_limit(character_id, activity_id, "detail")
            .await
        {
            return Err(Self::rate_limited_error());
        }
        let snapshot = self.load_detail(activity_id, now).await?;
        if version != 0 && snapshot.version.version_no != version as i32 {
            return Err(ActivityEngineError::new(
                "ACTIVITY_INVALID_VERSION",
                "requested activity version is not current",
            ));
        }
        Self::validate_read_status(&snapshot, now)?;
        Ok(snapshot)
    }

    async fn load_detail(
        &self,
        activity_id: &str,
        now: DateTime<Utc>,
    ) -> Result<PublishedActivitySnapshot, ActivityEngineError> {
        self.repository
            .get_published_for_detail(activity_id, now)
            .await
            .map_err(|_| {
                ActivityEngineError::new(
                    "ACTIVITY_STORAGE_UNAVAILABLE",
                    "activity storage unavailable",
                )
            })?
            .ok_or_else(|| {
                ActivityEngineError::new("ACTIVITY_NOT_FOUND", "published activity was not found")
            })
    }

    pub(crate) async fn dispatch_action(
        &self,
        character_id: &str,
        request: ActivityActionRequest,
        now: DateTime<Utc>,
    ) -> ActivityActionResponse {
        let base = ActivityActionResponse {
            ok: false,
            error_code: None,
            activity_id: request.activity_id.clone(),
            version: request.version,
            stage_id: request.stage_id.clone(),
            action_type: request.action_type.clone(),
            client_request_id: request.client_request_id.clone(),
            processing: false,
            duplicate: false,
            state_revision: 0,
        };
        if !self.enabled {
            return Self::failed(base, Self::unavailable_error().code);
        }
        if character_id.trim().is_empty() {
            return Self::failed(base, Self::auth_error().code);
        }
        if request.client_request_id.trim().is_empty() || request.client_request_id.len() > 128 {
            return Self::failed(base, "ACTIVITY_INVALID_REQUEST");
        }
        let request_key = format!("{character_id}:{}", request.client_request_id);
        let mut state = self.request_state.lock().await;
        if let Some(previous) = state.seen.get(&request_key).cloned() {
            let mut response = previous;
            response.duplicate = true;
            return response;
        }
        let limit_key = format!(
            "action:{character_id}:{}:{}",
            request.activity_id, request.action_type
        );
        if state
            .rate_limits
            .get(&limit_key)
            .is_some_and(|at| at.elapsed() < Duration::from_millis(100))
        {
            return Self::failed(base, "ACTIVITY_RATE_LIMITED");
        }
        state.rate_limits.insert(limit_key, Instant::now());
        drop(state);

        let snapshot = match self.load_detail(&request.activity_id, now).await {
            Ok(snapshot) => snapshot,
            Err(error) => return Self::failed(base, error.code),
        };
        if request.version != 0 && snapshot.version.version_no != request.version as i32 {
            return Self::failed(base, "ACTIVITY_INVALID_VERSION");
        }
        if let Err(error) = Self::validate_read_status(&snapshot, now) {
            let ended_claim_window = snapshot.activity.activity_type.as_str() == "login_reward"
                && request.action_type == "claim"
                && error.code == "ACTIVITY_ENDED"
                && now < snapshot.activity.claim_deadline
                && self
                    .login_progress
                    .load(
                        character_id,
                        &request.activity_id,
                        snapshot.version.version_no,
                    )
                    .ok()
                    .is_some_and(|(state, _)| state.last_period_key.is_some());
            if !ended_claim_window {
                return Self::failed(base, error.code);
            }
        }
        if request.action_type == "claim" && request.stage_id.trim().is_empty() {
            return Self::failed(base, "ACTIVITY_INVALID_REQUEST");
        }
        if snapshot.activity.activity_type.as_str() == "login_reward"
            && request.action_type == "claim"
        {
            let response = self
                .claim_login_reward(character_id, &request, &snapshot, base.clone(), false)
                .await;
            self.request_state
                .lock()
                .await
                .seen
                .insert(request_key, response.clone());
            return response;
        }
        let mut transaction = TransactionContext {
            request_id: request.client_request_id.clone(),
        };
        let outcome = self.registry.dispatch_action(
            &snapshot.activity,
            &snapshot.version,
            &request.action_type,
            &PlayerContext {
                character_id: character_id.to_string(),
            },
            None,
            &mut transaction,
        );
        let response = match outcome {
            Ok(outcome) if outcome.applied => {
                if request.action_type == "claim" {
                    if let Some(coordinator) = &self.claim_coordinator {
                        let items: Vec<NormalizedAssetItem> = outcome
                            .result
                            .get("reward_items")
                            .and_then(|items| serde_json::from_value(items.clone()).ok())
                            .unwrap_or_default();
                        if items.is_empty() {
                            return Self::failed(base, "ACTIVITY_MANUAL_REVIEW");
                        }
                        let semantic_claim_key = request.stage_id.clone();
                        let order = match build_reward_order(
                            character_id,
                            &request.activity_id,
                            snapshot.version.version_no,
                            &semantic_claim_key,
                            &items,
                            RewardDeliveryPolicy::PreferInventory,
                        ) {
                            Ok(order) => order,
                            Err(_) => return Self::failed(base, "ACTIVITY_MANUAL_REVIEW"),
                        };
                        let settlement = coordinator
                            .settle(
                                character_id,
                                &request.activity_id,
                                snapshot.version.version_no,
                                &semantic_claim_key,
                                &request.client_request_id,
                                order,
                            )
                            .await;
                        let duplicate = settlement.duplicate;
                        match settlement.status {
                            ClaimStatus::Granted => ActivityActionResponse {
                                ok: true,
                                duplicate,
                                ..base
                            },
                            ClaimStatus::Processing => {
                                let mut response = Self::failed(base, "ACTIVITY_PROCESSING");
                                response.processing = true;
                                response.duplicate = duplicate;
                                response
                            }
                            ClaimStatus::RetryableFailure => {
                                let mut response = Self::failed(base, "ACTIVITY_RETRYABLE_FAILURE");
                                response.duplicate = duplicate;
                                response
                            }
                            ClaimStatus::ReconciliationPending => {
                                let mut response =
                                    Self::failed(base, "ACTIVITY_RECONCILIATION_PENDING");
                                response.duplicate = duplicate;
                                response
                            }
                            ClaimStatus::ManualReview => {
                                let mut response = Self::failed(base, "ACTIVITY_MANUAL_REVIEW");
                                response.duplicate = duplicate;
                                response
                            }
                        }
                    } else {
                        ActivityActionResponse { ok: true, ..base }
                    }
                } else {
                    ActivityActionResponse { ok: true, ..base }
                }
            }
            Ok(_) => Self::failed(base, "ACTIVITY_QUALIFICATION_NOT_MET"),
            Err(error) => Self::failed(base, error.code.as_str()),
        };
        self.request_state
            .lock()
            .await
            .seen
            .insert(request_key, response.clone());
        response
    }

    fn validate_read_status(
        snapshot: &PublishedActivitySnapshot,
        now: DateTime<Utc>,
    ) -> Result<(), ActivityEngineError> {
        match snapshot.activity.effective_status(now) {
            ActivityStatus::Published if now < snapshot.activity.start_at => Err(
                ActivityEngineError::new("ACTIVITY_NOT_STARTED", "activity has not started"),
            ),
            ActivityStatus::Ended => Err(ActivityEngineError::new(
                "ACTIVITY_ENDED",
                "activity has ended",
            )),
            ActivityStatus::Offline => Err(ActivityEngineError::new(
                "ACTIVITY_OFFLINE",
                "activity is offline",
            )),
            _ => Ok(()),
        }
    }

    fn auth_error() -> ActivityEngineError {
        ActivityEngineError::new(
            "ACTIVITY_AUTH_REQUIRED",
            "character-bound authentication is required",
        )
    }

    fn unavailable_error() -> ActivityEngineError {
        ActivityEngineError::new(
            "ACTIVITY_ENGINE_UNAVAILABLE",
            "activity engine is not enabled in this server",
        )
    }

    fn rate_limited_error() -> ActivityEngineError {
        ActivityEngineError::new("ACTIVITY_RATE_LIMITED", "activity request rate limited")
    }

    async fn check_rate_limit(&self, character_id: &str, activity_id: &str, action: &str) -> bool {
        let key = format!("read:{character_id}:{activity_id}:{action}");
        let mut state = self.request_state.lock().await;
        if state
            .rate_limits
            .get(&key)
            .is_some_and(|at| at.elapsed() < Duration::from_millis(100))
        {
            return true;
        }
        state.rate_limits.insert(key, Instant::now());
        false
    }

    fn failed(mut response: ActivityActionResponse, code: &'static str) -> ActivityActionResponse {
        response.error_code = Some(code);
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{Activity, ActivityScope, ActivityType, ActivityVersion};
    use chrono::{Duration as ChronoDuration, TimeZone};
    use serde_json::json;

    async fn fixture_with_window(
        now: DateTime<Utc>,
        start_at: DateTime<Utc>,
        end_at: DateTime<Utc>,
    ) -> (ActivityEngine, Arc<InMemoryActivityRepository>) {
        let repo = Arc::new(InMemoryActivityRepository::default());
        let activity = Activity::new(
            "a1",
            "a1",
            ActivityType::new("login_reward").unwrap(),
            ActivityScope::Character,
            start_at,
            end_at,
            now + ChronoDuration::hours(2),
            "UTC",
        )
        .unwrap();
        let version = ActivityVersion::draft(
            activity.id.clone(),
            1,
            json!({}),
            json!({
                "schema_version": 1,
                "event_source": "game_entry",
                "cycle_unit": "natural_day",
                "progression": "consecutive",
                "miss_policy": "reset",
                "claim_mode": "manual",
                "stages": [{"stage_no": 1, "required_count": 1, "reward_group_key": "login-day-1"}]
            }),
            activity.start_at,
            activity.end_at,
            activity.claim_deadline,
            "UTC",
        )
        .unwrap();
        repo.save_draft(activity.clone(), version).await.unwrap();
        repo.publish(&activity.id, 1, None).await.unwrap();
        (ActivityEngine::new(repo.clone()), repo)
    }

    async fn fixture(now: DateTime<Utc>) -> ActivityEngine {
        fixture_with_window(
            now,
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(1),
        )
        .await
        .0
    }

    #[tokio::test]
    async fn list_detail_and_action_apply_server_context_and_idempotency() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let engine = fixture(now).await;
        assert_eq!(engine.list("character-1", now).await.unwrap().len(), 1);
        let detail = engine.detail("character-1", "a1", 1, now).await.unwrap();
        assert_eq!(detail.activity.id, "a1");
        let request = ActivityActionRequest {
            activity_id: "a1".into(),
            version: 1,
            stage_id: "stage-1".into(),
            action_type: "detail".into(),
            client_request_id: "req-1".into(),
        };
        let first = engine
            .dispatch_action("character-1", request.clone(), now)
            .await;
        assert_eq!(first.error_code, Some("ACTIVITY_QUALIFICATION_NOT_MET"));
        let second = engine.dispatch_action("character-1", request, now).await;
        assert!(second.duplicate);
    }

    #[tokio::test]
    async fn rejects_auth_version_and_rate_limit_boundaries() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let engine = fixture(now).await;
        assert_eq!(
            engine.list("", now).await.unwrap_err().code,
            "ACTIVITY_AUTH_REQUIRED"
        );
        assert_eq!(
            engine
                .detail("character-1", "not-owned-activity", 0, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_NOT_FOUND"
        );
        let unauthorized_action = engine
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "not-owned-activity".into(),
                    version: 0,
                    stage_id: "stage-1".into(),
                    action_type: "claim".into(),
                    client_request_id: "unauthorized-activity".into(),
                },
                now,
            )
            .await;
        assert_eq!(unauthorized_action.error_code, Some("ACTIVITY_NOT_FOUND"));
        let request = ActivityActionRequest {
            activity_id: "a1".into(),
            version: 1,
            stage_id: "".into(),
            action_type: "detail".into(),
            client_request_id: "req-2".into(),
        };
        let first = engine.dispatch_action("character-1", request, now).await;
        assert_eq!(first.error_code, Some("ACTIVITY_QUALIFICATION_NOT_MET"));
        engine.list("character-1", now).await.unwrap();
        assert_eq!(
            engine.list("character-1", now).await.unwrap_err().code,
            "ACTIVITY_RATE_LIMITED"
        );
        engine.detail("character-1", "a1", 1, now).await.unwrap();
        assert_eq!(
            engine
                .detail("character-1", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_RATE_LIMITED"
        );
    }

    #[tokio::test]
    async fn enforces_lifecycle_and_character_boundaries() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let (not_started, _) = fixture_with_window(
            now,
            now + ChronoDuration::hours(1),
            now + ChronoDuration::hours(2),
        )
        .await;
        assert_eq!(
            not_started
                .detail("character-1", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_NOT_STARTED"
        );
        let not_started_action = not_started
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 1,
                    stage_id: "stage-1".into(),
                    action_type: "claim".into(),
                    client_request_id: "not-started-action".into(),
                },
                now,
            )
            .await;
        assert_eq!(not_started_action.error_code, Some("ACTIVITY_NOT_STARTED"));
        let (ended, _) = fixture_with_window(
            now,
            now - ChronoDuration::hours(2),
            now - ChronoDuration::hours(1),
        )
        .await;
        assert_eq!(
            ended
                .detail("character-1", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_ENDED"
        );
        let ended_action = ended
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 1,
                    stage_id: "stage-1".into(),
                    action_type: "claim".into(),
                    client_request_id: "ended-action".into(),
                },
                now,
            )
            .await;
        assert_eq!(ended_action.error_code, Some("ACTIVITY_ENDED"));
        let (offline, repo) = fixture_with_window(
            now,
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(1),
        )
        .await;
        repo.offline("a1", 1).await.unwrap();
        assert_eq!(
            offline
                .detail("character-1", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_OFFLINE"
        );
        let offline_action = offline
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 1,
                    stage_id: "stage-1".into(),
                    action_type: "claim".into(),
                    client_request_id: "offline-action".into(),
                },
                now,
            )
            .await;
        assert_eq!(offline_action.error_code, Some("ACTIVITY_OFFLINE"));

        let request = ActivityActionRequest {
            activity_id: "a1".into(),
            version: 1,
            stage_id: "stage-1".into(),
            action_type: "detail".into(),
            client_request_id: "same-request".into(),
        };
        let (engine, _) = fixture_with_window(
            now,
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(1),
        )
        .await;
        let invalid_version = engine
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 9,
                    stage_id: "stage-1".into(),
                    action_type: "version-check".into(),
                    client_request_id: "invalid-version-action".into(),
                },
                now,
            )
            .await;
        assert_eq!(invalid_version.error_code, Some("ACTIVITY_INVALID_VERSION"));
        let first = engine
            .dispatch_action("character-1", request.clone(), now)
            .await;
        let other_character = engine
            .dispatch_action("character-2", request.clone(), now)
            .await;
        let duplicate = engine.dispatch_action("character-1", request, now).await;
        assert!(!first.duplicate);
        assert!(!other_character.duplicate);
        assert!(duplicate.duplicate);
    }

    #[tokio::test]
    async fn disabled_engine_returns_explicit_unavailable_error() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let engine = ActivityEngine::disabled();
        assert_eq!(
            engine.list("character-1", now).await.unwrap_err().code,
            "ACTIVITY_ENGINE_UNAVAILABLE"
        );
        let response = engine
            .dispatch_action(
                "character-1",
                ActivityActionRequest {
                    activity_id: "a1".into(),
                    version: 1,
                    stage_id: "stage-1".into(),
                    action_type: "detail".into(),
                    client_request_id: "req-1".into(),
                },
                now,
            )
            .await;
        assert_eq!(response.error_code, Some("ACTIVITY_ENGINE_UNAVAILABLE"));
        assert_eq!(
            engine
                .on_game_entry("character-1", "a1", 1, now)
                .await
                .unwrap_err()
                .code,
            "ACTIVITY_ENGINE_UNAVAILABLE"
        );
    }

    #[tokio::test]
    async fn trusted_game_entry_updates_login_progress_and_is_idempotent() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let engine = fixture(now).await;
        let first = engine
            .on_game_entry("character-1", "a1", 1, now)
            .await
            .unwrap();
        assert!(!first.duplicate);
        assert_eq!(first.state.cumulative_count, 1);
        assert_eq!(first.state.consecutive_count, 1);
        assert_eq!(first.current_stage_no, Some(1));

        let duplicate = engine
            .on_game_entry("character-1", "a1", 1, now + ChronoDuration::minutes(10))
            .await
            .unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.state.cumulative_count, 1);
        assert_eq!(duplicate.state_revision, first.state_revision);
    }

    #[tokio::test]
    async fn trusted_game_entry_rejects_version_lifecycle_and_identity_boundaries() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();
        let engine = fixture(now).await;
        assert_eq!(
            engine.on_game_entry("character-1", "a1", 9, now).await.unwrap_err().code,
            "ACTIVITY_INVALID_VERSION"
        );
        assert_eq!(
            engine.on_game_entry("", "a1", 1, now).await.unwrap_err().code,
            "ACTIVITY_AUTH_REQUIRED"
        );

        let (ended, _) = fixture_with_window(
            now,
            now - ChronoDuration::hours(2),
            now - ChronoDuration::hours(1),
        )
        .await;
        assert_eq!(
            ended.on_game_entry("character-1", "a1", 1, now).await.unwrap_err().code,
            "ACTIVITY_ENDED"
        );

        let (offline, repo) = fixture_with_window(
            now,
            now - ChronoDuration::hours(1),
            now + ChronoDuration::hours(1),
        )
        .await;
        repo.offline("a1", 1).await.unwrap();
        assert_eq!(
            offline.on_game_entry("character-1", "a1", 1, now).await.unwrap_err().code,
            "ACTIVITY_OFFLINE"
        );
    }
}
