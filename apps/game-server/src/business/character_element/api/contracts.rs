use crate::business::character_element::domain::{
    CharacterElementApplyResult, CharacterElementChange, CharacterElementChangeSource,
    CharacterElementError, CharacterElements, ElementDeltas, ElementValues,
};

const SOURCE_TYPE_MAX_CHARS: usize = 32;
const SOURCE_ID_MAX_CHARS: usize = 128;
const OPERATOR_TYPE_MAX_CHARS: usize = 32;
const OPERATOR_ID_MAX_CHARS: usize = 128;
const REASON_MAX_CHARS: usize = 255;

/// A query for the permanent elements of one server-authorized character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetCharacterElements {
    character_id: String,
}

impl GetCharacterElements {
    pub fn new(character_id: impl Into<String>) -> Self {
        Self {
            character_id: character_id.into(),
        }
    }

    pub fn character_id(&self) -> &str {
        &self.character_id
    }
}

/// A stable, immutable-by-API snapshot of the four permanent element values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementSnapshot {
    earth: i32,
    fire: i32,
    water: i32,
    wind: i32,
}

impl ElementSnapshot {
    pub fn earth(&self) -> i32 {
        self.earth
    }

    pub fn fire(&self) -> i32 {
        self.fire
    }

    pub fn water(&self) -> i32 {
        self.water
    }

    pub fn wind(&self) -> i32 {
        self.wind
    }
}

impl From<ElementValues> for ElementSnapshot {
    fn from(values: ElementValues) -> Self {
        Self {
            earth: values.earth,
            fire: values.fire,
            water: values.water,
            wind: values.wind,
        }
    }
}

/// A stable, immutable-by-API permanent element snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterElementSnapshot {
    character_id: String,
    affinity: ElementSnapshot,
    mastery: ElementSnapshot,
}

impl CharacterElementSnapshot {
    pub fn character_id(&self) -> &str {
        &self.character_id
    }

    pub fn affinity(&self) -> ElementSnapshot {
        self.affinity
    }

    pub fn mastery(&self) -> ElementSnapshot {
        self.mastery
    }
}

impl From<CharacterElements> for CharacterElementSnapshot {
    fn from(elements: CharacterElements) -> Self {
        Self {
            character_id: elements.character_id,
            affinity: elements.affinity.into(),
            mastery: elements.mastery.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetCharacterElementsResult {
    elements: CharacterElementSnapshot,
}

impl GetCharacterElementsResult {
    pub(crate) fn new(elements: CharacterElements) -> Self {
        Self {
            elements: elements.into(),
        }
    }

    pub fn elements(&self) -> &CharacterElementSnapshot {
        &self.elements
    }
}

/// A delta for a single element group. It is an input value, not mutable state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementDelta {
    earth: i32,
    fire: i32,
    water: i32,
    wind: i32,
}

impl ElementDelta {
    pub const fn new(earth: i32, fire: i32, water: i32, wind: i32) -> Self {
        Self {
            earth,
            fire,
            water,
            wind,
        }
    }

    pub const fn zero() -> Self {
        Self::new(0, 0, 0, 0)
    }
}

impl From<ElementDelta> for ElementDeltas {
    fn from(delta: ElementDelta) -> Self {
        Self::new(delta.earth, delta.fire, delta.water, delta.wind)
    }
}

/// The affinity and mastery deltas for one permanent element change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharacterElementDelta {
    affinity: ElementDelta,
    mastery: ElementDelta,
}

impl CharacterElementDelta {
    pub const fn new(affinity: ElementDelta, mastery: ElementDelta) -> Self {
        Self { affinity, mastery }
    }

    pub const fn zero() -> Self {
        Self::new(ElementDelta::zero(), ElementDelta::zero())
    }
}

impl From<CharacterElementDelta> for CharacterElementChange {
    fn from(change: CharacterElementDelta) -> Self {
        Self::new(change.affinity.into(), change.mastery.into())
    }
}

/// Rejected audit context. These errors are raised before a repository call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharacterElementChangeContextError {
    MissingSourceType,
    MissingPairedOperator,
    FieldTooLong {
        field: &'static str,
        max_chars: usize,
    },
}

impl CharacterElementChangeContextError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::MissingSourceType | Self::MissingPairedOperator => {
                "CHARACTER_ELEMENTS_INVALID_CHANGE_CONTEXT"
            }
            Self::FieldTooLong { .. } => "CHARACTER_ELEMENTS_CHANGE_CONTEXT_TOO_LONG",
        }
    }
}

impl std::fmt::Display for CharacterElementChangeContextError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSourceType => write!(formatter, "source_type must not be blank"),
            Self::MissingPairedOperator => {
                write!(
                    formatter,
                    "operator_type and operator_id must be supplied together"
                )
            }
            Self::FieldTooLong { field, max_chars } => {
                write!(formatter, "{field} must not exceed {max_chars} characters")
            }
        }
    }
}

impl std::error::Error for CharacterElementChangeContextError {}

/// Server-trusted audit metadata. It cannot be constructed from protocol or
/// session types and validates the existing audit-column bounds up front.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedCharacterElementChangeContext {
    source_type: String,
    source_id: Option<String>,
    operator_type: Option<String>,
    operator_id: Option<String>,
    reason: Option<String>,
}

impl TrustedCharacterElementChangeContext {
    pub fn try_new(
        source_type: impl Into<String>,
        source_id: Option<String>,
        operator_type: Option<String>,
        operator_id: Option<String>,
        reason: Option<String>,
    ) -> Result<Self, CharacterElementChangeContextError> {
        let source_type = source_type.into();
        if source_type.trim().is_empty() {
            return Err(CharacterElementChangeContextError::MissingSourceType);
        }
        validate_max_chars("source_type", &source_type, SOURCE_TYPE_MAX_CHARS)?;
        validate_optional_max_chars("source_id", source_id.as_deref(), SOURCE_ID_MAX_CHARS)?;
        validate_optional_max_chars(
            "operator_type",
            operator_type.as_deref(),
            OPERATOR_TYPE_MAX_CHARS,
        )?;
        validate_optional_max_chars("operator_id", operator_id.as_deref(), OPERATOR_ID_MAX_CHARS)?;

        if operator_type.is_some() != operator_id.is_some() {
            return Err(CharacterElementChangeContextError::MissingPairedOperator);
        }

        let reason = reason
            .map(|value| {
                value
                    .trim()
                    .chars()
                    .take(REASON_MAX_CHARS)
                    .collect::<String>()
            })
            .filter(|value| !value.is_empty());

        Ok(Self {
            source_type,
            source_id,
            operator_type,
            operator_id,
            reason,
        })
    }

    pub fn source_type(&self) -> &str {
        &self.source_type
    }

    pub fn source_id(&self) -> Option<&str> {
        self.source_id.as_deref()
    }

    pub fn operator_type(&self) -> Option<&str> {
        self.operator_type.as_deref()
    }

    pub fn operator_id(&self) -> Option<&str> {
        self.operator_id.as_deref()
    }

    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    pub(crate) fn to_domain_parts(&self) -> (CharacterElementChangeSource, Option<String>) {
        let mut source = CharacterElementChangeSource::new(self.source_type.clone());
        if let Some(source_id) = &self.source_id {
            source = source.with_source_id(source_id.clone());
        }
        if let (Some(operator_type), Some(operator_id)) = (&self.operator_type, &self.operator_id) {
            source = source.with_operator(operator_type.clone(), operator_id.clone());
        }
        (source, self.reason.clone())
    }
}

fn validate_optional_max_chars(
    field: &'static str,
    value: Option<&str>,
    max_chars: usize,
) -> Result<(), CharacterElementChangeContextError> {
    if let Some(value) = value {
        validate_max_chars(field, value, max_chars)?;
    }
    Ok(())
}

fn validate_max_chars(
    field: &'static str,
    value: &str,
    max_chars: usize,
) -> Result<(), CharacterElementChangeContextError> {
    if value.chars().count() > max_chars {
        return Err(CharacterElementChangeContextError::FieldTooLong { field, max_chars });
    }
    Ok(())
}

/// A command to change one server-authorized character's permanent elements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyCharacterElementChange {
    character_id: String,
    delta: CharacterElementDelta,
    context: TrustedCharacterElementChangeContext,
}

impl ApplyCharacterElementChange {
    pub fn new(
        character_id: impl Into<String>,
        delta: CharacterElementDelta,
        context: TrustedCharacterElementChangeContext,
    ) -> Self {
        Self {
            character_id: character_id.into(),
            delta,
            context,
        }
    }

    pub fn character_id(&self) -> &str {
        &self.character_id
    }

    pub(crate) fn delta(&self) -> CharacterElementDelta {
        self.delta
    }

    pub(crate) fn context(&self) -> &TrustedCharacterElementChangeContext {
        &self.context
    }
}

/// A fact that is constructible only after the repository confirms commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterElementsChanged {
    character_id: String,
    before: CharacterElementSnapshot,
    after: CharacterElementSnapshot,
    delta: CharacterElementDelta,
    context: TrustedCharacterElementChangeContext,
}

impl CharacterElementsChanged {
    pub fn character_id(&self) -> &str {
        &self.character_id
    }

    pub fn before(&self) -> &CharacterElementSnapshot {
        &self.before
    }

    pub fn after(&self) -> &CharacterElementSnapshot {
        &self.after
    }

    pub fn context(&self) -> &TrustedCharacterElementChangeContext {
        &self.context
    }
}

/// A successful result means the repository committed its entire transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyCharacterElementChangeResult {
    character_id: String,
    before: CharacterElementSnapshot,
    after: CharacterElementSnapshot,
    committed_event: CharacterElementsChanged,
}

impl ApplyCharacterElementChangeResult {
    pub(crate) fn from_committed(
        committed: CharacterElementApplyResult,
        command: &ApplyCharacterElementChange,
    ) -> Self {
        let before: CharacterElementSnapshot = committed.before.into();
        let after: CharacterElementSnapshot = committed.after.into();
        let committed_event = CharacterElementsChanged {
            character_id: committed.character_id.clone(),
            before: before.clone(),
            after: after.clone(),
            delta: command.delta,
            context: command.context.clone(),
        };

        Self {
            character_id: committed.character_id,
            before,
            after,
            committed_event,
        }
    }

    pub fn character_id(&self) -> &str {
        &self.character_id
    }

    pub fn before(&self) -> &CharacterElementSnapshot {
        &self.before
    }

    pub fn after(&self) -> &CharacterElementSnapshot {
        &self.after
    }

    /// This is a committed business fact, not an already-delivered push.
    pub fn committed_event(&self) -> &CharacterElementsChanged {
        &self.committed_event
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharacterElementChangeFailure {
    CharacterNotFound,
    InvalidAffinityTotal { total: i64 },
    NegativeAffinity { element: &'static str, value: i64 },
    NegativeMastery { element: &'static str, value: i64 },
    ValueOutOfRange { field: &'static str, value: i64 },
    RepositoryUnavailable,
    OutcomeUnknown,
    RepositoryFailure,
}

impl CharacterElementChangeFailure {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::CharacterNotFound => "CHARACTER_NOT_FOUND",
            Self::InvalidAffinityTotal { .. } => "INVALID_AFFINITY_TOTAL",
            Self::NegativeAffinity { .. } => "NEGATIVE_AFFINITY",
            Self::NegativeMastery { .. } => "NEGATIVE_MASTERY",
            Self::ValueOutOfRange { .. } => "CHARACTER_ELEMENTS_VALUE_OUT_OF_RANGE",
            Self::RepositoryUnavailable => "CHARACTER_ELEMENTS_DB_UNAVAILABLE",
            Self::OutcomeUnknown => "CHARACTER_ELEMENTS_OUTCOME_UNKNOWN",
            Self::RepositoryFailure => "CHARACTER_ELEMENTS_DB_ERROR",
        }
    }
}

impl From<CharacterElementError> for CharacterElementChangeFailure {
    fn from(error: CharacterElementError) -> Self {
        match error {
            CharacterElementError::CharacterNotFound => Self::CharacterNotFound,
            CharacterElementError::InvalidAffinityTotal { total } => {
                Self::InvalidAffinityTotal { total }
            }
            CharacterElementError::NegativeAffinity { element, value } => {
                Self::NegativeAffinity { element, value }
            }
            CharacterElementError::NegativeMastery { element, value } => {
                Self::NegativeMastery { element, value }
            }
            CharacterElementError::ValueOutOfRange { field, value } => {
                Self::ValueOutOfRange { field, value }
            }
            CharacterElementError::DbUnavailable => Self::RepositoryUnavailable,
            CharacterElementError::DbError { .. } => Self::RepositoryFailure,
        }
    }
}
