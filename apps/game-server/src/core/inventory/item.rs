use serde::{Deserialize, Serialize};

use crate::csv_code::itemtable::{ItemTable, ItemTableRow};

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
    /// 配置表模板四属性快照；老数据缺失时按零值兼容，计算时可回退到当前配置表。
    #[serde(default)]
    pub template_elements: ItemElementValues,
    /// 实例长期成长四属性。
    #[serde(default)]
    pub growth_elements: ItemElementValues,
    /// 实例运行期临时四属性。
    #[serde(default)]
    pub runtime_elements: ItemElementValues,
    /// 绑定到的角色。仅 `binded=true` 不足以表达跨角色校验，后续交易/继承也读取此字段。
    #[serde(default)]
    pub bound_character_id: Option<String>,
    /// 可成长道具的规则边界快照。
    #[serde(default)]
    pub growth_rules: ItemGrowthRules,
    /// 实例成长记录。
    #[serde(default)]
    pub growth_records: Vec<ItemGrowthRecord>,
}

impl Item {
    pub fn new(uid: u64, item_id: i32, count: u32, binded: bool) -> Self {
        Self {
            uid,
            item_id,
            count,
            binded,
            template_elements: ItemElementValues::zero(),
            growth_elements: ItemElementValues::zero(),
            runtime_elements: ItemElementValues::zero(),
            bound_character_id: None,
            growth_rules: ItemGrowthRules::default(),
            growth_records: Vec::new(),
        }
    }

    pub fn from_config(
        uid: u64,
        item_id: i32,
        count: u32,
        binded: bool,
        bound_character_id: Option<&str>,
        row: &ItemTableRow,
        item_table: &ItemTable,
    ) -> Self {
        let mut item = Self::new(uid, item_id, count, binded);
        if binded {
            if let Some(character_id) = bound_character_id {
                item.bind_to_character(character_id);
            }
        }
        item.template_elements = ItemElementValues::from_template_row(row);
        item.growth_rules = ItemGrowthRules::from_row(row, item_table);
        item
    }

    pub fn can_stack_with(&self, other: &Item) -> bool {
        self.asset_stack_identity()
            .can_stack_with(&other.asset_stack_identity())
    }

    pub fn effective_elements(&self, row: Option<&ItemTableRow>) -> ItemElementValues {
        let template = if self.template_elements.is_zero() {
            row.map(ItemElementValues::from_template_row)
                .unwrap_or_else(ItemElementValues::zero)
        } else {
            self.template_elements
        };

        template
            .saturating_add(self.growth_elements)
            .saturating_add(self.runtime_elements)
    }

    pub fn is_bound_to_other_character(&self, character_id: &str) -> bool {
        self.bound_character_id
            .as_deref()
            .is_some_and(|bound_character_id| bound_character_id != character_id)
    }

    pub fn bind_to_character(&mut self, character_id: impl Into<String>) {
        self.binded = true;
        self.bound_character_id = Some(character_id.into());
    }

    pub fn record_growth(
        &mut self,
        source: impl Into<String>,
        after: ItemElementValues,
        reason: Option<String>,
    ) {
        let before = self.growth_elements;
        self.growth_elements = after;
        self.growth_records.push(ItemGrowthRecord {
            source: source.into(),
            before,
            after,
            bound_character_id: self.bound_character_id.clone(),
            trade_rule: self.growth_rules.trade_rule.clone(),
            decompose_rule: self.growth_rules.decompose_rule.clone(),
            inherit_rule: self.growth_rules.inherit_rule.clone(),
            reason,
        });
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemElementValues {
    pub earth: i32,
    pub fire: i32,
    pub water: i32,
    pub wind: i32,
}

impl ItemElementValues {
    pub const fn new(earth: i32, fire: i32, water: i32, wind: i32) -> Self {
        Self {
            earth,
            fire,
            water,
            wind,
        }
    }

    pub const fn zero() -> Self {
        Self::new(0, 0, 0, 0)
    }

    pub fn from_template_row(row: &ItemTableRow) -> Self {
        Self::new(
            row.templateelementearth,
            row.templateelementfire,
            row.templateelementwater,
            row.templateelementwind,
        )
    }

    pub fn is_zero(self) -> bool {
        self == Self::zero()
    }

    pub fn has_negative(self) -> bool {
        self.earth < 0 || self.fire < 0 || self.water < 0 || self.wind < 0
    }

    pub fn saturating_add(self, other: Self) -> Self {
        Self::new(
            self.earth.saturating_add(other.earth),
            self.fire.saturating_add(other.fire),
            self.water.saturating_add(other.water),
            self.wind.saturating_add(other.wind),
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemGrowthRules {
    pub growth_enabled: bool,
    pub growth_source: Option<String>,
    pub trade_rule: String,
    pub decompose_rule: String,
    pub inherit_rule: String,
    pub discipline_condition_key: Option<String>,
    pub title_unlock_source: Option<String>,
}

impl ItemGrowthRules {
    fn from_row(row: &ItemTableRow, item_table: &ItemTable) -> Self {
        Self {
            growth_enabled: row.growthenabled != 0,
            growth_source: non_empty_string(item_table.resolve_string(row.growthsource)),
            trade_rule: item_table
                .resolve_string(row.traderule)
                .unwrap_or("None")
                .to_string(),
            decompose_rule: item_table
                .resolve_string(row.decomposerule)
                .unwrap_or("None")
                .to_string(),
            inherit_rule: item_table
                .resolve_string(row.inheritrule)
                .unwrap_or("None")
                .to_string(),
            discipline_condition_key: non_empty_string(
                item_table.resolve_string(row.disciplineconditionkey),
            ),
            title_unlock_source: non_empty_string(item_table.resolve_string(row.titleunlocksource)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemGrowthRecord {
    pub source: String,
    pub before: ItemElementValues,
    pub after: ItemElementValues,
    pub bound_character_id: Option<String>,
    pub trade_rule: String,
    pub decompose_rule: String,
    pub inherit_rule: String,
    pub reason: Option<String>,
}

fn non_empty_string(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() || value == "None" {
        return None;
    }
    Some(value.to_string())
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
    Cooldown,
    InvalidItemConfig,
    InvalidElementDelta,
    CharacterBindingMismatch,
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
            ItemError::Cooldown => "COOLDOWN",
            ItemError::InvalidItemConfig => "INVALID_ITEM_CONFIG",
            ItemError::InvalidElementDelta => "INVALID_ELEMENT_DELTA",
            ItemError::CharacterBindingMismatch => "ITEM_BINDING_MISMATCH",
            ItemError::Unknown => "UNKNOWN_ERROR",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config_table::CsvTableLoader;
    use std::path::Path;

    #[test]
    fn old_item_json_deserializes_with_default_element_fields() {
        let item: Item =
            serde_json::from_str(r#"{"uid":1,"item_id":1001,"count":1,"binded":false}"#).unwrap();

        assert_eq!(item.template_elements, ItemElementValues::zero());
        assert_eq!(item.growth_elements, ItemElementValues::zero());
        assert_eq!(item.runtime_elements, ItemElementValues::zero());
        assert!(item.growth_records.is_empty());
    }

    #[test]
    fn from_config_binds_target_character_and_snapshots_growth_rules() {
        let table = ItemTable::load_from_csv(Path::new("csv/ItemTable.csv")).unwrap();
        let row = table.get(1002).unwrap();

        let item = Item::from_config(1, 1002, 1, true, Some("chr_0000000000001"), row, &table);

        assert!(item.binded);
        assert_eq!(
            item.bound_character_id.as_deref(),
            Some("chr_0000000000001")
        );
        assert_eq!(item.template_elements, ItemElementValues::new(0, 80, 0, 0));
        assert!(item.growth_rules.growth_enabled);
        assert_eq!(item.growth_rules.growth_source.as_deref(), Some("Enhance"));
        assert_eq!(item.growth_rules.trade_rule, "NoTradeAfterGrowth");
        assert_eq!(item.growth_rules.decompose_rule, "ReturnMaterials");
        assert_eq!(item.growth_rules.inherit_rule, "InheritGrowth");
        assert_eq!(
            item.growth_rules.discipline_condition_key.as_deref(),
            Some("discipline:sword_fire")
        );
        assert_eq!(
            item.growth_rules.title_unlock_source.as_deref(),
            Some("item:sword_002")
        );
    }

    #[test]
    fn growth_record_persists_source_bound_character_and_rules() {
        let mut item = Item::new(1, 1001, 1, false);
        item.bind_to_character("chr_0000000000001");
        item.growth_rules = ItemGrowthRules {
            growth_enabled: true,
            growth_source: Some("enhance".to_string()),
            trade_rule: "NoTrade".to_string(),
            decompose_rule: "ReturnMaterials".to_string(),
            inherit_rule: "InheritGrowth".to_string(),
            discipline_condition_key: Some("discipline:flame".to_string()),
            title_unlock_source: Some("item:flame_sword".to_string()),
        };

        item.record_growth(
            "enhance",
            ItemElementValues::new(1, 2, 3, 4),
            Some("unit-test".to_string()),
        );

        let restored: Item = serde_json::from_value(serde_json::to_value(&item).unwrap()).unwrap();
        assert_eq!(restored.growth_elements, ItemElementValues::new(1, 2, 3, 4));
        assert_eq!(restored.growth_records[0].source, "enhance");
        assert_eq!(
            restored.growth_records[0].bound_character_id.as_deref(),
            Some("chr_0000000000001")
        );
        assert_eq!(restored.growth_records[0].inherit_rule, "InheritGrowth");
    }
}
