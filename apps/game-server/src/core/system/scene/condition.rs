use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::core::character_discipline::CharacterDiscipline;
use crate::core::character_element::{CharacterElements, ElementValues};
use crate::core::character_title::CharacterTitle;
use crate::core::inventory::item::ItemElementValues;
use crate::core::inventory::player_data::PlayerData;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneCharacterState {
    pub character_id: String,
    pub elements: CharacterElements,
    pub disciplines: Vec<CharacterDiscipline>,
    pub titles: Vec<CharacterTitle>,
    pub item_growth: Vec<SceneItemGrowthState>,
    pub completed_progress: BTreeSet<String>,
    pub quest_states: BTreeMap<String, String>,
    pub organization_states: BTreeMap<String, String>,
    pub regional_reputation: BTreeMap<String, i32>,
    pub world_events: BTreeMap<String, String>,
}

impl SceneCharacterState {
    pub fn new(elements: CharacterElements) -> Self {
        Self {
            character_id: elements.character_id.clone(),
            elements,
            disciplines: Vec::new(),
            titles: Vec::new(),
            item_growth: Vec::new(),
            completed_progress: BTreeSet::new(),
            quest_states: BTreeMap::new(),
            organization_states: BTreeMap::new(),
            regional_reputation: BTreeMap::new(),
            world_events: BTreeMap::new(),
        }
    }

    pub fn with_player_progress(mut self, player_data: &PlayerData) -> Self {
        self.completed_progress
            .extend(player_data.progress.completed.keys().cloned());
        self.item_growth = collect_item_growth(player_data);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneItemGrowthState {
    pub item_id: i32,
    pub growth_elements: ItemElementValues,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneCondition {
    Always,
    AllOf(Vec<SceneCondition>),
    AnyOf(Vec<SceneCondition>),
    Affinity {
        element: SceneElementKind,
        min: i32,
    },
    Mastery {
        element: SceneElementKind,
        min: i32,
    },
    Discipline {
        discipline_id: String,
        active: Option<bool>,
    },
    DisciplineTier {
        discipline_id: String,
        tier: String,
    },
    Title {
        title_id: String,
    },
    ItemGrowth {
        item_id: i32,
        element: SceneElementKind,
        min: i32,
    },
    Progress {
        progress_id: String,
        status: String,
    },
    Quest {
        quest_id: String,
        status: String,
    },
    Organization {
        organization_id: String,
        rank: Option<String>,
    },
    Reputation {
        region_id: String,
        min: i32,
    },
    WorldEvent {
        event_id: String,
        status: String,
    },
    Unsupported {
        condition_type: String,
    },
}

impl SceneCondition {
    pub fn parse(raw: &str) -> Result<Self, SceneConditionError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("always") {
            return Ok(Self::Always);
        }
        let value = serde_json::from_str::<Value>(trimmed).map_err(|error| {
            SceneConditionError::InvalidConfig(format!("invalid condition JSON: {error}"))
        })?;
        Self::parse_value(&value)
    }

    fn parse_value(value: &Value) -> Result<Self, SceneConditionError> {
        match value {
            Value::String(text) if text.eq_ignore_ascii_case("always") => Ok(Self::Always),
            Value::Array(_) => Self::parse_group(value, true),
            Value::Object(map) => {
                if let Some(all_of) = map.get("all_of").or_else(|| map.get("allOf")) {
                    return Self::parse_group(all_of, true);
                }
                if let Some(any_of) = map.get("any_of").or_else(|| map.get("anyOf")) {
                    return Self::parse_group(any_of, false);
                }

                let condition_type = string_field(value, &["type", "kind", "condition"])
                    .ok_or_else(|| invalid_config("condition requires type/all_of/any_of"))?;
                match condition_type.as_str() {
                    "always" => Ok(Self::Always),
                    "affinity" | "element_affinity" => Ok(Self::Affinity {
                        element: element_field(value)?,
                        min: number_field(value, &["min", "threshold", "value", "required"])
                            .ok_or_else(|| invalid_config("affinity condition requires min"))?,
                    }),
                    "mastery" | "element_mastery" => Ok(Self::Mastery {
                        element: element_field(value)?,
                        min: number_field(value, &["min", "threshold", "value", "required"])
                            .ok_or_else(|| invalid_config("mastery condition requires min"))?,
                    }),
                    "discipline" => Ok(Self::Discipline {
                        discipline_id: string_field(value, &["discipline_id", "discipline"])
                            .ok_or_else(|| {
                                invalid_config("discipline condition requires discipline_id")
                            })?,
                        active: bool_field(value, &["active"]),
                    }),
                    "discipline_tier" => Ok(Self::DisciplineTier {
                        discipline_id: string_field(value, &["discipline_id", "discipline"])
                            .ok_or_else(|| {
                                invalid_config("discipline_tier requires discipline_id")
                            })?,
                        tier: string_field(value, &["tier", "min_tier"])
                            .ok_or_else(|| invalid_config("discipline_tier requires tier"))?,
                    }),
                    "title" => Ok(Self::Title {
                        title_id: string_field_preserve(value, &["title_id", "title"])
                            .ok_or_else(|| invalid_config("title condition requires title_id"))?,
                    }),
                    "item_growth" => Ok(Self::ItemGrowth {
                        item_id: number_field(value, &["item_id", "itemId"])
                            .ok_or_else(|| invalid_config("item_growth requires item_id"))?,
                        element: element_field(value)?,
                        min: number_field(value, &["min", "threshold", "value", "required"])
                            .ok_or_else(|| invalid_config("item_growth requires min"))?,
                    }),
                    "progress" | "task" => Ok(Self::Progress {
                        progress_id: string_field(value, &["progress_id", "progress", "task_id"])
                            .ok_or_else(|| {
                            invalid_config("progress condition requires progress_id")
                        })?,
                        status: string_field(value, &["status"])
                            .unwrap_or_else(|| "completed".to_string()),
                    }),
                    "quest" => Ok(Self::Quest {
                        quest_id: string_field(value, &["quest_id", "quest"])
                            .ok_or_else(|| invalid_config("quest condition requires quest_id"))?,
                        status: string_field(value, &["status"])
                            .unwrap_or_else(|| "completed".to_string()),
                    }),
                    "organization" => Ok(Self::Organization {
                        organization_id: string_field(
                            value,
                            &["organization_id", "organization", "org_id"],
                        )
                        .ok_or_else(|| {
                            invalid_config("organization condition requires organization_id")
                        })?,
                        rank: string_field(value, &["rank", "identity", "status"]),
                    }),
                    "reputation" | "regional_reputation" => Ok(Self::Reputation {
                        region_id: string_field(value, &["region_id", "region", "area_id"])
                            .ok_or_else(|| invalid_config("reputation requires region_id"))?,
                        min: number_field(value, &["min", "threshold", "value", "required"])
                            .ok_or_else(|| invalid_config("reputation requires min"))?,
                    }),
                    "world_event" | "world_state" | "world_status" | "world_flag" => {
                        Ok(Self::WorldEvent {
                            event_id: string_field(value, &["event_id", "event", "key", "flag"])
                                .ok_or_else(|| invalid_config("world_event requires event_id"))?,
                            status: string_field(value, &["status", "state"])
                                .unwrap_or_else(|| "active".to_string()),
                        })
                    }
                    other => Ok(Self::Unsupported {
                        condition_type: other.to_string(),
                    }),
                }
            }
            _ => Err(invalid_config(
                "condition must be a JSON object, array, or `always`",
            )),
        }
    }

    fn parse_group(value: &Value, all_of: bool) -> Result<Self, SceneConditionError> {
        let Some(values) = value.as_array() else {
            return Err(invalid_config("condition group requires array"));
        };
        if values.is_empty() {
            return Err(invalid_config("condition group requires at least one item"));
        }

        let mut conditions = Vec::with_capacity(values.len());
        for nested in values {
            conditions.push(Self::parse_value(nested)?);
        }
        if all_of {
            Ok(Self::AllOf(conditions))
        } else {
            Ok(Self::AnyOf(conditions))
        }
    }

    pub fn evaluate(&self, state: &SceneCharacterState) -> SceneConditionOutcome {
        match self {
            Self::Always => SceneConditionOutcome::matched(),
            Self::AllOf(conditions) => {
                let mut evidence = Vec::new();
                for condition in conditions {
                    let outcome = condition.evaluate(state);
                    match outcome.status {
                        SceneConditionStatus::Matched => evidence.extend(outcome.evidence),
                        SceneConditionStatus::NotMatched | SceneConditionStatus::Unsupported => {
                            return outcome;
                        }
                    }
                }
                SceneConditionOutcome::matched_with(evidence)
            }
            Self::AnyOf(conditions) => {
                let mut first_not_matched = None;
                let mut first_unsupported = None;
                for condition in conditions {
                    let outcome = condition.evaluate(state);
                    match outcome.status {
                        SceneConditionStatus::Matched => return outcome,
                        SceneConditionStatus::NotMatched => {
                            if first_not_matched.is_none() {
                                first_not_matched = Some(outcome);
                            }
                        }
                        SceneConditionStatus::Unsupported => {
                            if first_unsupported.is_none() {
                                first_unsupported = Some(outcome);
                            }
                        }
                    }
                }
                first_unsupported
                    .or(first_not_matched)
                    .unwrap_or_else(|| SceneConditionOutcome::not_matched("any_of"))
            }
            Self::Affinity { element, min } => {
                let current = element.value(state.elements.affinity);
                if current >= *min {
                    SceneConditionOutcome::matched_evidence(format!(
                        "affinity.{}={current}>= {min}",
                        element.as_str()
                    ))
                } else {
                    SceneConditionOutcome::not_matched(format!(
                        "affinity.{} {current} < {min}",
                        element.as_str()
                    ))
                }
            }
            Self::Mastery { element, min } => {
                let current = element.value(state.elements.mastery);
                if current >= *min {
                    SceneConditionOutcome::matched_evidence(format!(
                        "mastery.{}={current}>= {min}",
                        element.as_str()
                    ))
                } else {
                    SceneConditionOutcome::not_matched(format!(
                        "mastery.{} {current} < {min}",
                        element.as_str()
                    ))
                }
            }
            Self::Discipline {
                discipline_id,
                active,
            } => {
                let Some(discipline) = state.disciplines.iter().find(|discipline| {
                    discipline.discipline_id.eq_ignore_ascii_case(discipline_id)
                }) else {
                    return SceneConditionOutcome::not_matched(format!(
                        "discipline {discipline_id} missing"
                    ));
                };
                if active.is_some_and(|required| discipline.active != required) {
                    return SceneConditionOutcome::not_matched(format!(
                        "discipline {discipline_id} active mismatch"
                    ));
                }
                SceneConditionOutcome::matched_evidence(format!("discipline {discipline_id}"))
            }
            Self::DisciplineTier {
                discipline_id,
                tier,
            } => {
                let Some(discipline) = state.disciplines.iter().find(|discipline| {
                    discipline.discipline_id.eq_ignore_ascii_case(discipline_id)
                }) else {
                    return SceneConditionOutcome::not_matched(format!(
                        "discipline {discipline_id} missing"
                    ));
                };
                if discipline_tier_rank(&discipline.tier) >= discipline_tier_rank(tier) {
                    SceneConditionOutcome::matched_evidence(format!(
                        "discipline {discipline_id} tier {} >= {tier}",
                        discipline.tier
                    ))
                } else {
                    SceneConditionOutcome::not_matched(format!(
                        "discipline {discipline_id} tier {} < {tier}",
                        discipline.tier
                    ))
                }
            }
            Self::Title { title_id } => {
                if state
                    .titles
                    .iter()
                    .any(|title| title.title_id == *title_id && !title.expired)
                {
                    SceneConditionOutcome::matched_evidence(format!("title {title_id}"))
                } else {
                    SceneConditionOutcome::not_matched(format!("title {title_id} missing"))
                }
            }
            Self::ItemGrowth {
                item_id,
                element,
                min,
            } => {
                let matched = state.item_growth.iter().any(|item| {
                    item.item_id == *item_id && element.item_value(item.growth_elements) >= *min
                });
                if matched {
                    SceneConditionOutcome::matched_evidence(format!(
                        "item_growth {item_id}.{} >= {min}",
                        element.as_str()
                    ))
                } else {
                    SceneConditionOutcome::not_matched(format!(
                        "item_growth {item_id}.{} < {min}",
                        element.as_str()
                    ))
                }
            }
            Self::Progress {
                progress_id,
                status,
            } => {
                if status == "completed" && state.completed_progress.contains(progress_id) {
                    SceneConditionOutcome::matched_evidence(format!("progress {progress_id}"))
                } else {
                    SceneConditionOutcome::not_matched(format!(
                        "progress {progress_id} status {status} not satisfied"
                    ))
                }
            }
            Self::Quest { quest_id, status } => match state.quest_states.get(quest_id) {
                Some(current) if current == status => {
                    SceneConditionOutcome::matched_evidence(format!("quest {quest_id}={status}"))
                }
                Some(current) => SceneConditionOutcome::not_matched(format!(
                    "quest {quest_id} status {current} != {status}"
                )),
                None => SceneConditionOutcome::unsupported(format!("quest:{quest_id}")),
            },
            Self::Organization {
                organization_id,
                rank,
            } => match state.organization_states.get(organization_id) {
                Some(current)
                    if rank
                        .as_ref()
                        .is_none_or(|required| current.eq_ignore_ascii_case(required)) =>
                {
                    SceneConditionOutcome::matched_evidence(format!(
                        "organization {organization_id}={current}"
                    ))
                }
                Some(current) => SceneConditionOutcome::not_matched(format!(
                    "organization {organization_id} rank {current} mismatch"
                )),
                None => {
                    SceneConditionOutcome::unsupported(format!("organization:{organization_id}"))
                }
            },
            Self::Reputation { region_id, min } => match state.regional_reputation.get(region_id) {
                Some(current) if *current >= *min => SceneConditionOutcome::matched_evidence(
                    format!("reputation {region_id}={current}>= {min}"),
                ),
                Some(current) => SceneConditionOutcome::not_matched(format!(
                    "reputation {region_id} {current} < {min}"
                )),
                None => SceneConditionOutcome::unsupported(format!("reputation:{region_id}")),
            },
            Self::WorldEvent { event_id, status } => match state.world_events.get(event_id) {
                Some(current) if current == status => SceneConditionOutcome::matched_evidence(
                    format!("world_event {event_id}={status}"),
                ),
                Some(current) => SceneConditionOutcome::not_matched(format!(
                    "world_event {event_id} status {current} != {status}"
                )),
                None => SceneConditionOutcome::unsupported(format!("world_event:{event_id}")),
            },
            Self::Unsupported { condition_type } => {
                SceneConditionOutcome::unsupported(condition_type.clone())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneConditionStatus {
    Matched,
    NotMatched,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneConditionOutcome {
    pub status: SceneConditionStatus,
    pub reason: Option<String>,
    pub evidence: Vec<String>,
}

impl SceneConditionOutcome {
    pub fn matched() -> Self {
        Self {
            status: SceneConditionStatus::Matched,
            reason: None,
            evidence: Vec::new(),
        }
    }

    pub fn matched_with(evidence: Vec<String>) -> Self {
        Self {
            status: SceneConditionStatus::Matched,
            reason: None,
            evidence,
        }
    }

    pub fn matched_evidence(evidence: impl Into<String>) -> Self {
        Self::matched_with(vec![evidence.into()])
    }

    pub fn not_matched(reason: impl Into<String>) -> Self {
        Self {
            status: SceneConditionStatus::NotMatched,
            reason: Some(reason.into()),
            evidence: Vec::new(),
        }
    }

    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            status: SceneConditionStatus::Unsupported,
            reason: Some(reason.into()),
            evidence: Vec::new(),
        }
    }

    pub fn matched_bool(&self) -> bool {
        matches!(self.status, SceneConditionStatus::Matched)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SceneConditionError {
    InvalidConfig(String),
}

impl std::fmt::Display for SceneConditionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for SceneConditionError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneElementKind {
    Earth,
    Fire,
    Water,
    Wind,
}

impl SceneElementKind {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "earth" => Some(Self::Earth),
            "fire" => Some(Self::Fire),
            "water" => Some(Self::Water),
            "wind" => Some(Self::Wind),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Earth => "earth",
            Self::Fire => "fire",
            Self::Water => "water",
            Self::Wind => "wind",
        }
    }

    pub fn value(self, values: ElementValues) -> i32 {
        match self {
            Self::Earth => values.earth,
            Self::Fire => values.fire,
            Self::Water => values.water,
            Self::Wind => values.wind,
        }
    }

    pub fn item_value(self, values: ItemElementValues) -> i32 {
        match self {
            Self::Earth => values.earth,
            Self::Fire => values.fire,
            Self::Water => values.water,
            Self::Wind => values.wind,
        }
    }
}

fn collect_item_growth(player_data: &PlayerData) -> Vec<SceneItemGrowthState> {
    let mut values = Vec::new();
    values.extend(
        player_data
            .get_inventory_items()
            .into_iter()
            .map(|item| SceneItemGrowthState {
                item_id: item.item_id,
                growth_elements: item.growth_elements,
            }),
    );
    values.extend(
        player_data
            .get_warehouse_items()
            .into_iter()
            .map(|item| SceneItemGrowthState {
                item_id: item.item_id,
                growth_elements: item.growth_elements,
            }),
    );
    values.extend(
        player_data
            .get_equipped_items()
            .into_iter()
            .map(|(_, item)| SceneItemGrowthState {
                item_id: item.item_id,
                growth_elements: item.growth_elements,
            }),
    );
    values
}

fn element_field(value: &Value) -> Result<SceneElementKind, SceneConditionError> {
    string_field(value, &["element"])
        .and_then(|value| SceneElementKind::parse(&value))
        .or_else(|| element_key(value))
        .ok_or_else(|| invalid_config("condition requires earth/fire/water/wind element"))
}

fn element_key(value: &Value) -> Option<SceneElementKind> {
    let map = value.as_object()?;
    map.keys().find_map(|key| SceneElementKind::parse(key))
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
            } else if let Some(boolean) = value.as_bool() {
                return Some(boolean.to_string());
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

fn bool_field(value: &Value, keys: &[&str]) -> Option<bool> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(value) = map.get(*key).or_else(|| map.get(&to_camel_case(key))) {
            if let Some(boolean) = value.as_bool() {
                return Some(boolean);
            }
            if let Some(text) = value.as_str() {
                match text.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" => return Some(true),
                    "false" | "0" | "no" => return Some(false),
                    _ => {}
                }
            }
        }
    }
    None
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

fn discipline_tier_rank(value: &str) -> i32 {
    match value.trim().to_ascii_lowercase().as_str() {
        "novice" => 0,
        "apprentice" => 1,
        "adept" => 2,
        "expert" => 3,
        "master" => 4,
        "grandmaster" => 5,
        _ => -1,
    }
}

fn invalid_config(message: impl Into<String>) -> SceneConditionError {
    SceneConditionError::InvalidConfig(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::character_discipline::CharacterDiscipline;
    use crate::core::character_element::ElementValues;
    use crate::core::inventory::item::Item;

    fn state() -> SceneCharacterState {
        let elements = CharacterElements {
            character_id: "chr_0000000000001".to_string(),
            affinity: ElementValues::new(2500, 2500, 2500, 2500),
            mastery: ElementValues::new(0, 20, 0, 0),
        };
        let mut state = SceneCharacterState::new(elements);
        state.disciplines.push(CharacterDiscipline {
            character_id: state.character_id.clone(),
            discipline_id: "forging".to_string(),
            points: 100,
            tier: "apprentice".to_string(),
            active: true,
            learned_at: "now".to_string(),
            updated_at: "now".to_string(),
        });
        state.titles.push(CharacterTitle {
            character_id: state.character_id.clone(),
            title_id: "2001".to_string(),
            source_type: "unit".to_string(),
            source_id: None,
            is_equipped: false,
            unlocked_at: "now".to_string(),
            expires_at: None,
            expired: false,
        });
        state
    }

    #[test]
    fn scene_condition_matches_supported_character_state() {
        let condition = SceneCondition::parse(
            r#"{"all_of":[{"type":"affinity","element":"fire","min":2000},{"type":"mastery","element":"fire","min":20},{"type":"discipline_tier","discipline_id":"forging","tier":"novice"},{"type":"title","title_id":"2001"}]}"#,
        )
        .unwrap();

        let outcome = condition.evaluate(&state());

        assert_eq!(outcome.status, SceneConditionStatus::Matched);
        assert!(outcome.evidence.iter().any(|item| item.contains("title")));
    }

    #[test]
    fn scene_condition_rejects_not_matched_without_unsupported_fallback() {
        let condition =
            SceneCondition::parse(r#"{"type":"mastery","element":"wind","min":1}"#).unwrap();

        let outcome = condition.evaluate(&state());

        assert_eq!(outcome.status, SceneConditionStatus::NotMatched);
        assert!(
            outcome
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("mastery.wind"))
        );
    }

    #[test]
    fn missing_external_source_returns_unsupported() {
        let condition =
            SceneCondition::parse(r#"{"type":"organization","organization_id":"guild"}"#).unwrap();

        let outcome = condition.evaluate(&state());

        assert_eq!(outcome.status, SceneConditionStatus::Unsupported);
        assert_eq!(outcome.reason.as_deref(), Some("organization:guild"));
    }

    #[test]
    fn any_of_continues_after_unsupported_and_matches_later_branch() {
        let condition = SceneCondition::parse(
            r#"{"any_of":[{"type":"organization","organization_id":"guild"},{"type":"title","title_id":"2001"}]}"#,
        )
        .unwrap();

        let outcome = condition.evaluate(&state());

        assert_eq!(outcome.status, SceneConditionStatus::Matched);
        assert!(outcome.evidence.iter().any(|item| item.contains("title")));
    }

    #[test]
    fn any_of_returns_unsupported_when_no_branch_matches_and_external_source_is_missing() {
        let condition = SceneCondition::parse(
            r#"{"any_of":[{"type":"mastery","element":"wind","min":1},{"type":"organization","organization_id":"guild"}]}"#,
        )
        .unwrap();

        let outcome = condition.evaluate(&state());

        assert_eq!(outcome.status, SceneConditionStatus::Unsupported);
        assert_eq!(outcome.reason.as_deref(), Some("organization:guild"));
    }

    #[test]
    fn player_data_snapshot_includes_progress_and_item_growth() {
        let mut player_data = PlayerData::new("chr_0000000000001".to_string());
        player_data.progress.completed.insert(
            "quest_1".to_string(),
            crate::core::inventory::player_data::CharacterProgressRecord {
                progress_id: "quest_1".to_string(),
                source_type: "quest".to_string(),
                source_id: "quest_1".to_string(),
                completed_at: "now".to_string(),
            },
        );
        let mut item = Item::new(7, 1002, 1, false);
        item.growth_elements = ItemElementValues::new(0, 1, 0, 0);
        player_data.add_item(item).unwrap();

        let state = state().with_player_progress(&player_data);
        assert!(state.completed_progress.contains("quest_1"));
        assert_eq!(state.item_growth[0].growth_elements.fire, 1);
    }
}
