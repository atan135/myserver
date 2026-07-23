//! The controlled public surface for permanent character element state.
//!
//! Callers use this module's API and facade. Domain and application details
//! remain module implementation details.

mod api;
pub(super) mod application;
mod domain;

// Persistence adapters are the sole crate-level consumers of these ports.
// Other modules must use the facade and contracts re-exported below.
pub(crate) use application::ports::{
    ApplyCharacterElementChangeInTransaction, CharacterElementRepository,
    CharacterElementRepositoryApplyError, CharacterElementRepositoryReadError,
    CharacterElementsRead, RepositoryFuture,
};

#[allow(unused_imports)]
pub use api::{
    ApplyCharacterElementChange, ApplyCharacterElementChangeResult,
    CharacterElementChangeContextError, CharacterElementChangeFailure, CharacterElementDelta,
    CharacterElementFacade, CharacterElementSnapshot, CharacterElementsChanged, ElementDelta,
    ElementSnapshot, GetCharacterElements, GetCharacterElementsResult,
    TrustedCharacterElementChangeContext,
};
#[allow(unused_imports)]
pub use domain::{
    CharacterElementApplyResult, CharacterElementChange, CharacterElementChangeSource,
    CharacterElementError, CharacterElements, ElementDeltas, ElementValues,
};
