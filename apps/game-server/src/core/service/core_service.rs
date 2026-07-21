use std::sync::atomic::Ordering;

use redis::AsyncCommands;
use tracing::info;

use crate::core::context::{ConnectionContext, PlayerConnectionHandle, ServiceContext};
use crate::core::online_route::{
    clear_online_route, online_route_ttl_secs, publish_online_route,
    reserve_online_route_authority, session_can_replace,
};
use crate::core::room::OutboundChannel;
use crate::metrics::METRICS;
use crate::pb::{AuthReq, AuthRes, PingRes};
use crate::protocol::{MessageType, Packet};
use crate::protocol_version_policy::{
    CURRENT_CLIENT_PROTOCOL_VERSION, MINIMUM_CLIENT_PROTOCOL_VERSION,
    negotiate_client_protocol_version,
};
use crate::session::SessionState;
use crate::ticket::{validate_ticket_owner, validate_ticket_version, verify_ticket};

pub async fn handle_auth(
    services: &ServiceContext,
    connection: &mut ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = match packet.decode_body::<AuthReq>("INVALID_AUTH_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            connection.queue_error(packet.header.seq, error_code, "invalid auth body")?;
            services
                .db_store
                .append_connection_event(
                    connection.session.id,
                    connection.session.account_player_id.as_deref(),
                    Some(&connection.peer_addr),
                    "invalid_auth_body",
                    Some(serde_json::json!({ "seq": packet.header.seq })),
                )
                .await;
            return Ok(());
        }
    };

    let protocol_decision = negotiate_client_protocol_version(request.client_protocol_version);
    METRICS.record_client_protocol_version(protocol_decision.metric());
    if let Some(rejection) = protocol_decision.rejection() {
        tracing::info!(
            session_id = connection.session.id,
            declared_protocol_version = request.client_protocol_version,
            effective_protocol_version = protocol_decision.effective_version(),
            protocol_version_source = ?protocol_decision.source(),
            error_code = rejection.error_code,
            "game auth rejected by client protocol version policy"
        );
        connection.queue_message(
            MessageType::AuthRes,
            packet.header.seq,
            protocol_rejection_auth_response(rejection),
        )?;
        return Ok(());
    }

    match verify_ticket(&services.config.ticket_secret, &request.ticket) {
        Ok(ticket_payload) => {
            let account_player_id = ticket_payload.account_player_id;
            let character_id = ticket_payload.character_id;
            let world_id = ticket_payload.world_id;
            let ticket_key = format!(
                "{}ticket:{}",
                services.config.redis_key_prefix,
                crate::ticket::hash_ticket(&request.ticket)
            );
            let ticket_version_key = format!(
                "{}player-ticket-version:{}",
                services.config.redis_key_prefix, account_player_id
            );
            let ticket_owner: Option<String> = connection.redis.get(ticket_key).await?;

            if let Err(error_code) =
                validate_ticket_owner(ticket_owner.as_deref(), &account_player_id)
            {
                connection.queue_message(
                    MessageType::AuthRes,
                    packet.header.seq,
                    auth_response(false, String::new(), error_code.to_string()),
                )?;
                services
                    .db_store
                    .append_connection_event_with_identity(
                        connection.session.id,
                        Some(&account_player_id),
                        Some(&account_player_id),
                        Some(&character_id),
                        Some(&connection.peer_addr),
                        "auth_ticket_owner_rejected",
                        Some(serde_json::json!({
                            "seq": packet.header.seq,
                            "errorCode": error_code,
                            "worldId": world_id
                        })),
                    )
                    .await;
                return Ok(());
            }

            let current_ticket_version: Option<u64> =
                connection.redis.get(ticket_version_key).await?;
            if let Err(error_code) =
                validate_ticket_version(ticket_payload.ver, current_ticket_version)
            {
                connection.queue_message(
                    MessageType::AuthRes,
                    packet.header.seq,
                    auth_response(false, String::new(), error_code.to_string()),
                )?;
                services
                    .db_store
                    .append_connection_event_with_identity(
                        connection.session.id,
                        Some(&account_player_id),
                        Some(&account_player_id),
                        Some(&character_id),
                        Some(&connection.peer_addr),
                        "auth_ticket_revoked",
                        Some(serde_json::json!({
                            "seq": packet.header.seq,
                            "errorCode": error_code,
                            "worldId": world_id
                        })),
                    )
                    .await;
                return Ok(());
            }

            let previous_account_player_id = connection.session.account_player_id.clone();
            let previous_character_id = connection.session.character_id.clone();
            let previous_authority = connection.session.online_authority.clone();
            let was_authenticated = connection.session.state == SessionState::Authenticated;
            let _route_guard = services
                .online_route_coordinator
                .lock_account(&account_player_id)
                .await;
            let existing_session_id = services
                .player_registry
                .read()
                .await
                .get_by_account(&account_player_id)
                .map(|handle| handle.session_id);
            if !session_can_replace(existing_session_id, connection.session.id) {
                tracing::warn!(
                    account_player_id = %account_player_id,
                    character_id = %character_id,
                    session_id = connection.session.id,
                    "stale local session authenticated after a newer session; authority reservation skipped"
                );
                connection.queue_message(
                    MessageType::AuthRes,
                    packet.header.seq,
                    auth_response(false, String::new(), "AUTHORITY_SUPERSEDED".to_string()),
                )?;
                *connection.kick_reason.write().await = "new_login".to_string();
                connection.kick_notify.notify_one();
                return Ok(());
            }

            let route_ttl_secs = online_route_ttl_secs(services.config.heartbeat_timeout_secs);
            let authority = match reserve_online_route_authority(
                &mut connection.redis,
                &services.config.redis_key_prefix,
                &character_id,
                &services.config.service_instance_id,
                connection.session.id,
                route_ttl_secs,
            )
            .await
            {
                Ok(authority) => authority,
                Err(error) => {
                    tracing::warn!(
                        character_id = %character_id,
                        instance_id = %services.config.service_instance_id,
                        error = %error,
                        "failed to reserve game online authority"
                    );
                    connection.queue_message(
                        MessageType::AuthRes,
                        packet.header.seq,
                        auth_response(
                            false,
                            String::new(),
                            "AUTHORITY_BACKEND_UNAVAILABLE".to_string(),
                        ),
                    )?;
                    return Ok(());
                }
            };
            match publish_online_route(
                &mut connection.redis,
                &services.config.redis_key_prefix,
                &character_id,
                &services.config.service_instance_id,
                connection.session.id,
                &authority,
                route_ttl_secs,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(
                        character_id = %character_id,
                        instance_id = %services.config.service_instance_id,
                        generation = authority.generation,
                        "game online authority was superseded before route publication"
                    );
                    connection.queue_message(
                        MessageType::AuthRes,
                        packet.header.seq,
                        auth_response(false, String::new(), "AUTHORITY_SUPERSEDED".to_string()),
                    )?;
                    *connection.kick_reason.write().await = "authority_changed".to_string();
                    connection.kick_notify.notify_one();
                    return Ok(());
                }
                Err(error) => {
                    tracing::warn!(
                        character_id = %character_id,
                        instance_id = %services.config.service_instance_id,
                        error = %error,
                        "failed to publish game online route"
                    );
                    connection.queue_message(
                        MessageType::AuthRes,
                        packet.header.seq,
                        auth_response(
                            false,
                            String::new(),
                            "AUTHORITY_BACKEND_UNAVAILABLE".to_string(),
                        ),
                    )?;
                    return Ok(());
                }
            }

            if let Some(previous_character_id) = previous_character_id.as_deref()
                && previous_account_player_id.as_deref() == Some(account_player_id.as_str())
                && previous_character_id != character_id
                && let Some(previous_authority) = previous_authority.as_ref()
                && let Err(error) = clear_online_route(
                    &mut connection.redis,
                    &services.config.redis_key_prefix,
                    previous_character_id,
                    &services.config.service_instance_id,
                    connection.session.id,
                    previous_authority,
                )
                .await
            {
                tracing::warn!(
                    character_id = %previous_character_id,
                    instance_id = %services.config.service_instance_id,
                    error = %error,
                    "failed to clear previous game online route"
                );
            }

            connection.session.set_authenticated_identity(
                account_player_id.clone(),
                character_id.clone(),
                world_id,
            );
            connection.session.set_online_authority(authority.clone());
            let old_handle =
                services
                    .player_registry
                    .write()
                    .await
                    .insert_by_account(PlayerConnectionHandle {
                        account_player_id: account_player_id.clone(),
                        character_id: character_id.clone(),
                        kick_notify: connection.kick_notify.clone(),
                        session_id: connection.session.id,
                        online_authority: authority.clone(),
                        outbound: OutboundChannel::new(
                            connection.tx.clone(),
                            connection.close_state.clone(),
                        ),
                        kick_reason: connection.kick_reason.clone(),
                    });
            drop(_route_guard);

            if let Some(previous_account_player_id) = previous_account_player_id.as_deref()
                && previous_account_player_id != account_player_id
            {
                let _previous_route_guard = services
                    .online_route_coordinator
                    .lock_account(previous_account_player_id)
                    .await;
                let removed = services
                    .player_registry
                    .write()
                    .await
                    .remove_by_account_if_session(
                        previous_account_player_id,
                        connection.session.id,
                    );
                if removed.is_some()
                    && let Some(previous_character_id) = previous_character_id.as_deref()
                    && let Some(previous_authority) = previous_authority.as_ref()
                    && let Err(error) = clear_online_route(
                        &mut connection.redis,
                        &services.config.redis_key_prefix,
                        previous_character_id,
                        &services.config.service_instance_id,
                        connection.session.id,
                        previous_authority,
                    )
                    .await
                {
                    tracing::warn!(error = %error, "failed to clear previous account route");
                }
            }

            if !was_authenticated {
                let online_players =
                    services.online_player_count.fetch_add(1, Ordering::Relaxed) + 1;
                METRICS.set_online_players(online_players);
            }
            info!(
                session_id = connection.session.id,
                account_player_id = %account_player_id,
                character_id = %character_id,
                generation = authority.generation,
                world_id = ?world_id,
                "player authenticated"
            );
            connection.queue_message(
                MessageType::AuthRes,
                packet.header.seq,
                auth_response(true, account_player_id.clone(), String::new()),
            )?;
            services
                .db_store
                .append_connection_event_with_identity(
                    connection.session.id,
                    Some(&account_player_id),
                    Some(&account_player_id),
                    Some(&character_id),
                    Some(&connection.peer_addr),
                    "auth_success",
                    Some(serde_json::json!({
                        "seq": packet.header.seq,
                        "accountPlayerId": account_player_id,
                        "characterId": character_id,
                        "worldId": world_id,
                        "authorityGeneration": authority.generation
                    })),
                )
                .await;
            if let Some(old_handle) = old_handle {
                if old_handle.session_id != connection.session.id {
                    info!(
                        account_player_id = %account_player_id,
                        old_character_id = %old_handle.character_id,
                        new_character_id = %character_id,
                        old_session_id = old_handle.session_id,
                        new_session_id = connection.session.id,
                        "kicking old connection on same account"
                    );
                    *old_handle.kick_reason.write().await = "new_login".to_string();
                    old_handle.kick_notify.notify_one();
                }
            }
        }
        Err(error_code) => {
            connection.queue_message(
                MessageType::AuthRes,
                packet.header.seq,
                auth_response(false, String::new(), error_code.to_string()),
            )?;
            services
                .db_store
                .append_connection_event(
                    connection.session.id,
                    None,
                    Some(&connection.peer_addr),
                    "auth_failed",
                    Some(serde_json::json!({
                        "seq": packet.header.seq,
                        "errorCode": error_code
                    })),
                )
                .await;
        }
    }

    Ok(())
}

fn auth_response(ok: bool, player_id: String, error_code: String) -> AuthRes {
    AuthRes {
        ok,
        player_id,
        error_code,
        server_protocol_version: CURRENT_CLIENT_PROTOCOL_VERSION,
        minimum_client_protocol_version: MINIMUM_CLIENT_PROTOCOL_VERSION,
        upgrade_message: String::new(),
        upgrade_url: String::new(),
    }
}

fn protocol_rejection_auth_response(
    rejection: crate::protocol_version_policy::ClientProtocolVersionRejection,
) -> AuthRes {
    AuthRes {
        upgrade_message: rejection.upgrade_message.to_string(),
        upgrade_url: rejection.upgrade_url.to_string(),
        ..auth_response(false, String::new(), rejection.error_code.to_string())
    }
}

pub fn handle_ping(connection: &ConnectionContext, packet: &Packet) -> Result<(), std::io::Error> {
    connection.queue_message(
        MessageType::PingRes,
        packet.header.seq,
        PingRes {
            server_time: crate::server::current_unix_ms(),
        },
    )
}
