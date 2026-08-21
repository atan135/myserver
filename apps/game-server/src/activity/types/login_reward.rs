use super::{
    ACTIVITY_TYPE_SCHEMA_VERSION, ActionApplier, ActionDecision, ActionEvaluator, ActionOutcome,
    ActivityTypeError, ActivityTypeHandler, ConfigValidator, PlayerContext, PlayerViewBuilder,
    TransactionContext, contract_decision,
};
use crate::activity::{Activity, ActivityVersion, PlayerActivityState};
use serde_json::{Value, json};

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
        Ok(())
    }
}

impl PlayerViewBuilder for LoginRewardHandler {
    fn build_player_view(
        &self,
        activity: &Activity,
        _version: &ActivityVersion,
        _player_state: Option<&PlayerActivityState>,
    ) -> Result<Value, ActivityTypeError> {
        Ok(
            json!({"type": activity.activity_type.as_str(), "schema_version": self.schema_version(), "contract_only": true}),
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
