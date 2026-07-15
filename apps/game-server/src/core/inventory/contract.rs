use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    AssetBinding, AssetContainer, AssetDeliveryMethod, AssetDeliverySemantics, AssetOrigin,
    AssetOriginType, AssetSettlementTarget, AssetType, EquipSlot,
};

/// Version of the server-to-server asset contract and its canonical fingerprint payloads.
pub const ASSET_CONTRACT_SCHEMA_VERSION: u16 = 1;
pub const ASSET_REQUEST_ID_MAX_BYTES: usize = 128;
pub const ASSET_CHARACTER_ID_MAX_BYTES: usize = 128;
pub const ASSET_REASON_MAX_BYTES: usize = 512;
pub const ASSET_OPERATOR_ID_MAX_BYTES: usize = 128;

/// Policy selected by a trusted reward source. This is not a player protocol field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RewardDeliveryPolicy {
    /// Persist one reward mail idempotently without attempting the inventory first.
    MailOnly,
    /// Try inventory first; only a definite, uncommitted capacity failure may use mail fallback.
    PreferInventory,
    /// Inventory capacity is part of an asset exchange and must reject the whole operation.
    InventoryRequired,
}

/// The source business class selects a conservative default delivery policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewardBusinessClass {
    Reward,
    AssetExchange,
}

impl RewardBusinessClass {
    pub const fn default_delivery_policy(self) -> RewardDeliveryPolicy {
        match self {
            Self::Reward => RewardDeliveryPolicy::PreferInventory,
            Self::AssetExchange => RewardDeliveryPolicy::InventoryRequired,
        }
    }
}

impl RewardDeliveryPolicy {
    pub const fn may_fallback_to_mail_after_capacity_failure(self) -> bool {
        matches!(self, Self::PreferInventory)
    }
}

/// Server-authenticated actor type. Player clients never construct this context directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetOperatorType {
    Player,
    Service,
    Gm,
    System,
}

/// Explicit capability required by an asset operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetPermission {
    Grant,
    Consume,
    Move,
    Equip,
    Unequip,
    Freeze,
    Unfreeze,
}

/// Trusted actor context recorded with a reward order or an asset command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetOperator {
    pub operator_type: AssetOperatorType,
    pub operator_id: String,
    pub permissions: Vec<AssetPermission>,
}

impl AssetOperator {
    pub fn new(
        operator_type: AssetOperatorType,
        operator_id: impl Into<String>,
        permissions: impl IntoIterator<Item = AssetPermission>,
    ) -> Result<Self, AssetCommandErrorCode> {
        let operator = Self {
            operator_type,
            operator_id: operator_id.into(),
            permissions: normalize_permissions(permissions),
        };
        operator.validate()?;
        Ok(operator)
    }

    pub fn validate(&self) -> Result<(), AssetCommandErrorCode> {
        if !is_trimmed_nonempty_with_max_bytes(&self.operator_id, ASSET_OPERATOR_ID_MAX_BYTES) {
            return Err(AssetCommandErrorCode::InvalidOperator);
        }
        if self.permissions.is_empty() {
            return Err(AssetCommandErrorCode::PermissionDenied);
        }
        Ok(())
    }

    pub fn authorizes(&self, permission: AssetPermission) -> bool {
        if self.operator_type == AssetOperatorType::Player
            && matches!(
                permission,
                AssetPermission::Grant | AssetPermission::Freeze | AssetPermission::Unfreeze
            )
        {
            return false;
        }

        self.permissions.contains(&permission)
    }
}

/// Normalized item intent. It deliberately has no `Item`, container snapshot, UID allocation, or
/// JSONB field: the transaction core owns materializing a concrete inventory item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NormalizedAssetItem {
    pub item_id: i32,
    pub count: u32,
    pub binding: AssetBinding,
}

impl NormalizedAssetItem {
    pub fn new(item_id: i32, count: u32, binding: AssetBinding) -> Result<Self, AssetCommandErrorCode> {
        let item = Self {
            item_id,
            count,
            binding,
        };
        item.validate_shape()?;
        Ok(item)
    }

    fn validate_shape(&self) -> Result<(), AssetCommandErrorCode> {
        if self.item_id <= 0 {
            return Err(AssetCommandErrorCode::InvalidItemId);
        }
        if self.count == 0 {
            return Err(AssetCommandErrorCode::InvalidItemCount);
        }
        if let AssetBinding::CharacterBound { character_id } = &self.binding
            && character_id.trim().is_empty()
        {
            return Err(AssetCommandErrorCode::InvalidBinding);
        }
        if matches!(
            &self.binding,
            AssetBinding::LegacyBoundWithoutCharacter
                | AssetBinding::LegacyUnboundWithCharacter { .. }
        ) {
            return Err(AssetCommandErrorCode::InvalidBinding);
        }
        Ok(())
    }

    fn validate_for_character(&self, character_id: &str) -> Result<(), AssetCommandErrorCode> {
        self.validate_shape()?;
        if let AssetBinding::CharacterBound {
            character_id: bound_character_id,
        } = &self.binding
            && bound_character_id != character_id
        {
            return Err(AssetCommandErrorCode::CharacterBindingMismatch);
        }
        Ok(())
    }

    fn sort_key(&self) -> Result<(i32, NormalizedBindingKey), AssetCommandErrorCode> {
        Ok((self.item_id, NormalizedBindingKey::from_binding(&self.binding)?))
    }
}

/// Convert untrusted item lists to a canonical order and merge only equal binding identities.
pub fn normalize_asset_items(
    character_id: &str,
    items: &[NormalizedAssetItem],
) -> Result<Vec<NormalizedAssetItem>, AssetCommandErrorCode> {
    if items.is_empty() {
        return Err(AssetCommandErrorCode::EmptyItems);
    }

    let mut merged = BTreeMap::<(i32, NormalizedBindingKey), u32>::new();
    for item in items {
        item.validate_for_character(character_id)?;
        let (item_id, binding) = item.sort_key()?;
        let count = merged.entry((item_id, binding)).or_default();
        *count = count
            .checked_add(item.count)
            .ok_or(AssetCommandErrorCode::ItemCountOverflow)?;
    }

    Ok(merged
        .into_iter()
        .map(|((item_id, binding), count)| NormalizedAssetItem {
            item_id,
            count,
            binding: binding.into_binding(),
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum NormalizedBindingKey {
    Unbound,
    CharacterBound(String),
}

impl NormalizedBindingKey {
    fn from_binding(binding: &AssetBinding) -> Result<Self, AssetCommandErrorCode> {
        match binding {
            AssetBinding::Unbound => Ok(Self::Unbound),
            AssetBinding::CharacterBound { character_id } => {
                if character_id.trim().is_empty() {
                    Err(AssetCommandErrorCode::InvalidBinding)
                } else {
                    Ok(Self::CharacterBound(character_id.clone()))
                }
            }
            AssetBinding::LegacyBoundWithoutCharacter
            | AssetBinding::LegacyUnboundWithCharacter { .. } => {
                Err(AssetCommandErrorCode::InvalidBinding)
            }
        }
    }

    fn into_binding(self) -> AssetBinding {
        match self {
            Self::Unbound => AssetBinding::Unbound,
            Self::CharacterBound(character_id) => AssetBinding::CharacterBound { character_id },
        }
    }
}

/// Server-authoritative reward request. Its serialized shape keeps `origin_type` and `origin_id`
/// flat while reusing the stage-one `AssetOrigin` contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewardOrder {
    pub request_id: String,
    pub character_id: String,
    #[serde(flatten)]
    pub origin: AssetOrigin,
    pub delivery_policy: RewardDeliveryPolicy,
    pub items: Vec<NormalizedAssetItem>,
    pub reason: String,
    pub operator: AssetOperator,
}

impl RewardOrder {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        request_id: impl Into<String>,
        character_id: impl Into<String>,
        origin: AssetOrigin,
        delivery_policy: RewardDeliveryPolicy,
        items: &[NormalizedAssetItem],
        reason: impl Into<String>,
        operator: AssetOperator,
    ) -> Result<Self, AssetCommandErrorCode> {
        let character_id = character_id.into();
        let order = Self {
            request_id: request_id.into(),
            items: normalize_asset_items(&character_id, items)?,
            character_id,
            origin,
            delivery_policy,
            reason: reason.into(),
            operator,
        };
        order.validate()?;
        Ok(order)
    }

    pub fn validate(&self) -> Result<(), AssetCommandErrorCode> {
        validate_common_request_fields(
            &self.request_id,
            &self.character_id,
            &self.origin,
            &self.reason,
            &self.operator,
        )?;
        if !self.operator.authorizes(AssetPermission::Grant) {
            return Err(AssetCommandErrorCode::PermissionDenied);
        }
        if normalize_asset_items(&self.character_id, &self.items)? != self.items {
            return Err(AssetCommandErrorCode::ItemsNotNormalized);
        }
        Ok(())
    }

    pub fn request_fingerprint(&self) -> AssetRequestFingerprint {
        AssetRequestFingerprint::for_reward_order(self)
    }
}

/// Inventory container revision expected by a command. Revisions are contractual preconditions in
/// this phase; stage three decides how they are persisted and checked atomically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetContainerVersion {
    pub container: AssetContainer,
    pub version: u64,
}

/// A UID and quantity reference, never a mutable item or JSONB snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetConsumption {
    pub asset_uid: u64,
    pub count: u32,
}

/// Atomic operation executed within an [`AssetCommand`] batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum AssetOperation {
    Grant { items: Vec<NormalizedAssetItem> },
    Consume { assets: Vec<AssetConsumption> },
    Move {
        asset_uid: u64,
        count: u32,
        from: AssetContainer,
        to: AssetContainer,
    },
    Equip {
        asset_uid: u64,
        from: AssetContainer,
        slot: EquipSlot,
    },
    Unequip { slot: EquipSlot, to: AssetContainer },
    Freeze { asset_uid: u64, reason: String },
    Unfreeze { asset_uid: u64 },
}

impl AssetOperation {
    pub const fn required_permission(&self) -> AssetPermission {
        match self {
            Self::Grant { .. } => AssetPermission::Grant,
            Self::Consume { .. } => AssetPermission::Consume,
            Self::Move { .. } => AssetPermission::Move,
            Self::Equip { .. } => AssetPermission::Equip,
            Self::Unequip { .. } => AssetPermission::Unequip,
            Self::Freeze { .. } => AssetPermission::Freeze,
            Self::Unfreeze { .. } => AssetPermission::Unfreeze,
        }
    }

    pub const fn preconditions(&self) -> &'static [AssetOperationPrecondition] {
        const GRANT: &[AssetOperationPrecondition] = &[
            AssetOperationPrecondition::TrustedGrantAuthority,
            AssetOperationPrecondition::ItemConfigExists,
            AssetOperationPrecondition::DestinationCapacity,
        ];
        const CONSUME: &[AssetOperationPrecondition] = &[
            AssetOperationPrecondition::AssetExists,
            AssetOperationPrecondition::AssetUnlocked,
            AssetOperationPrecondition::SufficientQuantity,
        ];
        const MOVE: &[AssetOperationPrecondition] = &[
            AssetOperationPrecondition::AssetExists,
            AssetOperationPrecondition::AssetUnlocked,
            AssetOperationPrecondition::SourceContainerMatches,
            AssetOperationPrecondition::SufficientQuantity,
            AssetOperationPrecondition::DestinationCapacity,
        ];
        const EQUIP: &[AssetOperationPrecondition] = &[
            AssetOperationPrecondition::AssetExists,
            AssetOperationPrecondition::AssetUnlocked,
            AssetOperationPrecondition::SourceContainerMatches,
            AssetOperationPrecondition::EquipmentSlotMatches,
            AssetOperationPrecondition::DestinationCapacity,
        ];
        const UNEQUIP: &[AssetOperationPrecondition] = &[
            AssetOperationPrecondition::AssetEquipped,
            AssetOperationPrecondition::AssetUnlocked,
            AssetOperationPrecondition::DestinationCapacity,
        ];
        const FREEZE: &[AssetOperationPrecondition] = &[
            AssetOperationPrecondition::AssetExists,
            AssetOperationPrecondition::AssetUnlocked,
        ];
        const UNFREEZE: &[AssetOperationPrecondition] = &[
            AssetOperationPrecondition::AssetExists,
            AssetOperationPrecondition::AssetFrozen,
        ];

        match self {
            Self::Grant { .. } => GRANT,
            Self::Consume { .. } => CONSUME,
            Self::Move { .. } => MOVE,
            Self::Equip { .. } => EQUIP,
            Self::Unequip { .. } => UNEQUIP,
            Self::Freeze { .. } => FREEZE,
            Self::Unfreeze { .. } => UNFREEZE,
        }
    }

    fn validate(&self, character_id: &str) -> Result<(), AssetCommandErrorCode> {
        match self {
            Self::Grant { items } => {
                if normalize_asset_items(character_id, items)? != *items {
                    return Err(AssetCommandErrorCode::ItemsNotNormalized);
                }
            }
            Self::Consume { assets } => validate_consumptions(assets)?,
            Self::Move {
                asset_uid,
                count,
                from,
                to,
            } => {
                validate_asset_uid_and_count(*asset_uid, *count)?;
                if from == to {
                    return Err(AssetCommandErrorCode::ContainerMismatch);
                }
            }
            Self::Equip {
                asset_uid, from, ..
            } => {
                validate_asset_uid(*asset_uid)?;
                if *from != AssetContainer::Inventory {
                    return Err(AssetCommandErrorCode::ContainerMismatch);
                }
            }
            Self::Unequip { to, .. } => {
                if *to != AssetContainer::Inventory {
                    return Err(AssetCommandErrorCode::ContainerMismatch);
                }
            }
            Self::Freeze { asset_uid, reason } => {
                validate_asset_uid(*asset_uid)?;
                if !is_trimmed_nonempty_with_max_bytes(reason, ASSET_REASON_MAX_BYTES) {
                    return Err(AssetCommandErrorCode::InvalidReason);
                }
            }
            Self::Unfreeze { asset_uid } => validate_asset_uid(*asset_uid)?,
        }
        Ok(())
    }
}

/// Runtime checks that every operation must satisfy inside the future shared transaction boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetOperationPrecondition {
    TrustedGrantAuthority,
    ItemConfigExists,
    AssetExists,
    AssetUnlocked,
    AssetFrozen,
    AssetEquipped,
    SourceContainerMatches,
    SufficientQuantity,
    DestinationCapacity,
    EquipmentSlotMatches,
}

/// Commands are batch-only: every contained operation commits together or none commits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AssetBatchAtomicity {
    AllOrNothing,
}

/// Server-side asset mutation request. It accepts intent and preconditions, never JSONB snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetCommand {
    pub request_id: String,
    pub character_id: String,
    #[serde(flatten)]
    pub origin: AssetOrigin,
    pub reason: String,
    pub operator: AssetOperator,
    pub expected_container_versions: Vec<AssetContainerVersion>,
    pub operations: Vec<AssetOperation>,
    pub atomicity: AssetBatchAtomicity,
}

impl AssetCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        request_id: impl Into<String>,
        character_id: impl Into<String>,
        origin: AssetOrigin,
        reason: impl Into<String>,
        operator: AssetOperator,
        expected_container_versions: Vec<AssetContainerVersion>,
        operations: Vec<AssetOperation>,
    ) -> Result<Self, AssetCommandErrorCode> {
        let command = Self {
            request_id: request_id.into(),
            character_id: character_id.into(),
            origin,
            reason: reason.into(),
            operator,
            expected_container_versions: normalize_container_versions(expected_container_versions)?,
            operations,
            atomicity: AssetBatchAtomicity::AllOrNothing,
        };
        command.validate()?;
        Ok(command)
    }

    pub fn validate(&self) -> Result<(), AssetCommandErrorCode> {
        validate_common_request_fields(
            &self.request_id,
            &self.character_id,
            &self.origin,
            &self.reason,
            &self.operator,
        )?;
        if self.atomicity != AssetBatchAtomicity::AllOrNothing {
            return Err(AssetCommandErrorCode::InvalidRequest);
        }
        if self.operations.is_empty() {
            return Err(AssetCommandErrorCode::EmptyCommandBatch);
        }

        let normalized_versions =
            normalize_container_versions(self.expected_container_versions.clone())?;
        if normalized_versions != self.expected_container_versions {
            return Err(AssetCommandErrorCode::ContainerVersionsNotNormalized);
        }
        for operation in &self.operations {
            if !self.operator.authorizes(operation.required_permission()) {
                return Err(AssetCommandErrorCode::PermissionDenied);
            }
            operation.validate(&self.character_id)?;
        }
        Ok(())
    }

    pub fn request_fingerprint(&self) -> AssetRequestFingerprint {
        AssetRequestFingerprint::for_command(self)
    }
}

/// Stable machine-readable error codes for every future asset command implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AssetCommandErrorCode {
    InvalidRequest,
    InvalidRequestId,
    InvalidCharacterId,
    InvalidOrigin,
    InvalidOperator,
    InvalidReason,
    EmptyItems,
    ItemsNotNormalized,
    InvalidItemId,
    InvalidItemCount,
    ItemCountOverflow,
    InvalidBinding,
    CharacterBindingMismatch,
    EmptyCommandBatch,
    DuplicateContainerVersion,
    ContainerVersionsNotNormalized,
    PermissionDenied,
    UnsupportedAssetType,
    AssetNotFound,
    AssetFrozen,
    NotEnoughAsset,
    ContainerMismatch,
    EquipmentSlotMismatch,
    AssetNotEquipped,
    InventoryCapacityFull,
    CapacityBlocked,
    RequestFingerprintConflict,
    ResultUnknown,
    InvalidResultContract,
}

impl AssetCommandErrorCode {
    /// Both capacity codes prove that no asset mutation was committed. `CapacityBlocked` is for an
    /// already-mail-backed claim and must not create another mail; `InventoryCapacityFull` may
    /// only use a reward-mail fallback under `PREFER_INVENTORY`.
    pub const fn is_definite_capacity_block(self) -> bool {
        matches!(self, Self::InventoryCapacityFull | Self::CapacityBlocked)
    }

    pub const fn result_state(self) -> AssetResultState {
        match self {
            Self::ResultUnknown => AssetResultState::Unknown,
            _ => AssetResultState::NotApplied,
        }
    }

    /// `CAPACITY_BLOCKED` keeps a mail-backed grant for the player to retry after making space.
    /// `INVENTORY_CAPACITY_FULL` is not player-retryable by itself because a reward delivery may
    /// still persist its one permitted mail fallback.
    pub const fn player_retryable(self) -> bool {
        matches!(self, Self::CapacityBlocked)
    }

    /// An unknown outcome is reconciled by querying the original request before any retry or
    /// fallback decision. Callers must not infer a second delivery attempt from this value.
    pub const fn requires_query_first(self) -> bool {
        matches!(self, Self::ResultUnknown)
    }
}

/// Digest of a canonical server-side request intent. It excludes request ID, reason and operator
/// so retries retain identity while audit-only context remains independently recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AssetRequestFingerprint(pub String);

impl AssetRequestFingerprint {
    pub fn parse(value: impl Into<String>) -> Result<Self, AssetCommandErrorCode> {
        let fingerprint = Self(value.into());
        if !is_sha256_fingerprint(&fingerprint.0) {
            return Err(AssetCommandErrorCode::InvalidRequest);
        }
        Ok(fingerprint)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn for_reward_order(order: &RewardOrder) -> Self {
        let canonical = serde_json::to_vec(&RewardOrderFingerprintPayload {
            schema_version: ASSET_CONTRACT_SCHEMA_VERSION,
            kind: "reward_order",
            character_id: &order.character_id,
            origin_type: order.origin.origin_type,
            origin_id: &order.origin.origin_id,
            delivery_policy: order.delivery_policy,
            items: &order.items,
        })
        .expect("asset reward fingerprint payload must serialize");
        Self(format!("sha256:{:x}", Sha256::digest(canonical)))
    }

    fn for_command(command: &AssetCommand) -> Self {
        let expected_container_versions =
            canonical_container_versions(&command.expected_container_versions);
        let canonical = serde_json::to_vec(&AssetCommandFingerprintPayload {
            schema_version: ASSET_CONTRACT_SCHEMA_VERSION,
            kind: "asset_command",
            character_id: &command.character_id,
            origin_type: command.origin.origin_type,
            origin_id: &command.origin.origin_id,
            expected_container_versions: &expected_container_versions,
            operations: &command.operations,
            atomicity: command.atomicity,
        })
        .expect("asset command fingerprint payload must serialize");
        Self(format!("sha256:{:x}", Sha256::digest(canonical)))
    }
}

#[derive(Serialize)]
struct RewardOrderFingerprintPayload<'a> {
    schema_version: u16,
    kind: &'static str,
    character_id: &'a str,
    origin_type: AssetOriginType,
    origin_id: &'a str,
    delivery_policy: RewardDeliveryPolicy,
    items: &'a [NormalizedAssetItem],
}

#[derive(Serialize)]
struct AssetCommandFingerprintPayload<'a> {
    schema_version: u16,
    kind: &'static str,
    character_id: &'a str,
    origin_type: AssetOriginType,
    origin_id: &'a str,
    expected_container_versions: &'a [AssetContainerVersion],
    operations: &'a [AssetOperation],
    atomicity: AssetBatchAtomicity,
}

/// Result certainty is distinct from transport errors and from a retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetResultState {
    Applied,
    NotApplied,
    Unknown,
}

impl AssetResultState {
    pub const fn requires_query_first(self) -> bool {
        matches!(self, Self::Unknown)
    }
}

/// Actual quantity change from a committed command. Positive values grant and negative values
/// consume; zero is invalid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetQuantityDelta {
    pub asset_type: AssetType,
    pub item_id: i32,
    pub binding: AssetBinding,
    pub delta: i64,
}

/// Why a mail delivery was chosen after a direct inventory attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AssetFallbackReason {
    InventoryCapacityFull,
}

/// Committed delivery metadata. The delivery ID is a direct transaction ID or a reward mail ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetDeliveryReceipt {
    pub semantics: AssetDeliverySemantics,
    pub delivery_id: String,
    pub fallback_reason: Option<AssetFallbackReason>,
}

impl AssetDeliveryReceipt {
    pub fn direct(delivery_id: impl Into<String>) -> Result<Self, AssetCommandErrorCode> {
        Self::new(
            AssetDeliverySemantics::to_inventory(AssetDeliveryMethod::Direct),
            delivery_id,
            None,
        )
    }

    pub fn mail(
        delivery_id: impl Into<String>,
        fallback_reason: Option<AssetFallbackReason>,
    ) -> Result<Self, AssetCommandErrorCode> {
        Self::new(
            AssetDeliverySemantics::to_inventory(AssetDeliveryMethod::Mail),
            delivery_id,
            fallback_reason,
        )
    }

    pub fn new(
        semantics: AssetDeliverySemantics,
        delivery_id: impl Into<String>,
        fallback_reason: Option<AssetFallbackReason>,
    ) -> Result<Self, AssetCommandErrorCode> {
        let receipt = Self {
            semantics,
            delivery_id: delivery_id.into(),
            fallback_reason,
        };
        receipt.validate()?;
        Ok(receipt)
    }

    pub fn validate(&self) -> Result<(), AssetCommandErrorCode> {
        if self.delivery_id.trim().is_empty()
            || self.semantics.settlement_target != AssetSettlementTarget::Inventory
        {
            return Err(AssetCommandErrorCode::InvalidResultContract);
        }
        if self.fallback_reason.is_some()
            && self.semantics.delivery_method != AssetDeliveryMethod::Mail
        {
            return Err(AssetCommandErrorCode::InvalidResultContract);
        }
        Ok(())
    }
}

/// Common result contract for reward delivery and direct asset command execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetCommandResult {
    pub request_id: String,
    pub request_fingerprint: AssetRequestFingerprint,
    pub result_state: AssetResultState,
    pub error_code: Option<AssetCommandErrorCode>,
    pub actual_deltas: Vec<AssetQuantityDelta>,
    pub container_versions: Vec<AssetContainerVersion>,
    pub asset_ledger_ids: Vec<String>,
    pub delivery: Option<AssetDeliveryReceipt>,
}

/// Reward delivery is a result of the same committed asset-operation contract.
pub type RewardDeliveryResult = AssetCommandResult;

impl AssetCommandResult {
    pub fn applied(
        request_id: impl Into<String>,
        request_fingerprint: AssetRequestFingerprint,
        actual_deltas: Vec<AssetQuantityDelta>,
        container_versions: Vec<AssetContainerVersion>,
        asset_ledger_ids: Vec<String>,
        delivery: Option<AssetDeliveryReceipt>,
    ) -> Result<Self, AssetCommandErrorCode> {
        let result = Self {
            request_id: request_id.into(),
            request_fingerprint,
            result_state: AssetResultState::Applied,
            error_code: None,
            actual_deltas,
            container_versions,
            asset_ledger_ids,
            delivery,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn not_applied(
        request_id: impl Into<String>,
        request_fingerprint: AssetRequestFingerprint,
        error_code: AssetCommandErrorCode,
    ) -> Result<Self, AssetCommandErrorCode> {
        if error_code.result_state() != AssetResultState::NotApplied {
            return Err(AssetCommandErrorCode::InvalidResultContract);
        }
        let result = Self {
            request_id: request_id.into(),
            request_fingerprint,
            result_state: AssetResultState::NotApplied,
            error_code: Some(error_code),
            actual_deltas: Vec::new(),
            container_versions: Vec::new(),
            asset_ledger_ids: Vec::new(),
            delivery: None,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn unknown(
        request_id: impl Into<String>,
        request_fingerprint: AssetRequestFingerprint,
    ) -> Result<Self, AssetCommandErrorCode> {
        let result = Self {
            request_id: request_id.into(),
            request_fingerprint,
            result_state: AssetResultState::Unknown,
            error_code: Some(AssetCommandErrorCode::ResultUnknown),
            actual_deltas: Vec::new(),
            container_versions: Vec::new(),
            asset_ledger_ids: Vec::new(),
            delivery: None,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), AssetCommandErrorCode> {
        if !is_trimmed_nonempty_with_max_bytes(&self.request_id, ASSET_REQUEST_ID_MAX_BYTES)
            || !is_sha256_fingerprint(self.request_fingerprint.as_str())
        {
            return Err(AssetCommandErrorCode::InvalidResultContract);
        }
        if self
            .actual_deltas
            .iter()
            .any(|delta| {
                delta.asset_type != AssetType::Item || delta.item_id <= 0 || delta.delta == 0
            })
        {
            return Err(AssetCommandErrorCode::InvalidResultContract);
        }
        if self.asset_ledger_ids.iter().any(|id| id.trim().is_empty()) {
            return Err(AssetCommandErrorCode::InvalidResultContract);
        }
        if let Some(delivery) = &self.delivery {
            delivery.validate()?;
        }

        match self.result_state {
            AssetResultState::Applied if self.error_code.is_none() => Ok(()),
            AssetResultState::NotApplied
                if self.error_code.is_some_and(|code| code.result_state() == AssetResultState::NotApplied)
                    && self.actual_deltas.is_empty()
                    && self.container_versions.is_empty()
                    && self.asset_ledger_ids.is_empty()
                    && self.delivery.is_none() =>
            {
                Ok(())
            }
            AssetResultState::Unknown
                if self.error_code == Some(AssetCommandErrorCode::ResultUnknown)
                    && self.actual_deltas.is_empty()
                    && self.container_versions.is_empty()
                    && self.asset_ledger_ids.is_empty()
                    && self.delivery.is_none() =>
            {
                Ok(())
            }
            _ => Err(AssetCommandErrorCode::InvalidResultContract),
        }
    }

    /// The only automatic mail fallback decision permitted by this contract.
    pub fn permits_reward_mail_fallback(&self, policy: RewardDeliveryPolicy) -> bool {
        policy.may_fallback_to_mail_after_capacity_failure()
            && self.result_state == AssetResultState::NotApplied
            && matches!(
                self.error_code,
                Some(AssetCommandErrorCode::InventoryCapacityFull)
            )
    }
}

fn validate_common_request_fields(
    request_id: &str,
    character_id: &str,
    origin: &AssetOrigin,
    reason: &str,
    operator: &AssetOperator,
) -> Result<(), AssetCommandErrorCode> {
    if !is_trimmed_nonempty_with_max_bytes(request_id, ASSET_REQUEST_ID_MAX_BYTES) {
        return Err(AssetCommandErrorCode::InvalidRequestId);
    }
    if !is_trimmed_nonempty_with_max_bytes(character_id, ASSET_CHARACTER_ID_MAX_BYTES) {
        return Err(AssetCommandErrorCode::InvalidCharacterId);
    }
    if origin.origin_id.trim().is_empty() {
        return Err(AssetCommandErrorCode::InvalidOrigin);
    }
    if !is_trimmed_nonempty_with_max_bytes(reason, ASSET_REASON_MAX_BYTES) {
        return Err(AssetCommandErrorCode::InvalidReason);
    }
    operator.validate()
}

fn normalize_permissions(
    permissions: impl IntoIterator<Item = AssetPermission>,
) -> Vec<AssetPermission> {
    let mut permissions = permissions.into_iter().collect::<Vec<_>>();
    permissions.sort_unstable();
    permissions.dedup();
    permissions
}

fn validate_consumptions(assets: &[AssetConsumption]) -> Result<(), AssetCommandErrorCode> {
    if assets.is_empty() {
        return Err(AssetCommandErrorCode::EmptyItems);
    }
    let mut asset_uids = BTreeSet::new();
    for asset in assets {
        validate_asset_uid_and_count(asset.asset_uid, asset.count)?;
        if !asset_uids.insert(asset.asset_uid) {
            return Err(AssetCommandErrorCode::InvalidRequest);
        }
    }
    Ok(())
}

fn validate_asset_uid(asset_uid: u64) -> Result<(), AssetCommandErrorCode> {
    if asset_uid == 0 {
        return Err(AssetCommandErrorCode::InvalidRequest);
    }
    Ok(())
}

fn validate_asset_uid_and_count(asset_uid: u64, count: u32) -> Result<(), AssetCommandErrorCode> {
    validate_asset_uid(asset_uid)?;
    if count == 0 {
        return Err(AssetCommandErrorCode::InvalidItemCount);
    }
    Ok(())
}

fn normalize_container_versions(
    mut versions: Vec<AssetContainerVersion>,
) -> Result<Vec<AssetContainerVersion>, AssetCommandErrorCode> {
    versions.sort_unstable_by_key(|version| asset_container_sort_key(version.container));
    if versions
        .windows(2)
        .any(|pair| pair[0].container == pair[1].container)
    {
        return Err(AssetCommandErrorCode::DuplicateContainerVersion);
    }
    Ok(versions)
}

fn canonical_container_versions(versions: &[AssetContainerVersion]) -> Vec<AssetContainerVersion> {
    let mut versions = versions.to_vec();
    versions.sort_unstable_by_key(|version| asset_container_sort_key(version.container));
    versions
}

fn asset_container_sort_key(container: AssetContainer) -> u8 {
    match container {
        AssetContainer::Inventory => 0,
        AssetContainer::Warehouse => 1,
        AssetContainer::Equipment => 2,
    }
}

fn is_trimmed_nonempty_with_max_bytes(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max_bytes
}

fn is_sha256_fingerprint(value: &str) -> bool {
    value.len() == "sha256:".len() + 64
        && value.starts_with("sha256:")
        && value["sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (byte.is_ascii_lowercase() && byte.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_operator() -> AssetOperator {
        AssetOperator::new(
            AssetOperatorType::Service,
            "achievement-service",
            [AssetPermission::Grant],
        )
        .unwrap()
    }

    fn fixture_order() -> RewardOrder {
        RewardOrder::new(
            "reward:achievement:chr_0000000000042:42",
            "chr_0000000000042",
            AssetOrigin::new(AssetOriginType::Achievement, "achievement:42").unwrap(),
            RewardDeliveryPolicy::PreferInventory,
            &[
                NormalizedAssetItem::new(1002, 2, AssetBinding::Unbound).unwrap(),
                NormalizedAssetItem::new(
                    1001,
                    1,
                    AssetBinding::CharacterBound {
                        character_id: "chr_0000000000042".to_string(),
                    },
                )
                .unwrap(),
                NormalizedAssetItem::new(1002, 3, AssetBinding::Unbound).unwrap(),
            ],
            "achievement reward",
            service_operator(),
        )
        .unwrap()
    }

    #[test]
    fn reward_order_normalizes_items_and_rejects_cross_character_binding() {
        let order = fixture_order();
        assert_eq!(order.items.len(), 2);
        assert_eq!(order.items[1].item_id, 1002);
        assert_eq!(order.items[1].count, 5);

        let result = RewardOrder::new(
            "reward:bad-binding",
            "chr_1",
            AssetOrigin::new(AssetOriginType::Achievement, "achievement:1").unwrap(),
            RewardDeliveryPolicy::PreferInventory,
            &[NormalizedAssetItem::new(
                1001,
                1,
                AssetBinding::CharacterBound {
                    character_id: "chr_2".to_string(),
                },
            )
            .unwrap()],
            "bad binding",
            service_operator(),
        );
        assert_eq!(result, Err(AssetCommandErrorCode::CharacterBindingMismatch));
    }

    #[test]
    fn command_requires_explicit_permissions_and_all_or_nothing_batches() {
        let player = AssetOperator::new(
            AssetOperatorType::Player,
            "player-1",
            [AssetPermission::Grant],
        )
        .unwrap();
        let result = AssetCommand::new(
            "asset:grant:1",
            "chr_1",
            AssetOrigin::new(AssetOriginType::PlayerOperation, "operation:1").unwrap(),
            "forged grant",
            player,
            Vec::new(),
            vec![AssetOperation::Grant {
                items: vec![NormalizedAssetItem::new(1001, 1, AssetBinding::Unbound).unwrap()],
            }],
        );
        assert_eq!(result, Err(AssetCommandErrorCode::PermissionDenied));
    }

    #[test]
    fn command_normalizes_container_versions_before_fingerprinting() {
        let make_command = |expected_container_versions| {
            AssetCommand::new(
                "asset:move:1",
                "chr_1",
                AssetOrigin::new(AssetOriginType::PlayerOperation, "operation:1").unwrap(),
                "move item",
                AssetOperator::new(
                    AssetOperatorType::Service,
                    "inventory-service",
                    [AssetPermission::Move],
                )
                .unwrap(),
                expected_container_versions,
                vec![AssetOperation::Move {
                    asset_uid: 100,
                    count: 1,
                    from: AssetContainer::Inventory,
                    to: AssetContainer::Warehouse,
                }],
            )
            .unwrap()
        };
        let left = make_command(vec![
            AssetContainerVersion {
                container: AssetContainer::Warehouse,
                version: 4,
            },
            AssetContainerVersion {
                container: AssetContainer::Inventory,
                version: 7,
            },
        ]);
        let right = make_command(vec![
            AssetContainerVersion {
                container: AssetContainer::Inventory,
                version: 7,
            },
            AssetContainerVersion {
                container: AssetContainer::Warehouse,
                version: 4,
            },
        ]);

        assert_eq!(left.expected_container_versions, right.expected_container_versions);
        assert_eq!(left.request_fingerprint(), right.request_fingerprint());

        let mut bypassed_constructor = left.clone();
        bypassed_constructor.expected_container_versions.reverse();
        assert_eq!(
            bypassed_constructor.validate(),
            Err(AssetCommandErrorCode::ContainerVersionsNotNormalized)
        );
        assert_eq!(
            bypassed_constructor.request_fingerprint(),
            left.request_fingerprint()
        );
    }

    #[test]
    fn only_definite_inventory_capacity_failure_can_fallback_to_reward_mail() {
        let fingerprint = fixture_order().request_fingerprint();
        let capacity = AssetCommandResult::not_applied(
            "reward:achievement:1",
            fingerprint.clone(),
            AssetCommandErrorCode::InventoryCapacityFull,
        )
        .unwrap();
        let blocked = AssetCommandResult::not_applied(
            "mail_claim:mail_1",
            fingerprint.clone(),
            AssetCommandErrorCode::CapacityBlocked,
        )
        .unwrap();
        let unknown = AssetCommandResult::unknown("reward:achievement:1", fingerprint).unwrap();

        assert!(capacity.permits_reward_mail_fallback(RewardDeliveryPolicy::PreferInventory));
        assert!(!capacity.permits_reward_mail_fallback(RewardDeliveryPolicy::InventoryRequired));
        assert!(!blocked.permits_reward_mail_fallback(RewardDeliveryPolicy::PreferInventory));
        assert!(!unknown.permits_reward_mail_fallback(RewardDeliveryPolicy::PreferInventory));
        assert!(AssetCommandErrorCode::CapacityBlocked.player_retryable());
        assert!(!AssetCommandErrorCode::InventoryCapacityFull.player_retryable());
        assert!(unknown.result_state.requires_query_first());
        assert!(AssetCommandErrorCode::ResultUnknown.requires_query_first());
    }

    #[test]
    fn shared_asset_contract_fixture_preserves_order_fingerprint_and_result_shape() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../tests/fixtures/asset-contract-v1.json"
        ))
        .unwrap();
        let contract = &fixture["asset_contract_v1"];
        let input = &contract["reward_order"];
        let items = input["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| serde_json::from_value::<NormalizedAssetItem>(item.clone()).unwrap())
            .collect::<Vec<_>>();
        let operator = serde_json::from_value::<AssetOperator>(input["operator"].clone()).unwrap();
        let order = RewardOrder::new(
            input["request_id"].as_str().unwrap(),
            input["character_id"].as_str().unwrap(),
            AssetOrigin::new(
                serde_json::from_value(input["origin_type"].clone()).unwrap(),
                input["origin_id"].as_str().unwrap(),
            )
            .unwrap(),
            serde_json::from_value(input["delivery_policy"].clone()).unwrap(),
            &items,
            input["reason"].as_str().unwrap(),
            operator,
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(&order).unwrap(),
            contract["expected_reward_order"]
        );
        assert_eq!(
            order.items,
            serde_json::from_value::<Vec<NormalizedAssetItem>>(
                contract["expected_normalized_items"].clone()
            )
            .unwrap()
        );
        assert_eq!(
            order.request_fingerprint().as_str(),
            contract["expected_fingerprint"].as_str().unwrap()
        );

        let result = AssetCommandResult::applied(
            order.request_id.clone(),
            order.request_fingerprint(),
            vec![AssetQuantityDelta {
                asset_type: AssetType::Item,
                item_id: 1002,
                binding: AssetBinding::Unbound,
                delta: 5,
            }],
            vec![AssetContainerVersion {
                container: AssetContainer::Inventory,
                version: 7,
            }],
            vec!["asset-ledger-0001".to_string()],
            Some(AssetDeliveryReceipt::direct("delivery-0001").unwrap()),
        )
        .unwrap();

        assert_eq!(
            serde_json::to_value(result).unwrap(),
            contract["expected_result"]
        );
    }
}
