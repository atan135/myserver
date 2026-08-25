//! Public activity domain contracts.
//!
//! This module owns activity configuration snapshots, lifecycle and player
//! activity facts. It deliberately does not contain login or lottery rules;
//! those belong to registered type handlers in a later phase.

mod cache;
mod domain;
mod engine;
mod history;
mod repository;
mod settlement;
mod types;

use crate::core::context::ConnectionContext;
use crate::pb::{
    ActivityActionReq, ActivityActionRes, ActivityClaimHistoryRecord, ActivityClaimHistoryReq,
    ActivityClaimHistoryRes, ActivityClaimReq, ActivityClaimRes, ActivityDetailReq,
    ActivityDetailRes, ActivityListReq, ActivityListRes, ActivityProgressReq, ActivityProgressRes,
    ActivityRewardSummary,
};
use crate::protocol::{MessageType, Packet};
use chrono::Utc;

#[allow(unused_imports)]
pub(crate) use cache::{
    ActivityCache, ActivityCacheError, ActivityCacheKey, InMemoryActivityCache, RedisActivityCache,
    RefreshNotice,
};
#[allow(unused_imports)]
pub(crate) use domain::{
    Activity, ActivityDomainError, ActivityErrorCode, ActivityScope, ActivityStage, ActivityStatus,
    ActivityType, ActivityVersion, ClaimRecord, PlayerActivityState, RewardGroup, RewardItem,
};
#[allow(unused_imports)]
pub(crate) use engine::{
    ActivityActionRequest, ActivityActionResponse, ActivityEngine, ActivityRequestContext,
    PlayerManagerLotteryAssetGateway,
};
#[allow(unused_imports)]
pub(crate) use history::{
    ActivityClaimHistoryPage, ActivityClaimHistoryStore, ActivityHistoryCursor,
    InMemoryActivityClaimHistoryStore, PgActivityClaimHistoryStore,
};
#[allow(unused_imports)]
pub(crate) use repository::{
    ActivityRepository, ActivityRepositoryError, InMemoryActivityRepository,
    PublishedActivitySnapshot,
};
#[allow(unused_imports)]
pub(crate) use settlement::{
    ActivityClaimCoordinator, ActivityClaimRecord, ActivityDeliveryOutcome, ActivityRewardDelivery,
    ClaimSettlement, ClaimStatus, build_reward_order, stable_reward_request_id,
};
#[allow(unused_imports)]
pub(crate) use types::{
    ActionApplier, ActionDecision, ActionEvaluator, ActionOutcome, ActivityTypeError,
    ActivityTypeErrorCode, ActivityTypeHandler, ActivityTypeRegistry, ConfigValidator,
    PlayerContext, PlayerViewBuilder, TransactionContext,
};
#[allow(unused_imports)]
pub(crate) use types::{GameEntryEvent, LoginRewardProgressError, LoginRewardProgressResult};

async fn read_snapshot_for_message(
    engine: &ActivityEngine,
    request_context: &ActivityRequestContext,
    message_type: MessageType,
    activity_id: &str,
    version: u32,
    now: chrono::DateTime<Utc>,
) -> Result<PublishedActivitySnapshot, engine::ActivityEngineError> {
    match message_type {
        MessageType::ActivityDetailReq => {
            engine
                .detail_with_context(request_context, activity_id, version, now)
                .await
        }
        MessageType::ActivityProgressReq => {
            engine
                .progress_with_context(request_context, activity_id, version, now)
                .await
        }
        _ => unreachable!("activity read routing only accepts detail or progress"),
    }
}

pub(crate) async fn handle_packet(
    engine: &ActivityEngine,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), std::io::Error> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };
    let request_context = ActivityRequestContext::authenticated(
        identity.character_id(),
        identity.account_player_id(),
        &connection.peer_addr,
        connection.session.credential_id.as_deref(),
        connection.session.device_subject.as_deref(),
    );
    let now = Utc::now();
    match packet.message_type() {
        Some(MessageType::ActivityListReq) => {
            if let Err(error_code) =
                packet.decode_body::<ActivityListReq>("ACTIVITY_INVALID_REQUEST")
            {
                connection.queue_error(
                    packet.header.seq,
                    error_code,
                    "invalid activity request body",
                )?;
                return Ok(());
            }
            let response = match engine.list_with_context(&request_context, now).await {
                Ok(snapshots) => ActivityListRes {
                    ok: true,
                    error_code: String::new(),
                    server_time_ms: now.timestamp_millis(),
                    activities: snapshots
                        .into_iter()
                        .map(|snapshot| summary(&snapshot, now))
                        .collect(),
                },
                Err(error) => ActivityListRes {
                    ok: false,
                    error_code: error.code.to_string(),
                    server_time_ms: now.timestamp_millis(),
                    activities: Vec::new(),
                },
            };
            connection.queue_message(MessageType::ActivityListRes, packet.header.seq, response)
        }
        Some(MessageType::ActivityClaimHistoryReq) => {
            let request =
                match packet.decode_body::<ActivityClaimHistoryReq>("ACTIVITY_INVALID_REQUEST") {
                    Ok(value) => value,
                    Err(error_code) => {
                        connection.queue_error(
                            packet.header.seq,
                            error_code,
                            "invalid activity history request body",
                        )?;
                        return Ok(());
                    }
                };
            let response = match engine
                .claim_history_with_context(&request_context, &request.cursor, request.limit)
                .await
            {
                Ok(page) => ActivityClaimHistoryRes {
                    ok: true,
                    error_code: String::new(),
                    records: page
                        .records
                        .into_iter()
                        .map(|record| ActivityClaimHistoryRecord {
                            activity_id: record.activity_id,
                            version: record.version.max(0) as u32,
                            activity_type: record.activity_type,
                            action_type: record.action_type,
                            stage_id: record.stage_id.unwrap_or_default(),
                            created_at_ms: record.created_at.timestamp_millis(),
                            completed_at_ms: record
                                .completed_at
                                .map(|value| value.timestamp_millis())
                                .unwrap_or_default(),
                            status: record.status,
                            rewards: record
                                .rewards
                                .into_iter()
                                .map(|reward| ActivityRewardSummary {
                                    reward_type: reward.reward_type,
                                    asset_id: reward.asset_id,
                                    quantity: reward.quantity,
                                })
                                .collect(),
                        })
                        .collect(),
                    next_cursor: page
                        .next_cursor
                        .as_ref()
                        .map(|cursor| engine.encode_history_cursor(identity.character_id(), cursor))
                        .unwrap_or_default(),
                    has_more: page.has_more,
                },
                Err(error) => ActivityClaimHistoryRes {
                    ok: false,
                    error_code: error.code.into(),
                    records: Vec::new(),
                    next_cursor: String::new(),
                    has_more: false,
                },
            };
            connection.queue_message(
                MessageType::ActivityClaimHistoryRes,
                packet.header.seq,
                response,
            )
        }
        Some(MessageType::ActivityDetailReq) => {
            let request = match packet.decode_body::<ActivityDetailReq>("ACTIVITY_INVALID_REQUEST")
            {
                Ok(value) => value,
                Err(error_code) => {
                    connection.queue_error(
                        packet.header.seq,
                        error_code,
                        "invalid activity request body",
                    )?;
                    return Ok(());
                }
            };
            let response = match read_snapshot_for_message(
                engine,
                &request_context,
                packet
                    .message_type()
                    .expect("matched activity detail request"),
                &request.activity_id,
                request.version,
                now,
            )
            .await
            {
                Ok(snapshot) => ActivityDetailRes {
                    ok: true,
                    error_code: String::new(),
                    activity: Some(summary(&snapshot, now)),
                    progress_json: engine
                        .player_view_json(identity.character_id(), &snapshot, now)
                        .await
                        .ok()
                        .and_then(|view| serde_json::to_string(&view).ok())
                        .unwrap_or_else(|| "{}".into()),
                    state_revision: 0,
                },
                Err(error) => ActivityDetailRes {
                    ok: false,
                    error_code: error.code.into(),
                    activity: None,
                    progress_json: String::new(),
                    state_revision: 0,
                },
            };
            connection.queue_message(MessageType::ActivityDetailRes, packet.header.seq, response)
        }
        Some(MessageType::ActivityProgressReq) => {
            let request =
                match packet.decode_body::<ActivityProgressReq>("ACTIVITY_INVALID_REQUEST") {
                    Ok(value) => value,
                    Err(error_code) => {
                        connection.queue_error(
                            packet.header.seq,
                            error_code,
                            "invalid activity request body",
                        )?;
                        return Ok(());
                    }
                };
            let response = match read_snapshot_for_message(
                engine,
                &request_context,
                packet
                    .message_type()
                    .expect("matched activity progress request"),
                &request.activity_id,
                request.version,
                now,
            )
            .await
            {
                Ok(snapshot) => ActivityProgressRes {
                    ok: true,
                    error_code: String::new(),
                    activity_id: snapshot.activity.id,
                    version: snapshot.version.version_no as u32,
                    progress_json: "{}".into(),
                    state_revision: 0,
                },
                Err(error) => ActivityProgressRes {
                    ok: false,
                    error_code: error.code.into(),
                    activity_id: request.activity_id,
                    version: request.version,
                    progress_json: String::new(),
                    state_revision: 0,
                },
            };
            connection.queue_message(
                MessageType::ActivityProgressRes,
                packet.header.seq,
                response,
            )
        }
        Some(MessageType::ActivityClaimReq) => {
            let request = match packet.decode_body::<ActivityClaimReq>("ACTIVITY_INVALID_REQUEST") {
                Ok(value) => value,
                Err(error_code) => {
                    connection.queue_error(
                        packet.header.seq,
                        error_code,
                        "invalid activity request body",
                    )?;
                    return Ok(());
                }
            };
            let response = engine
                .dispatch_action_with_context(
                    &request_context,
                    ActivityActionRequest {
                        activity_id: request.activity_id,
                        version: request.version,
                        stage_id: request.stage_id,
                        action_type: "claim".into(),
                        client_request_id: request.client_request_id,
                    },
                    now,
                )
                .await;
            connection.queue_message(
                MessageType::ActivityClaimRes,
                packet.header.seq,
                ActivityClaimRes {
                    ok: response.ok,
                    error_code: response.error_code.unwrap_or_default().into(),
                    activity_id: response.activity_id,
                    version: response.version,
                    stage_id: response.stage_id,
                    client_request_id: response.client_request_id,
                    processing: response.processing,
                    duplicate: response.duplicate,
                    state_revision: response.state_revision,
                },
            )
        }
        Some(MessageType::ActivityActionReq) => {
            let request = match packet.decode_body::<ActivityActionReq>("ACTIVITY_INVALID_REQUEST")
            {
                Ok(value) => value,
                Err(error_code) => {
                    connection.queue_error(
                        packet.header.seq,
                        error_code,
                        "invalid activity request body",
                    )?;
                    return Ok(());
                }
            };
            let response = engine
                .dispatch_action_with_context(
                    &request_context,
                    ActivityActionRequest {
                        activity_id: request.activity_id,
                        version: request.version,
                        stage_id: request.stage_id,
                        action_type: request.action_type,
                        client_request_id: request.client_request_id,
                    },
                    now,
                )
                .await;
            connection.queue_message(
                MessageType::ActivityActionRes,
                packet.header.seq,
                ActivityActionRes {
                    ok: response.ok,
                    error_code: response.error_code.unwrap_or_default().into(),
                    activity_id: response.activity_id,
                    version: response.version,
                    stage_id: response.stage_id,
                    action_type: response.action_type,
                    client_request_id: response.client_request_id,
                    processing: response.processing,
                    duplicate: response.duplicate,
                    state_revision: response.state_revision,
                },
            )
        }
        _ => Ok(()),
    }
}

fn summary(
    snapshot: &PublishedActivitySnapshot,
    now: chrono::DateTime<Utc>,
) -> crate::pb::ActivitySummary {
    let status = match snapshot.activity.effective_status(now) {
        ActivityStatus::Draft => "draft",
        ActivityStatus::Published => "published",
        ActivityStatus::Running => "running",
        ActivityStatus::Ended => "ended",
        ActivityStatus::Offline => "offline",
        ActivityStatus::Archived => "archived",
    };
    crate::pb::ActivitySummary {
        activity_id: snapshot.activity.id.clone(),
        version: snapshot.version.version_no as u32,
        activity_type: snapshot.activity.activity_type.as_str().to_string(),
        status: status.into(),
        start_at_ms: snapshot.activity.start_at.timestamp_millis(),
        end_at_ms: snapshot.activity.end_at.timestamp_millis(),
        claim_deadline_ms: snapshot.activity.claim_deadline.timestamp_millis(),
        timezone: snapshot.activity.timezone.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::MetricsCollector;
    use chrono::{Duration, TimeZone, Utc};
    use serde_json::json;
    use std::collections::HashMap;

    fn fixture() -> (Activity, ActivityVersion) {
        let start = Utc.with_ymd_and_hms(2026, 8, 21, 0, 0, 0).unwrap();
        let end = start + Duration::hours(24);
        let activity = Activity::new(
            "activity_1",
            "activity-key",
            ActivityType::new("login_reward").unwrap(),
            ActivityScope::Character,
            start,
            end,
            end + Duration::days(1),
            "Asia/Shanghai",
        )
        .unwrap();
        let version = ActivityVersion::draft(
            activity.id.clone(),
            1,
            json!({"title":"demo"}),
            json!({"schema_version": 1, "kind":"fake"}),
            start,
            end,
            end + Duration::days(1),
            "Asia/Shanghai",
        )
        .unwrap();
        (activity, version)
    }

    fn metric_value(fields: &HashMap<String, String>, key: &str) -> u64 {
        fields
            .get(key)
            .unwrap_or_else(|| panic!("missing metric field {key}"))
            .parse()
            .unwrap()
    }

    #[tokio::test]
    async fn detail_and_progress_handlers_route_to_distinct_metric_actions() {
        let metrics = Box::leak(Box::new(MetricsCollector::new()));
        let engine = ActivityEngine::disabled().with_metrics(metrics);
        let context = ActivityRequestContext::character_only("character-1");
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 1, 0, 0).unwrap();

        let detail_error = read_snapshot_for_message(
            &engine,
            &context,
            MessageType::ActivityDetailReq,
            "activity-1",
            1,
            now,
        )
        .await
        .unwrap_err();
        assert_eq!(detail_error.code, "ACTIVITY_ENGINE_UNAVAILABLE");
        let detail_fields = metrics
            .drain_activity_fields()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(
            metric_value(&detail_fields, "activity_request_detail_total"),
            1
        );
        assert_eq!(
            metric_value(&detail_fields, "activity_request_progress_total"),
            0
        );

        let progress_error = read_snapshot_for_message(
            &engine,
            &context,
            MessageType::ActivityProgressReq,
            "activity-1",
            1,
            now,
        )
        .await
        .unwrap_err();
        assert_eq!(progress_error.code, "ACTIVITY_ENGINE_UNAVAILABLE");
        let progress_fields = metrics
            .drain_activity_fields()
            .into_iter()
            .collect::<HashMap<_, _>>();
        assert_eq!(
            metric_value(&progress_fields, "activity_request_detail_total"),
            0
        );
        assert_eq!(
            metric_value(&progress_fields, "activity_request_progress_total"),
            1
        );
    }

    #[test]
    fn time_window_is_left_closed_and_right_open() {
        let (activity, _) = fixture();
        assert!(!activity.is_in_window(activity.start_at - Duration::seconds(1)));
        assert!(activity.is_in_window(activity.start_at));
        assert!(!activity.is_in_window(activity.end_at));
    }

    #[test]
    fn lifecycle_rejects_reopening_published_version() {
        assert!(ActivityStatus::Draft.can_transition_to(ActivityStatus::Published));
        assert!(ActivityStatus::Published.can_transition_to(ActivityStatus::Running));
        assert!(ActivityStatus::Running.can_transition_to(ActivityStatus::Ended));
        assert!(ActivityStatus::Ended.can_transition_to(ActivityStatus::Archived));
        assert!(!ActivityStatus::Published.can_transition_to(ActivityStatus::Draft));
        assert!(!ActivityStatus::Archived.can_transition_to(ActivityStatus::Running));
    }

    #[test]
    fn config_digest_is_stable_and_validated() {
        let (_, version) = fixture();
        assert!(version.config_digest.starts_with("sha256:"));
        assert!(ActivityVersion::validate_digest(&version.config_digest));
        assert!(!ActivityVersion::validate_digest("sha256:bad"));

        let admin_control_plane_vector = ActivityVersion::digest(
            &json!({
                "title": "Summer",
                "reward_groups": [{
                    "key": "g1",
                    "selection_mode": "fixed",
                    "items": [{"item_id": 1001, "quantity": 2}]
                }]
            }),
            &json!({
                "schema_version": 1,
                "event_source": "game_entry",
                "cycle_unit": "natural_day",
                "progression": "consecutive",
                "miss_policy": "reset",
                "claim_mode": "manual",
                "stages": [{"stage_no": 1, "required_count": 1, "reward_group_key": "g1"}]
            }),
        );
        assert_eq!(
            admin_control_plane_vector,
            "sha256:8db18111c07f7f457d6ee34bd7be5b12dc6a876a7dcfe6670b441a1c929d0dd5"
        );
    }

    #[test]
    fn unknown_activity_type_has_stable_error_code() {
        let error = ActivityType::from_registered("unknown", &["login_reward"]).unwrap_err();
        assert_eq!(error.code().as_str(), "ACTIVITY_UNKNOWN_TYPE");
    }

    #[test]
    fn ended_claims_allow_earned_qualification_until_deadline_only() {
        let (mut activity, _) = fixture();
        activity.status = ActivityStatus::Ended;
        let earned_at = activity.start_at + Duration::hours(1);
        assert!(
            activity
                .can_claim(activity.end_at + Duration::hours(1), earned_at)
                .is_ok()
        );
        assert_eq!(
            activity
                .can_claim(activity.claim_deadline, earned_at)
                .unwrap_err()
                .code()
                .as_str(),
            "ACTIVITY_CLAIM_EXPIRED"
        );
    }

    #[test]
    fn offline_claims_are_rejected_even_if_qualification_is_valid() {
        let (mut activity, _) = fixture();
        activity.status = ActivityStatus::Offline;
        let error = activity
            .can_claim(activity.end_at + Duration::hours(1), activity.start_at)
            .unwrap_err();
        assert_eq!(error.code().as_str(), "ACTIVITY_INVALID_STATE");
    }

    #[tokio::test]
    async fn repository_keeps_draft_write_separate_from_published_read() {
        let repository = InMemoryActivityRepository::default();
        let (activity, version) = fixture();
        repository
            .save_draft(activity.clone(), version)
            .await
            .unwrap();
        assert!(
            repository
                .get_published(&activity.id, activity.start_at)
                .await
                .unwrap()
                .is_none()
        );
        repository.publish(&activity.id, 1, None).await.unwrap();
        let snapshot = repository
            .get_published(&activity.id, activity.start_at)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.version.version_no, 1);
    }

    #[tokio::test]
    async fn repository_preserves_published_metadata_until_cas_publish() {
        let repository = InMemoryActivityRepository::default();
        let (activity, version) = fixture();
        repository
            .save_draft(activity.clone(), version)
            .await
            .unwrap();
        repository.publish(&activity.id, 1, None).await.unwrap();
        let published = repository
            .get_published(&activity.id, activity.start_at)
            .await
            .unwrap()
            .unwrap();

        let version2 = ActivityVersion::draft(
            activity.id.clone(),
            2,
            json!({"title":"next"}),
            json!({"kind":"fake"}),
            activity.start_at,
            activity.end_at + Duration::hours(24),
            activity.claim_deadline + Duration::hours(24),
            activity.timezone.clone(),
        )
        .unwrap();
        repository
            .save_draft(activity.clone(), version2)
            .await
            .unwrap();
        let still_published = repository
            .get_published(&activity.id, activity.start_at)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            still_published.version.version_no,
            published.version.version_no
        );
        assert_eq!(still_published.activity.end_at, published.activity.end_at);

        repository.publish(&activity.id, 2, Some(1)).await.unwrap();
        let switched = repository
            .get_published(&activity.id, activity.start_at)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(switched.version.version_no, 2);
        assert_ne!(switched.activity.end_at, published.activity.end_at);
    }

    #[tokio::test]
    async fn repository_rejects_scope_version_and_digest_conflicts() {
        let repository = InMemoryActivityRepository::default();
        let (mut account_activity, version) = fixture();
        account_activity.scope = ActivityScope::Account;
        assert_eq!(
            repository
                .save_draft(account_activity, version.clone())
                .await
                .unwrap_err()
                .code(),
            ActivityErrorCode::InvalidConfig
        );

        let (activity, mut bad_version) = fixture();
        bad_version.config_digest = format!("sha256:{}", "0".repeat(64));
        assert_eq!(
            repository
                .save_draft(activity.clone(), bad_version)
                .await
                .unwrap_err()
                .code(),
            ActivityErrorCode::InvalidConfig
        );
        repository
            .save_draft(activity.clone(), version)
            .await
            .unwrap();
        repository.publish(&activity.id, 1, None).await.unwrap();
        assert_eq!(
            repository
                .publish(&activity.id, 1, Some(99))
                .await
                .unwrap_err()
                .code(),
            ActivityErrorCode::VersionConflict
        );
    }

    #[tokio::test]
    async fn repository_offline_uses_cas_hides_snapshot_and_preserves_claim_rejection() {
        let repository = InMemoryActivityRepository::default();
        let (mut activity, version) = fixture();
        repository
            .save_draft(activity.clone(), version)
            .await
            .unwrap();
        repository.publish(&activity.id, 1, None).await.unwrap();
        repository.offline(&activity.id, 1).await.unwrap();
        assert!(
            repository
                .get_published(&activity.id, activity.start_at)
                .await
                .unwrap()
                .is_none()
        );

        activity.status = ActivityStatus::Offline;
        let error = activity
            .can_claim(activity.start_at, activity.start_at)
            .unwrap_err();
        assert_eq!(error.code(), ActivityErrorCode::InvalidState);
    }

    #[tokio::test]
    async fn repository_offline_rejects_stale_current_version() {
        let repository = InMemoryActivityRepository::default();
        let (activity, version) = fixture();
        repository
            .save_draft(activity.clone(), version)
            .await
            .unwrap();
        repository.publish(&activity.id, 1, None).await.unwrap();
        let error = repository.offline(&activity.id, 99).await.unwrap_err();
        assert_eq!(error.code(), ActivityErrorCode::VersionConflict);
        assert!(
            repository
                .get_published(&activity.id, activity.start_at)
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn cache_records_refresh_notification_without_external_redis() {
        let cache = InMemoryActivityCache::default();
        let (_, version) = fixture();
        cache.put_version(&version).await.unwrap();
        let cached = cache
            .get_version(&version.activity_id, version.version_no)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cached, version);
        assert_eq!(cached.type_config["schema_version"], 1);
        cache
            .publish_refresh(&version.activity_id, version.version_no)
            .await
            .unwrap();
        assert_eq!(cache.notifications().await.len(), 1);
    }

    #[tokio::test]
    async fn cache_rejects_tampered_digest_and_missing_schema_version() {
        let cache = InMemoryActivityCache::default();
        let (_, mut tampered) = fixture();
        tampered.config_digest = format!("sha256:{}", "0".repeat(64));
        assert!(matches!(
            cache.put_version(&tampered).await.unwrap_err(),
            ActivityCacheError::Serialization(_)
        ));

        let (_, mut missing_schema) = fixture();
        missing_schema.type_config = json!({"event_source": "game_entry"});
        missing_schema.config_digest =
            ActivityVersion::digest(&missing_schema.public_config, &missing_schema.type_config);
        assert!(matches!(
            cache.put_version(&missing_schema).await.unwrap_err(),
            ActivityCacheError::Serialization(_)
        ));
    }
}
