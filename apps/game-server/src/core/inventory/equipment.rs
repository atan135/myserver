use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::item::{Item, ItemError};

/// 装备槽位
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(i32)]
pub enum EquipSlot {
    Weapon = 0,
    Armor = 1,
    Helmet = 2,
    Pants = 3,
    Shoes = 4,
    Accessory = 5,
}

impl EquipSlot {
    /// 从字符串解析槽位
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Weapon" => Some(EquipSlot::Weapon),
            "Armor" => Some(EquipSlot::Armor),
            "Helmet" => Some(EquipSlot::Helmet),
            "Pants" => Some(EquipSlot::Pants),
            "Shoes" => Some(EquipSlot::Shoes),
            "Accessory" => Some(EquipSlot::Accessory),
            _ => None,
        }
    }

    /// 转为字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            EquipSlot::Weapon => "Weapon",
            EquipSlot::Armor => "Armor",
            EquipSlot::Helmet => "Helmet",
            EquipSlot::Pants => "Pants",
            EquipSlot::Shoes => "Shoes",
            EquipSlot::Accessory => "Accessory",
        }
    }

    /// 获取所有槽位
    pub fn all() -> [EquipSlot; 6] {
        [
            EquipSlot::Weapon,
            EquipSlot::Armor,
            EquipSlot::Helmet,
            EquipSlot::Pants,
            EquipSlot::Shoes,
            EquipSlot::Accessory,
        ]
    }
}

/// 装备栏
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquipmentSlots {
    #[serde(default)]
    slots: HashMap<EquipSlot, Item>,
}

impl EquipmentSlots {
    /// 创建空的装备栏
    pub fn new() -> Self {
        Self {
            slots: HashMap::new(),
        }
    }

    /// 穿戴装备到指定槽位
    /// 返回被替换下来的旧装备（如果有）
    pub fn equip(&mut self, slot: EquipSlot, item: Item) -> Result<Option<Item>, ItemError> {
        // 验证物品是否匹配槽位
        // 这里可以进一步验证 item.item_id 对应的装备类型是否匹配 slot
        Ok(self.slots.insert(slot, item))
    }

    /// 卸下指定槽位的装备
    pub fn unequip(&mut self, slot: EquipSlot) -> Result<Option<Item>, ItemError> {
        Ok(self.slots.remove(&slot))
    }

    /// 获取指定槽位的装备
    pub fn get(&self, slot: EquipSlot) -> Option<&Item> {
        self.slots.get(&slot)
    }

    /// 获取指定槽位的装备（可变）
    pub fn get_mut(&mut self, slot: EquipSlot) -> Option<&mut Item> {
        self.slots.get_mut(&slot)
    }

    /// 检查是否有装备在指定槽位
    pub fn has_equipment(&self, slot: EquipSlot) -> bool {
        self.slots.contains_key(&slot)
    }

    /// 获取所有已穿戴的装备
    pub fn all_equipped(&self) -> Vec<&Item> {
        self.slots.values().collect()
    }

    /// 获取已穿戴装备数量
    pub fn equipped_count(&self) -> usize {
        self.slots.len()
    }

    /// 迭代所有槽位和装备
    pub fn iter(&self) -> impl Iterator<Item = (EquipSlot, &Item)> {
        self.slots.iter().map(|(k, v)| (*k, v))
    }

    /// 清空所有装备
    pub fn clear(&mut self) {
        self.slots.clear();
    }
}

impl Default for EquipmentSlots {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_equip_unequip() {
        let mut equipment = EquipmentSlots::new();

        let sword = Item::new(1, 1001, 1, false);
        let result = equipment.equip(EquipSlot::Weapon, sword);
        assert!(result.unwrap().is_none());

        assert!(equipment.has_equipment(EquipSlot::Weapon));
        assert_eq!(equipment.equipped_count(), 1);

        let unequipped = equipment.unequip(EquipSlot::Weapon).unwrap();
        assert!(unequipped.is_some());
        assert!(!equipment.has_equipment(EquipSlot::Weapon));
    }

    #[test]
    fn test_equip_replace() {
        let mut equipment = EquipmentSlots::new();

        let sword1 = Item::new(1, 1001, 1, false);
        equipment.equip(EquipSlot::Weapon, sword1).unwrap();

        let sword2 = Item::new(2, 1001, 1, false);
        let old = equipment.equip(EquipSlot::Weapon, sword2).unwrap();

        assert!(old.is_some());
        assert_eq!(old.unwrap().uid, 1);
        assert_eq!(equipment.equipped_count(), 1);
    }
}
