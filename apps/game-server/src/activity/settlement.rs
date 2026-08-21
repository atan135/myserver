use crate::core::inventory::{
    AssetBinding, AssetCommandErrorCode, AssetOperator, AssetOperatorType, AssetOrigin,
    AssetOriginType, AssetPermission, AssetResultState, NormalizedAssetItem, RewardDeliveryPolicy,
    RewardDeliveryResult, RewardOrder,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimStatus {
    Processing,
    Granted,
    RetryableFailure,
    ReconciliationPending,
    ManualReview,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityClaimRecord {
    pub(crate) character_id: String,
    pub(crate) activity_id: String,
    pub(crate) version: i32,
    pub(crate) semantic_claim_key: String,
    pub(crate) client_request_id: String,
    pub(crate) reward_request_id: String,
    pub(crate) order: RewardOrder,
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

pub(crate) trait ActivityRewardDelivery: Send + Sync {
    fn deliver<'a>(
        &'a self,
        order: RewardOrder,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ActivityDeliveryOutcome, String>> + Send + 'a>,
    >;
}

#[derive(Clone)]
pub(crate) struct ActivityClaimCoordinator {
    delivery: Arc<dyn ActivityRewardDelivery>,
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
        Self {
            delivery,
            state: Arc::new(Mutex::new(ClaimState::default())),
        }
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
        let semantic_index = semantic_index(character_id, activity_id, version, semantic_claim_key);
        let request_index = request_index(character_id, client_request_id);
        let activity_index = format!("{character_id}\0{activity_id}");
        let mut state = self.state.lock().await;
        if let Some(player_state) = state.player_state.get(&activity_index)
            && player_state.version != version
        {
            drop(state);
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
        if let Some(existing_semantic) = state.request_index.get(&request_index)
            && existing_semantic != &semantic_index
        {
            drop(state);
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
        if let Some(existing) = state.by_semantic.get(&semantic_index).cloned() {
            if existing.status == ClaimStatus::RetryableFailure {
                let mut retry = existing;
                retry.status = ClaimStatus::Processing;
                retry.client_request_id = client_request_id.to_string();
                state
                    .request_index
                    .insert(request_index, semantic_index.clone());
                state.by_semantic.insert(semantic_index.clone(), retry);
            } else {
                return ClaimSettlement {
                    status: existing.status,
                    result: existing.result,
                    duplicate: true,
                    notification_failed: existing.notification_failed,
                };
            }
        } else {
            let next_revision = state
                .player_state
                .get(&activity_index)
                .map_or(1, |value| value.state_revision.saturating_add(1));
            state.player_state.insert(
                activity_index,
                ActivityPlayerStateRecord {
                    version,
                    current_stage_id: semantic_claim_key.to_string(),
                    state_revision: next_revision,
                },
            );
            let record = ActivityClaimRecord {
                character_id: character_id.to_string(),
                activity_id: activity_id.to_string(),
                version,
                semantic_claim_key: semantic_claim_key.to_string(),
                client_request_id: client_request_id.to_string(),
                reward_request_id: order.request_id.clone(),
                order: order.clone(),
                status: ClaimStatus::Processing,
                result: None,
                notification_failed: false,
            };
            state
                .request_index
                .insert(request_index, semantic_index.clone());
            state.by_semantic.insert(semantic_index.clone(), record);
        }
        drop(state);

        self.deliver_and_finish(&semantic_index, order).await
    }

    pub(crate) async fn reconcile(
        &self,
        character_id: &str,
        activity_id: &str,
        version: i32,
        semantic_claim_key: &str,
    ) -> ClaimSettlement {
        let semantic_index = semantic_index(character_id, activity_id, version, semantic_claim_key);
        let order = {
            let state = self.state.lock().await;
            let Some(record) = state.by_semantic.get(&semantic_index) else {
                return ClaimSettlement::manual_review();
            };
            if record.status != ClaimStatus::ReconciliationPending {
                return ClaimSettlement {
                    status: record.status,
                    result: record.result.clone(),
                    duplicate: true,
                    notification_failed: record.notification_failed,
                };
            }
            record.order.clone()
        };
        self.deliver_and_finish(&semantic_index, order).await
    }

    async fn deliver_and_finish(
        &self,
        semantic_index: &str,
        order: RewardOrder,
    ) -> ClaimSettlement {
        let outcome = match self.delivery.deliver(order).await {
            Ok(outcome) => outcome,
            Err(_error) => {
                self.finish(&semantic_index, ClaimStatus::RetryableFailure, None, false)
                    .await;
                return ClaimSettlement {
                    status: ClaimStatus::RetryableFailure,
                    result: None,
                    duplicate: false,
                    notification_failed: false,
                };
            }
        };
        let status = match outcome.result.result_state {
            AssetResultState::Applied => ClaimStatus::Granted,
            AssetResultState::Unknown => ClaimStatus::ReconciliationPending,
            AssetResultState::NotApplied => ClaimStatus::RetryableFailure,
        };
        self.finish(
            &semantic_index,
            status,
            Some(outcome.result.clone()),
            outcome.notification_failed,
        )
        .await;
        ClaimSettlement {
            status,
            result: Some(outcome.result),
            duplicate: false,
            notification_failed: outcome.notification_failed,
        }
    }

    async fn finish(
        &self,
        semantic_index: &str,
        status: ClaimStatus,
        result: Option<RewardDeliveryResult>,
        notification_failed: bool,
    ) {
        let mut state = self.state.lock().await;
        if let Some(record) = state.by_semantic.get_mut(semantic_index) {
            record.status = status;
            record.result = result;
            record.notification_failed = notification_failed;
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
        let semantic_index = semantic_index(character_id, activity_id, version, semantic_claim_key);
        let request_index = request_index(character_id, client_request_id);
        let record = ActivityClaimRecord {
            character_id: character_id.to_string(),
            activity_id: activity_id.to_string(),
            version,
            semantic_claim_key: semantic_claim_key.to_string(),
            client_request_id: client_request_id.to_string(),
            reward_request_id: order.request_id.clone(),
            order,
            status: ClaimStatus::ManualReview,
            result: None,
            notification_failed: false,
        };
        let mut state = self.state.lock().await;
        state
            .request_index
            .entry(request_index)
            .or_insert_with(|| semantic_index.clone());
        state.by_semantic.insert(semantic_index, record);
    }

    #[cfg(test)]
    async fn record(
        &self,
        character_id: &str,
        activity_id: &str,
        version: i32,
        semantic_claim_key: &str,
    ) -> Option<ActivityClaimRecord> {
        self.state
            .lock()
            .await
            .by_semantic
            .get(&semantic_index(
                character_id,
                activity_id,
                version,
                semantic_claim_key,
            ))
            .cloned()
    }

    #[cfg(test)]
    async fn state_revision(&self, character_id: &str, activity_id: &str) -> Option<u64> {
        self.state
            .lock()
            .await
            .player_state
            .get(&format!("{character_id}\0{activity_id}"))
            .map(|state| state.state_revision)
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
    let origin = AssetOrigin::new(
        AssetOriginType::Activity,
        format!("{activity_id}:{version}:{semantic_claim_key}"),
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
        format!("activity claim {activity_id} {semantic_claim_key}"),
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
    use crate::core::inventory::{AssetCommandResult, AssetRequestFingerprint};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Barrier;

    #[derive(Clone)]
    struct FakeDelivery {
        calls: Arc<AtomicUsize>,
        result: AssetResultState,
    }

    impl ActivityRewardDelivery for FakeDelivery {
        fn deliver<'a>(
            &'a self,
            order: RewardOrder,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<ActivityDeliveryOutcome, String>>
                    + Send
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
                        AssetCommandErrorCode::InventoryCapacityFull,
                    ),
                }
                .map_err(|error| format!("invalid fake result: {error:?}"))?;
                Ok(ActivityDeliveryOutcome {
                    result,
                    notification_failed: false,
                })
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
                "req-2",
                order("stage-1"),
            )
            .await;
        assert_eq!(first.status, ClaimStatus::Granted);
        assert!(duplicate.duplicate);
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
                    "req-2",
                    order("stage-1"),
                )
                .await
        };
        let _ = tokio::join!(left, right);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unknown_result_is_reconciliation_pending_and_not_retried() {
        let calls = Arc::new(AtomicUsize::new(0));
        let coordinator = ActivityClaimCoordinator::new(Arc::new(FakeDelivery {
            calls: calls.clone(),
            result: AssetResultState::Unknown,
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
        let retry = coordinator
            .settle(
                "character-1",
                "activity-1",
                1,
                "stage-1",
                "req-2",
                order("stage-1"),
            )
            .await;
        assert_eq!(first.status, ClaimStatus::ReconciliationPending);
        assert_eq!(retry.status, ClaimStatus::ReconciliationPending);
        assert!(retry.duplicate);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
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
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
