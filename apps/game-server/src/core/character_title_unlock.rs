#![allow(dead_code)]

use std::sync::Arc;

use serde_json::Value;

use crate::business::character_element::{
    CharacterElementChangeFailure, CharacterElementFacade, CharacterElementSnapshot,
    ElementSnapshot, GetCharacterElements,
};
use crate::core::character_discipline::{CharacterDiscipline, DisciplineError, DisciplineService};
use crate::core::character_title::{
    CharacterTitle, GrantTitleRequest, GrantTitleStatus, TitleError, TitleOperationContext,
    TitleService,
};
use crate::core::config_table::ConfigTableRuntime;
use crate::csv_code::titletable::{TitleTable, TitleTableRow};
use crate::session::AuthenticatedSessionIdentity;

const DISCIPLINE_TIER_ORDER: &[&str] = &[
    "novice",
    "apprentice",
    "adept",
    "expert",
    "master",
    "grandmaster",
];

#[derive(Clone)]
pub struct TitleUnlockService {
    title_service: TitleService,
    discipline_service: DisciplineService,
    character_element_facade: CharacterElementFacade,
    config_source: TitleUnlockConfigSource,
    #[cfg(test)]
    character_element_service: crate::adapters::persistence::InMemoryCharacterElementRepository,
}

impl TitleUnlockService {
    pub fn new(
        title_service: TitleService,
        discipline_service: DisciplineService,
        character_element_facade: CharacterElementFacade,
        config_tables: ConfigTableRuntime,
    ) -> Self {
        Self {
            title_service,
            discipline_service,
            character_element_facade,
            config_source: TitleUnlockConfigSource::Runtime(config_tables),
            #[cfg(test)]
            character_element_service: Default::default(),
        }
    }

    pub async fn check_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
        trigger: TitleUnlockTrigger,
    ) -> Result<TitleUnlockCheckResult, TitleUnlockError> {
        let table = self.config_source.title_table().await;
        let mut result = TitleUnlockCheckResult::default();
        let mut disciplines: Option<Vec<CharacterDiscipline>> = None;
        let mut elements: Option<CharacterElementSnapshot> = None;

        for row in table.all() {
            let title_id = row.titleid.to_string();
            let hidden = row.hidden != 0;
            let raw_rule = resolve_string(&table, row.unlockrules);
            let rule = match parse_unlock_rule(raw_rule.as_deref()) {
                Ok(rule) => rule,
                Err(reason) => {
                    result.skipped.push(TitleUnlockSkip {
                        title_id,
                        hidden,
                        reason,
                    });
                    continue;
                }
            };

            if row.limited != 0 {
                result.skipped.push(TitleUnlockSkip {
                    title_id,
                    hidden,
                    reason: TitleUnlockSkipReason::LimitedRequiresExpiry,
                });
                continue;
            }

            if rule.requires_discipline() && disciplines.is_none() {
                disciplines = Some(
                    self.discipline_service
                        .list_for_identity(identity)
                        .await
                        .map_err(TitleUnlockError::Discipline)?,
                );
            }
            if rule.requires_element() && elements.is_none() {
                elements = Some(
                    self.character_element_facade
                        .get_character_elements(GetCharacterElements::new(
                            identity.character_id.clone(),
                        ))
                        .await
                        .map_err(TitleUnlockError::Element)?
                        .elements()
                        .clone(),
                );
            }

            let evaluation = evaluate_rule(&rule, disciplines.as_deref(), elements.as_ref());
            let matched_source_type = match evaluation {
                RuleEvaluation::Matched { source_type } => source_type,
                RuleEvaluation::Skipped(reason) => {
                    result.skipped.push(TitleUnlockSkip {
                        title_id,
                        hidden,
                        reason,
                    });
                    continue;
                }
            };

            let context = build_operation_context(
                &trigger,
                &matched_source_type,
                row,
                &table,
                "title_unlock_check",
            );
            let grant = self
                .title_service
                .grant_for_identity(identity, GrantTitleRequest::new(title_id.clone()), context)
                .await
                .map_err(TitleUnlockError::Title)?;

            match grant.status {
                GrantTitleStatus::Granted | GrantTitleStatus::Renewed => {
                    result.unlocked.push(TitleUnlockGrant {
                        title_id,
                        hidden,
                        status: grant.status,
                        source_type: grant.title.source_type.clone(),
                        title: grant.title,
                    });
                }
                GrantTitleStatus::AlreadyOwned => {
                    result.skipped.push(TitleUnlockSkip {
                        title_id,
                        hidden,
                        reason: TitleUnlockSkipReason::AlreadyOwned,
                    });
                }
            }
        }

        Ok(result)
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(
        title_service: TitleService,
        discipline_service: DisciplineService,
        character_element_facade: CharacterElementFacade,
        character_element_service: crate::adapters::persistence::InMemoryCharacterElementRepository,
        title_table: Arc<TitleTable>,
    ) -> Self {
        Self {
            title_service,
            discipline_service,
            character_element_facade,
            config_source: TitleUnlockConfigSource::Static(title_table),
            character_element_service,
        }
    }
}

#[derive(Clone)]
enum TitleUnlockConfigSource {
    Runtime(ConfigTableRuntime),
    #[cfg(test)]
    Static(Arc<TitleTable>),
}

impl TitleUnlockConfigSource {
    async fn title_table(&self) -> Arc<TitleTable> {
        match self {
            Self::Runtime(runtime) => runtime.tables_snapshot().await.titletable.clone(),
            #[cfg(test)]
            Self::Static(table) => table.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleUnlockTrigger {
    Manual,
    Discipline { discipline_id: Option<String> },
    Element,
    Gm { operator_id: Option<String> },
    System,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TitleUnlockCheckResult {
    pub unlocked: Vec<TitleUnlockGrant>,
    pub skipped: Vec<TitleUnlockSkip>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleUnlockGrant {
    pub title_id: String,
    pub hidden: bool,
    pub status: GrantTitleStatus,
    pub source_type: String,
    pub title: CharacterTitle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleUnlockSkip {
    pub title_id: String,
    pub hidden: bool,
    pub reason: TitleUnlockSkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleUnlockSkipReason {
    ManualRule,
    LimitedRequiresExpiry,
    AlreadyOwned,
    RuleNotMatched,
    UnsupportedRule { rule_type: String },
    InvalidUnlockRule { message: String },
}

impl TitleUnlockSkipReason {
    pub fn code(&self) -> &'static str {
        match self {
            Self::ManualRule => "manual_rule",
            Self::LimitedRequiresExpiry => "limited_requires_expiry",
            Self::AlreadyOwned => "already_owned",
            Self::RuleNotMatched => "rule_not_matched",
            Self::UnsupportedRule { .. } => "unsupported_rule",
            Self::InvalidUnlockRule { .. } => "invalid_unlock_rule",
        }
    }
}

#[derive(Debug)]
pub enum TitleUnlockError {
    Title(TitleError),
    Discipline(DisciplineError),
    Element(CharacterElementChangeFailure),
}

impl TitleUnlockError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::Title(error) => error.error_code(),
            Self::Discipline(error) => error.error_code(),
            Self::Element(error) => error.error_code(),
        }
    }
}

impl std::fmt::Display for TitleUnlockError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Title(error) => write!(formatter, "title unlock grant failed: {error}"),
            Self::Discipline(error) => {
                write!(formatter, "title unlock discipline check failed: {error}")
            }
            Self::Element(error) => write!(
                formatter,
                "title unlock element check failed: {}",
                error.error_code()
            ),
        }
    }
}

impl std::error::Error for TitleUnlockError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UnlockRule {
    Manual,
    DisciplineTier { discipline_id: String, tier: String },
    ElementMastery { element: ElementKind, min: i32 },
    ElementAffinity { element: ElementKind, min: i32 },
    AllOf(Vec<UnlockRule>),
    Unsupported { rule_type: String },
}

impl UnlockRule {
    fn requires_discipline(&self) -> bool {
        match self {
            Self::DisciplineTier { .. } => true,
            Self::AllOf(rules) => rules.iter().any(Self::requires_discipline),
            Self::Manual
            | Self::ElementMastery { .. }
            | Self::ElementAffinity { .. }
            | Self::Unsupported { .. } => false,
        }
    }

    fn requires_element(&self) -> bool {
        match self {
            Self::ElementMastery { .. } | Self::ElementAffinity { .. } => true,
            Self::AllOf(rules) => rules.iter().any(Self::requires_element),
            Self::Manual | Self::DisciplineTier { .. } | Self::Unsupported { .. } => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementKind {
    Earth,
    Fire,
    Water,
    Wind,
}

impl ElementKind {
    fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "earth" => Some(Self::Earth),
            "fire" => Some(Self::Fire),
            "water" => Some(Self::Water),
            "wind" => Some(Self::Wind),
            _ => None,
        }
    }

    fn value_from(self, values: ElementSnapshot) -> i32 {
        match self {
            Self::Earth => values.earth(),
            Self::Fire => values.fire(),
            Self::Water => values.water(),
            Self::Wind => values.wind(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuleEvaluation {
    Matched { source_type: String },
    Skipped(TitleUnlockSkipReason),
}

fn parse_unlock_rule(raw: Option<&str>) -> Result<UnlockRule, TitleUnlockSkipReason> {
    let Some(raw) = raw else {
        return Err(invalid_rule("missing UnlockRules string"));
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(invalid_rule("empty UnlockRules"));
    }

    let value = serde_json::from_str::<Value>(trimmed)
        .map_err(|error| invalid_rule(format!("invalid JSON: {error}")))?;
    parse_rule_value(&value)
}

fn parse_rule_value(value: &Value) -> Result<UnlockRule, TitleUnlockSkipReason> {
    match value {
        Value::String(text) if text.eq_ignore_ascii_case("manual") => Ok(UnlockRule::Manual),
        Value::String(text) => Ok(UnlockRule::Unsupported {
            rule_type: text.to_string(),
        }),
        Value::Object(map) => {
            if let Some(all_of) = map.get("all_of").or_else(|| map.get("allOf")) {
                return parse_all_of(all_of);
            }
            if map.get("manual").and_then(Value::as_bool) == Some(true) {
                return Ok(UnlockRule::Manual);
            }
            if let Some(grant) = string_field(value, &["grant"]) {
                if matches!(grant.as_str(), "manual" | "gm" | "system") {
                    return Ok(UnlockRule::Manual);
                }
            }

            if let Some(nested) = map.get("element_mastery") {
                return parse_element_rule("element_mastery", nested);
            }
            if let Some(nested) = map.get("element_affinity") {
                return parse_element_rule("element_affinity", nested);
            }

            let rule_type = string_field(value, &["type", "rule", "kind", "event"])
                .or_else(|| infer_compat_rule_type(value));
            match rule_type.as_deref() {
                Some("manual") => Ok(UnlockRule::Manual),
                Some("all_of") => map
                    .get("rules")
                    .ok_or_else(|| invalid_rule("all_of requires rules array"))
                    .and_then(parse_all_of),
                Some("discipline_tier") => parse_discipline_tier_rule(value),
                Some("element_mastery") => parse_element_rule("element_mastery", value),
                Some("element_affinity") => parse_element_rule("element_affinity", value),
                Some(rule_type) => Ok(UnlockRule::Unsupported {
                    rule_type: rule_type.to_string(),
                }),
                None => Ok(UnlockRule::Unsupported {
                    rule_type: "unknown".to_string(),
                }),
            }
        }
        _ => Err(invalid_rule("UnlockRules must be an object or string")),
    }
}

fn parse_all_of(value: &Value) -> Result<UnlockRule, TitleUnlockSkipReason> {
    let Some(values) = value.as_array() else {
        return Err(invalid_rule("all_of requires an array"));
    };
    if values.is_empty() {
        return Err(invalid_rule("all_of requires at least one rule"));
    }
    let mut rules = Vec::with_capacity(values.len());
    for nested in values {
        rules.push(parse_rule_value(nested)?);
    }
    Ok(UnlockRule::AllOf(rules))
}

fn parse_discipline_tier_rule(value: &Value) -> Result<UnlockRule, TitleUnlockSkipReason> {
    let discipline_id = string_field(value, &["discipline_id", "discipline", "source_domain_id"])
        .ok_or_else(|| invalid_rule("discipline_tier requires discipline_id"))?;
    let tier = string_field(value, &["tier", "tier_required", "min_tier"])
        .ok_or_else(|| invalid_rule("discipline_tier requires tier"))?;
    Ok(UnlockRule::DisciplineTier {
        discipline_id,
        tier,
    })
}

fn parse_element_rule(rule_type: &str, value: &Value) -> Result<UnlockRule, TitleUnlockSkipReason> {
    let element = string_field(value, &["element"])
        .and_then(|value| ElementKind::parse(&value))
        .or_else(|| element_key(value))
        .ok_or_else(|| invalid_rule(format!("{rule_type} requires earth/fire/water/wind")))?;
    let min = number_field(value, &["min", "threshold", "value", "required", "amount"])
        .or_else(|| number_for_element_key(value, element))
        .ok_or_else(|| invalid_rule(format!("{rule_type} requires min threshold")))?;

    match rule_type {
        "element_mastery" => Ok(UnlockRule::ElementMastery { element, min }),
        "element_affinity" => Ok(UnlockRule::ElementAffinity { element, min }),
        _ => Err(invalid_rule(format!(
            "unsupported element rule {rule_type}"
        ))),
    }
}

fn infer_compat_rule_type(value: &Value) -> Option<String> {
    if string_field(value, &["discipline_id", "discipline"]).is_some()
        && string_field(value, &["tier", "tier_required", "min_tier"]).is_some()
    {
        return Some("discipline_tier".to_string());
    }
    None
}

fn evaluate_rule(
    rule: &UnlockRule,
    disciplines: Option<&[CharacterDiscipline]>,
    elements: Option<&CharacterElementSnapshot>,
) -> RuleEvaluation {
    match rule {
        UnlockRule::Manual => RuleEvaluation::Skipped(TitleUnlockSkipReason::ManualRule),
        UnlockRule::Unsupported { rule_type } => {
            RuleEvaluation::Skipped(TitleUnlockSkipReason::UnsupportedRule {
                rule_type: rule_type.clone(),
            })
        }
        UnlockRule::DisciplineTier {
            discipline_id,
            tier,
        } => {
            let matched = disciplines
                .unwrap_or_default()
                .iter()
                .find(|discipline| discipline.discipline_id == *discipline_id)
                .is_some_and(|discipline| discipline_tier_satisfies(&discipline.tier, tier));
            if matched {
                RuleEvaluation::Matched {
                    source_type: "discipline".to_string(),
                }
            } else {
                RuleEvaluation::Skipped(TitleUnlockSkipReason::RuleNotMatched)
            }
        }
        UnlockRule::ElementMastery { element, min } => {
            let matched = elements
                .map(|elements| element.value_from(elements.mastery()) >= *min)
                .unwrap_or(false);
            if matched {
                RuleEvaluation::Matched {
                    source_type: "element".to_string(),
                }
            } else {
                RuleEvaluation::Skipped(TitleUnlockSkipReason::RuleNotMatched)
            }
        }
        UnlockRule::ElementAffinity { element, min } => {
            let matched = elements
                .map(|elements| element.value_from(elements.affinity()) >= *min)
                .unwrap_or(false);
            if matched {
                RuleEvaluation::Matched {
                    source_type: "element".to_string(),
                }
            } else {
                RuleEvaluation::Skipped(TitleUnlockSkipReason::RuleNotMatched)
            }
        }
        UnlockRule::AllOf(rules) => {
            let mut matched_sources = Vec::new();
            for nested in rules {
                match evaluate_rule(nested, disciplines, elements) {
                    RuleEvaluation::Matched { source_type } => matched_sources.push(source_type),
                    RuleEvaluation::Skipped(reason) => return RuleEvaluation::Skipped(reason),
                }
            }
            let first_source = matched_sources
                .first()
                .cloned()
                .unwrap_or_else(|| "system".to_string());
            let source_type = if matched_sources
                .iter()
                .all(|source_type| source_type == &first_source)
            {
                first_source
            } else {
                "system".to_string()
            };
            RuleEvaluation::Matched { source_type }
        }
    }
}

fn discipline_tier_satisfies(current: &str, required: &str) -> bool {
    let current = current.trim().to_ascii_lowercase();
    let required = required.trim().to_ascii_lowercase();
    match (
        DISCIPLINE_TIER_ORDER
            .iter()
            .position(|tier| *tier == current),
        DISCIPLINE_TIER_ORDER
            .iter()
            .position(|tier| *tier == required),
    ) {
        (Some(current), Some(required)) => current >= required,
        _ => current == required,
    }
}

fn build_operation_context(
    trigger: &TitleUnlockTrigger,
    matched_source_type: &str,
    row: &TitleTableRow,
    table: &TitleTable,
    reason: &str,
) -> TitleOperationContext {
    let source_type = match trigger {
        TitleUnlockTrigger::Discipline { .. } => "discipline",
        TitleUnlockTrigger::Element => "element",
        TitleUnlockTrigger::Gm { .. } => "gm",
        TitleUnlockTrigger::System => "system",
        TitleUnlockTrigger::Manual => matched_source_type,
    };
    let source_id = match trigger {
        TitleUnlockTrigger::Discipline { discipline_id } => discipline_id
            .clone()
            .or_else(|| resolve_string(table, row.sourcedomainid))
            .unwrap_or_else(|| row.titleid.to_string()),
        TitleUnlockTrigger::Element => resolve_string(table, row.sourcedomainid)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "element".to_string()),
        TitleUnlockTrigger::Gm { .. } => "title_unlock_gm".to_string(),
        TitleUnlockTrigger::System => "title_unlock_system".to_string(),
        TitleUnlockTrigger::Manual => resolve_string(table, row.sourcedomainid)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| row.titleid.to_string()),
    };

    let mut context = TitleOperationContext::new(source_type)
        .with_source_id(source_id)
        .with_reason(reason);
    if let TitleUnlockTrigger::Gm {
        operator_id: Some(operator_id),
    } = trigger
    {
        context = context.with_operator("gm", operator_id.clone());
    }
    context
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(value) = map.get(*key) {
            if let Some(text) = value.as_str() {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_ascii_lowercase());
                }
            }
        }
    }
    None
}

fn number_field(value: &Value, keys: &[&str]) -> Option<i32> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(value) = map.get(*key).and_then(number_value) {
            return Some(value);
        }
    }
    None
}

fn element_key(value: &Value) -> Option<ElementKind> {
    let map = value.as_object()?;
    map.keys().find_map(|key| ElementKind::parse(key))
}

fn number_for_element_key(value: &Value, element: ElementKind) -> Option<i32> {
    let map = value.as_object()?;
    for key in ["earth", "fire", "water", "wind"] {
        if ElementKind::parse(key) == Some(element) {
            return map.get(key).and_then(number_value);
        }
    }
    None
}

fn number_value(value: &Value) -> Option<i32> {
    if let Some(number) = value.as_i64() {
        return i32::try_from(number).ok();
    }
    value
        .as_str()
        .and_then(|text| text.trim().parse::<i32>().ok())
}

fn invalid_rule(message: impl Into<String>) -> TitleUnlockSkipReason {
    TitleUnlockSkipReason::InvalidUnlockRule {
        message: message.into(),
    }
}

fn resolve_string(table: &TitleTable, key: u32) -> Option<String> {
    table.resolve_string(key).map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::business::character_element::{
        CharacterElementFacade, CharacterElements, ElementValues,
    };
    use crate::core::character_discipline::DisciplineUpsert;
    use crate::csv_code::titletable::{StringKey, TitleTableRow};
    use std::collections::HashMap;

    fn identity() -> AuthenticatedSessionIdentity {
        AuthenticatedSessionIdentity {
            account_player_id: "plr_0000000000001".to_string(),
            character_id: "chr_0000000000001".to_string(),
            world_id: Some(0),
        }
    }

    fn title_table(rows: Vec<TestTitleRow>) -> Arc<TitleTable> {
        let mut string_pool = HashMap::new();
        let mut next_key: StringKey = 1;
        let mut intern = |value: &str, string_pool: &mut HashMap<StringKey, String>| {
            let key = next_key;
            next_key = next_key.saturating_add(1);
            string_pool.insert(key, value.to_string());
            key
        };

        let rows = rows
            .into_iter()
            .map(|row| TitleTableRow {
                titleid: row.title_id,
                name: intern("unit title", &mut string_pool),
                titletype: intern(row.title_type, &mut string_pool),
                sourcedomainid: intern(row.source_domain_id, &mut string_pool),
                tierrequired: intern(row.tier_required, &mut string_pool),
                unlockrules: intern(row.unlock_rules, &mut string_pool),
                hidden: i32::from(row.hidden),
                limited: i32::from(row.limited),
                ..TitleTableRow::default()
            })
            .collect::<Vec<_>>();
        let by_id = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.titleid, index))
            .collect();
        Arc::new(TitleTable {
            string_pool,
            rows,
            by_id,
        })
    }

    struct TestTitleRow {
        title_id: i32,
        title_type: &'static str,
        source_domain_id: &'static str,
        tier_required: &'static str,
        unlock_rules: &'static str,
        hidden: bool,
        limited: bool,
    }

    impl TestTitleRow {
        fn new(title_id: i32, unlock_rules: &'static str) -> Self {
            Self {
                title_id,
                title_type: "system",
                source_domain_id: "unit",
                tier_required: "",
                unlock_rules,
                hidden: false,
                limited: false,
            }
        }

        fn title_type(mut self, title_type: &'static str) -> Self {
            self.title_type = title_type;
            self
        }

        fn source_domain_id(mut self, source_domain_id: &'static str) -> Self {
            self.source_domain_id = source_domain_id;
            self
        }

        fn hidden(mut self) -> Self {
            self.hidden = true;
            self
        }

        fn limited(mut self) -> Self {
            self.limited = true;
            self
        }
    }

    async fn service_fixture(rows: Vec<TestTitleRow>) -> TitleUnlockService {
        let table = title_table(rows);
        let title_service = TitleService::new_in_memory(table.clone());
        let discipline_service = DisciplineService::new_in_memory();
        let character_element_service =
            crate::adapters::persistence::InMemoryCharacterElementRepository::default();
        let character_element_facade =
            CharacterElementFacade::new(Arc::new(character_element_service.clone()));
        TitleUnlockService::new_for_test(
            title_service,
            discipline_service,
            character_element_facade,
            character_element_service,
            table,
        )
    }

    fn skip_reason(result: &TitleUnlockCheckResult, title_id: &str) -> TitleUnlockSkipReason {
        result
            .skipped
            .iter()
            .find(|skip| skip.title_id == title_id)
            .map(|skip| skip.reason.clone())
            .expect("skip reason should exist")
    }

    #[tokio::test]
    async fn manual_rules_are_not_auto_granted() {
        let service = service_fixture(vec![TestTitleRow::new(1001, r#"{"type":"manual"}"#)]).await;

        let result = service
            .check_for_identity(&identity(), TitleUnlockTrigger::System)
            .await
            .unwrap();

        assert!(result.unlocked.is_empty());
        assert_eq!(
            skip_reason(&result, "1001"),
            TitleUnlockSkipReason::ManualRule
        );
    }

    #[tokio::test]
    async fn discipline_tier_rule_supports_compatible_csv_shape() {
        let service = service_fixture(vec![
            TestTitleRow::new(2001, r#"{"discipline":"forging","tier":"novice"}"#)
                .title_type("discipline")
                .source_domain_id("forging"),
        ])
        .await;
        let identity = identity();
        service
            .discipline_service
            .upsert_for_identity(
                &identity,
                DisciplineUpsert::new("forging", 10, "apprentice", true),
            )
            .await
            .unwrap();

        let result = service
            .check_for_identity(
                &identity,
                TitleUnlockTrigger::Discipline {
                    discipline_id: Some("forging".to_string()),
                },
            )
            .await
            .unwrap();

        assert_eq!(result.unlocked.len(), 1);
        assert_eq!(result.unlocked[0].title_id, "2001");
        assert_eq!(result.unlocked[0].status, GrantTitleStatus::Granted);
        assert_eq!(result.unlocked[0].source_type, "discipline");
    }

    #[tokio::test]
    async fn element_mastery_and_affinity_rules_check_thresholds() {
        let service = service_fixture(vec![
            TestTitleRow::new(
                3001,
                r#"{"type":"element_mastery","element":"fire","min":50}"#,
            ),
            TestTitleRow::new(3002, r#"{"type":"element_affinity","water":3000}"#),
            TestTitleRow::new(3003, r#"{"type":"element_mastery","wind":999}"#),
        ])
        .await;
        let identity = identity();
        service
            .character_element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2000, 2500, 3000, 2500),
                mastery: ElementValues::new(10, 60, 20, 30),
            })
            .await;

        let result = service
            .check_for_identity(&identity, TitleUnlockTrigger::Element)
            .await
            .unwrap();

        let unlocked = result
            .unlocked
            .iter()
            .map(|grant| grant.title_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(unlocked, vec!["3001", "3002"]);
        assert_eq!(
            skip_reason(&result, "3003"),
            TitleUnlockSkipReason::RuleNotMatched
        );
    }

    #[tokio::test]
    async fn all_of_requires_all_nested_rules() {
        let service = service_fixture(vec![TestTitleRow::new(
            4001,
            r#"{"type":"all_of","rules":[{"discipline_id":"alchemy","tier":"adept"},{"type":"element_mastery","element":"earth","min":20}]}"#,
        )])
        .await;
        let identity = identity();
        service
            .discipline_service
            .upsert_for_identity(
                &identity,
                DisciplineUpsert::new("alchemy", 30, "adept", true),
            )
            .await
            .unwrap();
        service
            .character_element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::new(20, 0, 0, 0),
            })
            .await;

        let result = service
            .check_for_identity(&identity, TitleUnlockTrigger::System)
            .await
            .unwrap();

        assert_eq!(result.unlocked.len(), 1);
        assert_eq!(result.unlocked[0].title_id, "4001");
        assert_eq!(result.unlocked[0].source_type, "system");
    }

    #[tokio::test]
    async fn hidden_limited_and_already_owned_behaviors_are_explicit() {
        let service = service_fixture(vec![
            TestTitleRow::new(
                5001,
                r#"{"type":"element_affinity","element":"earth","min":2500}"#,
            )
            .hidden(),
            TestTitleRow::new(
                5002,
                r#"{"type":"element_affinity","element":"earth","min":2500}"#,
            )
            .limited(),
        ])
        .await;
        let identity = identity();
        service
            .character_element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::zero(),
            })
            .await;

        let first = service
            .check_for_identity(&identity, TitleUnlockTrigger::Element)
            .await
            .unwrap();
        assert_eq!(first.unlocked.len(), 1);
        assert!(first.unlocked[0].hidden);
        assert_eq!(
            skip_reason(&first, "5002"),
            TitleUnlockSkipReason::LimitedRequiresExpiry
        );

        let second = service
            .check_for_identity(&identity, TitleUnlockTrigger::Element)
            .await
            .unwrap();
        assert_eq!(second.unlocked.len(), 0);
        assert_eq!(
            skip_reason(&second, "5001"),
            TitleUnlockSkipReason::AlreadyOwned
        );
    }

    #[tokio::test]
    async fn expired_title_is_renewed_and_source_type_is_preserved() {
        let service = service_fixture(vec![TestTitleRow::new(
            6001,
            r#"{"type":"element_mastery","element":"fire","min":1}"#,
        )])
        .await;
        let identity = identity();
        service
            .character_element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::new(0, 1, 0, 0),
            })
            .await;

        service
            .title_service
            .grant_for_identity(
                &identity,
                GrantTitleRequest::new("6001"),
                TitleOperationContext::new("gm"),
            )
            .await
            .unwrap();
        service
            .title_service
            .mark_expired_for_test(&identity.character_id, "6001")
            .await;

        let result = service
            .check_for_identity(&identity, TitleUnlockTrigger::Element)
            .await
            .unwrap();

        assert_eq!(result.unlocked.len(), 1);
        assert_eq!(result.unlocked[0].status, GrantTitleStatus::Renewed);
        assert_eq!(result.unlocked[0].source_type, "element");

        let logs = service.title_service.logs().await;
        assert!(
            logs.iter()
                .any(|log| log.source_type.as_deref() == Some("element"))
        );
    }

    #[tokio::test]
    async fn unsupported_rules_return_skip_reason() {
        let service = service_fixture(vec![TestTitleRow::new(
            7001,
            r#"{"event":"wind_canyon_explored"}"#,
        )])
        .await;

        let result = service
            .check_for_identity(&identity(), TitleUnlockTrigger::System)
            .await
            .unwrap();

        assert_eq!(
            skip_reason(&result, "7001"),
            TitleUnlockSkipReason::UnsupportedRule {
                rule_type: "wind_canyon_explored".to_string()
            }
        );
    }
}
