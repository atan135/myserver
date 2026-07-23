//! The controlled public surface for permanent character element state.
//!
//! Callers use this module's API and facade. Domain and application details
//! remain module implementation details.

// The server switches to this API in stage 6; keep the contract warning-free
// while it is intentionally not yet wired into runtime assembly.
#[allow(dead_code)]
mod api;
#[allow(dead_code)]
pub(crate) mod application;
mod domain;

#[allow(unused_imports)]
pub use api::{
    ApplyCharacterElementChange, ApplyCharacterElementChangeResult,
    CharacterElementChangeContextError, CharacterElementChangeFailure, CharacterElementDelta,
    CharacterElementFacade, CharacterElementSnapshot, CharacterElementsChanged, ElementDelta,
    ElementSnapshot, GetCharacterElements, GetCharacterElementsResult,
    TrustedCharacterElementChangeContext,
};
pub use domain::{
    CharacterElementApplyResult, CharacterElementChange, CharacterElementChangeSource,
    CharacterElementError, CharacterElements, ElementDeltas, ElementValues,
};
