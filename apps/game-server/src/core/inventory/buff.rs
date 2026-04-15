use serde::{Deserialize, Serialize};

use super::attr::AttrPanel;

/// Buff 定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Buff {
    /// Buff ID
    pub id: u32,
    /// Buff 名称
    pub name: String,
    /// 持续时间（毫秒）
    pub duration_ms: u64,
    /// 属性加成
    pub attr_bonus: AttrPanel,
    /// 视觉特效 ID
    pub visual_effect: Option<u32>,
}

impl Buff {
    pub fn new(id: u32, name: String, duration_ms: u64) -> Self {
        Self {
            id,
            name,
            duration_ms,
            attr_bonus: AttrPanel::default(),
            visual_effect: None,
        }
    }

    pub fn with_attr_bonus(mut self, attr_bonus: AttrPanel) -> Self {
        self.attr_bonus = attr_bonus;
        self
    }

    pub fn with_visual_effect(mut self, visual_effect: u32) -> Self {
        self.visual_effect = Some(visual_effect);
        self
    }
}

/// Buff 叠加规则
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuffStackRule {
    /// 不可叠加，新buff替换旧buff
    Replace,
    /// 可叠加，效果叠加
    Additive,
    /// 可叠加，但有最大层数限制
    Limited(u32),
}

impl BuffStackRule {
    pub fn max_stacks(&self) -> u32 {
        match self {
            BuffStackRule::Replace => 1,
            BuffStackRule::Additive => u32::MAX,
            BuffStackRule::Limited(max) => *max,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buff_creation() {
        let buff = Buff::new(1, "TestBuff".to_string(), 5000)
            .with_attr_bonus(super::super::attr::AttrPanel {
                attack: 100,
                ..Default::default()
            })
            .with_visual_effect(123);

        assert_eq!(buff.id, 1);
        assert_eq!(buff.name, "TestBuff");
        assert_eq!(buff.duration_ms, 5000);
        assert_eq!(buff.attr_bonus.attack, 100);
        assert_eq!(buff.visual_effect, Some(123));
    }
}
