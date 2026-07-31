//! Instance-routed best-effort chat push transport.
//!
//! Chat history is the durable source of truth. This module only forwards an
//! already-persisted `ChatPush` to the instance currently named by Redis and
//! deliberately never turns a NATS or session failure into a chat send error.

use std::time::Duration;

use base64::Engine;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tracing::{debug, warn};

use crate::chat_server::MessageType;
use crate::chat_service::{ChatSessionMap, RoutedSessionPushOutcome, push_to_routed_session};
use crate::metrics::METRICS;
use crate::online_route::{self, OnlineRoute};
use crate::protocol::OutboundMessage;

pub const DEFAULT_PUBLISH_QUEUE_CAPACITY: usize = 1_024;
pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 64 * 1024;
pub const DEFAULT_RECONNECT_BASE_MS: u64 = 1_000;
pub const DEFAULT_RECONNECT_MAX_MS: u64 = 30_000;

const PUSH_VERSION: u8 = 1;
const MAX_PLAYER_ID_BYTES: usize = 128;
const MAX_CONNECTION_TOKEN_BYTES: usize = 256;

#[derive(Clone, Debug)]
pub struct RuntimeConfig {
    pub nats_url: String,
    pub redis_url: String,
    pub redis_key_prefix: String,
    pub instance_id: String,
    pub max_payload_bytes: usize,
    pub max_message_body_bytes: usize,
    pub reconnect_base_delay: Duration,
    pub reconnect_max_delay: Duration,
}

#[derive(Clone)]
pub struct ChatPushRouter {
    redis_url: String,
    redis_key_prefix: String,
    instance_id: String,
    max_payload_bytes: usize,
    max_message_body_bytes: usize,
    publish_queue_capacity: usize,
    outbound: mpsc::Sender<RoutedChatPush>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteQueueOutcome {
    Queued,
    Rejected,
    Full,
    Closed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoutedChatPush {
    version: u8,
    target_player_id: String,
    connection_token: String,
    message_type: u16,
    body: String,
}

impl RoutedChatPush {
    fn from_outbound(
        target_player_id: &str,
        connection_token: &str,
        message: OutboundMessage,
    ) -> Self {
        Self {
            version: PUSH_VERSION,
            target_player_id: target_player_id.to_string(),
            connection_token: connection_token.to_string(),
            message_type: message.message_type,
            body: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(message.body),
        }
    }

    fn into_outbound(self, max_message_body_bytes: usize) -> Result<OutboundMessage, ParseError> {
        if self.version != PUSH_VERSION {
            return Err(ParseError::UnsupportedVersion);
        }
        validate_non_empty(&self.target_player_id, MAX_PLAYER_ID_BYTES)?;
        validate_non_empty(&self.connection_token, MAX_CONNECTION_TOKEN_BYTES)?;
        if self.message_type != MessageType::ChatPush as u16 {
            return Err(ParseError::UnexpectedMessageType);
        }
        let body = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(self.body)
            .map_err(|_| ParseError::InvalidBody)?;
        if body.len() > max_message_body_bytes {
            return Err(ParseError::BodyTooLarge);
        }
        Ok(OutboundMessage {
            message_type: MessageType::ChatPush as u16,
            seq: 0,
            body,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParseError {
    PayloadTooLarge,
    InvalidJson,
    UnsupportedVersion,
    InvalidField,
    InvalidBody,
    BodyTooLarge,
    UnexpectedMessageType,
}

impl ParseError {
    fn category(self) -> &'static str {
        match self {
            Self::PayloadTooLarge => "payload_too_large",
            Self::InvalidJson => "invalid_json",
            Self::UnsupportedVersion => "unsupported_version",
            Self::InvalidField => "invalid_field",
            Self::InvalidBody => "invalid_body",
            Self::BodyTooLarge => "body_too_large",
            Self::UnexpectedMessageType => "unexpected_message_type",
        }
    }
}

impl ChatPushRouter {
    pub fn new(
        config: &RuntimeConfig,
        publish_queue_capacity: usize,
    ) -> (Self, mpsc::Receiver<RoutedChatPush>) {
        let publish_queue_capacity = publish_queue_capacity.max(1);
        let (outbound, receiver) = mpsc::channel(publish_queue_capacity);
        METRICS.set_extra("chat_push_publish_queue_depth", "0");
        METRICS.set_extra(
            "chat_push_publish_queue_capacity",
            publish_queue_capacity.to_string(),
        );
        (
            Self {
                redis_url: config.redis_url.clone(),
                redis_key_prefix: config.redis_key_prefix.clone(),
                instance_id: config.instance_id.clone(),
                max_payload_bytes: config.max_payload_bytes,
                max_message_body_bytes: config.max_message_body_bytes,
                publish_queue_capacity,
                outbound,
            },
            receiver,
        )
    }

    /// Resolves the target immediately before local enqueue or remote publish.
    /// A Redis lookup failure is an online-push failure only; callers have
    /// already committed the chat message to the durable store.
    pub async fn deliver(
        &self,
        sessions: &ChatSessionMap,
        target_player_id: &str,
        message: OutboundMessage,
    ) {
        let route = match online_route::get_online_route(
            &self.redis_url,
            &self.redis_key_prefix,
            target_player_id,
        )
        .await
        {
            Ok(Some(route)) => route,
            Ok(None) => {
                METRICS.record_chat_push_route_unavailable();
                debug!(error_category = "route_missing", "skipped online chat push");
                return;
            }
            Err(_) => {
                METRICS.record_chat_push_route_lookup_failed();
                warn!(
                    error_category = "route_lookup_failed",
                    "skipped online chat push"
                );
                return;
            }
        };

        if route.instance_id == self.instance_id {
            self.deliver_local(sessions, &route, target_player_id, message)
                .await;
            return;
        }

        let push =
            RoutedChatPush::from_outbound(target_player_id, &route.connection_token, message);
        match self.try_queue_remote(push) {
            RemoteQueueOutcome::Queued => METRICS.record_chat_push_remote_queued(),
            RemoteQueueOutcome::Rejected => {
                METRICS.record_chat_push_payload_rejected();
                warn!(
                    error_category = "payload_rejected",
                    "skipped remote chat push"
                );
            }
            RemoteQueueOutcome::Full => {
                METRICS.record_chat_push_remote_queue_failed();
                warn!(
                    error_category = "publish_queue_full",
                    "skipped remote chat push"
                );
            }
            RemoteQueueOutcome::Closed => {
                METRICS.record_chat_push_remote_queue_failed();
                warn!(
                    error_category = "publish_queue_closed",
                    "skipped remote chat push"
                );
            }
        }
    }

    async fn deliver_local(
        &self,
        sessions: &ChatSessionMap,
        route: &OnlineRoute,
        target_player_id: &str,
        message: OutboundMessage,
    ) {
        record_local_delivery(
            push_to_routed_session(
                sessions,
                &self.instance_id,
                Some(route),
                &self.instance_id,
                &route.connection_token,
                target_player_id,
                message,
            )
            .await,
        );
    }

    fn try_queue_remote(&self, push: RoutedChatPush) -> RemoteQueueOutcome {
        let outcome = match serde_json::to_vec(&push) {
            Ok(payload)
                if payload.len() <= self.max_payload_bytes
                    && push.body.len() <= self.max_message_body_bytes.saturating_mul(2) =>
            {
                match self.outbound.try_send(push) {
                    Ok(()) => RemoteQueueOutcome::Queued,
                    Err(mpsc::error::TrySendError::Full(_)) => RemoteQueueOutcome::Full,
                    Err(mpsc::error::TrySendError::Closed(_)) => RemoteQueueOutcome::Closed,
                }
            }
            _ => RemoteQueueOutcome::Rejected,
        };
        self.report_publish_queue_depth();
        outcome
    }

    fn report_publish_queue_depth(&self) {
        report_publish_queue_depth(self.publish_queue_capacity, self.outbound.capacity());
    }
}

fn report_publish_queue_depth(publish_queue_capacity: usize, remaining: usize) {
    METRICS.set_extra(
        "chat_push_publish_queue_depth",
        publish_queue_capacity.saturating_sub(remaining).to_string(),
    );
    METRICS.set_extra(
        "chat_push_publish_queue_capacity",
        publish_queue_capacity.to_string(),
    );
}

pub async fn run(
    config: RuntimeConfig,
    sessions: ChatSessionMap,
    outbound: mpsc::Receiver<RoutedChatPush>,
    publish_queue_capacity: usize,
    shutdown: watch::Receiver<bool>,
) {
    let publisher_config = config.clone();
    let publisher_shutdown = shutdown.clone();
    let publisher = run_publisher(
        publisher_config,
        outbound,
        publish_queue_capacity.max(1),
        publisher_shutdown,
    );
    let subscriber = run_subscriber(config, sessions, shutdown);
    tokio::join!(publisher, subscriber);
}

async fn run_publisher(
    config: RuntimeConfig,
    mut outbound: mpsc::Receiver<RoutedChatPush>,
    publish_queue_capacity: usize,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut reconnect_delay = config.reconnect_base_delay;
    let mut client = None;
    while let Some(push) = next_outbound(&mut outbound, &mut shutdown).await {
        report_publish_queue_depth(publish_queue_capacity, outbound.capacity());
        // Re-read Redis immediately before publishing so a queued event is not
        // sent to an instance that was replaced during a NATS reconnect.
        let target_route = match online_route::get_online_route(
            &config.redis_url,
            &config.redis_key_prefix,
            &push.target_player_id,
        )
        .await
        {
            Ok(Some(route)) => route,
            Ok(None) => {
                METRICS.record_chat_push_route_unavailable();
                continue;
            }
            Err(_) => {
                METRICS.record_chat_push_route_lookup_failed();
                continue;
            }
        };
        if target_route.connection_token != push.connection_token
            || target_route.instance_id == config.instance_id
        {
            METRICS.record_chat_push_stale_skipped();
            continue;
        }

        let subject = build_instance_subject(&target_route.instance_id);
        let payload = match serde_json::to_vec(&push) {
            Ok(payload) if payload.len() <= config.max_payload_bytes => payload,
            Ok(_) => {
                METRICS.record_chat_push_payload_rejected();
                continue;
            }
            Err(_) => {
                METRICS.record_chat_push_payload_rejected();
                continue;
            }
        };

        while client.is_none() {
            client = tokio::select! {
                result = nats_client::connect(&config.nats_url) => match result {
                    Ok(client) => {
                        reconnect_delay = config.reconnect_base_delay;
                        Some(client)
                    }
                    Err(_) => {
                        METRICS.record_chat_push_publish_failed();
                        if wait_for_shutdown(&mut shutdown, reconnect_delay).await {
                            return;
                        }
                        reconnect_delay = reconnect_delay.saturating_mul(2).min(config.reconnect_max_delay);
                        None
                    }
                },
                _ = wait_for_shutdown_signal(&mut shutdown) => return,
            };
        }

        // Core NATS is best effort. A publish/flush failure is deliberately not
        // retried because the broker may already have accepted the message; the
        // persisted chat history is the recovery path and avoids duplicate push.
        let connected = client.as_ref().expect("connected client must be present");
        match connected.publish(subject, payload.into()).await {
            Ok(()) => match connected.flush().await {
                Ok(()) => METRICS.record_chat_push_published(),
                Err(_) => {
                    METRICS.record_chat_push_publish_failed();
                    client = None;
                }
            },
            Err(_) => {
                METRICS.record_chat_push_publish_failed();
                client = None;
            }
        }
    }
}

async fn next_outbound(
    outbound: &mut mpsc::Receiver<RoutedChatPush>,
    shutdown: &mut watch::Receiver<bool>,
) -> Option<RoutedChatPush> {
    tokio::select! {
        message = outbound.recv() => message,
        _ = wait_for_shutdown_signal(shutdown) => None,
    }
}

async fn run_subscriber(
    config: RuntimeConfig,
    sessions: ChatSessionMap,
    mut shutdown: watch::Receiver<bool>,
) {
    let subject = build_instance_subject(&config.instance_id);
    let mut reconnect_delay = config.reconnect_base_delay;

    loop {
        let client = tokio::select! {
            result = nats_client::connect(&config.nats_url) => match result {
                Ok(client) => client,
                Err(_) => {
                    if wait_for_shutdown(&mut shutdown, reconnect_delay).await {
                        return;
                    }
                    reconnect_delay = reconnect_delay.saturating_mul(2).min(config.reconnect_max_delay);
                    continue;
                }
            },
            _ = wait_for_shutdown_signal(&mut shutdown) => return,
        };
        let mut subscriber = tokio::select! {
            result = client.subscribe(subject.clone()) => match result {
                Ok(subscriber) => subscriber,
                Err(_) => {
                    if wait_for_shutdown(&mut shutdown, reconnect_delay).await {
                        return;
                    }
                    reconnect_delay = reconnect_delay.saturating_mul(2).min(config.reconnect_max_delay);
                    continue;
                }
            },
            _ = wait_for_shutdown_signal(&mut shutdown) => return,
        };
        reconnect_delay = config.reconnect_base_delay;

        loop {
            let message = tokio::select! {
                message = subscriber.next() => message,
                _ = wait_for_shutdown_signal(&mut shutdown) => return,
            };
            let Some(message) = message else {
                break;
            };
            handle_inbound(&config, &sessions, message.payload.as_ref()).await;
        }
    }
}

async fn handle_inbound(config: &RuntimeConfig, sessions: &ChatSessionMap, payload: &[u8]) {
    METRICS.record_chat_push_received();
    let push = match parse_push(
        payload,
        config.max_payload_bytes,
        config.max_message_body_bytes,
    ) {
        Ok(push) => push,
        Err(error) => {
            METRICS.record_chat_push_payload_rejected();
            warn!(
                error_category = error.category(),
                payload_bytes = payload.len(),
                "rejected routed chat push"
            );
            return;
        }
    };

    let route = match online_route::get_online_route(
        &config.redis_url,
        &config.redis_key_prefix,
        &push.target_player_id,
    )
    .await
    {
        Ok(route) => route,
        Err(_) => {
            METRICS.record_chat_push_route_lookup_failed();
            warn!(
                error_category = "route_lookup_failed",
                "skipped routed chat push"
            );
            return;
        }
    };
    let target_player_id = push.target_player_id.clone();
    let connection_token = push.connection_token.clone();
    let message = match push.into_outbound(config.max_message_body_bytes) {
        Ok(message) => message,
        Err(error) => {
            METRICS.record_chat_push_payload_rejected();
            warn!(
                error_category = error.category(),
                "rejected routed chat push"
            );
            return;
        }
    };
    record_local_delivery(
        push_to_routed_session(
            sessions,
            &config.instance_id,
            route.as_ref(),
            &config.instance_id,
            &connection_token,
            &target_player_id,
            message,
        )
        .await,
    );
}

fn record_local_delivery(outcome: RoutedSessionPushOutcome) {
    match outcome {
        RoutedSessionPushOutcome::Pushed => METRICS.record_chat_push_delivered(),
        RoutedSessionPushOutcome::QueueFull | RoutedSessionPushOutcome::QueueClosed => {
            METRICS.record_chat_push_session_queue_failed();
            warn!(
                error_category = outcome.category(),
                "chat push session queue unavailable"
            );
        }
        _ => {
            METRICS.record_chat_push_stale_skipped();
            debug!(
                error_category = outcome.category(),
                "skipped routed chat push"
            );
        }
    }
}

fn parse_push(
    payload: &[u8],
    max_payload_bytes: usize,
    max_message_body_bytes: usize,
) -> Result<RoutedChatPush, ParseError> {
    if payload.len() > max_payload_bytes {
        return Err(ParseError::PayloadTooLarge);
    }
    let push: RoutedChatPush =
        serde_json::from_slice(payload).map_err(|_| ParseError::InvalidJson)?;
    // Validate all fields before a Redis lookup.
    let _ = push.clone().into_outbound(max_message_body_bytes)?;
    Ok(push)
}

fn validate_non_empty(value: &str, max_bytes: usize) -> Result<(), ParseError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(ParseError::InvalidField);
    }
    Ok(())
}

fn build_instance_subject(instance_id: &str) -> String {
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(instance_id);
    format!("myserver.chat.push.instance.{encoded}")
}

fn shutdown_requested(shutdown: &watch::Receiver<bool>) -> bool {
    *shutdown.borrow()
}

async fn wait_for_shutdown_signal(shutdown: &mut watch::Receiver<bool>) {
    if !shutdown_requested(shutdown) {
        let _ = shutdown.changed().await;
    }
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => shutdown_requested(shutdown),
        _ = wait_for_shutdown_signal(shutdown) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_config(max_payload_bytes: usize) -> RuntimeConfig {
        RuntimeConfig {
            nats_url: "nats://127.0.0.1:4222".to_string(),
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: String::new(),
            instance_id: "chat-source".to_string(),
            max_payload_bytes,
            max_message_body_bytes: 128,
            reconnect_base_delay: Duration::from_millis(1),
            reconnect_max_delay: Duration::from_millis(2),
        }
    }

    fn push() -> RoutedChatPush {
        RoutedChatPush::from_outbound(
            "player_001",
            "connection_001",
            OutboundMessage {
                message_type: MessageType::ChatPush as u16,
                seq: 0,
                body: vec![1, 2, 3],
            },
        )
    }

    #[test]
    fn routed_push_round_trip_has_no_player_in_subject() {
        let encoded = serde_json::to_vec(&push()).unwrap();
        let parsed = parse_push(&encoded, DEFAULT_MAX_PAYLOAD_BYTES, 128).unwrap();
        assert_eq!(parsed, push());
        assert_eq!(
            build_instance_subject("chat-a"),
            "myserver.chat.push.instance.Y2hhdC1h"
        );
        assert!(build_instance_subject("chat-a").contains("chat"));
        assert!(!build_instance_subject("chat-a").contains("player_001"));
    }

    #[test]
    fn routed_push_rejects_invalid_contract_before_delivery() {
        let mut invalid = push();
        invalid.message_type = MessageType::MailNotifyPush as u16;
        let payload = serde_json::to_vec(&invalid).unwrap();
        assert_eq!(
            parse_push(&payload, DEFAULT_MAX_PAYLOAD_BYTES, 128),
            Err(ParseError::UnexpectedMessageType)
        );

        let oversized = vec![b'x'; 9];
        assert_eq!(
            parse_push(&oversized, 8, 128),
            Err(ParseError::PayloadTooLarge)
        );
    }

    #[tokio::test]
    async fn remote_publish_queue_preserves_owner_token_and_is_bounded() {
        let (router, mut receiver) = ChatPushRouter::new(&runtime_config(1024), 1);
        let expected = push();

        assert_eq!(
            router.try_queue_remote(expected.clone()),
            RemoteQueueOutcome::Queued
        );
        let queued = receiver.recv().await.unwrap();
        assert_eq!(queued.target_player_id, "player_001");
        assert_eq!(queued.connection_token, "connection_001");
        assert_eq!(queued, expected);

        assert_eq!(router.try_queue_remote(push()), RemoteQueueOutcome::Queued);
        assert_eq!(router.try_queue_remote(push()), RemoteQueueOutcome::Full);
        drop(receiver);
        assert_eq!(router.try_queue_remote(push()), RemoteQueueOutcome::Closed);
    }

    #[test]
    fn remote_publish_queue_rejects_payload_above_its_configured_limit() {
        let (router, _receiver) = ChatPushRouter::new(&runtime_config(1), 1);

        assert_eq!(
            router.try_queue_remote(push()),
            RemoteQueueOutcome::Rejected
        );
    }
}
