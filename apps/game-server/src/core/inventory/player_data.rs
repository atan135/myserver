use super::attr::PlayerAttr;
use super::buff::Buff;
use super::container::ItemContainer;
use super::equipment::{EquipSlot, EquipmentSlots};
use super::item::{Item, ItemError};
use super::visual::PlayerVisual;
use crate::csv_code::itemtable::ItemTable;

/// 玩家完整数据
#[derive(Debug, Clone)]
pub struct PlayerData {
    /// 玩家 ID
    pub player_id: String,

    // ========== 物品存储 ==========
    /// 背包（随身）
    pub inventory: ItemContainer,
    /// 仓库（固定于主城/据点 NPC 处）
    pub warehouse: ItemContainer,
    /// 装备栏（穿戴中）
    pub equipment: EquipmentSlots,

    // ========== 角色状态 ==========
    /// 等级
    pub level: i32,
    /// 属性
    pub attr: PlayerAttr,
    /// 外观
    pub visual: PlayerVisual,
    /// 当前激活的 Buff
    pub buffs: Vec<Buff>,

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
    /// 创建新玩家数据
    pub fn new(player_id: String) -> Self {
        Self::with_capacity(player_id, DEFAULT_INVENTORY_CAPACITY, DEFAULT_WAREHOUSE_CAPACITY)
    }

    /// 使用指定容量创建玩家数据
    pub fn with_capacity(
        player_id: String,
        inventory_capacity: usize,
        warehouse_capacity: usize,
    ) -> Self {
        Self {
            player_id: player_id.clone(),
            inventory: ItemContainer::new(inventory_capacity),
            warehouse: ItemContainer::new(warehouse_capacity),
            equipment: EquipmentSlots::new(),
            level: 1,
            attr: PlayerAttr::new(),
            visual: PlayerVisual::new(0),
            buffs: Vec::new(),
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
    pub fn equip_item(
        &mut self,
        item_uid: u64,
        item_table: &ItemTable,
    ) -> Result<(), ItemError> {
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
    pub fn unequip_item(
        &mut self,
        slot: EquipSlot,
    ) -> Result<Option<Item>, ItemError> {
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
        let row = item_table
            .get(item_id)
            .ok_or(ItemError::ItemNotFound)?;

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
    pub fn use_item(
        &mut self,
        item_uid: u64,
        item_table: &ItemTable,
    ) -> Result<(), ItemError> {
        let item = self
            .inventory
            .find_item(item_uid)
            .ok_or(ItemError::ItemNotFound)?
            .clone();

        let row = item_table
            .get(item.item_id)
            .ok_or(ItemError::ItemNotFound)?;

        // 检查使用效果类型
        let effect_str = item_table
            .resolve_string(row.useeffect)
            .unwrap_or("");

        match effect_str {
            "Heal" => {
                // 消耗品：直接移除，产生效果
                self.inventory.remove_item(item_uid, 1)?;
                self.attr.final_.hp = (self.attr.final_.hp + row.usevalue as i64).min(self.attr.final_.max_hp);
                self.set_attr_dirty();
            }
            "Buff" => {
                // 产生 Buff
                self.inventory.remove_item(item_uid, 1)?;
                let buff = Buff::new(
                    row.usevalue as u32,
                    item_table
                        .resolve_string(row.name)
                        .unwrap_or("Unknown")
                        .to_string(),
                    row.cooldownms as u64,
                );
                self.buffs.push(buff);
                self.set_attr_dirty();
                self.set_visual_dirty();
            }
            _ => {
                return Err(ItemError::CannotUse);
            }
        }

        self.set_data_dirty();
        Ok(())
    }

    // ========== 仓库操作 ==========

    /// 仓库存取（位置校验由调用方负责）
    pub fn warehouse_deposit(
        &mut self,
        item_uid: u64,
        count: u32,
    ) -> Result<(), ItemError> {
        let item = self.inventory.remove_item(item_uid, count)?;
        self.warehouse.add_item(item)?;
        self.set_data_dirty();
        Ok(())
    }

    /// 仓库取出
    pub fn warehouse_withdraw(
        &mut self,
        item_uid: u64,
        count: u32,
    ) -> Result<(), ItemError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_item_table() -> ItemTable {
        // 创建测试用 ItemTable
        // 实际使用时应该从 CSV 加载
        ItemTable {
            string_pool: std::collections::HashMap::new(),
            rows: vec![],
            by_id: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn test_player_data_creation() {
        let player = PlayerData::new("player1".to_string());

        assert_eq!(player.player_id, "player1");
        assert_eq!(player.level, 1);
        assert_eq!(player.inventory.capacity(), DEFAULT_INVENTORY_CAPACITY);
        assert_eq!(player.warehouse.capacity(), DEFAULT_WAREHOUSE_CAPACITY);
    }

    #[test]
    fn test_add_remove_item() {
        let mut player = PlayerData::new("player1".to_string());

        let item = Item::new(1, 1001, 5, false);
        player.add_item(item).unwrap();

        assert_eq!(player.inventory.item_count(), 1);
        assert!(player.is_data_dirty());

        let removed = player.remove_item(1, 2).unwrap();
        assert_eq!(removed.count, 2);
        assert_eq!(player.inventory.item_count(), 1);
    }
}
