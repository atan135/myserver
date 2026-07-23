//! The controlled public surface for permanent character element state.
//!
//! Domain implementation stays private so callers depend on these re-exports
//! rather than a module-internal path.

mod domain;

pub use domain::{
    CharacterElementApplyResult, CharacterElementChange, CharacterElementChangeSource,
    CharacterElementError, CharacterElements, ElementDeltas, ElementValues,
};
