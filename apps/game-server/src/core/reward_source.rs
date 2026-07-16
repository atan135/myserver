use std::collections::HashMap;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::core::inventory::{
    AssetBatchAtomicity, AssetCommand, AssetCommandErrorCode, AssetConsumption, AssetOperator,
    AssetOperatorType, AssetOrigin, AssetOriginType, AssetPermission, AssetResultState,
    NormalizedAssetItem, RewardDeliveryError, RewardDeliveryNotifier, RewardDeliveryPolicy,
    RewardDeliveryResult, RewardDeliveryService, RewardDeliveryStore, RewardInventoryPort,
    RewardOrder,
};

/// Server-owned business domains which may create an item reward. The serialized `AssetOrigin`
/// type remains deliberately smaller: task and quest share its `quest` audit category, while
/// their canonical origin IDs remain distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RewardSourceKind {
    Achievement,
    Task,
    Quest,
    Battle,
    ScenePickup,
    Activity,
    Ranking,
    WorldEvent,
}

impl RewardSourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Achievement => "achievement",
            Self::Task => "task",
            Self::Quest => "quest",
            Self::Battle => "battle",
            Self::ScenePickup => "scene_pickup",
            Self::Activity => "activity",
            Self::Ranking => "ranking",
            Self::WorldEvent => "world_event",
        }
    }

    pub const fn asset_origin_type(self) -> AssetOriginType {
        match self {
            Self::Achievement => AssetOriginType::Achievement,
            Self::Task | Self::Quest => AssetOriginType::Quest,
            Self::Battle => AssetOriginType::Battle,
            Self::ScenePickup => AssetOriginType::ScenePickup,
            Self::Activity => AssetOriginType::Activity,
            Self::Ranking => AssetOriginType::Ranking,
            Self::WorldEvent => AssetOriginType::WorldEvent,
        }
    }

    pub const fn default_delivery_policy(self) -> RewardDeliveryPolicy {
        match self {
            // Ranking settlement is normally asynchronous. It is intentionally mail-backed
            // rather than depending on a character being online with spare inventory capacity.
            Self::Ranking => RewardDeliveryPolicy::MailOnly,
            Self::Achievement
            | Self::Task
            | Self::Quest
            | Self::Battle
            | Self::ScenePickup
            | Self::Activity
            | Self::WorldEvent => RewardDeliveryPolicy::PreferInventory,
        }
    }

    pub fn from_character_progress_source(source_type: &str) -> Option<Self> {
        match source_type.trim().to_ascii_lowercase().as_str() {
            "achievement" => Some(Self::Achievement),
            "task" => Some(Self::Task),
            "quest" => Some(Self::Quest),
            "activity" => Some(Self::Activity),
            "ranking" => Some(Self::Ranking),
            "world_event" => Some(Self::WorldEvent),
            _ => None,
        }
    }
}

/// A stable reward origin made from a server-side business key. `origin_id` is canonical and
/// never comes from a client reward payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewardSource {
    kind: RewardSourceKind,
    origin: AssetOrigin,
}

impl RewardSource {
    pub fn from_server_id(
        kind: RewardSourceKind,
        business_id: impl AsRef<str>,
    ) -> Result<Self, RewardSourceError> {
        let business_id = business_id.as_ref();
        validate_business_id(business_id)?;
        let origin_id = format!("{}:{business_id}", kind.as_str());
        let origin = AssetOrigin::new(kind.asset_origin_type(), origin_id)
            .map_err(|_| RewardSourceError::InvalidBusinessId)?;
        Ok(Self { kind, origin })
    }

    /// Maps the existing `CharacterProgressTable` source fields to the canonical source ID used
    /// by item rewards. The protocol still submits only `ProgressId`; the table supplies this
    /// business key after the server has authenticated the character.
    pub fn from_character_progress(
        source_type: &str,
        source_id: impl AsRef<str>,
    ) -> Result<Self, RewardSourceError> {
        let kind = RewardSourceKind::from_character_progress_source(source_type)
            .ok_or(RewardSourceError::UnsupportedProgressSource)?;
        Self::from_server_id(kind, source_id)
    }

    pub const fn kind(&self) -> RewardSourceKind {
        self.kind
    }

    pub fn origin(&self) -> &AssetOrigin {
        &self.origin
    }

    pub fn canonical_origin_id(&self) -> &str {
        &self.origin.origin_id
    }
}

fn validate_business_id(value: &str) -> Result<(), RewardSourceError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed != value
        || trimmed.len() > 96
        || value.chars().any(char::is_whitespace)
    {
        return Err(RewardSourceError::InvalidBusinessId);
    }
    Ok(())
}

/// Trusted server-side claim intent. The only client-facing equivalent in this repository is
/// `ApplyCharacterProgressReq { progress_id }`; it has no item, policy, origin, or success field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewardSourceClaim {
    pub character_id: String,
    pub source: RewardSource,
    pub items: Vec<NormalizedAssetItem>,
    pub delivery_policy: RewardDeliveryPolicy,
    pub reason: String,
}

impl RewardSourceClaim {
    pub fn new(
        character_id: impl Into<String>,
        source: RewardSource,
        items: Vec<NormalizedAssetItem>,
        reason: impl Into<String>,
    ) -> Result<Self, RewardSourceError> {
        let character_id = character_id.into();
        if character_id.trim().is_empty() || character_id.len() > 128 {
            return Err(RewardSourceError::InvalidCharacterId);
        }
        if items.is_empty() {
            return Err(RewardSourceError::EmptyItems);
        }
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Err(RewardSourceError::InvalidReason);
        }
        Ok(Self {
            character_id,
            delivery_policy: source.kind.default_delivery_policy(),
            source,
            items,
            reason,
        })
    }

    pub fn request_id(&self) -> String {
        // The source record is character-scoped. Hashing keeps the durable request ID below the
        // v1 schema limit even for a long, but valid, canonical origin ID.
        let input = format!(
            "reward-source-v1:{}:{}:{}",
            self.character_id,
            self.source.kind.as_str(),
            self.source.canonical_origin_id()
        );
        let digest = format!("{:x}", Sha256::digest(input.as_bytes()));
        format!("reward:{}:{}", self.source.kind.as_str(), &digest[..48])
    }

    pub fn state_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.character_id,
            self.source.kind.as_str(),
            self.source.canonical_origin_id()
        )
    }

    pub fn into_reward_order(self) -> Result<RewardOrder, RewardSourceError> {
        RewardOrder::new(
            self.request_id(),
            self.character_id,
            self.source.origin,
            self.delivery_policy,
            &self.items,
            self.reason,
            AssetOperator::new(
                AssetOperatorType::Service,
                format!("{}-reward-source", self.source.kind.as_str()),
                [AssetPermission::Grant],
            )
            .map_err(RewardSourceError::InvalidAssetContract)?,
        )
        .map_err(RewardSourceError::InvalidAssetContract)
    }
}

/// Object-ID-only UI intent. A source adapter resolves this against server-side progress/config
/// state before it may construct a [`RewardSourceClaim`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiRewardClaimRequest {
    pub business_object_id: String,
}

impl UiRewardClaimRequest {
    pub fn new(business_object_id: impl Into<String>) -> Result<Self, RewardSourceError> {
        let business_object_id = business_object_id.into();
        validate_business_id(&business_object_id)?;
        Ok(Self { business_object_id })
    }
}

/// Adapter over the shared delivery service. Source modules are unable to write player inventory
/// or create a mail themselves; they only submit a `RewardSourceClaim` here.
pub trait RewardDeliveryGateway: Send + Sync {
    async fn deliver_reward(
        &self,
        order: RewardOrder,
    ) -> Result<RewardDeliveryResult, RewardDeliveryError>;
}

impl<I, S, N> RewardDeliveryGateway for RewardDeliveryService<I, S, N>
where
    I: RewardInventoryPort,
    S: RewardDeliveryStore,
    N: RewardDeliveryNotifier,
{
    async fn deliver_reward(
        &self,
        order: RewardOrder,
    ) -> Result<RewardDeliveryResult, RewardDeliveryError> {
        self.deliver(order).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedSourceClaim {
    pub delivery: RewardDeliveryResult,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewardSourceReservation {
    Acquired,
    Completed(CompletedSourceClaim),
    InProgress,
}

/// A source's state must only become complete after `RewardDeliveryService` has a committed
/// direct or mail result. Production implementations should persist this next to their own
/// achievement/task/drop state; the in-memory implementation exists for deterministic tests.
pub trait RewardSourceStateStore: Send + Sync {
    async fn reserve(&self, key: &str) -> Result<RewardSourceReservation, String>;
    async fn complete(&self, key: &str, completed: CompletedSourceClaim) -> Result<(), String>;
    async fn release(&self, key: &str) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RewardSourceDeliveryOutcome {
    pub delivery: RewardDeliveryResult,
    pub source_completed: bool,
    pub replayed_source_completion: bool,
}

pub struct RewardSourceService<D, S> {
    delivery: D,
    state: S,
}

impl<D, S> RewardSourceService<D, S>
where
    D: RewardDeliveryGateway,
    S: RewardSourceStateStore,
{
    pub fn new(delivery: D, state: S) -> Self {
        Self { delivery, state }
    }

    pub async fn deliver_claim(
        &self,
        claim: RewardSourceClaim,
    ) -> Result<RewardSourceDeliveryOutcome, RewardSourceError> {
        let state_key = claim.state_key();
        match self
            .state
            .reserve(&state_key)
            .await
            .map_err(RewardSourceError::StateUnavailable)?
        {
            RewardSourceReservation::Completed(completed) => {
                return Ok(RewardSourceDeliveryOutcome {
                    delivery: completed.delivery,
                    source_completed: true,
                    replayed_source_completion: true,
                });
            }
            RewardSourceReservation::InProgress => return Err(RewardSourceError::ClaimInProgress),
            RewardSourceReservation::Acquired => {}
        }

        let delivery = match self
            .delivery
            .deliver_reward(claim.into_reward_order()?)
            .await
        {
            Ok(delivery) => delivery,
            Err(error) => {
                let _ = self.state.release(&state_key).await;
                return Err(RewardSourceError::Delivery(error));
            }
        };

        if delivery.result_state != AssetResultState::Applied {
            self.state
                .release(&state_key)
                .await
                .map_err(RewardSourceError::StateUnavailable)?;
            return Ok(RewardSourceDeliveryOutcome {
                delivery,
                source_completed: false,
                replayed_source_completion: false,
            });
        }

        if let Err(error) = self
            .state
            .complete(
                &state_key,
                CompletedSourceClaim {
                    delivery: delivery.clone(),
                },
            )
            .await
        {
            // The reward request is idempotent. Releasing lets a later source retry query that
            // committed result and finish its own state transition rather than becoming stuck.
            let _ = self.state.release(&state_key).await;
            return Err(RewardSourceError::StateUnavailable(error));
        }

        Ok(RewardSourceDeliveryOutcome {
            delivery,
            source_completed: true,
            replayed_source_completion: false,
        })
    }
}

#[derive(Clone, Default)]
pub struct InMemoryRewardSourceStateStore {
    state: Arc<Mutex<HashMap<String, InMemorySourceClaimState>>>,
}

#[derive(Clone)]
enum InMemorySourceClaimState {
    InProgress,
    Completed(CompletedSourceClaim),
}

impl RewardSourceStateStore for InMemoryRewardSourceStateStore {
    async fn reserve(&self, key: &str) -> Result<RewardSourceReservation, String> {
        let mut state = self.state.lock().await;
        match state.get(key) {
            Some(InMemorySourceClaimState::Completed(completed)) => {
                Ok(RewardSourceReservation::Completed(completed.clone()))
            }
            Some(InMemorySourceClaimState::InProgress) => Ok(RewardSourceReservation::InProgress),
            None => {
                state.insert(key.to_string(), InMemorySourceClaimState::InProgress);
                Ok(RewardSourceReservation::Acquired)
            }
        }
    }

    async fn complete(&self, key: &str, completed: CompletedSourceClaim) -> Result<(), String> {
        let mut state = self.state.lock().await;
        match state.get(key) {
            Some(InMemorySourceClaimState::InProgress) => {
                state.insert(
                    key.to_string(),
                    InMemorySourceClaimState::Completed(completed),
                );
                Ok(())
            }
            Some(InMemorySourceClaimState::Completed(_)) => {
                Err("source claim was already completed".to_string())
            }
            None => Err("source claim reservation disappeared".to_string()),
        }
    }

    async fn release(&self, key: &str) -> Result<(), String> {
        let mut state = self.state.lock().await;
        if matches!(state.get(key), Some(InMemorySourceClaimState::InProgress)) {
            state.remove(key);
        }
        Ok(())
    }
}

/// A battle reward can only be created from server simulation output. No player protocol in this
/// repository carries `BattleServerResult`, reward items, or a client-declared success flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattleServerResult {
    settlement_id: String,
    character_id: String,
    items: Vec<NormalizedAssetItem>,
}

impl BattleServerResult {
    pub(crate) fn from_authoritative_simulation(
        settlement_id: impl AsRef<str>,
        character_id: impl Into<String>,
        items: Vec<NormalizedAssetItem>,
    ) -> Result<Self, RewardSourceError> {
        validate_business_id(settlement_id.as_ref())?;
        if items.is_empty() {
            return Err(RewardSourceError::EmptyItems);
        }
        Ok(Self {
            settlement_id: settlement_id.as_ref().to_string(),
            character_id: character_id.into(),
            items,
        })
    }

    pub(crate) fn into_reward_claim(self) -> Result<RewardSourceClaim, RewardSourceError> {
        RewardSourceClaim::new(
            self.character_id,
            RewardSource::from_server_id(RewardSourceKind::Battle, self.settlement_id)?,
            self.items,
            "authoritative battle settlement reward",
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScenePosition {
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

impl ScenePosition {
    fn squared_distance_to(self, other: Self) -> u128 {
        let dx = i128::from(self.x) - i128::from(other.x);
        let dy = i128::from(self.y) - i128::from(other.y);
        let dz = i128::from(self.z) - i128::from(other.z);
        (dx * dx + dy * dy + dz * dz) as u128
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenePickupRequest {
    pub drop_entity_id: String,
}

impl ScenePickupRequest {
    pub fn new(drop_entity_id: impl Into<String>) -> Result<Self, RewardSourceError> {
        let drop_entity_id = drop_entity_id.into();
        validate_business_id(&drop_entity_id)?;
        Ok(Self { drop_entity_id })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneDrop {
    drop_entity_id: String,
    owner_character_id: Option<String>,
    position: ScenePosition,
    pickup_radius: u64,
    items: Vec<NormalizedAssetItem>,
    removed: bool,
}

impl SceneDrop {
    pub(crate) fn new(
        drop_entity_id: impl Into<String>,
        owner_character_id: Option<String>,
        position: ScenePosition,
        pickup_radius: u64,
        items: Vec<NormalizedAssetItem>,
    ) -> Result<Self, RewardSourceError> {
        let drop_entity_id = drop_entity_id.into();
        validate_business_id(&drop_entity_id)?;
        if pickup_radius == 0 || items.is_empty() {
            return Err(RewardSourceError::InvalidDrop);
        }
        Ok(Self {
            drop_entity_id,
            owner_character_id,
            position,
            pickup_radius,
            items,
            removed: false,
        })
    }

    /// Validates the authoritative drop entity, ownership, radius and current state. It does not
    /// remove the drop: callers must first deliver the returned claim and call
    /// `remove_after_delivery` with a committed result.
    pub(crate) fn prepare_pickup(
        &self,
        character_id: &str,
        request: &ScenePickupRequest,
        character_position: ScenePosition,
    ) -> Result<RewardSourceClaim, RewardSourceError> {
        if request.drop_entity_id != self.drop_entity_id {
            return Err(RewardSourceError::DropNotFound);
        }
        if self.removed {
            return Err(RewardSourceError::DropAlreadyRemoved);
        }
        if self
            .owner_character_id
            .as_deref()
            .is_some_and(|owner| owner != character_id)
        {
            return Err(RewardSourceError::DropNotOwned);
        }
        let radius_sq = u128::from(self.pickup_radius) * u128::from(self.pickup_radius);
        if self.position.squared_distance_to(character_position) > radius_sq {
            return Err(RewardSourceError::DropOutOfRange);
        }
        RewardSourceClaim::new(
            character_id,
            RewardSource::from_server_id(RewardSourceKind::ScenePickup, &self.drop_entity_id)?,
            self.items.clone(),
            "authoritative scene pickup reward",
        )
    }

    pub(crate) fn remove_after_delivery(
        &mut self,
        claim: &RewardSourceClaim,
        outcome: &RewardSourceDeliveryOutcome,
    ) -> Result<(), RewardSourceError> {
        if claim.source.kind != RewardSourceKind::ScenePickup
            || claim.source.canonical_origin_id()
                != RewardSource::from_server_id(
                    RewardSourceKind::ScenePickup,
                    &self.drop_entity_id,
                )?
                .canonical_origin_id()
        {
            return Err(RewardSourceError::InvalidDrop);
        }
        let order = claim.clone().into_reward_order()?;
        if !outcome.source_completed
            || outcome.delivery.result_state != AssetResultState::Applied
            || outcome.delivery.request_id != order.request_id
            || outcome.delivery.request_fingerprint != order.request_fingerprint()
        {
            return Err(RewardSourceError::DeliveryNotCommitted);
        }
        self.removed = true;
        Ok(())
    }

    pub(crate) const fn is_removed(&self) -> bool {
        self.removed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetExchangeKind {
    StorePurchase,
    Crafting,
    Redemption,
}

impl AssetExchangeKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::StorePurchase => "store_purchase",
            Self::Crafting => "crafting",
            Self::Redemption => "redemption",
        }
    }
}

/// Contract for future shop/crafting/redemption modules. Currency settlement has not been
/// implemented in this repository, so this presently expresses item material consumption and
/// output grant as one `AssetCommand` batch. It has no mail fallback path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventoryRequiredExchange {
    pub command: AssetCommand,
    pub delivery_policy: RewardDeliveryPolicy,
}

impl InventoryRequiredExchange {
    pub fn new(
        kind: AssetExchangeKind,
        exchange_id: impl AsRef<str>,
        character_id: impl Into<String>,
        consumed_assets: Vec<AssetConsumption>,
        produced_items: Vec<NormalizedAssetItem>,
    ) -> Result<Self, AssetCommandErrorCode> {
        let exchange_id = exchange_id.as_ref();
        if validate_business_id(exchange_id).is_err() {
            return Err(AssetCommandErrorCode::InvalidOrigin);
        }
        let character_id = character_id.into();
        let origin = AssetOrigin::new(
            AssetOriginType::PlayerOperation,
            format!("{}:{exchange_id}", kind.as_str()),
        )
        .map_err(|_| AssetCommandErrorCode::InvalidOrigin)?;
        let request_seed = format!(
            "exchange-v1:{}:{}:{}",
            kind.as_str(),
            character_id,
            exchange_id
        );
        let request_digest = format!("{:x}", Sha256::digest(request_seed.as_bytes()));
        let command = AssetCommand::new(
            format!("exchange:{}:{}", kind.as_str(), &request_digest[..48]),
            character_id,
            origin,
            format!("{} asset exchange", kind.as_str()),
            AssetOperator::new(
                AssetOperatorType::Service,
                format!("{}-service", kind.as_str()),
                [AssetPermission::Consume, AssetPermission::Grant],
            )?,
            Vec::new(),
            vec![
                crate::core::inventory::AssetOperation::Consume {
                    assets: consumed_assets,
                },
                crate::core::inventory::AssetOperation::Grant {
                    items: produced_items,
                },
            ],
        )?;
        Ok(Self {
            command,
            delivery_policy: RewardDeliveryPolicy::InventoryRequired,
        })
    }
}

#[derive(Debug)]
pub enum RewardSourceError {
    InvalidBusinessId,
    InvalidCharacterId,
    EmptyItems,
    InvalidReason,
    UnsupportedProgressSource,
    InvalidAssetContract(AssetCommandErrorCode),
    StateUnavailable(String),
    ClaimInProgress,
    Delivery(RewardDeliveryError),
    InvalidDrop,
    DropNotFound,
    DropAlreadyRemoved,
    DropNotOwned,
    DropOutOfRange,
    DeliveryNotCommitted,
}

impl std::fmt::Display for RewardSourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBusinessId => formatter.write_str("invalid reward source business id"),
            Self::InvalidCharacterId => formatter.write_str("invalid reward character id"),
            Self::EmptyItems => formatter.write_str("reward source items must not be empty"),
            Self::InvalidReason => formatter.write_str("reward source reason must not be empty"),
            Self::UnsupportedProgressSource => {
                formatter.write_str("unsupported character-progress reward source")
            }
            Self::InvalidAssetContract(error) => {
                write!(formatter, "invalid asset contract: {error:?}")
            }
            Self::StateUnavailable(error) => {
                write!(formatter, "reward source state unavailable: {error}")
            }
            Self::ClaimInProgress => formatter.write_str("reward source claim already in progress"),
            Self::Delivery(error) => write!(formatter, "reward delivery failed: {error}"),
            Self::InvalidDrop => formatter.write_str("invalid scene drop"),
            Self::DropNotFound => formatter.write_str("scene drop entity not found"),
            Self::DropAlreadyRemoved => formatter.write_str("scene drop was already removed"),
            Self::DropNotOwned => formatter.write_str("scene drop is not owned by character"),
            Self::DropOutOfRange => formatter.write_str("scene drop is out of pickup range"),
            Self::DeliveryNotCommitted => {
                formatter.write_str("scene pickup delivery was not committed")
            }
        }
    }
}

impl std::error::Error for RewardSourceError {}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::core::inventory::{
        AssetBinding, AssetCommandResult, AssetDeliveryMethod, AssetDeliveryReceipt,
        AssetFallbackReason, AssetRequestFingerprint,
    };

    #[derive(Clone, Default)]
    struct FakeGateway {
        results: Arc<Mutex<VecDeque<RewardDeliveryResult>>>,
        calls: Arc<AtomicUsize>,
    }

    impl FakeGateway {
        async fn push(&self, result: RewardDeliveryResult) {
            self.results.lock().await.push_back(result);
        }
    }

    impl RewardDeliveryGateway for FakeGateway {
        async fn deliver_reward(
            &self,
            _order: RewardOrder,
        ) -> Result<RewardDeliveryResult, RewardDeliveryError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.results.lock().await.pop_front().ok_or_else(|| {
                RewardDeliveryError::InventoryUnavailable("missing fake result".to_string())
            })
        }
    }

    #[derive(Clone)]
    struct DeterministicInventory {
        capacity_full: bool,
        execute_calls: Arc<AtomicUsize>,
    }

    impl DeterministicInventory {
        fn direct() -> Self {
            Self {
                capacity_full: false,
                execute_calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn capacity_full() -> Self {
            Self {
                capacity_full: true,
                execute_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl RewardInventoryPort for DeterministicInventory {
        async fn execute_reward(
            &self,
            order: &RewardOrder,
        ) -> Result<AssetCommandResult, crate::core::inventory::RewardInventoryPortError> {
            self.execute_calls.fetch_add(1, Ordering::Relaxed);
            if self.capacity_full {
                return AssetCommandResult::not_applied(
                    &order.request_id,
                    order.request_fingerprint(),
                    AssetCommandErrorCode::InventoryCapacityFull,
                )
                .map_err(|error| {
                    crate::core::inventory::RewardInventoryPortError::unavailable(format!(
                        "invalid capacity fixture result: {error:?}"
                    ))
                });
            }
            AssetCommandResult::applied(
                &order.request_id,
                order.request_fingerprint(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                None,
            )
            .map_err(|error| {
                crate::core::inventory::RewardInventoryPortError::unavailable(format!(
                    "invalid direct fixture result: {error:?}"
                ))
            })
        }

        async fn query_reward(
            &self,
            _request_id: &str,
            _request_fingerprint: &AssetRequestFingerprint,
        ) -> Result<Option<AssetCommandResult>, crate::core::inventory::RewardInventoryPortError>
        {
            Ok(None)
        }
    }

    fn item() -> NormalizedAssetItem {
        NormalizedAssetItem::new(1001, 2, AssetBinding::Unbound).unwrap()
    }

    fn result(claim: &RewardSourceClaim, method: AssetDeliveryMethod) -> RewardDeliveryResult {
        let order = claim.clone().into_reward_order().unwrap();
        AssetCommandResult::applied(
            &order.request_id,
            order.request_fingerprint(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Some(match method {
                AssetDeliveryMethod::Direct => {
                    AssetDeliveryReceipt::direct(&order.request_id).unwrap()
                }
                AssetDeliveryMethod::Mail => AssetDeliveryReceipt::mail(
                    "rw_test",
                    Some(AssetFallbackReason::InventoryCapacityFull),
                )
                .unwrap(),
            }),
        )
        .unwrap()
    }

    fn not_applied(claim: &RewardSourceClaim) -> RewardDeliveryResult {
        let order = claim.clone().into_reward_order().unwrap();
        AssetCommandResult::not_applied(
            &order.request_id,
            order.request_fingerprint(),
            AssetCommandErrorCode::InventoryCapacityFull,
        )
        .unwrap()
    }

    #[test]
    fn source_ids_are_stable_and_source_type_scoped() {
        let achievement =
            RewardSource::from_server_id(RewardSourceKind::Achievement, "first_clear").unwrap();
        let task = RewardSource::from_server_id(RewardSourceKind::Task, "first_clear").unwrap();
        let quest = RewardSource::from_server_id(RewardSourceKind::Quest, "first_clear").unwrap();
        let activity =
            RewardSource::from_server_id(RewardSourceKind::Activity, "summer_2026").unwrap();
        let ranking =
            RewardSource::from_server_id(RewardSourceKind::Ranking, "arena:2026w29:1").unwrap();
        let world_event =
            RewardSource::from_server_id(RewardSourceKind::WorldEvent, "keep_guard:42").unwrap();

        assert_eq!(achievement.canonical_origin_id(), "achievement:first_clear");
        assert_eq!(task.canonical_origin_id(), "task:first_clear");
        assert_eq!(quest.canonical_origin_id(), "quest:first_clear");
        assert_ne!(task.canonical_origin_id(), quest.canonical_origin_id());
        assert_eq!(activity.origin().origin_type, AssetOriginType::Activity);
        assert_eq!(
            ranking.kind().default_delivery_policy(),
            RewardDeliveryPolicy::MailOnly
        );
        assert_eq!(
            world_event.origin().origin_type,
            AssetOriginType::WorldEvent
        );
        assert!(RewardSource::from_server_id(RewardSourceKind::Battle, "has whitespace").is_err());
    }

    #[tokio::test]
    async fn source_claim_completes_only_after_direct_or_mail_delivery_and_replays_completion() {
        let source =
            RewardSource::from_server_id(RewardSourceKind::Achievement, "first_clear").unwrap();
        let claim =
            RewardSourceClaim::new("chr_1", source, vec![item()], "achievement reward").unwrap();
        let gateway = FakeGateway::default();
        gateway
            .push(result(&claim, AssetDeliveryMethod::Direct))
            .await;
        let service =
            RewardSourceService::new(gateway.clone(), InMemoryRewardSourceStateStore::default());

        let first = service.deliver_claim(claim.clone()).await.unwrap();
        let second = service.deliver_claim(claim).await.unwrap();

        assert!(first.source_completed);
        assert!(!first.replayed_source_completion);
        assert!(second.source_completed);
        assert!(second.replayed_source_completion);
        assert_eq!(gateway.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn not_applied_exchange_style_result_leaves_source_uncompleted_for_retry() {
        let source = RewardSource::from_server_id(RewardSourceKind::Quest, "wind_canyon").unwrap();
        let claim = RewardSourceClaim::new("chr_1", source, vec![item()], "quest reward").unwrap();
        let gateway = FakeGateway::default();
        gateway.push(not_applied(&claim)).await;
        gateway
            .push(result(&claim, AssetDeliveryMethod::Mail))
            .await;
        let service =
            RewardSourceService::new(gateway.clone(), InMemoryRewardSourceStateStore::default());

        let blocked = service.deliver_claim(claim.clone()).await.unwrap();
        let retried = service.deliver_claim(claim).await.unwrap();

        assert!(!blocked.source_completed);
        assert!(retried.source_completed);
        assert_eq!(
            retried.delivery.delivery.unwrap().semantics.delivery_method,
            AssetDeliveryMethod::Mail
        );
        assert_eq!(gateway.calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn source_defaults_cover_direct_mail_only_and_capacity_fallback() {
        let gateway = FakeGateway::default();
        let state = InMemoryRewardSourceStateStore::default();
        let service = RewardSourceService::new(gateway.clone(), state);

        let direct = RewardSourceClaim::new(
            "chr_1",
            RewardSource::from_server_id(RewardSourceKind::Achievement, "first_clear").unwrap(),
            vec![item()],
            "achievement reward",
        )
        .unwrap();
        let mail_only = RewardSourceClaim::new(
            "chr_1",
            RewardSource::from_server_id(RewardSourceKind::Ranking, "arena:2026w29:1").unwrap(),
            vec![item()],
            "ranking reward",
        )
        .unwrap();
        let fallback = RewardSourceClaim::new(
            "chr_1",
            RewardSource::from_server_id(RewardSourceKind::WorldEvent, "keep_guard:42").unwrap(),
            vec![item()],
            "world event reward",
        )
        .unwrap();
        assert_eq!(
            direct.delivery_policy,
            RewardDeliveryPolicy::PreferInventory
        );
        assert_eq!(mail_only.delivery_policy, RewardDeliveryPolicy::MailOnly);
        assert_eq!(
            fallback.delivery_policy,
            RewardDeliveryPolicy::PreferInventory
        );
        gateway
            .push(result(&direct, AssetDeliveryMethod::Direct))
            .await;
        gateway
            .push(result(&mail_only, AssetDeliveryMethod::Mail))
            .await;
        gateway
            .push(result(&fallback, AssetDeliveryMethod::Mail))
            .await;

        assert_eq!(
            service
                .deliver_claim(direct)
                .await
                .unwrap()
                .delivery
                .delivery
                .unwrap()
                .semantics
                .delivery_method,
            AssetDeliveryMethod::Direct
        );
        assert_eq!(
            service
                .deliver_claim(mail_only)
                .await
                .unwrap()
                .delivery
                .delivery
                .unwrap()
                .semantics
                .delivery_method,
            AssetDeliveryMethod::Mail
        );
        let fallback_outcome = service.deliver_claim(fallback).await.unwrap();
        assert_eq!(
            fallback_outcome.delivery.delivery.unwrap().fallback_reason,
            Some(AssetFallbackReason::InventoryCapacityFull)
        );
    }

    #[tokio::test]
    async fn every_source_uses_shared_delivery_for_direct_mail_and_duplicate_guards() {
        let source_kinds = [
            RewardSourceKind::Achievement,
            RewardSourceKind::Task,
            RewardSourceKind::Quest,
            RewardSourceKind::Battle,
            RewardSourceKind::ScenePickup,
            RewardSourceKind::Activity,
            RewardSourceKind::Ranking,
            RewardSourceKind::WorldEvent,
        ];

        for kind in source_kinds {
            let source = RewardSource::from_server_id(kind, format!("fixture:{:?}", kind)).unwrap();
            let claim =
                RewardSourceClaim::new("chr_1", source, vec![item()], "source fixture").unwrap();
            let inventory = DeterministicInventory::direct();
            let shared_delivery = RewardDeliveryService::new(
                inventory.clone(),
                crate::core::inventory::InMemoryRewardDeliveryStore::default(),
                crate::core::inventory::NoopRewardDeliveryNotifier,
            );
            let source_service = RewardSourceService::new(
                shared_delivery,
                InMemoryRewardSourceStateStore::default(),
            );

            let first = source_service.deliver_claim(claim.clone()).await.unwrap();
            let duplicate = source_service.deliver_claim(claim).await.unwrap();

            assert!(
                first.source_completed,
                "{kind:?} should complete after delivery"
            );
            assert!(
                duplicate.replayed_source_completion,
                "{kind:?} duplicate must replay source state"
            );
            if kind == RewardSourceKind::Ranking {
                assert_eq!(
                    first.delivery.delivery.unwrap().semantics.delivery_method,
                    AssetDeliveryMethod::Mail,
                    "ranking is intentionally MAIL_ONLY"
                );
                assert_eq!(inventory.execute_calls.load(Ordering::Relaxed), 0);
            } else {
                assert_eq!(
                    first.delivery.delivery.unwrap().semantics.delivery_method,
                    AssetDeliveryMethod::Direct,
                    "{kind:?} should settle directly when capacity is available"
                );
                assert_eq!(inventory.execute_calls.load(Ordering::Relaxed), 1);
            }
        }
    }

    #[tokio::test]
    async fn inventory_first_sources_fallback_once_when_the_shared_transaction_reports_capacity() {
        for kind in [
            RewardSourceKind::Achievement,
            RewardSourceKind::Task,
            RewardSourceKind::Quest,
            RewardSourceKind::Battle,
            RewardSourceKind::ScenePickup,
            RewardSourceKind::Activity,
            RewardSourceKind::WorldEvent,
        ] {
            let source =
                RewardSource::from_server_id(kind, format!("capacity:{:?}", kind)).unwrap();
            let claim =
                RewardSourceClaim::new("chr_1", source, vec![item()], "capacity fixture").unwrap();
            let inventory = DeterministicInventory::capacity_full();
            let shared_delivery = RewardDeliveryService::new(
                inventory.clone(),
                crate::core::inventory::InMemoryRewardDeliveryStore::default(),
                crate::core::inventory::NoopRewardDeliveryNotifier,
            );
            let source_service = RewardSourceService::new(
                shared_delivery,
                InMemoryRewardSourceStateStore::default(),
            );

            let first = source_service.deliver_claim(claim.clone()).await.unwrap();
            let duplicate = source_service.deliver_claim(claim).await.unwrap();
            let receipt = first
                .delivery
                .delivery
                .expect("capacity fallback must be mail-backed");

            assert_eq!(receipt.semantics.delivery_method, AssetDeliveryMethod::Mail);
            assert_eq!(
                receipt.fallback_reason,
                Some(AssetFallbackReason::InventoryCapacityFull)
            );
            assert!(duplicate.replayed_source_completion);
            assert_eq!(inventory.execute_calls.load(Ordering::Relaxed), 1);
        }
    }

    #[test]
    fn battle_claim_is_constructed_from_authoritative_server_result() {
        let battle =
            BattleServerResult::from_authoritative_simulation("match:9001", "chr_1", vec![item()])
                .unwrap();
        let claim = battle.into_reward_claim().unwrap();

        assert_eq!(claim.source.kind(), RewardSourceKind::Battle);
        assert_eq!(claim.source.canonical_origin_id(), "battle:match:9001");
        assert_eq!(claim.delivery_policy, RewardDeliveryPolicy::PreferInventory);
    }

    #[tokio::test]
    async fn pickup_removes_drop_only_after_authoritative_delivery() {
        let mut drop = SceneDrop::new(
            "drop:9001",
            Some("chr_1".to_string()),
            ScenePosition { x: 10, y: 0, z: 0 },
            5,
            vec![item()],
        )
        .unwrap();
        let request = ScenePickupRequest::new("drop:9001").unwrap();
        assert!(matches!(
            drop.prepare_pickup("chr_other", &request, ScenePosition { x: 10, y: 0, z: 0 }),
            Err(RewardSourceError::DropNotOwned)
        ));
        assert!(matches!(
            drop.prepare_pickup("chr_1", &request, ScenePosition { x: 16, y: 0, z: 0 }),
            Err(RewardSourceError::DropOutOfRange)
        ));

        let claim = drop
            .prepare_pickup("chr_1", &request, ScenePosition { x: 13, y: 0, z: 0 })
            .unwrap();
        let unrelated_claim = RewardSourceClaim::new(
            "chr_1",
            RewardSource::from_server_id(RewardSourceKind::ScenePickup, "drop:other").unwrap(),
            vec![item()],
            "unrelated pickup",
        )
        .unwrap();
        let unrelated_outcome = RewardSourceDeliveryOutcome {
            delivery: result(&unrelated_claim, AssetDeliveryMethod::Direct),
            source_completed: true,
            replayed_source_completion: false,
        };
        assert!(
            drop.remove_after_delivery(&claim, &unrelated_outcome)
                .is_err()
        );
        assert!(!drop.is_removed());
        let gateway = FakeGateway::default();
        gateway.push(not_applied(&claim)).await;
        gateway
            .push(result(&claim, AssetDeliveryMethod::Mail))
            .await;
        let service = RewardSourceService::new(gateway, InMemoryRewardSourceStateStore::default());

        let blocked = service.deliver_claim(claim.clone()).await.unwrap();
        assert!(drop.remove_after_delivery(&claim, &blocked).is_err());
        assert!(!drop.is_removed());

        let delivered = service.deliver_claim(claim.clone()).await.unwrap();
        drop.remove_after_delivery(&claim, &delivered).unwrap();
        assert!(drop.is_removed());
    }

    #[test]
    fn exchanges_are_all_or_nothing_and_never_allow_mail_fallback() {
        let exchange = InventoryRequiredExchange::new(
            AssetExchangeKind::Crafting,
            "iron_sword:1",
            "chr_1",
            vec![AssetConsumption {
                asset_uid: 99,
                count: 2,
            }],
            vec![item()],
        )
        .unwrap();

        assert_eq!(
            exchange.delivery_policy,
            RewardDeliveryPolicy::InventoryRequired
        );
        assert_eq!(
            exchange.command.atomicity,
            AssetBatchAtomicity::AllOrNothing
        );
        assert_eq!(exchange.command.operations.len(), 2);
        assert!(exchange.command.validate().is_ok());
    }

    #[test]
    fn ui_claim_accepts_only_a_business_object_id() {
        let request = UiRewardClaimRequest::new("achievement_first_forge").unwrap();
        assert_eq!(request.business_object_id, "achievement_first_forge");
        assert!(UiRewardClaimRequest::new(" ").is_err());
    }
}
