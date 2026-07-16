use serde::{Deserialize, Serialize};

use super::item::{Item, ItemElementValues, ItemGrowthRules};

/// 统一资产模型的类别。
///
/// 首期只有 `Item` 具备持久化和操作实现；`Currency` 仅保留类型位置，不能被当前
/// inventory 入口、容器或 grant 路径使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetType {
    Item,
    Currency,
}

/// 已提交角色物品可处于的容器。
///
/// 邮件附件是待交付状态而不是角色资产容器。邮件领取成功后，物品的最终结算目标
/// 仍然是 `Inventory`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetContainer {
    Inventory,
    Warehouse,
    Equipment,
}

/// 物品绑定状态。
///
/// 两个 `Legacy*` 变体明确保留旧 JSONB 中 `binded` 与 `bound_character_id` 不一致的
/// 事实。后续资产事务不得把这类状态误判为可交易的未绑定物品。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetBinding {
    Unbound,
    CharacterBound { character_id: String },
    LegacyBoundWithoutCharacter,
    LegacyUnboundWithCharacter { character_id: String },
}

impl AssetBinding {
    pub fn from_item(item: &Item) -> Self {
        match (item.binded, item.bound_character_id.as_deref()) {
            (false, None) => Self::Unbound,
            (false, Some(character_id)) => Self::LegacyUnboundWithCharacter {
                character_id: character_id.to_string(),
            },
            (true, Some(character_id)) => Self::CharacterBound {
                character_id: character_id.to_string(),
            },
            (true, None) => Self::LegacyBoundWithoutCharacter,
        }
    }
}

/// 资产冻结状态。
///
/// 当前 `Item` JSONB 快照尚未持久化锁定字段，因此从现有物品生成的身份一律为
/// `Unlocked`。阶段 4 在写入冻结能力前必须把该状态纳入快照与所有资产操作校验。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetLockState {
    Unlocked,
    Frozen { reason: String },
}

impl Default for AssetLockState {
    fn default() -> Self {
        Self::Unlocked
    }
}

/// 配置表身份。当前物品只存配置属性快照，未持久化 ItemTable 的修订号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetConfigTable {
    ItemTable,
}

/// 配置版本进入堆叠身份，避免未来热更后的规则快照被错误合并。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetConfigVersion {
    LegacyUnversioned {
        table: AssetConfigTable,
    },
    Revision {
        table: AssetConfigTable,
        revision: String,
    },
}

impl AssetConfigVersion {
    pub const fn legacy_item_table() -> Self {
        Self::LegacyUnversioned {
            table: AssetConfigTable::ItemTable,
        }
    }
}

impl Default for AssetConfigVersion {
    fn default() -> Self {
        Self::legacy_item_table()
    }
}

/// 决定物品能否与另一个格子合并的完整身份。
///
/// UID、数量与所在格子不属于堆叠身份；绑定、属性/规则快照、配置版本、冻结状态和
/// 成长历史均属于身份。拥有成长历史的实例始终独立占格，保持既有 `Item` 行为。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemStackIdentity {
    pub item_id: i32,
    pub binding: AssetBinding,
    pub lock_state: AssetLockState,
    pub config_version: AssetConfigVersion,
    pub expires_at_ms: Option<i64>,
    pub template_elements: ItemElementValues,
    pub growth_elements: ItemElementValues,
    pub runtime_elements: ItemElementValues,
    pub growth_rules: ItemGrowthRules,
    pub has_growth_history: bool,
}

impl ItemStackIdentity {
    /// 从已持久化的当前 `Item` 生成身份。
    pub fn from_item(item: &Item) -> Self {
        Self::from_item_state(item, item.lock_state.clone(), item.config_version.clone())
    }

    /// 为后续拥有配置修订和锁定快照的资产事务构造身份。
    pub fn from_item_state(
        item: &Item,
        lock_state: AssetLockState,
        config_version: AssetConfigVersion,
    ) -> Self {
        Self {
            item_id: item.item_id,
            binding: AssetBinding::from_item(item),
            lock_state,
            config_version,
            expires_at_ms: item.expires_at_ms,
            template_elements: item.template_elements,
            growth_elements: item.growth_elements,
            runtime_elements: item.runtime_elements,
            growth_rules: item.growth_rules.clone(),
            has_growth_history: !item.growth_records.is_empty(),
        }
    }

    pub fn can_stack_with(&self, other: &Self) -> bool {
        self.lock_state == AssetLockState::Unlocked
            && other.lock_state == AssetLockState::Unlocked
            && !self.has_growth_history
            && !other.has_growth_history
            && self == other
    }
}

/// 当前首期可堆叠资产身份。货币在落地独立余额模型前没有此类身份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetStackIdentity {
    Item(ItemStackIdentity),
}

impl AssetStackIdentity {
    pub const fn asset_type(&self) -> AssetType {
        match self {
            Self::Item(_) => AssetType::Item,
        }
    }

    pub fn can_stack_with(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Item(left), Self::Item(right)) => left.can_stack_with(right),
        }
    }
}

impl Item {
    /// 当前 JSONB 物品的统一资产堆叠身份。
    pub fn asset_stack_identity(&self) -> AssetStackIdentity {
        AssetStackIdentity::Item(ItemStackIdentity::from_item(self))
    }
}

/// 资产变更的业务来源，值是后续流水和审计使用的稳定枚举而不是展示文本。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetOriginType {
    Achievement,
    Quest,
    Battle,
    ScenePickup,
    Activity,
    Ranking,
    WorldEvent,
    Gm,
    MailClaim,
    PlayerOperation,
    System,
}

impl AssetOriginType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Achievement => "achievement",
            Self::Quest => "quest",
            Self::Battle => "battle",
            Self::ScenePickup => "scene_pickup",
            Self::Activity => "activity",
            Self::Ranking => "ranking",
            Self::WorldEvent => "world_event",
            Self::Gm => "gm",
            Self::MailClaim => "mail_claim",
            Self::PlayerOperation => "player_operation",
            Self::System => "system",
        }
    }
}

/// 来源 ID 必须是可重试、可审计的业务主键，例如任务结算 ID、掉落实体 ID 或 GM 操作 ID。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetOrigin {
    pub origin_type: AssetOriginType,
    pub origin_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetOriginError {
    EmptyOriginId,
}

impl std::fmt::Display for AssetOriginError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyOriginId => formatter.write_str("asset origin_id must not be empty"),
        }
    }
}

impl std::error::Error for AssetOriginError {}

impl AssetOrigin {
    pub fn new(
        origin_type: AssetOriginType,
        origin_id: impl Into<String>,
    ) -> Result<Self, AssetOriginError> {
        let origin_id = origin_id.into();
        if origin_id.trim().is_empty() {
            return Err(AssetOriginError::EmptyOriginId);
        }
        Ok(Self {
            origin_type,
            origin_id,
        })
    }
}

/// 奖励交付方式。它不描述最终的角色物品容器。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetDeliveryMethod {
    Direct,
    Mail,
}

/// 首期物品奖励提交后仅结算至角色背包。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetSettlementTarget {
    Inventory,
}

/// 推送只在权威状态提交后发生，失败不改变已提交资产。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetPostCommitNotification {
    Push,
}

/// 交付、最终结算和提交后通知的固定语义。
///
/// 这里不承载奖励内容、request_id 或执行结果；这些由后续 `RewardOrder` 和资产命令契约定义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetDeliverySemantics {
    pub delivery_method: AssetDeliveryMethod,
    pub settlement_target: AssetSettlementTarget,
    pub post_commit_notification: Option<AssetPostCommitNotification>,
}

impl AssetDeliverySemantics {
    pub const fn to_inventory(delivery_method: AssetDeliveryMethod) -> Self {
        Self {
            delivery_method,
            settlement_target: AssetSettlementTarget::Inventory,
            post_commit_notification: Some(AssetPostCommitNotification::Push),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_item_stacking_uses_asset_stack_identity() {
        let first = Item::new(1, 1001, 2, false);
        let second = Item::new(2, 1001, 3, false);

        assert_eq!(first.asset_stack_identity().asset_type(), AssetType::Item);
        assert!(
            first
                .asset_stack_identity()
                .can_stack_with(&second.asset_stack_identity())
        );
    }

    #[test]
    fn frozen_or_growth_history_item_is_never_stackable() {
        let mut item = Item::new(1, 1001, 1, false);
        item.record_growth("enhance", ItemElementValues::new(1, 0, 0, 0), None);

        let frozen = ItemStackIdentity::from_item_state(
            &item,
            AssetLockState::Frozen {
                reason: "trade".to_string(),
            },
            AssetConfigVersion::Revision {
                table: AssetConfigTable::ItemTable,
                revision: "itemtable-20260715".to_string(),
            },
        );
        let unlocked = ItemStackIdentity::from_item(&item);

        assert!(!frozen.can_stack_with(&unlocked));
        assert!(!unlocked.can_stack_with(&unlocked));
    }

    #[test]
    fn inconsistent_legacy_binding_does_not_merge_with_unbound_item() {
        let unbound = Item::new(1, 1001, 1, false);
        let mut inconsistent = Item::new(2, 1001, 1, false);
        inconsistent.bound_character_id = Some("chr_legacy".to_string());

        assert!(
            !unbound
                .asset_stack_identity()
                .can_stack_with(&inconsistent.asset_stack_identity())
        );
    }

    #[test]
    fn delivery_semantics_keep_mail_out_of_the_final_container() {
        let semantics = AssetDeliverySemantics::to_inventory(AssetDeliveryMethod::Mail);

        assert_eq!(semantics.delivery_method, AssetDeliveryMethod::Mail);
        assert_eq!(
            semantics.settlement_target,
            AssetSettlementTarget::Inventory
        );
        assert_eq!(
            semantics.post_commit_notification,
            Some(AssetPostCommitNotification::Push)
        );
    }
}
