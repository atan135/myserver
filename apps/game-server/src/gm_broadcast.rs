//! GM broadcast delivery and NATS subscriber.

use std::collections::{HashSet, VecDeque};

use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tracing::{error, info, warn};

use crate::core::context::PlayerRegistry;
use crate::core::room::{OutboundMessage, OutboundQueueLogContext};
use crate::pb::GameMessagePush;
use crate::protocol::{MessageType, encode_body};
use crate::server::current_unix_ms;

pub const GM_BROADCAST_SUBJECT: &str = "myserver.gm.broadcast";
pub const GM_BROADCAST_TITLE_MAX_LEN: usize = 128;
pub const GM_BROADCAST_CONTENT_MAX_LEN: usize = 4096;
pub const GM_SENDER_MAX_LEN: usize = 64;
const GM_BROADCAST_ID_MAX_LEN: usize = 128;
const GM_BROADCAST_DEDUPE_CAPACITY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmBroadcastCommand {
    pub title: String,
    pub content: String,
    pub sender: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GmBroadcastEvent {
    pub broadcast_id: String,
    pub command: GmBroadcastCommand,
}

#[derive(Debug)]
pub struct GmBroadcastDedupe {
    capacity: usize,
    order: VecDeque<String>,
    seen: HashSet<String>,
}

impl GmBroadcastDedupe {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            order: VecDeque::with_capacity(capacity),
            seen: HashSet::with_capacity(capacity),
        }
    }

    pub fn remember(&mut self, broadcast_id: &str) -> bool {
        if self.seen.contains(broadcast_id) {
            return false;
        }

        if self.capacity == 0 {
            return true;
        }

        let broadcast_id = broadcast_id.to_string();
        self.seen.insert(broadcast_id.clone());
        self.order.push_back(broadcast_id);
        while self.order.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.seen.remove(&evicted);
            }
        }
        true
    }
}

impl Default for GmBroadcastDedupe {
    fn default() -> Self {
        Self::new(GM_BROADCAST_DEDUPE_CAPACITY)
    }
}

/// Subscribe to GM broadcast events with automatic reconnection.
pub async fn subscribe_gm_broadcasts(nats_url: String, player_registry: PlayerRegistry) {
    loop {
        match run_subscriber(&nats_url, &player_registry).await {
            Ok(()) => {
                info!("gm broadcast subscriber completed normally");
                break;
            }
            Err(e) => {
                error!("gm broadcast subscriber error: {}, reconnecting in 5s", e);
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
    let mut subscriber = client.subscribe(GM_BROADCAST_SUBJECT).await?;
    let mut dedupe = GmBroadcastDedupe::default();
    info!(
        subject = GM_BROADCAST_SUBJECT,
        "subscribed to gm broadcast subject"
    );

    while let Some(msg) = subscriber.next().await {
        let payload = match std::str::from_utf8(msg.payload.as_ref()) {
            Ok(payload) => payload,
            Err(error) => {
                warn!(subject = %msg.subject, error = %error, "invalid gm broadcast payload");
                continue;
            }
        };

        let event = match parse_gm_broadcast_event(payload) {
            Ok(event) => event,
            Err(error) => {
                warn!(subject = %msg.subject, error, "failed to parse gm broadcast payload");
                continue;
            }
        };

        if !dedupe.remember(&event.broadcast_id) {
            info!(
                broadcast_id = %event.broadcast_id,
                "duplicate gm broadcast skipped"
            );
            continue;
        }

        let delivered =
            broadcast_gm_message_to_online_players(player_registry, &event.command).await;
        info!(
            broadcast_id = %event.broadcast_id,
            delivered,
            sender = %event.command.sender,
            title = %event.command.title,
            "gm broadcast delivered from nats"
        );
    }

    Ok(())
}

pub fn parse_gm_broadcast_event(payload: &str) -> Result<GmBroadcastEvent, &'static str> {
    #[derive(Deserialize)]
    struct GmBroadcastJson {
        #[serde(rename = "broadcast_id", alias = "broadcastId")]
        broadcast_id: Option<String>,
        title: Option<String>,
        content: Option<String>,
        sender: Option<String>,
    }

    let request: GmBroadcastJson =
        serde_json::from_str(payload).map_err(|_| "INVALID_GM_BROADCAST_BODY")?;

    let broadcast_id = normalize_required_string(
        request.broadcast_id,
        "INVALID_BROADCAST_ID",
        GM_BROADCAST_ID_MAX_LEN,
        "BROADCAST_ID_TOO_LONG",
    )?;
    let title = normalize_required_string(
        request.title,
        "INVALID_TITLE",
        GM_BROADCAST_TITLE_MAX_LEN,
        "TITLE_TOO_LONG",
    )?;
    let content = normalize_required_string(
        request.content,
        "INVALID_CONTENT",
        GM_BROADCAST_CONTENT_MAX_LEN,
        "CONTENT_TOO_LONG",
    )?;
    let sender = normalize_required_string(
        request.sender,
        "INVALID_SENDER",
        GM_SENDER_MAX_LEN,
        "SENDER_TOO_LONG",
    )?;

    Ok(GmBroadcastEvent {
        broadcast_id,
        command: GmBroadcastCommand {
            title,
            content,
            sender,
        },
    })
}

pub fn normalize_required_string(
    value: Option<String>,
    invalid_code: &'static str,
    max_chars: usize,
    too_long_code: &'static str,
) -> Result<String, &'static str> {
    let value = value.ok_or(invalid_code)?;
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid_code);
    }
    if value.chars().count() > max_chars {
        return Err(too_long_code);
    }
    Ok(value.to_string())
}

pub fn normalize_optional_string(
    value: Option<String>,
    default_value: &str,
    max_chars: usize,
    too_long_code: &'static str,
) -> Result<String, &'static str> {
    let value = value.unwrap_or_else(|| default_value.to_string());
    let value = value.trim();
    if value.chars().count() > max_chars {
        return Err(too_long_code);
    }
    if value.is_empty() {
        Ok(default_value.to_string())
    } else {
        Ok(value.to_string())
    }
}

pub async fn broadcast_gm_message_to_online_players(
    player_registry: &PlayerRegistry,
    request: &GmBroadcastCommand,
) -> usize {
    let handles = {
        let registry = player_registry.read().await;
        registry
            .online_connections()
            .into_iter()
            .map(|handle| {
                (
                    handle.account_player_id,
                    handle.character_id,
                    handle.outbound,
                )
            })
            .collect::<Vec<_>>()
    };

    let body = encode_body(&GameMessagePush {
        event: "gm_broadcast".to_string(),
        room_id: String::new(),
        player_id: String::new(),
        action: "broadcast".to_string(),
        payload_json: json!({
            "title": request.title,
            "content": request.content,
            "sender": request.sender,
            "timestamp": current_unix_ms()
        })
        .to_string(),
    });

    let mut delivered = 0;
    for (account_player_id, character_id, outbound) in handles {
        match outbound.try_send(
            OutboundMessage {
                message_type: MessageType::GameMessagePush,
                seq: 0,
                body: body.clone(),
            },
            OutboundQueueLogContext {
                player_id: Some(&account_player_id),
                operation: "gm_broadcast",
                ..OutboundQueueLogContext::default()
            },
        ) {
            Ok(()) => delivered += 1,
            Err(error) => {
                warn!(
                    account_player_id = %account_player_id,
                    player_id = %account_player_id,
                    character_id = %character_id,
                    error = %error,
                    "failed to queue gm broadcast"
                );
            }
        }
    }

    delivered
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::{Notify, RwLock, mpsc};

    use super::*;
    use crate::core::context::{OnlinePlayerRegistry, PlayerConnectionHandle};
    use crate::core::room::{ConnectionCloseState, OutboundChannel};

    fn player_registry_fixture(
        player_id: &str,
    ) -> (
        PlayerRegistry,
        Arc<Notify>,
        Arc<RwLock<String>>,
        mpsc::Receiver<OutboundMessage>,
    ) {
        let (tx, rx) = mpsc::channel(8);
        let notify = Arc::new(Notify::new());
        let kick_reason = Arc::new(RwLock::new("session_kicked".to_string()));
        let mut registry_state = OnlinePlayerRegistry::default();
        registry_state.insert_by_account(PlayerConnectionHandle {
            account_player_id: player_id.to_string(),
            character_id: "chr_0000000000001".to_string(),
            kick_notify: notify.clone(),
            session_id: 42,
            outbound: OutboundChannel::new(tx, ConnectionCloseState::new()),
            kick_reason: kick_reason.clone(),
        });
        let registry = Arc::new(RwLock::new(registry_state));

        (registry, notify, kick_reason, rx)
    }

    #[test]
    fn gm_broadcast_parse_trims_payload_and_accepts_camel_case_id() {
        let event = parse_gm_broadcast_event(
            r#"{"broadcastId":" id-1 ","title":" Notice ","content":" Hello ","sender":" GM "}"#,
        )
        .unwrap();

        assert_eq!(
            event,
            GmBroadcastEvent {
                broadcast_id: "id-1".to_string(),
                command: GmBroadcastCommand {
                    title: "Notice".to_string(),
                    content: "Hello".to_string(),
                    sender: "GM".to_string(),
                },
            }
        );
    }

    #[test]
    fn gm_broadcast_parse_rejects_missing_or_invalid_payload() {
        assert_eq!(
            parse_gm_broadcast_event(r#"{"title":"Notice","content":"Hello","sender":"GM"}"#),
            Err("INVALID_BROADCAST_ID")
        );
        assert_eq!(
            parse_gm_broadcast_event(
                r#"{"broadcast_id":"id-1","title":" ","content":"Hello","sender":"GM"}"#
            ),
            Err("INVALID_TITLE")
        );
        assert_eq!(
            parse_gm_broadcast_event(
                r#"{"broadcast_id":"id-1","title":"Notice","content":"Hello","sender":" "}"#
            ),
            Err("INVALID_SENDER")
        );
    }

    #[test]
    fn gm_broadcast_dedupe_rejects_duplicate_and_evicts_oldest() {
        let mut dedupe = GmBroadcastDedupe::new(2);

        assert!(dedupe.remember("a"));
        assert!(!dedupe.remember("a"));
        assert!(dedupe.remember("b"));
        assert!(dedupe.remember("c"));
        assert!(dedupe.remember("a"));
        assert!(!dedupe.remember("c"));
    }

    #[tokio::test]
    async fn gm_broadcast_delivery_queues_game_message_for_online_players() {
        let (registry, _notify, _kick_reason, mut rx) = player_registry_fixture("player-a");
        let request = GmBroadcastCommand {
            title: "Notice".to_string(),
            content: "Hello".to_string(),
            sender: "System".to_string(),
        };

        let delivered = broadcast_gm_message_to_online_players(&registry, &request).await;

        assert_eq!(delivered, 1);
        let message = rx.try_recv().expect("gm broadcast queued");
        assert_eq!(message.message_type, MessageType::GameMessagePush);
        let push = prost::Message::decode(message.body.as_slice()).unwrap();
        let push: GameMessagePush = push;
        assert_eq!(push.event, "gm_broadcast");
        assert_eq!(push.action, "broadcast");
        assert!(push.payload_json.contains("\"title\":\"Notice\""));
    }
}
