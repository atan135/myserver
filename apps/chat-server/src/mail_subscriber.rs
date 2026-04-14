//! Mail notification subscriber
//!
//! Subscribes to Redis Pub/Sub channel "mail:notify:*" and pushes
//! mail notifications to connected chat clients.

use futures_util::StreamExt;
use tracing::{error, info, warn};

use crate::chat_service::ChatSessionMap;
use crate::chat_server::MessageType;
use crate::protocol::{encode_body, OutboundMessage};
use crate::proto::chat::MailNotifyPush;

/// Mail notification payload from pubsub
#[derive(Debug, serde::Deserialize)]
struct MailNotification {
    mail_id: String,
    title: String,
    from: String,
    #[serde(rename = "type")]
    mail_type: String,
    created_at: i64,
}

/// Subscribe to mail:notify:* channel and push notifications to players
pub async fn subscribe_mail_notifications(
    client: redis::Client,
    sessions: ChatSessionMap,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        match run_subscriber(client.clone(), sessions.clone()).await {
            Ok(()) => {
                info!("mail subscriber completed normally");
                break;
            }
            Err(e) => {
                error!("mail subscriber error: {}, reconnecting in 5s", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
            }
        }
    }
    Ok(())
}

async fn run_subscriber(
    client: redis::Client,
    sessions: ChatSessionMap,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut pubsub = client.get_async_pubsub().await?;
    pubsub.psubscribe("mail:notify:*").await?;
    info!("subscribed to mail:notify:* channel");

    let mut msg_stream = pubsub.on_message();
    while let Some(msg) = msg_stream.next().await {
        // Get the channel to extract player_id
        let channel: String = msg.get_channel_name().to_string();
        // Channel format: "mail:notify:{player_id}"
        let player_id = channel
            .strip_prefix("mail:notify:")
            .unwrap_or("");

        let payload: Result<String, _> = msg.get_payload();
        match payload {
            Ok(payload_str) => {
                if let Err(e) = handle_notification(&sessions, player_id, &payload_str).await {
                    warn!("failed to handle mail notification: {}", e);
                }
            }
            Err(e) => {
                warn!("failed to get notification payload: {}", e);
            }
        }
    }

    Ok(())
}

async fn handle_notification(
    sessions: &ChatSessionMap,
    player_id: &str,
    payload: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if player_id.is_empty() {
        warn!("empty player_id in mail notification");
        return Ok(());
    }

    let notification: MailNotification = serde_json::from_str(payload)
        .map_err(|e| format!("failed to parse notification: {}", e))?;

    info!(
        mail_id = %notification.mail_id,
        title = %notification.title,
        from = %notification.from,
        to = %player_id,
        "received mail notification"
    );

    // Push notification to the player if online
    push_mail_to_player(sessions, player_id, &notification).await;

    Ok(())
}

/// Push a mail notification to a specific player
async fn push_mail_to_player(
    sessions: &ChatSessionMap,
    player_id: &str,
    notification: &MailNotification,
) {
    let push = MailNotifyPush {
        mail_id: notification.mail_id.clone(),
        title: notification.title.clone(),
        from_player_id: notification.from.clone(),
        mail_type: notification.mail_type.clone(),
        created_at: notification.created_at,
    };

    let body = encode_body(&push);
    let msg = OutboundMessage {
        message_type: MessageType::MailNotifyPush as u16,
        seq: 0,
        body,
    };

    if let Some(sender) = sessions.read().await.get(player_id) {
        if let Err(e) = sender.send(msg) {
            error!("failed to push mail notification to {}: {}", player_id, e);
        } else {
            info!("pushed mail notification to player {}", player_id);
        }
    } else {
        info!("player {} not online, skipping notification", player_id);
    }
}
