use std::collections::BTreeSet;

use serde_json::Value;

use crate::config_table::CsvLoadError;
use crate::csv_code::titletable::TitleTable;

const ALLOWED_TITLE_TYPES: &[&str] = &["identity", "discipline", "event", "honor", "gm", "system"];

const COMBAT_EFFECT_KEYS: &[&str] = &[
    "attack",
    "defense",
    "max_hp",
    "maxhp",
    "hp",
    "crit",
    "crit_rate",
    "critrate",
    "damage",
    "damage_bonus",
    "damagebonus",
    "resist",
    "resistance",
    "heal",
    "healing",
    "move_speed",
    "movespeed",
    "speed",
    "cooldown",
    "cooldown_reduction",
    "cooldownreduction",
    "combat",
    "buff",
    "debuff",
    "element_mastery",
    "mastery",
    "affinity",
];

pub fn validate_title_table(table: &TitleTable) -> Result<(), CsvLoadError> {
    let mut seen_title_ids = BTreeSet::new();
    for (row_index, row) in table.rows.iter().enumerate() {
        let csv_row = row_index + 3;
        if !seen_title_ids.insert(row.titleid) {
            return invalid_row(csv_row, format!("duplicate TitleId {}", row.titleid));
        }

        let name = resolve_required_string(table, row.name, csv_row, "Name")?;
        if name.trim().is_empty() {
            return invalid_row(csv_row, "missing display Name");
        }

        let title_type = resolve_required_string(table, row.titletype, csv_row, "TitleType")?;
        if !ALLOWED_TITLE_TYPES.contains(&title_type) {
            return invalid_row(
                csv_row,
                format!(
                    "invalid TitleType `{title_type}`; allowed values are {}",
                    ALLOWED_TITLE_TYPES.join(",")
                ),
            );
        }

        validate_json_field(table, row.unlockrules, csv_row, "UnlockRules", false, false)?;
        validate_json_field(table, row.effects, csv_row, "Effects", true, true)?;
    }

    Ok(())
}

fn validate_json_field(
    table: &TitleTable,
    key: u32,
    csv_row: usize,
    field_name: &'static str,
    allow_empty: bool,
    reject_combat_effects: bool,
) -> Result<(), CsvLoadError> {
    let raw = resolve_required_string(table, key, csv_row, field_name)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        if allow_empty {
            return Ok(());
        }
        return invalid_row(csv_row, format!("field `{field_name}` must be valid JSON"));
    }

    let value = serde_json::from_str::<Value>(trimmed).map_err(|error| {
        CsvLoadError::InvalidRow(format!(
            "table TitleTable row {csv_row} field `{field_name}` invalid JSON: {error}"
        ))
    })?;

    if reject_combat_effects {
        reject_combat_effect_value(&value, csv_row, field_name)?;
    }

    Ok(())
}

fn reject_combat_effect_value(
    value: &Value,
    csv_row: usize,
    field_name: &'static str,
) -> Result<(), CsvLoadError> {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                let normalized = normalize_effect_key(key);
                if COMBAT_EFFECT_KEYS.contains(&normalized.as_str()) {
                    return invalid_row(
                        csv_row,
                        format!("field `{field_name}` contains combat effect key `{key}`"),
                    );
                }
                reject_combat_effect_value(nested, csv_row, field_name)?;
            }
        }
        Value::Array(values) => {
            for nested in values {
                reject_combat_effect_value(nested, csv_row, field_name)?;
            }
        }
        Value::String(text) => {
            let normalized = normalize_effect_key(text);
            if COMBAT_EFFECT_KEYS.contains(&normalized.as_str()) {
                return invalid_row(
                    csv_row,
                    format!("field `{field_name}` contains combat effect marker `{text}`"),
                );
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }

    Ok(())
}

fn resolve_required_string<'a>(
    table: &'a TitleTable,
    key: u32,
    csv_row: usize,
    field_name: &'static str,
) -> Result<&'a str, CsvLoadError> {
    table.resolve_string(key).ok_or_else(|| {
        CsvLoadError::InvalidRow(format!(
            "table TitleTable row {csv_row} field `{field_name}` references missing string key {key}"
        ))
    })
}

fn normalize_effect_key(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn invalid_row<T>(csv_row: usize, message: impl Into<String>) -> Result<T, CsvLoadError> {
    Err(CsvLoadError::InvalidRow(format!(
        "table TitleTable row {csv_row}: {}",
        message.into()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_table::CsvTableLoader;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn title_table_accepts_supported_minimal_types() {
        let fixture = TempCsvFile::new(
            "TitleId,Name,Description,TitleType,SourceDomainId,TierRequired,UnlockRules,Effects,Rarity,Icon,Color,Tags,Hidden,Limited,SortOrder\n\
             int,string,string,string,string,string,string,string,string,string,string,Array<string>,int,int,int\n\
             1,身份称号,,identity,,,{}, {},common,icon,#fff,identity,0,0,1\n\
             2,职业称号,,discipline,forging,novice,\"{\"\"discipline\"\":\"\"forging\"\"}\",\"{\"\"display_badge\"\":\"\"forging\"\"}\",common,icon,#fff,discipline,0,0,2\n\
             3,活动称号,,event,event_a,,{}, {},rare,icon,#fff,event,0,0,3\n\
             4,荣誉称号,,honor,arena,,{}, {},rare,icon,#fff,honor,0,0,4\n\
             5,GM称号,,gm,gm,,{}, {},epic,icon,#fff,gm,1,0,5\n\
             6,系统称号,,system,system,,{}, {},epic,icon,#fff,system,1,0,6\n",
        );

        let table = TitleTable::load_from_csv(fixture.path()).expect("csv should load");
        validate_title_table(&table).expect("title table should validate");
    }

    #[test]
    fn title_table_accepts_base_sample_titles() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("csv/TitleTable.csv");
        let table = TitleTable::load_from_csv(&path).expect("sample TitleTable.csv should load");
        validate_title_table(&table).expect("sample TitleTable.csv should validate");

        for title_id in [1001, 2001, 3001, 9001] {
            assert!(
                table.get(title_id).is_some(),
                "sample TitleTable.csv should include title {title_id}"
            );
        }
    }

    #[test]
    fn title_table_rejects_duplicate_title_id() {
        let fixture = TempCsvFile::new(
            "TitleId,Name,Description,TitleType,SourceDomainId,TierRequired,UnlockRules,Effects,Rarity,Icon,Color,Tags,Hidden,Limited,SortOrder\n\
             int,string,string,string,string,string,string,string,string,string,string,Array<string>,int,int,int\n\
             1,测试称号A,,identity,,,{}, {},common,icon,#fff,test,0,0,1\n\
             1,测试称号B,,identity,,,{}, {},common,icon,#fff,test,0,0,2\n",
        );

        let error = TitleTable::load_from_csv(fixture.path())
            .expect_err("duplicate TitleId should be rejected during csv load");
        assert!(
            error.to_string().contains("duplicate id 1"),
            "error `{error}` should mention duplicate id"
        );
    }

    #[test]
    fn title_table_rejects_missing_name() {
        assert_invalid(
            "TitleId,Name,Description,TitleType,SourceDomainId,TierRequired,UnlockRules,Effects,Rarity,Icon,Color,Tags,Hidden,Limited,SortOrder\n\
             int,string,string,string,string,string,string,string,string,string,string,Array<string>,int,int,int\n\
             1,,,identity,,,{}, {},common,icon,#fff,identity,0,0,1\n",
            "missing display Name",
        );
    }

    #[test]
    fn title_table_rejects_invalid_title_type() {
        assert_invalid(
            "TitleId,Name,Description,TitleType,SourceDomainId,TierRequired,UnlockRules,Effects,Rarity,Icon,Color,Tags,Hidden,Limited,SortOrder\n\
             int,string,string,string,string,string,string,string,string,string,string,Array<string>,int,int,int\n\
             1,测试称号,,combat,,,{}, {},common,icon,#fff,test,0,0,1\n",
            "invalid TitleType",
        );
    }

    #[test]
    fn title_table_rejects_invalid_json() {
        assert_invalid(
            "TitleId,Name,Description,TitleType,SourceDomainId,TierRequired,UnlockRules,Effects,Rarity,Icon,Color,Tags,Hidden,Limited,SortOrder\n\
             int,string,string,string,string,string,string,string,string,string,string,Array<string>,int,int,int\n\
             1,测试称号,,identity,,,{bad}, {},common,icon,#fff,test,0,0,1\n",
            "invalid JSON",
        );
    }

    #[test]
    fn title_table_rejects_combat_effects() {
        assert_invalid(
            "TitleId,Name,Description,TitleType,SourceDomainId,TierRequired,UnlockRules,Effects,Rarity,Icon,Color,Tags,Hidden,Limited,SortOrder\n\
             int,string,string,string,string,string,string,string,string,string,string,Array<string>,int,int,int\n\
             1,测试称号,,identity,,,{}, \"{\"\"attack\"\":10}\",common,icon,#fff,test,0,0,1\n",
            "combat effect",
        );
    }

    fn assert_invalid(contents: &str, expected: &str) {
        let fixture = TempCsvFile::new(contents);
        let table = TitleTable::load_from_csv(fixture.path()).expect("csv should parse");
        let error = validate_title_table(&table).expect_err("title table should be invalid");
        assert!(
            error.to_string().contains(expected),
            "error `{error}` should contain `{expected}`"
        );
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
                "game-server-title-table-test-{}-{unique}.csv",
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
