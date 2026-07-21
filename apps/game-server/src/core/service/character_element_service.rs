use tracing::info;

use crate::core::character_element::{
    CharacterElementApplyResult, CharacterElementChange, CharacterElementChangeSource,
    CharacterElementError, ElementDeltas,
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
        .character_element_service
        .get_elements_for_identity(&identity)
        .await
    {
        Ok(elements) => GetCharacterElementsRes {
            ok: true,
            error_code: String::new(),
            character_id: elements.character_id.clone(),
            elements: Some(to_pb_elements(&elements)),
        },
        Err(error) => GetCharacterElementsRes {
            ok: false,
            error_code: error.error_code().to_string(),
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
    let change = CharacterElementChange::new(
        to_deltas(request.affinity_delta.as_ref()),
        to_deltas(request.mastery_delta.as_ref()),
    );
    let source = debug_change_source(&identity);

    info!(
        session_id = connection.session.id,
        account_player_id = %identity.account_player_id,
        player_id = %identity.account_player_id,
        character_id = %identity.character_id,
        world_id = ?identity.world_id,
        affinity_earth_delta = change.affinity.earth,
        affinity_fire_delta = change.affinity.fire,
        affinity_water_delta = change.affinity.water,
        affinity_wind_delta = change.affinity.wind,
        mastery_earth_delta = change.mastery.earth,
        mastery_fire_delta = change.mastery.fire,
        mastery_water_delta = change.mastery.water,
        mastery_wind_delta = change.mastery.wind,
        reason = %reason,
        "handle_debug_apply_character_element_change"
    );

    let result = services
        .character_element_service
        .apply_change(
            &identity.character_id,
            change,
            source,
            Some(reason.as_str()),
        )
        .await;

    match result {
        Ok(result) => {
            queue_debug_apply_response(
                connection,
                packet.header.seq,
                true,
                "",
                &result.character_id,
                Some(&result),
            )?;
            queue_character_element_push(
                services,
                connection,
                &identity,
                &result,
                CharacterPushSource::new(
                    DEBUG_SOURCE_TYPE,
                    DEBUG_SOURCE_ID,
                    "element_change",
                    reason.as_str(),
                ),
            )
            .await?;
        }
        Err(error) => queue_debug_apply_error(connection, packet.header.seq, &identity, error)?,
    }

    Ok(())
}

fn debug_change_source(identity: &AuthenticatedSessionIdentity) -> CharacterElementChangeSource {
    CharacterElementChangeSource::new(DEBUG_SOURCE_TYPE)
        .with_source_id(DEBUG_SOURCE_ID)
        .with_operator(DEBUG_OPERATOR_TYPE, identity.account_player_id.clone())
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

fn queue_debug_apply_error(
    connection: &ConnectionContext,
    seq: u32,
    identity: &AuthenticatedSessionIdentity,
    error: CharacterElementError,
) -> Result<(), std::io::Error> {
    queue_debug_apply_response(
        connection,
        seq,
        false,
        error.error_code(),
        &identity.character_id,
        None,
    )
}

fn queue_debug_apply_response(
    connection: &ConnectionContext,
    seq: u32,
    ok: bool,
    error_code: &str,
    character_id: &str,
    result: Option<&CharacterElementApplyResult>,
) -> Result<(), std::io::Error> {
    connection.queue_message(
        MessageType::DebugApplyCharacterElementChangeRes,
        seq,
        DebugApplyCharacterElementChangeRes {
            ok,
            error_code: error_code.to_string(),
            character_id: character_id.to_string(),
            before: result.map(|value| to_pb_elements(&value.before)),
            after: result.map(|value| to_pb_elements(&value.after)),
        },
    )
}

fn to_deltas(value: Option<&PbElementValues>) -> ElementDeltas {
    value
        .map(|value| ElementDeltas::new(value.earth, value.fire, value.water, value.wind))
        .unwrap_or_else(ElementDeltas::zero)
}

fn to_pb_values(value: crate::core::character_element::ElementValues) -> PbElementValues {
    PbElementValues {
        earth: value.earth,
        fire: value.fire,
        water: value.water,
        wind: value.wind,
    }
}

fn to_pb_elements(
    elements: &crate::core::character_element::CharacterElements,
) -> PbCharacterElements {
    PbCharacterElements {
        affinity: Some(to_pb_values(elements.affinity)),
        mastery: Some(to_pb_values(elements.mastery)),
    }
}

pub(crate) async fn queue_character_element_push(
    services: &ServiceContext,
    connection: &ConnectionContext,
    identity: &AuthenticatedSessionIdentity,
    result: &CharacterElementApplyResult,
    source: CharacterPushSource,
) -> Result<(), std::io::Error> {
    let record = services
        .character_push_service
        .record_elements_change(
            &identity.character_id,
            source,
            CharacterElementsChangePush {
                meta: None,
                before: Some(to_pb_elements(&result.before)),
                after: Some(to_pb_elements(&result.after)),
            },
        )
        .await;
    queue_character_push(connection, &identity.character_id, &record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_token_requires_dedicated_non_empty_match_after_trimming() {
        unsafe {
            std::env::set_var("GAME_ADMIN_TOKEN", "global-admin-token");
            std::env::set_var("MYSERVER_CHARACTER_ELEMENT_DEBUG_TOKEN", " element-debug-token ");
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
}
