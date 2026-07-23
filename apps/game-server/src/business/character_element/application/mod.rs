pub(crate) mod ports;

use std::sync::Arc;

use ports::{
    ApplyCharacterElementChangeInTransaction, CharacterElementRepository,
    CharacterElementRepositoryApplyError, CharacterElementRepositoryReadError,
    CharacterElementsRead,
};

use crate::business::character_element::domain::{
    CharacterElementApplyResult, CharacterElementChange, CharacterElementChangeSource,
    CharacterElementError, CharacterElements,
};

#[derive(Clone)]
pub(crate) struct CharacterElementApplicationService {
    repository: Arc<dyn CharacterElementRepository>,
}

impl CharacterElementApplicationService {
    pub(crate) fn new(repository: Arc<dyn CharacterElementRepository>) -> Self {
        Self { repository }
    }

    pub(crate) async fn get_character_elements(
        &self,
        character_id: &str,
    ) -> Result<CharacterElements, CharacterElementApplicationError> {
        match self.repository.get(character_id).await {
            Ok(CharacterElementsRead::Found(elements)) => Ok(elements),
            Ok(CharacterElementsRead::Missing) => Err(CharacterElementApplicationError::Rejected(
                CharacterElementError::CharacterNotFound,
            )),
            Err(CharacterElementRepositoryReadError::Unavailable) => {
                Err(CharacterElementApplicationError::RepositoryUnavailable)
            }
            Err(CharacterElementRepositoryReadError::Failure) => {
                Err(CharacterElementApplicationError::RepositoryFailure)
            }
        }
    }

    pub(crate) async fn apply_character_element_change(
        &self,
        character_id: &str,
        change: CharacterElementChange,
        source: CharacterElementChangeSource,
        reason: Option<String>,
    ) -> Result<CharacterElementApplyResult, CharacterElementApplicationError> {
        let request = ApplyCharacterElementChangeInTransaction::new(
            character_id.to_string(),
            change,
            source,
            reason,
        );

        self.repository
            .apply_change(request)
            .await
            .map_err(CharacterElementApplicationError::from)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CharacterElementApplicationError {
    Rejected(CharacterElementError),
    RepositoryUnavailable,
    OutcomeUnknown,
    RepositoryFailure,
}

impl From<CharacterElementRepositoryApplyError> for CharacterElementApplicationError {
    fn from(error: CharacterElementRepositoryApplyError) -> Self {
        match error {
            CharacterElementRepositoryApplyError::Rejected(error) => Self::Rejected(error),
            CharacterElementRepositoryApplyError::Unavailable => Self::RepositoryUnavailable,
            CharacterElementRepositoryApplyError::OutcomeUnknown => Self::OutcomeUnknown,
            CharacterElementRepositoryApplyError::Failure => Self::RepositoryFailure,
        }
    }
}

#[cfg(test)]
mod tests;
