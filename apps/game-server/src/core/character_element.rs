use std::sync::Arc;

use crate::business::character_element::application::ports::{
    ApplyCharacterElementChangeInTransaction, CharacterElementRepository,
    CharacterElementRepositoryApplyError, CharacterElementRepositoryReadError,
    CharacterElementsRead,
};
use crate::session::AuthenticatedSessionIdentity;

pub use crate::adapters::persistence::character_element_repository::PgCharacterElementStore;
/// Temporary compatibility exports for callers that have not yet migrated to
/// `business::character_element`. Remove this forwarding layer in stage 8.
pub use crate::business::character_element::{
    CharacterElementApplyResult, CharacterElementChange, CharacterElementChangeSource,
    CharacterElementError, CharacterElements, ElementDeltas, ElementValues,
};

/// Transitional compatibility service for callers that have not moved to the
/// public business facade. Remove this service in stage 8.
#[derive(Clone)]
pub struct CharacterElementService {
    repository: Arc<dyn CharacterElementRepository>,
    closeable_store: Option<PgCharacterElementStore>,
    #[cfg(test)]
    in_memory_repository:
        Option<crate::adapters::persistence::character_element_repository::InMemoryCharacterElementRepository>,
}

impl CharacterElementService {
    pub fn new(store: PgCharacterElementStore) -> Self {
        Self {
            repository: Arc::new(store.clone()),
            closeable_store: Some(store),
            #[cfg(test)]
            in_memory_repository: None,
        }
    }

    pub async fn get_elements_for_identity(
        &self,
        identity: &AuthenticatedSessionIdentity,
    ) -> Result<CharacterElements, CharacterElementError> {
        match self.repository.get(&identity.character_id).await {
            Ok(CharacterElementsRead::Found(elements)) => Ok(elements),
            Ok(CharacterElementsRead::Missing) => Err(CharacterElementError::CharacterNotFound),
            Err(CharacterElementRepositoryReadError::Unavailable) => {
                Err(CharacterElementError::DbUnavailable)
            }
            Err(CharacterElementRepositoryReadError::Failure) => {
                Err(CharacterElementError::DbError {
                    message: "character element repository read failed".to_string(),
                })
            }
        }
    }

    pub async fn apply_change(
        &self,
        character_id: &str,
        change: CharacterElementChange,
        source: CharacterElementChangeSource,
        reason: Option<&str>,
    ) -> Result<CharacterElementApplyResult, CharacterElementError> {
        let request = ApplyCharacterElementChangeInTransaction::new(
            character_id.to_string(),
            change,
            source,
            reason.map(str::to_string),
        );

        match self.repository.apply_change(request).await {
            Ok(result) => Ok(result),
            Err(CharacterElementRepositoryApplyError::Rejected(error)) => Err(error),
            Err(CharacterElementRepositoryApplyError::Unavailable) => {
                Err(CharacterElementError::DbUnavailable)
            }
            Err(CharacterElementRepositoryApplyError::OutcomeUnknown) => {
                Err(CharacterElementError::DbError {
                    message: "character element transaction commit outcome is unknown".to_string(),
                })
            }
            Err(CharacterElementRepositoryApplyError::Failure) => {
                Err(CharacterElementError::DbError {
                    message: "character element repository write failed".to_string(),
                })
            }
        }
    }

    pub async fn close(&self) {
        if let Some(store) = &self.closeable_store {
            store.close().await;
        }
    }

    #[cfg(test)]
    pub(crate) fn new_in_memory() -> Self {
        let repository = crate::adapters::persistence::character_element_repository::InMemoryCharacterElementRepository::default();
        Self {
            repository: Arc::new(repository.clone()),
            closeable_store: None,
            in_memory_repository: Some(repository),
        }
    }

    #[cfg(test)]
    pub(crate) async fn set_elements(&self, elements: CharacterElements) {
        if let Some(repository) = &self.in_memory_repository {
            repository.set_elements(elements).await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn applied_change_logs(
        &self,
    ) -> Vec<crate::adapters::persistence::character_element_repository::MemoryCharacterElementLog>
    {
        match &self.in_memory_repository {
            Some(repository) => repository.applied_change_logs().await,
            None => Vec::new(),
        }
    }
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

    fn identity(character_id: &str) -> AuthenticatedSessionIdentity {
        AuthenticatedSessionIdentity {
            account_player_id: "plr_0000000000001".to_string(),
            character_id: character_id.to_string(),
            world_id: Some(0),
        }
    }

    #[tokio::test]
    async fn identity_query_reads_only_the_authenticated_characters_elements() {
        let service = CharacterElementService::new_in_memory();
        service.set_elements(base_elements()).await;
        service
            .set_elements(CharacterElements {
                character_id: "chr_0000000000002".to_string(),
                affinity: ElementValues::new(1000, 2000, 3000, 4000),
                mastery: ElementValues::new(1, 2, 3, 4),
            })
            .await;

        let selected = service
            .get_elements_for_identity(&identity("chr_0000000000002"))
            .await
            .expect("current authenticated character should be queried");
        assert_eq!(selected.character_id, "chr_0000000000002");
        assert_eq!(
            selected.affinity,
            ElementValues::new(1000, 2000, 3000, 4000)
        );

        let error = service
            .get_elements_for_identity(&identity("chr_0000000000003"))
            .await
            .expect_err(
                "unknown authenticated character should not read another characters values",
            );
        assert_eq!(error, CharacterElementError::CharacterNotFound);
    }

    #[tokio::test]
    async fn apply_change_returns_snapshots_and_records_complete_audit_context() {
        let service = CharacterElementService::new_in_memory();
        let before = base_elements();
        service.set_elements(before.clone()).await;
        let change = CharacterElementChange::new(
            ElementDeltas::new(-100, 100, 0, 0),
            ElementDeltas::new(0, 5, -10, 0),
        );
        let source = CharacterElementChangeSource::new("quest")
            .with_source_id("quest_0001")
            .with_operator("player", "plr_0000000000001");

        let result = service
            .apply_change(
                &before.character_id,
                change,
                source.clone(),
                Some("quest reward"),
            )
            .await
            .expect("valid change should be persisted");

        let expected_after = CharacterElements {
            character_id: before.character_id.clone(),
            affinity: ElementValues::new(2400, 2600, 2500, 2500),
            mastery: ElementValues::new(10, 25, 20, 40),
        };
        assert_eq!(result.character_id, before.character_id);
        assert_eq!(result.before, before);
        assert_eq!(result.after, expected_after);

        let logs = service.applied_change_logs().await;
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].character_id, result.character_id);
        assert_eq!(logs[0].change, change);
        assert_eq!(logs[0].source, source);
        assert_eq!(logs[0].reason.as_deref(), Some("quest reward"));
        assert_eq!(logs[0].before, result.before);
        assert_eq!(logs[0].after, result.after);
    }

    #[tokio::test]
    async fn rejected_or_missing_character_change_leaves_memory_state_and_logs_unchanged() {
        let service = CharacterElementService::new_in_memory();
        let before = base_elements();
        service.set_elements(before.clone()).await;

        let invalid_error = service
            .apply_change(
                &before.character_id,
                CharacterElementChange::new(ElementDeltas::new(1, 0, 0, 0), ElementDeltas::zero()),
                CharacterElementChangeSource::new("quest"),
                Some("invalid affinity total"),
            )
            .await
            .expect_err("unbalanced affinity change should fail");
        assert_eq!(invalid_error.error_code(), "INVALID_AFFINITY_TOTAL");
        assert_eq!(
            service
                .get_elements_for_identity(&identity(&before.character_id))
                .await
                .expect("rejected change must not mutate current values"),
            before
        );

        let missing_error = service
            .apply_change(
                "chr_missing",
                CharacterElementChange::zero(),
                CharacterElementChangeSource::new("quest"),
                None,
            )
            .await
            .expect_err("missing character should fail before a log is written");
        assert_eq!(missing_error, CharacterElementError::CharacterNotFound);
        assert!(service.applied_change_logs().await.is_empty());
    }

    #[tokio::test]
    async fn disabled_store_returns_explicit_db_unavailable_error() {
        let service = CharacterElementService::new(PgCharacterElementStore::new_disabled());
        let identity = AuthenticatedSessionIdentity {
            account_player_id: "plr_0000000000001".to_string(),
            character_id: "chr_0000000000001".to_string(),
            world_id: Some(0),
        };

        let get_error = service
            .get_elements_for_identity(&identity)
            .await
            .unwrap_err();
        assert_eq!(get_error.error_code(), "CHARACTER_ELEMENTS_DB_UNAVAILABLE");

        let apply_error = service
            .apply_change(
                &identity.character_id,
                CharacterElementChange::zero(),
                CharacterElementChangeSource::new("system"),
                Some("unit-test"),
            )
            .await
            .unwrap_err();
        assert_eq!(
            apply_error.error_code(),
            "CHARACTER_ELEMENTS_DB_UNAVAILABLE"
        );
    }
}
