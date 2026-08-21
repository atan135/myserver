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
pub(crate) struct LotteryPoolItemConfig {
    pub(crate) item_id: i32,
    pub(crate) quantity: u32,
    pub(crate) weight: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LotteryExtensionConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) threshold: Option<u32>,
    pub(crate) stock: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LotteryConfig {
    pub(crate) schema_version: i64,
    pub(crate) draw_source: String,
    pub(crate) pool_version: u32,
    pub(crate) free_draw_count: u32,
    pub(crate) voucher_item_id: Option<i32>,
    pub(crate) daily_draw_limit: u32,
    pub(crate) total_draw_limit: u32,
    pub(crate) pool_items: Vec<LotteryPoolItemConfig>,
    pub(crate) pity: Option<LotteryExtensionConfig>,
    pub(crate) limited_stock: Option<LotteryExtensionConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LotteryState {
    pub(crate) free_draws_remaining: u32,
    pub(crate) voucher_count: u32,
    pub(crate) daily_draw_count: u32,
    pub(crate) total_draw_count: u32,
    pub(crate) last_draw_period_key: Option<String>,
    pub(crate) pool_version: Option<u32>,
    pub(crate) draw_request_id: Option<String>,
    pub(crate) result_item_id: Option<i32>,
    pub(crate) result_state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LotteryDrawResult {
    pub(crate) accepted: bool,
    pub(crate) contract_only: bool,
    pub(crate) state: LotteryState,
}

#[derive(Default)]
pub(crate) struct LotteryHandler;

impl ConfigValidator for LotteryHandler {
    fn validate_config(&self, config: &Value) -> Result<(), ActivityTypeError> {
        if !config.is_object() {
            return Err(ActivityTypeError {
                code: super::ActivityTypeErrorCode::InvalidConfig,
                message: "type config must be an object".into(),
            });
        }
        let parsed: LotteryConfig = serde_json::from_value(config.clone()).map_err(|error| ActivityTypeError { code: super::ActivityTypeErrorCode::InvalidConfig, message: format!("lottery config is invalid: {error}") })?;
        if parsed.schema_version != self.schema_version() { return Err(ActivityTypeError { code: super::ActivityTypeErrorCode::SchemaVersionUnsupported, message: "lottery schema version is unsupported".into() }); }
        if parsed.draw_source != "player_action" || parsed.pool_version == 0 { return Err(invalid("draw_source/player pool version is invalid")); }
        if let Some(item_id) = parsed.voucher_item_id { if item_id <= 0 { return Err(invalid("voucher_item_id must be positive")); } }
        if parsed.pool_items.is_empty() { return Err(invalid("pool_items must not be empty")); }
        let mut ids = std::collections::BTreeSet::new();
        let mut total = 0u64;
        for item in parsed.pool_items { if item.item_id <= 0 || item.quantity == 0 || item.weight == 0 || !ids.insert(item.item_id) { return Err(invalid("pool item id, quantity and weight must be positive and unique")); } total = total.checked_add(item.weight).ok_or_else(|| invalid("pool weights exceed u64"))?; }
        if total == 0 { return Err(invalid("pool total weight must be positive")); }
        Ok(())
    }
}

impl PlayerViewBuilder for LotteryHandler {
    fn build_player_view(
        &self,
        activity: &Activity,
        _version: &ActivityVersion,
        player_state: Option<&PlayerActivityState>,
    ) -> Result<Value, ActivityTypeError> {
        let config: LotteryConfig = serde_json::from_value(_version.type_config.clone()).map_err(|error| invalid(error.to_string()))?;
        let state = player_state.and_then(|value| serde_json::from_value::<LotteryState>(value.type_state.clone()).ok()).unwrap_or_default();
        Ok(json!({"type": activity.activity_type.as_str(), "schema_version": self.schema_version(), "draw_source": config.draw_source, "pool_version": config.pool_version, "free_draw_count": config.free_draw_count, "daily_draw_limit": config.daily_draw_limit, "total_draw_limit": config.total_draw_limit, "pool_total_weight": config.pool_items.iter().map(|item| item.weight).sum::<u64>(), "state": state, "contract_only": true}))
    }
}

fn invalid(message: impl Into<String>) -> ActivityTypeError { ActivityTypeError { code: super::ActivityTypeErrorCode::InvalidConfig, message: message.into() } }

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> Value { json!({"schema_version": 1, "draw_source": "player_action", "pool_version": 3, "free_draw_count": 2, "voucher_item_id": 9001, "daily_draw_limit": 10, "total_draw_limit": 100, "pool_items": [{"item_id": 1001, "quantity": 1, "weight": 3}, {"item_id": 1002, "quantity": 2, "weight": 7}]}) }

    #[test]
    fn validates_integer_weight_pool_and_builds_contract_view() {
        let handler = LotteryHandler::default();
        handler.validate_config(&config()).unwrap();
        let activity = Activity::new("a1", "a1", crate::activity::ActivityType::new("lottery").unwrap(), crate::activity::ActivityScope::Character, chrono::Utc::now(), chrono::Utc::now() + chrono::Duration::hours(1), chrono::Utc::now() + chrono::Duration::hours(2), "UTC").unwrap();
        let version = ActivityVersion::draft(activity.id.clone(), 1, json!({}), config(), activity.start_at, activity.end_at, activity.claim_deadline, "UTC").unwrap();
        let view = handler.build_player_view(&activity, &version, None).unwrap();
        assert_eq!(view["pool_total_weight"], 10);
        assert_eq!(view["contract_only"], true);
    }

    #[test]
    fn rejects_invalid_pool_and_client_result_fields() {
        let handler = LotteryHandler::default();
        let mut bad = config(); bad["pool_items"][0]["weight"] = json!(0);
        assert_eq!(handler.validate_config(&bad).unwrap_err().code, super::super::ActivityTypeErrorCode::InvalidConfig);
        let mut duplicate = config(); duplicate["pool_items"][1]["item_id"] = json!(1001);
        assert_eq!(handler.validate_config(&duplicate).unwrap_err().code, super::super::ActivityTypeErrorCode::InvalidConfig);
        let mut result = config(); result["result_item_id"] = json!(1001);
        assert_eq!(handler.validate_config(&result).unwrap_err().code, super::super::ActivityTypeErrorCode::InvalidConfig);
    }

    #[test]
    fn draw_handler_is_contract_only_and_never_returns_client_result() {
        let decision = LotteryHandler::default().evaluate_action("draw", &PlayerContext { character_id: "c1".into() }, None).unwrap();
        let outcome = LotteryHandler::default().apply_action(&decision, &mut TransactionContext { request_id: "r1".into() }).unwrap();
        assert!(!outcome.applied);
        assert_eq!(outcome.result["contract_only"], true);
        assert!(outcome.result.get("result_item_id").is_none());
    }
}

impl ActionEvaluator for LotteryHandler {
    fn evaluate_action(
        &self,
        action: &str,
        _context: &PlayerContext,
        _player_state: Option<&PlayerActivityState>,
    ) -> Result<ActionDecision, ActivityTypeError> {
        Ok(contract_decision(action))
    }
}

impl ActionApplier for LotteryHandler {
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

impl ActivityTypeHandler for LotteryHandler {
    fn activity_type(&self) -> &'static str {
        "lottery"
    }
    fn schema_version(&self) -> i64 {
        ACTIVITY_TYPE_SCHEMA_VERSION
    }
    fn supported_actions(&self) -> &'static [&'static str] {
        &["list", "detail", "draw", "progress"]
    }
}
