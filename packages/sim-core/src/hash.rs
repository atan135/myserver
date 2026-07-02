//! Deterministic state hashing placeholders.

use crate::ids::FrameId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimHash {
    pub frame: FrameId,
    pub value: u64,
}

impl SimHash {
    pub const fn placeholder(frame: FrameId) -> Self {
        Self { frame, value: 0 }
    }
}
