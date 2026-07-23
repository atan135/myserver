use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::ports::{
    ApplyCharacterElementChangeInTransaction, CharacterElementRepository,
    CharacterElementRepositoryApplyError, CharacterElementRepositoryReadError,
    CharacterElementsRead, RepositoryFuture,
};
use crate::business::character_element::{
    ApplyCharacterElementChange, CharacterElementChangeFailure, CharacterElementDelta,
    CharacterElementFacade, CharacterElements, ElementDelta, ElementValues, GetCharacterElements,
    TrustedCharacterElementChangeContext,
};
use crate::business::character_element::{CharacterElementApplyResult, CharacterElementError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeMode {
    Ready,
    Unavailable,
    OutcomeUnknown,
}

struct FakeCharacterElementRepository {
    values: Mutex<BTreeMap<String, CharacterElements>>,
    mode: Mutex<FakeMode>,
}

impl FakeCharacterElementRepository {
    fn with_elements(elements: impl IntoIterator<Item = CharacterElements>) -> Self {
        Self {
            values: Mutex::new(
                elements
                    .into_iter()
                    .map(|elements| (elements.character_id.clone(), elements))
                    .collect(),
            ),
            mode: Mutex::new(FakeMode::Ready),
        }
    }

    fn set_mode(&self, mode: FakeMode) {
        *self.mode.lock().expect("fake repository mode lock") = mode;
    }

    fn current(&self, character_id: &str) -> Option<CharacterElements> {
        self.values
            .lock()
            .expect("fake repository values lock")
            .get(character_id)
            .cloned()
    }
}

impl CharacterElementRepository for FakeCharacterElementRepository {
    fn get<'a>(
        &'a self,
        character_id: &'a str,
    ) -> RepositoryFuture<'a, Result<CharacterElementsRead, CharacterElementRepositoryReadError>>
    {
        Box::pin(async move {
            match *self.mode.lock().expect("fake repository mode lock") {
                FakeMode::Ready | FakeMode::OutcomeUnknown => Ok(self
                    .current(character_id)
                    .map(CharacterElementsRead::Found)
                    .unwrap_or(CharacterElementsRead::Missing)),
                FakeMode::Unavailable => Err(CharacterElementRepositoryReadError::Unavailable),
            }
        })
    }

    fn apply_change<'a>(
        &'a self,
        request: ApplyCharacterElementChangeInTransaction,
    ) -> RepositoryFuture<
        'a,
        Result<CharacterElementApplyResult, CharacterElementRepositoryApplyError>,
    > {
        Box::pin(async move {
            match *self.mode.lock().expect("fake repository mode lock") {
                FakeMode::Unavailable => {
                    return Err(CharacterElementRepositoryApplyError::Unavailable);
                }
                FakeMode::OutcomeUnknown => {
                    return Err(CharacterElementRepositoryApplyError::OutcomeUnknown);
                }
                FakeMode::Ready => {}
            }

            let mut values = self.values.lock().expect("fake repository values lock");
            let before = values.get(request.character_id()).cloned().ok_or(
                CharacterElementRepositoryApplyError::Rejected(
                    CharacterElementError::CharacterNotFound,
                ),
            )?;
            let after = before
                .apply_change(request.change())
                .map_err(CharacterElementRepositoryApplyError::Rejected)?;

            values.insert(after.character_id.clone(), after.clone());
            Ok(CharacterElementApplyResult {
                character_id: after.character_id.clone(),
                before,
                after,
            })
        })
    }
}

fn base_elements() -> CharacterElements {
    CharacterElements {
        character_id: "chr_0000000000001".to_string(),
        affinity: ElementValues::new(2500, 2500, 2500, 2500),
        mastery: ElementValues::new(10, 20, 30, 40),
    }
}

fn context() -> TrustedCharacterElementChangeContext {
    TrustedCharacterElementChangeContext::try_new(
        "quest",
        Some("quest_0001".to_string()),
        Some("system".to_string()),
        Some("scheduler_0001".to_string()),
        Some("quest completion".to_string()),
    )
    .expect("test context must be valid")
}

fn facade(repository: Arc<dyn CharacterElementRepository>) -> CharacterElementFacade {
    CharacterElementFacade::new(repository)
}

#[tokio::test]
async fn facade_returns_a_stable_snapshot_for_a_successful_query() {
    let repository = Arc::new(FakeCharacterElementRepository::with_elements([
        base_elements(),
    ]));
    let facade = facade(repository);

    let result = facade
        .get_character_elements(GetCharacterElements::new("chr_0000000000001"))
        .await
        .expect("existing character should be returned");

    assert_eq!(result.elements().character_id(), "chr_0000000000001");
    assert_eq!(result.elements().affinity().earth(), 2500);
    assert_eq!(result.elements().mastery().wind(), 40);
}

#[tokio::test]
async fn facade_reports_a_missing_character_without_a_repository_failure() {
    let repository = Arc::new(FakeCharacterElementRepository::with_elements([]));
    let facade = facade(repository);

    let error = facade
        .get_character_elements(GetCharacterElements::new("chr_missing"))
        .await
        .expect_err("missing character should be a business failure");

    assert_eq!(error, CharacterElementChangeFailure::CharacterNotFound);
    assert_eq!(error.error_code(), "CHARACTER_NOT_FOUND");
}

#[tokio::test]
async fn committed_change_returns_snapshots_and_the_committed_fact() {
    let repository = Arc::new(FakeCharacterElementRepository::with_elements([
        base_elements(),
    ]));
    let facade = facade(repository);
    let command = ApplyCharacterElementChange::new(
        "chr_0000000000001",
        CharacterElementDelta::new(
            ElementDelta::new(-100, 100, 0, 0),
            ElementDelta::new(0, 5, -10, 0),
        ),
        context(),
    );

    let result = facade
        .apply_character_element_change(command)
        .await
        .expect("legal change must be committed by the fake repository");

    assert_eq!(result.before().affinity().earth(), 2500);
    assert_eq!(result.after().affinity().earth(), 2400);
    assert_eq!(result.after().mastery().fire(), 25);
    assert_eq!(result.committed_event().character_id(), "chr_0000000000001");
    assert_eq!(result.committed_event().context().source_type(), "quest");
    assert_eq!(
        result.committed_event().context().reason(),
        Some("quest completion")
    );
}

#[tokio::test]
async fn invalid_change_is_rejected_without_mutating_the_fake_repository() {
    let repository = Arc::new(FakeCharacterElementRepository::with_elements([
        base_elements(),
    ]));
    let facade = facade(repository.clone());
    let command = ApplyCharacterElementChange::new(
        "chr_0000000000001",
        CharacterElementDelta::new(ElementDelta::new(1, 0, 0, 0), ElementDelta::zero()),
        context(),
    );

    let error = facade
        .apply_character_element_change(command)
        .await
        .expect_err("unbalanced affinity must fail before fake persistence changes state");

    assert_eq!(
        error,
        CharacterElementChangeFailure::InvalidAffinityTotal { total: 10001 }
    );
    assert_eq!(
        repository
            .current("chr_0000000000001")
            .expect("existing character should remain")
            .affinity
            .earth,
        2500
    );
}

#[tokio::test]
async fn unavailable_repository_never_produces_a_success_result_or_event() {
    let repository = Arc::new(FakeCharacterElementRepository::with_elements([
        base_elements(),
    ]));
    repository.set_mode(FakeMode::Unavailable);
    let facade = facade(repository);

    let error = facade
        .apply_character_element_change(ApplyCharacterElementChange::new(
            "chr_0000000000001",
            CharacterElementDelta::zero(),
            context(),
        ))
        .await
        .expect_err("unavailable repository must not return a committed event");

    assert_eq!(error, CharacterElementChangeFailure::RepositoryUnavailable);
}

#[tokio::test]
async fn unknown_commit_outcome_never_produces_a_success_result_or_event() {
    let repository = Arc::new(FakeCharacterElementRepository::with_elements([
        base_elements(),
    ]));
    repository.set_mode(FakeMode::OutcomeUnknown);
    let facade = facade(repository);

    let error = facade
        .apply_character_element_change(ApplyCharacterElementChange::new(
            "chr_0000000000001",
            CharacterElementDelta::zero(),
            context(),
        ))
        .await
        .expect_err("unknown commit outcome must not return a committed event");

    assert_eq!(error, CharacterElementChangeFailure::OutcomeUnknown);
    assert_eq!(error.error_code(), "CHARACTER_ELEMENTS_OUTCOME_UNKNOWN");
}

#[test]
fn trusted_context_keeps_existing_audit_lengths_and_normalizes_reason() {
    let long_reason = format!("  {}  ", "a".repeat(300));
    let context = TrustedCharacterElementChangeContext::try_new(
        "gm",
        Some("debug-character-elements".to_string()),
        Some("player_debug".to_string()),
        Some("plr_0000000000001".to_string()),
        Some(long_reason),
    )
    .expect("legacy reason behavior should trim and cap at 255 characters");

    assert_eq!(
        context
            .reason()
            .expect("reason should remain")
            .chars()
            .count(),
        255
    );
    assert_eq!(
        TrustedCharacterElementChangeContext::try_new("x".repeat(33), None, None, None, None,)
            .expect_err("source_type must respect the audit column bound")
            .error_code(),
        "CHARACTER_ELEMENTS_CHANGE_CONTEXT_TOO_LONG"
    );
}
