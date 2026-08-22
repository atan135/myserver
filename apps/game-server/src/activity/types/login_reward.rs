use super::{
    ACTIVITY_TYPE_SCHEMA_VERSION, ActionApplier, ActionDecision, ActionEvaluator, ActionOutcome,
    ActivityTypeError, ActivityTypeHandler, ConfigValidator, PlayerContext, PlayerViewBuilder,
    TransactionContext, contract_decision,
};
use crate::activity::{Activity, ActivityVersion, PlayerActivityState};
use chrono::TimeZone;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginRewardStageConfig {
    pub(crate) stage_no: u32,
    pub(crate) required_count: u32,
    pub(crate) reward_group_key: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoginRewardConfig {
    pub(crate) schema_version: i64,
    pub(crate) event_source: String,
    pub(crate) cycle_unit: String,
    pub(crate) progression: String,
    pub(crate) miss_policy: String,
    pub(crate) claim_mode: String,
    pub(crate) stages: Vec<LoginRewardStageConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LoginRewardState {
    pub(crate) last_period_key: Option<String>,
    pub(crate) consecutive_count: u32,
    pub(crate) cumulative_count: u32,
    pub(crate) claimed_stage_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoginRewardView {
    pub(crate) event_source: String,
    pub(crate) cycle_unit: String,
    pub(crate) progression: String,
    pub(crate) miss_policy: String,
    pub(crate) claim_mode: String,
    pub(crate) stages: Vec<LoginRewardStageConfig>,
    pub(crate) state: LoginRewardState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GameEntryEvent {
    pub(crate) character_id: String,
    pub(crate) activity_id: String,
    pub(crate) version_no: i32,
    pub(crate) occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoginRewardProgressResult {
    pub(crate) period_key: String,
    pub(crate) duplicate: bool,
    pub(crate) state_revision: i64,
    pub(crate) state: LoginRewardState,
    pub(crate) current_stage_no: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoginRewardProgressError {
    InvalidEvent(String),
    InvalidConfig(String),
    ActivityNotActive,
    VersionConflict,
    StorageUnavailable,
    NotQualified,
    AlreadyClaimed,
}

type LoginProgressFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub(crate) trait LoginRewardProgressRepository: Send + Sync {
    fn load<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version_no: i32,
    ) -> LoginProgressFuture<'a, Result<(LoginRewardState, i64), LoginRewardProgressError>>;
    fn load_activity_state<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version_no: i32,
    ) -> LoginProgressFuture<'a, Result<Option<PlayerActivityState>, LoginRewardProgressError>>;
    fn compare_and_set<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version_no: i32,
        expected_revision: i64,
        state: LoginRewardState,
        current_stage_id: Option<String>,
    ) -> LoginProgressFuture<'a, Result<i64, LoginRewardProgressError>>;
    fn claim_stage<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version_no: i32,
        expected_revision: i64,
        claim_key: &'a str,
        current_stage_id: Option<String>,
    ) -> LoginProgressFuture<'a, Result<i64, LoginRewardProgressError>>;
}

#[derive(Clone, Default)]
pub(crate) struct InMemoryLoginRewardProgressRepository {
    state: Arc<Mutex<HashMap<(String, String, i32), PlayerActivityState>>>,
}

impl LoginRewardProgressRepository for InMemoryLoginRewardProgressRepository {
    fn load<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version_no: i32,
    ) -> LoginProgressFuture<'a, Result<(LoginRewardState, i64), LoginRewardProgressError>> {
        Box::pin(async move {
            let state = self
                .state
                .lock()
                .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            let Some(record) = state.get(&(
                character_id.to_string(),
                activity_id.to_string(),
                version_no,
            )) else {
                return Ok((LoginRewardState::default(), 0));
            };
            let login_state = serde_json::from_value(record.type_state.clone())
                .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            Ok((login_state, record.state_revision))
        })
    }

    fn load_activity_state<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version_no: i32,
    ) -> LoginProgressFuture<'a, Result<Option<PlayerActivityState>, LoginRewardProgressError>>
    {
        Box::pin(async move {
            let state = self
                .state
                .lock()
                .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            Ok(state
                .get(&(
                    character_id.to_string(),
                    activity_id.to_string(),
                    version_no,
                ))
                .cloned())
        })
    }

    fn compare_and_set<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version_no: i32,
        expected_revision: i64,
        next: LoginRewardState,
        current_stage_id: Option<String>,
    ) -> LoginProgressFuture<'a, Result<i64, LoginRewardProgressError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            let key = (
                character_id.to_string(),
                activity_id.to_string(),
                version_no,
            );
            let current_revision = state
                .get(&key)
                .map(|record| record.state_revision)
                .unwrap_or(0);
            if current_revision != expected_revision {
                return Err(LoginRewardProgressError::VersionConflict);
            }
            let next_revision = current_revision.saturating_add(1);
            let progress = json!({
                "last_period_key": next.last_period_key,
                "consecutive_count": next.consecutive_count,
                "cumulative_count": next.cumulative_count,
            });
            let type_state = serde_json::to_value(&next)
                .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            state.insert(
                key,
                PlayerActivityState {
                    character_id: character_id.to_string(),
                    activity_id: activity_id.to_string(),
                    version_no,
                    current_stage_id,
                    progress,
                    type_state,
                    state_revision: next_revision,
                },
            );
            Ok(next_revision)
        })
    }

    fn claim_stage<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version_no: i32,
        expected_revision: i64,
        claim_key: &'a str,
        current_stage_id: Option<String>,
    ) -> LoginProgressFuture<'a, Result<i64, LoginRewardProgressError>> {
        Box::pin(async move {
            let mut state = self
                .state
                .lock()
                .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            let key = (
                character_id.to_string(),
                activity_id.to_string(),
                version_no,
            );
            let record = state
                .get_mut(&key)
                .ok_or(LoginRewardProgressError::NotQualified)?;
            if record.state_revision != expected_revision {
                return Err(LoginRewardProgressError::VersionConflict);
            }
            let mut login_state: LoginRewardState =
                serde_json::from_value(record.type_state.clone())
                    .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            if login_state
                .claimed_stage_ids
                .iter()
                .any(|value| value == claim_key)
            {
                return Err(LoginRewardProgressError::AlreadyClaimed);
            }
            login_state.claimed_stage_ids.push(claim_key.to_string());
            let next_revision = expected_revision.saturating_add(1);
            record.current_stage_id = current_stage_id;
            record.progress["claimed_stage_ids"] = json!(login_state.claimed_stage_ids);
            record.type_state = serde_json::to_value(&login_state)
                .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            record.state_revision = next_revision;
            Ok(next_revision)
        })
    }
}

impl InMemoryLoginRewardProgressRepository {
    pub(crate) fn load_activity_state_sync(
        &self,
        character_id: &str,
        activity_id: &str,
        version_no: i32,
    ) -> Option<PlayerActivityState> {
        self.state
            .lock()
            .ok()?
            .get(&(
                character_id.to_string(),
                activity_id.to_string(),
                version_no,
            ))
            .cloned()
    }
}

#[derive(Clone)]
pub(crate) struct PgLoginRewardProgressRepository {
    pool: PgPool,
}

impl PgLoginRewardProgressRepository {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    fn decode_state(value: Value) -> Result<LoginRewardState, LoginRewardProgressError> {
        serde_json::from_value(value).map_err(|_| LoginRewardProgressError::StorageUnavailable)
    }

    fn state_values(state: &LoginRewardState) -> Result<(Value, Value), LoginRewardProgressError> {
        let progress = json!({
            "last_period_key": state.last_period_key,
            "consecutive_count": state.consecutive_count,
            "cumulative_count": state.cumulative_count,
            "claimed_stage_ids": state.claimed_stage_ids,
        });
        let type_state = serde_json::to_value(state)
            .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
        Ok((progress, type_state))
    }
}

impl LoginRewardProgressRepository for PgLoginRewardProgressRepository {
    fn load<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version_no: i32,
    ) -> LoginProgressFuture<'a, Result<(LoginRewardState, i64), LoginRewardProgressError>> {
        Box::pin(async move {
            let row = sqlx::query_as::<_, (Value, i64)>(
                "SELECT type_state_json, state_revision FROM player_activity_state WHERE character_id = $1 AND activity_id = $2 AND version_no = $3",
            )
            .bind(character_id)
            .bind(activity_id)
            .bind(version_no)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            match row {
                Some((state, revision)) => Ok((Self::decode_state(state)?, revision)),
                None => Ok((LoginRewardState::default(), 0)),
            }
        })
    }

    fn load_activity_state<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version_no: i32,
    ) -> LoginProgressFuture<'a, Result<Option<PlayerActivityState>, LoginRewardProgressError>>
    {
        Box::pin(async move {
            let row = sqlx::query_as::<_, (Option<String>, Value, Value, i64)>(
                "SELECT current_stage_id, progress_json, type_state_json, state_revision FROM player_activity_state WHERE character_id = $1 AND activity_id = $2 AND version_no = $3",
            )
            .bind(character_id)
            .bind(activity_id)
            .bind(version_no)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            Ok(
                row.map(|(current_stage_id, progress, type_state, state_revision)| {
                    PlayerActivityState {
                        character_id: character_id.to_string(),
                        activity_id: activity_id.to_string(),
                        version_no,
                        current_stage_id,
                        progress,
                        type_state,
                        state_revision,
                    }
                }),
            )
        })
    }

    fn compare_and_set<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version_no: i32,
        expected_revision: i64,
        state: LoginRewardState,
        current_stage_id: Option<String>,
    ) -> LoginProgressFuture<'a, Result<i64, LoginRewardProgressError>> {
        Box::pin(async move {
            let (progress, type_state) = Self::state_values(&state)?;
            let next_revision = expected_revision.saturating_add(1);
            let changed = if expected_revision == 0 {
                sqlx::query(
                    r#"INSERT INTO player_activity_state (
                        character_id, activity_id, version_no, current_stage_id,
                        progress_json, type_state_json, state_revision
                    ) VALUES ($1, $2, $3, $4, $5, $6, 1)
                    ON CONFLICT (character_id, activity_id, version_no) DO NOTHING"#,
                )
                .bind(character_id)
                .bind(activity_id)
                .bind(version_no)
                .bind(&current_stage_id)
                .bind(&progress)
                .bind(&type_state)
                .execute(&self.pool)
                .await
            } else {
                sqlx::query(
                    r#"UPDATE player_activity_state SET
                        current_stage_id = $4,
                        progress_json = $5,
                        type_state_json = $6,
                        state_revision = $7,
                        updated_at = current_timestamp
                    WHERE character_id = $1 AND activity_id = $2 AND version_no = $3
                      AND state_revision = $8"#,
                )
                .bind(character_id)
                .bind(activity_id)
                .bind(version_no)
                .bind(&current_stage_id)
                .bind(&progress)
                .bind(&type_state)
                .bind(next_revision)
                .bind(expected_revision)
                .execute(&self.pool)
                .await
            }
            .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            if changed.rows_affected() == 1 {
                Ok(next_revision)
            } else {
                Err(LoginRewardProgressError::VersionConflict)
            }
        })
    }

    fn claim_stage<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version_no: i32,
        expected_revision: i64,
        claim_key: &'a str,
        current_stage_id: Option<String>,
    ) -> LoginProgressFuture<'a, Result<i64, LoginRewardProgressError>> {
        Box::pin(async move {
            let mut transaction = self
                .pool
                .begin()
                .await
                .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            let row = sqlx::query_as::<_, (Value, i64)>(
                "SELECT type_state_json, state_revision FROM player_activity_state WHERE character_id = $1 AND activity_id = $2 AND version_no = $3 FOR UPDATE",
            )
            .bind(character_id)
            .bind(activity_id)
            .bind(version_no)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| LoginRewardProgressError::StorageUnavailable)?
            .ok_or(LoginRewardProgressError::NotQualified)?;
            let mut state = Self::decode_state(row.0)?;
            if state
                .claimed_stage_ids
                .iter()
                .any(|value| value == claim_key)
            {
                return Err(LoginRewardProgressError::AlreadyClaimed);
            }
            if row.1 != expected_revision {
                return Err(LoginRewardProgressError::VersionConflict);
            }
            state.claimed_stage_ids.push(claim_key.to_string());
            let (progress, type_state) = Self::state_values(&state)?;
            let next_revision = expected_revision.saturating_add(1);
            sqlx::query(
                r#"UPDATE player_activity_state SET
                    current_stage_id = $4,
                    progress_json = $5,
                    type_state_json = $6,
                    state_revision = $7,
                    updated_at = current_timestamp
                WHERE character_id = $1 AND activity_id = $2 AND version_no = $3"#,
            )
            .bind(character_id)
            .bind(activity_id)
            .bind(version_no)
            .bind(current_stage_id)
            .bind(progress)
            .bind(type_state)
            .bind(next_revision)
            .execute(&mut *transaction)
            .await
            .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            transaction
                .commit()
                .await
                .map_err(|_| LoginRewardProgressError::StorageUnavailable)?;
            Ok(next_revision)
        })
    }
}

pub(crate) fn login_period_key(
    occurred_at: DateTime<Utc>,
    timezone: &str,
) -> Result<String, LoginRewardProgressError> {
    let zone: Tz = timezone.parse().map_err(|_| {
        LoginRewardProgressError::InvalidConfig(format!("unsupported IANA timezone '{timezone}'"))
    })?;
    Ok(zone
        .from_utc_datetime(&occurred_at.naive_utc())
        .date_naive()
        .format("%Y-%m-%d")
        .to_string())
}

pub(crate) fn login_reward_claim_key(stage_id: &str, period_key: &str, version_no: i32) -> String {
    format!("stage_id={stage_id};period_key={period_key};activity_version={version_no}")
}

fn period_date(period_key: &str) -> Result<NaiveDate, LoginRewardProgressError> {
    NaiveDate::parse_from_str(period_key, "%Y-%m-%d")
        .map_err(|_| LoginRewardProgressError::InvalidEvent("stored period key is invalid".into()))
}

pub(crate) async fn apply_game_entry(
    config: &LoginRewardConfig,
    activity_status: crate::activity::domain::ActivityStatus,
    activity_start: DateTime<Utc>,
    activity_end: DateTime<Utc>,
    timezone: &str,
    event: &GameEntryEvent,
    repository: &dyn LoginRewardProgressRepository,
) -> Result<LoginRewardProgressResult, LoginRewardProgressError> {
    if event.character_id.trim().is_empty()
        || event.activity_id.trim().is_empty()
        || event.version_no <= 0
    {
        return Err(LoginRewardProgressError::InvalidEvent(
            "trusted game entry identity is required".into(),
        ));
    }
    if !matches!(
        activity_status,
        crate::activity::domain::ActivityStatus::Published
            | crate::activity::domain::ActivityStatus::Running
    ) || event.occurred_at < activity_start
        || event.occurred_at >= activity_end
    {
        return Err(LoginRewardProgressError::ActivityNotActive);
    }
    if config.event_source != "game_entry" || config.cycle_unit != "natural_day" {
        return Err(LoginRewardProgressError::InvalidConfig(
            "login_reward requires game_entry/natural_day".into(),
        ));
    }
    let period_key = login_period_key(event.occurred_at, timezone)?;
    let (current, revision) = repository
        .load(&event.character_id, &event.activity_id, event.version_no)
        .await?;
    if current.last_period_key.as_deref() == Some(period_key.as_str()) {
        return Ok(LoginRewardProgressResult {
            period_key,
            duplicate: true,
            state_revision: revision,
            current_stage_no: current_stage(config, &current),
            state: current,
        });
    }
    let mut next = current.clone();
    let current_date = period_date(&period_key)?;
    let previous_date = current
        .last_period_key
        .as_deref()
        .map(period_date)
        .transpose()?;
    let consecutive = previous_date
        .is_some_and(|date| current_date.signed_duration_since(date) == Duration::days(1));
    next.cumulative_count = next.cumulative_count.saturating_add(1);
    next.consecutive_count = if consecutive || config.miss_policy == "carry" {
        next.consecutive_count.saturating_add(1)
    } else {
        1
    };
    next.last_period_key = Some(period_key.clone());
    let stage_id = current_stage(config, &next).map(|stage_no| stage_no.to_string());
    let next_revision = repository
        .compare_and_set(
            &event.character_id,
            &event.activity_id,
            event.version_no,
            revision,
            next.clone(),
            stage_id,
        )
        .await?;
    Ok(LoginRewardProgressResult {
        period_key,
        duplicate: false,
        state_revision: next_revision,
        current_stage_no: current_stage(config, &next),
        state: next,
    })
}

fn current_stage(config: &LoginRewardConfig, state: &LoginRewardState) -> Option<u32> {
    let count = if config.progression == "cumulative" {
        state.cumulative_count
    } else {
        state.consecutive_count
    };
    config
        .stages
        .iter()
        .filter(|stage| stage.required_count <= count)
        .max_by_key(|stage| stage.stage_no)
        .map(|stage| stage.stage_no)
}

pub(crate) fn eligible_stage_numbers(
    config: &LoginRewardConfig,
    state: &LoginRewardState,
) -> Vec<u32> {
    let count = if config.progression == "cumulative" {
        state.cumulative_count
    } else {
        state.consecutive_count
    };
    config
        .stages
        .iter()
        .filter(|stage| stage.required_count <= count)
        .map(|stage| stage.stage_no)
        .collect()
}

fn invalid(message: impl Into<String>) -> ActivityTypeError {
    ActivityTypeError {
        code: super::ActivityTypeErrorCode::InvalidConfig,
        message: message.into(),
    }
}

#[derive(Default)]
pub(crate) struct LoginRewardHandler;

impl ConfigValidator for LoginRewardHandler {
    fn validate_config(&self, config: &Value) -> Result<(), ActivityTypeError> {
        if !config.is_object() {
            return Err(ActivityTypeError {
                code: super::ActivityTypeErrorCode::InvalidConfig,
                message: "type config must be an object".into(),
            });
        }
        let parsed: LoginRewardConfig = serde_json::from_value(config.clone())
            .map_err(|error| invalid(format!("login_reward config is invalid: {error}")))?;
        if parsed.schema_version != self.schema_version() {
            return Err(invalid("login_reward schema version is unsupported"));
        }
        if parsed.event_source != "game_entry" {
            return Err(invalid("event_source must be game_entry"));
        }
        if parsed.cycle_unit != "natural_day" {
            return Err(invalid("cycle_unit must be natural_day"));
        }
        if !matches!(parsed.progression.as_str(), "consecutive" | "cumulative") {
            return Err(invalid("progression must be consecutive or cumulative"));
        }
        if !matches!(parsed.miss_policy.as_str(), "reset" | "carry") {
            return Err(invalid("miss_policy must be reset or carry"));
        }
        if !matches!(parsed.claim_mode.as_str(), "manual" | "automatic") {
            return Err(invalid("claim_mode must be manual or automatic"));
        }
        if parsed.stages.is_empty() {
            return Err(invalid("stages must not be empty"));
        }
        let mut stage_nos = std::collections::BTreeSet::new();
        for stage in &parsed.stages {
            if stage.stage_no == 0
                || stage.required_count == 0
                || stage.reward_group_key.trim().is_empty()
            {
                return Err(invalid(
                    "stage_no, required_count and reward_group_key are required",
                ));
            }
            if !stage_nos.insert(stage.stage_no) {
                return Err(invalid("stage_no must be unique"));
            }
        }
        Ok(())
    }
}

impl PlayerViewBuilder for LoginRewardHandler {
    fn build_player_view(
        &self,
        activity: &Activity,
        _version: &ActivityVersion,
        player_state: Option<&PlayerActivityState>,
    ) -> Result<Value, ActivityTypeError> {
        let config: LoginRewardConfig = serde_json::from_value(_version.type_config.clone())
            .map_err(|error| invalid(error.to_string()))?;
        let state = player_state
            .and_then(|value| {
                serde_json::from_value::<LoginRewardState>(value.type_state.clone()).ok()
            })
            .unwrap_or_default();
        let progression_count = if config.progression == "cumulative" {
            state.cumulative_count
        } else {
            state.consecutive_count
        };
        let claimed = state
            .claimed_stage_ids
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let stages = config.stages.iter().map(|stage| {
            let stage_id = stage.stage_no.to_string();
            let claim_key = crate::activity::types::login_reward_claim_key(&stage_id, state.last_period_key.as_deref().unwrap_or(""), _version.version_no);
            json!({
                "stage_no": stage.stage_no,
                "required_count": stage.required_count,
                "reward_group_key": stage.reward_group_key,
                "achieved": progression_count >= stage.required_count,
                "claimable": progression_count >= stage.required_count && !claimed.contains(&claim_key),
                "claimed": claimed.contains(&claim_key),
            })
        }).collect::<Vec<_>>();
        let current_stage_id = player_state.and_then(|value| value.current_stage_id.clone());
        Ok(
            json!({"type": activity.activity_type.as_str(), "schema_version": self.schema_version(), "contract_only": true, "event_source": config.event_source, "cycle_unit": config.cycle_unit, "progression": config.progression, "miss_policy": config.miss_policy, "claim_mode": config.claim_mode, "stages": stages, "current_stage_id": current_stage_id, "last_period_key": state.last_period_key, "consecutive_days": state.consecutive_count, "cumulative_days": state.cumulative_count, "today_status": if state.last_period_key.is_some() { "logged_in" } else { "not_logged_in" }}),
        )
    }
}

impl ActionEvaluator for LoginRewardHandler {
    fn evaluate_action(
        &self,
        action: &str,
        _context: &PlayerContext,
        _player_state: Option<&PlayerActivityState>,
    ) -> Result<ActionDecision, ActivityTypeError> {
        Ok(contract_decision(action))
    }
}

impl ActionApplier for LoginRewardHandler {
    fn apply_action(
        &self,
        decision: &ActionDecision,
        _transaction: &mut TransactionContext,
    ) -> Result<ActionOutcome, ActivityTypeError> {
        Ok(ActionOutcome {
            action: decision.action.clone(),
            applied: false,
            result: json!({"contract_only": true}),
        })
    }
}

impl ActivityTypeHandler for LoginRewardHandler {
    fn activity_type(&self) -> &'static str {
        "login_reward"
    }
    fn schema_version(&self) -> i64 {
        ACTIVITY_TYPE_SCHEMA_VERSION
    }
    fn supported_actions(&self) -> &'static [&'static str] {
        &["list", "detail", "claim", "progress"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> Value {
        json!({"schema_version": 1, "event_source": "game_entry", "cycle_unit": "natural_day", "progression": "consecutive", "miss_policy": "reset", "claim_mode": "manual", "stages": [{"stage_no": 1, "required_count": 1, "reward_group_key": "g1"}]})
    }

    #[test]
    fn validates_login_reward_contract_fields() {
        LoginRewardHandler::default()
            .validate_config(&config())
            .unwrap();
    }

    #[test]
    fn rejects_unknown_source_and_duplicate_stage_numbers() {
        let handler = LoginRewardHandler::default();
        let mut bad_source = config();
        bad_source["event_source"] = json!("client");
        assert_eq!(
            handler.validate_config(&bad_source).unwrap_err().code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
        let mut duplicate = config();
        duplicate["stages"] = json!([{ "stage_no": 1, "required_count": 1, "reward_group_key": "g1" }, { "stage_no": 1, "required_count": 2, "reward_group_key": "g2" }]);
        assert_eq!(
            handler.validate_config(&duplicate).unwrap_err().code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
        let mut bad_cycle = config();
        bad_cycle["cycle_unit"] = json!("weekly");
        assert_eq!(
            handler.validate_config(&bad_cycle).unwrap_err().code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
        let mut empty_reward = config();
        empty_reward["stages"][0]["reward_group_key"] = json!("");
        assert_eq!(
            handler.validate_config(&empty_reward).unwrap_err().code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
        let mut invalid_stage = config();
        invalid_stage["stages"][0]["stage_no"] = json!(0);
        assert_eq!(
            handler.validate_config(&invalid_stage).unwrap_err().code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
    }

    #[tokio::test]
    async fn game_entry_is_idempotent_and_uses_activity_timezone_boundary() {
        let handler = LoginRewardHandler::default();
        let config: LoginRewardConfig = serde_json::from_value(config()).unwrap();
        let repository = InMemoryLoginRewardProgressRepository::default();
        let first = GameEntryEvent {
            character_id: "c1".into(),
            activity_id: "a1".into(),
            version_no: 1,
            occurred_at: chrono::DateTime::parse_from_rfc3339("2026-08-21T16:30:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        };
        let second = GameEntryEvent {
            occurred_at: chrono::DateTime::parse_from_rfc3339("2026-08-21T16:45:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            ..first.clone()
        };
        let result = apply_game_entry(
            &config,
            crate::activity::domain::ActivityStatus::Running,
            first.occurred_at - chrono::Duration::hours(1),
            first.occurred_at + chrono::Duration::days(2),
            "Asia/Shanghai",
            &first,
            &repository,
        )
        .await
        .unwrap();
        assert_eq!(result.period_key, "2026-08-22");
        assert!(!result.duplicate);
        let duplicate = apply_game_entry(
            &config,
            crate::activity::domain::ActivityStatus::Running,
            first.occurred_at - chrono::Duration::hours(1),
            first.occurred_at + chrono::Duration::days(2),
            "Asia/Shanghai",
            &second,
            &repository,
        )
        .await
        .unwrap();
        assert!(duplicate.duplicate);
        assert_eq!(duplicate.state.cumulative_count, 1);
        let _ = handler;
    }

    #[tokio::test]
    async fn game_entry_handles_consecutive_cumulative_reset_and_carry() {
        let mut config: LoginRewardConfig = serde_json::from_value(config()).unwrap();
        let repository = InMemoryLoginRewardProgressRepository::default();
        let at = chrono::DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let event = |day| GameEntryEvent {
            character_id: "c1".into(),
            activity_id: "a1".into(),
            version_no: 1,
            occurred_at: at + chrono::Duration::days(day),
        };
        let range_start = at - chrono::Duration::hours(1);
        let range_end = at + chrono::Duration::days(10);
        let first = apply_game_entry(
            &config,
            crate::activity::domain::ActivityStatus::Running,
            range_start,
            range_end,
            "UTC",
            &event(0),
            &repository,
        )
        .await
        .unwrap();
        assert_eq!(first.state.consecutive_count, 1);
        let second = apply_game_entry(
            &config,
            crate::activity::domain::ActivityStatus::Running,
            range_start,
            range_end,
            "UTC",
            &event(1),
            &repository,
        )
        .await
        .unwrap();
        assert_eq!(second.state.consecutive_count, 2);
        let gap = apply_game_entry(
            &config,
            crate::activity::domain::ActivityStatus::Running,
            range_start,
            range_end,
            "UTC",
            &event(3),
            &repository,
        )
        .await
        .unwrap();
        assert_eq!(gap.state.consecutive_count, 1);
        assert_eq!(gap.state.cumulative_count, 3);
        config.miss_policy = "carry".into();
        let carry_repo = InMemoryLoginRewardProgressRepository::default();
        let _ = apply_game_entry(
            &config,
            crate::activity::domain::ActivityStatus::Running,
            range_start,
            range_end,
            "UTC",
            &event(0),
            &carry_repo,
        )
        .await
        .unwrap();
        let carry = apply_game_entry(
            &config,
            crate::activity::domain::ActivityStatus::Running,
            range_start,
            range_end,
            "UTC",
            &event(3),
            &carry_repo,
        )
        .await
        .unwrap();
        assert_eq!(carry.state.consecutive_count, 2);
    }

    #[tokio::test]
    async fn game_entry_rejects_ended_offline_and_version_isolation() {
        let config: LoginRewardConfig = serde_json::from_value(config()).unwrap();
        let repository = InMemoryLoginRewardProgressRepository::default();
        let event = GameEntryEvent {
            character_id: "c1".into(),
            activity_id: "a1".into(),
            version_no: 1,
            occurred_at: chrono::DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        };
        let start = event.occurred_at - chrono::Duration::hours(1);
        let end = event.occurred_at + chrono::Duration::hours(1);
        assert!(matches!(
            apply_game_entry(
                &config,
                crate::activity::domain::ActivityStatus::Ended,
                start,
                end,
                "UTC",
                &event,
                &repository
            )
            .await,
            Err(LoginRewardProgressError::ActivityNotActive)
        ));
        assert!(matches!(
            apply_game_entry(
                &config,
                crate::activity::domain::ActivityStatus::Offline,
                start,
                end,
                "UTC",
                &event,
                &repository
            )
            .await,
            Err(LoginRewardProgressError::ActivityNotActive)
        ));
        let mut version_two = event.clone();
        version_two.version_no = 2;
        let result = apply_game_entry(
            &config,
            crate::activity::domain::ActivityStatus::Running,
            start,
            end,
            "UTC",
            &version_two,
            &repository,
        )
        .await
        .unwrap();
        assert_eq!(result.state.cumulative_count, 1);
    }

    #[tokio::test]
    async fn progress_repository_syncs_player_activity_state_and_isolates_versions() {
        let config: LoginRewardConfig = serde_json::from_value(config()).unwrap();
        let repository = InMemoryLoginRewardProgressRepository::default();
        let occurred_at = chrono::DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let event = |version_no| GameEntryEvent {
            character_id: "c1".into(),
            activity_id: "a1".into(),
            version_no,
            occurred_at,
        };
        apply_game_entry(
            &config,
            crate::activity::domain::ActivityStatus::Running,
            occurred_at - chrono::Duration::hours(1),
            occurred_at + chrono::Duration::hours(1),
            "UTC",
            &event(1),
            &repository,
        )
        .await
        .unwrap();
        let stored = repository.load_activity_state_sync("c1", "a1", 1).unwrap();
        assert_eq!(stored.character_id, "c1");
        assert_eq!(stored.activity_id, "a1");
        assert_eq!(stored.version_no, 1);
        assert_eq!(stored.current_stage_id.as_deref(), Some("1"));
        assert_eq!(stored.progress["cumulative_count"], 1);
        assert_eq!(stored.type_state["consecutive_count"], 1);
        assert_eq!(stored.state_revision, 1);

        apply_game_entry(
            &config,
            crate::activity::domain::ActivityStatus::Running,
            occurred_at - chrono::Duration::hours(1),
            occurred_at + chrono::Duration::hours(1),
            "UTC",
            &event(2),
            &repository,
        )
        .await
        .unwrap();
        let version_two = repository.load_activity_state_sync("c1", "a1", 2).unwrap();
        assert_eq!(version_two.version_no, 2);
        assert_eq!(version_two.state_revision, 1);
        assert_eq!(
            repository
                .load_activity_state_sync("c1", "a1", 1)
                .unwrap()
                .state_revision,
            1
        );
    }

    #[test]
    fn login_period_key_uses_full_iana_timezone_and_dst_boundaries() {
        let before_midnight = chrono::DateTime::parse_from_rfc3339("2026-03-08T04:59:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let after_midnight = chrono::DateTime::parse_from_rfc3339("2026-03-08T05:01:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            login_period_key(before_midnight, "America/New_York").unwrap(),
            "2026-03-07"
        );
        assert_eq!(
            login_period_key(after_midnight, "America/New_York").unwrap(),
            "2026-03-08"
        );
        let dst_transition = chrono::DateTime::parse_from_rfc3339("2026-03-08T07:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            login_period_key(dst_transition, "America/New_York").unwrap(),
            "2026-03-08"
        );
        assert!(matches!(
            login_period_key(after_midnight, "Mars/Olympus"),
            Err(LoginRewardProgressError::InvalidConfig(_))
        ));
    }

    #[tokio::test]
    async fn claim_stage_records_semantic_key_and_is_idempotent() {
        let config: LoginRewardConfig = serde_json::from_value(config()).unwrap();
        let repository = InMemoryLoginRewardProgressRepository::default();
        let occurred_at = chrono::DateTime::parse_from_rfc3339("2026-08-21T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let event = GameEntryEvent {
            character_id: "c1".into(),
            activity_id: "a1".into(),
            version_no: 1,
            occurred_at,
        };
        let progress = apply_game_entry(
            &config,
            crate::activity::domain::ActivityStatus::Running,
            occurred_at - chrono::Duration::hours(1),
            occurred_at + chrono::Duration::hours(1),
            "UTC",
            &event,
            &repository,
        )
        .await
        .unwrap();
        let claim_key = login_reward_claim_key("1", &progress.period_key, 1);
        let revision = repository
            .claim_stage(
                "c1",
                "a1",
                1,
                progress.state_revision,
                &claim_key,
                Some("1".into()),
            )
            .await
            .unwrap();
        assert_eq!(revision, 2);
        let stored = repository.load_activity_state_sync("c1", "a1", 1).unwrap();
        assert_eq!(stored.current_stage_id.as_deref(), Some("1"));
        assert_eq!(stored.type_state["claimed_stage_ids"][0], claim_key);
        assert_eq!(stored.progress["claimed_stage_ids"][0], claim_key);
        assert_eq!(
            repository
                .claim_stage("c1", "a1", 1, revision, &claim_key, Some("1".into()))
                .await,
            Err(LoginRewardProgressError::AlreadyClaimed)
        );
    }

    #[test]
    fn eligible_stage_numbers_include_all_reached_stages_for_automatic_claims() {
        let mut config: LoginRewardConfig = serde_json::from_value(config()).unwrap();
        config.stages.push(LoginRewardStageConfig {
            stage_no: 2,
            required_count: 2,
            reward_group_key: "g2".into(),
        });
        let state = LoginRewardState {
            consecutive_count: 2,
            cumulative_count: 2,
            ..Default::default()
        };
        assert_eq!(eligible_stage_numbers(&config, &state), vec![1, 2]);
    }
}
