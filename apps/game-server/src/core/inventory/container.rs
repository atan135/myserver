use std::slice::Iter;

use serde::{Deserialize, Serialize};

use super::item::{Item, ItemError};

/// 物品容器（背包或仓库）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemContainer {
    slots: Vec<Option<Item>>,
    #[serde(skip)]
    capacity: usize,
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

    /// 添加物品到容器
    /// 如果物品可堆叠且有足够空间，会合并到已有物品；否则放入空格子
    pub fn add_item(&mut self, item: Item) -> Result<(), ItemError> {
        // 先尝试堆叠
        if item.count > 1 {
            if let Some(existing) = self.find_item_mut(item.uid) {
                existing.count += item.count;
                return Ok(());
            }
            // 找相同 item_id 的物品堆叠
            for slot in &mut self.slots {
                if let Some(existing) = slot {
                    if existing.item_id == item.item_id && existing.binded == item.binded {
                        // 可以堆叠
                        existing.count += item.count;
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
        let idx = self
            .find_item_index(uid)
            .ok_or(ItemError::ItemNotFound)?;

        let slot = self.slots.get_mut(idx).unwrap();
        let item = slot.as_mut().ok_or(ItemError::ItemNotFound)?;

        if item.count < count {
            return Err(ItemError::NotEnoughCount);
        }

        if item.count == count {
            let item = slot.take().unwrap();
            Ok(item)
        } else {
            item.count -= count;
            Ok(Item {
                uid,
                item_id: item.item_id,
                count,
                binded: item.binded,
            })
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
        self.slots
            .iter()
            .filter_map(|s| s.as_ref())
            .collect()
    }

    /// 清空容器
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
    }
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
}
