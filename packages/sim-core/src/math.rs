//! Deterministic fixed-point math primitives for simulation code.
//!
//! Core simulation state should use the integer-backed types in this module.
//! Floating-point conversion is intentionally limited to render-boundary helpers.

use serde::de;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

pub const FP_SCALE: i64 = 1000;
pub const QUANTIZED_DIR_SCALE: i16 = 1000;
pub const QUANTIZED_DIR_MAX_LEN_SQUARED: i32 = 1_000_000;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
/// Fixed-point simulation scalar backed by raw milli-units.
///
/// `FP_SCALE` raw units represent one simulation unit. Core simulation code
/// should keep values as `Fp`; floating-point conversion is for rendering only.
pub struct Fp(i64);

impl Fp {
    pub const ZERO: Self = Self(0);

    pub const fn from_raw(value: i64) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> i64 {
        self.0
    }

    pub const fn from_i32(value: i32) -> Self {
        Self(value as i64 * FP_SCALE)
    }

    pub const fn from_milli(value: i64) -> Self {
        Self(value)
    }

    /// Converts this value to `f32` for rendering only.
    ///
    /// The returned value must not be fed back into core simulation state or
    /// step logic. Simulation code should keep using raw fixed-point values.
    pub fn to_f32_for_render(self) -> f32 {
        self.0 as f32 / FP_SCALE as f32
    }

    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.0.checked_add(rhs.0).map(Self)
    }

    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        self.0.checked_sub(rhs.0).map(Self)
    }

    pub fn saturating_add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }

    pub fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }

    /// Multiplies by `numerator / denominator` using integer arithmetic.
    ///
    /// Division truncates toward zero. A zero denominator or an `i64` result
    /// overflow returns `None`.
    pub fn mul_ratio(self, numerator: i64, denominator: i64) -> Option<Self> {
        if denominator == 0 {
            return None;
        }

        let value = (self.0 as i128).checked_mul(numerator as i128)? / denominator as i128;
        i64::try_from(value).ok().map(Self)
    }

    /// Divides by `divisor` using integer arithmetic.
    ///
    /// Division truncates toward zero. A zero divisor or an `i64` result
    /// overflow returns `None`.
    pub fn div_i64_trunc(self, divisor: i64) -> Option<Self> {
        if divisor == 0 {
            return None;
        }

        let value = self.0 as i128 / divisor as i128;
        i64::try_from(value).ok().map(Self)
    }

    pub fn clamp(self, min: Self, max: Self) -> Self {
        let low = min.min(max);
        let high = min.max(max);
        Self(self.0.clamp(low.0, high.0))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
/// Two-dimensional fixed-point vector used for simulation-space positions.
///
/// Both axes use the same raw milli-unit scale as `Fp`.
pub struct Vec2Fp {
    pub x: Fp,
    pub y: Fp,
}

impl Vec2Fp {
    pub const fn new(x: Fp, y: Fp) -> Self {
        Self { x, y }
    }

    pub const fn zero() -> Self {
        Self {
            x: Fp::ZERO,
            y: Fp::ZERO,
        }
    }

    pub const fn raw_tuple(self) -> (i64, i64) {
        (self.x.raw(), self.y.raw())
    }

    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        Some(Self {
            x: self.x.checked_add(rhs.x)?,
            y: self.y.checked_add(rhs.y)?,
        })
    }

    pub fn checked_sub(self, rhs: Self) -> Option<Self> {
        Some(Self {
            x: self.x.checked_sub(rhs.x)?,
            y: self.y.checked_sub(rhs.y)?,
        })
    }

    pub fn saturating_add(self, rhs: Self) -> Self {
        Self {
            x: self.x.saturating_add(rhs.x),
            y: self.y.saturating_add(rhs.y),
        }
    }

    pub fn saturating_sub(self, rhs: Self) -> Self {
        Self {
            x: self.x.saturating_sub(rhs.x),
            y: self.y.saturating_sub(rhs.y),
        }
    }

    pub fn clamp(self, min: Self, max: Self) -> Self {
        Self {
            x: self.x.clamp(min.x, max.x),
            y: self.y.clamp(min.y, max.y),
        }
    }

    pub fn distance_squared_raw(self, other: Self) -> Option<i128> {
        let dx = self.x.raw() as i128 - other.x.raw() as i128;
        let dy = self.y.raw() as i128 - other.y.raw() as i128;
        let dx2 = dx.checked_mul(dx)?;
        let dy2 = dy.checked_mul(dy)?;
        dx2.checked_add(dy2)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum QuantizedDirError {
    XOutOfRange { value: i16 },
    YOutOfRange { value: i16 },
    LengthSquaredTooLarge { length_squared: i32 },
}

impl fmt::Display for QuantizedDirError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::XOutOfRange { value } => {
                write!(f, "quantized direction x is out of range: {value}")
            }
            Self::YOutOfRange { value } => {
                write!(f, "quantized direction y is out of range: {value}")
            }
            Self::LengthSquaredTooLarge { length_squared } => write!(
                f,
                "quantized direction length squared is too large: {length_squared}"
            ),
        }
    }
}

impl std::error::Error for QuantizedDirError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
/// Quantized 2D direction accepted by deterministic input and movement.
///
/// Each component is constrained to `-1000..=1000`, and the squared length must
/// not exceed one unit direction at the same scale.
pub struct QuantizedDir {
    x: i16,
    y: i16,
}

impl QuantizedDir {
    pub const ZERO: Self = Self { x: 0, y: 0 };
    pub const RIGHT: Self = Self {
        x: QUANTIZED_DIR_SCALE,
        y: 0,
    };
    pub const LEFT: Self = Self {
        x: -QUANTIZED_DIR_SCALE,
        y: 0,
    };
    pub const UP: Self = Self {
        x: 0,
        y: -QUANTIZED_DIR_SCALE,
    };
    pub const DOWN: Self = Self {
        x: 0,
        y: QUANTIZED_DIR_SCALE,
    };
    pub const UP_RIGHT: Self = Self { x: 707, y: -707 };
    pub const UP_LEFT: Self = Self { x: -707, y: -707 };
    pub const DOWN_RIGHT: Self = Self { x: 707, y: 707 };
    pub const DOWN_LEFT: Self = Self { x: -707, y: 707 };

    pub fn new(x: i16, y: i16) -> Result<Self, QuantizedDirError> {
        validate_quantized_dir(x, y)?;
        Ok(Self { x, y })
    }

    pub const fn x(self) -> i16 {
        self.x
    }

    pub const fn y(self) -> i16 {
        self.y
    }

    pub const fn raw_tuple(self) -> (i16, i16) {
        (self.x, self.y)
    }

    pub const fn length_squared(self) -> i32 {
        let x = self.x as i32;
        let y = self.y as i32;
        x * x + y * y
    }
}

impl Serialize for QuantizedDir {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("QuantizedDir", 2)?;
        state.serialize_field("x", &self.x)?;
        state.serialize_field("y", &self.y)?;
        state.end()
    }
}

impl<'de> Deserialize<'de> for QuantizedDir {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawQuantizedDir {
            x: i16,
            y: i16,
        }

        let raw = RawQuantizedDir::deserialize(deserializer)?;
        Self::new(raw.x, raw.y).map_err(de::Error::custom)
    }
}

fn validate_quantized_dir(x: i16, y: i16) -> Result<(), QuantizedDirError> {
    if !(-QUANTIZED_DIR_SCALE..=QUANTIZED_DIR_SCALE).contains(&x) {
        return Err(QuantizedDirError::XOutOfRange { value: x });
    }

    if !(-QUANTIZED_DIR_SCALE..=QUANTIZED_DIR_SCALE).contains(&y) {
        return Err(QuantizedDirError::YOutOfRange { value: y });
    }

    let length_squared = x as i32 * x as i32 + y as i32 * y as i32;
    if length_squared > QUANTIZED_DIR_MAX_LEN_SQUARED {
        return Err(QuantizedDirError::LengthSquaredTooLarge { length_squared });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fp_constructors_expose_raw_milli_units() {
        assert_eq!(Fp::from_raw(42).raw(), 42);
        assert_eq!(Fp::from_i32(3).raw(), 3_000);
        assert_eq!(Fp::from_i32(-3).raw(), -3_000);
        assert_eq!(Fp::from_milli(707).raw(), 707);
    }

    #[test]
    fn fp_checked_and_saturating_add_sub_are_explicit() {
        assert_eq!(
            Fp::from_milli(700)
                .checked_add(Fp::from_milli(500))
                .unwrap()
                .raw(),
            1_200
        );
        assert_eq!(
            Fp::from_milli(700)
                .checked_sub(Fp::from_milli(1_200))
                .unwrap()
                .raw(),
            -500
        );
        assert_eq!(Fp::from_raw(i64::MAX).checked_add(Fp::from_raw(1)), None);
        assert_eq!(Fp::from_raw(i64::MIN).checked_sub(Fp::from_raw(1)), None);
        assert_eq!(
            Fp::from_raw(i64::MAX).saturating_add(Fp::from_raw(1)).raw(),
            i64::MAX
        );
        assert_eq!(
            Fp::from_raw(i64::MIN).saturating_sub(Fp::from_raw(1)).raw(),
            i64::MIN
        );
    }

    #[test]
    fn fp_ratio_and_division_truncate_toward_zero() {
        assert_eq!(Fp::from_milli(1_001).mul_ratio(1, 2).unwrap().raw(), 500);
        assert_eq!(Fp::from_milli(-1_001).mul_ratio(1, 2).unwrap().raw(), -500);
        assert_eq!(Fp::from_milli(1).mul_ratio(1, 0), None);
        assert_eq!(Fp::from_milli(1_001).div_i64_trunc(2).unwrap().raw(), 500);
        assert_eq!(Fp::from_milli(-1_001).div_i64_trunc(2).unwrap().raw(), -500);
        assert_eq!(Fp::from_milli(1).div_i64_trunc(0), None);
    }

    #[test]
    fn vec2_add_sub_clamp_and_distance_use_raw_units() {
        let a = Vec2Fp::new(Fp::from_milli(1_000), Fp::from_milli(-2_000));
        let b = Vec2Fp::new(Fp::from_milli(500), Fp::from_milli(3_000));

        assert_eq!(a.checked_add(b).unwrap().raw_tuple(), (1_500, 1_000));
        assert_eq!(a.checked_sub(b).unwrap().raw_tuple(), (500, -5_000));
        assert_eq!(
            a.clamp(
                Vec2Fp::new(Fp::from_milli(-100), Fp::from_milli(-1_000)),
                Vec2Fp::new(Fp::from_milli(800), Fp::from_milli(1_000)),
            )
            .raw_tuple(),
            (800, -1_000)
        );
        assert_eq!(a.distance_squared_raw(b), Some(25_250_000));
    }

    #[test]
    fn quantized_dir_accepts_horizontal_and_vertical_unit_directions() {
        assert_eq!(QuantizedDir::new(1_000, 0), Ok(QuantizedDir::RIGHT));
        assert_eq!(QuantizedDir::new(-1_000, 0), Ok(QuantizedDir::LEFT));
        assert_eq!(QuantizedDir::new(0, -1_000), Ok(QuantizedDir::UP));
        assert_eq!(QuantizedDir::new(0, 1_000), Ok(QuantizedDir::DOWN));
        assert_eq!(QuantizedDir::RIGHT.length_squared(), 1_000_000);
        assert_eq!(QuantizedDir::UP.length_squared(), 1_000_000);
    }

    #[test]
    fn quantized_dir_accepts_707_diagonal_unit_directions() {
        assert_eq!(QuantizedDir::new(707, -707), Ok(QuantizedDir::UP_RIGHT));
        assert_eq!(QuantizedDir::new(-707, -707), Ok(QuantizedDir::UP_LEFT));
        assert_eq!(QuantizedDir::UP_RIGHT.length_squared(), 999_698);
    }

    #[test]
    fn quantized_dir_rejects_out_of_range_or_overlong_directions() {
        assert_eq!(
            QuantizedDir::new(1_001, 0),
            Err(QuantizedDirError::XOutOfRange { value: 1_001 })
        );
        assert_eq!(
            QuantizedDir::new(0, -1_001),
            Err(QuantizedDirError::YOutOfRange { value: -1_001 })
        );
        assert_eq!(
            QuantizedDir::new(1_000, 1_000),
            Err(QuantizedDirError::LengthSquaredTooLarge {
                length_squared: 2_000_000
            })
        );
    }

    #[test]
    fn quantized_dir_deserialization_reuses_validation() {
        let valid: QuantizedDir = serde_json::from_str(r#"{"x":707,"y":707}"#).unwrap();
        assert_eq!(valid, QuantizedDir::DOWN_RIGHT);
        assert!(serde_json::from_str::<QuantizedDir>(r#"{"x":1000,"y":1000}"#).is_err());
    }

    #[test]
    fn render_conversion_is_read_only_boundary() {
        let value = Fp::from_milli(1_234);
        let rendered = value.to_f32_for_render();

        assert!((rendered - 1.234).abs() < 0.000_001);
        assert_eq!(value.raw(), 1_234);
        assert_eq!(value.checked_add(Fp::from_milli(1)).unwrap().raw(), 1_235);

        let pos = Vec2Fp::new(value, Fp::from_milli(-2_000));
        let _render_only = (pos.x.to_f32_for_render(), pos.y.to_f32_for_render());
        assert_eq!(pos.raw_tuple(), (1_234, -2_000));
    }
}
