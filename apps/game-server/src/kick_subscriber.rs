//! Session kick subscriber
//!
//! Subscribes to Redis Pub/Sub channel "{prefix}session:kick:*" and notifies
//! the corresponding player connection to disconnect.

use futures_util::StreamExt;
use tracing::{error, info, warn};

use crate::core::context::PlayerRegistry;

/// Subscribe to session kick events with automatic reconnection.
pub async fn subscribe_session_kicks(
    client: redis::Client,
    key_prefix: String,
    player_registry: PlayerRegistry,
) {
    loop {
        match run_subscriber(&client, &key_prefix, &player_registry).await {
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
    client: &redis::Client,
    key_prefix: &str,
    player_registry: &PlayerRegistry,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut pubsub = client.get_async_pubsub().await?;
    let pattern = format!("{}session:kick:*", key_prefix);
    pubsub.psubscribe(&pattern).await?;
    info!(pattern = %pattern, "subscribed to session kick channel");

    let channel_prefix = format!("{}session:kick:", key_prefix);
    let mut msg_stream = pubsub.on_message();

    while let Some(msg) = msg_stream.next().await {
        let channel: String = msg.get_channel_name().to_string();
        let player_id = match channel.strip_prefix(&channel_prefix) {
            Some(pid) if !pid.is_empty() => pid,
            _ => {
                warn!(channel = %channel, "unexpected kick channel format");
                continue;
            }
        };

        let registry = player_registry.read().await;
        if let Some((notify, session_id)) = registry.get(player_id) {
            info!(
                player_id = %player_id,
                session_id = session_id,
                "received session kick event, notifying connection"
            );
            notify.notify_one();
        } else {
            info!(
                player_id = %player_id,
                "received session kick event, player not on this server"
            );
        }
    }

    Ok(())
}
