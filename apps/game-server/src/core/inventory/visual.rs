use serde::{Deserialize, Serialize};

/// 外观
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerVisual {
    /// 当前外观 ID（通常由装备决定）
    pub appearance: u32,
    /// 激活的 Buff 列表
    pub active_buffs: Vec<u32>,
}

impl PlayerVisual {
    pub fn new(appearance: u32) -> Self {
        Self {
            appearance,
            active_buffs: Vec::new(),
        }
    }

    /// 添加一个 Buff
    pub fn add_buff(&mut self, buff_id: u32) {
        if !self.active_buffs.contains(&buff_id) {
            self.active_buffs.push(buff_id);
        }
    }

    /// 移除一个 Buff
    pub fn remove_buff(&mut self, buff_id: u32) {
        self.active_buffs.retain(|&id| id != buff_id);
    }

    /// 检查是否有某个 Buff
    pub fn has_buff(&self, buff_id: u32) -> bool {
        self.active_buffs.contains(&buff_id)
    }

    /// 清空所有 Buff
    pub fn clear_buffs(&mut self) {
        self.active_buffs.clear();
    }

    /// 设置外观 ID
    pub fn set_appearance(&mut self, appearance: u32) {
        self.appearance = appearance;
    }
}

impl Default for PlayerVisual {
    fn default() -> Self {
        Self {
            appearance: 0,
            active_buffs: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_visual_buffs() {
        let mut visual = PlayerVisual::new(1001);

        visual.add_buff(1);
        visual.add_buff(2);
        visual.add_buff(1); // 重复添加

        assert_eq!(visual.active_buffs.len(), 2);
        assert!(visual.has_buff(1));
        assert!(visual.has_buff(2));
        assert!(!visual.has_buff(3));

        visual.remove_buff(1);
        assert!(!visual.has_buff(1));
        assert_eq!(visual.active_buffs.len(), 1);
    }
}
