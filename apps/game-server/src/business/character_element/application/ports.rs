use std::future::Future;
use std::pin::Pin;

use crate::business::character_element::domain::{
    CharacterElementApplyResult, CharacterElementChange, CharacterElementChangeSource,
    CharacterElementError, CharacterElements,
};

pub(crate) type RepositoryFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The read outcome distinguishes a missing character from repository failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CharacterElementsRead {
    Found(CharacterElements),
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CharacterElementRepositoryReadError {
    Unavailable,
    Failure,
}

/// Input for the one atomic persistence operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplyCharacterElementChangeInTransaction {
    character_id: String,
    change: CharacterElementChange,
    source: CharacterElementChangeSource,
    reason: Option<String>,
}

impl ApplyCharacterElementChangeInTransaction {
    pub(crate) fn new(
        character_id: String,
        change: CharacterElementChange,
        source: CharacterElementChangeSource,
        reason: Option<String>,
    ) -> Self {
        Self {
            character_id,
            change,
            source,
            reason,
        }
    }

    pub(crate) fn character_id(&self) -> &str {
        &self.character_id
    }

    pub(crate) fn change(&self) -> CharacterElementChange {
        self.change
    }

    pub(crate) fn source(&self) -> &CharacterElementChangeSource {
        &self.source
    }

    pub(crate) fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

/// `OutcomeUnknown` means the adapter cannot prove whether commit happened.
/// Callers must not publish a success event or enqueue a success push for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CharacterElementRepositoryApplyError {
    Rejected(CharacterElementError),
    Unavailable,
    OutcomeUnknown,
    Failure,
}

/// Persistence contract for permanent character element state.
///
/// `apply_change` must lock the target character, load its state, apply the
/// domain rule, update all eight columns, insert the audit log, and commit as
/// one transaction. It may return `CharacterElementApplyResult` only after
/// that commit succeeds. A failed or unknown commit must use an error variant.
pub(crate) trait CharacterElementRepository: Send + Sync {
    fn get<'a>(
        &'a self,
        character_id: &'a str,
    ) -> RepositoryFuture<'a, Result<CharacterElementsRead, CharacterElementRepositoryReadError>>;

    fn apply_change<'a>(
        &'a self,
        request: ApplyCharacterElementChangeInTransaction,
    ) -> RepositoryFuture<
        'a,
        Result<CharacterElementApplyResult, CharacterElementRepositoryApplyError>,
    >;
}
