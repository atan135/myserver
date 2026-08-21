use super::domain::{
    Activity, ActivityDomainError, ActivityErrorCode, ActivityScope, ActivityStatus,
    ActivityVersion,
};
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

pub(crate) type RepositoryFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActivityRepositoryError {
    Domain(ActivityDomainError),
    StorageUnavailable,
}

impl ActivityRepositoryError {
    pub(crate) fn code(&self) -> ActivityErrorCode {
        match self {
            Self::Domain(error) => error.code(),
            Self::StorageUnavailable => ActivityErrorCode::CacheUnavailable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PublishedActivitySnapshot {
    pub(crate) activity: Activity,
    pub(crate) version: ActivityVersion,
}

pub(crate) trait ActivityRepository: Send + Sync {
    fn save_draft<'a>(
        &'a self,
        activity: Activity,
        version: ActivityVersion,
    ) -> RepositoryFuture<'a, Result<(), ActivityRepositoryError>>;

    fn publish<'a>(
        &'a self,
        activity_id: &'a str,
        version_no: i32,
        expected_current_version: Option<i32>,
    ) -> RepositoryFuture<'a, Result<(), ActivityRepositoryError>>;

    fn get_published<'a>(
        &'a self,
        activity_id: &'a str,
        now: DateTime<Utc>,
    ) -> RepositoryFuture<'a, Result<Option<PublishedActivitySnapshot>, ActivityRepositoryError>>;

    fn list_published<'a>(
        &'a self,
        now: DateTime<Utc>,
    ) -> RepositoryFuture<'a, Result<Vec<PublishedActivitySnapshot>, ActivityRepositoryError>>;
}

#[derive(Clone, Default)]
pub(crate) struct InMemoryActivityRepository {
    state: Arc<RwLock<BTreeMap<String, StoredActivity>>>,
}

#[derive(Clone)]
struct StoredActivity {
    activity: Activity,
    versions: BTreeMap<i32, ActivityVersion>,
}

impl InMemoryActivityRepository {
    fn lock_error() -> ActivityRepositoryError {
        ActivityRepositoryError::StorageUnavailable
    }

    fn snapshot(stored: &StoredActivity, now: DateTime<Utc>) -> Option<PublishedActivitySnapshot> {
        let status = stored.activity.effective_status(now);
        if !matches!(
            status,
            ActivityStatus::Published | ActivityStatus::Running | ActivityStatus::Ended
        ) {
            return None;
        }
        let version_no = stored.activity.current_version?;
        let version = stored.versions.get(&version_no)?.clone();
        if version.published_at.is_none() {
            return None;
        }
        Some(PublishedActivitySnapshot {
            activity: stored.activity.clone(),
            version,
        })
    }
}

impl ActivityRepository for InMemoryActivityRepository {
    fn save_draft<'a>(
        &'a self,
        activity: Activity,
        version: ActivityVersion,
    ) -> RepositoryFuture<'a, Result<(), ActivityRepositoryError>> {
        Box::pin(async move {
            if activity.scope != ActivityScope::Character {
                return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::InvalidConfig,
                    "account-scoped activities are not enabled in the first phase",
                )));
            }
            if activity.id != version.activity_id {
                return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::VersionConflict,
                    "activity and version identifiers do not match",
                )));
            }
            if !version.has_valid_digest() {
                return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::InvalidConfig,
                    "activity version digest does not match its JSON configuration",
                )));
            }
            let mut state = self.state.write().map_err(|_| Self::lock_error())?;
            if let Some(existing) = state.get(&activity.id) {
                if existing.activity.status == ActivityStatus::Archived {
                    return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                        ActivityErrorCode::InvalidState,
                        "archived activities cannot receive new drafts",
                    )));
                }
                if let Some(existing_version) = existing.versions.get(&version.version_no) {
                    if existing_version.published_at.is_some() {
                        return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                            ActivityErrorCode::VersionConflict,
                            "published activity version is immutable",
                        )));
                    }
                }
            }
            let stored = state
                .entry(activity.id.clone())
                .or_insert_with(|| StoredActivity {
                    activity: activity.clone(),
                    versions: BTreeMap::new(),
                });
            if stored.activity.status == ActivityStatus::Draft {
                stored.activity = activity;
            }
            stored.versions.insert(version.version_no, version);
            Ok(())
        })
    }

    fn publish<'a>(
        &'a self,
        activity_id: &'a str,
        version_no: i32,
        expected_current_version: Option<i32>,
    ) -> RepositoryFuture<'a, Result<(), ActivityRepositoryError>> {
        Box::pin(async move {
            let mut state = self.state.write().map_err(|_| Self::lock_error())?;
            let stored = state.get_mut(activity_id).ok_or_else(|| {
                ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::NotFound,
                    "activity draft was not found",
                ))
            })?;
            if stored.activity.current_version != expected_current_version {
                return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::VersionConflict,
                    "activity current version changed",
                )));
            }
            let published_at = Utc::now();
            let (start_at, end_at, claim_deadline, timezone) = {
                let version = stored.versions.get_mut(&version_no).ok_or_else(|| {
                    ActivityRepositoryError::Domain(ActivityDomainError::new(
                        ActivityErrorCode::NotFound,
                        "activity version was not found",
                    ))
                })?;
                if version.published_at.is_some() {
                    return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                        ActivityErrorCode::InvalidState,
                        "published activity versions are immutable",
                    )));
                }
                version.published_at = Some(published_at);
                (
                    version.start_at,
                    version.end_at,
                    version.claim_deadline,
                    version.timezone.clone(),
                )
            };
            stored.activity.status = ActivityStatus::Published;
            stored.activity.current_version = Some(version_no);
            stored.activity.start_at = start_at;
            stored.activity.end_at = end_at;
            stored.activity.claim_deadline = claim_deadline;
            stored.activity.timezone = timezone;
            Ok(())
        })
    }

    fn get_published<'a>(
        &'a self,
        activity_id: &'a str,
        now: DateTime<Utc>,
    ) -> RepositoryFuture<'a, Result<Option<PublishedActivitySnapshot>, ActivityRepositoryError>>
    {
        Box::pin(async move {
            let state = self.state.read().map_err(|_| Self::lock_error())?;
            Ok(state
                .get(activity_id)
                .and_then(|stored| Self::snapshot(stored, now)))
        })
    }

    fn list_published<'a>(
        &'a self,
        now: DateTime<Utc>,
    ) -> RepositoryFuture<'a, Result<Vec<PublishedActivitySnapshot>, ActivityRepositoryError>> {
        Box::pin(async move {
            let state = self.state.read().map_err(|_| Self::lock_error())?;
            Ok(state
                .values()
                .filter_map(|stored| Self::snapshot(stored, now))
                .collect())
        })
    }
}
