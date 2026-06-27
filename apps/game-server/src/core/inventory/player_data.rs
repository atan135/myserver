use super::attr::PlayerAttr;
use super::buff::Buff;
use super::container::ItemContainer;
use super::equipment::{EquipSlot, EquipmentSlots};
use super::item::{Item, ItemElementValues, ItemError};
use super::visual::PlayerVisual;
use crate::core::character_element::{CharacterElementChange, ElementDeltas};
use crate::csv_code::itemtable::{ItemTable, ItemTableRow};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterProgressState {
    #[serde(default)]
    pub completed: BTreeMap<String, CharacterProgressRecord>,
    #[serde(default)]
    pub discipline_learning_eligibilities: BTreeSet<String>,
    #[serde(default)]
    pub reward_logs: Vec<CharacterProgressRewardLog>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterProgressRecord {
    pub progress_id: String,
    pub source_type: String,
    pub source_id: String,
    pub completed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterProgressRewardLog {
    pub progress_id: String,
    pub source_type: String,
    pub source_id: String,
    pub reward_type: String,
    pub reward_id: String,
    pub status: String,
    pub created_at: String,
}

/// 角色完整玩法数据
#[derive(Debug, Clone)]
pub struct PlayerData {
    /// 角色 ID
    pub character_id: String,

    // ========== 物品存储 ==========
    /// 背包（随身）
    pub inventory: ItemContainer,
    /// 仓库（固定于主城/据点 NPC 处）
    pub warehouse: ItemContainer,
    /// 装备栏（穿戴中）
    pub equipment: EquipmentSlots,

    // ========== 角色状态 ==========
    /// 属性
    pub attr: PlayerAttr,
    /// 外观
    pub visual: PlayerVisual,
    /// 当前激活的 Buff
    pub buffs: Vec<Buff>,

    /// 任务/成就/活动等正式进度奖励的最小持久化状态。
    pub progress: CharacterProgressState,

    // ========== 脏标记 ==========
    pub attr_dirty: bool,
    pub visual_dirty: bool,
    pub data_dirty: bool,
}

/// 默认背包容量
pub const DEFAULT_INVENTORY_CAPACITY: usize = 48;
/// 默认仓库容量
pub const DEFAULT_WAREHOUSE_CAPACITY: usize = 64;

impl PlayerData {
    /// 创建新角色数据
    pub fn new(character_id: String) -> Self {
        Self::with_capacity(
            character_id,
            DEFAULT_INVENTORY_CAPACITY,
            DEFAULT_WAREHOUSE_CAPACITY,
        )
    }

    /// 使用指定容量创建角色数据
    pub fn with_capacity(
        character_id: String,
        inventory_capacity: usize,
        warehouse_capacity: usize,
    ) -> Self {
        Self {
            character_id: character_id.clone(),
            inventory: ItemContainer::new(inventory_capacity),
            warehouse: ItemContainer::new(warehouse_capacity),
            equipment: EquipmentSlots::new(),
            attr: PlayerAttr::new(),
            visual: PlayerVisual::new(0),
            buffs: Vec::new(),
            progress: CharacterProgressState::default(),
            attr_dirty: true,
            visual_dirty: false,
            data_dirty: true,
        }
    }

    // ========== 脏标记操作 ==========

    pub fn set_attr_dirty(&mut self) {
        self.attr_dirty = true;
        self.data_dirty = true;
    }

    pub fn set_visual_dirty(&mut self) {
        self.visual_dirty = true;
        self.data_dirty = true;
    }

    pub fn set_data_dirty(&mut self) {
        self.data_dirty = true;
    }

    pub fn clear_attr_dirty(&mut self) {
        self.attr_dirty = false;
    }

    pub fn clear_visual_dirty(&mut self) {
        self.visual_dirty = false;
    }

    pub fn clear_data_dirty(&mut self) {
        self.data_dirty = false;
    }

    pub fn is_attr_dirty(&self) -> bool {
        self.attr_dirty
    }

    pub fn is_visual_dirty(&self) -> bool {
        self.visual_dirty
    }

    pub fn is_data_dirty(&self) -> bool {
        self.data_dirty
    }

    // ========== 装备操作 ==========

    /// 穿戴装备
    pub fn equip_item(&mut self, item_uid: u64, item_table: &ItemTable) -> Result<(), ItemError> {
        // 1. 从背包找到物品
        let item = self
            .inventory
            .find_item(item_uid)
            .cloned()
            .ok_or(ItemError::ItemNotFound)?;

        // 2. 确定装备槽位
        let slot = self.determine_equip_slot(item.item_id, item_table)?;

        // 3. 卸下当前装备到背包（如果有）
        if let Some(old_item) = self.equipment.unequip(slot)? {
            self.inventory.add_item(old_item)?;
        }

        // 4. 从背包移除新装备
        self.inventory.remove_item(item_uid, item.count)?;

        // 5. 穿上新装备
        self.equipment.equip(slot, item)?;

        // 6. 触发重算和脏标记
        self.recalculate_attr(item_table);
        self.set_attr_dirty();
        self.set_visual_dirty();

        Ok(())
    }

    /// 卸下装备
    pub fn unequip_item(&mut self, slot: EquipSlot) -> Result<Option<Item>, ItemError> {
        // 1. 卸下装备
        let unequipped = self.equipment.unequip(slot)?;

        // 2. 如果有装备，放入背包
        match unequipped {
            Some(item) => {
                let item_clone = item.clone();
                self.inventory.add_item(item)?;
                self.attr_dirty = true;
                self.visual_dirty = true;
                self.data_dirty = true;
                Ok(Some(item_clone))
            }
            None => {
                self.attr_dirty = true;
                self.visual_dirty = true;
                self.data_dirty = true;
                Ok(None)
            }
        }
    }

    /// 确定物品应该装备到哪个槽位
    fn determine_equip_slot(
        &self,
        item_id: i32,
        item_table: &ItemTable,
    ) -> Result<EquipSlot, ItemError> {
        let row = item_table.get(item_id).ok_or(ItemError::ItemNotFound)?;

        let slot_str = item_table
            .resolve_string(row.equipslot)
            .ok_or(ItemError::SlotMismatch)?;

        EquipSlot::from_str(slot_str).ok_or(ItemError::SlotMismatch)
    }

    // ========== 物品操作 ==========

    /// 添加物品到背包
    pub fn add_item(&mut self, item: Item) -> Result<(), ItemError> {
        self.inventory.add_item(item)?;
        self.set_data_dirty();
        Ok(())
    }

    /// 从背包移除物品
    pub fn remove_item(&mut self, item_uid: u64, count: u32) -> Result<Item, ItemError> {
        let item = self.inventory.remove_item(item_uid, count)?;
        self.set_data_dirty();
        Ok(item)
    }

    /// 使用物品
    pub fn use_item(&mut self, item_uid: u64, item_table: &ItemTable) -> Result<(), ItemError> {
        let plan = self.prepare_item_use(item_uid, item_table)?;

        // 检查使用效果类型
        match plan.effect {
            PreparedItemUseEffect::Heal { hp } => {
                self.inventory.remove_item(item_uid, 1)?;
                self.attr.final_.hp = (self.attr.final_.hp + hp).min(self.attr.final_.max_hp);
                self.set_attr_dirty();
            }
            PreparedItemUseEffect::Buff {
                buff_id,
                name,
                duration_ms,
            } => {
                self.inventory.remove_item(item_uid, 1)?;
                let buff = Buff::new(buff_id, name, duration_ms);
                self.buffs.push(buff);
                self.set_attr_dirty();
                self.set_visual_dirty();
            }
            PreparedItemUseEffect::CharacterElementChange { .. } => {
                return Err(ItemError::CannotUse);
            }
        }

        self.set_data_dirty();
        Ok(())
    }

    pub fn prepare_item_use(
        &self,
        item_uid: u64,
        item_table: &ItemTable,
    ) -> Result<PreparedItemUse, ItemError> {
        let item = self
            .inventory
            .find_item(item_uid)
            .ok_or(ItemError::ItemNotFound)?;

        if item.count == 0 {
            return Err(ItemError::NotEnoughCount);
        }

        if item.is_bound_to_other_character(&self.character_id) {
            return Err(ItemError::CharacterBindingMismatch);
        }

        let row = item_table
            .get(item.item_id)
            .ok_or(ItemError::ItemNotFound)?;
        validate_item_element_config(row)?;

        let effect_str = item_table.resolve_string(row.useeffect).unwrap_or("");
        let effect = match effect_str {
            "Heal" => PreparedItemUseEffect::Heal {
                hp: row.usevalue as i64,
            },
            "Buff" => PreparedItemUseEffect::Buff {
                buff_id: row.usevalue as u32,
                name: item_table
                    .resolve_string(row.name)
                    .unwrap_or("Unknown")
                    .to_string(),
                duration_ms: row.cooldownms as u64,
            },
            "CharacterElementChange" => {
                let change = CharacterElementChange::new(
                    ElementDeltas::new(
                        row.useaffinityearthdelta,
                        row.useaffinityfiredelta,
                        row.useaffinitywaterdelta,
                        row.useaffinitywinddelta,
                    ),
                    ElementDeltas::new(
                        row.usemasteryearthdelta,
                        row.usemasteryfiredelta,
                        row.usemasterywaterdelta,
                        row.usemasterywinddelta,
                    ),
                );

                if change == CharacterElementChange::zero() {
                    return Err(ItemError::InvalidElementDelta);
                }

                PreparedItemUseEffect::CharacterElementChange { change }
            }
            _ => return Err(ItemError::CannotUse),
        };

        Ok(PreparedItemUse {
            item_uid,
            item_id: item.item_id,
            effect,
        })
    }

    pub fn finalize_prepared_item_use(
        &mut self,
        plan: &PreparedItemUse,
        item_table: &ItemTable,
    ) -> Result<(), ItemError> {
        let current = self.prepare_item_use(plan.item_uid, item_table)?;
        if current.item_id != plan.item_id || current.effect != plan.effect.clone() {
            return Err(ItemError::CannotUse);
        }

        match &plan.effect {
            PreparedItemUseEffect::Heal { hp } => {
                self.inventory.remove_item(plan.item_uid, 1)?;
                self.attr.final_.hp = (self.attr.final_.hp + *hp).min(self.attr.final_.max_hp);
                self.set_attr_dirty();
            }
            PreparedItemUseEffect::Buff {
                buff_id,
                name,
                duration_ms,
            } => {
                self.inventory.remove_item(plan.item_uid, 1)?;
                self.buffs
                    .push(Buff::new(*buff_id, name.clone(), *duration_ms));
                self.set_attr_dirty();
                self.set_visual_dirty();
            }
            PreparedItemUseEffect::CharacterElementChange { .. } => {
                self.inventory.remove_item(plan.item_uid, 1)?;
            }
        }

        self.set_data_dirty();
        Ok(())
    }

    // ========== 仓库操作 ==========

    /// 仓库存取（位置校验由调用方负责）
    pub fn warehouse_deposit(&mut self, item_uid: u64, count: u32) -> Result<(), ItemError> {
        let item = self.inventory.remove_item(item_uid, count)?;
        self.warehouse.add_item(item)?;
        self.set_data_dirty();
        Ok(())
    }

    /// 仓库取出
    pub fn warehouse_withdraw(&mut self, item_uid: u64, count: u32) -> Result<(), ItemError> {
        let item = self.warehouse.remove_item(item_uid, count)?;
        self.inventory.add_item(item)?;
        self.set_data_dirty();
        Ok(())
    }

    // ========== 属性重算 ==========

    /// 重算角色属性
    pub fn recalculate_attr(&mut self, item_table: &ItemTable) {
        self.attr
            .recalculate(&self.equipment, item_table, &self.buffs);
    }

    pub fn equipped_item_elements(&self, item_table: &ItemTable) -> ItemElementValues {
        self.equipment
            .iter()
            .fold(ItemElementValues::zero(), |acc, (_, item)| {
                acc.saturating_add(item.effective_elements(item_table.get(item.item_id)))
            })
    }

    pub fn effective_item_elements(&self, item_table: &ItemTable) -> ItemElementValues {
        self.equipped_item_elements(item_table)
    }

    // ========== 帧末处理 ==========

    /// 帧末处理（处理脏标记和通知）
    pub fn tick(&mut self) {
        // 清理已过期的 Buff
        self.buffs.retain(|buff| buff.duration_ms > 0);

        // 这里会由调用方处理具体的通知发送
        // 因为通知需要访问玩家所在的场景/房间信息
    }

    /// 获取背包所有物品（用于同步给客户端）
    pub fn get_inventory_items(&self) -> Vec<&Item> {
        self.inventory.non_empty_items()
    }

    /// 获取仓库所有物品
    pub fn get_warehouse_items(&self) -> Vec<&Item> {
        self.warehouse.non_empty_items()
    }

    /// 获取已装备的物品
    pub fn get_equipped_items(&self) -> Vec<(EquipSlot, &Item)> {
        self.equipment.iter().collect()
    }

    /// 获取当前 HP
    pub fn get_hp(&self) -> i64 {
        self.attr.final_.hp
    }

    /// 获取最大 HP
    pub fn get_max_hp(&self) -> i64 {
        self.attr.final_.max_hp
    }

    /// 设置 HP
    pub fn set_hp(&mut self, hp: i64) {
        self.attr.final_.hp = hp.max(0).min(self.attr.final_.max_hp);
        self.set_attr_dirty();
    }

    /// 增加 HP
    pub fn heal(&mut self, amount: i64) {
        self.set_hp(self.attr.final_.hp + amount);
    }

    /// 减少 HP
    pub fn damage(&mut self, amount: i64) -> i64 {
        let actual = amount.min(self.attr.final_.hp);
        self.set_hp(self.attr.final_.hp - actual);
        actual
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedItemUse {
    pub item_uid: u64,
    pub item_id: i32,
    pub effect: PreparedItemUseEffect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedItemUseEffect {
    Heal {
        hp: i64,
    },
    Buff {
        buff_id: u32,
        name: String,
        duration_ms: u64,
    },
    CharacterElementChange {
        change: CharacterElementChange,
    },
}

fn validate_item_element_config(row: &ItemTableRow) -> Result<(), ItemError> {
    let template = ItemElementValues::from_template_row(row);
    if template.has_negative() {
        return Err(ItemError::InvalidItemConfig);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csv_code::itemtable::ItemTableRow;
    use std::collections::HashMap;

    const STRING_CHARACTER_ELEMENT_CHANGE: u32 = 1;

    struct ItemTableBuilder {
        string_pool: HashMap<u32, String>,
        rows: Vec<ItemTableRow>,
        by_id: HashMap<i32, usize>,
        next_key: u32,
    }

    impl ItemTableBuilder {
        fn new() -> Self {
            let mut string_pool = HashMap::new();
            string_pool.insert(
                STRING_CHARACTER_ELEMENT_CHANGE,
                "CharacterElementChange".to_string(),
            );
            Self {
                string_pool,
                rows: Vec::new(),
                by_id: HashMap::new(),
                next_key: 100,
            }
        }

        fn key(&mut self, value: &str) -> u32 {
            if let Some((&key, _)) = self
                .string_pool
                .iter()
                .find(|(_, existing)| existing.as_str() == value)
            {
                return key;
            }

            let key = self.next_key;
            self.next_key += 1;
            self.string_pool.insert(key, value.to_string());
            key
        }

        fn add_row(&mut self, mut row: ItemTableRow) {
            if row.code == 0 {
                row.code = self.key("item");
            }
            if row.name == 0 {
                row.name = self.key("Item");
            }
            if row.type_ == 0 {
                row.type_ = self.key("Consumable");
            }
            if row.quality == 0 {
                row.quality = self.key("White");
            }
            if row.equipslot == 0 {
                row.equipslot = self.key("None");
            }
            if row.bindtype == 0 {
                row.bindtype = self.key("Never");
            }
            if row.useeffect == 0 {
                row.useeffect = self.key("None");
            }
            if row.usetarget == 0 {
                row.usetarget = self.key("Self");
            }
            if row.growthsource == 0 {
                row.growthsource = self.key("None");
            }
            if row.traderule == 0 {
                row.traderule = self.key("Tradable");
            }
            if row.decomposerule == 0 {
                row.decomposerule = self.key("None");
            }
            if row.inheritrule == 0 {
                row.inheritrule = self.key("None");
            }
            if row.disciplineconditionkey == 0 {
                row.disciplineconditionkey = self.key("None");
            }
            if row.titleunlocksource == 0 {
                row.titleunlocksource = self.key("None");
            }
            if row.description == 0 {
                row.description = self.key("desc");
            }

            self.by_id.insert(row.id, self.rows.len());
            self.rows.push(row);
        }

        fn finish(self) -> ItemTable {
            ItemTable {
                string_pool: self.string_pool,
                rows: self.rows,
                by_id: self.by_id,
            }
        }
    }

    fn item_table(rows: Vec<ItemTableRow>) -> ItemTable {
        let mut builder = ItemTableBuilder::new();
        for row in rows {
            builder.add_row(row);
        }
        builder.finish()
    }

    fn element_item_row(id: i32) -> ItemTableRow {
        ItemTableRow {
            id,
            maxstack: 99,
            useeffect: STRING_CHARACTER_ELEMENT_CHANGE,
            usemasteryfiredelta: 10,
            ..ItemTableRow::default()
        }
    }

    fn equipment_row(id: i32, fire: i32) -> ItemTableRow {
        ItemTableRow {
            id,
            maxstack: 1,
            templateelementfire: fire,
            ..ItemTableRow::default()
        }
    }

    #[test]
    fn test_player_data_creation() {
        let player = PlayerData::new("chr_0000000000001".to_string());

        assert_eq!(player.character_id, "chr_0000000000001");
        assert_eq!(player.inventory.capacity(), DEFAULT_INVENTORY_CAPACITY);
        assert_eq!(player.warehouse.capacity(), DEFAULT_WAREHOUSE_CAPACITY);
    }

    #[test]
    fn test_add_remove_item() {
        let mut player = PlayerData::new("chr_0000000000001".to_string());

        let item = Item::new(1, 1001, 5, false);
        player.add_item(item).unwrap();

        assert_eq!(player.inventory.item_count(), 1);
        assert!(player.is_data_dirty());

        let removed = player.remove_item(1, 2).unwrap();
        assert_eq!(removed.count, 2);
        assert_eq!(player.inventory.item_count(), 1);
    }

    #[test]
    fn prepare_element_item_use_builds_character_element_change() {
        let table = item_table(vec![element_item_row(4101)]);
        let mut player = PlayerData::new("chr_0000000000001".to_string());
        player.add_item(Item::new(1, 4101, 1, false)).unwrap();

        let plan = player.prepare_item_use(1, &table).unwrap();

        assert_eq!(
            plan.effect,
            PreparedItemUseEffect::CharacterElementChange {
                change: CharacterElementChange::new(
                    ElementDeltas::zero(),
                    ElementDeltas::new(0, 10, 0, 0)
                )
            }
        );
        assert_eq!(player.inventory.find_item(1).unwrap().count, 1);
    }

    #[test]
    fn invalid_zero_element_delta_is_rejected_without_consuming_item() {
        let table = item_table(vec![ItemTableRow {
            id: 4101,
            maxstack: 99,
            useeffect: STRING_CHARACTER_ELEMENT_CHANGE,
            ..ItemTableRow::default()
        }]);
        let mut player = PlayerData::new("chr_0000000000001".to_string());
        player.add_item(Item::new(1, 4101, 1, false)).unwrap();

        let error = player.prepare_item_use(1, &table).unwrap_err();

        assert_eq!(error, ItemError::InvalidElementDelta);
        assert_eq!(player.inventory.find_item(1).unwrap().count, 1);
    }

    #[test]
    fn invalid_negative_template_element_config_is_rejected() {
        let table = item_table(vec![ItemTableRow {
            id: 4101,
            maxstack: 99,
            templateelementearth: -1,
            usemasteryfiredelta: 10,
            ..ItemTableRow::default()
        }]);
        let mut player = PlayerData::new("chr_0000000000001".to_string());
        player.add_item(Item::new(1, 4101, 1, false)).unwrap();

        let error = player.prepare_item_use(1, &table).unwrap_err();

        assert_eq!(error, ItemError::InvalidItemConfig);
        assert_eq!(player.inventory.find_item(1).unwrap().count, 1);
    }

    #[test]
    fn bound_item_cannot_be_used_by_other_character() {
        let table = item_table(vec![element_item_row(4101)]);
        let mut player = PlayerData::new("chr_0000000000001".to_string());
        let mut item = Item::new(1, 4101, 1, true);
        item.bound_character_id = Some("chr_0000000000002".to_string());
        player.add_item(item).unwrap();

        let error = player.prepare_item_use(1, &table).unwrap_err();

        assert_eq!(error, ItemError::CharacterBindingMismatch);
        assert_eq!(player.inventory.find_item(1).unwrap().count, 1);
    }

    #[test]
    fn finalize_prepared_item_use_consumes_once_and_repeat_fails() {
        let table = item_table(vec![element_item_row(4101)]);
        let mut player = PlayerData::new("chr_0000000000001".to_string());
        player.add_item(Item::new(1, 4101, 1, false)).unwrap();
        let plan = player.prepare_item_use(1, &table).unwrap();

        player.finalize_prepared_item_use(&plan, &table).unwrap();
        let repeated = player
            .finalize_prepared_item_use(&plan, &table)
            .unwrap_err();

        assert_eq!(repeated, ItemError::ItemNotFound);
        assert!(player.inventory.find_item(1).is_none());
    }

    #[test]
    fn equipped_item_elements_include_template_growth_and_runtime_without_changing_base_attr() {
        let table = item_table(vec![equipment_row(1002, 80)]);
        let mut player = PlayerData::new("chr_0000000000001".to_string());
        player.attr.base.attack = 12;
        player.attr.final_.attack = 12;
        let mut item = Item::new(1, 1002, 1, false);
        item.growth_elements = ItemElementValues::new(1, 2, 3, 4);
        item.runtime_elements = ItemElementValues::new(0, 5, 0, 0);
        player.equipment.equip(EquipSlot::Weapon, item).unwrap();

        let elements = player.effective_item_elements(&table);

        assert_eq!(elements, ItemElementValues::new(1, 87, 3, 4));
        assert_eq!(player.attr.base.attack, 12);
    }
}
