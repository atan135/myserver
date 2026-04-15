use serde::{Deserialize, Serialize};

/// 物品实例
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    /// 唯一实例ID（用于区分同名物品）
    pub uid: u64,
    /// 物品配置ID（对应 ItemTableRow.id）
    pub item_id: i32,
    /// 数量（堆叠物品）
    pub count: u32,
    /// 是否绑定（绑定后不可交易）
    pub binded: bool,
}

impl Item {
    pub fn new(uid: u64, item_id: i32, count: u32, binded: bool) -> Self {
        Self {
            uid,
            item_id,
            count,
            binded,
        }
    }
}

/// 物品操作错误
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemError {
    ItemNotFound,
    SlotMismatch,
    StackOverflow,
    CannotUse,
    NotEnoughCount,
    InventoryFull,
    WarehouseFull,
    CannotTrade,
    LevelRequired,
    Cooldown,
    Unknown,
}

impl ItemError {
    pub fn as_str(&self) -> &'static str {
        match self {
            ItemError::ItemNotFound => "ITEM_NOT_FOUND",
            ItemError::SlotMismatch => "SLOT_MISMATCH",
            ItemError::StackOverflow => "STACK_OVERFLOW",
            ItemError::CannotUse => "CANNOT_USE",
            ItemError::NotEnoughCount => "NOT_ENOUGH_COUNT",
            ItemError::InventoryFull => "INVENTORY_FULL",
            ItemError::WarehouseFull => "WAREHOUSE_FULL",
            ItemError::CannotTrade => "CANNOT_TRADE",
            ItemError::LevelRequired => "LEVEL_REQUIRED",
            ItemError::Cooldown => "COOLDOWN",
            ItemError::Unknown => "UNKNOWN_ERROR",
        }
    }
}
