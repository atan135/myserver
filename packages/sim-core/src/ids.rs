//! Stable identifier types and helpers.

use serde::{Deserialize, Serialize};

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct EntityId(u32);

impl EntityId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct FrameId(u32);

impl FrameId {
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct TeamId(u16);

impl TeamId {
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    pub const fn raw(self) -> u16 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_newtypes_expose_raw_values() {
        assert_eq!(EntityId::new(1001).raw(), 1001);
        assert_eq!(FrameId::new(20).raw(), 20);
        assert_eq!(TeamId::new(2).raw(), 2);
    }
}
