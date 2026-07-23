use std::sync::Arc;

use crate::business::character_element::api::contracts::{
    ApplyCharacterElementChange, ApplyCharacterElementChangeResult, CharacterElementChangeFailure,
    GetCharacterElements, GetCharacterElementsResult,
};
use crate::business::character_element::application::ports::CharacterElementRepository;
use crate::business::character_element::application::{
    CharacterElementApplicationError, CharacterElementApplicationService,
};

/// The only public entry point for permanent character element use cases.
#[derive(Clone)]
pub struct CharacterElementFacade {
    application: CharacterElementApplicationService,
}

impl CharacterElementFacade {
    pub(crate) fn new(repository: Arc<dyn CharacterElementRepository>) -> Self {
        Self {
            application: CharacterElementApplicationService::new(repository),
        }
    }

    pub async fn get_character_elements(
        &self,
        query: GetCharacterElements,
    ) -> Result<GetCharacterElementsResult, CharacterElementChangeFailure> {
        self.application
            .get_character_elements(query.character_id())
            .await
            .map(GetCharacterElementsResult::new)
            .map_err(CharacterElementChangeFailure::from)
    }

    pub async fn apply_character_element_change(
        &self,
        command: ApplyCharacterElementChange,
    ) -> Result<ApplyCharacterElementChangeResult, CharacterElementChangeFailure> {
        let (source, reason) = command.context().to_domain_parts();
        let committed = self
            .application
            .apply_character_element_change(
                command.character_id(),
                command.delta().into(),
                source,
                reason,
            )
            .await
            .map_err(CharacterElementChangeFailure::from)?;

        Ok(ApplyCharacterElementChangeResult::from_committed(
            committed, &command,
        ))
    }

    /// Validates and projects a change without writing it or creating a
    /// committed event. Callers use this when they must validate a group of
    /// independent rewards before applying any of them.
    pub fn preview_character_element_change(
        &self,
        before: &crate::business::character_element::CharacterElementSnapshot,
        delta: crate::business::character_element::CharacterElementDelta,
    ) -> Result<
        crate::business::character_element::CharacterElementSnapshot,
        CharacterElementChangeFailure,
    > {
        before
            .to_domain()
            .apply_change(delta.into())
            .map(Into::into)
            .map_err(CharacterElementChangeFailure::from)
    }
}

impl From<CharacterElementApplicationError> for CharacterElementChangeFailure {
    fn from(error: CharacterElementApplicationError) -> Self {
        match error {
            CharacterElementApplicationError::Rejected(error) => error.into(),
            CharacterElementApplicationError::RepositoryUnavailable => Self::RepositoryUnavailable,
            CharacterElementApplicationError::OutcomeUnknown => Self::OutcomeUnknown,
            CharacterElementApplicationError::RepositoryFailure => Self::RepositoryFailure,
        }
    }
}
