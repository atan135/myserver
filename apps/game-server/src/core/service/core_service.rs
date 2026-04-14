use std::sync::atomic::Ordering;

use redis::AsyncCommands;
use tracing::info;

use crate::core::context::{ConnectionContext, ServiceContext};
use crate::metrics::METRICS;
use crate::pb::{AuthReq, AuthRes, PingRes};
use crate::protocol::{MessageType, Packet};
use crate::session::SessionState;
use crate::ticket::verify_ticket;

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
                .mysql_store
                .append_connection_event(
                    connection.session.id,
                    connection.session.player_id.as_deref(),
                    Some(&connection.peer_addr),
                    "invalid_auth_body",
                    Some(serde_json::json!({ "seq": packet.header.seq })),
                )
                .await;
            return Ok(());
        }
    };

    match verify_ticket(&services.config.ticket_secret, &request.ticket) {
        Ok(player_id) => {
            let ticket_key = format!(
                "{}ticket:{}",
                services.config.redis_key_prefix,
                crate::ticket::hash_ticket(&request.ticket)
            );
            let ticket_owner: Option<String> = connection.redis.get(ticket_key).await?;

            if ticket_owner.as_deref() != Some(player_id.as_str()) {
                connection.queue_message(
                    MessageType::AuthRes,
                    packet.header.seq,
                    AuthRes {
                        ok: false,
                        player_id: String::new(),
                        error_code: "TICKET_NOT_FOUND".to_string(),
                    },
                )?;
                services
                    .mysql_store
                    .append_connection_event(
                        connection.session.id,
                        Some(&player_id),
                        Some(&connection.peer_addr),
                        "auth_ticket_not_found",
                        Some(serde_json::json!({ "seq": packet.header.seq })),
                    )
                    .await;
                return Ok(());
            }

            let was_authenticated = connection.session.state == SessionState::Authenticated;
            connection.session.state = SessionState::Authenticated;
            connection.session.player_id = Some(player_id.clone());
            if !was_authenticated {
                let online_players =
                    services.online_player_count.fetch_add(1, Ordering::Relaxed) + 1;
                METRICS.set_online_players(online_players);
            }

            info!(
                session_id = connection.session.id,
                player_id = %player_id,
                "player authenticated"
            );

            connection.queue_message(
                MessageType::AuthRes,
                packet.header.seq,
                AuthRes {
                    ok: true,
                    player_id: player_id.clone(),
                    error_code: String::new(),
                },
            )?;
            services
                .mysql_store
                .append_connection_event(
                    connection.session.id,
                    Some(&player_id),
                    Some(&connection.peer_addr),
                    "auth_success",
                    Some(serde_json::json!({ "seq": packet.header.seq })),
                )
                .await;
        }
        Err(error_code) => {
            connection.queue_message(
                MessageType::AuthRes,
                packet.header.seq,
                AuthRes {
                    ok: false,
                    player_id: String::new(),
                    error_code: error_code.to_string(),
                },
            )?;
            services
                .mysql_store
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

pub fn handle_ping(connection: &ConnectionContext, packet: &Packet) -> Result<(), std::io::Error> {
    connection.queue_message(
        MessageType::PingRes,
        packet.header.seq,
        PingRes {
            server_time: crate::server::current_unix_ms(),
        },
    )
}
