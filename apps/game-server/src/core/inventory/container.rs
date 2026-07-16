use std::slice::Iter;

use serde::{Deserialize, Deserializer, Serialize};

use super::item::{Item, ItemError};
use crate::csv_code::itemtable::ItemTable;

/// A capacity-checked addition prepared against an immutable container snapshot.
///
/// `generated_uid_slots` are the extra stacks created when one incoming item crosses
/// `ItemTable.MaxStack`. The caller allocates those UIDs only after capacity has been proven.
#[derive(Debug, Clone)]
pub struct ItemContainerAdditionPlan {
    slots: Vec<Option<Item>>,
    generated_uid_slots: Vec<usize>,
}

/// 物品容器（背包或仓库）
#[derive(Debug, Clone, Serialize)]
pub struct ItemContainer {
    slots: Vec<Option<Item>>,
    #[serde(skip)]
    capacity: usize,
}

impl<'de> Deserialize<'de> for ItemContainer {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireContainer {
            slots: Vec<Option<Item>>,
        }

        let wire = WireContainer::deserialize(deserializer)?;
        // Historical JSONB intentionally omitted capacity. Slots are the authoritative fixed-grid
        // shape, so deriving it here preserves old snapshots instead of loading a zero-capacity bag.
        Ok(Self {
            capacity: wire.slots.len(),
            slots: wire.slots,
        })
    }
}

impl ItemContainer {
    /// 创建指定容量的容器
    pub fn new(capacity: usize) -> Self {
        Self {
            slots: vec![None; capacity],
            capacity,
        }
    }

    /// 获取容量
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 获取当前物品数量（非空格子数）
    pub fn item_count(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// 获取指定索引的格子
    pub fn get_slot(&self, index: usize) -> Option<&Option<Item>> {
        self.slots.get(index)
    }

    /// 获取可变格子
    pub fn get_slot_mut(&mut self, index: usize) -> Option<&mut Option<Item>> {
        self.slots.get_mut(index)
    }

    /// 通过 uid 查找物品所在的格子索引
    pub fn find_item_index(&self, uid: u64) -> Option<usize> {
        self.slots
            .iter()
            .enumerate()
            .find(|(_, slot)| slot.as_ref().map(|i| i.uid == uid).unwrap_or(false))
            .map(|(idx, _)| idx)
    }

    /// 通过 uid 查找物品引用
    pub fn find_item(&self, uid: u64) -> Option<&Item> {
        self.slots
            .iter()
            .find(|slot| slot.as_ref().map(|i| i.uid == uid).unwrap_or(false))
            .and_then(|slot| slot.as_ref())
    }

    /// 通过 uid 查找可变物品引用
    pub fn find_item_mut(&mut self, uid: u64) -> Option<&mut Item> {
        for slot in &mut self.slots {
            if let Some(item) = slot {
                if item.uid == uid {
                    return Some(item);
                }
            }
        }
        None
    }

    /// Plans a full batch without changing this container. It validates all source items before
    /// returning and therefore a capacity error cannot partially merge an earlier item.
    pub fn plan_add_items(
        &self,
        items: &[Item],
        item_table: &ItemTable,
    ) -> Result<ItemContainerAdditionPlan, ItemError> {
        let mut slots = self.slots.clone();
        let mut known_uids = std::collections::HashSet::new();
        for item in slots.iter().flatten() {
            if item.uid == 0 || !known_uids.insert(item.uid) {
                return Err(ItemError::DuplicateItemUid);
            }
            validate_item_for_container(item, item_table)?;
        }

        let mut generated_uid_slots = Vec::new();
        for incoming in items {
            validate_item_for_container(incoming, item_table)?;
            if !known_uids.insert(incoming.uid) {
                return Err(ItemError::DuplicateItemUid);
            }

            let max_stack = item_table
                .get(incoming.item_id)
                .and_then(|row| u32::try_from(row.maxstack).ok())
                .filter(|max_stack| *max_stack > 0)
                .ok_or(ItemError::InvalidItemConfig)?;
            let mut remaining = incoming.count;

            for existing in slots.iter_mut().flatten() {
                if !existing.can_stack_with(incoming) {
                    continue;
                }
                if existing.count > max_stack {
                    return Err(ItemError::StackOverflow);
                }
                let available = max_stack - existing.count;
                let merged = available.min(remaining);
                existing.count = existing
                    .count
                    .checked_add(merged)
                    .ok_or(ItemError::StackOverflow)?;
                remaining -= merged;
                if remaining == 0 {
                    break;
                }
            }

            let mut uses_input_uid = true;
            while remaining > 0 {
                let slot_index = slots
                    .iter()
                    .position(Option::is_none)
                    .ok_or(ItemError::InventoryFull)?;
                let count = remaining.min(max_stack);
                let mut stack = incoming.clone();
                stack.count = count;
                if !uses_input_uid {
                    // The plan cannot mutate a global UID generator. The caller fills these
                    // placeholders after the complete capacity preflight succeeds.
                    stack.uid = 0;
                    generated_uid_slots.push(slot_index);
                }
                slots[slot_index] = Some(stack);
                remaining -= count;
                uses_input_uid = false;
            }
        }

        Ok(ItemContainerAdditionPlan {
            slots,
            generated_uid_slots,
        })
    }

    /// Applies a previously successful plan. UID generation failure leaves this container intact.
    pub fn apply_addition_plan<F>(
        &mut self,
        mut plan: ItemContainerAdditionPlan,
        mut next_uid: F,
    ) -> Result<(), ItemError>
    where
        F: FnMut() -> Result<u64, ItemError>,
    {
        let mut known_uids = plan
            .slots
            .iter()
            .flatten()
            .filter_map(|item| (item.uid != 0).then_some(item.uid))
            .collect::<std::collections::HashSet<_>>();
        for index in plan.generated_uid_slots {
            let uid = next_uid()?;
            if uid == 0 || !known_uids.insert(uid) {
                return Err(ItemError::DuplicateItemUid);
            }
            plan.slots[index].as_mut().ok_or(ItemError::Unknown)?.uid = uid;
        }
        *self = Self {
            capacity: plan.slots.len(),
            slots: plan.slots,
        };
        Ok(())
    }

    pub fn add_item_with_table<F>(
        &mut self,
        item: Item,
        item_table: &ItemTable,
        next_uid: F,
    ) -> Result<(), ItemError>
    where
        F: FnMut() -> Result<u64, ItemError>,
    {
        let plan = self.plan_add_items(&[item], item_table)?;
        self.apply_addition_plan(plan, next_uid)
    }

    /// Legacy non-transactional API kept until phase 7 migrates all old callers. New asset
    /// mutations must use `plan_add_items` / `apply_addition_plan` with `ItemTable`.
    pub fn add_item(&mut self, item: Item) -> Result<(), ItemError> {
        // 先尝试堆叠
        if item.count > 1 {
            if let Some(existing) = self.find_item_mut(item.uid) {
                existing.count = existing
                    .count
                    .checked_add(item.count)
                    .ok_or(ItemError::StackOverflow)?;
                return Ok(());
            }
            // 找相同 item_id 的物品堆叠
            for slot in &mut self.slots {
                if let Some(existing) = slot {
                    if existing.can_stack_with(&item) {
                        // 可以堆叠
                        existing.count = existing
                            .count
                            .checked_add(item.count)
                            .ok_or(ItemError::StackOverflow)?;
                        return Ok(());
                    }
                }
            }
        }

        // 找空格子
        let empty_idx = self
            .slots
            .iter()
            .position(|slot| slot.is_none())
            .ok_or(ItemError::InventoryFull)?;

        self.slots[empty_idx] = Some(item);
        Ok(())
    }

    /// 从容器中移除物品
    pub fn remove_item(&mut self, uid: u64, count: u32) -> Result<Item, ItemError> {
        if count == 0 {
            return Err(ItemError::InvalidItemCount);
        }
        let idx = self.find_item_index(uid).ok_or(ItemError::ItemNotFound)?;

        let slot = self.slots.get_mut(idx).unwrap();
        let item = slot.as_mut().ok_or(ItemError::ItemNotFound)?;

        if item.is_frozen() {
            return Err(ItemError::AssetFrozen);
        }

        if item.count < count {
            return Err(ItemError::NotEnoughCount);
        }

        if item.count == count {
            let item = slot.take().unwrap();
            Ok(item)
        } else {
            item.count -= count;
            let mut removed = item.clone();
            removed.uid = uid;
            removed.count = count;
            Ok(removed)
        }
    }

    /// 检查容器是否为空
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(|s| s.is_none())
    }

    /// 检查容器是否已满
    pub fn is_full(&self) -> bool {
        self.slots.iter().all(|s| s.is_some())
    }

    /// 获取所有物品（迭代器）
    pub fn items(&self) -> ItemContainerIter {
        ItemContainerIter(self.slots.iter())
    }

    /// 获取所有非空物品
    pub fn non_empty_items(&self) -> Vec<&Item> {
        self.slots.iter().filter_map(|s| s.as_ref()).collect()
    }

    /// 清空容器
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
    }
}

fn validate_item_for_container(item: &Item, item_table: &ItemTable) -> Result<(), ItemError> {
    if item.uid == 0 {
        return Err(ItemError::DuplicateItemUid);
    }
    if item.count == 0 {
        return Err(ItemError::InvalidItemCount);
    }
    if item.binded != item.bound_character_id.is_some()
        || item
            .bound_character_id
            .as_deref()
            .is_some_and(|character_id| character_id.trim().is_empty())
    {
        return Err(ItemError::InvalidBinding);
    }
    let row = item_table
        .get(item.item_id)
        .ok_or(ItemError::InvalidItemConfig)?;
    super::item::validate_item_table_row(row, item_table)?;
    Ok(())
}

pub struct ItemContainerIter<'a>(Iter<'a, Option<Item>>);

impl<'a> Iterator for ItemContainerIter<'a> {
    type Item = &'a Option<Item>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::csv_code::itemtable::ItemTableRow;
    use std::collections::HashMap;

    fn item_table(entries: &[(i32, i32)]) -> ItemTable {
        let mut strings = HashMap::new();
        strings.insert(1, "Material".to_string());
        strings.insert(2, "None".to_string());
        strings.insert(3, "Never".to_string());
        let rows = entries
            .iter()
            .map(|(id, maxstack)| ItemTableRow {
                id: *id,
                type_: 1,
                maxstack: *maxstack,
                equipslot: 2,
                bindtype: 3,
                useeffect: 2,
                usetarget: 2,
                ..ItemTableRow::default()
            })
            .collect::<Vec<_>>();
        let by_id = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id, index))
            .collect();
        ItemTable {
            string_pool: strings,
            rows,
            by_id,
        }
    }

    #[test]
    fn test_container_add_remove() {
        let mut container = ItemContainer::new(4);

        let item1 = Item::new(1, 1001, 1, false);
        container.add_item(item1).unwrap();

        let item2 = Item::new(2, 1002, 5, false);
        container.add_item(item2).unwrap();

        assert_eq!(container.item_count(), 2);

        let removed = container.remove_item(1, 1).unwrap();
        assert_eq!(removed.uid, 1);
        assert_eq!(container.item_count(), 1);

        let found = container.find_item(2).unwrap();
        assert_eq!(found.count, 5);
    }

    #[test]
    fn test_container_full() {
        let mut container = ItemContainer::new(2);

        container.add_item(Item::new(1, 1001, 1, false)).unwrap();
        container.add_item(Item::new(2, 1002, 1, false)).unwrap();

        let result = container.add_item(Item::new(3, 1003, 1, false));
        assert_eq!(result, Err(ItemError::InventoryFull));
    }

    #[test]
    fn stacking_rejects_count_overflow_without_mutating_existing_item() {
        let mut container = ItemContainer::new(2);
        container
            .add_item(Item::new(1, 1001, u32::MAX, false))
            .unwrap();

        assert_eq!(
            container.add_item(Item::new(2, 1001, 2, false)),
            Err(ItemError::StackOverflow)
        );
        assert_eq!(container.find_item(1).unwrap().count, u32::MAX);
    }

    #[test]
    fn test_container_does_not_stack_items_with_different_growth_elements() {
        let mut container = ItemContainer::new(4);
        let first = Item::new(1, 1001, 2, false);
        let mut second = Item::new(2, 1001, 3, false);
        second.growth_elements = super::super::item::ItemElementValues::new(0, 1, 0, 0);

        container.add_item(first).unwrap();
        container.add_item(second).unwrap();

        assert_eq!(container.item_count(), 2);
        assert_eq!(container.find_item(1).unwrap().count, 2);
        assert_eq!(container.find_item(2).unwrap().count, 3);
    }

    #[test]
    fn plan_splits_large_grant_after_full_capacity_preflight() {
        let table = item_table(&[(1001, 3)]);
        let mut container = ItemContainer::new(3);
        let plan = container
            .plan_add_items(&[Item::new(10, 1001, 8, false)], &table)
            .unwrap();

        container
            .apply_addition_plan(plan, {
                let mut next = 20;
                move || {
                    let value = next;
                    next += 1;
                    Ok(value)
                }
            })
            .unwrap();

        let items = container.non_empty_items();
        assert_eq!(
            items.iter().map(|item| item.count).collect::<Vec<_>>(),
            vec![3, 3, 2]
        );
        assert_eq!(
            items.iter().map(|item| item.uid).collect::<Vec<_>>(),
            vec![10, 20, 21]
        );
    }

    #[test]
    fn capacity_plan_failure_leaves_source_container_unchanged() {
        let table = item_table(&[(1001, 1), (1002, 1)]);
        let mut container = ItemContainer::new(1);
        container.add_item(Item::new(1, 1001, 1, false)).unwrap();

        assert_eq!(
            container
                .plan_add_items(&[Item::new(2, 1002, 1, false)], &table)
                .unwrap_err(),
            ItemError::InventoryFull
        );
        assert_eq!(container.find_item(1).unwrap().count, 1);
        assert!(container.find_item(2).is_none());
    }

    #[test]
    fn frozen_item_cannot_be_removed_or_repeatedly_frozen() {
        let mut container = ItemContainer::new(1);
        let mut item = Item::new(1, 1001, 1, false);
        item.freeze("trade").unwrap();
        assert_eq!(item.freeze("again"), Err(ItemError::AssetFrozen));
        container.add_item(item).unwrap();

        assert_eq!(
            container.remove_item(1, 1).unwrap_err(),
            ItemError::AssetFrozen
        );
        assert!(container.find_item(1).is_some());
    }
}
