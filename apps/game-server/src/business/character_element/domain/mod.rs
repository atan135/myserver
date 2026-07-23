use serde::{Deserialize, Serialize};

pub const AFFINITY_TOTAL: i32 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementValues {
    pub earth: i32,
    pub fire: i32,
    pub water: i32,
    pub wind: i32,
}

impl ElementValues {
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

    pub fn total(self) -> i64 {
        i64::from(self.earth) + i64::from(self.fire) + i64::from(self.water) + i64::from(self.wind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementDeltas {
    pub earth: i32,
    pub fire: i32,
    pub water: i32,
    pub wind: i32,
}

impl ElementDeltas {
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterElements {
    pub character_id: String,
    pub affinity: ElementValues,
    pub mastery: ElementValues,
}

impl CharacterElements {
    pub fn apply_change(
        &self,
        change: CharacterElementChange,
    ) -> Result<Self, CharacterElementError> {
        let after = Self {
            character_id: self.character_id.clone(),
            affinity: apply_deltas("affinity", self.affinity, change.affinity)?,
            mastery: apply_deltas("mastery", self.mastery, change.mastery)?,
        };

        validate_elements(&after)?;
        Ok(after)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CharacterElementChange {
    pub affinity: ElementDeltas,
    pub mastery: ElementDeltas,
}

impl CharacterElementChange {
    pub const fn new(affinity: ElementDeltas, mastery: ElementDeltas) -> Self {
        Self { affinity, mastery }
    }

    pub const fn zero() -> Self {
        Self::new(ElementDeltas::zero(), ElementDeltas::zero())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterElementChangeSource {
    pub source_type: String,
    pub source_id: Option<String>,
    pub operator_type: Option<String>,
    pub operator_id: Option<String>,
}

impl CharacterElementChangeSource {
    pub fn new(source_type: impl Into<String>) -> Self {
        Self {
            source_type: source_type.into(),
            source_id: None,
            operator_type: None,
            operator_id: None,
        }
    }

    pub fn with_source_id(mut self, source_id: impl Into<String>) -> Self {
        self.source_id = Some(source_id.into());
        self
    }

    pub fn with_operator(
        mut self,
        operator_type: impl Into<String>,
        operator_id: impl Into<String>,
    ) -> Self {
        self.operator_type = Some(operator_type.into());
        self.operator_id = Some(operator_id.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterElementApplyResult {
    pub character_id: String,
    pub before: CharacterElements,
    pub after: CharacterElements,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CharacterElementError {
    CharacterNotFound,
    InvalidAffinityTotal { total: i64 },
    NegativeAffinity { element: &'static str, value: i64 },
    NegativeMastery { element: &'static str, value: i64 },
    ValueOutOfRange { field: &'static str, value: i64 },
    DbUnavailable,
    DbError { message: String },
}

impl CharacterElementError {
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::CharacterNotFound => "CHARACTER_NOT_FOUND",
            Self::InvalidAffinityTotal { .. } => "INVALID_AFFINITY_TOTAL",
            Self::NegativeAffinity { .. } => "NEGATIVE_AFFINITY",
            Self::NegativeMastery { .. } => "NEGATIVE_MASTERY",
            Self::ValueOutOfRange { .. } => "CHARACTER_ELEMENTS_VALUE_OUT_OF_RANGE",
            Self::DbUnavailable => "CHARACTER_ELEMENTS_DB_UNAVAILABLE",
            Self::DbError { .. } => "CHARACTER_ELEMENTS_DB_ERROR",
        }
    }
}

impl std::fmt::Display for CharacterElementError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CharacterNotFound => write!(formatter, "character not found"),
            Self::InvalidAffinityTotal { total } => {
                write!(
                    formatter,
                    "affinity total must be {AFFINITY_TOTAL}, got {total}"
                )
            }
            Self::NegativeAffinity { element, value } => {
                write!(
                    formatter,
                    "affinity {element} must be non-negative, got {value}"
                )
            }
            Self::NegativeMastery { element, value } => {
                write!(
                    formatter,
                    "mastery {element} must be non-negative, got {value}"
                )
            }
            Self::ValueOutOfRange { field, value } => {
                write!(formatter, "{field} value is out of range: {value}")
            }
            Self::DbUnavailable => write!(formatter, "character elements database is unavailable"),
            Self::DbError { message } => {
                write!(formatter, "character elements database error: {message}")
            }
        }
    }
}

impl std::error::Error for CharacterElementError {}

fn apply_deltas(
    group: &'static str,
    current: ElementValues,
    delta: ElementDeltas,
) -> Result<ElementValues, CharacterElementError> {
    Ok(ElementValues::new(
        checked_add(group, "earth", current.earth, delta.earth)?,
        checked_add(group, "fire", current.fire, delta.fire)?,
        checked_add(group, "water", current.water, delta.water)?,
        checked_add(group, "wind", current.wind, delta.wind)?,
    ))
}

fn checked_add(
    group: &'static str,
    element: &'static str,
    current: i32,
    delta: i32,
) -> Result<i32, CharacterElementError> {
    let value = i64::from(current) + i64::from(delta);
    if value < i64::from(i32::MIN) || value > i64::from(i32::MAX) {
        return Err(CharacterElementError::ValueOutOfRange {
            field: match group {
                "affinity" => match element {
                    "earth" => "affinity_earth",
                    "fire" => "affinity_fire",
                    "water" => "affinity_water",
                    "wind" => "affinity_wind",
                    _ => "affinity",
                },
                "mastery" => match element {
                    "earth" => "mastery_earth",
                    "fire" => "mastery_fire",
                    "water" => "mastery_water",
                    "wind" => "mastery_wind",
                    _ => "mastery",
                },
                _ => "element",
            },
            value,
        });
    }

    Ok(value as i32)
}

fn validate_elements(elements: &CharacterElements) -> Result<(), CharacterElementError> {
    validate_affinity(elements.affinity)?;
    validate_mastery(elements.mastery)
}

fn validate_affinity(affinity: ElementValues) -> Result<(), CharacterElementError> {
    let total = affinity.total();
    if total != i64::from(AFFINITY_TOTAL) {
        return Err(CharacterElementError::InvalidAffinityTotal { total });
    }

    for (element, value) in [
        ("earth", affinity.earth),
        ("fire", affinity.fire),
        ("water", affinity.water),
        ("wind", affinity.wind),
    ] {
        if value < 0 {
            return Err(CharacterElementError::NegativeAffinity {
                element,
                value: i64::from(value),
            });
        }
    }

    Ok(())
}

fn validate_mastery(mastery: ElementValues) -> Result<(), CharacterElementError> {
    for (element, value) in [
        ("earth", mastery.earth),
        ("fire", mastery.fire),
        ("water", mastery.water),
        ("wind", mastery.wind),
    ] {
        if value < 0 {
            return Err(CharacterElementError::NegativeMastery {
                element,
                value: i64::from(value),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_elements() -> CharacterElements {
        CharacterElements {
            character_id: "chr_0000000000001".to_string(),
            affinity: ElementValues::new(2500, 2500, 2500, 2500),
            mastery: ElementValues::new(10, 20, 30, 40),
        }
    }

    #[test]
    fn apply_change_accepts_balanced_affinity_and_non_negative_mastery() {
        let before = base_elements();
        let change = CharacterElementChange::new(
            ElementDeltas::new(-500, 500, 0, 0),
            ElementDeltas::new(5, 0, -10, 20),
        );

        let after = before
            .apply_change(change)
            .expect("valid element change should apply");

        assert_eq!(after.affinity, ElementValues::new(2000, 3000, 2500, 2500));
        assert_eq!(after.affinity.total(), i64::from(AFFINITY_TOTAL));
        assert_eq!(after.mastery, ElementValues::new(15, 20, 20, 60));
    }

    #[test]
    fn apply_change_rejects_invalid_affinity_total() {
        let before = base_elements();
        let change =
            CharacterElementChange::new(ElementDeltas::new(1, 0, 0, 0), ElementDeltas::zero());

        let error = before.apply_change(change).unwrap_err();

        assert_eq!(error.error_code(), "INVALID_AFFINITY_TOTAL");
        assert_eq!(
            error,
            CharacterElementError::InvalidAffinityTotal { total: 10001 }
        );
    }

    #[test]
    fn apply_change_rejects_negative_mastery() {
        let before = base_elements();
        let change =
            CharacterElementChange::new(ElementDeltas::zero(), ElementDeltas::new(0, 0, -31, 0));

        let error = before.apply_change(change).unwrap_err();

        assert_eq!(error.error_code(), "NEGATIVE_MASTERY");
        assert_eq!(
            error,
            CharacterElementError::NegativeMastery {
                element: "water",
                value: -1
            }
        );
    }

    #[test]
    fn apply_change_rejects_negative_affinity_even_when_total_stays_balanced() {
        let before = base_elements();
        let change = CharacterElementChange::new(
            ElementDeltas::new(-3000, 3000, 0, 0),
            ElementDeltas::zero(),
        );

        let error = before.apply_change(change).unwrap_err();

        assert_eq!(error.error_code(), "NEGATIVE_AFFINITY");
        assert_eq!(
            error,
            CharacterElementError::NegativeAffinity {
                element: "earth",
                value: -500
            }
        );
    }

    #[test]
    fn apply_change_rejects_integer_overflow_before_db_update() {
        let before = CharacterElements {
            mastery: ElementValues::new(i32::MAX, 0, 0, 0),
            ..base_elements()
        };
        let change =
            CharacterElementChange::new(ElementDeltas::zero(), ElementDeltas::new(1, 0, 0, 0));

        let error = before.apply_change(change).unwrap_err();

        assert_eq!(error.error_code(), "CHARACTER_ELEMENTS_VALUE_OUT_OF_RANGE");
    }

    #[test]
    fn character_element_error_codes_are_stable() {
        let cases = [
            (
                CharacterElementError::CharacterNotFound,
                "CHARACTER_NOT_FOUND",
            ),
            (
                CharacterElementError::InvalidAffinityTotal { total: 9999 },
                "INVALID_AFFINITY_TOTAL",
            ),
            (
                CharacterElementError::NegativeAffinity {
                    element: "earth",
                    value: -1,
                },
                "NEGATIVE_AFFINITY",
            ),
            (
                CharacterElementError::NegativeMastery {
                    element: "fire",
                    value: -1,
                },
                "NEGATIVE_MASTERY",
            ),
            (
                CharacterElementError::ValueOutOfRange {
                    field: "mastery_fire",
                    value: i64::from(i32::MAX) + 1,
                },
                "CHARACTER_ELEMENTS_VALUE_OUT_OF_RANGE",
            ),
            (
                CharacterElementError::DbUnavailable,
                "CHARACTER_ELEMENTS_DB_UNAVAILABLE",
            ),
            (
                CharacterElementError::DbError {
                    message: "connection reset".to_string(),
                },
                "CHARACTER_ELEMENTS_DB_ERROR",
            ),
        ];

        for (error, expected_code) in cases {
            assert_eq!(error.error_code(), expected_code);
        }
    }
}
