use super::{
    ACTIVITY_TYPE_SCHEMA_VERSION, ActionApplier, ActionDecision, ActionEvaluator, ActionOutcome,
    ActivityTypeError, ActivityTypeHandler, ConfigValidator, PlayerContext, PlayerViewBuilder,
    TransactionContext, contract_decision,
};
use crate::activity::{Activity, ActivityVersion, PlayerActivityState};
use serde_json::{Value, json};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
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

fn invalid(message: impl Into<String>) -> ActivityTypeError {
    ActivityTypeError { code: super::ActivityTypeErrorCode::InvalidConfig, message: message.into() }
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
        let parsed: LoginRewardConfig = serde_json::from_value(config.clone()).map_err(|error| invalid(format!("login_reward config is invalid: {error}")))?;
        if parsed.schema_version != self.schema_version() { return Err(invalid("login_reward schema version is unsupported")); }
        if parsed.event_source != "game_entry" { return Err(invalid("event_source must be game_entry")); }
        if parsed.cycle_unit != "natural_day" { return Err(invalid("cycle_unit must be natural_day")); }
        if !matches!(parsed.progression.as_str(), "consecutive" | "cumulative") { return Err(invalid("progression must be consecutive or cumulative")); }
        if !matches!(parsed.miss_policy.as_str(), "reset" | "carry") { return Err(invalid("miss_policy must be reset or carry")); }
        if !matches!(parsed.claim_mode.as_str(), "manual" | "automatic") { return Err(invalid("claim_mode must be manual or automatic")); }
        if parsed.stages.is_empty() { return Err(invalid("stages must not be empty")); }
        let mut stage_nos = std::collections::BTreeSet::new();
        for stage in &parsed.stages {
            if stage.stage_no == 0 || stage.required_count == 0 || stage.reward_group_key.trim().is_empty() { return Err(invalid("stage_no, required_count and reward_group_key are required")); }
            if !stage_nos.insert(stage.stage_no) { return Err(invalid("stage_no must be unique")); }
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
        let config: LoginRewardConfig = serde_json::from_value(_version.type_config.clone()).map_err(|error| invalid(error.to_string()))?;
        Ok(json!({"type": activity.activity_type.as_str(), "schema_version": self.schema_version(), "contract_only": true, "event_source": config.event_source, "cycle_unit": config.cycle_unit, "progression": config.progression, "miss_policy": config.miss_policy, "claim_mode": config.claim_mode, "stages": config.stages}))
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
        LoginRewardHandler::default().validate_config(&config()).unwrap();
    }

    #[test]
    fn rejects_unknown_source_and_duplicate_stage_numbers() {
        let handler = LoginRewardHandler::default();
        let mut bad_source = config();
        bad_source["event_source"] = json!("client");
        assert_eq!(handler.validate_config(&bad_source).unwrap_err().code, super::super::ActivityTypeErrorCode::InvalidConfig);
        let mut duplicate = config();
        duplicate["stages"] = json!([{ "stage_no": 1, "required_count": 1, "reward_group_key": "g1" }, { "stage_no": 1, "required_count": 2, "reward_group_key": "g2" }]);
        assert_eq!(handler.validate_config(&duplicate).unwrap_err().code, super::super::ActivityTypeErrorCode::InvalidConfig);
    }
}
