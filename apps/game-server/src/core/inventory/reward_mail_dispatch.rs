use super::{AssetBinding, PgRewardDeliveryStore, RewardMailOutboxEntry};
use crate::metrics::METRICS;
use futures_util::future::join_all;
use serde_json::{Value, json};
use service_registry::RegistryClient;
use sqlx::PgPool;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

const MAIL_DELIVERY_TIMEOUT_SECS: u64 = 5;
const DISPATCH_LEASE_SECONDS: i64 = 30;
const DISPATCH_PROCESSING_BUDGET_SECS: u64 = 20;
const DISPATCH_BATCH_SIZE: i64 = 16;

type DispatchFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Debug, Clone)]
pub(crate) struct RewardMailDispatchLease {
    pub(crate) entry: RewardMailOutboxEntry,
    pub(crate) account_player_id: String,
    pub(crate) lease_owner: String,
    pub(crate) attempt_count: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RewardMailDispatchFailure {
    Retryable,
    Permanent,
    ManualReview,
}

pub(crate) trait RewardMailDispatchStore: Send + Sync {
    fn claim<'a>(
        &'a self,
        lease_owner: &'a str,
        batch_size: i64,
        lease_seconds: i64,
        max_attempts: i32,
    ) -> DispatchFuture<'a, Result<Vec<RewardMailDispatchLease>, String>>;

    fn mark_delivered<'a>(
        &'a self,
        lease: &'a RewardMailDispatchLease,
        response: Value,
    ) -> DispatchFuture<'a, Result<(), String>>;

    fn mark_failed<'a>(
        &'a self,
        lease: &'a RewardMailDispatchLease,
        failure: RewardMailDispatchFailure,
        error_code: &'a str,
        retry_after: Duration,
    ) -> DispatchFuture<'a, Result<(), String>>;
}

pub(crate) trait RewardMailDeliveryClient: Send + Sync {
    fn deliver<'a>(
        &'a self,
        lease: &'a RewardMailDispatchLease,
    ) -> DispatchFuture<'a, Result<Value, RewardMailDispatchFailure>>;
}

#[derive(Clone)]
pub(crate) struct PgRewardMailDispatchStore {
    pool: PgPool,
    reward_store: PgRewardDeliveryStore,
}

impl PgRewardMailDispatchStore {
    pub(crate) async fn new(pool: PgPool) -> Result<Self, String> {
        Ok(Self {
            reward_store: PgRewardDeliveryStore::from_pool(pool.clone()).await?,
            pool,
        })
    }
}

impl RewardMailDispatchStore for PgRewardMailDispatchStore {
    fn claim<'a>(
        &'a self,
        lease_owner: &'a str,
        batch_size: i64,
        lease_seconds: i64,
        max_attempts: i32,
    ) -> DispatchFuture<'a, Result<Vec<RewardMailDispatchLease>, String>> {
        Box::pin(async move {
            let exhausted = sqlx::query(
                r#"UPDATE reward_mail_outbox SET status = 'manual_review',
                    last_error_code = 'MAIL_DISPATCH_ATTEMPTS_EXHAUSTED',
                    lease_owner = NULL, lease_expires_at = NULL,
                    updated_at = current_timestamp
                WHERE attempt_count >= $1
                  AND (
                    status IN ('pending', 'retryable_failure')
                    OR (status = 'processing' AND (
                        lease_expires_at IS NULL OR lease_expires_at <= current_timestamp
                    ))
                  )"#,
            )
            .bind(max_attempts)
            .execute(&self.pool)
            .await
            .map_err(|error| error.to_string())?;
            for _ in 0..exhausted.rows_affected() {
                METRICS.record_reward_mail_dispatch_manual_review();
            }

            let rows = sqlx::query_as::<_, (String, String, i32)>(
                r#"WITH candidates AS (
                    SELECT o.delivery_request_id
                    FROM reward_mail_outbox o
                    JOIN characters c ON c.character_id = o.character_id AND c.deleted_at IS NULL
                    WHERE o.attempt_count < $1
                      AND o.next_attempt_at <= current_timestamp
                      AND (
                        o.status IN ('pending', 'retryable_failure')
                        OR (o.status = 'processing' AND (
                            o.lease_expires_at IS NULL OR o.lease_expires_at <= current_timestamp
                        ))
                      )
                    ORDER BY o.next_attempt_at, o.created_at
                    FOR UPDATE OF o SKIP LOCKED
                    LIMIT $2
                )
                UPDATE reward_mail_outbox o SET
                    status = 'processing', lease_owner = $3,
                    lease_expires_at = current_timestamp + make_interval(secs => $4),
                    attempt_count = attempt_count + 1, updated_at = current_timestamp
                FROM candidates c
                WHERE o.delivery_request_id = c.delivery_request_id
                RETURNING o.reward_request_id, o.character_id, o.attempt_count"#,
            )
            .bind(max_attempts)
            .bind(batch_size)
            .bind(lease_owner)
            .bind(lease_seconds)
            .fetch_all(&self.pool)
            .await
            .map_err(|error| error.to_string())?;

            let mut leases = Vec::with_capacity(rows.len());
            for (request_id, character_id, attempt_count) in rows {
                let account_player_id = sqlx::query_scalar::<_, String>(
                    "SELECT account_player_id FROM characters WHERE character_id = $1 AND deleted_at IS NULL",
                )
                .bind(&character_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|error| error.to_string())?;
                let entry = self.reward_store.find_mail(&request_id).await?;
                match (entry, account_player_id) {
                    (Some(entry), Some(account_player_id)) => {
                        leases.push(RewardMailDispatchLease {
                            entry,
                            account_player_id,
                            lease_owner: lease_owner.to_string(),
                            attempt_count,
                        })
                    }
                    _ => {
                        sqlx::query(
                            r#"UPDATE reward_mail_outbox SET status = 'manual_review',
                                last_error_code = 'MAIL_OWNER_OR_PAYLOAD_MISSING',
                                lease_owner = NULL, lease_expires_at = NULL,
                                updated_at = current_timestamp
                            WHERE reward_request_id = $1 AND lease_owner = $2"#,
                        )
                        .bind(&request_id)
                        .bind(lease_owner)
                        .execute(&self.pool)
                        .await
                        .map_err(|error| error.to_string())?;
                        METRICS.record_reward_mail_dispatch_manual_review();
                    }
                }
            }
            Ok(leases)
        })
    }

    fn mark_delivered<'a>(
        &'a self,
        lease: &'a RewardMailDispatchLease,
        response: Value,
    ) -> DispatchFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let changed = sqlx::query(
                r#"UPDATE reward_mail_outbox SET status = 'delivered', response_json = $3,
                    delivered_at = current_timestamp, updated_at = current_timestamp,
                    lease_owner = NULL, lease_expires_at = NULL, last_error_code = NULL
                WHERE delivery_request_id = $1 AND status = 'processing' AND lease_owner = $2"#,
            )
            .bind(&lease.entry.delivery_request_id)
            .bind(&lease.lease_owner)
            .bind(response)
            .execute(&self.pool)
            .await
            .map_err(|error| error.to_string())?;
            if changed.rows_affected() == 1 {
                Ok(())
            } else {
                Err("reward mail dispatch lease was lost before ack".to_string())
            }
        })
    }

    fn mark_failed<'a>(
        &'a self,
        lease: &'a RewardMailDispatchLease,
        failure: RewardMailDispatchFailure,
        error_code: &'a str,
        retry_after: Duration,
    ) -> DispatchFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let status = match failure {
                RewardMailDispatchFailure::Retryable => "retryable_failure",
                RewardMailDispatchFailure::Permanent => "permanent_failure",
                RewardMailDispatchFailure::ManualReview => "manual_review",
            };
            let changed = sqlx::query(
                r#"UPDATE reward_mail_outbox SET status = $3, last_error_code = $4,
                    next_attempt_at = current_timestamp + make_interval(secs => $5),
                    lease_owner = NULL, lease_expires_at = NULL, updated_at = current_timestamp
                WHERE delivery_request_id = $1 AND status = 'processing' AND lease_owner = $2"#,
            )
            .bind(&lease.entry.delivery_request_id)
            .bind(&lease.lease_owner)
            .bind(status)
            .bind(error_code)
            .bind(i64::try_from(retry_after.as_secs()).unwrap_or(i64::MAX))
            .execute(&self.pool)
            .await
            .map_err(|error| error.to_string())?;
            if changed.rows_affected() == 1 {
                Ok(())
            } else {
                Err("reward mail dispatch lease was lost before failure persistence".to_string())
            }
        })
    }
}

#[derive(Clone)]
pub(crate) struct RegistryRewardMailClient {
    registry: Arc<RegistryClient>,
    http: reqwest::Client,
    service_token: String,
}

impl RegistryRewardMailClient {
    pub(crate) fn new(
        registry: Arc<RegistryClient>,
        service_token: String,
    ) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(MAIL_DELIVERY_TIMEOUT_SECS))
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            registry,
            http,
            service_token,
        })
    }

    fn payload(lease: &RewardMailDispatchLease) -> Value {
        let entry = &lease.entry;
        json!({
            "delivery_request_id": entry.delivery_request_id,
            "mail_id": entry.mail_id,
            "to_player_id": lease.account_player_id,
            "character_id": entry.character_id,
            "origin_type": entry.order.origin.origin_type.as_str(),
            "origin_id": entry.order.origin.origin_id,
            "delivery_policy": "MAIL_ONLY",
            "title": "Activity reward",
            "content": entry.order.reason,
            "attachments": entry.order.items.iter().map(|item| json!({
                "type": "item",
                "id": item.item_id,
                "count": item.count,
                "binded": !matches!(item.binding, AssetBinding::Unbound),
            })).collect::<Vec<_>>(),
            "operator": {
                "type": match entry.order.operator.operator_type {
                    super::AssetOperatorType::Player => "player",
                    super::AssetOperatorType::Service => "service",
                    super::AssetOperatorType::Gm => "gm",
                    super::AssetOperatorType::System => "system",
                },
                "id": entry.order.operator.operator_id,
                "name": "game-server activity",
            },
        })
    }
}

impl RewardMailDeliveryClient for RegistryRewardMailClient {
    fn deliver<'a>(
        &'a self,
        lease: &'a RewardMailDispatchLease,
    ) -> DispatchFuture<'a, Result<Value, RewardMailDispatchFailure>> {
        Box::pin(async move {
            let endpoint = self
                .registry
                .discover_endpoint("mail-service", "http")
                .await
                .map_err(|_| RewardMailDispatchFailure::Retryable)?
                .ok_or(RewardMailDispatchFailure::Retryable)?;
            let protocol = match endpoint.protocol.to_ascii_lowercase().as_str() {
                "" | "http" => "http",
                "https" => "https",
                _ => return Err(RewardMailDispatchFailure::ManualReview),
            };
            let url = format!(
                "{protocol}://{}:{}/api/v1/mails/reward-deliveries",
                endpoint.host, endpoint.port,
            );
            let response = self
                .http
                .post(url)
                .header("x-service-token", &self.service_token)
                .json(&Self::payload(lease))
                .send()
                .await
                .map_err(|_| RewardMailDispatchFailure::Retryable)?;
            let status = response.status();
            if status.as_u16() == 409 || status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(RewardMailDispatchFailure::ManualReview);
            }
            if status.is_client_error() {
                return Err(RewardMailDispatchFailure::Permanent);
            }
            if !status.is_success() {
                return Err(RewardMailDispatchFailure::Retryable);
            }
            let body = response
                .json::<Value>()
                .await
                .map_err(|_| RewardMailDispatchFailure::Retryable)?;
            if body.get("ok").and_then(Value::as_bool) == Some(true)
                && body.get("mail_id").and_then(Value::as_str) == Some(lease.entry.mail_id.as_str())
                && body.get("delivery_request_id").and_then(Value::as_str)
                    == Some(lease.entry.delivery_request_id.as_str())
            {
                return Ok(body);
            }
            Err(RewardMailDispatchFailure::ManualReview)
        })
    }
}

pub(crate) struct RewardMailDispatcher<S, C> {
    store: S,
    client: C,
    lease_owner: String,
    batch_size: i64,
    lease_seconds: i64,
    max_attempts: i32,
}

impl<S, C> RewardMailDispatcher<S, C>
where
    S: RewardMailDispatchStore,
    C: RewardMailDeliveryClient,
{
    pub(crate) fn new(store: S, client: C, lease_owner: String) -> Self {
        Self {
            store,
            client,
            lease_owner,
            batch_size: DISPATCH_BATCH_SIZE,
            lease_seconds: DISPATCH_LEASE_SECONDS,
            max_attempts: 12,
        }
    }

    async fn process_lease(&self, lease: &RewardMailDispatchLease) -> Result<(), String> {
        match self.client.deliver(lease).await {
            Ok(response) => {
                self.store.mark_delivered(lease, response).await?;
                METRICS.record_reward_mail_dispatched();
            }
            Err(failure) => {
                let attempts_exhausted = failure == RewardMailDispatchFailure::Retryable
                    && lease.attempt_count >= self.max_attempts;
                let failure = if attempts_exhausted {
                    RewardMailDispatchFailure::ManualReview
                } else {
                    failure
                };
                let retry_after = if failure == RewardMailDispatchFailure::Retryable {
                    retry_backoff(lease.attempt_count)
                } else {
                    Duration::ZERO
                };
                let code = match failure {
                    RewardMailDispatchFailure::Retryable => "MAIL_DISPATCH_RETRYABLE",
                    RewardMailDispatchFailure::Permanent => "MAIL_DISPATCH_PERMANENT",
                    RewardMailDispatchFailure::ManualReview if attempts_exhausted => {
                        "MAIL_DISPATCH_ATTEMPTS_EXHAUSTED"
                    }
                    RewardMailDispatchFailure::ManualReview => "MAIL_DISPATCH_MANUAL_REVIEW",
                };
                self.store
                    .mark_failed(lease, failure, code, retry_after)
                    .await?;
                match failure {
                    RewardMailDispatchFailure::Retryable => {
                        METRICS.record_reward_mail_dispatch_retry()
                    }
                    RewardMailDispatchFailure::Permanent
                    | RewardMailDispatchFailure::ManualReview => {
                        METRICS.record_reward_mail_dispatch_manual_review()
                    }
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn run_once(&self) -> Result<usize, String> {
        let leases = self
            .store
            .claim(
                &self.lease_owner,
                self.batch_size,
                self.lease_seconds,
                self.max_attempts,
            )
            .await?;
        let count = leases.len();
        let results = join_all(leases.iter().map(|lease| async move {
            tokio::time::timeout(
                Duration::from_secs(DISPATCH_PROCESSING_BUDGET_SECS),
                self.process_lease(lease),
            )
            .await
            .map_err(|_| "reward mail dispatch processing budget exceeded".to_string())?
        }))
        .await;
        for result in results {
            result?;
        }
        Ok(count)
    }

    pub(crate) async fn run(self, interval: Duration) {
        loop {
            if let Err(error) = self.run_once().await {
                tracing::warn!(error = %error, "reward mail dispatch pass failed");
            }
            tokio::time::sleep(interval).await;
        }
    }
}

fn retry_backoff(attempt_count: i32) -> Duration {
    let exponent = u32::try_from(attempt_count.saturating_sub(1))
        .unwrap_or(0)
        .min(7);
    Duration::from_secs(2_u64.saturating_mul(1_u64 << exponent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::inventory::{
        AssetBinding, AssetOperator, AssetOperatorType, AssetOrigin, AssetOriginType,
        AssetPermission, NormalizedAssetItem, RewardDeliveryPolicy, RewardMailDispatchStatus,
        RewardOrder,
    };
    use std::collections::VecDeque;
    use tokio::sync::Mutex;

    #[derive(Clone)]
    struct MemoryStore {
        state: Arc<Mutex<(Option<RewardMailDispatchLease>, RewardMailDispatchStatus)>>,
    }

    impl RewardMailDispatchStore for MemoryStore {
        fn claim<'a>(
            &'a self,
            owner: &'a str,
            _batch: i64,
            _lease: i64,
            max_attempts: i32,
        ) -> DispatchFuture<'a, Result<Vec<RewardMailDispatchLease>, String>> {
            Box::pin(async move {
                let mut state = self.state.lock().await;
                if !matches!(
                    state.1,
                    RewardMailDispatchStatus::Pending | RewardMailDispatchStatus::RetryableFailure
                ) {
                    return Ok(Vec::new());
                }
                state.1 = RewardMailDispatchStatus::Processing;
                let mut lease = state.0.clone().unwrap();
                if lease.attempt_count >= max_attempts {
                    state.1 = RewardMailDispatchStatus::ManualReview;
                    return Ok(Vec::new());
                }
                lease.lease_owner = owner.to_string();
                lease.attempt_count += 1;
                state.0 = Some(lease.clone());
                Ok(vec![lease])
            })
        }

        fn mark_delivered<'a>(
            &'a self,
            _lease: &'a RewardMailDispatchLease,
            _response: Value,
        ) -> DispatchFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.state.lock().await.1 = RewardMailDispatchStatus::Delivered;
                Ok(())
            })
        }

        fn mark_failed<'a>(
            &'a self,
            _lease: &'a RewardMailDispatchLease,
            failure: RewardMailDispatchFailure,
            _code: &'a str,
            _retry: Duration,
        ) -> DispatchFuture<'a, Result<(), String>> {
            Box::pin(async move {
                self.state.lock().await.1 = match failure {
                    RewardMailDispatchFailure::Retryable => {
                        RewardMailDispatchStatus::RetryableFailure
                    }
                    RewardMailDispatchFailure::Permanent => {
                        RewardMailDispatchStatus::PermanentFailure
                    }
                    RewardMailDispatchFailure::ManualReview => {
                        RewardMailDispatchStatus::ManualReview
                    }
                };
                Ok(())
            })
        }
    }

    #[derive(Clone)]
    struct FakeClient {
        outcomes: Arc<Mutex<VecDeque<Result<Value, RewardMailDispatchFailure>>>>,
        calls: Arc<Mutex<usize>>,
    }

    impl RewardMailDeliveryClient for FakeClient {
        fn deliver<'a>(
            &'a self,
            _lease: &'a RewardMailDispatchLease,
        ) -> DispatchFuture<'a, Result<Value, RewardMailDispatchFailure>> {
            Box::pin(async move {
                *self.calls.lock().await += 1;
                self.outcomes.lock().await.pop_front().unwrap()
            })
        }
    }

    fn fixture() -> (MemoryStore, RewardMailDispatchLease) {
        let order = RewardOrder::new(
            "activity_claim:test",
            "character-1",
            AssetOrigin::new(AssetOriginType::Activity, "activity:test").unwrap(),
            RewardDeliveryPolicy::PreferInventory,
            &[NormalizedAssetItem::new(1001, 1, AssetBinding::Unbound).unwrap()],
            "activity reward",
            AssetOperator::new(
                AssetOperatorType::Service,
                "game-server.activity",
                [AssetPermission::Grant],
            )
            .unwrap(),
        )
        .unwrap();
        let lease = RewardMailDispatchLease {
            entry: RewardMailOutboxEntry::for_order(&order),
            account_player_id: "player-1".into(),
            lease_owner: String::new(),
            attempt_count: 0,
        };
        let store = MemoryStore {
            state: Arc::new(Mutex::new((
                Some(lease.clone()),
                RewardMailDispatchStatus::Pending,
            ))),
        };
        (store, lease)
    }

    #[tokio::test]
    async fn response_loss_retries_idempotently_and_only_then_marks_delivered() {
        let (store, _) = fixture();
        let calls = Arc::new(Mutex::new(0));
        let client = FakeClient {
            outcomes: Arc::new(Mutex::new(VecDeque::from([
                Err(RewardMailDispatchFailure::Retryable),
                Ok(json!({"ok": true})),
            ]))),
            calls: calls.clone(),
        };
        let first = RewardMailDispatcher::new(store.clone(), client.clone(), "worker-1".into());
        first.run_once().await.unwrap();
        assert_eq!(
            store.state.lock().await.1,
            RewardMailDispatchStatus::RetryableFailure
        );

        let restarted = RewardMailDispatcher::new(store.clone(), client, "worker-2".into());
        restarted.run_once().await.unwrap();
        assert_eq!(
            store.state.lock().await.1,
            RewardMailDispatchStatus::Delivered
        );
        assert_eq!(*calls.lock().await, 2);
    }

    #[tokio::test]
    async fn permanent_failure_reaches_terminal_state_without_reclaim() {
        let (store, _) = fixture();
        let client = FakeClient {
            outcomes: Arc::new(Mutex::new(VecDeque::from([Err(
                RewardMailDispatchFailure::Permanent,
            )]))),
            calls: Arc::new(Mutex::new(0)),
        };
        let dispatcher = RewardMailDispatcher::new(store.clone(), client, "worker-1".into());
        dispatcher.run_once().await.unwrap();
        assert_eq!(
            store.state.lock().await.1,
            RewardMailDispatchStatus::PermanentFailure
        );
        assert_eq!(dispatcher.run_once().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn concurrent_dispatchers_cannot_claim_the_same_mail() {
        let (store, _) = fixture();
        let calls = Arc::new(Mutex::new(0));
        let client = FakeClient {
            outcomes: Arc::new(Mutex::new(VecDeque::from([Ok(json!({"ok": true}))]))),
            calls: calls.clone(),
        };
        let first = RewardMailDispatcher::new(store.clone(), client.clone(), "worker-1".into());
        let second = RewardMailDispatcher::new(store.clone(), client, "worker-2".into());

        let (first_count, second_count) = tokio::join!(first.run_once(), second.run_once());

        assert_eq!(first_count.unwrap() + second_count.unwrap(), 1);
        assert_eq!(*calls.lock().await, 1);
        assert_eq!(
            store.state.lock().await.1,
            RewardMailDispatchStatus::Delivered
        );
    }

    #[tokio::test]
    async fn retryable_failure_at_attempt_limit_requires_manual_review() {
        let (store, _) = fixture();
        store.state.lock().await.0.as_mut().unwrap().attempt_count = 11;
        let client = FakeClient {
            outcomes: Arc::new(Mutex::new(VecDeque::from([Err(
                RewardMailDispatchFailure::Retryable,
            )]))),
            calls: Arc::new(Mutex::new(0)),
        };
        let dispatcher = RewardMailDispatcher::new(store.clone(), client, "worker-1".into());

        assert_eq!(dispatcher.run_once().await.unwrap(), 1);
        assert_eq!(
            store.state.lock().await.1,
            RewardMailDispatchStatus::ManualReview
        );
        assert_eq!(dispatcher.run_once().await.unwrap(), 0);
    }

    #[test]
    fn retry_backoff_is_bounded_and_uses_attempt_count() {
        assert_eq!(retry_backoff(1), Duration::from_secs(2));
        assert_eq!(retry_backoff(2), Duration::from_secs(4));
        assert_eq!(retry_backoff(8), Duration::from_secs(256));
        assert_eq!(retry_backoff(i32::MAX), Duration::from_secs(256));
    }

    #[test]
    fn concurrent_dispatch_budget_finishes_before_lease_expiry() {
        assert!(MAIL_DELIVERY_TIMEOUT_SECS < DISPATCH_PROCESSING_BUDGET_SECS);
        assert!(DISPATCH_PROCESSING_BUDGET_SECS < DISPATCH_LEASE_SECONDS as u64);
        assert!(DISPATCH_LEASE_SECONDS as u64 - DISPATCH_PROCESSING_BUDGET_SECS >= 10);
        assert!(DISPATCH_BATCH_SIZE > 1);
    }
}
