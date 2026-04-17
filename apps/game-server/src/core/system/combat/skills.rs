use serde::{Deserialize, Serialize};

use crate::core::config_table::CsvLoadError;

use super::components::DamageFormula;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillTargetType {
    Enemy,
    Ally,
    SelfOnly,
    Ground,
}

impl SkillTargetType {
    pub fn parse(value: &str, context: &str) -> Result<Self, CsvLoadError> {
        match value {
            "Enemy" => Ok(Self::Enemy),
            "Ally" => Ok(Self::Ally),
            "SelfOnly" => Ok(Self::SelfOnly),
            "Ground" => Ok(Self::Ground),
            _ => Err(CsvLoadError::Parse(format!(
                "{context}: unsupported skill target type `{value}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillEffectType {
    Damage,
    Heal,
    ApplyBuff,
    Knockback,
    Custom,
}

impl SkillEffectType {
    pub fn parse(value: &str, context: &str) -> Result<Self, CsvLoadError> {
        match value {
            "Damage" => Ok(Self::Damage),
            "Heal" => Ok(Self::Heal),
            "ApplyBuff" => Ok(Self::ApplyBuff),
            "Knockback" => Ok(Self::Knockback),
            "Custom" => Ok(Self::Custom),
            _ => Err(CsvLoadError::Parse(format!(
                "{context}: unsupported skill effect type `{value}`"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SkillEffect {
    pub effect_type: SkillEffectType,
    pub formula: DamageFormula,
    pub value: i32,
    pub buff_id: u16,
    pub buff_duration: u16,
    pub aoe_radius: f32,
    pub displacement_distance: f32,
}

impl SkillEffect {
    #[allow(dead_code)]
    pub const fn damage(formula: DamageFormula, aoe_radius: f32) -> Self {
        Self {
            effect_type: SkillEffectType::Damage,
            formula,
            value: 0,
            buff_id: 0,
            buff_duration: 0,
            aoe_radius,
            displacement_distance: 0.0,
        }
    }

    #[allow(dead_code)]
    pub const fn heal(value: i32, aoe_radius: f32) -> Self {
        Self {
            effect_type: SkillEffectType::Heal,
            formula: DamageFormula::Fixed(0),
            value,
            buff_id: 0,
            buff_duration: 0,
            aoe_radius,
            displacement_distance: 0.0,
        }
    }

    #[allow(dead_code)]
    pub const fn apply_buff(buff_id: u16, buff_duration: u16, aoe_radius: f32) -> Self {
        Self {
            effect_type: SkillEffectType::ApplyBuff,
            formula: DamageFormula::Fixed(0),
            value: 0,
            buff_id,
            buff_duration,
            aoe_radius,
            displacement_distance: 0.0,
        }
    }

    #[allow(dead_code)]
    pub const fn knockback(distance: f32, aoe_radius: f32) -> Self {
        Self {
            effect_type: SkillEffectType::Knockback,
            formula: DamageFormula::Fixed(0),
            value: 0,
            buff_id: 0,
            buff_duration: 0,
            aoe_radius,
            displacement_distance: distance,
        }
    }

    pub fn parse_script_entry(entry: &str, context: &str) -> Result<Self, CsvLoadError> {
        let parts = split_script_fields(entry, 8, context)?;
        let effect_type = SkillEffectType::parse(parts[0], context)?;
        let formula_value = parse_i32_field(parts[2], "FormulaValue", context)?;
        let value = parse_i32_field(parts[3], "Value", context)?;
        let buff_id = parse_u16_field(parts[4], "BuffId", context)?;
        let buff_duration = parse_u16_field(parts[5], "BuffDuration", context)?;
        let aoe_radius = parse_f32_field(parts[6], "AoeRadius", context)?;
        let displacement_distance =
            parse_f32_field(parts[7], "DisplacementDistance", context)?;

        Ok(Self {
            effect_type,
            formula: DamageFormula::parse_script(parts[1], formula_value, context)?,
            value,
            buff_id,
            buff_duration,
            aoe_radius,
            displacement_distance,
        })
    }
}

#[derive(Debug, Clone)]
pub struct SkillDefinition {
    pub id: u16,
    pub code: String,
    pub name: String,
    pub description: String,
    pub cooldown_frames: u16,
    pub cast_frames: u16,
    pub range: f32,
    pub target_type: SkillTargetType,
    pub effects: Vec<SkillEffect>,
}

pub fn parse_skill_effects(script: &str, context: &str) -> Result<Vec<SkillEffect>, CsvLoadError> {
    if script.trim().is_empty() {
        return Ok(Vec::new());
    }

    script
        .split('|')
        .filter(|entry| !entry.trim().is_empty())
        .map(|entry| SkillEffect::parse_script_entry(entry.trim(), context))
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

fn parse_u16_field(value: &str, field_name: &str, context: &str) -> Result<u16, CsvLoadError> {
    value.parse::<u16>().map_err(|error| {
        CsvLoadError::Parse(format!(
            "{context}: invalid {field_name} `{value}`: {error}"
        ))
    })
}

fn parse_f32_field(value: &str, field_name: &str, context: &str) -> Result<f32, CsvLoadError> {
    value.parse::<f32>().map_err(|error| {
        CsvLoadError::Parse(format!(
            "{context}: invalid {field_name} `{value}`: {error}"
        ))
    })
}
