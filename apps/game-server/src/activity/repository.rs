use super::domain::{
    Activity, ActivityDomainError, ActivityErrorCode, ActivityScope, ActivityStatus,
    ActivityVersion,
};
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions};
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

    fn offline<'a>(
        &'a self,
        activity_id: &'a str,
        expected_current_version: i32,
    ) -> RepositoryFuture<'a, Result<(), ActivityRepositoryError>>;

    fn get_published<'a>(
        &'a self,
        activity_id: &'a str,
        now: DateTime<Utc>,
    ) -> RepositoryFuture<'a, Result<Option<PublishedActivitySnapshot>, ActivityRepositoryError>>;

    /// Detail/action reads may inspect an offline row to return a stable lifecycle error. The
    /// historical published read contract remains hidden for callers using get_published.
    fn get_published_for_detail<'a>(
        &'a self,
        activity_id: &'a str,
        now: DateTime<Utc>,
    ) -> RepositoryFuture<'a, Result<Option<PublishedActivitySnapshot>, ActivityRepositoryError>>
    {
        self.get_published(activity_id, now)
    }

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

    fn detail_snapshot(
        stored: &StoredActivity,
        now: DateTime<Utc>,
    ) -> Option<PublishedActivitySnapshot> {
        let status = stored.activity.effective_status(now);
        if !matches!(
            status,
            ActivityStatus::Published
                | ActivityStatus::Running
                | ActivityStatus::Ended
                | ActivityStatus::Offline
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

    fn offline<'a>(
        &'a self,
        activity_id: &'a str,
        expected_current_version: i32,
    ) -> RepositoryFuture<'a, Result<(), ActivityRepositoryError>> {
        Box::pin(async move {
            let mut state = self.state.write().map_err(|_| Self::lock_error())?;
            let stored = state.get_mut(activity_id).ok_or_else(|| {
                ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::NotFound,
                    "published activity was not found",
                ))
            })?;
            if stored.activity.current_version != Some(expected_current_version) {
                return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::VersionConflict,
                    "activity current version changed",
                )));
            }
            if !matches!(
                stored.activity.status,
                ActivityStatus::Published | ActivityStatus::Running
            ) {
                return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::InvalidState,
                    "only published or running activities can be taken offline",
                )));
            }
            stored.activity.status = ActivityStatus::Offline;
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

    fn get_published_for_detail<'a>(
        &'a self,
        activity_id: &'a str,
        now: DateTime<Utc>,
    ) -> RepositoryFuture<'a, Result<Option<PublishedActivitySnapshot>, ActivityRepositoryError>>
    {
        Box::pin(async move {
            let state = self.state.read().map_err(|_| Self::lock_error())?;
            Ok(state
                .get(activity_id)
                .and_then(|stored| Self::detail_snapshot(stored, now)))
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

#[derive(Clone)]
pub(crate) struct PgActivityRepository {
    pool: PgPool,
}

#[derive(sqlx::FromRow)]
struct PublishedActivityRow {
    activity_id: String,
    activity_key: String,
    activity_type: String,
    scope: String,
    status: String,
    current_version: Option<i32>,
    activity_start_at: DateTime<Utc>,
    activity_end_at: DateTime<Utc>,
    activity_claim_deadline: DateTime<Utc>,
    activity_timezone: String,
    version_no: i32,
    public_config_json: serde_json::Value,
    type_config_json: serde_json::Value,
    config_digest: String,
    version_start_at: DateTime<Utc>,
    version_end_at: DateTime<Utc>,
    version_claim_deadline: DateTime<Utc>,
    version_timezone: String,
    version_published_at: Option<DateTime<Utc>>,
}

const PUBLISHED_SNAPSHOT_SELECT: &str = r#"
SELECT
  a.activity_id,
  a.activity_key,
  a.activity_type,
  a.scope,
  a.status,
  a.current_version,
  a.start_at AS activity_start_at,
  a.end_at AS activity_end_at,
  a.claim_deadline AS activity_claim_deadline,
  a.timezone AS activity_timezone,
  v.version_no,
  v.public_config_json,
  v.type_config_json,
  v.config_digest,
  v.start_at AS version_start_at,
  v.end_at AS version_end_at,
  v.claim_deadline AS version_claim_deadline,
  v.timezone AS version_timezone,
  v.published_at AS version_published_at
FROM activity a
JOIN activity_version v
  ON v.activity_id = a.activity_id AND v.version_no = a.current_version
"#;

impl PgActivityRepository {
    pub(crate) async fn connect(
        database_url: &str,
        pool_size: u32,
    ) -> Result<Self, ActivityRepositoryError> {
        let pool = PgPoolOptions::new()
            .max_connections(pool_size.max(1))
            .connect(database_url)
            .await
            .map_err(|_| ActivityRepositoryError::StorageUnavailable)?;
        Ok(Self { pool })
    }

    pub(crate) fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub(crate) fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    fn row_snapshot(
        row: PublishedActivityRow,
    ) -> Result<PublishedActivitySnapshot, ActivityRepositoryError> {
        let scope = match row.scope.as_str() {
            "character" => ActivityScope::Character,
            "account" => ActivityScope::Account,
            _ => return Err(ActivityRepositoryError::StorageUnavailable),
        };
        let status = match row.status.as_str() {
            "draft" => ActivityStatus::Draft,
            "published" => ActivityStatus::Published,
            "running" => ActivityStatus::Running,
            "ended" => ActivityStatus::Ended,
            "offline" => ActivityStatus::Offline,
            "archived" => ActivityStatus::Archived,
            _ => return Err(ActivityRepositoryError::StorageUnavailable),
        };
        let activity_type = super::domain::ActivityType::new(row.activity_type)
            .map_err(ActivityRepositoryError::Domain)?;
        let activity = Activity {
            id: row.activity_id.clone(),
            key: row.activity_key,
            activity_type,
            scope,
            status,
            start_at: row.activity_start_at,
            end_at: row.activity_end_at,
            claim_deadline: row.activity_claim_deadline,
            timezone: row.activity_timezone,
            current_version: row.current_version,
        };
        let version = ActivityVersion {
            activity_id: row.activity_id,
            version_no: row.version_no,
            public_config: row.public_config_json,
            type_config: row.type_config_json,
            config_digest: row.config_digest,
            start_at: row.version_start_at,
            end_at: row.version_end_at,
            claim_deadline: row.version_claim_deadline,
            timezone: row.version_timezone,
            published_at: row.version_published_at,
        };
        if !version.has_valid_digest() {
            return Err(ActivityRepositoryError::StorageUnavailable);
        }
        Ok(PublishedActivitySnapshot { activity, version })
    }

    async fn fetch_snapshot(
        &self,
        activity_id: &str,
    ) -> Result<Option<PublishedActivitySnapshot>, ActivityRepositoryError> {
        let query = format!("{PUBLISHED_SNAPSHOT_SELECT} WHERE a.activity_id = $1");
        sqlx::query_as::<_, PublishedActivityRow>(&query)
            .bind(activity_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|_| ActivityRepositoryError::StorageUnavailable)?
            .map(Self::row_snapshot)
            .transpose()
    }
}

impl ActivityRepository for PgActivityRepository {
    fn save_draft<'a>(
        &'a self,
        activity: Activity,
        version: ActivityVersion,
    ) -> RepositoryFuture<'a, Result<(), ActivityRepositoryError>> {
        Box::pin(async move {
            if activity.scope != ActivityScope::Character
                || activity.id != version.activity_id
                || !version.has_valid_digest()
            {
                return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::InvalidConfig,
                    "invalid character-scoped activity draft",
                )));
            }
            let mut transaction = self
                .pool
                .begin()
                .await
                .map_err(|_| ActivityRepositoryError::StorageUnavailable)?;
            let activity_write = sqlx::query(
                r#"INSERT INTO activity (
                    activity_id, activity_key, activity_type, scope, status,
                    start_at, end_at, claim_deadline, timezone, created_by
                ) VALUES ($1, $2, $3, 'character', 'draft', $4, $5, $6, $7, 'game-server.activity')
                ON CONFLICT (activity_id) DO UPDATE SET
                    activity_key = EXCLUDED.activity_key,
                    activity_type = EXCLUDED.activity_type,
                    start_at = EXCLUDED.start_at,
                    end_at = EXCLUDED.end_at,
                    claim_deadline = EXCLUDED.claim_deadline,
                    timezone = EXCLUDED.timezone,
                    updated_at = current_timestamp
                WHERE activity.status = 'draft'"#,
            )
            .bind(&activity.id)
            .bind(&activity.key)
            .bind(activity.activity_type.as_str())
            .bind(activity.start_at)
            .bind(activity.end_at)
            .bind(activity.claim_deadline)
            .bind(&activity.timezone)
            .execute(&mut *transaction)
            .await
            .map_err(|_| ActivityRepositoryError::StorageUnavailable)?;
            if activity_write.rows_affected() == 0 {
                return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::InvalidState,
                    "published or archived activity cannot be overwritten",
                )));
            }
            let version_write = sqlx::query(
                r#"INSERT INTO activity_version (
                    activity_id, version_no, public_config_json, type_config_json,
                    config_digest, start_at, end_at, claim_deadline, timezone
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                ON CONFLICT (activity_id, version_no) DO UPDATE SET
                    public_config_json = EXCLUDED.public_config_json,
                    type_config_json = EXCLUDED.type_config_json,
                    config_digest = EXCLUDED.config_digest,
                    start_at = EXCLUDED.start_at,
                    end_at = EXCLUDED.end_at,
                    claim_deadline = EXCLUDED.claim_deadline,
                    timezone = EXCLUDED.timezone
                WHERE activity_version.published_at IS NULL"#,
            )
            .bind(&version.activity_id)
            .bind(version.version_no)
            .bind(&version.public_config)
            .bind(&version.type_config)
            .bind(&version.config_digest)
            .bind(version.start_at)
            .bind(version.end_at)
            .bind(version.claim_deadline)
            .bind(&version.timezone)
            .execute(&mut *transaction)
            .await
            .map_err(|_| ActivityRepositoryError::StorageUnavailable)?;
            if version_write.rows_affected() == 0 {
                return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::VersionConflict,
                    "published activity version is immutable",
                )));
            }
            transaction
                .commit()
                .await
                .map_err(|_| ActivityRepositoryError::StorageUnavailable)
        })
    }

    fn publish<'a>(
        &'a self,
        activity_id: &'a str,
        version_no: i32,
        expected_current_version: Option<i32>,
    ) -> RepositoryFuture<'a, Result<(), ActivityRepositoryError>> {
        Box::pin(async move {
            let mut transaction = self
                .pool
                .begin()
                .await
                .map_err(|_| ActivityRepositoryError::StorageUnavailable)?;
            let current = sqlx::query_as::<_, (Option<i32>, String)>(
                "SELECT current_version, status FROM activity WHERE activity_id = $1 FOR UPDATE",
            )
            .bind(activity_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| ActivityRepositoryError::StorageUnavailable)?
            .ok_or_else(|| {
                ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::NotFound,
                    "activity draft was not found",
                ))
            })?;
            if current.0 != expected_current_version || current.1 != "draft" {
                return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::VersionConflict,
                    "activity current version or state changed",
                )));
            }
            let version_row = sqlx::query_as::<_, (DateTime<Utc>, DateTime<Utc>, DateTime<Utc>, String, Option<DateTime<Utc>>)>(
                "SELECT start_at, end_at, claim_deadline, timezone, published_at FROM activity_version WHERE activity_id = $1 AND version_no = $2 FOR UPDATE",
            )
            .bind(activity_id)
            .bind(version_no)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|_| ActivityRepositoryError::StorageUnavailable)?
            .ok_or_else(|| ActivityRepositoryError::Domain(ActivityDomainError::new(ActivityErrorCode::NotFound, "activity version was not found")))?;
            if version_row.4.is_some() {
                return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::InvalidState,
                    "published activity version is immutable",
                )));
            }
            sqlx::query("UPDATE activity_version SET published_at = current_timestamp, published_by = 'game-server.activity' WHERE activity_id = $1 AND version_no = $2")
                .bind(activity_id)
                .bind(version_no)
                .execute(&mut *transaction)
                .await
                .map_err(|_| ActivityRepositoryError::StorageUnavailable)?;
            sqlx::query("UPDATE activity SET status = 'published', current_version = $2, start_at = $3, end_at = $4, claim_deadline = $5, timezone = $6, published_at = current_timestamp, updated_at = current_timestamp WHERE activity_id = $1")
                .bind(activity_id)
                .bind(version_no)
                .bind(version_row.0)
                .bind(version_row.1)
                .bind(version_row.2)
                .bind(version_row.3)
                .execute(&mut *transaction)
                .await
                .map_err(|_| ActivityRepositoryError::StorageUnavailable)?;
            transaction
                .commit()
                .await
                .map_err(|_| ActivityRepositoryError::StorageUnavailable)
        })
    }

    fn offline<'a>(
        &'a self,
        activity_id: &'a str,
        expected_current_version: i32,
    ) -> RepositoryFuture<'a, Result<(), ActivityRepositoryError>> {
        Box::pin(async move {
            let result = sqlx::query(
                "UPDATE activity SET status = 'offline', offlined_at = current_timestamp, updated_at = current_timestamp WHERE activity_id = $1 AND current_version = $2 AND status IN ('published', 'running')",
            )
            .bind(activity_id)
            .bind(expected_current_version)
            .execute(&self.pool)
            .await
            .map_err(|_| ActivityRepositoryError::StorageUnavailable)?;
            if result.rows_affected() == 0 {
                return Err(ActivityRepositoryError::Domain(ActivityDomainError::new(
                    ActivityErrorCode::VersionConflict,
                    "activity version or lifecycle state changed",
                )));
            }
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
            let snapshot = self.fetch_snapshot(activity_id).await?;
            Ok(snapshot.filter(|value| {
                matches!(
                    value.activity.effective_status(now),
                    ActivityStatus::Published | ActivityStatus::Running | ActivityStatus::Ended
                ) && value.version.published_at.is_some()
            }))
        })
    }

    fn get_published_for_detail<'a>(
        &'a self,
        activity_id: &'a str,
        now: DateTime<Utc>,
    ) -> RepositoryFuture<'a, Result<Option<PublishedActivitySnapshot>, ActivityRepositoryError>>
    {
        Box::pin(async move {
            let snapshot = self.fetch_snapshot(activity_id).await?;
            Ok(snapshot.filter(|value| {
                matches!(
                    value.activity.effective_status(now),
                    ActivityStatus::Published
                        | ActivityStatus::Running
                        | ActivityStatus::Ended
                        | ActivityStatus::Offline
                ) && value.version.published_at.is_some()
            }))
        })
    }

    fn list_published<'a>(
        &'a self,
        now: DateTime<Utc>,
    ) -> RepositoryFuture<'a, Result<Vec<PublishedActivitySnapshot>, ActivityRepositoryError>> {
        Box::pin(async move {
            let query = format!(
                "{PUBLISHED_SNAPSHOT_SELECT} WHERE a.status IN ('published', 'running', 'ended') ORDER BY a.activity_id"
            );
            let rows = sqlx::query_as::<_, PublishedActivityRow>(&query)
                .fetch_all(&self.pool)
                .await
                .map_err(|_| ActivityRepositoryError::StorageUnavailable)?;
            rows.into_iter()
                .map(Self::row_snapshot)
                .filter_map(|snapshot| match snapshot {
                    Ok(value)
                        if matches!(
                            value.activity.effective_status(now),
                            ActivityStatus::Published
                                | ActivityStatus::Running
                                | ActivityStatus::Ended
                        ) && value.version.published_at.is_some() =>
                    {
                        Some(Ok(value))
                    }
                    Ok(_) => None,
                    Err(error) => Some(Err(error)),
                })
                .collect()
        })
    }
}
