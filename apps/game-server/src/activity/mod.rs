//! Public activity domain contracts.
//!
//! This module owns activity configuration snapshots, lifecycle and player
//! activity facts. It deliberately does not contain login or lottery rules;
//! those belong to registered type handlers in a later phase.

mod cache;
mod domain;
mod repository;
mod types;

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
pub(crate) use repository::{
    ActivityRepository, ActivityRepositoryError, InMemoryActivityRepository,
    PublishedActivitySnapshot,
};
#[allow(unused_imports)]
pub(crate) use types::{
    ActionApplier, ActionDecision, ActionEvaluator, ActionOutcome, ActivityTypeError,
    ActivityTypeErrorCode, ActivityTypeHandler, ActivityTypeRegistry, ConfigValidator,
    PlayerContext, PlayerViewBuilder, TransactionContext,
};

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone, Utc};
    use serde_json::json;

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
            json!({"kind":"fake"}),
            start,
            end,
            end + Duration::days(1),
            "Asia/Shanghai",
        )
        .unwrap();
        (activity, version)
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
    async fn cache_records_refresh_notification_without_external_redis() {
        let cache = InMemoryActivityCache::default();
        let (_, version) = fixture();
        cache.put_version(&version).await.unwrap();
        assert!(
            cache
                .get_version(&version.activity_id, version.version_no)
                .await
                .unwrap()
                .is_some()
        );
        cache
            .publish_refresh(&version.activity_id, version.version_no)
            .await
            .unwrap();
        assert_eq!(cache.notifications().await.len(), 1);
    }
}
