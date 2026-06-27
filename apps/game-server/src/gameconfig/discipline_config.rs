use std::collections::BTreeSet;

use serde_json::Value;

use crate::config_table::CsvLoadError;
use crate::csv_code::disciplinetable::DisciplineTable;
use crate::csv_code::skillbase::SkillBase;

const VALID_DISCIPLINE_TIERS: &[&str] = &[
    "novice",
    "apprentice",
    "adept",
    "expert",
    "master",
    "grandmaster",
];

const SUPPORTED_CONDITION_TYPES: &[&str] = &[
    "all_of",
    "any_of",
    "affinity",
    "mastery",
    "discipline_tier",
    "title",
    "item",
    "quest",
    "event",
    "npc_affection",
    "organization",
    "scene_location",
    "world_state",
    "world_status",
    "world_flag",
];

pub fn validate_discipline_table(table: &DisciplineTable) -> Result<(), CsvLoadError> {
    let mut seen_ids = BTreeSet::new();
    let mut seen_keys = BTreeSet::new();

    for (row_index, row) in table.rows.iter().enumerate() {
        let csv_row = row_index + 3;
        if !seen_ids.insert(row.id) {
            return invalid_row(csv_row, format!("duplicate Id {}", row.id));
        }

        let discipline_id =
            resolve_required_string(table, row.disciplineid, csv_row, "DisciplineId")?;
        if discipline_id.trim().is_empty() {
            return invalid_row(csv_row, "missing DisciplineId");
        }
        if !seen_keys.insert(discipline_id.to_string()) {
            return invalid_row(csv_row, format!("duplicate DisciplineId `{discipline_id}`"));
        }

        let name = resolve_required_string(table, row.name, csv_row, "Name")?;
        if name.trim().is_empty() {
            return invalid_row(csv_row, "missing display Name");
        }

        let learn_conditions =
            parse_json_object(table, row.learnconditions, csv_row, "LearnConditions")?;
        validate_condition_value(&learn_conditions, csv_row, "LearnConditions")?;

        let tier_rules = parse_json_object(table, row.tierrules, csv_row, "TierRules")?;
        validate_tier_rules(&tier_rules, csv_row)?;

        let display_fields = parse_json_object(table, row.displayfields, csv_row, "DisplayFields")?;
        validate_display_fields(&display_fields, csv_row)?;
    }

    Ok(())
}

pub fn validate_discipline_skill_pool(
    discipline_table: &DisciplineTable,
    skill_table: &SkillBase,
) -> Result<(), CsvLoadError> {
    let skill_codes = skill_table
        .all()
        .iter()
        .filter_map(|row| skill_table.resolve_string(row.code))
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .collect::<BTreeSet<_>>();

    for (row_index, row) in discipline_table.rows.iter().enumerate() {
        let csv_row = row_index + 3;
        for key in &row.skillpool {
            let skill_code = resolve_required_string(discipline_table, *key, csv_row, "SkillPool")?;
            let skill_code = skill_code.trim();
            if skill_code.is_empty() {
                return invalid_row(csv_row, "SkillPool contains empty SkillBase.Code");
            }
            if !skill_codes.contains(skill_code) {
                return invalid_row(
                    csv_row,
                    format!("SkillPool references unknown SkillBase.Code `{skill_code}`"),
                );
            }
        }
    }

    Ok(())
}

fn validate_condition_value(
    value: &Value,
    csv_row: usize,
    field_name: &'static str,
) -> Result<(), CsvLoadError> {
    match value {
        Value::Object(map) => {
            if let Some(all_of) = map.get("all_of").or_else(|| map.get("allOf")) {
                return validate_condition_array(all_of, csv_row, field_name);
            }
            if let Some(any_of) = map.get("any_of").or_else(|| map.get("anyOf")) {
                return validate_condition_array(any_of, csv_row, field_name);
            }

            let Some(condition_type) = string_field(value, &["type", "kind", "rule"]) else {
                return invalid_row(
                    csv_row,
                    format!("field `{field_name}` condition requires type/all_of/any_of"),
                );
            };
            if !SUPPORTED_CONDITION_TYPES.contains(&condition_type.as_str()) {
                return invalid_row(
                    csv_row,
                    format!(
                        "field `{field_name}` uses unsupported condition type `{condition_type}`"
                    ),
                );
            }
            validate_typed_condition(&condition_type, value, csv_row, field_name)
        }
        Value::Array(_) => validate_condition_array(value, csv_row, field_name),
        _ => invalid_row(
            csv_row,
            format!("field `{field_name}` must be a JSON object or array"),
        ),
    }
}

fn validate_condition_array(
    value: &Value,
    csv_row: usize,
    field_name: &'static str,
) -> Result<(), CsvLoadError> {
    let Some(values) = value.as_array() else {
        return invalid_row(csv_row, format!("field `{field_name}` requires an array"));
    };
    if values.is_empty() {
        return invalid_row(
            csv_row,
            format!("field `{field_name}` array must not be empty"),
        );
    }
    for nested in values {
        validate_condition_value(nested, csv_row, field_name)?;
    }
    Ok(())
}

fn validate_typed_condition(
    condition_type: &str,
    value: &Value,
    csv_row: usize,
    field_name: &'static str,
) -> Result<(), CsvLoadError> {
    match condition_type {
        "affinity" | "mastery" => {
            require_element(value, csv_row, field_name)?;
            require_non_negative_i32(
                value,
                &["min", "threshold", "value", "required"],
                csv_row,
                field_name,
            )
        }
        "discipline_tier" => {
            require_string(value, &["discipline_id", "discipline"], csv_row, field_name)?;
            let tier = require_string(value, &["tier", "min_tier"], csv_row, field_name)?;
            if !VALID_DISCIPLINE_TIERS.contains(&tier.as_str()) {
                return invalid_row(
                    csv_row,
                    format!("field `{field_name}` invalid tier `{tier}`"),
                );
            }
            Ok(())
        }
        "title" => {
            require_string(value, &["title_id", "title"], csv_row, field_name)?;
            Ok(())
        }
        "item" => {
            let item_id = number_field(value, &["item_id", "itemId"]).ok_or_else(|| {
                invalid_row_value(
                    csv_row,
                    format!("field `{field_name}` item requires item_id"),
                )
            })?;
            if item_id <= 0 {
                return invalid_row(
                    csv_row,
                    format!("field `{field_name}` item_id must be positive"),
                );
            }
            require_positive_u32(value, &["count", "amount"], csv_row, field_name)
        }
        "quest" => {
            require_string(value, &["quest_id", "quest"], csv_row, field_name)?;
            Ok(())
        }
        "event" => {
            require_string(value, &["event_id", "event"], csv_row, field_name)?;
            Ok(())
        }
        "npc_affection" => {
            require_string(value, &["npc_id", "npc"], csv_row, field_name)?;
            require_non_negative_i32(value, &["min", "affection", "favor"], csv_row, field_name)
        }
        "organization" => {
            require_string(
                value,
                &["organization_id", "organization", "org_id"],
                csv_row,
                field_name,
            )?;
            Ok(())
        }
        "scene_location" => {
            require_string(
                value,
                &["scene_id", "scene", "region_id", "region"],
                csv_row,
                field_name,
            )?;
            Ok(())
        }
        "world_state" | "world_status" | "world_flag" => {
            require_string(value, &["key", "state", "flag"], csv_row, field_name)?;
            Ok(())
        }
        "all_of" | "any_of" => Ok(()),
        _ => invalid_row(
            csv_row,
            format!("field `{field_name}` uses unsupported condition type `{condition_type}`"),
        ),
    }
}

fn validate_tier_rules(value: &Value, csv_row: usize) -> Result<(), CsvLoadError> {
    let Some(map) = value.as_object() else {
        return invalid_row(csv_row, "field `TierRules` must be a JSON object");
    };

    let initial_tier = string_field(value, &["initial_tier", "initialTier"])
        .unwrap_or_else(|| "novice".to_string());
    if !VALID_DISCIPLINE_TIERS.contains(&initial_tier.as_str()) {
        return invalid_row(
            csv_row,
            format!("field `TierRules` invalid initial_tier `{initial_tier}`"),
        );
    }

    if number_field(value, &["initial_points", "initialPoints"]).unwrap_or(0) < 0 {
        return invalid_row(
            csv_row,
            "field `TierRules` initial_points must be non-negative",
        );
    }

    let Some(tiers) = map.get("tiers").and_then(Value::as_array) else {
        return invalid_row(csv_row, "field `TierRules` requires tiers array");
    };
    if tiers.is_empty() {
        return invalid_row(csv_row, "field `TierRules` tiers array must not be empty");
    }

    let mut seen = BTreeSet::new();
    let mut previous_min = -1;
    for tier in tiers {
        let Some(tier_name) = string_field(tier, &["tier", "name"]) else {
            return invalid_row(csv_row, "field `TierRules` tier entry requires tier");
        };
        if !VALID_DISCIPLINE_TIERS.contains(&tier_name.as_str()) {
            return invalid_row(
                csv_row,
                format!("field `TierRules` invalid tier `{tier_name}`"),
            );
        }
        if !seen.insert(tier_name.clone()) {
            return invalid_row(
                csv_row,
                format!("field `TierRules` duplicate tier `{tier_name}`"),
            );
        }
        let min_points =
            number_field(tier, &["min_points", "minPoints", "points"]).ok_or_else(|| {
                invalid_row_value(csv_row, "field `TierRules` tier requires min_points")
            })?;
        if min_points < 0 {
            return invalid_row(csv_row, "field `TierRules` min_points must be non-negative");
        }
        if min_points < previous_min {
            return invalid_row(csv_row, "field `TierRules` min_points must be ascending");
        }
        previous_min = min_points;
    }

    if !seen.contains(&initial_tier) {
        return invalid_row(csv_row, "field `TierRules` tiers must include initial_tier");
    }

    Ok(())
}

fn validate_display_fields(value: &Value, csv_row: usize) -> Result<(), CsvLoadError> {
    let Some(map) = value.as_object() else {
        return invalid_row(csv_row, "field `DisplayFields` must be a JSON object");
    };
    if map.is_empty() {
        return invalid_row(csv_row, "field `DisplayFields` must not be empty");
    }
    Ok(())
}

fn parse_json_object(
    table: &DisciplineTable,
    key: u32,
    csv_row: usize,
    field_name: &'static str,
) -> Result<Value, CsvLoadError> {
    let raw = resolve_required_string(table, key, csv_row, field_name)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return invalid_row(csv_row, format!("field `{field_name}` must be valid JSON"));
    }
    serde_json::from_str::<Value>(trimmed).map_err(|error| {
        CsvLoadError::InvalidRow(format!(
            "table DisciplineTable row {csv_row} field `{field_name}` invalid JSON: {error}"
        ))
    })
}

fn require_element(
    value: &Value,
    csv_row: usize,
    field_name: &'static str,
) -> Result<String, CsvLoadError> {
    let element = require_string(value, &["element"], csv_row, field_name)?;
    match element.as_str() {
        "earth" | "fire" | "water" | "wind" => Ok(element),
        _ => invalid_row(
            csv_row,
            format!("field `{field_name}` element must be earth/fire/water/wind"),
        ),
    }
}

fn require_string(
    value: &Value,
    keys: &[&str],
    csv_row: usize,
    field_name: &'static str,
) -> Result<String, CsvLoadError> {
    string_field(value, keys).ok_or_else(|| {
        invalid_row_value(
            csv_row,
            format!("field `{field_name}` requires one of {}", keys.join("/")),
        )
    })
}

fn require_non_negative_i32(
    value: &Value,
    keys: &[&str],
    csv_row: usize,
    field_name: &'static str,
) -> Result<(), CsvLoadError> {
    let number = number_field(value, keys).ok_or_else(|| {
        invalid_row_value(
            csv_row,
            format!("field `{field_name}` requires one of {}", keys.join("/")),
        )
    })?;
    if number < 0 {
        return invalid_row(
            csv_row,
            format!("field `{field_name}` numeric value must be non-negative"),
        );
    }
    Ok(())
}

fn require_positive_u32(
    value: &Value,
    keys: &[&str],
    csv_row: usize,
    field_name: &'static str,
) -> Result<(), CsvLoadError> {
    let number = number_field(value, keys).unwrap_or(1);
    if number <= 0 {
        return invalid_row(
            csv_row,
            format!("field `{field_name}` count must be positive"),
        );
    }
    Ok(())
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(value) = map.get(*key).or_else(|| map.get(&to_camel_case(key))) {
            if let Some(text) = value.as_str() {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_ascii_lowercase());
                }
            } else if let Some(number) = value.as_i64() {
                return Some(number.to_string());
            }
        }
    }
    None
}

fn number_field(value: &Value, keys: &[&str]) -> Option<i32> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(value) = map.get(*key).or_else(|| map.get(&to_camel_case(key))) {
            if let Some(number) = value.as_i64() {
                return i32::try_from(number).ok();
            }
            if let Some(text) = value.as_str() {
                if let Ok(parsed) = text.trim().parse::<i32>() {
                    return Some(parsed);
                }
            }
        }
    }
    None
}

fn resolve_required_string<'a>(
    table: &'a DisciplineTable,
    key: u32,
    csv_row: usize,
    field_name: &'static str,
) -> Result<&'a str, CsvLoadError> {
    table.resolve_string(key).ok_or_else(|| {
        CsvLoadError::InvalidRow(format!(
            "table DisciplineTable row {csv_row} field `{field_name}` references missing string key {key}"
        ))
    })
}

fn to_camel_case(value: &str) -> String {
    let mut result = String::new();
    let mut uppercase_next = false;
    for ch in value.chars() {
        if ch == '_' {
            uppercase_next = true;
        } else if uppercase_next {
            result.push(ch.to_ascii_uppercase());
            uppercase_next = false;
        } else {
            result.push(ch);
        }
    }
    result
}

fn invalid_row<T>(csv_row: usize, message: impl Into<String>) -> Result<T, CsvLoadError> {
    Err(invalid_row_value(csv_row, message))
}

fn invalid_row_value(csv_row: usize, message: impl Into<String>) -> CsvLoadError {
    CsvLoadError::InvalidRow(format!(
        "table DisciplineTable row {csv_row}: {}",
        message.into()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_table::CsvTableLoader;
    use crate::csv_code::disciplinetable::DisciplineTable;
    use crate::csv_code::skillbase::{SkillBase, SkillBaseRow};
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn discipline_table_accepts_base_sample() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("csv/DisciplineTable.csv");
        let table =
            DisciplineTable::load_from_csv(&path).expect("sample DisciplineTable.csv should load");

        validate_discipline_table(&table).expect("sample DisciplineTable.csv should validate");

        assert_eq!(table.rows.len(), 3);
    }

    #[test]
    fn discipline_skill_pool_must_reference_skillbase_code() {
        let table = discipline_table_from_skill_pool("basic_attack|charge");
        validate_discipline_skill_pool(&table, &skill_table(&["basic_attack", "charge"]))
            .expect("known SkillBase.Code values should validate");

        let table = discipline_table_from_skill_pool("unknown_skill");
        let error = validate_discipline_skill_pool(&table, &skill_table(&["basic_attack"]))
            .expect_err("unknown skill code should be rejected");
        assert!(
            error.to_string().contains("unknown SkillBase.Code"),
            "error `{error}` should mention unknown SkillBase.Code"
        );
    }

    #[test]
    fn discipline_table_rejects_invalid_condition_json() {
        assert_invalid(
            r#"Id,DisciplineId,Name,Description,LearnConditions,TierRules,SkillPool,InteractionPermissions,DisplayFields
int,string,string,string,string,string,Array<string>,Array<string>,string
1,test,Test,,{bad},"{""initial_tier"":""novice"",""tiers"":[{""tier"":""novice"",""min_points"":0}]}",skill,learn,"{""icon"":""x""}"
"#,
            "invalid JSON",
        );
    }

    #[test]
    fn discipline_table_rejects_missing_tier_rules() {
        assert_invalid(
            r#"Id,DisciplineId,Name,Description,LearnConditions,TierRules,SkillPool,InteractionPermissions,DisplayFields
int,string,string,string,string,string,Array<string>,Array<string>,string
1,test,Test,,"{""type"":""affinity"",""element"":""fire"",""min"":1}",{},skill,learn,"{""icon"":""x""}"
"#,
            "requires tiers array",
        );
    }

    #[test]
    fn discipline_table_rejects_unknown_condition_type() {
        assert_invalid(
            r#"Id,DisciplineId,Name,Description,LearnConditions,TierRules,SkillPool,InteractionPermissions,DisplayFields
int,string,string,string,string,string,Array<string>,Array<string>,string
1,test,Test,,"{""type"":""unknown""}","{""initial_tier"":""novice"",""tiers"":[{""tier"":""novice"",""min_points"":0}]}",skill,learn,"{""icon"":""x""}"
"#,
            "unsupported condition type",
        );
    }

    fn assert_invalid(contents: &str, expected: &str) {
        let fixture = TempCsvFile::new(contents);
        let table = DisciplineTable::load_from_csv(fixture.path()).expect("csv should parse");
        let error =
            validate_discipline_table(&table).expect_err("discipline table should be invalid");
        assert!(
            error.to_string().contains(expected),
            "error `{error}` should contain `{expected}`"
        );
    }

    fn discipline_table_from_skill_pool(skill_pool: &str) -> DisciplineTable {
        let fixture = TempCsvFile::new(&format!(
            r#"Id,DisciplineId,Name,Description,LearnConditions,TierRules,SkillPool,InteractionPermissions,DisplayFields
int,string,string,string,string,string,Array<string>,Array<string>,string
1,test,Test,,"{{""type"":""affinity"",""element"":""fire"",""min"":1}}","{{""initial_tier"":""novice"",""tiers"":[{{""tier"":""novice"",""min_points"":0}}]}}",{skill_pool},learn,"{{""icon"":""x""}}"
"#
        ));
        DisciplineTable::load_from_csv(fixture.path()).expect("csv should parse")
    }

    fn skill_table(codes: &[&str]) -> SkillBase {
        let mut string_pool = HashMap::new();
        let mut rows = Vec::new();
        let mut by_id = HashMap::new();
        for (index, code) in codes.iter().enumerate() {
            let id = i32::try_from(index + 1).unwrap();
            let key = u32::try_from(index + 1).unwrap();
            string_pool.insert(key, (*code).to_string());
            by_id.insert(id, rows.len());
            rows.push(SkillBaseRow {
                id,
                code: key,
                ..SkillBaseRow::default()
            });
        }
        SkillBase {
            string_pool,
            rows,
            by_id,
        }
    }

    struct TempCsvFile {
        path: PathBuf,
    }

    impl TempCsvFile {
        fn new(contents: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "game-server-discipline-table-test-{}-{unique}.csv",
                std::process::id()
            ));
            fs::write(&path, contents).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempCsvFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }
}
