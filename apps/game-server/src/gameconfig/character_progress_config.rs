use std::collections::BTreeSet;

use serde_json::Value;

use crate::config_table::CsvLoadError;
use crate::csv_code::characterprogresstable::CharacterProgressTable;
use crate::csv_code::disciplinetable::DisciplineTable;
use crate::csv_code::itemtable::ItemTable;
use crate::csv_code::titletable::TitleTable;
use crate::core::reward_source::RewardSource;

const VALID_SOURCE_TYPES: &[&str] = &[
    "task",
    "quest",
    "achievement",
    "activity",
    "ranking",
    "world_event",
];

const VALID_DISCIPLINE_TIERS: &[&str] = &[
    "novice",
    "apprentice",
    "adept",
    "expert",
    "master",
    "grandmaster",
];

pub fn validate_character_progress_table(
    progress_table: &CharacterProgressTable,
    title_table: &TitleTable,
    discipline_table: &DisciplineTable,
    item_table: &ItemTable,
) -> Result<(), CsvLoadError> {
    let mut progress_ids = BTreeSet::new();
    let discipline_ids = collect_discipline_ids(discipline_table);

    for (row_index, row) in progress_table.rows.iter().enumerate() {
        let csv_row = row_index + 3;
        let progress_id =
            resolve_required_string(progress_table, row.progressid, csv_row, "ProgressId")?;
        let progress_id = progress_id.trim();
        if progress_id.is_empty() {
            return invalid_row(csv_row, "ProgressId must not be empty");
        }
        if !progress_ids.insert(progress_id.to_string()) {
            return invalid_row(csv_row, format!("duplicate ProgressId `{progress_id}`"));
        }

        let source_type =
            resolve_required_string(progress_table, row.sourcetype, csv_row, "SourceType")?;
        let source_type = source_type.trim().to_ascii_lowercase();
        if !VALID_SOURCE_TYPES.contains(&source_type.as_str()) {
            return invalid_row(
                csv_row,
                format!(
                    "SourceType `{source_type}` must be one of {}",
                    VALID_SOURCE_TYPES.join(",")
                ),
            );
        }

        let source_id = resolve_required_string(progress_table, row.sourceid, csv_row, "SourceId")?;
        if source_id.trim().is_empty() {
            return invalid_row(csv_row, "SourceId must not be empty");
        }
        RewardSource::from_character_progress(&source_type, source_id)
            .map_err(|error| invalid_row_value(csv_row, format!("invalid reward source: {error}")))?;

        let conditions = parse_json_field(progress_table, row.conditions, csv_row, "Conditions")?;
        validate_condition_value(
            &conditions,
            csv_row,
            title_table,
            &discipline_ids,
            item_table,
        )?;

        let rewards = parse_json_field(progress_table, row.rewards, csv_row, "Rewards")?;
        validate_rewards_value(&rewards, csv_row, title_table, &discipline_ids)?;
    }

    Ok(())
}

fn validate_condition_value(
    value: &Value,
    csv_row: usize,
    title_table: &TitleTable,
    discipline_ids: &BTreeSet<String>,
    item_table: &ItemTable,
) -> Result<(), CsvLoadError> {
    match value {
        Value::String(text) if text.eq_ignore_ascii_case("always") => Ok(()),
        Value::Object(map) => {
            if let Some(all_of) = map.get("all_of").or_else(|| map.get("allOf")) {
                return validate_condition_array(
                    all_of,
                    csv_row,
                    title_table,
                    discipline_ids,
                    item_table,
                );
            }
            if let Some(any_of) = map.get("any_of").or_else(|| map.get("anyOf")) {
                return validate_condition_array(
                    any_of,
                    csv_row,
                    title_table,
                    discipline_ids,
                    item_table,
                );
            }

            let condition_type = require_string(value, &["type", "kind", "condition"], csv_row)?;
            match condition_type.as_str() {
                "always" => Ok(()),
                "affinity" | "element_affinity" | "mastery" | "element_mastery" => {
                    require_element(value, csv_row)?;
                    require_non_negative_i32(
                        value,
                        &["min", "threshold", "value", "required"],
                        csv_row,
                    )
                }
                "discipline_tier" => {
                    let discipline_id =
                        require_string(value, &["discipline_id", "discipline"], csv_row)?;
                    require_known_discipline(csv_row, &discipline_id, discipline_ids)?;
                    let tier = require_string(value, &["tier", "min_tier"], csv_row)?;
                    if !VALID_DISCIPLINE_TIERS.contains(&tier.as_str()) {
                        return invalid_row(
                            csv_row,
                            format!("discipline_tier uses invalid tier `{tier}`"),
                        );
                    }
                    Ok(())
                }
                "title" => {
                    let title_id = require_string_preserve(value, &["title_id", "title"], csv_row)?;
                    require_title(title_table, csv_row, &title_id).map(|_| ())
                }
                "discipline" => {
                    let discipline_id =
                        require_string(value, &["discipline_id", "discipline"], csv_row)?;
                    require_known_discipline(csv_row, &discipline_id, discipline_ids)
                }
                "item_growth" => {
                    let item_id = require_positive_i32(value, &["item_id", "itemId"], csv_row)?;
                    if item_table.get(item_id).is_none() {
                        return invalid_row(
                            csv_row,
                            format!("item_growth references unknown ItemTable.Id `{item_id}`"),
                        );
                    }
                    require_element(value, csv_row)?;
                    require_non_negative_i32(
                        value,
                        &["min", "threshold", "value", "required"],
                        csv_row,
                    )
                }
                other => invalid_row(
                    csv_row,
                    format!("unsupported condition type `{other}` in Conditions"),
                ),
            }
        }
        Value::Array(_) => {
            validate_condition_array(value, csv_row, title_table, discipline_ids, item_table)
        }
        _ => invalid_row(
            csv_row,
            "Conditions must be a JSON object, array, or `always`",
        ),
    }
}

fn validate_condition_array(
    value: &Value,
    csv_row: usize,
    title_table: &TitleTable,
    discipline_ids: &BTreeSet<String>,
    item_table: &ItemTable,
) -> Result<(), CsvLoadError> {
    let Some(values) = value.as_array() else {
        return invalid_row(csv_row, "condition group requires an array");
    };
    if values.is_empty() {
        return invalid_row(csv_row, "condition group must not be empty");
    }
    for nested in values {
        validate_condition_value(nested, csv_row, title_table, discipline_ids, item_table)?;
    }
    Ok(())
}

fn validate_rewards_value(
    value: &Value,
    csv_row: usize,
    title_table: &TitleTable,
    discipline_ids: &BTreeSet<String>,
) -> Result<(), CsvLoadError> {
    let Some(values) = value.as_array() else {
        return invalid_row(csv_row, "Rewards must be an array");
    };
    if values.is_empty() {
        return invalid_row(csv_row, "Rewards must not be empty");
    }
    for reward in values {
        validate_reward_value(reward, csv_row, title_table, discipline_ids)?;
    }
    Ok(())
}

fn validate_reward_value(
    value: &Value,
    csv_row: usize,
    title_table: &TitleTable,
    discipline_ids: &BTreeSet<String>,
) -> Result<(), CsvLoadError> {
    let reward_type = require_string(value, &["type", "kind", "reward"], csv_row)?;
    match reward_type.as_str() {
        "affinity" | "element_affinity" | "mastery" | "element_mastery" => {
            if !["earth", "fire", "water", "wind"]
                .iter()
                .any(|key| number_field(value, &[*key]).is_some())
            {
                return invalid_row(
                    csv_row,
                    format!("{reward_type} reward requires at least one element delta"),
                );
            }
            Ok(())
        }
        "discipline_points" | "mastery_points" => {
            let discipline_id = require_string(value, &["discipline_id", "discipline"], csv_row)?;
            require_known_discipline(csv_row, &discipline_id, discipline_ids)?;
            let points = require_i32(value, &["points", "points_delta", "delta"], csv_row)?;
            if points <= 0 {
                return invalid_row(csv_row, "discipline_points reward points must be positive");
            }
            Ok(())
        }
        "title" | "title_unlock" => {
            let title_id = require_string_preserve(value, &["title_id", "title"], csv_row)?;
            let title_config = require_title(title_table, csv_row, &title_id)?;
            if title_config.limited != 0
                && string_field_preserve(value, &["expires_at", "expiresAt"]).is_none()
            {
                return invalid_row(
                    csv_row,
                    format!("limited title reward `{title_id}` requires expires_at"),
                );
            }
            Ok(())
        }
        "discipline_eligibility" | "discipline_learn_eligibility" => {
            let discipline_id = require_string(value, &["discipline_id", "discipline"], csv_row)?;
            require_known_discipline(csv_row, &discipline_id, discipline_ids)
        }
        other => invalid_row(
            csv_row,
            format!("unsupported reward type `{other}` in Rewards"),
        ),
    }
}

fn collect_discipline_ids(table: &DisciplineTable) -> BTreeSet<String> {
    table
        .all()
        .iter()
        .filter_map(|row| table.resolve_string(row.disciplineid))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

fn require_known_discipline(
    csv_row: usize,
    discipline_id: &str,
    discipline_ids: &BTreeSet<String>,
) -> Result<(), CsvLoadError> {
    if discipline_ids.contains(discipline_id) {
        Ok(())
    } else {
        invalid_row(
            csv_row,
            format!("references unknown DisciplineTable.DisciplineId `{discipline_id}`"),
        )
    }
}

fn require_title<'a>(
    table: &'a TitleTable,
    csv_row: usize,
    title_id: &str,
) -> Result<&'a crate::csv_code::titletable::TitleTableRow, CsvLoadError> {
    let parsed_id = require_parse_title_id(csv_row, title_id)?;
    table.get(parsed_id).ok_or_else(|| {
        invalid_row_value(csv_row, format!("references unknown TitleId `{title_id}`"))
    })
}

fn require_parse_title_id(csv_row: usize, title_id: &str) -> Result<i32, CsvLoadError> {
    title_id
        .trim()
        .parse::<i32>()
        .map_err(|_| invalid_row_value(csv_row, format!("TitleId `{title_id}` must be numeric")))
}

fn parse_json_field(
    table: &CharacterProgressTable,
    key: u32,
    csv_row: usize,
    field_name: &'static str,
) -> Result<Value, CsvLoadError> {
    let raw = resolve_required_string(table, key, csv_row, field_name)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return invalid_row(csv_row, format!("{field_name} must be valid JSON"));
    }
    serde_json::from_str::<Value>(trimmed).map_err(|error| {
        CsvLoadError::InvalidRow(format!(
            "table CharacterProgressTable row {csv_row} field `{field_name}` invalid JSON: {error}"
        ))
    })
}

fn require_element(value: &Value, csv_row: usize) -> Result<(), CsvLoadError> {
    if string_field(value, &["element"])
        .as_deref()
        .is_some_and(|element| matches!(element, "earth" | "fire" | "water" | "wind"))
    {
        return Ok(());
    }
    if let Some(map) = value.as_object() {
        if map
            .keys()
            .any(|key| matches!(key.as_str(), "earth" | "fire" | "water" | "wind"))
        {
            return Ok(());
        }
    }
    invalid_row(csv_row, "condition requires earth/fire/water/wind element")
}

fn require_string(value: &Value, keys: &[&str], csv_row: usize) -> Result<String, CsvLoadError> {
    string_field(value, keys).ok_or_else(|| {
        invalid_row_value(csv_row, format!("field requires one of {}", keys.join("/")))
    })
}

fn require_string_preserve(
    value: &Value,
    keys: &[&str],
    csv_row: usize,
) -> Result<String, CsvLoadError> {
    string_field_preserve(value, keys).ok_or_else(|| {
        invalid_row_value(csv_row, format!("field requires one of {}", keys.join("/")))
    })
}

fn require_non_negative_i32(
    value: &Value,
    keys: &[&str],
    csv_row: usize,
) -> Result<(), CsvLoadError> {
    let number = require_i32(value, keys, csv_row)?;
    if number < 0 {
        return invalid_row(csv_row, "numeric value must be non-negative");
    }
    Ok(())
}

fn require_positive_i32(value: &Value, keys: &[&str], csv_row: usize) -> Result<i32, CsvLoadError> {
    let number = require_i32(value, keys, csv_row)?;
    if number <= 0 {
        return invalid_row(csv_row, "numeric value must be positive");
    }
    Ok(number)
}

fn require_i32(value: &Value, keys: &[&str], csv_row: usize) -> Result<i32, CsvLoadError> {
    number_field(value, keys).ok_or_else(|| {
        invalid_row_value(csv_row, format!("field requires one of {}", keys.join("/")))
    })
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    string_field_preserve(value, keys).map(|value| value.to_ascii_lowercase())
}

fn string_field_preserve(value: &Value, keys: &[&str]) -> Option<String> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(value) = map.get(*key).or_else(|| map.get(&to_camel_case(key))) {
            if let Some(text) = value.as_str() {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
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
            if let Some(number) = value.as_i64().and_then(|value| i32::try_from(value).ok()) {
                return Some(number);
            }
            if let Some(number) = value.as_str().and_then(|value| value.trim().parse().ok()) {
                return Some(number);
            }
        }
    }
    None
}

fn resolve_required_string<'a>(
    table: &'a CharacterProgressTable,
    key: u32,
    csv_row: usize,
    field_name: &'static str,
) -> Result<&'a str, CsvLoadError> {
    table.resolve_string(key).ok_or_else(|| {
        CsvLoadError::InvalidRow(format!(
            "table CharacterProgressTable row {csv_row} field `{field_name}` references missing string key {key}"
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
        "table CharacterProgressTable row {csv_row}: {}",
        message.into()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config_table::CsvTableLoader;
    use crate::csv_code::characterprogresstable::CharacterProgressTable;
    use crate::csv_code::disciplinetable::{DisciplineTable, DisciplineTableRow};
    use crate::csv_code::itemtable::{ItemTable, ItemTableRow};
    use crate::csv_code::titletable::{TitleTable, TitleTableRow};
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn character_progress_config_accepts_base_sample() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let progress =
            CharacterProgressTable::load_from_csv(&root.join("csv/CharacterProgressTable.csv"))
                .expect("sample CharacterProgressTable.csv should load");
        let title =
            TitleTable::load_from_csv(&root.join("csv/TitleTable.csv")).expect("titles load");
        let discipline = DisciplineTable::load_from_csv(&root.join("csv/DisciplineTable.csv"))
            .expect("disciplines load");
        let item = ItemTable::load_from_csv(&root.join("csv/ItemTable.csv")).expect("items load");

        validate_character_progress_table(&progress, &title, &discipline, &item)
            .expect("sample CharacterProgressTable.csv should validate");
    }

    #[test]
    fn character_progress_config_accepts_top_level_condition_array_as_all_of() {
        let fixture = TempCsvFile::new(
            r#"Id,ProgressId,SourceType,SourceId,Name,Conditions,Rewards,Repeatable,Enabled,Description
int,string,string,string,string,string,string,int,int,string
1,quest_array,quest,q1,Quest,"[{""type"":""affinity"",""element"":""fire"",""min"":2500},{""type"":""mastery"",""element"":""fire"",""min"":0}]","[{""type"":""title"",""title_id"":""2001""}]",0,1,array condition
"#,
        );
        let progress = CharacterProgressTable::load_from_csv(fixture.path()).unwrap();

        validate_character_progress_table(
            &progress,
            &title_table(),
            &discipline_table(),
            &item_table(),
        )
        .expect("top-level condition arrays should validate as all_of groups");
    }

    #[test]
    fn character_progress_config_rejects_limited_title_without_expiry() {
        let fixture = TempCsvFile::new(
            r#"Id,ProgressId,SourceType,SourceId,Name,Conditions,Rewards,Repeatable,Enabled,Description
int,string,string,string,string,string,string,int,int,string
1,activity_bad,activity,summer,Activity,"{""type"":""always""}","[{""type"":""title"",""title_id"":""9001""}]",0,1,bad
"#,
        );
        let progress = CharacterProgressTable::load_from_csv(fixture.path()).unwrap();
        let error = validate_character_progress_table(
            &progress,
            &title_table(),
            &discipline_table(),
            &item_table(),
        )
        .expect_err("limited title without expires_at should be rejected");
        assert!(
            error.to_string().contains("requires expires_at"),
            "error `{error}` should mention expires_at"
        );
    }

    #[test]
    fn character_progress_config_rejects_unknown_condition_and_item_reference() {
        let fixture = TempCsvFile::new(
            r#"Id,ProgressId,SourceType,SourceId,Name,Conditions,Rewards,Repeatable,Enabled,Description
int,string,string,string,string,string,string,int,int,string
1,quest_bad,quest,q1,Quest,"{""type"":""item_growth"",""item_id"":9999,""element"":""fire"",""min"":1}","[{""type"":""title"",""title_id"":""2001""}]",0,1,bad
"#,
        );
        let progress = CharacterProgressTable::load_from_csv(fixture.path()).unwrap();
        let error = validate_character_progress_table(
            &progress,
            &title_table(),
            &discipline_table(),
            &item_table(),
        )
        .expect_err("unknown item reference should be rejected");
        assert!(
            error.to_string().contains("unknown ItemTable.Id"),
            "error `{error}` should mention unknown item reference"
        );

        let fixture = TempCsvFile::new(
            r#"Id,ProgressId,SourceType,SourceId,Name,Conditions,Rewards,Repeatable,Enabled,Description
int,string,string,string,string,string,string,int,int,string
1,quest_bad,quest,q1,Quest,"{""type"":""unknown""}","[{""type"":""title"",""title_id"":""2001""}]",0,1,bad
"#,
        );
        let progress = CharacterProgressTable::load_from_csv(fixture.path()).unwrap();
        let error = validate_character_progress_table(
            &progress,
            &title_table(),
            &discipline_table(),
            &item_table(),
        )
        .expect_err("unknown condition should be rejected");
        assert!(
            error.to_string().contains("unsupported condition type"),
            "error `{error}` should mention unsupported condition: {error}"
        );
    }

    #[test]
    fn character_progress_config_rejects_empty_rewards_and_unknown_discipline() {
        let fixture = TempCsvFile::new(
            r#"Id,ProgressId,SourceType,SourceId,Name,Conditions,Rewards,Repeatable,Enabled,Description
int,string,string,string,string,string,string,int,int,string
1,quest_bad,quest,q1,Quest,"{""type"":""always""}",[],0,1,bad
"#,
        );
        let progress = CharacterProgressTable::load_from_csv(fixture.path()).unwrap();
        let error = validate_character_progress_table(
            &progress,
            &title_table(),
            &discipline_table(),
            &item_table(),
        )
        .expect_err("empty rewards should be rejected");
        assert!(
            error.to_string().contains("Rewards must not be empty"),
            "error `{error}` should mention empty rewards"
        );

        let fixture = TempCsvFile::new(
            r#"Id,ProgressId,SourceType,SourceId,Name,Conditions,Rewards,Repeatable,Enabled,Description
int,string,string,string,string,string,string,int,int,string
1,quest_bad,quest,q1,Quest,"{""type"":""always""}","[{""type"":""discipline_eligibility"",""discipline_id"":""unknown""}]",0,1,bad
"#,
        );
        let progress = CharacterProgressTable::load_from_csv(fixture.path()).unwrap();
        let error = validate_character_progress_table(
            &progress,
            &title_table(),
            &discipline_table(),
            &item_table(),
        )
        .expect_err("unknown discipline should be rejected");
        assert!(
            error
                .to_string()
                .contains("unknown DisciplineTable.DisciplineId"),
            "error `{error}` should mention unknown discipline"
        );
    }

    fn title_table() -> TitleTable {
        let rows = vec![
            TitleTableRow {
                titleid: 2001,
                limited: 0,
                ..TitleTableRow::default()
            },
            TitleTableRow {
                titleid: 9001,
                limited: 1,
                ..TitleTableRow::default()
            },
        ];
        let by_id = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.titleid, index))
            .collect();
        TitleTable {
            string_pool: HashMap::new(),
            rows,
            by_id,
        }
    }

    fn discipline_table() -> DisciplineTable {
        let mut string_pool = HashMap::new();
        string_pool.insert(1, "forging".to_string());
        string_pool.insert(2, "wind_canyon_lore".to_string());
        let rows = vec![
            DisciplineTableRow {
                id: 1,
                disciplineid: 1,
                ..DisciplineTableRow::default()
            },
            DisciplineTableRow {
                id: 2,
                disciplineid: 2,
                ..DisciplineTableRow::default()
            },
        ];
        let by_id = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id, index))
            .collect();
        DisciplineTable {
            string_pool,
            rows,
            by_id,
        }
    }

    fn item_table() -> ItemTable {
        let rows = vec![ItemTableRow {
            id: 1002,
            ..ItemTableRow::default()
        }];
        let by_id = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id, index))
            .collect();
        ItemTable {
            string_pool: HashMap::new(),
            rows,
            by_id,
        }
    }

    struct TempCsvFile {
        path: PathBuf,
    }

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

    impl TempCsvFile {
        fn new(contents: &str) -> Self {
            let unique = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "game-server-character-progress-table-test-{}-{unique}.csv",
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
