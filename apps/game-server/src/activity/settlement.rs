use crate::core::inventory::{
    AssetCommandErrorCode, AssetOperator, AssetOperatorType, AssetOrigin, AssetOriginType,
    AssetPermission, AssetResultState, NormalizedAssetItem, RewardDeliveryError,
    RewardDeliveryNotifier, RewardDeliveryPolicy, RewardDeliveryResult, RewardDeliveryService,
    RewardDeliveryStore, RewardInventoryPort, RewardOrder,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClaimStatus {
    Processing,
    Granted,
    RetryableFailure,
    ReconciliationPending,
    BlockedCapacity,
    ManualReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActivityClaimRecord {
    pub(crate) character_id: String,
    pub(crate) activity_id: String,
    pub(crate) version: i32,
    pub(crate) semantic_claim_key: String,
    pub(crate) client_request_id: String,
    pub(crate) reward_request_id: String,
    pub(crate) order: Option<RewardOrder>,
    pub(crate) status: ClaimStatus,
    pub(crate) result: Option<RewardDeliveryResult>,
    pub(crate) notification_failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ActivityPlayerStateRecord {
    version: i32,
    current_stage_id: String,
    state_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaimSettlement {
    pub(crate) status: ClaimStatus,
    pub(crate) result: Option<RewardDeliveryResult>,
    pub(crate) duplicate: bool,
    pub(crate) notification_failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityDeliveryOutcome {
    pub(crate) result: RewardDeliveryResult,
    pub(crate) notification_failed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityDeliveryFailure {
    Retryable,
    ManualReview,
}

pub(crate) trait ActivityRewardDelivery: Send + Sync {
    fn deliver<'a>(
        &'a self,
        order: RewardOrder,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = Result<ActivityDeliveryOutcome, ActivityDeliveryFailure>,
                > + Send
                + 'a,
        >,
    >;
}

impl<I, S, N> ActivityRewardDelivery for RewardDeliveryService<I, S, N>
where
    I: RewardInventoryPort,
    S: RewardDeliveryStore,
    N: RewardDeliveryNotifier,
{
    fn deliver<'a>(
        &'a self,
        order: RewardOrder,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<ActivityDeliveryOutcome, ActivityDeliveryFailure>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            RewardDeliveryService::deliver(self, order)
                .await
                .map(|result| ActivityDeliveryOutcome {
                    result,
                    notification_failed: false,
                })
                .map_err(|error| match error {
                    RewardDeliveryError::InvalidOrder(_)
                    | RewardDeliveryError::InvalidInventoryResult(_) => {
                        ActivityDeliveryFailure::ManualReview
                    }
                    RewardDeliveryError::InventoryUnavailable(_)
                    | RewardDeliveryError::DeliveryRecordUnavailable(_)
                    | RewardDeliveryError::RewardMailOutboxUnavailable(_) => {
                        ActivityDeliveryFailure::Retryable
                    }
                })
        })
    }
}

type ClaimStoreFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaimStart {
    Deliver(ActivityClaimRecord),
    Existing(ActivityClaimRecord),
    Conflict,
}

pub(crate) trait ActivityClaimStore: Send + Sync {
    fn start<'a>(
        &'a self,
        record: ActivityClaimRecord,
    ) -> ClaimStoreFuture<'a, Result<ClaimStart, String>>;

    fn finish<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        semantic_claim_key: &'a str,
        status: ClaimStatus,
        result: Option<RewardDeliveryResult>,
        notification_failed: bool,
    ) -> ClaimStoreFuture<'a, Result<(), String>>;

    fn load<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        semantic_claim_key: &'a str,
    ) -> ClaimStoreFuture<'a, Result<Option<ActivityClaimRecord>, String>>;

    fn resume<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        semantic_claim_key: &'a str,
    ) -> ClaimStoreFuture<'a, Result<Option<ActivityClaimRecord>, String>>;

    fn record_manual_review<'a>(
        &'a self,
        record: ActivityClaimRecord,
    ) -> ClaimStoreFuture<'a, Result<(), String>>;

    #[cfg(test)]
    fn state_revision<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
    ) -> ClaimStoreFuture<'a, Option<u64>>;
}

#[derive(Clone)]
pub(crate) struct ActivityClaimCoordinator {
    delivery: Arc<dyn ActivityRewardDelivery>,
    store: Arc<dyn ActivityClaimStore>,
}

#[derive(Clone, Default)]
pub(crate) struct InMemoryActivityClaimStore {
    state: Arc<Mutex<ClaimState>>,
}

#[derive(Default)]
struct ClaimState {
    by_semantic: HashMap<String, ActivityClaimRecord>,
    request_index: HashMap<String, String>,
    player_state: HashMap<String, ActivityPlayerStateRecord>,
}

impl ActivityClaimCoordinator {
    pub(crate) fn new(delivery: Arc<dyn ActivityRewardDelivery>) -> Self {
        Self::with_store(delivery, Arc::new(InMemoryActivityClaimStore::default()))
    }

    pub(crate) fn with_store(
        delivery: Arc<dyn ActivityRewardDelivery>,
        store: Arc<dyn ActivityClaimStore>,
    ) -> Self {
        Self { delivery, store }
    }

    pub(crate) async fn settle(
        &self,
        character_id: &str,
        activity_id: &str,
        version: i32,
        semantic_claim_key: &str,
        client_request_id: &str,
        order: RewardOrder,
    ) -> ClaimSettlement {
        let expected_request_id =
            stable_reward_request_id(character_id, activity_id, version, semantic_claim_key);
        if order.request_id != expected_request_id {
            self.record_manual_review(
                character_id,
                activity_id,
                version,
                semantic_claim_key,
                client_request_id,
                order,
            )
            .await;
            return ClaimSettlement::manual_review();
        }
        let record = ActivityClaimRecord {
            character_id: character_id.to_string(),
            activity_id: activity_id.to_string(),
            version,
            semantic_claim_key: semantic_claim_key.to_string(),
            client_request_id: client_request_id.to_string(),
            reward_request_id: order.request_id.clone(),
            order: Some(order),
            status: ClaimStatus::Processing,
            result: None,
            notification_failed: false,
        };
        let conflict_record = record.clone();
        match self.store.start(record).await {
            Ok(ClaimStart::Deliver(record)) => self.deliver_and_finish(record).await,
            Ok(ClaimStart::Existing(existing))
                if matches!(
                    existing.status,
                    ClaimStatus::Processing | ClaimStatus::ReconciliationPending
                ) =>
            {
                self.deliver_and_finish(existing).await
            }
            Ok(ClaimStart::Existing(existing)) => ClaimSettlement::from_existing(existing),
            Ok(ClaimStart::Conflict) => {
                let mut conflict_record = conflict_record;
                conflict_record.client_request_id.clear();
                conflict_record.status = ClaimStatus::ManualReview;
                let _ = self.store.record_manual_review(conflict_record).await;
                ClaimSettlement::manual_review()
            }
            Err(_) => ClaimSettlement::retryable_failure(),
        }
    }

    pub(crate) async fn reconcile(
        &self,
        character_id: &str,
        activity_id: &str,
        version: i32,
        semantic_claim_key: &str,
    ) -> ClaimSettlement {
        match self
            .store
            .resume(character_id, activity_id, version, semantic_claim_key)
            .await
        {
            Ok(Some(record))
                if matches!(
                    record.status,
                    ClaimStatus::Processing
                        | ClaimStatus::RetryableFailure
                        | ClaimStatus::ReconciliationPending
                        | ClaimStatus::BlockedCapacity
                ) =>
            {
                self.deliver_and_finish(record).await
            }
            Ok(Some(record)) => ClaimSettlement::from_existing(record),
            Ok(None) => ClaimSettlement::manual_review(),
            Err(_) => ClaimSettlement::retryable_failure(),
        }
    }

    async fn deliver_and_finish(&self, record: ActivityClaimRecord) -> ClaimSettlement {
        let Some(order) = record.order.clone() else {
            return ClaimSettlement::manual_review();
        };
        let outcome = match self.delivery.deliver(order).await {
            Ok(outcome) => outcome,
            Err(error) => {
                let status = match error {
                    ActivityDeliveryFailure::Retryable => ClaimStatus::RetryableFailure,
                    ActivityDeliveryFailure::ManualReview => ClaimStatus::ManualReview,
                };
                if self
                    .store
                    .finish(
                        &record.character_id,
                        &record.activity_id,
                        record.version,
                        &record.semantic_claim_key,
                        status,
                        None,
                        false,
                    )
                    .await
                    .is_err()
                {
                    return ClaimSettlement {
                        status: ClaimStatus::ReconciliationPending,
                        result: None,
                        duplicate: false,
                        notification_failed: false,
                    };
                }
                return match status {
                    ClaimStatus::ManualReview => ClaimSettlement::manual_review(),
                    _ => ClaimSettlement::retryable_failure(),
                };
            }
        };
        let status = match outcome.result.result_state {
            AssetResultState::Applied => ClaimStatus::Granted,
            AssetResultState::Unknown => ClaimStatus::ReconciliationPending,
            AssetResultState::NotApplied
                if outcome.result.error_code
                    == Some(AssetCommandErrorCode::InventoryCapacityFull) =>
            {
                ClaimStatus::BlockedCapacity
            }
            // A reward grant has no player-correctable non-capacity failure. Conflicts,
            // invalid result contracts, and terminal mail dispatch outcomes must remain
            // queryable without causing the coordinator to execute the delivery again.
            AssetResultState::NotApplied => ClaimStatus::ManualReview,
        };
        if self
            .store
            .finish(
                &record.character_id,
                &record.activity_id,
                record.version,
                &record.semantic_claim_key,
                status,
                Some(outcome.result.clone()),
                outcome.notification_failed,
            )
            .await
            .is_err()
        {
            return ClaimSettlement {
                status: ClaimStatus::ReconciliationPending,
                result: Some(outcome.result),
                duplicate: false,
                notification_failed: outcome.notification_failed,
            };
        }
        ClaimSettlement {
            status,
            result: Some(outcome.result),
            duplicate: false,
            notification_failed: outcome.notification_failed,
        }
    }

    async fn record_manual_review(
        &self,
        character_id: &str,
        activity_id: &str,
        version: i32,
        semantic_claim_key: &str,
        client_request_id: &str,
        order: RewardOrder,
    ) {
        let record = ActivityClaimRecord {
            character_id: character_id.to_string(),
            activity_id: activity_id.to_string(),
            version,
            semantic_claim_key: semantic_claim_key.to_string(),
            client_request_id: client_request_id.to_string(),
            reward_request_id: order.request_id.clone(),
            order: Some(order),
            status: ClaimStatus::ManualReview,
            result: None,
            notification_failed: false,
        };
        let _ = self.store.record_manual_review(record).await;
    }

    #[cfg(test)]
    async fn record(
        &self,
        character_id: &str,
        activity_id: &str,
        version: i32,
        semantic_claim_key: &str,
    ) -> Option<ActivityClaimRecord> {
        self.store
            .load(character_id, activity_id, version, semantic_claim_key)
            .await
            .ok()
            .flatten()
    }

    #[cfg(test)]
    async fn state_revision(&self, character_id: &str, activity_id: &str) -> Option<u64> {
        self.store.state_revision(character_id, activity_id).await
    }
}

impl ActivityClaimStore for InMemoryActivityClaimStore {
    fn start<'a>(
        &'a self,
        record: ActivityClaimRecord,
    ) -> ClaimStoreFuture<'a, Result<ClaimStart, String>> {
        Box::pin(async move {
            let semantic_key = semantic_index(
                &record.character_id,
                &record.activity_id,
                record.version,
                &record.semantic_claim_key,
            );
            let request_key = request_index(&record.character_id, &record.client_request_id);
            let activity_key = format!("{}\0{}", record.character_id, record.activity_id);
            let mut state = self.state.lock().await;
            if state
                .player_state
                .get(&activity_key)
                .is_some_and(|value| value.version != record.version)
            {
                return Ok(ClaimStart::Conflict);
            }
            if state
                .request_index
                .get(&request_key)
                .is_some_and(|existing| existing != &semantic_key)
            {
                return Ok(ClaimStart::Conflict);
            }
            if let Some(existing) = state.by_semantic.get(&semantic_key).cloned() {
                if existing.reward_request_id != record.reward_request_id
                    || existing
                        .order
                        .as_ref()
                        .map(RewardOrder::request_fingerprint)
                        != record.order.as_ref().map(RewardOrder::request_fingerprint)
                    || existing.client_request_id != record.client_request_id
                {
                    return Ok(ClaimStart::Conflict);
                }
                if matches!(
                    existing.status,
                    ClaimStatus::RetryableFailure | ClaimStatus::BlockedCapacity
                ) {
                    let mut retry = existing;
                    retry.status = ClaimStatus::Processing;
                    state
                        .request_index
                        .insert(request_key, semantic_key.clone());
                    state.by_semantic.insert(semantic_key, retry.clone());
                    return Ok(ClaimStart::Deliver(retry));
                }
                return Ok(ClaimStart::Existing(existing));
            }
            let next_revision = state
                .player_state
                .get(&activity_key)
                .map_or(1, |value| value.state_revision.saturating_add(1));
            state.player_state.insert(
                activity_key,
                ActivityPlayerStateRecord {
                    version: record.version,
                    current_stage_id: record.semantic_claim_key.clone(),
                    state_revision: next_revision,
                },
            );
            state
                .request_index
                .insert(request_key, semantic_key.clone());
            state.by_semantic.insert(semantic_key, record.clone());
            Ok(ClaimStart::Deliver(record))
        })
    }

    fn finish<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        semantic_claim_key: &'a str,
        status: ClaimStatus,
        result: Option<RewardDeliveryResult>,
        notification_failed: bool,
    ) -> ClaimStoreFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let key = semantic_index(character_id, activity_id, version, semantic_claim_key);
            let mut state = self.state.lock().await;
            let record = state
                .by_semantic
                .get_mut(&key)
                .ok_or_else(|| "activity claim disappeared before completion".to_string())?;
            record.status = status;
            record.result = result;
            record.notification_failed = notification_failed;
            Ok(())
        })
    }

    fn load<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        semantic_claim_key: &'a str,
    ) -> ClaimStoreFuture<'a, Result<Option<ActivityClaimRecord>, String>> {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .await
                .by_semantic
                .get(&semantic_index(
                    character_id,
                    activity_id,
                    version,
                    semantic_claim_key,
                ))
                .cloned())
        })
    }

    fn resume<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        semantic_claim_key: &'a str,
    ) -> ClaimStoreFuture<'a, Result<Option<ActivityClaimRecord>, String>> {
        Box::pin(async move {
            let key = semantic_index(character_id, activity_id, version, semantic_claim_key);
            let mut state = self.state.lock().await;
            let Some(record) = state.by_semantic.get_mut(&key) else {
                return Ok(None);
            };
            if matches!(
                record.status,
                ClaimStatus::Processing
                    | ClaimStatus::RetryableFailure
                    | ClaimStatus::ReconciliationPending
                    | ClaimStatus::BlockedCapacity
            ) {
                record.status = ClaimStatus::Processing;
            }
            Ok(Some(record.clone()))
        })
    }

    fn record_manual_review<'a>(
        &'a self,
        record: ActivityClaimRecord,
    ) -> ClaimStoreFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let semantic_key = semantic_index(
                &record.character_id,
                &record.activity_id,
                record.version,
                &record.semantic_claim_key,
            );
            let mut state = self.state.lock().await;
            if !record.client_request_id.is_empty() {
                let request_key = request_index(&record.character_id, &record.client_request_id);
                state
                    .request_index
                    .entry(request_key)
                    .or_insert_with(|| semantic_key.clone());
            }
            state.by_semantic.entry(semantic_key).or_insert(record);
            Ok(())
        })
    }

    #[cfg(test)]
    fn state_revision<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
    ) -> ClaimStoreFuture<'a, Option<u64>> {
        Box::pin(async move {
            self.state
                .lock()
                .await
                .player_state
                .get(&format!("{character_id}\0{activity_id}"))
                .map(|state| {
                    let _ = &state.current_stage_id;
                    state.state_revision
                })
        })
    }
}

#[derive(Clone)]
pub(crate) struct PgActivityClaimStore {
    pool: PgPool,
}

#[derive(sqlx::FromRow)]
struct PgActivityClaimRow {
    character_id: String,
    activity_id: String,
    version_no: i32,
    semantic_claim_key: String,
    client_request_id: Option<String>,
    status: String,
    reward_request_id: Option<String>,
    order_snapshot_json: serde_json::Value,
    result_json: serde_json::Value,
    notification_failed: bool,
}

impl PgActivityClaimStore {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    fn status(value: &str) -> Result<ClaimStatus, String> {
        match value {
            "processing" => Ok(ClaimStatus::Processing),
            "granted" => Ok(ClaimStatus::Granted),
            "retryable_failure" => Ok(ClaimStatus::RetryableFailure),
            "reconciliation_pending" => Ok(ClaimStatus::ReconciliationPending),
            "blocked_capacity" => Ok(ClaimStatus::BlockedCapacity),
            "manual_review" => Ok(ClaimStatus::ManualReview),
            _ => Err(format!("unsupported activity claim status '{value}'")),
        }
    }

    fn status_value(status: ClaimStatus) -> &'static str {
        match status {
            ClaimStatus::Processing => "processing",
            ClaimStatus::Granted => "granted",
            ClaimStatus::RetryableFailure => "retryable_failure",
            ClaimStatus::ReconciliationPending => "reconciliation_pending",
            ClaimStatus::BlockedCapacity => "blocked_capacity",
            ClaimStatus::ManualReview => "manual_review",
        }
    }

    fn decode(row: PgActivityClaimRow) -> Result<ActivityClaimRecord, String> {
        let status = Self::status(&row.status)?;
        let order = if status == ClaimStatus::ManualReview
            && row.order_snapshot_json == serde_json::json!({})
        {
            None
        } else {
            Some(
                serde_json::from_value::<RewardOrder>(row.order_snapshot_json)
                    .map_err(|error| format!("invalid stored activity reward order: {error}"))?,
            )
        };
        let result = if row.result_json.is_null() {
            None
        } else {
            Some(
                serde_json::from_value(row.result_json)
                    .map_err(|error| format!("invalid stored activity reward result: {error}"))?,
            )
        };
        let reward_request_id = row.reward_request_id.unwrap_or_default();
        if status != ClaimStatus::ManualReview
            && order.as_ref().map(|order| order.request_id.as_str())
                != Some(reward_request_id.as_str())
        {
            return Err("stored activity claim reward request does not match order".to_string());
        }
        Ok(ActivityClaimRecord {
            character_id: row.character_id,
            activity_id: row.activity_id,
            version: row.version_no,
            semantic_claim_key: row.semantic_claim_key,
            client_request_id: row.client_request_id.unwrap_or_default(),
            reward_request_id,
            order,
            status,
            result,
            notification_failed: row.notification_failed,
        })
    }

    async fn load_from<'e, E>(
        executor: E,
        character_id: &str,
        activity_id: &str,
        version: i32,
        semantic_claim_key: &str,
        lock: bool,
    ) -> Result<Option<ActivityClaimRecord>, String>
    where
        E: sqlx::Executor<'e, Database = sqlx::Postgres>,
    {
        let suffix = if lock { " FOR UPDATE" } else { "" };
        let query = format!(
            r#"SELECT character_id, activity_id, version_no, semantic_claim_key,
                client_request_id, status, reward_request_id, order_snapshot_json,
                result_json, notification_failed
            FROM activity_claim_record
            WHERE character_id = $1 AND activity_id = $2 AND version_no = $3
              AND semantic_claim_key = $4{suffix}"#
        );
        sqlx::query_as::<_, PgActivityClaimRow>(&query)
            .bind(character_id)
            .bind(activity_id)
            .bind(version)
            .bind(semantic_claim_key)
            .fetch_optional(executor)
            .await
            .map_err(|error| error.to_string())?
            .map(Self::decode)
            .transpose()
    }
}

impl ActivityClaimStore for PgActivityClaimStore {
    fn start<'a>(
        &'a self,
        record: ActivityClaimRecord,
    ) -> ClaimStoreFuture<'a, Result<ClaimStart, String>> {
        Box::pin(async move {
            let mut transaction = self.pool.begin().await.map_err(|error| error.to_string())?;
            let request_binding = sqlx::query_as::<_, (String, i32, String)>(
                r#"SELECT activity_id, version_no, semantic_claim_key
                FROM activity_claim_record
                WHERE character_id = $1 AND client_request_id = $2
                FOR UPDATE"#,
            )
            .bind(&record.character_id)
            .bind(&record.client_request_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
            if request_binding.is_some_and(|binding| {
                binding
                    != (
                        record.activity_id.clone(),
                        record.version,
                        record.semantic_claim_key.clone(),
                    )
            }) {
                transaction
                    .rollback()
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(ClaimStart::Conflict);
            }
            if let Some(existing) = Self::load_from(
                &mut *transaction,
                &record.character_id,
                &record.activity_id,
                record.version,
                &record.semantic_claim_key,
                true,
            )
            .await?
            {
                if existing.reward_request_id != record.reward_request_id
                    || existing
                        .order
                        .as_ref()
                        .map(RewardOrder::request_fingerprint)
                        != record.order.as_ref().map(RewardOrder::request_fingerprint)
                    || existing.client_request_id != record.client_request_id
                {
                    transaction
                        .rollback()
                        .await
                        .map_err(|error| error.to_string())?;
                    return Ok(ClaimStart::Conflict);
                }
                if matches!(
                    existing.status,
                    ClaimStatus::RetryableFailure | ClaimStatus::BlockedCapacity
                ) {
                    sqlx::query(
                        r#"UPDATE activity_claim_record
                        SET status = 'processing', error_code = NULL,
                            last_retry_at = current_timestamp, updated_at = current_timestamp,
                            attempt_count = attempt_count + 1
                        WHERE character_id = $1 AND activity_id = $2 AND version_no = $3
                          AND semantic_claim_key = $4"#,
                    )
                    .bind(&record.character_id)
                    .bind(&record.activity_id)
                    .bind(record.version)
                    .bind(&record.semantic_claim_key)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| error.to_string())?;
                    transaction
                        .commit()
                        .await
                        .map_err(|error| error.to_string())?;
                    let mut retry = existing;
                    retry.status = ClaimStatus::Processing;
                    return Ok(ClaimStart::Deliver(retry));
                }
                transaction
                    .commit()
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(ClaimStart::Existing(existing));
            }
            let order = record
                .order
                .as_ref()
                .ok_or_else(|| "activity claim order is missing".to_string())?;
            let reward_snapshot =
                serde_json::to_value(&order.items).map_err(|error| error.to_string())?;
            let order_snapshot = serde_json::to_value(order).map_err(|error| error.to_string())?;
            let inserted = sqlx::query_scalar::<_, i64>(
                r#"INSERT INTO activity_claim_record (
                    character_id, activity_id, version_no, activity_type, action_type,
                    period_key, semantic_claim_key, client_request_id, status,
                    reward_snapshot_json, cost_snapshot_json, reward_request_id,
                    order_snapshot_json, result_json, notification_failed, attempt_count
                ) VALUES ($1, $2, $3, 'login_reward', 'claim', $4, $4, $5, 'processing',
                    $6, '[]'::jsonb, $7, $8, 'null'::jsonb, false, 1)
                ON CONFLICT DO NOTHING RETURNING id"#,
            )
            .bind(&record.character_id)
            .bind(&record.activity_id)
            .bind(record.version)
            .bind(&record.semantic_claim_key)
            .bind(&record.client_request_id)
            .bind(reward_snapshot)
            .bind(&record.reward_request_id)
            .bind(&order_snapshot)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
            transaction
                .commit()
                .await
                .map_err(|error| error.to_string())?;
            if inserted.is_some() {
                return Ok(ClaimStart::Deliver(record));
            }
            match self
                .load(
                    &record.character_id,
                    &record.activity_id,
                    record.version,
                    &record.semantic_claim_key,
                )
                .await?
            {
                Some(existing)
                    if existing.reward_request_id == record.reward_request_id
                        && existing
                            .order
                            .as_ref()
                            .map(RewardOrder::request_fingerprint)
                            == record.order.as_ref().map(RewardOrder::request_fingerprint)
                        && existing.client_request_id == record.client_request_id =>
                {
                    Ok(ClaimStart::Existing(existing))
                }
                _ => Ok(ClaimStart::Conflict),
            }
        })
    }

    fn finish<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        semantic_claim_key: &'a str,
        status: ClaimStatus,
        result: Option<RewardDeliveryResult>,
        notification_failed: bool,
    ) -> ClaimStoreFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let result_json = serde_json::to_value(result).map_err(|error| error.to_string())?;
            let changed = sqlx::query(
                r#"UPDATE activity_claim_record SET
                    status = $5, result_json = $6, notification_failed = $7,
                    completed_at = CASE WHEN $5 = 'granted' THEN current_timestamp ELSE completed_at END,
                    updated_at = current_timestamp
                WHERE character_id = $1 AND activity_id = $2 AND version_no = $3
                  AND semantic_claim_key = $4"#,
            )
            .bind(character_id)
            .bind(activity_id)
            .bind(version)
            .bind(semantic_claim_key)
            .bind(Self::status_value(status))
            .bind(result_json)
            .bind(notification_failed)
            .execute(&self.pool)
            .await
            .map_err(|error| error.to_string())?;
            if changed.rows_affected() == 1 {
                Ok(())
            } else {
                Err("activity claim disappeared before completion".to_string())
            }
        })
    }

    fn load<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        semantic_claim_key: &'a str,
    ) -> ClaimStoreFuture<'a, Result<Option<ActivityClaimRecord>, String>> {
        Box::pin(async move {
            Self::load_from(
                &self.pool,
                character_id,
                activity_id,
                version,
                semantic_claim_key,
                false,
            )
            .await
        })
    }

    fn resume<'a>(
        &'a self,
        character_id: &'a str,
        activity_id: &'a str,
        version: i32,
        semantic_claim_key: &'a str,
    ) -> ClaimStoreFuture<'a, Result<Option<ActivityClaimRecord>, String>> {
        Box::pin(async move {
            let mut transaction = self.pool.begin().await.map_err(|error| error.to_string())?;
            let Some(mut record) = Self::load_from(
                &mut *transaction,
                character_id,
                activity_id,
                version,
                semantic_claim_key,
                true,
            )
            .await?
            else {
                transaction
                    .commit()
                    .await
                    .map_err(|error| error.to_string())?;
                return Ok(None);
            };
            if matches!(
                record.status,
                ClaimStatus::Processing
                    | ClaimStatus::RetryableFailure
                    | ClaimStatus::ReconciliationPending
                    | ClaimStatus::BlockedCapacity
            ) {
                sqlx::query(
                    r#"UPDATE activity_claim_record
                    SET status = 'processing', last_retry_at = current_timestamp,
                        updated_at = current_timestamp, attempt_count = attempt_count + 1
                    WHERE character_id = $1 AND activity_id = $2 AND version_no = $3
                      AND semantic_claim_key = $4"#,
                )
                .bind(character_id)
                .bind(activity_id)
                .bind(version)
                .bind(semantic_claim_key)
                .execute(&mut *transaction)
                .await
                .map_err(|error| error.to_string())?;
                record.status = ClaimStatus::Processing;
            }
            transaction
                .commit()
                .await
                .map_err(|error| error.to_string())?;
            Ok(Some(record))
        })
    }

    fn record_manual_review<'a>(
        &'a self,
        record: ActivityClaimRecord,
    ) -> ClaimStoreFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let reward_snapshot = record
                .order
                .as_ref()
                .map(|order| serde_json::to_value(&order.items))
                .transpose()
                .map_err(|error| error.to_string())?
                .unwrap_or_else(|| serde_json::json!([]));
            let order_snapshot = record
                .order
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| error.to_string())?
                .unwrap_or_else(|| serde_json::json!({}));
            let inserted = sqlx::query(
                r#"INSERT INTO activity_claim_record (
                    character_id, activity_id, version_no, activity_type, action_type,
                    period_key, semantic_claim_key, client_request_id, status,
                    reward_snapshot_json, cost_snapshot_json, reward_request_id,
                    order_snapshot_json, result_json, notification_failed, attempt_count,
                    error_code
                ) VALUES ($1, $2, $3, 'login_reward', 'claim', $4, $4, NULLIF($5, ''),
                    'manual_review', $6, '[]'::jsonb, $7, $8, 'null'::jsonb, false, 0,
                    'ACTIVITY_REQUEST_CONFLICT')
                ON CONFLICT DO NOTHING"#,
            )
            .bind(&record.character_id)
            .bind(&record.activity_id)
            .bind(record.version)
            .bind(&record.semantic_claim_key)
            .bind(&record.client_request_id)
            .bind(reward_snapshot)
            .bind(&record.reward_request_id)
            .bind(&order_snapshot)
            .execute(&self.pool)
            .await
            .map_err(|error| error.to_string())?;
            if inserted.rows_affected() == 0 {
                sqlx::query(
                    r#"INSERT INTO activity_claim_review (
                        character_id, activity_id, version_no, semantic_claim_key,
                        client_request_id, reason_code, order_snapshot_json
                    ) VALUES ($1, $2, $3, $4, NULLIF($5, ''),
                        'ACTIVITY_REQUEST_CONFLICT', $6)"#,
                )
                .bind(&record.character_id)
                .bind(&record.activity_id)
                .bind(record.version)
                .bind(&record.semantic_claim_key)
                .bind(&record.client_request_id)
                .bind(order_snapshot)
                .execute(&self.pool)
                .await
                .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
    }

    #[cfg(test)]
    fn state_revision<'a>(
        &'a self,
        _character_id: &'a str,
        _activity_id: &'a str,
    ) -> ClaimStoreFuture<'a, Option<u64>> {
        Box::pin(async { None })
    }
}

impl ClaimSettlement {
    fn manual_review() -> Self {
        Self {
            status: ClaimStatus::ManualReview,
            result: None,
            duplicate: false,
            notification_failed: false,
        }
    }

    fn retryable_failure() -> Self {
        Self {
            status: ClaimStatus::RetryableFailure,
            result: None,
            duplicate: false,
            notification_failed: false,
        }
    }

    fn from_existing(record: ActivityClaimRecord) -> Self {
        Self {
            status: record.status,
            result: record.result,
            duplicate: true,
            notification_failed: record.notification_failed,
        }
    }
}

pub(crate) fn stable_reward_request_id(
    character_id: &str,
    activity_id: &str,
    version: i32,
    semantic_claim_key: &str,
) -> String {
    let canonical =
        format!("activity-claim\0{character_id}\0{activity_id}\0{version}\0{semantic_claim_key}");
    format!("activity_claim:{:x}", Sha256::digest(canonical.as_bytes()))
}

pub(crate) fn build_reward_order(
    character_id: &str,
    activity_id: &str,
    version: i32,
    semantic_claim_key: &str,
    items: &[NormalizedAssetItem],
    policy: RewardDeliveryPolicy,
) -> Result<RewardOrder, AssetCommandErrorCode> {
    let request_id =
        stable_reward_request_id(character_id, activity_id, version, semantic_claim_key);
    let reason = format!("activity claim {request_id}");
    let origin = AssetOrigin::new(
        AssetOriginType::Activity,
        request_id.replacen("activity_claim:", "activity:", 1),
    )
    .map_err(|_| AssetCommandErrorCode::InvalidOrigin)?;
    let operator = AssetOperator::new(
        AssetOperatorType::Service,
        "game-server.activity",
        [AssetPermission::Grant],
    )?;
    RewardOrder::new(
        request_id,
        character_id,
        origin,
        policy,
        items,
        reason,
        operator,
    )
}

fn semantic_index(character_id: &str, activity_id: &str, version: i32, key: &str) -> String {
    format!("{character_id}\0{activity_id}\0{version}\0{key}")
}

fn request_index(character_id: &str, request_id: &str) -> String {
    format!("{character_id}\0{request_id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::inventory::{AssetBinding, AssetCommandResult};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Barrier;

    #[derive(Clone)]
    struct FakeDelivery {
        calls: Arc<AtomicUsize>,
        result: AssetResultState,
        error_code: AssetCommandErrorCode,
    }

    impl ActivityRewardDelivery for FakeDelivery {
        fn deliver<'a>(
            &'a self,
            order: RewardOrder,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<ActivityDeliveryOutcome, ActivityDeliveryFailure>,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let fingerprint = order.request_fingerprint();
                let result = match self.result {
                    AssetResultState::Applied => AssetCommandResult::applied(
                        &order.request_id,
                        fingerprint,
                        Vec::new(),
                        Vec::new(),
                        Vec::new(),
                        None,
                    ),
                    AssetResultState::Unknown => {
                        AssetCommandResult::unknown(&order.request_id, fingerprint)
                    }
                    AssetResultState::NotApplied => AssetCommandResult::not_applied(
                        &order.request_id,
                        fingerprint,
                        self.error_code,
                    ),
                }
                .map_err(|_| ActivityDeliveryFailure::ManualReview)?;
                Ok(ActivityDeliveryOutcome {
                    result,
                    notification_failed: false,
                })
            })
        }
    }

    #[derive(Clone)]
    struct FailingDelivery {
        calls: Arc<AtomicUsize>,
        failure: ActivityDeliveryFailure,
    }

    impl ActivityRewardDelivery for FailingDelivery {
        fn deliver<'a>(
            &'a self,
            _order: RewardOrder,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<ActivityDeliveryOutcome, ActivityDeliveryFailure>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                Err(self.failure)
            })
        }
    }

    fn order(request_key: &str) -> RewardOrder {
        build_reward_order(
            "character-1",
            "activity-1",
            1,
            request_key,
            &[NormalizedAssetItem::new(1001, 1, AssetBinding::Unbound).unwrap()],
            RewardDeliveryPolicy::PreferInventory,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn stable_order_and_semantic_key_are_idempotent() {
        let calls = Arc::new(AtomicUsize::new(0));
        let coordinator = ActivityClaimCoordinator::new(Arc::new(FakeDelivery {
            calls: calls.clone(),
            result: AssetResultState::Applied,
            error_code: AssetCommandErrorCode::InvalidResultContract,
        }));
        let first = coordinator
            .settle(
                "character-1",
                "activity-1",
                1,
                "stage-1",
                "req-1",
                order("stage-1"),
            )
            .await;
        let duplicate = coordinator
            .settle(
                "character-1",
                "activity-1",
                1,
                "stage-1",
                "req-1",
                order("stage-1"),
            )
            .await;
        assert_eq!(first.status, ClaimStatus::Granted);
        assert!(duplicate.duplicate);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let request_alias = coordinator
            .settle(
                "character-1",
                "activity-1",
                1,
                "stage-1",
                "req-2",
                order("stage-1"),
            )
            .await;
        assert_eq!(request_alias.status, ClaimStatus::ManualReview);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(order("stage-1").request_id, order("stage-1").request_id);
        assert_eq!(
            coordinator
                .state_revision("character-1", "activity-1")
                .await,
            Some(1)
        );

        let conflict = coordinator
            .settle(
                "character-1",
                "activity-1",
                1,
                "stage-2",
                "req-1",
                order("stage-2"),
            )
            .await;
        assert_eq!(conflict.status, ClaimStatus::ManualReview);
        assert_eq!(
            coordinator
                .record("character-1", "activity-1", 1, "stage-1")
                .await
                .unwrap()
                .status,
            ClaimStatus::Granted
        );
        assert_eq!(
            coordinator
                .record("character-1", "activity-1", 1, "stage-2")
                .await
                .unwrap()
                .status,
            ClaimStatus::ManualReview
        );

        let cross_activity_order = build_reward_order(
            "character-1",
            "activity-2",
            1,
            "stage-1",
            &[NormalizedAssetItem::new(1001, 1, AssetBinding::Unbound).unwrap()],
            RewardDeliveryPolicy::PreferInventory,
        )
        .unwrap();
        let cross_activity = coordinator
            .settle(
                "character-1",
                "activity-2",
                1,
                "stage-1",
                "req-1",
                cross_activity_order,
            )
            .await;
        assert_eq!(cross_activity.status, ClaimStatus::ManualReview);

        let mut invalid_order = order("stage-invalid");
        invalid_order.request_id = "client-supplied-id".into();
        let manual = coordinator
            .settle(
                "character-1",
                "activity-1",
                1,
                "stage-invalid",
                "req-invalid",
                invalid_order,
            )
            .await;
        assert_eq!(manual.status, ClaimStatus::ManualReview);
        assert_eq!(
            coordinator
                .record("character-1", "activity-1", 1, "stage-invalid")
                .await
                .unwrap()
                .status,
            ClaimStatus::ManualReview
        );

        let version_conflict_order = build_reward_order(
            "character-1",
            "activity-1",
            2,
            "stage-v2",
            &[NormalizedAssetItem::new(1001, 1, AssetBinding::Unbound).unwrap()],
            RewardDeliveryPolicy::PreferInventory,
        )
        .unwrap();
        let version_conflict = coordinator
            .settle(
                "character-1",
                "activity-1",
                2,
                "stage-v2",
                "req-v2",
                version_conflict_order,
            )
            .await;
        assert_eq!(version_conflict.status, ClaimStatus::ManualReview);
        assert_eq!(
            coordinator
                .state_revision("character-1", "activity-1")
                .await,
            Some(1)
        );
    }

    #[tokio::test]
    async fn concurrent_same_semantic_claim_calls_delivery_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let coordinator = Arc::new(ActivityClaimCoordinator::new(Arc::new(FakeDelivery {
            calls: calls.clone(),
            result: AssetResultState::Applied,
            error_code: AssetCommandErrorCode::InvalidResultContract,
        })));
        let barrier = Arc::new(Barrier::new(2));
        let left = async {
            barrier.wait().await;
            coordinator
                .settle(
                    "character-1",
                    "activity-1",
                    1,
                    "stage-1",
                    "req-1",
                    order("stage-1"),
                )
                .await
        };
        let right = async {
            barrier.wait().await;
            coordinator
                .settle(
                    "character-1",
                    "activity-1",
                    1,
                    "stage-1",
                    "req-1",
                    order("stage-1"),
                )
                .await
        };
        let (left, right) = tokio::join!(left, right);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            left.status,
            ClaimStatus::Granted | ClaimStatus::Processing
        ));
        assert!(matches!(
            right.status,
            ClaimStatus::Granted | ClaimStatus::Processing
        ));
        assert!(left.duplicate || right.duplicate);
        assert_eq!(
            coordinator
                .state_revision("character-1", "activity-1")
                .await,
            Some(1)
        );
    }

    #[tokio::test]
    async fn two_coordinators_share_claim_store_and_deliver_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let delivery = Arc::new(FakeDelivery {
            calls: calls.clone(),
            result: AssetResultState::Applied,
            error_code: AssetCommandErrorCode::InvalidResultContract,
        });
        let store = Arc::new(InMemoryActivityClaimStore::default());
        let left = ActivityClaimCoordinator::with_store(delivery.clone(), store.clone());
        let right = ActivityClaimCoordinator::with_store(delivery, store);
        let barrier = Arc::new(Barrier::new(2));

        let (left_result, right_result) = tokio::join!(
            async {
                barrier.wait().await;
                left.settle(
                    "character-1",
                    "activity-1",
                    1,
                    "stage-1",
                    "shared-request",
                    order("stage-1"),
                )
                .await
            },
            async {
                barrier.wait().await;
                right
                    .settle(
                        "character-1",
                        "activity-1",
                        1,
                        "stage-1",
                        "shared-request",
                        order("stage-1"),
                    )
                    .await
            }
        );

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            left_result.status,
            ClaimStatus::Granted | ClaimStatus::Processing
        ));
        assert!(matches!(
            right_result.status,
            ClaimStatus::Granted | ClaimStatus::Processing
        ));
        assert!(left_result.duplicate || right_result.duplicate);
    }

    #[tokio::test]
    async fn capacity_block_is_queryable_and_reconcile_reuses_original_order() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(InMemoryActivityClaimStore::default());
        let coordinator = ActivityClaimCoordinator::with_store(
            Arc::new(FakeDelivery {
                calls: calls.clone(),
                result: AssetResultState::NotApplied,
                error_code: AssetCommandErrorCode::InventoryCapacityFull,
            }),
            store,
        );
        let original_order = order("capacity-stage");
        let blocked = coordinator
            .settle(
                "character-1",
                "activity-1",
                1,
                "capacity-stage",
                "capacity-request",
                original_order.clone(),
            )
            .await;
        let stored = coordinator
            .record("character-1", "activity-1", 1, "capacity-stage")
            .await
            .unwrap();
        let retried = coordinator
            .reconcile("character-1", "activity-1", 1, "capacity-stage")
            .await;

        assert_eq!(blocked.status, ClaimStatus::BlockedCapacity);
        assert_eq!(stored.status, ClaimStatus::BlockedCapacity);
        assert_eq!(stored.order, Some(original_order));
        assert_eq!(retried.status, ClaimStatus::BlockedCapacity);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn terminal_delivery_result_is_persisted_for_manual_review_without_replay() {
        let calls = Arc::new(AtomicUsize::new(0));
        let coordinator = ActivityClaimCoordinator::new(Arc::new(FakeDelivery {
            calls: calls.clone(),
            result: AssetResultState::NotApplied,
            error_code: AssetCommandErrorCode::InvalidResultContract,
        }));
        let original_order = order("terminal-mail-stage");

        let first = coordinator
            .settle(
                "character-1",
                "activity-1",
                1,
                "terminal-mail-stage",
                "terminal-mail-request",
                original_order.clone(),
            )
            .await;
        let replay = coordinator
            .settle(
                "character-1",
                "activity-1",
                1,
                "terminal-mail-stage",
                "terminal-mail-request",
                original_order,
            )
            .await;

        assert_eq!(first.status, ClaimStatus::ManualReview);
        assert_eq!(replay.status, ClaimStatus::ManualReview);
        assert!(replay.duplicate);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn deterministic_delivery_error_is_manual_review_and_not_retried() {
        let calls = Arc::new(AtomicUsize::new(0));
        let coordinator = ActivityClaimCoordinator::new(Arc::new(FailingDelivery {
            calls: calls.clone(),
            failure: ActivityDeliveryFailure::ManualReview,
        }));
        let original_order = order("invalid-delivery-stage");

        let first = coordinator
            .settle(
                "character-1",
                "activity-1",
                1,
                "invalid-delivery-stage",
                "invalid-delivery-request",
                original_order.clone(),
            )
            .await;
        let replay = coordinator
            .settle(
                "character-1",
                "activity-1",
                1,
                "invalid-delivery-stage",
                "invalid-delivery-request",
                original_order,
            )
            .await;

        assert_eq!(first.status, ClaimStatus::ManualReview);
        assert_eq!(replay.status, ClaimStatus::ManualReview);
        assert!(replay.duplicate);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_result_is_retried_with_the_original_request_after_restart() {
        let calls = Arc::new(AtomicUsize::new(0));
        let store = Arc::new(InMemoryActivityClaimStore::default());
        let coordinator = ActivityClaimCoordinator::with_store(
            Arc::new(FakeDelivery {
                calls: calls.clone(),
                result: AssetResultState::Unknown,
                error_code: AssetCommandErrorCode::InvalidResultContract,
            }),
            store.clone(),
        );
        let restarted = ActivityClaimCoordinator::with_store(
            Arc::new(FakeDelivery {
                calls: calls.clone(),
                result: AssetResultState::Unknown,
                error_code: AssetCommandErrorCode::InvalidResultContract,
            }),
            store,
        );
        let first = coordinator
            .settle(
                "character-1",
                "activity-1",
                1,
                "stage-1",
                "req-1",
                order("stage-1"),
            )
            .await;
        let retry = restarted
            .settle(
                "character-1",
                "activity-1",
                1,
                "stage-1",
                "req-1",
                order("stage-1"),
            )
            .await;
        assert_eq!(first.status, ClaimStatus::ReconciliationPending);
        assert_eq!(retry.status, ClaimStatus::ReconciliationPending);
        assert!(!retry.duplicate);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            coordinator
                .record("character-1", "activity-1", 1, "stage-1")
                .await
                .unwrap()
                .status,
            ClaimStatus::ReconciliationPending
        );
        let recovered = coordinator
            .reconcile("character-1", "activity-1", 1, "stage-1")
            .await;
        assert_eq!(recovered.status, ClaimStatus::ReconciliationPending);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn stable_reward_identifiers_are_bounded_for_long_inputs() {
        let character_id = "c".repeat(128);
        let activity_id = "a".repeat(64);
        let semantic_claim_key = "s".repeat(320);
        let request_id =
            stable_reward_request_id(&character_id, &activity_id, i32::MAX, &semantic_claim_key);
        let order = build_reward_order(
            &character_id,
            &activity_id,
            i32::MAX,
            &semantic_claim_key,
            &[NormalizedAssetItem::new(1001, 1, AssetBinding::Unbound).unwrap()],
            RewardDeliveryPolicy::PreferInventory,
        )
        .unwrap();

        assert_eq!(request_id.len(), "activity_claim:".len() + 64);
        assert!(request_id.len() <= 128);
        assert!(order.origin.origin_id.len() <= 128);
    }
}
