use super::domain::ActivityVersion;
use chrono::Utc;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, RwLock};

pub(crate) type CacheFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ActivityCacheError {
    Unavailable(String),
    Serialization(String),
}

impl ActivityCacheError {
    pub(crate) fn code(&self) -> super::domain::ActivityErrorCode {
        super::domain::ActivityErrorCode::CacheUnavailable
    }

    pub(crate) fn reason_code(&self) -> &'static str {
        match self {
            Self::Unavailable(_) => "unavailable",
            Self::Serialization(_) => "serialization",
        }
    }
}

fn validate_snapshot(version: &ActivityVersion) -> Result<(), ActivityCacheError> {
    if !version.has_valid_digest() {
        return Err(ActivityCacheError::Serialization(
            "activity cache snapshot digest mismatch".to_string(),
        ));
    }
    if !version
        .type_config
        .get("schema_version")
        .and_then(serde_json::Value::as_i64)
        .is_some_and(|schema_version| schema_version == super::types::ACTIVITY_TYPE_SCHEMA_VERSION)
    {
        return Err(ActivityCacheError::Serialization(
            "activity cache snapshot schema_version is unsupported".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ActivityCacheKey {
    prefix: String,
}

impl ActivityCacheKey {
    pub(crate) fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }

    pub(crate) fn version(&self, activity_id: &str, version_no: i32) -> String {
        format!(
            "{}activity:version:{}:{}",
            self.prefix, activity_id, version_no
        )
    }

    pub(crate) fn list(&self) -> String {
        format!("{}activity:list", self.prefix)
    }

    pub(crate) fn refresh_channel(&self) -> String {
        format!("{}activity:refresh", self.prefix)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RefreshNotice {
    pub(crate) activity_id: String,
    pub(crate) version_no: i32,
    pub(crate) published_at: String,
}

pub(crate) trait ActivityCache: Send + Sync {
    fn get_version<'a>(
        &'a self,
        activity_id: &'a str,
        version_no: i32,
    ) -> CacheFuture<'a, Result<Option<ActivityVersion>, ActivityCacheError>>;

    fn put_version<'a>(
        &'a self,
        version: &'a ActivityVersion,
    ) -> CacheFuture<'a, Result<(), ActivityCacheError>>;

    fn invalidate_version<'a>(
        &'a self,
        activity_id: &'a str,
        version_no: i32,
    ) -> CacheFuture<'a, Result<(), ActivityCacheError>>;

    fn put_activity_list<'a>(
        &'a self,
        activity_ids: &'a [String],
    ) -> CacheFuture<'a, Result<(), ActivityCacheError>>;

    fn publish_refresh<'a>(
        &'a self,
        activity_id: &'a str,
        version_no: i32,
    ) -> CacheFuture<'a, Result<(), ActivityCacheError>>;
}

#[derive(Clone, Default)]
pub(crate) struct InMemoryActivityCache {
    keys: ActivityCacheKey,
    versions: Arc<RwLock<HashMap<String, ActivityVersion>>>,
    activity_ids: Arc<RwLock<Vec<String>>>,
    refreshes: Arc<RwLock<Vec<RefreshNotice>>>,
}

impl InMemoryActivityCache {
    pub(crate) async fn notifications(&self) -> Vec<RefreshNotice> {
        self.refreshes
            .read()
            .map(|refreshes| refreshes.clone())
            .unwrap_or_default()
    }

    pub(crate) async fn activity_ids(&self) -> Vec<String> {
        self.activity_ids
            .read()
            .map(|ids| ids.clone())
            .unwrap_or_default()
    }
}

impl ActivityCache for InMemoryActivityCache {
    fn get_version<'a>(
        &'a self,
        activity_id: &'a str,
        version_no: i32,
    ) -> CacheFuture<'a, Result<Option<ActivityVersion>, ActivityCacheError>> {
        Box::pin(async move {
            let key = self.keys.version(activity_id, version_no);
            let value = self
                .versions
                .read()
                .map(|versions| versions.get(&key).cloned())
                .map_err(|_| ActivityCacheError::Unavailable("cache lock poisoned".to_string()))?;
            if let Some(version) = value.as_ref() {
                validate_snapshot(version)?;
            }
            Ok(value)
        })
    }

    fn put_version<'a>(
        &'a self,
        version: &'a ActivityVersion,
    ) -> CacheFuture<'a, Result<(), ActivityCacheError>> {
        Box::pin(async move {
            validate_snapshot(version)?;
            let key = self.keys.version(&version.activity_id, version.version_no);
            self.versions
                .write()
                .map_err(|_| ActivityCacheError::Unavailable("cache lock poisoned".to_string()))?
                .insert(key, version.clone());
            Ok(())
        })
    }

    fn invalidate_version<'a>(
        &'a self,
        activity_id: &'a str,
        version_no: i32,
    ) -> CacheFuture<'a, Result<(), ActivityCacheError>> {
        Box::pin(async move {
            let key = self.keys.version(activity_id, version_no);
            self.versions
                .write()
                .map_err(|_| ActivityCacheError::Unavailable("cache lock poisoned".to_string()))?
                .remove(&key);
            Ok(())
        })
    }

    fn put_activity_list<'a>(
        &'a self,
        activity_ids: &'a [String],
    ) -> CacheFuture<'a, Result<(), ActivityCacheError>> {
        Box::pin(async move {
            *self.activity_ids.write().map_err(|_| {
                ActivityCacheError::Unavailable("cache lock poisoned".to_string())
            })? = activity_ids.to_vec();
            Ok(())
        })
    }

    fn publish_refresh<'a>(
        &'a self,
        activity_id: &'a str,
        version_no: i32,
    ) -> CacheFuture<'a, Result<(), ActivityCacheError>> {
        Box::pin(async move {
            self.refreshes
                .write()
                .map_err(|_| ActivityCacheError::Unavailable("cache lock poisoned".to_string()))?
                .push(RefreshNotice {
                    activity_id: activity_id.to_string(),
                    version_no,
                    published_at: Utc::now().to_rfc3339(),
                });
            Ok(())
        })
    }
}

/// Redis adapter for cache-only activity snapshots. PostgreSQL remains the
/// business fact source; a Redis outage is surfaced to the caller so it can
/// fall back to the repository instead of treating cache state as authoritative.
#[derive(Clone)]
pub(crate) struct RedisActivityCache {
    client: redis::Client,
    keys: ActivityCacheKey,
}

impl RedisActivityCache {
    pub(crate) fn new(client: redis::Client, key_prefix: impl Into<String>) -> Self {
        Self {
            client,
            keys: ActivityCacheKey::new(key_prefix),
        }
    }

    async fn connection(&self) -> Result<redis::aio::MultiplexedConnection, ActivityCacheError> {
        self.client
            .get_multiplexed_async_connection()
            .await
            .map_err(|error| ActivityCacheError::Unavailable(error.to_string()))
    }
}

impl ActivityCache for RedisActivityCache {
    fn get_version<'a>(
        &'a self,
        activity_id: &'a str,
        version_no: i32,
    ) -> CacheFuture<'a, Result<Option<ActivityVersion>, ActivityCacheError>> {
        Box::pin(async move {
            let mut connection = self.connection().await?;
            let raw: Option<String> = connection
                .get(self.keys.version(activity_id, version_no))
                .await
                .map_err(|error| ActivityCacheError::Unavailable(error.to_string()))?;
            let version = raw
                .map(|value| {
                    serde_json::from_str::<ActivityVersion>(&value)
                        .map_err(|error| ActivityCacheError::Serialization(error.to_string()))
                })
                .transpose()?;
            if let Some(version) = version.as_ref() {
                if version.activity_id != activity_id || version.version_no != version_no {
                    return Err(ActivityCacheError::Serialization(
                        "activity cache snapshot identity mismatch".to_string(),
                    ));
                }
                validate_snapshot(version)?;
            }
            Ok(version)
        })
    }

    fn put_version<'a>(
        &'a self,
        version: &'a ActivityVersion,
    ) -> CacheFuture<'a, Result<(), ActivityCacheError>> {
        Box::pin(async move {
            validate_snapshot(version)?;
            let mut connection = self.connection().await?;
            let value = serde_json::to_string(version)
                .map_err(|error| ActivityCacheError::Serialization(error.to_string()))?;
            connection
                .set::<_, _, ()>(
                    self.keys.version(&version.activity_id, version.version_no),
                    value,
                )
                .await
                .map_err(|error| ActivityCacheError::Unavailable(error.to_string()))
        })
    }

    fn invalidate_version<'a>(
        &'a self,
        activity_id: &'a str,
        version_no: i32,
    ) -> CacheFuture<'a, Result<(), ActivityCacheError>> {
        Box::pin(async move {
            let mut connection = self.connection().await?;
            connection
                .del::<_, ()>(self.keys.version(activity_id, version_no))
                .await
                .map_err(|error| ActivityCacheError::Unavailable(error.to_string()))
        })
    }

    fn put_activity_list<'a>(
        &'a self,
        activity_ids: &'a [String],
    ) -> CacheFuture<'a, Result<(), ActivityCacheError>> {
        Box::pin(async move {
            let mut connection = self.connection().await?;
            let value = serde_json::to_string(activity_ids)
                .map_err(|error| ActivityCacheError::Serialization(error.to_string()))?;
            connection
                .set::<_, _, ()>(self.keys.list(), value)
                .await
                .map_err(|error| ActivityCacheError::Unavailable(error.to_string()))
        })
    }

    fn publish_refresh<'a>(
        &'a self,
        activity_id: &'a str,
        version_no: i32,
    ) -> CacheFuture<'a, Result<(), ActivityCacheError>> {
        Box::pin(async move {
            let mut connection = self.connection().await?;
            let notice = RefreshNotice {
                activity_id: activity_id.to_string(),
                version_no,
                published_at: Utc::now().to_rfc3339(),
            };
            let payload = serde_json::to_string(&notice)
                .map_err(|error| ActivityCacheError::Serialization(error.to_string()))?;
            connection
                .publish::<_, _, i64>(self.keys.refresh_channel(), payload)
                .await
                .map(|_| ())
                .map_err(|error| ActivityCacheError::Unavailable(error.to_string()))
        })
    }
}
