use std::collections::HashMap;
use std::sync::Arc;
#[cfg(test)]
use std::path::Path;

use crate::core::config_table::CsvLoadError;
use crate::csv_code::bufferbase::BufferBase;
use crate::csv_code::skillbase::SkillBase;
use crate::gameconfig::ConfigTables;

use super::buffs::{BuffDefinition, BuffType, parse_buff_effects};
use super::components::DamageFormula;
use super::skills::{SkillDefinition, SkillTargetType, parse_skill_effects};

pub trait CombatCatalog: Send + Sync {
    fn skill_definition(&self, skill_id: u16) -> Option<&SkillDefinition>;
    fn buff_definition(&self, buff_id: u16) -> Option<&BuffDefinition>;
}

pub type SharedCombatCatalog = Arc<dyn CombatCatalog>;

#[derive(Debug, Clone, Default)]
pub struct CsvCombatCatalog {
    skills: HashMap<u16, SkillDefinition>,
    buffs: HashMap<u16, BuffDefinition>,
}

impl CsvCombatCatalog {
    pub fn from_tables(tables: &ConfigTables) -> Result<Self, CsvLoadError> {
        let mut skills = HashMap::with_capacity(tables.skillbase.rows.len());
        let mut buffs = HashMap::with_capacity(tables.bufferbase.rows.len());

        for row in tables.skillbase.all() {
            let id = cast_u16(row.id, "SkillBase", "Id")?;
            let context = format!("SkillBase id={id}");
            let code = resolve_skill_string(&tables.skillbase, row.code, &context, "Code")?;
            let name = resolve_skill_string(&tables.skillbase, row.name, &context, "Name")?;
            let description = resolve_skill_string(
                &tables.skillbase,
                row.description,
                &context,
                "Description",
            )?;
            let target_type = SkillTargetType::parse(
                resolve_skill_string(&tables.skillbase, row.targettype, &context, "TargetType")?
                    .as_str(),
                &context,
            )?;
            let effects = parse_skill_effects(
                resolve_skill_string(&tables.skillbase, row.effectscript, &context, "EffectScript")?
                    .as_str(),
                &context,
            )?;

            let definition = SkillDefinition {
                id,
                code,
                name,
                description,
                cooldown_frames: cast_u16(row.cooldownframes, "SkillBase", "CooldownFrames")?,
                cast_frames: cast_u16(row.castframes, "SkillBase", "CastFrames")?,
                range: row.range,
                target_type,
                effects,
            };

            if skills.insert(id, definition).is_some() {
                return Err(CsvLoadError::InvalidRow(format!(
                    "SkillBase duplicate skill id {id}"
                )));
            }
        }

        for row in tables.bufferbase.all() {
            let id = cast_u16(row.id, "BufferBase", "Id")?;
            let context = format!("BufferBase id={id}");
            let code = resolve_buff_string(&tables.bufferbase, row.code, &context, "Code")?;
            let name = resolve_buff_string(&tables.bufferbase, row.name, &context, "Name")?;
            let description = resolve_buff_string(
                &tables.bufferbase,
                row.description,
                &context,
                "Description",
            )?;
            let buff_type = BuffType::parse(
                resolve_buff_string(&tables.bufferbase, row.bufftype, &context, "BuffType")?
                    .as_str(),
                &context,
            )?;
            let effects = parse_buff_effects(
                resolve_buff_string(
                    &tables.bufferbase,
                    row.effectscript,
                    &context,
                    "EffectScript",
                )?
                .as_str(),
                &context,
            )?;

            let definition = BuffDefinition {
                id,
                code,
                name,
                description,
                buff_type,
                max_stacks: cast_u8(row.maxstacks, "BufferBase", "MaxStacks")?,
                duration_frames: cast_u16(row.durationframes, "BufferBase", "DurationFrames")?,
                interval_frames: cast_u16(row.intervalframes, "BufferBase", "IntervalFrames")?,
                effects,
                can_dispel: row.candispel != 0,
            };

            if buffs.insert(id, definition).is_some() {
                return Err(CsvLoadError::InvalidRow(format!(
                    "BufferBase duplicate buff id {id}"
                )));
            }
        }

        Ok(Self { skills, buffs })
    }

    #[cfg(test)]
    pub fn load_from_csv_dir(csv_dir: &Path) -> Result<Self, CsvLoadError> {
        let tables = ConfigTables::load_from_dir(csv_dir)?;
        Self::from_tables(&tables)
    }
}

impl CombatCatalog for CsvCombatCatalog {
    fn skill_definition(&self, skill_id: u16) -> Option<&SkillDefinition> {
        self.skills.get(&skill_id)
    }

    fn buff_definition(&self, buff_id: u16) -> Option<&BuffDefinition> {
        self.buffs.get(&buff_id)
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct BuiltinCombatCatalog {
    inner: CsvCombatCatalog,
}

#[allow(dead_code)]
impl BuiltinCombatCatalog {
    pub fn new() -> Self {
        let skills = HashMap::from([
            (
                1,
                SkillDefinition {
                    id: 1,
                    code: "basic_attack".to_string(),
                    name: "basic_attack".to_string(),
                    description: "基础近战攻击".to_string(),
                    cooldown_frames: 30,
                    cast_frames: 0,
                    range: 50.0,
                    target_type: SkillTargetType::Enemy,
                    effects: vec![super::skills::SkillEffect::damage(
                        DamageFormula::Fixed(10),
                        0.0,
                    )],
                },
            ),
            (
                2,
                SkillDefinition {
                    id: 2,
                    code: "fireball".to_string(),
                    name: "fireball".to_string(),
                    description: "远程范围伤害".to_string(),
                    cooldown_frames: 90,
                    cast_frames: 0,
                    range: 300.0,
                    target_type: SkillTargetType::Enemy,
                    effects: vec![super::skills::SkillEffect::damage(
                        DamageFormula::Fixed(50),
                        30.0,
                    )],
                },
            ),
            (
                3,
                SkillDefinition {
                    id: 3,
                    code: "heal".to_string(),
                    name: "heal".to_string(),
                    description: "单体恢复生命".to_string(),
                    cooldown_frames: 120,
                    cast_frames: 0,
                    range: 200.0,
                    target_type: SkillTargetType::Ally,
                    effects: vec![super::skills::SkillEffect::heal(80, 0.0)],
                },
            ),
            (
                4,
                SkillDefinition {
                    id: 4,
                    code: "charge".to_string(),
                    name: "charge".to_string(),
                    description: "造成伤害并击退".to_string(),
                    cooldown_frames: 60,
                    cast_frames: 0,
                    range: 150.0,
                    target_type: SkillTargetType::Enemy,
                    effects: vec![
                        super::skills::SkillEffect::damage(DamageFormula::Fixed(20), 0.0),
                        super::skills::SkillEffect::knockback(100.0, 0.0),
                    ],
                },
            ),
            (
                5,
                SkillDefinition {
                    id: 5,
                    code: "burn".to_string(),
                    name: "burn".to_string(),
                    description: "命中后附加灼烧".to_string(),
                    cooldown_frames: 0,
                    cast_frames: 0,
                    range: 50.0,
                    target_type: SkillTargetType::Enemy,
                    effects: vec![
                        super::skills::SkillEffect::damage(DamageFormula::Fixed(5), 0.0),
                        super::skills::SkillEffect::apply_buff(1, 180, 0.0),
                    ],
                },
            ),
        ]);

        let buffs = HashMap::from([
            (
                1,
                BuffDefinition {
                    id: 1,
                    code: "burn".to_string(),
                    name: "burn".to_string(),
                    description: "每秒造成持续伤害".to_string(),
                    buff_type: BuffType::Dot,
                    max_stacks: 1,
                    duration_frames: 180,
                    interval_frames: 30,
                    effects: vec![super::buffs::BuffEffect::damage_periodic(5)],
                    can_dispel: true,
                },
            ),
            (
                2,
                BuffDefinition {
                    id: 2,
                    code: "shield".to_string(),
                    name: "shield".to_string(),
                    description: "提升防御能力".to_string(),
                    buff_type: BuffType::Buff,
                    max_stacks: 1,
                    duration_frames: 300,
                    interval_frames: 0,
                    effects: vec![super::buffs::BuffEffect::modify_defense(5)],
                    can_dispel: true,
                },
            ),
            (
                3,
                BuffDefinition {
                    id: 3,
                    code: "slow".to_string(),
                    name: "slow".to_string(),
                    description: "降低移动速度".to_string(),
                    buff_type: BuffType::Debuff,
                    max_stacks: 3,
                    duration_frames: 120,
                    interval_frames: 0,
                    effects: vec![super::buffs::BuffEffect::modify_speed(-20)],
                    can_dispel: true,
                },
            ),
            (
                4,
                BuffDefinition {
                    id: 4,
                    code: "attack_up".to_string(),
                    name: "attack_up".to_string(),
                    description: "提高攻击属性".to_string(),
                    buff_type: BuffType::Buff,
                    max_stacks: 5,
                    duration_frames: 180,
                    interval_frames: 0,
                    effects: vec![super::buffs::BuffEffect::modify_attack(10)],
                    can_dispel: true,
                },
            ),
            (
                5,
                BuffDefinition {
                    id: 5,
                    code: "regen".to_string(),
                    name: "regen".to_string(),
                    description: "持续恢复生命".to_string(),
                    buff_type: BuffType::Hot,
                    max_stacks: 1,
                    duration_frames: 180,
                    interval_frames: 30,
                    effects: vec![super::buffs::BuffEffect::heal_periodic(5)],
                    can_dispel: true,
                },
            ),
        ]);

        Self {
            inner: CsvCombatCatalog { skills, buffs },
        }
    }
}

impl Default for BuiltinCombatCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl CombatCatalog for BuiltinCombatCatalog {
    fn skill_definition(&self, skill_id: u16) -> Option<&SkillDefinition> {
        self.inner.skill_definition(skill_id)
    }

    fn buff_definition(&self, buff_id: u16) -> Option<&BuffDefinition> {
        self.inner.buff_definition(buff_id)
    }
}

fn cast_u16(value: i32, table: &str, field: &str) -> Result<u16, CsvLoadError> {
    u16::try_from(value).map_err(|_| {
        CsvLoadError::Parse(format!("{table} field `{field}` out of range for u16: {value}"))
    })
}

fn cast_u8(value: i32, table: &str, field: &str) -> Result<u8, CsvLoadError> {
    u8::try_from(value).map_err(|_| {
        CsvLoadError::Parse(format!("{table} field `{field}` out of range for u8: {value}"))
    })
}

fn resolve_skill_string(
    table: &SkillBase,
    key: u32,
    context: &str,
    field: &str,
) -> Result<String, CsvLoadError> {
    table.resolve_string(key).map(ToOwned::to_owned).ok_or_else(|| {
        CsvLoadError::Parse(format!(
            "{context}: missing string pool entry for field `{field}` key {key}"
        ))
    })
}

fn resolve_buff_string(
    table: &BufferBase,
    key: u32,
    context: &str,
    field: &str,
) -> Result<String, CsvLoadError> {
    table.resolve_string(key).map(ToOwned::to_owned).ok_or_else(|| {
        CsvLoadError::Parse(format!(
            "{context}: missing string pool entry for field `{field}` key {key}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_catalog_loads_sample_tables() {
        let csv_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("csv");
        let catalog = CsvCombatCatalog::load_from_csv_dir(&csv_dir).unwrap();

        let fireball = catalog.skill_definition(2).unwrap();
        assert_eq!(fireball.code, "fireball");
        assert_eq!(fireball.effects.len(), 1);
        assert_eq!(fireball.range, 300.0);

        let burn = catalog.buff_definition(1).unwrap();
        assert_eq!(burn.code, "burn");
        assert_eq!(burn.interval_frames, 30);
        assert_eq!(burn.effects.len(), 1);
    }
}
