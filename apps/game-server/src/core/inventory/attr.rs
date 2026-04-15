use serde::{Deserialize, Serialize};

use super::buff::Buff;
use super::equipment::EquipmentSlots;
use crate::csv_code::itemtable::ItemTable;

/// 属性来源
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AttrSource {
    /// 基础属性（升级/转职）
    Base,
    /// 装备（按实例ID）
    Equipment(u64),
    /// Buff
    Buff(u32),
    /// 技能
    Skill(u32),
    /// 临时消耗品
    Food,
}

impl AttrSource {
    pub fn as_str(&self) -> String {
        match self {
            AttrSource::Base => "Base".to_string(),
            AttrSource::Equipment(uid) => format!("Equipment:{}", uid),
            AttrSource::Buff(id) => format!("Buff:{}", id),
            AttrSource::Skill(id) => format!("Skill:{}", id),
            AttrSource::Food => "Food".to_string(),
        }
    }
}

/// 属性类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttrType {
    Hp,
    MaxHp,
    Attack,
    Defense,
    Speed,
    CritRate,
    CritDmg,
}

impl AttrType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "Hp" | "HP" | "hp" => Some(AttrType::Hp),
            "MaxHp" | "MAX_HP" | "max_hp" => Some(AttrType::MaxHp),
            "Attack" | "ATTACK" | "attack" => Some(AttrType::Attack),
            "Defense" | "DEFENSE" | "defense" | "Def" => Some(AttrType::Defense),
            "Speed" | "SPEED" | "speed" => Some(AttrType::Speed),
            "CritRate" | "CRIT_RATE" | "crit_rate" => Some(AttrType::CritRate),
            "CritDmg" | "CRIT_DMG" | "crit_dmg" => Some(AttrType::CritDmg),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            AttrType::Hp => "Hp",
            AttrType::MaxHp => "MaxHp",
            AttrType::Attack => "Attack",
            AttrType::Defense => "Defense",
            AttrType::Speed => "Speed",
            AttrType::CritRate => "CritRate",
            AttrType::CritDmg => "CritDmg",
        }
    }
}

/// 属性面板（战斗用）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AttrPanel {
    pub hp: i64,
    pub max_hp: i64,
    pub attack: i64,
    pub defense: i64,
    pub speed: i32,
    pub crit_rate: f32,
    pub crit_dmg: f32,
}

impl AttrPanel {
    /// 添加属性
    pub fn add(&mut self, attr_type: AttrType, value: i32) {
        match attr_type {
            AttrType::Hp => self.hp += value as i64,
            AttrType::MaxHp => self.max_hp += value as i64,
            AttrType::Attack => self.attack += value as i64,
            AttrType::Defense => self.defense += value as i64,
            AttrType::Speed => self.speed += value as i32,
            AttrType::CritRate => self.crit_rate += value as f32,
            AttrType::CritDmg => self.crit_dmg += value as f32,
        }
    }

    /// 从 ItemTableRow 添加装备属性
    pub fn add_from_item_row(&mut self, row: &crate::csv_code::itemtable::ItemTableRow) {
        if row.attack != 0 {
            self.attack += row.attack as i64;
        }
        if row.defense != 0 {
            self.defense += row.defense as i64;
        }
        if row.maxhp != 0 {
            self.max_hp += row.maxhp as i64;
        }
        if row.critrate != 0.0 {
            self.crit_rate += row.critrate;
        }
        if row.movespeed != 0.0 {
            self.speed += row.movespeed as i32;
        }
    }

    /// 克隆并加上另一个面板
    pub fn add_panel(&mut self, other: &AttrPanel) {
        self.hp += other.hp;
        self.max_hp += other.max_hp;
        self.attack += other.attack;
        self.defense += other.defense;
        self.speed += other.speed;
        self.crit_rate += other.crit_rate;
        self.crit_dmg += other.crit_dmg;
    }
}

/// 单条属性记录（用于面板展示）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttrRecord {
    pub source: AttrSource,
    pub attr_type: AttrType,
    pub value: i32,
}

impl AttrRecord {
    pub fn new(source: AttrSource, attr_type: AttrType, value: i32) -> Self {
        Self {
            source,
            attr_type,
            value,
        }
    }
}

/// 完整属性（包含来源记录）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerAttr {
    /// 基础属性（升级点满）
    pub base: AttrPanel,
    /// 所有附加（用于面板展示）
    pub bonus: Vec<AttrRecord>,
    /// 最终属性（战斗用）
    pub final_: AttrPanel,
}

impl PlayerAttr {
    pub fn new() -> Self {
        Self {
            base: AttrPanel::default(),
            bonus: Vec::new(),
            final_: AttrPanel::default(),
        }
    }

    /// 设置基础属性
    pub fn set_base(&mut self, base: AttrPanel) {
        self.base = base;
    }

    /// 重算属性
    pub fn recalculate(
        &mut self,
        equipment: &EquipmentSlots,
        item_table: &ItemTable,
        buffs: &[Buff],
    ) {
        let mut all_bonus: Vec<AttrRecord> = Vec::new();

        // 1. 收集装备附加
        for (slot, item) in equipment.iter() {
            let _ = slot; // 未使用警告
            if let Some(row) = item_table.get(item.item_id) {
                let source = AttrSource::Equipment(item.uid);

                if row.attack != 0 {
                    all_bonus.push(AttrRecord::new(source.clone(), AttrType::Attack, row.attack));
                }
                if row.defense != 0 {
                    all_bonus.push(AttrRecord::new(source.clone(), AttrType::Defense, row.defense));
                }
                if row.maxhp != 0 {
                    all_bonus.push(AttrRecord::new(source.clone(), AttrType::MaxHp, row.maxhp));
                }
                if row.critrate != 0.0 {
                    all_bonus.push(AttrRecord::new(
                        source.clone(),
                        AttrType::CritRate,
                        (row.critrate * 10000.0) as i32, // 百分比转基点
                    ));
                }
                if row.movespeed != 0.0 {
                    all_bonus.push(AttrRecord::new(
                        source.clone(),
                        AttrType::Speed,
                        (row.movespeed * 100.0) as i32,
                    ));
                }
            }
        }

        // 2. 收集 Buff 附加
        for buff in buffs {
            let source = AttrSource::Buff(buff.id);
            if buff.attr_bonus.attack != 0 {
                all_bonus.push(AttrRecord::new(
                    source.clone(),
                    AttrType::Attack,
                    buff.attr_bonus.attack as i32,
                ));
            }
            if buff.attr_bonus.defense != 0 {
                all_bonus.push(AttrRecord::new(
                    source.clone(),
                    AttrType::Defense,
                    buff.attr_bonus.defense as i32,
                ));
            }
            if buff.attr_bonus.max_hp != 0 {
                all_bonus.push(AttrRecord::new(
                    source.clone(),
                    AttrType::MaxHp,
                    buff.attr_bonus.max_hp as i32,
                ));
            }
        }

        // 3. 聚合到 final
        let mut final_panel = self.base.clone();
        for record in &all_bonus {
            final_panel.add(record.attr_type, record.value);
        }

        // 4. 更新
        self.bonus = all_bonus;
        self.final_ = final_panel;
    }
}

impl Default for PlayerAttr {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attr_panel_add() {
        let mut panel = AttrPanel::default();
        panel.add(AttrType::Attack, 100);
        panel.add(AttrType::MaxHp, 500);

        assert_eq!(panel.attack, 100);
        assert_eq!(panel.max_hp, 500);
    }

    #[test]
    fn test_player_attr() {
        let mut attr = PlayerAttr::new();
        attr.base.max_hp = 1000;
        attr.base.attack = 50;

        let mut equipment = EquipmentSlots::new();
        equipment
            .equip(
                EquipSlot::Weapon,
                Item::new(1, 1001, 1, false),
            )
            .unwrap();

        // 假设 ItemTable 有对应的物品定义
        // 这里简化测试
    }
}
