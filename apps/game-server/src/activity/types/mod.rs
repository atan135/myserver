//! Registered activity type contracts.
//!
//! Handlers own type-specific validation and decisions. The public activity
//! engine owns lifecycle, idempotency, persistence and reward delivery.

mod login_reward;
mod lottery;

use super::domain::{Activity, ActivityVersion, PlayerActivityState};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

pub(crate) use login_reward::{
    apply_game_entry, GameEntryEvent, InMemoryLoginRewardProgressRepository,
    LoginRewardConfig, LoginRewardProgressError, LoginRewardProgressRepository,
    LoginRewardProgressResult, LoginRewardHandler, eligible_stage_numbers, login_period_key, login_reward_claim_key,
};
pub(crate) use lottery::LotteryHandler;

pub(crate) const ACTIVITY_TYPE_SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActivityTypeErrorCode {
    UnknownType,
    UnknownAction,
    SchemaVersionUnsupported,
    InvalidConfig,
    HandlerRejected,
}

impl ActivityTypeErrorCode {
    pub(crate) const fn as_str(&self) -> &'static str {
        match self {
            Self::UnknownType => "ACTIVITY_UNKNOWN_TYPE",
            Self::UnknownAction => "ACTIVITY_UNKNOWN_ACTION",
            Self::SchemaVersionUnsupported => "ACTIVITY_SCHEMA_VERSION_UNSUPPORTED",
            Self::InvalidConfig => "ACTIVITY_INVALID_CONFIG",
            Self::HandlerRejected => "ACTIVITY_HANDLER_REJECTED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityTypeError {
    pub(crate) code: ActivityTypeErrorCode,
    pub(crate) message: String,
}

impl ActivityTypeError {
    fn new(code: ActivityTypeErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ActivityTypeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ActivityTypeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlayerContext {
    pub(crate) character_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionDecision {
    pub(crate) action: String,
    pub(crate) accepted: bool,
    pub(crate) state_patch: Value,
    pub(crate) reward_intent: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransactionContext {
    pub(crate) request_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActionOutcome {
    pub(crate) action: String,
    pub(crate) applied: bool,
    pub(crate) result: Value,
}

pub(crate) trait ConfigValidator {
    fn validate_config(&self, config: &Value) -> Result<(), ActivityTypeError>;
}

pub(crate) trait PlayerViewBuilder {
    fn build_player_view(
        &self,
        activity: &Activity,
        version: &ActivityVersion,
        player_state: Option<&PlayerActivityState>,
    ) -> Result<Value, ActivityTypeError>;
}

pub(crate) trait ActionEvaluator {
    fn evaluate_action(
        &self,
        action: &str,
        context: &PlayerContext,
        player_state: Option<&PlayerActivityState>,
    ) -> Result<ActionDecision, ActivityTypeError>;
}

pub(crate) trait ActionApplier {
    fn apply_action(
        &self,
        decision: &ActionDecision,
        transaction: &mut TransactionContext,
    ) -> Result<ActionOutcome, ActivityTypeError>;
}

pub(crate) trait ActivityTypeHandler:
    ConfigValidator + PlayerViewBuilder + ActionEvaluator + ActionApplier + Send + Sync
{
    fn activity_type(&self) -> &'static str;
    fn schema_version(&self) -> i64;
    fn supported_actions(&self) -> &'static [&'static str];
}

#[derive(Default)]
pub(crate) struct ActivityTypeRegistry {
    handlers: BTreeMap<String, Arc<dyn ActivityTypeHandler>>,
}

impl ActivityTypeRegistry {
    pub(crate) fn with_defaults() -> Self {
        let mut registry = Self::default();
        registry
            .register(Arc::new(LoginRewardHandler::default()))
            .expect("default activity type registration");
        registry
            .register(Arc::new(LotteryHandler::default()))
            .expect("default activity type registration");
        registry
    }

    pub(crate) fn register(
        &mut self,
        handler: Arc<dyn ActivityTypeHandler>,
    ) -> Result<(), ActivityTypeError> {
        let name = handler.activity_type();
        if name.trim().is_empty() {
            return Err(ActivityTypeError::new(
                ActivityTypeErrorCode::InvalidConfig,
                "activity type name is empty",
            ));
        }
        if self.handlers.contains_key(name) {
            return Err(ActivityTypeError::new(
                ActivityTypeErrorCode::InvalidConfig,
                format!("activity type '{name}' is already registered"),
            ));
        }
        self.handlers.insert(name.to_string(), handler);
        Ok(())
    }

    pub(crate) fn get(
        &self,
        activity_type: &str,
    ) -> Result<&dyn ActivityTypeHandler, ActivityTypeError> {
        self.handlers
            .get(activity_type)
            .map(AsRef::as_ref)
            .ok_or_else(|| {
                ActivityTypeError::new(
                    ActivityTypeErrorCode::UnknownType,
                    format!("activity type '{activity_type}' is not registered"),
                )
            })
    }

    pub(crate) fn validate_config(
        &self,
        activity_type: &str,
        config: &Value,
    ) -> Result<(), ActivityTypeError> {
        let handler = self.get(activity_type)?;
        let version = config
            .get("schema_version")
            .and_then(Value::as_i64)
            .ok_or_else(|| {
                ActivityTypeError::new(
                    ActivityTypeErrorCode::SchemaVersionUnsupported,
                    "type config requires integer schema_version",
                )
            })?;
        if version != handler.schema_version() {
            return Err(ActivityTypeError::new(
                ActivityTypeErrorCode::SchemaVersionUnsupported,
                format!("schema version {version} is not supported for '{activity_type}'"),
            ));
        }
        handler.validate_config(config)
    }

    pub(crate) fn build_player_view(
        &self,
        activity: &Activity,
        version: &ActivityVersion,
        player_state: Option<&PlayerActivityState>,
    ) -> Result<Value, ActivityTypeError> {
        let handler = self.get(activity.activity_type.as_str())?;
        self.validate_config(activity.activity_type.as_str(), &version.type_config)?;
        handler.build_player_view(activity, version, player_state)
    }

    pub(crate) fn dispatch_action(
        &self,
        activity: &Activity,
        version: &ActivityVersion,
        action: &str,
        context: &PlayerContext,
        player_state: Option<&PlayerActivityState>,
        transaction: &mut TransactionContext,
    ) -> Result<ActionOutcome, ActivityTypeError> {
        let handler = self.get(activity.activity_type.as_str())?;
        self.validate_config(activity.activity_type.as_str(), &version.type_config)?;
        if !handler.supported_actions().contains(&action) {
            return Err(ActivityTypeError::new(
                ActivityTypeErrorCode::UnknownAction,
                format!(
                    "action '{action}' is not registered for '{}'",
                    activity.activity_type.as_str()
                ),
            ));
        }
        let decision = handler.evaluate_action(action, context, player_state)?;
        handler.apply_action(&decision, transaction)
    }
}

/// A contract-only handler result used by both initial registrations. It does
/// not calculate progress, randomness, or rewards.
pub(crate) fn contract_decision(action: &str) -> ActionDecision {
    ActionDecision {
        action: action.to_string(),
        accepted: false,
        state_patch: json!({}),
        reward_intent: json!([]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{Activity, ActivityScope, ActivityType, ActivityVersion};
    use chrono::{Duration, TimeZone, Utc};
    use serde_json::json;

    fn fixture() -> (Activity, ActivityVersion) {
        let start = Utc.with_ymd_and_hms(2026, 8, 21, 0, 0, 0).unwrap();
        let activity = Activity::new(
            "a1",
            "a1",
            ActivityType::new("login_reward").unwrap(),
            ActivityScope::Character,
            start,
            start + Duration::hours(1),
            start + Duration::hours(2),
            "UTC",
        )
        .unwrap();
        let version = ActivityVersion::draft(
            activity.id.clone(),
            1,
            json!({}),
            json!({"schema_version": 1, "event_source": "game_entry", "cycle_unit": "natural_day", "progression": "consecutive", "miss_policy": "reset", "claim_mode": "manual", "stages": [{"stage_no": 1, "required_count": 1, "reward_group_key": "g1"}]}),
            activity.start_at,
            activity.end_at,
            activity.claim_deadline,
            "UTC",
        )
        .unwrap();
        (activity, version)
    }

    #[test]
    fn defaults_register_both_types_without_rules() {
        fn assert_composed<
            T: ActivityTypeHandler
                + ConfigValidator
                + PlayerViewBuilder
                + ActionEvaluator
                + ActionApplier,
        >() {
        }
        assert_composed::<LoginRewardHandler>();
        assert_composed::<LotteryHandler>();
        let registry = ActivityTypeRegistry::with_defaults();
        assert!(registry.get("login_reward").is_ok());
        assert!(registry.get("lottery").is_ok());
    }

    #[test]
    fn duplicate_registration_does_not_replace_existing_handler() {
        let mut registry = ActivityTypeRegistry::with_defaults();
        let error = registry
            .register(Arc::new(LoginRewardHandler::default()))
            .unwrap_err();
        assert_eq!(error.code, ActivityTypeErrorCode::InvalidConfig);
        assert_eq!(
            registry.get("login_reward").unwrap().activity_type(),
            "login_reward"
        );
    }

    #[test]
    fn rejects_unknown_type_action_and_schema_version() {
        let registry = ActivityTypeRegistry::with_defaults();
        let missing = match registry.get("missing") {
            Ok(_) => panic!("missing type unexpectedly registered"),
            Err(error) => error,
        };
        assert_eq!(missing.code, ActivityTypeErrorCode::UnknownType);
        let (activity, mut version) = fixture();
        version.type_config = json!({"schema_version": 2});
        assert_eq!(
            registry
                .build_player_view(&activity, &version, None)
                .unwrap_err()
                .code,
            ActivityTypeErrorCode::SchemaVersionUnsupported
        );
        let (_, version) = fixture();
        let mut tx = TransactionContext {
            request_id: "r1".into(),
        };
        let error = registry
            .dispatch_action(
                &activity,
                &version,
                "draw",
                &PlayerContext {
                    character_id: "c1".into(),
                },
                None,
                &mut tx,
            )
            .unwrap_err();
        assert_eq!(error.code, ActivityTypeErrorCode::UnknownAction);
    }

    #[test]
    fn fake_handler_dispatches_contract_action_without_reward_rules() {
        let registry = ActivityTypeRegistry::with_defaults();
        let (activity, version) = fixture();
        let view = registry
            .build_player_view(&activity, &version, None)
            .unwrap();
        assert_eq!(view["type"], "login_reward");
        let mut tx = TransactionContext {
            request_id: "r1".into(),
        };
        let outcome = registry
            .dispatch_action(
                &activity,
                &version,
                "detail",
                &PlayerContext {
                    character_id: "c1".into(),
                },
                None,
                &mut tx,
            )
            .unwrap();
        assert!(!outcome.applied);
    }
}
