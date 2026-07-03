//! Combat configuration model.
//!
//! P2 phase 1 only defines serializable configuration and validation. Runtime
//! combat resolution intentionally remains outside this module.

use crate::math::Fp;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeSet;
use std::fmt;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct SkillId(u32);

impl SkillId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct BuffId(u32);

impl BuffId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CombatConfig {
    pub skills: SkillCatalog,
    pub buffs: BuffCatalog,
}

impl CombatConfig {
    pub fn new(skills: SkillCatalog, buffs: BuffCatalog) -> Result<Self, CombatConfigError> {
        let config = Self {
            skills: skills.sorted_by_id(),
            buffs: buffs.sorted_by_id(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn from_definitions(
        skills: Vec<SkillDefinition>,
        buffs: Vec<BuffDefinition>,
    ) -> Result<Self, CombatConfigError> {
        Self::new(SkillCatalog::new(skills)?, BuffCatalog::new(buffs)?)
    }

    pub fn validate(&self) -> Result<(), CombatConfigError> {
        self.skills.validate()?;
        self.buffs.validate()?;

        let buff_ids = self
            .buffs
            .iter()
            .map(|buff| buff.id)
            .collect::<BTreeSet<_>>();

        for skill in self.skills.iter() {
            validate_effect_references(
                CombatEffectOwner::Skill(skill.id),
                &skill.effects,
                &buff_ids,
            )?;
        }

        for buff in self.buffs.iter() {
            validate_effect_references(CombatEffectOwner::Buff(buff.id), &buff.effects, &buff_ids)?;
        }

        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillCatalog {
    #[serde(deserialize_with = "deserialize_skill_definitions_sorted")]
    pub skills: Vec<SkillDefinition>,
}

impl SkillCatalog {
    pub fn new(mut skills: Vec<SkillDefinition>) -> Result<Self, CombatConfigError> {
        skills.sort_by_key(|skill| skill.id);
        let catalog = Self { skills };
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn empty() -> Self {
        Self { skills: Vec::new() }
    }

    pub fn sorted_by_id(mut self) -> Self {
        self.skills.sort_by_key(|skill| skill.id);
        self
    }

    pub fn get(&self, id: SkillId) -> Option<&SkillDefinition> {
        self.skills
            .binary_search_by_key(&id, |skill| skill.id)
            .ok()
            .map(|index| &self.skills[index])
    }

    pub fn iter(&self) -> impl Iterator<Item = &SkillDefinition> {
        self.skills.iter()
    }

    pub fn validate(&self) -> Result<(), CombatConfigError> {
        let mut ids = BTreeSet::new();

        for skill in &self.skills {
            if !ids.insert(skill.id) {
                return Err(CombatConfigError::DuplicateSkillId { id: skill.id });
            }

            skill.validate()?;
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillDefinition {
    pub id: SkillId,
    pub cooldown_frames: u32,
    pub cast_range: Fp,
    pub target_type: SkillTargetType,
    pub effects: Vec<CombatEffect>,
}

impl SkillDefinition {
    pub fn validate(&self) -> Result<(), CombatConfigError> {
        if self.cooldown_frames == 0 {
            return Err(CombatConfigError::InvalidSkillCooldown {
                skill_id: self.id,
                cooldown_frames: self.cooldown_frames,
            });
        }

        if self.cast_range < Fp::ZERO {
            return Err(CombatConfigError::InvalidSkillCastRange {
                skill_id: self.id,
                cast_range: self.cast_range,
            });
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillTargetType {
    None,
    SelfOnly,
    Ally,
    Enemy,
    AnyEntity,
    Position,
    Direction,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuffCatalog {
    #[serde(deserialize_with = "deserialize_buff_definitions_sorted")]
    pub buffs: Vec<BuffDefinition>,
}

impl BuffCatalog {
    pub fn new(mut buffs: Vec<BuffDefinition>) -> Result<Self, CombatConfigError> {
        buffs.sort_by_key(|buff| buff.id);
        let catalog = Self { buffs };
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn empty() -> Self {
        Self { buffs: Vec::new() }
    }

    pub fn sorted_by_id(mut self) -> Self {
        self.buffs.sort_by_key(|buff| buff.id);
        self
    }

    pub fn get(&self, id: BuffId) -> Option<&BuffDefinition> {
        self.buffs
            .binary_search_by_key(&id, |buff| buff.id)
            .ok()
            .map(|index| &self.buffs[index])
    }

    pub fn iter(&self) -> impl Iterator<Item = &BuffDefinition> {
        self.buffs.iter()
    }

    pub fn validate(&self) -> Result<(), CombatConfigError> {
        let mut ids = BTreeSet::new();

        for buff in &self.buffs {
            if !ids.insert(buff.id) {
                return Err(CombatConfigError::DuplicateBuffId { id: buff.id });
            }

            buff.validate()?;
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuffDefinition {
    pub id: BuffId,
    pub duration_frames: u32,
    pub interval_frames: u32,
    pub max_stacks: u16,
    pub effects: Vec<CombatEffect>,
}

impl BuffDefinition {
    pub fn validate(&self) -> Result<(), CombatConfigError> {
        if self.duration_frames == 0 {
            return Err(CombatConfigError::InvalidBuffDuration {
                buff_id: self.id,
                duration_frames: self.duration_frames,
            });
        }

        if self.interval_frames == 0 {
            return Err(CombatConfigError::InvalidBuffInterval {
                buff_id: self.id,
                interval_frames: self.interval_frames,
            });
        }

        if self.max_stacks == 0 {
            return Err(CombatConfigError::InvalidBuffMaxStacks {
                buff_id: self.id,
                max_stacks: self.max_stacks,
            });
        }

        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CombatEffect {
    Damage { formula: DamageFormula },
    Heal { formula: DamageFormula },
    AddBuff { buff_id: BuffId },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum DamageFormula {
    Fixed { amount: i32 },
    Scaling { base: i32, attack_scale_bps: i32 },
    TrueDamage { amount: i32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CombatEffectOwner {
    Skill(SkillId),
    Buff(BuffId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CombatConfigError {
    DuplicateSkillId {
        id: SkillId,
    },
    DuplicateBuffId {
        id: BuffId,
    },
    InvalidSkillCooldown {
        skill_id: SkillId,
        cooldown_frames: u32,
    },
    InvalidSkillCastRange {
        skill_id: SkillId,
        cast_range: Fp,
    },
    InvalidBuffDuration {
        buff_id: BuffId,
        duration_frames: u32,
    },
    InvalidBuffInterval {
        buff_id: BuffId,
        interval_frames: u32,
    },
    InvalidBuffMaxStacks {
        buff_id: BuffId,
        max_stacks: u16,
    },
    UnknownBuffReference {
        owner: CombatEffectOwner,
        buff_id: BuffId,
    },
}

impl fmt::Display for CombatConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateSkillId { id } => {
                write!(f, "duplicate combat skill id: {}", id.raw())
            }
            Self::DuplicateBuffId { id } => {
                write!(f, "duplicate combat buff id: {}", id.raw())
            }
            Self::InvalidSkillCooldown {
                skill_id,
                cooldown_frames,
            } => write!(
                f,
                "skill {} cooldown_frames must be greater than zero: {}",
                skill_id.raw(),
                cooldown_frames
            ),
            Self::InvalidSkillCastRange {
                skill_id,
                cast_range,
            } => write!(
                f,
                "skill {} cast_range must be greater than or equal to zero: {}",
                skill_id.raw(),
                cast_range.raw()
            ),
            Self::InvalidBuffDuration {
                buff_id,
                duration_frames,
            } => write!(
                f,
                "buff {} duration_frames must be greater than zero: {}",
                buff_id.raw(),
                duration_frames
            ),
            Self::InvalidBuffInterval {
                buff_id,
                interval_frames,
            } => write!(
                f,
                "buff {} interval_frames must be greater than zero: {}",
                buff_id.raw(),
                interval_frames
            ),
            Self::InvalidBuffMaxStacks {
                buff_id,
                max_stacks,
            } => write!(
                f,
                "buff {} max_stacks must be greater than zero: {}",
                buff_id.raw(),
                max_stacks
            ),
            Self::UnknownBuffReference { owner, buff_id } => {
                write!(f, "{} references unknown buff id: {}", owner, buff_id.raw())
            }
        }
    }
}

impl std::error::Error for CombatConfigError {}

impl fmt::Display for CombatEffectOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Skill(skill_id) => write!(f, "skill {}", skill_id.raw()),
            Self::Buff(buff_id) => write!(f, "buff {}", buff_id.raw()),
        }
    }
}

fn validate_effect_references(
    owner: CombatEffectOwner,
    effects: &[CombatEffect],
    buff_ids: &BTreeSet<BuffId>,
) -> Result<(), CombatConfigError> {
    for effect in effects {
        if let CombatEffect::AddBuff { buff_id } = effect {
            if !buff_ids.contains(buff_id) {
                return Err(CombatConfigError::UnknownBuffReference {
                    owner,
                    buff_id: *buff_id,
                });
            }
        }
    }

    Ok(())
}

fn deserialize_skill_definitions_sorted<'de, D>(
    deserializer: D,
) -> Result<Vec<SkillDefinition>, D::Error>
where
    D: Deserializer<'de>,
{
    let mut skills = Vec::<SkillDefinition>::deserialize(deserializer)?;
    skills.sort_by_key(|skill| skill.id);
    Ok(skills)
}

fn deserialize_buff_definitions_sorted<'de, D>(
    deserializer: D,
) -> Result<Vec<BuffDefinition>, D::Error>
where
    D: Deserializer<'de>,
{
    let mut buffs = Vec::<BuffDefinition>::deserialize(deserializer)?;
    buffs.sort_by_key(|buff| buff.id);
    Ok(buffs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn damage_effect(amount: i32) -> CombatEffect {
        CombatEffect::Damage {
            formula: DamageFormula::Fixed { amount },
        }
    }

    fn skill(id: u32) -> SkillDefinition {
        SkillDefinition {
            id: SkillId::new(id),
            cooldown_frames: 30,
            cast_range: Fp::from_i32(5),
            target_type: SkillTargetType::Enemy,
            effects: vec![damage_effect(10)],
        }
    }

    fn buff(id: u32) -> BuffDefinition {
        BuffDefinition {
            id: BuffId::new(id),
            duration_frames: 120,
            interval_frames: 30,
            max_stacks: 3,
            effects: vec![CombatEffect::Heal {
                formula: DamageFormula::Scaling {
                    base: 2,
                    attack_scale_bps: 100,
                },
            }],
        }
    }

    #[test]
    fn skill_catalog_sorts_by_id_and_supports_lookup() {
        let catalog = SkillCatalog::new(vec![skill(30), skill(10), skill(20)]).unwrap();

        assert_eq!(
            catalog
                .iter()
                .map(|skill| skill.id.raw())
                .collect::<Vec<_>>(),
            vec![10, 20, 30]
        );
        assert_eq!(catalog.get(SkillId::new(20)).unwrap().id, SkillId::new(20));
        assert!(catalog.get(SkillId::new(99)).is_none());
    }

    #[test]
    fn buff_catalog_sorts_by_id_and_supports_lookup() {
        let catalog = BuffCatalog::new(vec![buff(3), buff(1), buff(2)]).unwrap();

        assert_eq!(
            catalog.iter().map(|buff| buff.id.raw()).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert_eq!(catalog.get(BuffId::new(2)).unwrap().id, BuffId::new(2));
        assert!(catalog.get(BuffId::new(99)).is_none());
    }

    #[test]
    fn catalog_deserialization_sorts_definitions_for_stable_lookup() {
        let json = r#"{
            "skills": {
                "skills": [
                    {
                        "id": 30,
                        "cooldown_frames": 30,
                        "cast_range": 5000,
                        "target_type": "enemy",
                        "effects": [{ "type": "damage", "formula": { "type": "fixed", "amount": 10 } }]
                    },
                    {
                        "id": 10,
                        "cooldown_frames": 30,
                        "cast_range": 5000,
                        "target_type": "enemy",
                        "effects": [{ "type": "damage", "formula": { "type": "fixed", "amount": 10 } }]
                    }
                ]
            },
            "buffs": {
                "buffs": [
                    {
                        "id": 2,
                        "duration_frames": 120,
                        "interval_frames": 30,
                        "max_stacks": 3,
                        "effects": [{ "type": "heal", "formula": { "type": "fixed", "amount": 5 } }]
                    },
                    {
                        "id": 1,
                        "duration_frames": 120,
                        "interval_frames": 30,
                        "max_stacks": 3,
                        "effects": [{ "type": "heal", "formula": { "type": "fixed", "amount": 5 } }]
                    }
                ]
            }
        }"#;

        let config = serde_json::from_str::<CombatConfig>(json).unwrap();

        assert_eq!(
            config
                .skills
                .iter()
                .map(|skill| skill.id.raw())
                .collect::<Vec<_>>(),
            vec![10, 30]
        );
        assert_eq!(
            config
                .buffs
                .iter()
                .map(|buff| buff.id.raw())
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(config.skills.get(SkillId::new(30)).unwrap().id.raw(), 30);
        assert_eq!(config.buffs.get(BuffId::new(2)).unwrap().id.raw(), 2);
    }

    #[test]
    fn skill_catalog_rejects_duplicate_id() {
        let error = SkillCatalog::new(vec![skill(10), skill(10)]).unwrap_err();

        assert_eq!(
            error,
            CombatConfigError::DuplicateSkillId {
                id: SkillId::new(10)
            }
        );
    }

    #[test]
    fn skill_validation_rejects_negative_cast_range() {
        let mut definition = skill(10);
        definition.cast_range = Fp::from_milli(-1);

        let error = definition.validate().unwrap_err();

        assert_eq!(
            error,
            CombatConfigError::InvalidSkillCastRange {
                skill_id: SkillId::new(10),
                cast_range: Fp::from_milli(-1),
            }
        );
    }

    #[test]
    fn skill_validation_rejects_zero_cooldown() {
        let mut definition = skill(10);
        definition.cooldown_frames = 0;

        let error = definition.validate().unwrap_err();

        assert_eq!(
            error,
            CombatConfigError::InvalidSkillCooldown {
                skill_id: SkillId::new(10),
                cooldown_frames: 0,
            }
        );
    }

    #[test]
    fn buff_catalog_rejects_duplicate_id() {
        let error = BuffCatalog::new(vec![buff(7), buff(7)]).unwrap_err();

        assert_eq!(
            error,
            CombatConfigError::DuplicateBuffId { id: BuffId::new(7) }
        );
    }

    #[test]
    fn buff_validation_rejects_zero_duration_interval_and_stacks() {
        let mut zero_duration = buff(1);
        zero_duration.duration_frames = 0;
        assert_eq!(
            zero_duration.validate().unwrap_err(),
            CombatConfigError::InvalidBuffDuration {
                buff_id: BuffId::new(1),
                duration_frames: 0,
            }
        );

        let mut zero_interval = buff(2);
        zero_interval.interval_frames = 0;
        assert_eq!(
            zero_interval.validate().unwrap_err(),
            CombatConfigError::InvalidBuffInterval {
                buff_id: BuffId::new(2),
                interval_frames: 0,
            }
        );

        let mut zero_stacks = buff(3);
        zero_stacks.max_stacks = 0;
        assert_eq!(
            zero_stacks.validate().unwrap_err(),
            CombatConfigError::InvalidBuffMaxStacks {
                buff_id: BuffId::new(3),
                max_stacks: 0,
            }
        );
    }

    #[test]
    fn combat_config_rejects_skill_effect_referencing_unknown_buff() {
        let mut definition = skill(10);
        definition.effects = vec![CombatEffect::AddBuff {
            buff_id: BuffId::new(999),
        }];
        let config = CombatConfig {
            skills: SkillCatalog::new(vec![definition]).unwrap(),
            buffs: BuffCatalog::new(vec![buff(1)]).unwrap(),
        };

        let error = config.validate().unwrap_err();

        assert_eq!(
            error,
            CombatConfigError::UnknownBuffReference {
                owner: CombatEffectOwner::Skill(SkillId::new(10)),
                buff_id: BuffId::new(999),
            }
        );
    }

    #[test]
    fn combat_config_rejects_buff_effect_referencing_unknown_buff() {
        let mut definition = buff(1);
        definition.effects = vec![CombatEffect::AddBuff {
            buff_id: BuffId::new(999),
        }];
        let config = CombatConfig {
            skills: SkillCatalog::empty(),
            buffs: BuffCatalog::new(vec![definition]).unwrap(),
        };

        let error = config.validate().unwrap_err();

        assert_eq!(
            error,
            CombatConfigError::UnknownBuffReference {
                owner: CombatEffectOwner::Buff(BuffId::new(1)),
                buff_id: BuffId::new(999),
            }
        );
    }

    #[test]
    fn combat_config_accepts_damage_heal_add_buff_and_true_damage_formulas() {
        let burn = BuffDefinition {
            id: BuffId::new(100),
            duration_frames: 180,
            interval_frames: 30,
            max_stacks: 5,
            effects: vec![CombatEffect::Damage {
                formula: DamageFormula::TrueDamage { amount: 3 },
            }],
        };
        let fireball = SkillDefinition {
            id: SkillId::new(200),
            cooldown_frames: 60,
            cast_range: Fp::from_i32(8),
            target_type: SkillTargetType::Enemy,
            effects: vec![
                CombatEffect::Damage {
                    formula: DamageFormula::Fixed { amount: 25 },
                },
                CombatEffect::Heal {
                    formula: DamageFormula::Scaling {
                        base: 5,
                        attack_scale_bps: 250,
                    },
                },
                CombatEffect::AddBuff {
                    buff_id: BuffId::new(100),
                },
            ],
        };

        let config = CombatConfig::from_definitions(vec![fireball], vec![burn]).unwrap();

        assert!(config.validate().is_ok());
        assert_eq!(
            config.skills.get(SkillId::new(200)).unwrap().effects.len(),
            3
        );
    }

    #[test]
    fn serde_rejects_unknown_effect_type() {
        let json = r#"{"type":"teleport","distance":1000}"#;

        let error = serde_json::from_str::<CombatEffect>(json).unwrap_err();

        assert!(error.to_string().contains("unknown variant"));
    }
}
