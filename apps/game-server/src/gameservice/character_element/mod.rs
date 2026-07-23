use tracing::info;

use crate::business::character_element::{
    ApplyCharacterElementChange, CharacterElementChangeFailure, CharacterElementDelta,
    CharacterElementSnapshot, CharacterElementsChanged, ElementDelta, GetCharacterElements,
    TrustedCharacterElementChangeContext,
};
use crate::core::character_push::{CharacterPushSource, queue_character_push};
use crate::core::context::{ConnectionContext, ServiceContext};
use crate::pb::{
    CharacterElements as PbCharacterElements, CharacterElementsChangePush,
    DebugApplyCharacterElementChangeReq, DebugApplyCharacterElementChangeRes,
    ElementValues as PbElementValues, GetCharacterElementsRes,
};
use crate::protocol::{MessageType, Packet};
use crate::session::AuthenticatedSessionIdentity;

const DEBUG_SOURCE_TYPE: &str = "gm";
const DEBUG_SOURCE_ID: &str = "debug-character-elements";
const DEBUG_OPERATOR_TYPE: &str = "player_debug";
const DEFAULT_DEBUG_REASON: &str = "mock-client character element debug";

pub async fn handle_get_character_elements(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };

    info!(
        session_id = connection.session.id,
        account_player_id = %identity.account_player_id,
        player_id = %identity.account_player_id,
        character_id = %identity.character_id,
        world_id = ?identity.world_id,
        "handle_get_character_elements"
    );

    let response = match services
        .character_element_facade
        .get_character_elements(GetCharacterElements::new(identity.character_id.clone()))
        .await
    {
        Ok(result) => {
            let elements = result.elements();
            GetCharacterElementsRes {
                ok: true,
                error_code: String::new(),
                character_id: elements.character_id().to_string(),
                elements: Some(to_pb_snapshot(elements)),
            }
        }
        Err(error) => GetCharacterElementsRes {
            ok: false,
            error_code: protocol_error_code(&error).to_string(),
            character_id: identity.character_id,
            elements: None,
        },
    };

    connection.queue_message(
        MessageType::GetCharacterElementsRes,
        packet.header.seq,
        response,
    )?;

    Ok(())
}

pub async fn handle_debug_apply_character_element_change(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet
        .decode_body::<DebugApplyCharacterElementChangeReq>("INVALID_CHARACTER_ELEMENT_CHANGE_BODY")
    {
        Ok(value) => value,
        Err(error_code) => {
            queue_debug_apply_response(
                connection,
                packet.header.seq,
                false,
                error_code,
                &identity.character_id,
                None,
            )?;
            return Ok(());
        }
    };

    if !debug_token_matches(&request.debug_token) {
        queue_debug_apply_response(
            connection,
            packet.header.seq,
            false,
            "CHARACTER_ELEMENT_DEBUG_FORBIDDEN",
            &identity.character_id,
            None,
        )?;
        return Ok(());
    }

    let reason = normalize_debug_reason(&request.reason);
    let delta = CharacterElementDelta::new(
        to_delta(request.affinity_delta.as_ref()),
        to_delta(request.mastery_delta.as_ref()),
    );
    let context = match debug_change_context(&identity, reason.clone()) {
        Ok(context) => context,
        Err(error) => {
            queue_debug_apply_response(
                connection,
                packet.header.seq,
                false,
                protocol_context_error_code(&error),
                &identity.character_id,
                None,
            )?;
            return Ok(());
        }
    };

    info!(
        session_id = connection.session.id,
        account_player_id = %identity.account_player_id,
        player_id = %identity.account_player_id,
        character_id = %identity.character_id,
        world_id = ?identity.world_id,
        affinity_earth_delta = request.affinity_delta.as_ref().map_or(0, |value| value.earth),
        affinity_fire_delta = request.affinity_delta.as_ref().map_or(0, |value| value.fire),
        affinity_water_delta = request.affinity_delta.as_ref().map_or(0, |value| value.water),
        affinity_wind_delta = request.affinity_delta.as_ref().map_or(0, |value| value.wind),
        mastery_earth_delta = request.mastery_delta.as_ref().map_or(0, |value| value.earth),
        mastery_fire_delta = request.mastery_delta.as_ref().map_or(0, |value| value.fire),
        mastery_water_delta = request.mastery_delta.as_ref().map_or(0, |value| value.water),
        mastery_wind_delta = request.mastery_delta.as_ref().map_or(0, |value| value.wind),
        reason = %reason,
        "handle_debug_apply_character_element_change"
    );

    let result = services
        .character_element_facade
        .apply_character_element_change(ApplyCharacterElementChange::new(
            identity.character_id.clone(),
            delta,
            context,
        ))
        .await;

    match result {
        Ok(result) => {
            queue_debug_apply_response(
                connection,
                packet.header.seq,
                true,
                "",
                result.character_id(),
                Some(&result),
            )?;
            queue_character_element_push(
                services,
                connection,
                &identity,
                result.committed_event(),
                CharacterPushSource::new(
                    DEBUG_SOURCE_TYPE,
                    DEBUG_SOURCE_ID,
                    "element_change",
                    reason.as_str(),
                ),
            )
            .await?;
        }
        Err(error) => {
            queue_debug_apply_response(
                connection,
                packet.header.seq,
                false,
                protocol_error_code(&error),
                &identity.character_id,
                None,
            )?;
        }
    }

    Ok(())
}

fn debug_change_context(
    identity: &AuthenticatedSessionIdentity,
    reason: String,
) -> Result<
    TrustedCharacterElementChangeContext,
    crate::business::character_element::CharacterElementChangeContextError,
> {
    TrustedCharacterElementChangeContext::try_new(
        DEBUG_SOURCE_TYPE,
        Some(DEBUG_SOURCE_ID.to_string()),
        Some(DEBUG_OPERATOR_TYPE.to_string()),
        Some(identity.account_player_id.clone()),
        Some(reason),
    )
}

fn protocol_error_code(error: &CharacterElementChangeFailure) -> &'static str {
    match error {
        // The former core service only exposed a generic database error when
        // COMMIT acknowledgement was interrupted. Keep that player protocol
        // contract while preserving the richer failure for non-protocol users.
        CharacterElementChangeFailure::OutcomeUnknown => "CHARACTER_ELEMENTS_DB_ERROR",
        _ => error.error_code(),
    }
}

fn protocol_context_error_code(
    _error: &crate::business::character_element::CharacterElementChangeContextError,
) -> &'static str {
    // This is only reachable from trusted server-derived audit fields. Before
    // the facade validated those fields, the same condition surfaced as a
    // database error to the player protocol.
    "CHARACTER_ELEMENTS_DB_ERROR"
}

fn normalize_debug_reason(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_DEBUG_REASON.to_string();
    }

    trimmed.chars().take(255).collect()
}

fn debug_token_matches(actual: &str) -> bool {
    let actual = actual.trim();
    !actual.is_empty()
        && std::env::var("MYSERVER_CHARACTER_ELEMENT_DEBUG_TOKEN")
            .ok()
            .is_some_and(|expected| !expected.trim().is_empty() && expected.trim() == actual)
}

fn queue_debug_apply_response(
    connection: &ConnectionContext,
    seq: u32,
    ok: bool,
    error_code: &str,
    character_id: &str,
    result: Option<&crate::business::character_element::ApplyCharacterElementChangeResult>,
) -> Result<(), std::io::Error> {
    connection.queue_message(
        MessageType::DebugApplyCharacterElementChangeRes,
        seq,
        DebugApplyCharacterElementChangeRes {
            ok,
            error_code: error_code.to_string(),
            character_id: character_id.to_string(),
            before: result.map(|value| to_pb_snapshot(value.before())),
            after: result.map(|value| to_pb_snapshot(value.after())),
        },
    )
}

fn to_delta(value: Option<&PbElementValues>) -> ElementDelta {
    value
        .map(|value| ElementDelta::new(value.earth, value.fire, value.water, value.wind))
        .unwrap_or_else(ElementDelta::zero)
}

fn to_pb_values(earth: i32, fire: i32, water: i32, wind: i32) -> PbElementValues {
    PbElementValues {
        earth,
        fire,
        water,
        wind,
    }
}

fn to_pb_snapshot(elements: &CharacterElementSnapshot) -> PbCharacterElements {
    let affinity = elements.affinity();
    let mastery = elements.mastery();
    PbCharacterElements {
        affinity: Some(to_pb_values(
            affinity.earth(),
            affinity.fire(),
            affinity.water(),
            affinity.wind(),
        )),
        mastery: Some(to_pb_values(
            mastery.earth(),
            mastery.fire(),
            mastery.water(),
            mastery.wind(),
        )),
    }
}

pub(crate) async fn queue_character_element_push(
    services: &ServiceContext,
    connection: &ConnectionContext,
    identity: &AuthenticatedSessionIdentity,
    changed: &CharacterElementsChanged,
    source: CharacterPushSource,
) -> Result<(), std::io::Error> {
    let record = services
        .character_push_service
        .record_elements_change(
            changed.character_id(),
            source,
            CharacterElementsChangePush {
                meta: None,
                before: Some(to_pb_snapshot(changed.before())),
                after: Some(to_pb_snapshot(changed.after())),
            },
        )
        .await;
    queue_character_push(connection, &identity.character_id, &record)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::adapters::persistence::InMemoryCharacterElementRepository;
    use crate::business::character_element::{
        CharacterElementFacade, CharacterElements, ElementValues,
    };

    fn identity() -> AuthenticatedSessionIdentity {
        AuthenticatedSessionIdentity {
            account_player_id: "plr_0000000000001".to_string(),
            character_id: "chr_0000000000001".to_string(),
            world_id: Some(0),
        }
    }

    #[test]
    fn debug_token_requires_dedicated_non_empty_match_after_trimming() {
        unsafe {
            std::env::set_var("GAME_ADMIN_TOKEN", "global-admin-token");
            std::env::set_var(
                "MYSERVER_CHARACTER_ELEMENT_DEBUG_TOKEN",
                " element-debug-token ",
            );
        }
        assert!(debug_token_matches("element-debug-token"));
        assert!(!debug_token_matches("global-admin-token"));
        assert!(!debug_token_matches(""));
        assert!(!debug_token_matches("other"));
        unsafe {
            std::env::remove_var("GAME_ADMIN_TOKEN");
            std::env::remove_var("MYSERVER_CHARACTER_ELEMENT_DEBUG_TOKEN");
        }
    }

    #[test]
    fn empty_debug_reason_uses_controlled_default() {
        assert_eq!(normalize_debug_reason("   "), DEFAULT_DEBUG_REASON);
    }

    #[test]
    fn protocol_keeps_the_legacy_database_code_for_unknown_commit_outcomes() {
        assert_eq!(
            protocol_error_code(&CharacterElementChangeFailure::OutcomeUnknown),
            "CHARACTER_ELEMENTS_DB_ERROR"
        );
        assert_eq!(
            protocol_error_code(&CharacterElementChangeFailure::CharacterNotFound),
            "CHARACTER_NOT_FOUND"
        );
    }

    #[test]
    fn protocol_keeps_context_validation_internal() {
        let error = TrustedCharacterElementChangeContext::try_new(
            "gm",
            None,
            Some("player_debug".to_string()),
            None,
            None,
        )
        .expect_err("unpaired operator data should be rejected");

        assert_eq!(
            protocol_context_error_code(&error),
            "CHARACTER_ELEMENTS_DB_ERROR"
        );
    }

    #[test]
    fn debug_change_context_preserves_server_owned_identity() {
        let identity = identity();
        let context = debug_change_context(&identity, "quest reward".to_string())
            .expect("fixed debug context should be valid");

        assert_eq!(context.source_type(), DEBUG_SOURCE_TYPE);
        assert_eq!(context.source_id(), Some(DEBUG_SOURCE_ID));
        assert_eq!(context.operator_type(), Some(DEBUG_OPERATOR_TYPE));
        assert_eq!(context.operator_id(), Some("plr_0000000000001"));
        assert_eq!(context.reason(), Some("quest reward"));
    }

    #[tokio::test]
    async fn facade_snapshots_map_to_protocol_values() {
        let repository = InMemoryCharacterElementRepository::default();
        let identity = identity();
        repository
            .set_elements(CharacterElements {
                character_id: identity.character_id.clone(),
                affinity: ElementValues::new(2500, 2500, 2500, 2500),
                mastery: ElementValues::new(0, 0, 0, 0),
            })
            .await;
        let facade = CharacterElementFacade::new(Arc::new(repository));
        let context = debug_change_context(&identity, "quest reward".to_string())
            .expect("fixed debug context should be valid");
        let result = facade
            .apply_character_element_change(ApplyCharacterElementChange::new(
                identity.character_id.clone(),
                CharacterElementDelta::new(
                    ElementDelta::new(-100, 100, 0, 0),
                    ElementDelta::new(0, 5, 0, 0),
                ),
                context,
            ))
            .await
            .expect("legal change should commit in the fake repository");

        let before = to_pb_snapshot(result.before());
        let after = to_pb_snapshot(result.after());
        assert_eq!(before.affinity.expect("affinity").earth, 2500);
        assert_eq!(after.affinity.expect("affinity").fire, 2600);
        assert_eq!(after.mastery.expect("mastery").fire, 5);
    }
}
