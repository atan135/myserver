//! Mail notification subscriber
//!
//! Subscribes to NATS subject "myserver.mail.notify.*" and pushes
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
    player_id: String,
    mail_id: String,
    title: String,
    from: String,
    #[serde(rename = "type")]
    mail_type: String,
    created_at: i64,
}

/// Subscribe to myserver.mail.notify.* and push notifications to players
pub async fn subscribe_mail_notifications(
    nats_url: String,
    sessions: ChatSessionMap,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        match run_subscriber(&nats_url, sessions.clone()).await {
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
    nats_url: &str,
    sessions: ChatSessionMap,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let client = async_nats::connect(nats_url).await?;
    let mut subscriber = client.subscribe("myserver.mail.notify.*").await?;
    info!("subscribed to myserver.mail.notify.* subject");

    while let Some(msg) = subscriber.next().await {
        match std::str::from_utf8(msg.payload.as_ref()) {
            Ok(payload_str) => {
                if let Err(e) = handle_notification(&sessions, payload_str).await {
                    warn!(subject = %msg.subject, "failed to handle mail notification: {}", e);
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
    payload: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let notification: MailNotification = serde_json::from_str(payload)
        .map_err(|e| format!("failed to parse notification: {}", e))?;
    let player_id = notification.player_id.as_str();

    if player_id.is_empty() {
        warn!("empty player_id in mail notification");
        return Ok(());
    }

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
