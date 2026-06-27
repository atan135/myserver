#![allow(dead_code)]

use serde_json::Value;

use crate::core::character_discipline::{
    CharacterDiscipline, DisciplineError, DisciplineOperationContext, DisciplineService,
};
use crate::core::character_element::{
    CharacterElementChange, CharacterElementChangeSource, CharacterElementError, ElementDeltas,
    ElementValues,
};
use crate::core::character_title::{
    CharacterTitle, GrantTitleRequest, GrantTitleStatus, TitleError, TitleOperationContext,
    TitleService,
};
use crate::core::inventory::item::ItemElementValues;
use crate::core::inventory::player_data::{
    CharacterProgressRecord, CharacterProgressRewardLog, PlayerData,
};
use crate::csv_code::characterprogresstable::{CharacterProgressTable, CharacterProgressTableRow};
use crate::csv_code::disciplinetable::DisciplineTable;
use crate::session::AuthenticatedSessionIdentity;

const VALID_SOURCE_TYPES: &[&str] = &[
    "task",
    "quest",
    "achievement",
    "activity",
    "ranking",
    "world_event",
];
const DISCIPLINE_TIER_ORDER: &[&str] = &[
    "novice",
    "apprentice",
    "adept",
    "expert",
    "master",
    "grandmaster",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyCharacterProgressRequest {
    pub progress_id: String,
}

impl ApplyCharacterProgressRequest {
    pub fn new(progress_id: impl Into<String>) -> Self {
        Self {
            progress_id: progress_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterProgressOutcome {
    pub applied: bool,
    pub progress_id: String,
    pub source_type: String,
    pub source_id: String,
    pub rewards: Vec<CharacterProgressRewardOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterProgressRewardOutcome {
    pub reward_type: String,
    pub reward_id: String,
    pub status: String,
    pub title: Option<CharacterTitle>,
    pub discipline: Option<CharacterDiscipline>,
    pub eligibility: Option<String>,
}

#[derive(Debug)]
pub enum CharacterProgressError {
    ProgressNotFound,
    ProgressDisabled,
    InvalidProgressId,
    InvalidProgressConfig { message: String },
    ConditionNotMet { reason: String },
    UnsupportedCondition { condition_type: String },
    UnsupportedReward { reward_type: String },
    LimitedTitleRequiresExpiry { title_id: String },
    CharacterElement(CharacterElementError),
    Discipline(DisciplineError),
    Title(TitleError),
}

impl CharacterProgressError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::ProgressNotFound => "CHARACTER_PROGRESS_NOT_FOUND",
            Self::ProgressDisabled => "CHARACTER_PROGRESS_DISABLED",
            Self::InvalidProgressId => "INVALID_CHARACTER_PROGRESS_ID",
            Self::InvalidProgressConfig { .. } => "INVALID_CHARACTER_PROGRESS_CONFIG",
            Self::ConditionNotMet { .. } => "CHARACTER_PROGRESS_CONDITION_NOT_MET",
            Self::UnsupportedCondition { .. } => "UNSUPPORTED_CHARACTER_PROGRESS_CONDITION",
            Self::UnsupportedReward { .. } => "UNSUPPORTED_CHARACTER_PROGRESS_REWARD",
            Self::LimitedTitleRequiresExpiry { .. } => "LIMITED_TITLE_REQUIRES_EXPIRES_AT",
            Self::CharacterElement(error) => error.error_code(),
            Self::Discipline(error) => error.error_code(),
            Self::Title(error) => error.error_code(),
        }
    }
}

impl std::fmt::Display for CharacterProgressError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProgressNotFound => write!(formatter, "character progress not found"),
            Self::ProgressDisabled => write!(formatter, "character progress is disabled"),
            Self::InvalidProgressId => write!(formatter, "progress_id must not be empty"),
            Self::InvalidProgressConfig { message } => {
                write!(formatter, "invalid character progress config: {message}")
            }
            Self::ConditionNotMet { reason } => {
                write!(formatter, "character progress condition not met: {reason}")
            }
            Self::UnsupportedCondition { condition_type } => {
                write!(
                    formatter,
                    "unsupported character progress condition: {condition_type}"
                )
            }
            Self::UnsupportedReward { reward_type } => {
                write!(
                    formatter,
                    "unsupported character progress reward: {reward_type}"
                )
            }
            Self::LimitedTitleRequiresExpiry { title_id } => {
                write!(
                    formatter,
                    "limited title reward {title_id} requires explicit expires_at"
                )
            }
            Self::CharacterElement(error) => write!(formatter, "character element error: {error}"),
            Self::Discipline(error) => write!(formatter, "discipline error: {error}"),
            Self::Title(error) => write!(formatter, "title error: {error}"),
        }
    }
}

impl std::error::Error for CharacterProgressError {}

#[derive(Clone)]
pub struct CharacterProgressService {
    character_element_service: crate::core::character_element::CharacterElementService,
    discipline_service: DisciplineService,
    title_service: TitleService,
}

impl CharacterProgressService {
    pub fn new(
        character_element_service: crate::core::character_element::CharacterElementService,
        discipline_service: DisciplineService,
        title_service: TitleService,
    ) -> Self {
        Self {
            character_element_service,
            discipline_service,
            title_service,
        }
    }

    pub async fn apply_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
        request: ApplyCharacterProgressRequest,
        table: &CharacterProgressTable,
        discipline_table: &DisciplineTable,
        player_data: &mut PlayerData,
    ) -> Result<CharacterProgressOutcome, CharacterProgressError> {
        let progress_id = request.progress_id.trim();
        if progress_id.is_empty() {
            return Err(CharacterProgressError::InvalidProgressId);
        }

        let row = resolve_progress_row(table, progress_id)?;
        if row.enabled == 0 {
            return Err(CharacterProgressError::ProgressDisabled);
        }

        let source_type = normalize_source_type(resolve_required_string(table, row.sourcetype)?)?;
        let source_id = resolve_required_string(table, row.sourceid)?
            .trim()
            .to_string();
        if source_id.is_empty() {
            return Err(invalid_config("SourceId must not be empty"));
        }

        let repeatable = row.repeatable != 0;
        if !repeatable && player_data.progress.completed.contains_key(progress_id) {
            return Ok(CharacterProgressOutcome {
                applied: false,
                progress_id: progress_id.to_string(),
                source_type,
                source_id,
                rewards: Vec::new(),
            });
        }

        let conditions = parse_condition(resolve_required_string(table, row.conditions)?)?;
        let rewards = parse_rewards(resolve_required_string(table, row.rewards)?)?;
        let snapshot =
            ProgressSnapshot::load(self, identity, player_data, &conditions, &rewards).await?;
        evaluate_condition(&conditions, &snapshot)?;
        self.validate_rewards_before_apply(&rewards, identity, discipline_table, &snapshot)
            .await?;

        let mut outcomes = Vec::new();
        let completed_at = now_text();
        for reward in rewards {
            let outcome = self
                .apply_reward(
                    identity,
                    &reward,
                    progress_id,
                    &source_type,
                    &source_id,
                    discipline_table,
                    player_data,
                    &completed_at,
                )
                .await?;
            player_data
                .progress
                .reward_logs
                .push(CharacterProgressRewardLog {
                    progress_id: progress_id.to_string(),
                    source_type: source_type.clone(),
                    source_id: source_id.clone(),
                    reward_type: outcome.reward_type.clone(),
                    reward_id: outcome.reward_id.clone(),
                    status: outcome.status.clone(),
                    created_at: completed_at.clone(),
                });
            outcomes.push(outcome);
        }

        player_data.progress.completed.insert(
            progress_id.to_string(),
            CharacterProgressRecord {
                progress_id: progress_id.to_string(),
                source_type: source_type.clone(),
                source_id: source_id.clone(),
                completed_at,
            },
        );
        player_data.set_data_dirty();

        Ok(CharacterProgressOutcome {
            applied: true,
            progress_id: progress_id.to_string(),
            source_type,
            source_id,
            rewards: outcomes,
        })
    }

    async fn validate_rewards_before_apply(
        &self,
        rewards: &[ProgressReward],
        identity: &AuthenticatedSessionIdentity,
        discipline_table: &DisciplineTable,
        snapshot: &ProgressSnapshot,
    ) -> Result<(), CharacterProgressError> {
        let mut projected_elements = snapshot.elements.clone();
        for reward in rewards {
            match reward {
                ProgressReward::Affinity(delta) => {
                    let before = projected_elements.as_ref().ok_or(
                        CharacterProgressError::CharacterElement(
                            CharacterElementError::CharacterNotFound,
                        ),
                    )?;
                    projected_elements = Some(
                        before
                            .apply_change(CharacterElementChange::new(
                                *delta,
                                ElementDeltas::zero(),
                            ))
                            .map_err(CharacterProgressError::CharacterElement)?,
                    );
                }
                ProgressReward::Mastery(delta) => {
                    let before = projected_elements.as_ref().ok_or(
                        CharacterProgressError::CharacterElement(
                            CharacterElementError::CharacterNotFound,
                        ),
                    )?;
                    projected_elements = Some(
                        before
                            .apply_change(CharacterElementChange::new(
                                ElementDeltas::zero(),
                                *delta,
                            ))
                            .map_err(CharacterProgressError::CharacterElement)?,
                    );
                }
                ProgressReward::DisciplinePoints {
                    discipline_id,
                    points,
                } => {
                    if *points <= 0 {
                        return Err(CharacterProgressError::Discipline(
                            DisciplineError::InvalidPoints,
                        ));
                    }
                    self.discipline_service
                        .get_for_identity(identity, discipline_id)
                        .await
                        .map_err(CharacterProgressError::Discipline)?;
                    validate_discipline_exists(discipline_table, discipline_id)?;
                }
                ProgressReward::Title {
                    title_id,
                    expires_at,
                } => {
                    let limited = self.is_limited_title(title_id).await?;
                    if expires_at.is_none() && limited {
                        return Err(CharacterProgressError::LimitedTitleRequiresExpiry {
                            title_id: title_id.clone(),
                        });
                    }
                }
                ProgressReward::DisciplineEligibility { discipline_id } => {
                    if discipline_id.trim().is_empty() {
                        return Err(invalid_config(
                            "discipline_eligibility reward requires discipline_id",
                        ));
                    }
                }
                ProgressReward::Unsupported { reward_type } => {
                    return Err(CharacterProgressError::UnsupportedReward {
                        reward_type: reward_type.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    async fn is_limited_title(&self, title_id: &str) -> Result<bool, CharacterProgressError> {
        self.title_service
            .is_limited_title(title_id)
            .await
            .map_err(CharacterProgressError::Title)
    }

    #[allow(clippy::too_many_arguments)]
    async fn apply_reward(
        &self,
        identity: &AuthenticatedSessionIdentity,
        reward: &ProgressReward,
        progress_id: &str,
        source_type: &str,
        source_id: &str,
        discipline_table: &DisciplineTable,
        player_data: &mut PlayerData,
        completed_at: &str,
    ) -> Result<CharacterProgressRewardOutcome, CharacterProgressError> {
        match reward {
            ProgressReward::Affinity(delta) => {
                let reason = format!("character progress {progress_id} affinity reward");
                self.character_element_service
                    .apply_change(
                        &identity.character_id,
                        CharacterElementChange::new(*delta, ElementDeltas::zero()),
                        element_source(identity, source_type, source_id),
                        Some(reason.as_str()),
                    )
                    .await
                    .map_err(CharacterProgressError::CharacterElement)?;
                Ok(CharacterProgressRewardOutcome {
                    reward_type: "affinity".to_string(),
                    reward_id: "element_affinity".to_string(),
                    status: "applied".to_string(),
                    title: None,
                    discipline: None,
                    eligibility: None,
                })
            }
            ProgressReward::Mastery(delta) => {
                let reason = format!("character progress {progress_id} mastery reward");
                self.character_element_service
                    .apply_change(
                        &identity.character_id,
                        CharacterElementChange::new(ElementDeltas::zero(), *delta),
                        element_source(identity, source_type, source_id),
                        Some(reason.as_str()),
                    )
                    .await
                    .map_err(CharacterProgressError::CharacterElement)?;
                Ok(CharacterProgressRewardOutcome {
                    reward_type: "mastery".to_string(),
                    reward_id: "element_mastery".to_string(),
                    status: "applied".to_string(),
                    title: None,
                    discipline: None,
                    eligibility: None,
                })
            }
            ProgressReward::DisciplinePoints {
                discipline_id,
                points,
            } => {
                let result = self
                    .discipline_service
                    .add_points_for_identity(
                        identity,
                        discipline_id,
                        *points,
                        discipline_table,
                        discipline_context(identity, source_type, source_id, progress_id),
                    )
                    .await
                    .map_err(CharacterProgressError::Discipline)?;
                Ok(CharacterProgressRewardOutcome {
                    reward_type: "discipline_points".to_string(),
                    reward_id: discipline_id.clone(),
                    status: "applied".to_string(),
                    title: None,
                    discipline: Some(result.discipline),
                    eligibility: None,
                })
            }
            ProgressReward::Title {
                title_id,
                expires_at,
            } => {
                let mut request = GrantTitleRequest::new(title_id.clone());
                if let Some(expires_at) = expires_at.as_ref() {
                    request = request.with_expires_at(expires_at.clone());
                }
                let grant = self
                    .title_service
                    .grant_for_identity(
                        identity,
                        request,
                        title_context(identity, source_type, source_id, progress_id),
                    )
                    .await
                    .map_err(|error| {
                        if matches!(error, TitleError::InvalidTitleAction) && expires_at.is_none() {
                            CharacterProgressError::LimitedTitleRequiresExpiry {
                                title_id: title_id.clone(),
                            }
                        } else {
                            CharacterProgressError::Title(error)
                        }
                    })?;
                let status = match grant.status {
                    GrantTitleStatus::Granted => "granted",
                    GrantTitleStatus::AlreadyOwned => "already_owned",
                    GrantTitleStatus::Renewed => "renewed",
                };
                Ok(CharacterProgressRewardOutcome {
                    reward_type: "title".to_string(),
                    reward_id: title_id.clone(),
                    status: status.to_string(),
                    title: Some(grant.title),
                    discipline: None,
                    eligibility: None,
                })
            }
            ProgressReward::DisciplineEligibility { discipline_id } => {
                player_data
                    .progress
                    .discipline_learning_eligibilities
                    .insert(discipline_id.clone());
                Ok(CharacterProgressRewardOutcome {
                    reward_type: "discipline_eligibility".to_string(),
                    reward_id: discipline_id.clone(),
                    status: "granted".to_string(),
                    title: None,
                    discipline: None,
                    eligibility: Some(discipline_id.clone()),
                })
            }
            ProgressReward::Unsupported { reward_type } => {
                Err(CharacterProgressError::UnsupportedReward {
                    reward_type: reward_type.clone(),
                })
            }
        }
        .map(|mut outcome| {
            if outcome.status.is_empty() {
                outcome.status = completed_at.to_string();
            }
            outcome
        })
    }
}

struct ProgressSnapshot {
    disciplines: Option<Vec<CharacterDiscipline>>,
    elements: Option<crate::core::character_element::CharacterElements>,
    titles: Option<Vec<CharacterTitle>>,
    item_growth: Option<Vec<ItemGrowthSnapshot>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ItemGrowthSnapshot {
    item_id: i32,
    growth_elements: ItemElementValues,
}

impl ProgressSnapshot {
    async fn load(
        service: &CharacterProgressService,
        identity: &AuthenticatedSessionIdentity,
        player_data: &PlayerData,
        condition: &ProgressCondition,
        rewards: &[ProgressReward],
    ) -> Result<Self, CharacterProgressError> {
        let disciplines = if condition.requires_disciplines() {
            Some(
                service
                    .discipline_service
                    .list_for_identity(identity)
                    .await
                    .map_err(CharacterProgressError::Discipline)?,
            )
        } else {
            None
        };
        let elements = if condition.requires_elements()
            || rewards.iter().any(ProgressReward::requires_elements)
        {
            Some(
                service
                    .character_element_service
                    .get_elements_for_identity(identity)
                    .await
                    .map_err(CharacterProgressError::CharacterElement)?,
            )
        } else {
            None
        };
        let titles = if condition.requires_titles() {
            Some(
                service
                    .title_service
                    .list_for_identity(
                        identity,
                        TitleOperationContext::new("progress_check")
                            .with_source_id("character_progress_condition")
                            .with_operator("player", identity.account_player_id.clone())
                            .with_reason("character progress condition check"),
                    )
                    .await
                    .map_err(CharacterProgressError::Title)?,
            )
        } else {
            None
        };

        let item_growth = if condition.requires_item_growth() {
            Some(collect_item_growth(player_data))
        } else {
            None
        };

        Ok(Self {
            disciplines,
            elements,
            titles,
            item_growth,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProgressCondition {
    Always,
    Affinity {
        element: ElementKind,
        min: i32,
    },
    Mastery {
        element: ElementKind,
        min: i32,
    },
    DisciplineTier {
        discipline_id: String,
        tier: String,
    },
    Title {
        title_id: String,
    },
    Discipline {
        discipline_id: String,
    },
    ItemGrowth {
        item_id: i32,
        element: ElementKind,
        min: i32,
    },
    AllOf(Vec<ProgressCondition>),
    AnyOf(Vec<ProgressCondition>),
    Unsupported {
        condition_type: String,
    },
}

impl ProgressCondition {
    fn requires_disciplines(&self) -> bool {
        match self {
            Self::DisciplineTier { .. } | Self::Discipline { .. } => true,
            Self::AllOf(values) | Self::AnyOf(values) => {
                values.iter().any(Self::requires_disciplines)
            }
            Self::Always
            | Self::Affinity { .. }
            | Self::Mastery { .. }
            | Self::Title { .. }
            | Self::ItemGrowth { .. }
            | Self::Unsupported { .. } => false,
        }
    }

    fn requires_elements(&self) -> bool {
        match self {
            Self::Affinity { .. } | Self::Mastery { .. } => true,
            Self::AllOf(values) | Self::AnyOf(values) => values.iter().any(Self::requires_elements),
            Self::Always
            | Self::DisciplineTier { .. }
            | Self::Title { .. }
            | Self::Discipline { .. }
            | Self::ItemGrowth { .. }
            | Self::Unsupported { .. } => false,
        }
    }

    fn requires_titles(&self) -> bool {
        match self {
            Self::Title { .. } => true,
            Self::AllOf(values) | Self::AnyOf(values) => values.iter().any(Self::requires_titles),
            Self::Always
            | Self::Affinity { .. }
            | Self::Mastery { .. }
            | Self::DisciplineTier { .. }
            | Self::Discipline { .. }
            | Self::ItemGrowth { .. }
            | Self::Unsupported { .. } => false,
        }
    }

    fn requires_item_growth(&self) -> bool {
        match self {
            Self::ItemGrowth { .. } => true,
            Self::AllOf(values) | Self::AnyOf(values) => {
                values.iter().any(Self::requires_item_growth)
            }
            Self::Always
            | Self::Affinity { .. }
            | Self::Mastery { .. }
            | Self::DisciplineTier { .. }
            | Self::Title { .. }
            | Self::Discipline { .. }
            | Self::Unsupported { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProgressReward {
    Affinity(ElementDeltas),
    Mastery(ElementDeltas),
    DisciplinePoints {
        discipline_id: String,
        points: i64,
    },
    Title {
        title_id: String,
        expires_at: Option<String>,
    },
    DisciplineEligibility {
        discipline_id: String,
    },
    Unsupported {
        reward_type: String,
    },
}

impl ProgressReward {
    fn requires_elements(&self) -> bool {
        matches!(self, Self::Affinity(_) | Self::Mastery(_))
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

    fn value_from_elements(self, values: ElementValues) -> i32 {
        match self {
            Self::Earth => values.earth,
            Self::Fire => values.fire,
            Self::Water => values.water,
            Self::Wind => values.wind,
        }
    }

    fn value_from_item(self, values: ItemElementValues) -> i32 {
        match self {
            Self::Earth => values.earth,
            Self::Fire => values.fire,
            Self::Water => values.water,
            Self::Wind => values.wind,
        }
    }
}

fn resolve_progress_row<'a>(
    table: &'a CharacterProgressTable,
    progress_id: &str,
) -> Result<&'a CharacterProgressTableRow, CharacterProgressError> {
    table
        .all()
        .iter()
        .find(|row| {
            table
                .resolve_string(row.progressid)
                .is_some_and(|value| value == progress_id)
        })
        .ok_or(CharacterProgressError::ProgressNotFound)
}

fn validate_discipline_exists(
    table: &DisciplineTable,
    discipline_id: &str,
) -> Result<(), CharacterProgressError> {
    if table
        .all()
        .iter()
        .any(|row| table.resolve_string(row.disciplineid).as_deref() == Some(discipline_id))
    {
        Ok(())
    } else {
        Err(CharacterProgressError::Discipline(
            DisciplineError::DisciplineConfigNotFound,
        ))
    }
}

fn parse_condition(raw: &str) -> Result<ProgressCondition, CharacterProgressError> {
    let value = serde_json::from_str::<Value>(raw).map_err(|error| {
        invalid_config(format!("Conditions must be valid JSON object: {error}"))
    })?;
    parse_condition_value(&value)
}

fn parse_condition_value(value: &Value) -> Result<ProgressCondition, CharacterProgressError> {
    match value {
        Value::String(text) if text.eq_ignore_ascii_case("always") => Ok(ProgressCondition::Always),
        Value::String(text) => Ok(ProgressCondition::Unsupported {
            condition_type: text.to_ascii_lowercase(),
        }),
        Value::Object(map) => {
            if let Some(all_of) = map.get("all_of").or_else(|| map.get("allOf")) {
                return parse_condition_list(all_of, true);
            }
            if let Some(any_of) = map.get("any_of").or_else(|| map.get("anyOf")) {
                return parse_condition_list(any_of, false);
            }
            let condition_type = string_field(value, &["type", "kind", "condition"])
                .unwrap_or_else(|| "unknown".to_string());
            match condition_type.as_str() {
                "always" => Ok(ProgressCondition::Always),
                "affinity" | "element_affinity" => Ok(ProgressCondition::Affinity {
                    element: element_field(value)?,
                    min: number_field(value, &["min", "threshold", "value", "required"])
                        .ok_or_else(|| invalid_config("affinity condition requires min"))?,
                }),
                "mastery" | "element_mastery" => Ok(ProgressCondition::Mastery {
                    element: element_field(value)?,
                    min: number_field(value, &["min", "threshold", "value", "required"])
                        .ok_or_else(|| invalid_config("mastery condition requires min"))?,
                }),
                "discipline_tier" => Ok(ProgressCondition::DisciplineTier {
                    discipline_id: string_field(value, &["discipline_id", "discipline"])
                        .ok_or_else(|| invalid_config("discipline_tier requires discipline_id"))?,
                    tier: string_field(value, &["tier", "min_tier"])
                        .ok_or_else(|| invalid_config("discipline_tier requires tier"))?,
                }),
                "title" => Ok(ProgressCondition::Title {
                    title_id: string_field_preserve(value, &["title_id", "title"])
                        .ok_or_else(|| invalid_config("title condition requires title_id"))?,
                }),
                "discipline" => Ok(ProgressCondition::Discipline {
                    discipline_id: string_field(value, &["discipline_id", "discipline"])
                        .ok_or_else(|| {
                            invalid_config("discipline condition requires discipline_id")
                        })?,
                }),
                "item_growth" => Ok(ProgressCondition::ItemGrowth {
                    item_id: number_field(value, &["item_id", "itemId"])
                        .ok_or_else(|| invalid_config("item_growth condition requires item_id"))?,
                    element: element_field(value)?,
                    min: number_field(value, &["min", "threshold", "value", "required"])
                        .ok_or_else(|| invalid_config("item_growth condition requires min"))?,
                }),
                other => Ok(ProgressCondition::Unsupported {
                    condition_type: other.to_string(),
                }),
            }
        }
        Value::Array(_) => parse_condition_list(value, true),
        _ => Err(invalid_config(
            "Conditions must be an object, array, or string",
        )),
    }
}

fn parse_condition_list(
    value: &Value,
    all_of: bool,
) -> Result<ProgressCondition, CharacterProgressError> {
    let Some(values) = value.as_array() else {
        return Err(invalid_config("condition group requires array"));
    };
    if values.is_empty() {
        return Err(invalid_config("condition group requires at least one item"));
    }
    let mut conditions = Vec::with_capacity(values.len());
    for nested in values {
        conditions.push(parse_condition_value(nested)?);
    }
    if all_of {
        Ok(ProgressCondition::AllOf(conditions))
    } else {
        Ok(ProgressCondition::AnyOf(conditions))
    }
}

fn parse_rewards(raw: &str) -> Result<Vec<ProgressReward>, CharacterProgressError> {
    let value = serde_json::from_str::<Value>(raw)
        .map_err(|error| invalid_config(format!("Rewards must be valid JSON: {error}")))?;
    let values = value
        .as_array()
        .ok_or_else(|| invalid_config("Rewards must be an array"))?;
    if values.is_empty() {
        return Err(invalid_config("Rewards must not be empty"));
    }
    values.iter().map(parse_reward_value).collect()
}

fn parse_reward_value(value: &Value) -> Result<ProgressReward, CharacterProgressError> {
    let reward_type =
        string_field(value, &["type", "kind", "reward"]).unwrap_or_else(|| "unknown".to_string());
    match reward_type.as_str() {
        "affinity" | "element_affinity" => Ok(ProgressReward::Affinity(delta_from_value(value))),
        "mastery" | "element_mastery" => Ok(ProgressReward::Mastery(delta_from_value(value))),
        "discipline_points" | "mastery_points" => Ok(ProgressReward::DisciplinePoints {
            discipline_id: string_field(value, &["discipline_id", "discipline"])
                .ok_or_else(|| invalid_config("discipline_points reward requires discipline_id"))?,
            points: i64::from(
                number_field(value, &["points", "points_delta", "delta"])
                    .ok_or_else(|| invalid_config("discipline_points reward requires points"))?,
            ),
        }),
        "title" | "title_unlock" => Ok(ProgressReward::Title {
            title_id: string_field_preserve(value, &["title_id", "title"])
                .ok_or_else(|| invalid_config("title reward requires title_id"))?,
            expires_at: string_field_preserve(value, &["expires_at", "expiresAt"]),
        }),
        "discipline_eligibility" | "discipline_learn_eligibility" => {
            Ok(ProgressReward::DisciplineEligibility {
                discipline_id: string_field(value, &["discipline_id", "discipline"]).ok_or_else(
                    || invalid_config("discipline_eligibility reward requires discipline_id"),
                )?,
            })
        }
        other => Ok(ProgressReward::Unsupported {
            reward_type: other.to_string(),
        }),
    }
}

fn evaluate_condition(
    condition: &ProgressCondition,
    snapshot: &ProgressSnapshot,
) -> Result<(), CharacterProgressError> {
    match condition {
        ProgressCondition::Always => Ok(()),
        ProgressCondition::Affinity { element, min } => {
            let matched = snapshot
                .elements
                .as_ref()
                .map(|elements| element.value_from_elements(elements.affinity) >= *min)
                .unwrap_or(false);
            condition_result(matched, "affinity")
        }
        ProgressCondition::Mastery { element, min } => {
            let matched = snapshot
                .elements
                .as_ref()
                .map(|elements| element.value_from_elements(elements.mastery) >= *min)
                .unwrap_or(false);
            condition_result(matched, "mastery")
        }
        ProgressCondition::DisciplineTier {
            discipline_id,
            tier,
        } => {
            let matched = snapshot
                .disciplines
                .as_deref()
                .unwrap_or_default()
                .iter()
                .find(|discipline| discipline.discipline_id == *discipline_id)
                .is_some_and(|discipline| discipline_tier_satisfies(&discipline.tier, tier));
            condition_result(matched, "discipline_tier")
        }
        ProgressCondition::Title { title_id } => {
            let matched = snapshot
                .titles
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|title| title.title_id == *title_id && !title.expired);
            condition_result(matched, "title")
        }
        ProgressCondition::Discipline { discipline_id } => {
            let matched = snapshot
                .disciplines
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|discipline| discipline.discipline_id == *discipline_id);
            condition_result(matched, "discipline")
        }
        ProgressCondition::ItemGrowth {
            item_id,
            element,
            min,
        } => {
            let matched = snapshot
                .item_growth
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|item| {
                    item.item_id == *item_id
                        && element.value_from_item(item.growth_elements) >= *min
                });
            condition_result(matched, "item_growth")
        }
        ProgressCondition::AllOf(values) => {
            for nested in values {
                evaluate_condition(nested, snapshot)?;
            }
            Ok(())
        }
        ProgressCondition::AnyOf(values) => {
            let mut last_error = None;
            for nested in values {
                match evaluate_condition(nested, snapshot) {
                    Ok(()) => return Ok(()),
                    Err(error) => last_error = Some(error),
                }
            }
            Err(
                last_error.unwrap_or(CharacterProgressError::ConditionNotMet {
                    reason: "any_of".to_string(),
                }),
            )
        }
        ProgressCondition::Unsupported { condition_type } => {
            Err(CharacterProgressError::UnsupportedCondition {
                condition_type: condition_type.clone(),
            })
        }
    }
}

fn condition_result(matched: bool, reason: &str) -> Result<(), CharacterProgressError> {
    if matched {
        Ok(())
    } else {
        Err(CharacterProgressError::ConditionNotMet {
            reason: reason.to_string(),
        })
    }
}

fn collect_item_growth(player_data: &PlayerData) -> Vec<ItemGrowthSnapshot> {
    let mut values = Vec::new();
    values.extend(
        player_data
            .get_inventory_items()
            .into_iter()
            .map(|item| ItemGrowthSnapshot {
                item_id: item.item_id,
                growth_elements: item.growth_elements,
            }),
    );
    values.extend(
        player_data
            .get_warehouse_items()
            .into_iter()
            .map(|item| ItemGrowthSnapshot {
                item_id: item.item_id,
                growth_elements: item.growth_elements,
            }),
    );
    values.extend(
        player_data
            .get_equipped_items()
            .into_iter()
            .map(|(_, item)| ItemGrowthSnapshot {
                item_id: item.item_id,
                growth_elements: item.growth_elements,
            }),
    );
    values
}

fn normalize_source_type(value: &str) -> Result<String, CharacterProgressError> {
    let normalized = value.trim().to_ascii_lowercase();
    if VALID_SOURCE_TYPES.contains(&normalized.as_str()) {
        Ok(normalized)
    } else {
        Err(invalid_config(format!(
            "SourceType `{value}` must be one of {}",
            VALID_SOURCE_TYPES.join(",")
        )))
    }
}

fn resolve_required_string(
    table: &CharacterProgressTable,
    key: u32,
) -> Result<&str, CharacterProgressError> {
    table
        .resolve_string(key)
        .ok_or_else(|| invalid_config(format!("missing string key {key}")))
}

fn element_field(value: &Value) -> Result<ElementKind, CharacterProgressError> {
    string_field(value, &["element"])
        .and_then(|value| ElementKind::parse(&value))
        .or_else(|| element_key(value))
        .ok_or_else(|| invalid_config("condition requires earth/fire/water/wind element"))
}

fn element_key(value: &Value) -> Option<ElementKind> {
    let map = value.as_object()?;
    map.keys().find_map(|key| ElementKind::parse(key))
}

fn number_field(value: &Value, keys: &[&str]) -> Option<i32> {
    let map = value.as_object()?;
    for key in keys {
        let Some(value) = map.get(*key) else {
            continue;
        };
        if let Some(number) = value.as_i64().and_then(|value| i32::try_from(value).ok()) {
            return Some(number);
        }
        if let Some(number) = value.as_str().and_then(|value| value.parse::<i32>().ok()) {
            return Some(number);
        }
    }
    None
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    string_field_preserve(value, keys).map(|value| value.to_ascii_lowercase())
}

fn string_field_preserve(value: &Value, keys: &[&str]) -> Option<String> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(text) = map.get(*key).and_then(Value::as_str) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn delta_from_value(value: &Value) -> ElementDeltas {
    ElementDeltas::new(
        number_field(value, &["earth"]).unwrap_or_default(),
        number_field(value, &["fire"]).unwrap_or_default(),
        number_field(value, &["water"]).unwrap_or_default(),
        number_field(value, &["wind"]).unwrap_or_default(),
    )
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

fn element_source(
    identity: &AuthenticatedSessionIdentity,
    source_type: &str,
    source_id: &str,
) -> CharacterElementChangeSource {
    CharacterElementChangeSource::new(source_type.to_string())
        .with_source_id(source_id.to_string())
        .with_operator("player", identity.account_player_id.clone())
}

fn discipline_context(
    identity: &AuthenticatedSessionIdentity,
    source_type: &str,
    source_id: &str,
    progress_id: &str,
) -> DisciplineOperationContext {
    DisciplineOperationContext::new(source_type.to_string())
        .with_source_id(source_id.to_string())
        .with_operator("player", identity.account_player_id.clone())
        .with_reason(format!(
            "character progress {progress_id} discipline reward"
        ))
}

fn title_context(
    identity: &AuthenticatedSessionIdentity,
    source_type: &str,
    source_id: &str,
    progress_id: &str,
) -> TitleOperationContext {
    TitleOperationContext::new(source_type.to_string())
        .with_source_id(source_id.to_string())
        .with_operator("player", identity.account_player_id.clone())
        .with_reason(format!("character progress {progress_id} title reward"))
}

fn invalid_config(message: impl Into<String>) -> CharacterProgressError {
    CharacterProgressError::InvalidProgressConfig {
        message: message.into(),
    }
}

fn now_text() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::character_discipline::{DisciplineService, DisciplineUpsert};
    use crate::core::character_element::{
        CharacterElementService, CharacterElements, ElementValues,
    };
    use crate::core::character_title::{GrantTitleRequest, TitleService};
    use crate::core::inventory::Item;
    use crate::csv_code::characterprogresstable::{
        CharacterProgressTable, CharacterProgressTableRow, StringKey,
    };
    use crate::csv_code::disciplinetable::{DisciplineTable, DisciplineTableRow};
    use crate::csv_code::titletable::{TitleTable, TitleTableRow};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn identity() -> AuthenticatedSessionIdentity {
        AuthenticatedSessionIdentity {
            account_player_id: "plr_0000000000001".to_string(),
            character_id: "chr_0000000000001".to_string(),
            world_id: Some(0),
        }
    }

    fn progress_table(rows: Vec<TestProgressRow>) -> CharacterProgressTable {
        let mut string_pool = HashMap::new();
        let mut next_key: StringKey = 1;
        let mut key = |value: &str, string_pool: &mut HashMap<StringKey, String>| {
            let key = next_key;
            next_key = next_key.saturating_add(1);
            string_pool.insert(key, value.to_string());
            key
        };
        let rows = rows
            .into_iter()
            .map(|row| CharacterProgressTableRow {
                id: row.id,
                progressid: key(row.progress_id, &mut string_pool),
                sourcetype: key(row.source_type, &mut string_pool),
                sourceid: key(row.source_id, &mut string_pool),
                name: key(row.name, &mut string_pool),
                conditions: key(row.conditions, &mut string_pool),
                rewards: key(row.rewards, &mut string_pool),
                repeatable: i32::from(row.repeatable),
                enabled: i32::from(row.enabled),
                description: key("", &mut string_pool),
            })
            .collect::<Vec<_>>();
        let by_id = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id, index))
            .collect();
        CharacterProgressTable {
            string_pool,
            rows,
            by_id,
        }
    }

    struct TestProgressRow {
        id: i32,
        progress_id: &'static str,
        source_type: &'static str,
        source_id: &'static str,
        name: &'static str,
        conditions: &'static str,
        rewards: &'static str,
        repeatable: bool,
        enabled: bool,
    }

    impl TestProgressRow {
        fn new(progress_id: &'static str, rewards: &'static str) -> Self {
            Self {
                id: 1,
                progress_id,
                source_type: "quest",
                source_id: "quest_1",
                name: "quest",
                conditions: r#"{"type":"always"}"#,
                rewards,
                repeatable: false,
                enabled: true,
            }
        }

        fn conditions(mut self, conditions: &'static str) -> Self {
            self.conditions = conditions;
            self
        }

        fn source(mut self, source_type: &'static str, source_id: &'static str) -> Self {
            self.source_type = source_type;
            self.source_id = source_id;
            self
        }
    }

    async fn service_fixture() -> CharacterProgressService {
        CharacterProgressService::new(
            CharacterElementService::new_in_memory(),
            DisciplineService::new_in_memory(),
            TitleService::new_in_memory(title_table()),
        )
    }

    fn title_table() -> Arc<TitleTable> {
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
        Arc::new(TitleTable {
            string_pool: HashMap::new(),
            rows,
            by_id,
        })
    }

    fn discipline_table() -> DisciplineTable {
        let mut string_pool = HashMap::new();
        string_pool.insert(1, "forging".to_string());
        string_pool.insert(
            2,
            r#"{"initial_tier":"novice","initial_points":0,"tiers":[{"tier":"novice","min_points":0},{"tier":"apprentice","min_points":10}]}"#.to_string(),
        );
        let rows = vec![DisciplineTableRow {
            id: 1,
            disciplineid: 1,
            name: 1,
            description: 1,
            learnconditions: 1,
            tierrules: 2,
            ..DisciplineTableRow::default()
        }];
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

    #[tokio::test]
    async fn progress_reward_applies_growth_and_records_stable_sources() {
        let service = service_fixture().await;
        let identity = identity();
        service
            .character_element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::zero(),
            })
            .await;
        service
            .discipline_service
            .upsert_for_identity(
                &identity,
                DisciplineUpsert::new("forging", 0, "novice", true),
            )
            .await
            .unwrap();
        service
            .title_service
            .grant_for_identity(
                &identity,
                GrantTitleRequest::new("2001"),
                TitleOperationContext::new("system"),
            )
            .await
            .unwrap();
        let mut player_data = PlayerData::new(identity.character_id.clone());
        let mut item = Item::new(7, 1002, 1, false);
        item.growth_elements = ItemElementValues::new(0, 1, 0, 0);
        player_data.add_item(item).unwrap();
        let table = progress_table(vec![
            TestProgressRow::new(
                "quest_1",
                r#"[{"type":"affinity","earth":-10,"fire":10},{"type":"mastery","fire":5},{"type":"discipline_points","discipline_id":"forging","points":10},{"type":"title","title_id":"2001"},{"type":"discipline_eligibility","discipline_id":"fire_art"}]"#,
            )
            .conditions(r#"{"all_of":[{"type":"affinity","element":"fire","min":2500},{"type":"mastery","element":"fire","min":0},{"type":"discipline_tier","discipline_id":"forging","tier":"novice"},{"type":"title","title_id":"2001"},{"type":"discipline","discipline_id":"forging"},{"type":"item_growth","item_id":1002,"element":"fire","min":1}]}"#),
        ]);

        let result = service
            .apply_for_identity(
                &identity,
                ApplyCharacterProgressRequest::new("quest_1"),
                &table,
                &discipline_table(),
                &mut player_data,
            )
            .await
            .unwrap();

        assert!(result.applied);
        assert_eq!(result.source_type, "quest");
        assert_eq!(result.source_id, "quest_1");
        assert_eq!(result.rewards.len(), 5);
        assert!(
            player_data
                .progress
                .discipline_learning_eligibilities
                .contains("fire_art")
        );

        let element_logs = service
            .character_element_service
            .applied_change_logs()
            .await;
        assert_eq!(element_logs.len(), 2);
        assert_eq!(element_logs[0].source.source_type, "quest");
        assert_eq!(element_logs[0].source.source_id.as_deref(), Some("quest_1"));
        let title_logs = service.title_service.logs().await;
        assert!(
            title_logs
                .iter()
                .any(|log| log.source_type.as_deref() == Some("quest")
                    && log.source_id.as_deref() == Some("quest_1"))
        );
    }

    #[tokio::test]
    async fn top_level_condition_array_is_evaluated_as_all_of() {
        let service = service_fixture().await;
        let identity = identity();
        service
            .character_element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::zero(),
            })
            .await;
        let mut player_data = PlayerData::new(identity.character_id.clone());
        let table = progress_table(vec![
            TestProgressRow::new("quest_array", r#"[{"type":"mastery","fire":1}]"#)
                .conditions(r#"[{"type":"affinity","element":"fire","min":2500},{"type":"mastery","element":"fire","min":1}]"#),
        ]);

        let error = service
            .apply_for_identity(
                &identity,
                ApplyCharacterProgressRequest::new("quest_array"),
                &table,
                &discipline_table(),
                &mut player_data,
            )
            .await
            .expect_err("top-level array should require every nested condition");
        assert!(
            matches!(
                error,
                CharacterProgressError::ConditionNotMet { ref reason } if reason == "mastery"
            ),
            "unexpected error: {error}"
        );
        assert!(!player_data.progress.completed.contains_key("quest_array"));

        service
            .character_element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::new(0, 1, 0, 0),
            })
            .await;

        let result = service
            .apply_for_identity(
                &identity,
                ApplyCharacterProgressRequest::new("quest_array"),
                &table,
                &discipline_table(),
                &mut player_data,
            )
            .await
            .unwrap();

        assert!(result.applied);
        assert_eq!(result.rewards.len(), 1);
        assert!(player_data.progress.completed.contains_key("quest_array"));
    }

    #[tokio::test]
    async fn repeated_non_repeatable_progress_is_idempotent() {
        let service = service_fixture().await;
        let identity = identity();
        service
            .character_element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::zero(),
            })
            .await;
        let mut player_data = PlayerData::new(identity.character_id.clone());
        let table = progress_table(vec![
            TestProgressRow::new("achievement_1", r#"[{"type":"mastery","wind":1}]"#)
                .source("achievement", "first_clear"),
        ]);

        let first = service
            .apply_for_identity(
                &identity,
                ApplyCharacterProgressRequest::new("achievement_1"),
                &table,
                &discipline_table(),
                &mut player_data,
            )
            .await
            .unwrap();
        let second = service
            .apply_for_identity(
                &identity,
                ApplyCharacterProgressRequest::new("achievement_1"),
                &table,
                &discipline_table(),
                &mut player_data,
            )
            .await
            .unwrap();

        assert!(first.applied);
        assert!(!second.applied);
        assert_eq!(player_data.progress.reward_logs.len(), 1);
        assert_eq!(
            service
                .character_element_service
                .applied_change_logs()
                .await
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn limited_title_reward_requires_explicit_expiry() {
        let service = service_fixture().await;
        let identity = identity();
        let mut player_data = PlayerData::new(identity.character_id.clone());
        let table = progress_table(vec![
            TestProgressRow::new("activity_1", r#"[{"type":"title","title_id":"9001"}]"#)
                .source("activity", "summer"),
        ]);

        let error = service
            .apply_for_identity(
                &identity,
                ApplyCharacterProgressRequest::new("activity_1"),
                &table,
                &discipline_table(),
                &mut player_data,
            )
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), "LIMITED_TITLE_REQUIRES_EXPIRES_AT");
        assert!(player_data.progress.completed.is_empty());
    }

    #[tokio::test]
    async fn reward_validation_runs_before_any_growth_is_written() {
        let service = service_fixture().await;
        let identity = identity();
        service
            .character_element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::zero(),
            })
            .await;
        let mut player_data = PlayerData::new(identity.character_id.clone());
        let table = progress_table(vec![
            TestProgressRow::new(
                "activity_mixed_invalid",
                r#"[{"type":"mastery","fire":5},{"type":"title","title_id":"9001"}]"#,
            )
            .source("activity", "summer"),
        ]);

        let error = service
            .apply_for_identity(
                &identity,
                ApplyCharacterProgressRequest::new("activity_mixed_invalid"),
                &table,
                &discipline_table(),
                &mut player_data,
            )
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), "LIMITED_TITLE_REQUIRES_EXPIRES_AT");
        assert!(player_data.progress.completed.is_empty());
        assert!(player_data.progress.reward_logs.is_empty());
        assert!(
            service
                .character_element_service
                .applied_change_logs()
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn reward_validation_checks_accumulated_element_rewards() {
        let service = service_fixture().await;
        let identity = identity();
        service
            .character_element_service
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::new(0, 5, 0, 0),
            })
            .await;
        let mut player_data = PlayerData::new(identity.character_id.clone());
        let table = progress_table(vec![TestProgressRow::new(
            "quest_accumulated_invalid",
            r#"[{"type":"mastery","fire":-3},{"type":"mastery","fire":-3}]"#,
        )]);

        let error = service
            .apply_for_identity(
                &identity,
                ApplyCharacterProgressRequest::new("quest_accumulated_invalid"),
                &table,
                &discipline_table(),
                &mut player_data,
            )
            .await
            .unwrap_err();

        assert_eq!(error.error_code(), "NEGATIVE_MASTERY");
        assert!(player_data.progress.completed.is_empty());
        assert!(player_data.progress.reward_logs.is_empty());
        assert!(
            service
                .character_element_service
                .applied_change_logs()
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn repeatable_title_reward_records_duplicate_grant_attempt() {
        let service = service_fixture().await;
        let identity = identity();
        let mut player_data = PlayerData::new(identity.character_id.clone());
        let table = progress_table(vec![TestProgressRow {
            id: 1,
            progress_id: "event_title",
            source_type: "achievement",
            source_id: "same_title_twice",
            name: "repeatable title",
            conditions: r#"{"type":"always"}"#,
            rewards: r#"[{"type":"title","title_id":"2001"}]"#,
            repeatable: true,
            enabled: true,
        }]);

        let first = service
            .apply_for_identity(
                &identity,
                ApplyCharacterProgressRequest::new("event_title"),
                &table,
                &discipline_table(),
                &mut player_data,
            )
            .await
            .unwrap();
        let second = service
            .apply_for_identity(
                &identity,
                ApplyCharacterProgressRequest::new("event_title"),
                &table,
                &discipline_table(),
                &mut player_data,
            )
            .await
            .unwrap();

        assert_eq!(first.rewards[0].status, "granted");
        assert_eq!(second.rewards[0].status, "already_owned");
        assert_eq!(player_data.progress.reward_logs.len(), 2);
        let title_logs = service.title_service.logs().await;
        assert_eq!(
            title_logs
                .iter()
                .filter(|log| {
                    log.title_id == "2001"
                        && log.source_type.as_deref() == Some("achievement")
                        && log.source_id.as_deref() == Some("same_title_twice")
                })
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn expired_title_reward_is_renewed_with_business_source() {
        let service = service_fixture().await;
        let identity = identity();
        service
            .title_service
            .grant_for_identity(
                &identity,
                GrantTitleRequest::new("2001"),
                TitleOperationContext::new("system"),
            )
            .await
            .unwrap();
        service
            .title_service
            .mark_expired_for_test(&identity.character_id, "2001")
            .await;
        let mut player_data = PlayerData::new(identity.character_id.clone());
        let table = progress_table(vec![
            TestProgressRow::new(
                "world_event_renew",
                r#"[{"type":"title","title_id":"2001"}]"#,
            )
            .source("world_event", "keep_guard"),
        ]);

        let result = service
            .apply_for_identity(
                &identity,
                ApplyCharacterProgressRequest::new("world_event_renew"),
                &table,
                &discipline_table(),
                &mut player_data,
            )
            .await
            .unwrap();

        assert_eq!(result.rewards[0].status, "renewed");
        assert_eq!(
            result.rewards[0].title.as_ref().map(|title| title.expired),
            Some(false)
        );
        let title_logs = service.title_service.logs().await;
        assert!(title_logs.iter().any(|log| {
            log.action == "grant"
                && log.title_id == "2001"
                && log.source_type.as_deref() == Some("world_event")
                && log.source_id.as_deref() == Some("keep_guard")
                && log.before_json.is_some()
                && log.after_json.is_some()
        }));
    }

    #[tokio::test]
    async fn activity_ranking_and_world_event_sources_are_preserved() {
        let service = service_fixture().await;
        let identity = identity();
        for (idx, (source_type, source_id)) in [
            ("achievement", "first_clear"),
            ("activity", "summer"),
            ("ranking", "arena_weekly"),
            ("world_event", "keep_guard"),
        ]
        .into_iter()
        .enumerate()
        {
            let progress_id = format!("progress_{idx}");
            let row = TestProgressRow {
                id: i32::try_from(idx + 1).unwrap(),
                progress_id: Box::leak(progress_id.clone().into_boxed_str()),
                source_type,
                source_id,
                name: "source",
                conditions: r#"{"type":"always"}"#,
                rewards: r#"[{"type":"title","title_id":"2001"}]"#,
                repeatable: false,
                enabled: true,
            };
            let table = progress_table(vec![row]);
            let mut player_data = PlayerData::new(identity.character_id.clone());
            service
                .apply_for_identity(
                    &identity,
                    ApplyCharacterProgressRequest::new(progress_id),
                    &table,
                    &discipline_table(),
                    &mut player_data,
                )
                .await
                .unwrap();
        }

        let logs = service.title_service.logs().await;
        for source_type in ["achievement", "activity", "ranking", "world_event"] {
            assert!(
                logs.iter()
                    .any(|log| log.source_type.as_deref() == Some(source_type)),
                "missing title log for {source_type}"
            );
        }
    }
}
