use crate::core::config_table::CsvLoadError;

use serde::{Deserialize, Serialize};

pub type EntityId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum DamageFormula {
    Fixed(i32),
    Scaling {
        base: i32,
        attack_scale_bps: u16,
    },
    TrueDamage(i32),
}

impl DamageFormula {
    pub fn parse_script(kind: &str, value: i32, context: &str) -> Result<Self, CsvLoadError> {
        match kind {
            "Fixed" => Ok(Self::Fixed(value)),
            "True" => Ok(Self::TrueDamage(value)),
            _ => {
                if let Some(raw_scale) = kind.strip_prefix("Scaling:") {
                    let attack_scale_bps = raw_scale.parse::<u16>().map_err(|error| {
                        CsvLoadError::Parse(format!(
                            "{context}: invalid scaling formula `{kind}`: {error}"
                        ))
                    })?;
                    Ok(Self::Scaling {
                        base: value,
                        attack_scale_bps,
                    })
                } else {
                    Err(CsvLoadError::Parse(format!(
                        "{context}: unsupported formula kind `{kind}`"
                    )))
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

impl Default for Position {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0 }
    }
}

impl Position {
    pub fn distance_squared(self, other: Self) -> f32 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        dx * dx + dy * dy
    }

    pub fn distance(self, other: Self) -> f32 {
        self.distance_squared(other).sqrt()
    }

    pub fn direction_to(self, other: Self) -> Self {
        let dx = other.x - self.x;
        let dy = other.y - self.y;
        let len_sq = dx * dx + dy * dy;
        if len_sq <= f32::EPSILON {
            return Self { x: 1.0, y: 0.0 };
        }

        let inv_len = len_sq.sqrt().recip();
        Self {
            x: dx * inv_len,
            y: dy * inv_len,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityType {
    Player,
    Npc,
    Monster,
    Projectile,
    Summon,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityMeta {
    pub entity_id: EntityId,
    pub entity_type: EntityType,
    pub player_id: Option<String>,
    pub team_id: u16,
    pub alive: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MoveStateType {
    Idle,
    Sliding,
    Knockback,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MoveState {
    pub state_type: MoveStateType,
    pub start_x: f32,
    pub start_y: f32,
    pub target_x: f32,
    pub target_y: f32,
    pub progress: f32,
    pub speed: f32,
}

impl Default for MoveState {
    fn default() -> Self {
        Self::idle()
    }
}

impl MoveState {
    pub fn idle() -> Self {
        Self {
            state_type: MoveStateType::Idle,
            start_x: 0.0,
            start_y: 0.0,
            target_x: 0.0,
            target_y: 0.0,
            progress: 1.0,
            speed: 0.0,
        }
    }

    pub fn is_active(&self) -> bool {
        self.state_type != MoveStateType::Idle && self.progress < 1.0
    }

    pub fn current_position(&self) -> Position {
        let progress = self.progress.clamp(0.0, 1.0);
        Position {
            x: self.start_x + (self.target_x - self.start_x) * progress,
            y: self.start_y + (self.target_y - self.start_y) * progress,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Health {
    pub current: i32,
    pub max: i32,
    pub base_max: i32,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            current: 0,
            max: 0,
            base_max: 0,
        }
    }
}

impl Health {
    pub fn new(max: i32) -> Self {
        Self {
            current: max.max(0),
            max: max.max(0),
            base_max: max.max(0),
        }
    }

    pub fn is_alive(&self) -> bool {
        self.current > 0
    }

    pub fn take_damage(&mut self, amount: i32) -> i32 {
        let applied = amount.max(0).min(self.current.max(0));
        self.current -= applied;
        applied
    }

    pub fn heal(&mut self, amount: i32) -> i32 {
        let missing = (self.max - self.current).max(0);
        let applied = amount.max(0).min(missing);
        self.current += applied;
        applied
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stats {
    pub attack: i32,
    pub defense: i32,
    pub speed: i32,
    pub crit_rate_bps: u16,
    pub crit_damage_bps: u16,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            attack: 0,
            defense: 0,
            speed: 0,
            crit_rate_bps: 0,
            crit_damage_bps: 5_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSlot {
    pub skill_id: u16,
    pub cooldown_remaining: u16,
}

impl SkillSlot {
    pub const fn empty() -> Self {
        Self {
            skill_id: 0,
            cooldown_remaining: 0,
        }
    }

    pub fn tick(&mut self) {
        if self.cooldown_remaining > 0 {
            self.cooldown_remaining -= 1;
        }
    }
}

impl Default for SkillSlot {
    fn default() -> Self {
        Self::empty()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuffSlot {
    pub buff_id: u16,
    pub duration_remaining: u16,
    pub interval_remaining: u16,
    pub stacks: u8,
    pub source_entity: EntityId,
}

impl BuffSlot {
    pub const fn empty() -> Self {
        Self {
            buff_id: 0,
            duration_remaining: 0,
            interval_remaining: 0,
            stacks: 0,
            source_entity: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.buff_id == 0 || self.duration_remaining == 0 || self.stacks == 0
    }

    pub fn clear(&mut self) {
        *self = Self::empty();
    }
}

impl Default for BuffSlot {
    fn default() -> Self {
        Self::empty()
    }
}
