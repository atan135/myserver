//! Session kick subscriber
//!
//! Subscribes to NATS subject "myserver.session.kick.*" and notifies
//! the corresponding player connection to disconnect.

use futures_util::StreamExt;
use tracing::{error, info, warn};

use crate::core::context::PlayerRegistry;

#[derive(Debug, serde::Deserialize)]
struct SessionKickEvent {
    player_id: String,
    reason: Option<String>,
}

/// Subscribe to session kick events with automatic reconnection.
pub async fn subscribe_session_kicks(nats_url: String, player_registry: PlayerRegistry) {
    loop {
        match run_subscriber(&nats_url, &player_registry).await {
            Ok(()) => {
                info!("kick subscriber completed normally");
                break;
            }
            Err(e) => {
                error!("kick subscriber error: {}, reconnecting in 5s", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }
    }
}

async fn run_subscriber(
    nats_url: &str,
    player_registry: &PlayerRegistry,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = async_nats::connect(nats_url).await?;
    let mut subscriber = client.subscribe("myserver.session.kick.*").await?;
    info!("subscribed to myserver.session.kick.* subject");

    while let Some(msg) = subscriber.next().await {
        let payload = match std::str::from_utf8(msg.payload.as_ref()) {
            Ok(payload) => payload,
            Err(error) => {
                warn!(subject = %msg.subject, error = %error, "invalid session kick payload");
                continue;
            }
        };
        let event: SessionKickEvent = match serde_json::from_str(payload) {
            Ok(event) => event,
            Err(error) => {
                warn!(subject = %msg.subject, error = %error, "failed to parse session kick payload");
                continue;
            }
        };

        if event.player_id.is_empty() {
            warn!(subject = %msg.subject, "empty player_id in session kick event");
            continue;
        }

        let registry = player_registry.read().await;
        if let Some((notify, session_id)) = registry.get(&event.player_id) {
            info!(
                player_id = %event.player_id,
                session_id = session_id,
                reason = ?event.reason,
                "received session kick event, notifying connection"
            );
            notify.notify_one();
        } else {
            info!(
                player_id = %event.player_id,
                reason = ?event.reason,
                "received session kick event, player not on this server"
            );
        }
    }

    Ok(())
}
