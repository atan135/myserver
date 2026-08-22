use super::{
    ACTIVITY_TYPE_SCHEMA_VERSION, ActionApplier, ActionDecision, ActionEvaluator, ActionOutcome,
    ActivityTypeError, ActivityTypeHandler, ConfigValidator, PlayerContext, PlayerViewBuilder,
    TransactionContext, contract_decision,
};
use crate::activity::{Activity, ActivityVersion, PlayerActivityState};
use crate::core::inventory::{AssetCommandErrorCode, AssetConsumption, NormalizedAssetItem};
use crate::core::reward_source::{AssetExchangeKind, InventoryRequiredExchange};
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) const LOTTERY_RANDOM_ALGORITHM_VERSION: &str = "os_rng_rejection_v1";

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

/// The server-selected source for one draw. A client never supplies this value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LotteryDrawCost {
    Free,
    Voucher { item_id: i32, quantity: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LotteryDrawDecision {
    /// State after a successful asset transaction. Callers must not persist this
    /// state until the corresponding transaction has been applied.
    pub(crate) next_state: LotteryState,
    pub(crate) period_key: String,
    pub(crate) cost: LotteryDrawCost,
}

/// Returns the activity-timezone natural-day key used by the daily limit.
pub(crate) fn lottery_period_key(
    occurred_at: DateTime<Utc>,
    timezone: &str,
) -> Result<String, ActivityTypeError> {
    let timezone: Tz = timezone
        .parse()
        .map_err(|_| invalid("lottery timezone is invalid"))?;
    Ok(timezone
        .from_utc_datetime(&occurred_at.naive_utc())
        .date_naive()
        .format("%Y-%m-%d")
        .to_string())
}

/// Applies the daily-period rollover without consuming a draw. This is kept
/// separate so detail/progress reads can normalize stale state safely.
pub(crate) fn normalize_lottery_state(
    config: &LotteryConfig,
    state: &LotteryState,
    period_key: &str,
) -> LotteryState {
    let mut normalized = state.clone();
    if normalized.pool_version != Some(config.pool_version) {
        normalized.pool_version = Some(config.pool_version);
        normalized.free_draws_remaining = config.free_draw_count;
        normalized.daily_draw_count = 0;
        normalized.total_draw_count = 0;
        normalized.last_draw_period_key = None;
        normalized.draw_request_id = None;
        normalized.result_item_id = None;
        normalized.result_state = None;
    }
    if normalized.last_draw_period_key.as_deref() != Some(period_key) {
        normalized.daily_draw_count = 0;
        normalized.last_draw_period_key = Some(period_key.to_string());
    }
    normalized
}

/// Checks lifecycle, limits and server-owned voucher inventory, then reserves
/// the next state in memory. Persist `next_state` only after the matching
/// inventory/reward transaction has returned `Applied`.
pub(crate) fn evaluate_lottery_draw(
    activity: &Activity,
    config: &LotteryConfig,
    state: &LotteryState,
    voucher_quantity: u32,
    now: DateTime<Utc>,
) -> Result<LotteryDrawDecision, ActivityTypeError> {
    if !matches!(
        activity.effective_status(now),
        crate::activity::ActivityStatus::Running
    ) {
        return Err(rejected("lottery activity is not running"));
    }
    let period_key = lottery_period_key(now, &activity.timezone)?;
    let mut next_state = normalize_lottery_state(config, state, &period_key);
    if config.daily_draw_limit > 0 && next_state.daily_draw_count >= config.daily_draw_limit {
        return Err(rejected("lottery daily draw limit reached"));
    }
    if config.total_draw_limit > 0 && next_state.total_draw_count >= config.total_draw_limit {
        return Err(rejected("lottery total draw limit reached"));
    }

    let cost = if next_state.free_draws_remaining > 0 {
        next_state.free_draws_remaining -= 1;
        LotteryDrawCost::Free
    } else {
        let item_id = config
            .voucher_item_id
            .ok_or_else(|| rejected("lottery draw qualification is exhausted"))?;
        if voucher_quantity == 0 {
            return Err(rejected("lottery voucher is unavailable"));
        }
        LotteryDrawCost::Voucher {
            item_id,
            quantity: 1,
        }
    };
    next_state.daily_draw_count = next_state.daily_draw_count.saturating_add(1);
    next_state.total_draw_count = next_state.total_draw_count.saturating_add(1);
    Ok(LotteryDrawDecision {
        next_state,
        period_key,
        cost,
    })
}

/// Commits the voucher path through the existing all-or-nothing inventory
/// exchange contract. `asset_uid` is resolved by the server from the player's
/// inventory; item IDs, quantities and results are never accepted from the
/// client. The returned command must be delivered before `next_state` is saved.
pub(crate) fn build_lottery_voucher_exchange(
    character_id: &str,
    exchange_id: &str,
    asset_uid: u64,
    voucher_item_id: i32,
    selection: &LotterySelection,
) -> Result<InventoryRequiredExchange, AssetCommandErrorCode> {
    if character_id.trim().is_empty() || exchange_id.trim().is_empty() || voucher_item_id <= 0 {
        return Err(AssetCommandErrorCode::InvalidRequest);
    }
    let reward = NormalizedAssetItem::new(
        selection.item_id,
        selection.quantity,
        crate::core::inventory::AssetBinding::Unbound,
    )
    .map_err(|_| AssetCommandErrorCode::InvalidItemCount)?;
    // The configured item ID is retained in the origin for audit/fingerprint
    // context; the authoritative stack identity is the server-resolved UID.
    let exchange_digest = format!(
        "{:x}",
        Sha256::digest(format!("lottery-voucher\0{exchange_id}\0{voucher_item_id}").as_bytes())
    );
    let exchange_key = format!("lottery:{}", &exchange_digest[..48]);
    InventoryRequiredExchange::new(
        AssetExchangeKind::Redemption,
        exchange_key,
        character_id.to_string(),
        vec![AssetConsumption {
            asset_uid,
            count: 1,
        }],
        vec![reward],
    )
    .map_err(|error| error)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LotteryPoolMetadata {
    pub(crate) pool_version: u32,
    pub(crate) total_weight: u64,
    pub(crate) weight_digest: String,
    pub(crate) random_algorithm_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LotterySelection {
    pub(crate) item_id: i32,
    pub(crate) quantity: u32,
    pub(crate) metadata: LotteryPoolMetadata,
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
        let parsed: LotteryConfig =
            serde_json::from_value(config.clone()).map_err(|error| ActivityTypeError {
                code: super::ActivityTypeErrorCode::InvalidConfig,
                message: format!("lottery config is invalid: {error}"),
            })?;
        validate_parsed_config(&parsed, self.schema_version()).map(|_| ())
    }
}

fn validate_parsed_config(
    config: &LotteryConfig,
    schema_version: i64,
) -> Result<u64, ActivityTypeError> {
    if config.schema_version != schema_version {
        return Err(ActivityTypeError {
            code: super::ActivityTypeErrorCode::SchemaVersionUnsupported,
            message: "lottery schema version is unsupported".into(),
        });
    }
    if config.draw_source != "player_action" || config.pool_version == 0 {
        return Err(invalid("draw_source/player pool version is invalid"));
    }
    if let Some(item_id) = config.voucher_item_id {
        if item_id <= 0 {
            return Err(invalid("voucher_item_id must be positive"));
        }
    }
    if config.pool_items.is_empty() {
        return Err(invalid("pool_items must not be empty"));
    }
    let mut ids = BTreeSet::new();
    let mut total = 0u64;
    for item in &config.pool_items {
        if item.item_id <= 0 || item.quantity == 0 || item.weight == 0 || !ids.insert(item.item_id)
        {
            return Err(invalid(
                "pool item id, quantity and weight must be positive and unique",
            ));
        }
        total = total
            .checked_add(item.weight)
            .ok_or_else(|| invalid("pool weights exceed u64"))?;
    }
    if total == 0 {
        return Err(invalid("pool total weight must be positive"));
    }
    Ok(total)
}

pub(crate) fn lottery_pool_weight_digest(config: &LotteryConfig) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"lottery-pool-weight-v1");
    hasher.update(config.pool_version.to_le_bytes());
    for item in &config.pool_items {
        hasher.update(item.item_id.to_le_bytes());
        hasher.update(item.quantity.to_le_bytes());
        hasher.update(item.weight.to_le_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

pub(crate) fn validate_lottery_reward_catalog(
    config: &LotteryConfig,
    available_item_ids: &BTreeSet<i32>,
) -> Result<(), ActivityTypeError> {
    validate_parsed_config(config, ACTIVITY_TYPE_SCHEMA_VERSION)?;
    for item in &config.pool_items {
        if !available_item_ids.contains(&item.item_id) {
            return Err(invalid(format!(
                "pool item {} is not present in the reward catalog",
                item.item_id
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_lottery_pool_version_frozen(
    previous: &LotteryConfig,
    current: &LotteryConfig,
) -> Result<(), ActivityTypeError> {
    validate_parsed_config(previous, ACTIVITY_TYPE_SCHEMA_VERSION)?;
    validate_parsed_config(current, ACTIVITY_TYPE_SCHEMA_VERSION)?;
    if previous.pool_version == current.pool_version
        && lottery_pool_weight_digest(previous) != lottery_pool_weight_digest(current)
    {
        return Err(invalid(
            "published lottery pool version cannot change weights or rewards",
        ));
    }
    Ok(())
}

pub(crate) fn select_lottery_item_with_random_word(
    config: &LotteryConfig,
    random_target: u64,
) -> Result<LotterySelection, ActivityTypeError> {
    let total_weight = validate_parsed_config(config, ACTIVITY_TYPE_SCHEMA_VERSION)?;
    if random_target >= total_weight {
        return Err(invalid("lottery random target must be below total weight"));
    }
    build_lottery_selection(config, total_weight, random_target)
}

fn build_lottery_selection(
    config: &LotteryConfig,
    total_weight: u64,
    target: u64,
) -> Result<LotterySelection, ActivityTypeError> {
    let mut cumulative = 0u64;
    for item in &config.pool_items {
        cumulative = cumulative
            .checked_add(item.weight)
            .ok_or_else(|| invalid("pool weights exceed u64"))?;
        if target < cumulative {
            return Ok(LotterySelection {
                item_id: item.item_id,
                quantity: item.quantity,
                metadata: LotteryPoolMetadata {
                    pool_version: config.pool_version,
                    total_weight,
                    weight_digest: lottery_pool_weight_digest(config),
                    random_algorithm_version: LOTTERY_RANDOM_ALGORITHM_VERSION.to_string(),
                },
            });
        }
    }
    Err(invalid("weighted pool selection did not resolve an item"))
}

fn secure_random_target(total_weight: u64) -> Result<u64, ActivityTypeError> {
    let acceptance_zone = u64::MAX - (u64::MAX % total_weight);
    loop {
        let mut bytes = [0u8; 8];
        getrandom::getrandom(&mut bytes).map_err(|error| ActivityTypeError {
            code: super::ActivityTypeErrorCode::HandlerRejected,
            message: format!("secure lottery random source unavailable: {error}"),
        })?;
        let random_word = u64::from_le_bytes(bytes);
        if random_word < acceptance_zone {
            return Ok(random_word % total_weight);
        }
    }
}

pub(crate) fn draw_lottery_item(
    config: &LotteryConfig,
) -> Result<LotterySelection, ActivityTypeError> {
    let total_weight = validate_parsed_config(config, ACTIVITY_TYPE_SCHEMA_VERSION)?;
    let target = secure_random_target(total_weight)?;
    build_lottery_selection(config, total_weight, target)
}

impl PlayerViewBuilder for LotteryHandler {
    fn build_player_view(
        &self,
        activity: &Activity,
        _version: &ActivityVersion,
        player_state: Option<&PlayerActivityState>,
    ) -> Result<Value, ActivityTypeError> {
        let config: LotteryConfig = serde_json::from_value(_version.type_config.clone())
            .map_err(|error| invalid(error.to_string()))?;
        let state = player_state
            .and_then(|value| serde_json::from_value::<LotteryState>(value.type_state.clone()).ok())
            .unwrap_or_default();
        Ok(
            json!({"type": activity.activity_type.as_str(), "schema_version": self.schema_version(), "draw_source": config.draw_source, "pool_version": config.pool_version, "free_draw_count": config.free_draw_count, "daily_draw_limit": config.daily_draw_limit, "total_draw_limit": config.total_draw_limit, "pool_total_weight": config.pool_items.iter().map(|item| item.weight).sum::<u64>(), "state": state, "contract_only": true}),
        )
    }
}

fn invalid(message: impl Into<String>) -> ActivityTypeError {
    ActivityTypeError {
        code: super::ActivityTypeErrorCode::InvalidConfig,
        message: message.into(),
    }
}

fn rejected(message: impl Into<String>) -> ActivityTypeError {
    ActivityTypeError {
        code: super::ActivityTypeErrorCode::HandlerRejected,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config() -> Value {
        json!({"schema_version": 1, "draw_source": "player_action", "pool_version": 3, "free_draw_count": 2, "voucher_item_id": 9001, "daily_draw_limit": 10, "total_draw_limit": 100, "pool_items": [{"item_id": 1001, "quantity": 1, "weight": 3}, {"item_id": 1002, "quantity": 2, "weight": 7}]})
    }

    #[test]
    fn validates_integer_weight_pool_and_builds_contract_view() {
        let handler = LotteryHandler::default();
        handler.validate_config(&config()).unwrap();
        let activity = Activity::new(
            "a1",
            "a1",
            crate::activity::ActivityType::new("lottery").unwrap(),
            crate::activity::ActivityScope::Character,
            chrono::Utc::now(),
            chrono::Utc::now() + chrono::Duration::hours(1),
            chrono::Utc::now() + chrono::Duration::hours(2),
            "UTC",
        )
        .unwrap();
        let mut activity = activity;
        activity.status = crate::activity::ActivityStatus::Running;
        let version = ActivityVersion::draft(
            activity.id.clone(),
            1,
            json!({}),
            config(),
            activity.start_at,
            activity.end_at,
            activity.claim_deadline,
            "UTC",
        )
        .unwrap();
        let view = handler
            .build_player_view(&activity, &version, None)
            .unwrap();
        assert_eq!(view["pool_total_weight"], 10);
        assert_eq!(view["contract_only"], true);
    }

    #[test]
    fn rejects_invalid_pool_and_client_result_fields() {
        let handler = LotteryHandler::default();
        let mut empty = config();
        empty["pool_items"] = json!([]);
        assert_eq!(
            handler.validate_config(&empty).unwrap_err().code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
        let mut bad = config();
        bad["pool_items"][0]["weight"] = json!(0);
        assert_eq!(
            handler.validate_config(&bad).unwrap_err().code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
        let mut duplicate = config();
        duplicate["pool_items"][1]["item_id"] = json!(1001);
        assert_eq!(
            handler.validate_config(&duplicate).unwrap_err().code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
        let mut result = config();
        result["result_item_id"] = json!(1001);
        assert_eq!(
            handler.validate_config(&result).unwrap_err().code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
        let mut negative = config();
        negative["pool_items"][0]["weight"] = json!(-1);
        assert_eq!(
            handler.validate_config(&negative).unwrap_err().code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
    }

    #[test]
    fn draw_handler_is_contract_only_and_never_returns_client_result() {
        let decision = LotteryHandler::default()
            .evaluate_action(
                "draw",
                &PlayerContext {
                    character_id: "c1".into(),
                },
                None,
            )
            .unwrap();
        let outcome = LotteryHandler::default()
            .apply_action(
                &decision,
                &mut TransactionContext {
                    request_id: "r1".into(),
                },
            )
            .unwrap();
        assert!(!outcome.applied);
        assert_eq!(outcome.result["contract_only"], true);
        assert!(outcome.result.get("result_item_id").is_none());
    }

    #[test]
    fn weighted_selection_uses_integer_boundaries_and_records_metadata() {
        let parsed: LotteryConfig = serde_json::from_value(config()).unwrap();
        let first = select_lottery_item_with_random_word(&parsed, 0).unwrap();
        let first_boundary = select_lottery_item_with_random_word(&parsed, 2).unwrap();
        let second = select_lottery_item_with_random_word(&parsed, 3).unwrap();
        assert_eq!(first.item_id, 1001);
        assert_eq!(first_boundary.item_id, 1001);
        assert_eq!(second.item_id, 1002);
        assert_eq!(second.metadata.total_weight, 10);
        assert_eq!(second.metadata.pool_version, 3);
        assert_eq!(
            second.metadata.random_algorithm_version,
            LOTTERY_RANDOM_ALGORITHM_VERSION
        );
        assert!(second.metadata.weight_digest.starts_with("sha256:"));
        assert_eq!(
            select_lottery_item_with_random_word(&parsed, 10)
                .unwrap_err()
                .code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
    }

    #[test]
    fn weighted_selection_has_expected_integer_distribution() {
        let parsed: LotteryConfig = serde_json::from_value(config()).unwrap();
        let mut counts = BTreeSet::new();
        let mut first_count = 0usize;
        let mut second_count = 0usize;
        for random_word in 0..1000u64 {
            let selection =
                select_lottery_item_with_random_word(&parsed, random_word % 10).unwrap();
            counts.insert(selection.item_id);
            if selection.item_id == 1001 {
                first_count += 1;
            } else {
                second_count += 1;
            }
        }
        assert_eq!(counts, BTreeSet::from([1001, 1002]));
        assert_eq!(first_count, 300);
        assert_eq!(second_count, 700);
    }

    #[test]
    fn pool_version_freezes_weight_digest_and_catalog_references() {
        let previous: LotteryConfig = serde_json::from_value(config()).unwrap();
        let mut changed = previous.clone();
        changed.pool_items[0].weight = 4;
        assert_eq!(
            validate_lottery_pool_version_frozen(&previous, &changed)
                .unwrap_err()
                .code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
        changed.pool_version = previous.pool_version + 1;
        validate_lottery_pool_version_frozen(&previous, &changed).unwrap();

        let catalog = BTreeSet::from([1001, 1002]);
        validate_lottery_reward_catalog(&previous, &catalog).unwrap();
        let missing = BTreeSet::from([1001]);
        assert_eq!(
            validate_lottery_reward_catalog(&previous, &missing)
                .unwrap_err()
                .code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
    }

    #[test]
    fn rejects_weight_sum_overflow_before_selection() {
        let mut value = config();
        value["pool_items"][0]["weight"] = json!(u64::MAX);
        value["pool_items"][1]["weight"] = json!(1);
        let parsed: LotteryConfig = serde_json::from_value(value).unwrap();
        assert_eq!(
            select_lottery_item_with_random_word(&parsed, 0)
                .unwrap_err()
                .code,
            super::super::ActivityTypeErrorCode::InvalidConfig
        );
    }

    #[test]
    fn secure_draw_returns_only_server_selection_metadata() {
        let parsed: LotteryConfig = serde_json::from_value(config()).unwrap();
        let selection = draw_lottery_item(&parsed).unwrap();
        assert!([1001, 1002].contains(&selection.item_id));
        assert_eq!(selection.metadata.total_weight, 10);
        assert_eq!(selection.metadata.pool_version, 3);
    }

    #[test]
    fn qualification_prefers_free_then_server_owned_voucher() {
        let parsed: LotteryConfig = serde_json::from_value(config()).unwrap();
        let activity = Activity::new(
            "a1",
            "a1",
            crate::activity::ActivityType::new("lottery").unwrap(),
            crate::activity::ActivityScope::Character,
            chrono::Utc::now() - chrono::Duration::hours(1),
            chrono::Utc::now() + chrono::Duration::hours(1),
            chrono::Utc::now() + chrono::Duration::hours(2),
            "UTC",
        )
        .unwrap();
        let mut activity = activity;
        activity.status = crate::activity::ActivityStatus::Running;
        let now = chrono::Utc::now();
        let first =
            evaluate_lottery_draw(&activity, &parsed, &LotteryState::default(), 0, now).unwrap();
        assert_eq!(first.cost, LotteryDrawCost::Free);
        assert_eq!(first.next_state.free_draws_remaining, 1);

        let mut exhausted = first.next_state.clone();
        exhausted.free_draws_remaining = 0;
        let voucher = evaluate_lottery_draw(&activity, &parsed, &exhausted, 1, now).unwrap();
        assert_eq!(
            voucher.cost,
            LotteryDrawCost::Voucher {
                item_id: 9001,
                quantity: 1
            }
        );
        assert_eq!(voucher.next_state.total_draw_count, 2);
    }

    #[test]
    fn qualification_resets_daily_count_by_activity_timezone_and_enforces_limits() {
        let mut value = config();
        value["free_draw_count"] = json!(0);
        value["daily_draw_limit"] = json!(1);
        value["total_draw_limit"] = json!(2);
        let parsed: LotteryConfig = serde_json::from_value(value).unwrap();
        let start = chrono::Utc.with_ymd_and_hms(2026, 8, 21, 15, 0, 0).unwrap();
        let activity = Activity::new(
            "a1",
            "a1",
            crate::activity::ActivityType::new("lottery").unwrap(),
            crate::activity::ActivityScope::Character,
            start,
            start + chrono::Duration::days(2),
            start + chrono::Duration::days(3),
            "Asia/Shanghai",
        )
        .unwrap();
        let mut activity = activity;
        activity.status = crate::activity::ActivityStatus::Running;
        let state = LotteryState {
            pool_version: Some(3),
            voucher_count: 0,
            daily_draw_count: 1,
            total_draw_count: 1,
            last_draw_period_key: Some("2026-08-21".into()),
            ..Default::default()
        };
        let next_day = start + chrono::Duration::hours(10);
        let decision = evaluate_lottery_draw(&activity, &parsed, &state, 1, next_day).unwrap();
        assert_eq!(decision.period_key, "2026-08-22");
        assert_eq!(decision.next_state.daily_draw_count, 1);
        assert_eq!(decision.next_state.total_draw_count, 2);
        assert!(
            evaluate_lottery_draw(&activity, &parsed, &decision.next_state, 1, next_day).is_err()
        );
    }

    #[test]
    fn qualification_rejects_activity_end_and_missing_voucher() {
        let parsed: LotteryConfig = serde_json::from_value(config()).unwrap();
        let start = chrono::Utc::now() - chrono::Duration::hours(2);
        let activity = Activity::new(
            "a1",
            "a1",
            crate::activity::ActivityType::new("lottery").unwrap(),
            crate::activity::ActivityScope::Character,
            start,
            start + chrono::Duration::hours(1),
            start + chrono::Duration::hours(2),
            "UTC",
        )
        .unwrap();
        let mut state = LotteryState::default();
        state.free_draws_remaining = 0;
        let now = start + chrono::Duration::hours(1);
        assert!(evaluate_lottery_draw(&activity, &parsed, &state, 0, now).is_err());
        assert!(
            evaluate_lottery_draw(
                &activity,
                &parsed,
                &state,
                0,
                now + chrono::Duration::seconds(1)
            )
            .is_err()
        );
    }

    #[test]
    fn voucher_exchange_contains_consume_and_grant_in_one_batch() {
        let parsed: LotteryConfig = serde_json::from_value(config()).unwrap();
        let selection = select_lottery_item_with_random_word(&parsed, 0).unwrap();
        let exchange =
            build_lottery_voucher_exchange("c1", "draw-1", 42, 9001, &selection).unwrap();
        assert_eq!(
            exchange.delivery_policy,
            crate::core::inventory::RewardDeliveryPolicy::InventoryRequired
        );
        assert_eq!(exchange.command.operations.len(), 2);
        assert!(matches!(
            exchange.command.operations[0],
            crate::core::inventory::AssetOperation::Consume { .. }
        ));
        assert!(matches!(
            exchange.command.operations[1],
            crate::core::inventory::AssetOperation::Grant { .. }
        ));
    }

    #[test]
    fn voucher_exchange_identifiers_are_bounded_for_long_draw_ids() {
        let parsed: LotteryConfig = serde_json::from_value(config()).unwrap();
        let selection = select_lottery_item_with_random_word(&parsed, 0).unwrap();
        let exchange =
            build_lottery_voucher_exchange("c1", &"draw:".repeat(256), 42, 9001, &selection)
                .unwrap();

        assert!(exchange.command.request_id.len() <= 128);
        assert!(exchange.command.origin.origin_id.len() <= 128);
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
