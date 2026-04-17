use serde::{Deserialize, Serialize};

use crate::core::config_table::CsvLoadError;

use super::components::DamageFormula;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuffType {
    Buff,
    Debuff,
    Dot,
    Hot,
}

impl BuffType {
    pub fn parse(value: &str, context: &str) -> Result<Self, CsvLoadError> {
        match value {
            "Buff" => Ok(Self::Buff),
            "Debuff" => Ok(Self::Debuff),
            "Dot" => Ok(Self::Dot),
            "Hot" => Ok(Self::Hot),
            _ => Err(CsvLoadError::Parse(format!(
                "{context}: unsupported buff type `{value}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuffEffectType {
    DamagePeriodic,
    HealPeriodic,
    ModifyAttack,
    ModifyDefense,
    ModifySpeed,
    Custom,
}

impl BuffEffectType {
    pub fn parse(value: &str, context: &str) -> Result<Self, CsvLoadError> {
        match value {
            "DamagePeriodic" => Ok(Self::DamagePeriodic),
            "HealPeriodic" => Ok(Self::HealPeriodic),
            "ModifyAttack" => Ok(Self::ModifyAttack),
            "ModifyDefense" => Ok(Self::ModifyDefense),
            "ModifySpeed" => Ok(Self::ModifySpeed),
            "Custom" => Ok(Self::Custom),
            _ => Err(CsvLoadError::Parse(format!(
                "{context}: unsupported buff effect type `{value}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BuffEffect {
    pub effect_type: BuffEffectType,
    pub value: i32,
    pub formula: DamageFormula,
}

impl BuffEffect {
    #[allow(dead_code)]
    pub const fn damage_periodic(value: i32) -> Self {
        Self {
            effect_type: BuffEffectType::DamagePeriodic,
            value,
            formula: DamageFormula::Fixed(value),
        }
    }

    #[allow(dead_code)]
    pub const fn heal_periodic(value: i32) -> Self {
        Self {
            effect_type: BuffEffectType::HealPeriodic,
            value,
            formula: DamageFormula::Fixed(0),
        }
    }

    #[allow(dead_code)]
    pub const fn modify_attack(value: i32) -> Self {
        Self {
            effect_type: BuffEffectType::ModifyAttack,
            value,
            formula: DamageFormula::Fixed(0),
        }
    }

    #[allow(dead_code)]
    pub const fn modify_defense(value: i32) -> Self {
        Self {
            effect_type: BuffEffectType::ModifyDefense,
            value,
            formula: DamageFormula::Fixed(0),
        }
    }

    #[allow(dead_code)]
    pub const fn modify_speed(value: i32) -> Self {
        Self {
            effect_type: BuffEffectType::ModifySpeed,
            value,
            formula: DamageFormula::Fixed(0),
        }
    }

    pub fn parse_script_entry(entry: &str, context: &str) -> Result<Self, CsvLoadError> {
        let parts = split_script_fields(entry, 4, context)?;
        let effect_type = BuffEffectType::parse(parts[0], context)?;
        let formula_value = parse_i32_field(parts[2], "FormulaValue", context)?;
        let value = parse_i32_field(parts[3], "Value", context)?;

        Ok(Self {
            effect_type,
            value,
            formula: DamageFormula::parse_script(parts[1], formula_value, context)?,
        })
    }
}

#[derive(Debug, Clone)]
pub struct BuffDefinition {
    pub id: u16,
    pub code: String,
    pub name: String,
    pub description: String,
    pub buff_type: BuffType,
    pub max_stacks: u8,
    pub duration_frames: u16,
    pub interval_frames: u16,
    pub effects: Vec<BuffEffect>,
    pub can_dispel: bool,
}

pub fn parse_buff_effects(script: &str, context: &str) -> Result<Vec<BuffEffect>, CsvLoadError> {
    if script.trim().is_empty() {
        return Ok(Vec::new());
    }

    script
        .split('|')
        .filter(|entry| !entry.trim().is_empty())
        .map(|entry| BuffEffect::parse_script_entry(entry.trim(), context))
        .collect()
}

fn split_script_fields<'a>(
    entry: &'a str,
    expected_len: usize,
    context: &str,
) -> Result<Vec<&'a str>, CsvLoadError> {
    let parts = entry.split(',').map(|value| value.trim()).collect::<Vec<_>>();
    if parts.len() != expected_len {
        return Err(CsvLoadError::Parse(format!(
            "{context}: expected {expected_len} effect columns, got {} in `{entry}`",
            parts.len()
        )));
    }
    Ok(parts)
}

fn parse_i32_field(value: &str, field_name: &str, context: &str) -> Result<i32, CsvLoadError> {
    value.parse::<i32>().map_err(|error| {
        CsvLoadError::Parse(format!(
            "{context}: invalid {field_name} `{value}`: {error}"
        ))
    })
}
