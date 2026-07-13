//! Mail notification subscriber.
//!
//! Core NATS only carries best-effort online notifications. Both the legacy
//! player subject and the instance-routed subject feed the same bounded
//! deduplicator so an event is never pushed twice during a rolling upgrade.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use base64::Engine;
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::chat_server::MessageType;
use crate::chat_service::ChatSessionMap;
use crate::metrics::METRICS;
use crate::proto::chat::MailNotifyPush;
use crate::protocol::{OutboundMessage, encode_body};

pub const DEFAULT_MAX_PAYLOAD_BYTES: usize = 4096;
pub const DEFAULT_DEDUP_CAPACITY: usize = 10_000;
pub const DEFAULT_DEDUP_TTL_SECS: u64 = 300;
pub const DEFAULT_RECONNECT_BASE_MS: u64 = 1_000;
pub const DEFAULT_RECONNECT_MAX_MS: u64 = 30_000;

const EVENT_TYPE: &str = "mail.created";
const EVENT_VERSION: i64 = 1;

#[derive(Clone, Debug)]
pub struct SubscriberConfig {
    pub max_payload_bytes: usize,
    pub dedup_capacity: usize,
    pub dedup_ttl: Duration,
    pub reconnect_base_delay: Duration,
    pub reconnect_max_delay: Duration,
}

impl Default for SubscriberConfig {
    fn default() -> Self {
        Self {
            max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
            dedup_capacity: DEFAULT_DEDUP_CAPACITY,
            dedup_ttl: Duration::from_secs(DEFAULT_DEDUP_TTL_SECS),
            reconnect_base_delay: Duration::from_millis(DEFAULT_RECONNECT_BASE_MS),
            reconnect_max_delay: Duration::from_millis(DEFAULT_RECONNECT_MAX_MS),
        }
    }
}

#[derive(Debug, Deserialize)]
struct LegacyMailNotification {
    player_id: String,
    mail_id: String,
    title: String,
    from: String,
    #[serde(default)]
    from_name: String,
    #[serde(rename = "type")]
    mail_type: String,
    created_at: i64,
}

#[derive(Debug, Deserialize)]
struct MailNotificationEnvelope {
    event_id: String,
    occurred_at: i64,
    player_id: String,
    mail: EnvelopeMail,
    trace_id: String,
}

#[derive(Debug, Deserialize)]
struct EnvelopeMail {
    mail_id: String,
    title: String,
    from_player_id: String,
    from_name: String,
    mail_type: String,
    created_at: i64,
}

#[derive(Debug, PartialEq, Eq)]
struct MailNotification {
    event_id: Option<String>,
    player_id: String,
    mail_id: String,
    title: String,
    from_player_id: String,
    mail_type: String,
    created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParseErrorKind {
    PayloadTooLarge,
    InvalidJson,
    InvalidEnvelope,
    InvalidLegacy,
    UnsupportedEventType,
    UnsupportedVersion,
}

impl ParseErrorKind {
    fn code(self) -> &'static str {
        match self {
            Self::PayloadTooLarge => "payload_too_large",
            Self::InvalidJson => "invalid_json",
            Self::InvalidEnvelope => "invalid_envelope",
            Self::InvalidLegacy => "invalid_legacy_payload",
            Self::UnsupportedEventType => "unsupported_event_type",
            Self::UnsupportedVersion => "unsupported_version",
        }
    }
}

#[derive(Debug)]
struct ParseError {
    kind: ParseErrorKind,
}

impl ParseError {
    fn new(kind: ParseErrorKind) -> Self {
        Self { kind }
    }
}

#[derive(Debug)]
struct EventDeduplicator {
    capacity: usize,
    ttl: Duration,
    entries: HashMap<String, Instant>,
    insertion_order: VecDeque<String>,
}

impl EventDeduplicator {
    fn new(capacity: usize, ttl: Duration) -> Self {
        assert!(
            capacity > 0,
            "mail notification dedup capacity must be positive"
        );
        assert!(
            !ttl.is_zero(),
            "mail notification dedup ttl must be positive"
        );
        Self {
            capacity,
            ttl,
            entries: HashMap::with_capacity(capacity),
            insertion_order: VecDeque::with_capacity(capacity),
        }
    }

    /// Returns true if the event is already present. Entries expire from their
    /// first observation and duplicates do not extend the TTL.
    fn seen_or_insert(&mut self, event_id: &str, now: Instant) -> bool {
        self.evict_expired(now);
        if self.entries.contains_key(event_id) {
            return true;
        }

        while self.entries.len() >= self.capacity {
            self.evict_oldest();
        }

        self.entries.insert(event_id.to_string(), now + self.ttl);
        self.insertion_order.push_back(event_id.to_string());
        false
    }

    fn evict_expired(&mut self, now: Instant) {
        while let Some(event_id) = self.insertion_order.front() {
            let Some(expires_at) = self.entries.get(event_id) else {
                self.insertion_order.pop_front();
                continue;
            };
            if *expires_at > now {
                break;
            }
            self.evict_oldest();
        }
    }

    fn evict_oldest(&mut self) {
        if let Some(event_id) = self.insertion_order.pop_front() {
            self.entries.remove(&event_id);
        }
    }
}

enum RunOutcome {
    Shutdown,
    Disconnected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PushOutcome {
    Pushed,
    Offline,
    QueueFull,
    QueueClosed,
}

/// Subscribe to both mail notification routes until shutdown is requested.
pub async fn subscribe_mail_notifications(
    nats_url: String,
    instance_id: String,
    sessions: ChatSessionMap,
    config: SubscriberConfig,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut deduplicator = EventDeduplicator::new(config.dedup_capacity, config.dedup_ttl);
    let mut reconnect_delay = config.reconnect_base_delay;

    loop {
        if shutdown_requested(&shutdown) {
            return Ok(());
        }

        match run_subscriber(
            &nats_url,
            &instance_id,
            &sessions,
            &config,
            &mut deduplicator,
            &mut shutdown,
        )
        .await
        {
            Ok(RunOutcome::Shutdown) => return Ok(()),
            Ok(RunOutcome::Disconnected) => {
                warn!(
                    reconnect_delay_ms = reconnect_delay.as_millis() as u64,
                    "mail notification subscriptions closed; reconnecting"
                );
            }
            Err(error) => {
                error!(
                    error = %error,
                    reconnect_delay_ms = reconnect_delay.as_millis() as u64,
                    "mail subscriber failed; reconnecting"
                );
            }
        }

        if wait_for_shutdown(&mut shutdown, reconnect_delay).await {
            return Ok(());
        }
        reconnect_delay = reconnect_delay
            .saturating_mul(2)
            .min(config.reconnect_max_delay);
    }
}

async fn run_subscriber(
    nats_url: &str,
    instance_id: &str,
    sessions: &ChatSessionMap,
    config: &SubscriberConfig,
    deduplicator: &mut EventDeduplicator,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<RunOutcome, Box<dyn std::error::Error + Send + Sync>> {
    let client = tokio::select! {
        result = async_nats::connect(nats_url) => result?,
        _ = wait_for_shutdown_signal(shutdown) => return Ok(RunOutcome::Shutdown),
    };
    let legacy_subject = "myserver.mail.notify.*";
    let instance_subject = build_instance_subject(instance_id);
    let mut legacy_subscriber = tokio::select! {
        result = client.subscribe(legacy_subject) => result?,
        _ = wait_for_shutdown_signal(shutdown) => {
            drain_client(&client).await;
            return Ok(RunOutcome::Shutdown);
        }
    };
    let mut instance_subscriber = tokio::select! {
        result = client.subscribe(instance_subject.clone()) => result?,
        _ = wait_for_shutdown_signal(shutdown) => {
            drain_client(&client).await;
            return Ok(RunOutcome::Shutdown);
        }
    };
    info!(
        legacy_subject = %legacy_subject,
        instance_subject = %instance_subject,
        "subscribed to mail notification subjects"
    );

    loop {
        let message = tokio::select! {
            value = legacy_subscriber.next() => value.map(|message| (message, "legacy")),
            value = instance_subscriber.next() => value.map(|message| (message, "instance")),
            _ = wait_for_shutdown_signal(shutdown) => {
                drain_client(&client).await;
                info!("mail notification subscriber stopped");
                return Ok(RunOutcome::Shutdown);
            }
        };

        let Some((message, route)) = message else {
            return Ok(RunOutcome::Disconnected);
        };
        handle_notification(
            sessions,
            message.payload.as_ref(),
            route,
            config.max_payload_bytes,
            deduplicator,
        )
        .await;
    }
}

async fn drain_client(client: &async_nats::Client) {
    match tokio::time::timeout(Duration::from_secs(2), client.drain()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            warn!(error = %error, "failed to drain mail subscriber NATS client");
        }
        Err(error) => {
            warn!(error = %error, "timed out draining mail subscriber NATS client");
        }
    }
}

fn shutdown_requested(shutdown: &watch::Receiver<bool>) -> bool {
    *shutdown.borrow()
}

async fn wait_for_shutdown_signal(shutdown: &mut watch::Receiver<bool>) {
    if shutdown_requested(shutdown) {
        return;
    }
    let _ = shutdown.changed().await;
}

async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => shutdown_requested(shutdown),
        _ = wait_for_shutdown_signal(shutdown) => true,
    }
}

fn build_instance_subject(instance_id: &str) -> String {
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(instance_id);
    format!("myserver.mail.notify.instance.{}", encoded)
}

async fn handle_notification(
    sessions: &ChatSessionMap,
    payload: &[u8],
    route: &'static str,
    max_payload_bytes: usize,
    deduplicator: &mut EventDeduplicator,
) {
    METRICS.record_mail_notification_received();
    let notification = match parse_notification(payload, max_payload_bytes) {
        Ok(notification) => notification,
        Err(error) => {
            if error.kind == ParseErrorKind::UnsupportedVersion {
                METRICS.record_mail_notification_version_rejected();
            } else {
                METRICS.record_mail_notification_parse_failed();
            }
            warn!(
                route,
                error_code = error.kind.code(),
                payload_bytes = payload.len(),
                "rejected mail notification"
            );
            return;
        }
    };

    if let Some(event_id) = notification.event_id.as_deref()
        && deduplicator.seen_or_insert(event_id, Instant::now())
    {
        METRICS.record_mail_notification_deduplicated();
        debug!(route, "skipped duplicate mail notification");
        return;
    }

    match push_mail_to_player(sessions, &notification.player_id, &notification).await {
        PushOutcome::Pushed => {
            METRICS.record_mail_notification_pushed();
            debug!(route, "queued mail notification for online player");
        }
        PushOutcome::Offline => {
            METRICS.record_mail_notification_offline_skipped();
            debug!(route, "mail notification target is offline");
        }
        PushOutcome::QueueFull => {
            METRICS.record_mail_notification_queue_failed();
            warn!(
                route,
                reason = "full",
                "mail notification session queue unavailable"
            );
        }
        PushOutcome::QueueClosed => {
            METRICS.record_mail_notification_queue_failed();
            debug!(
                route,
                reason = "closed",
                "mail notification session queue unavailable"
            );
        }
    }
}

fn parse_notification(
    payload: &[u8],
    max_payload_bytes: usize,
) -> Result<MailNotification, ParseError> {
    if payload.len() > max_payload_bytes {
        return Err(ParseError::new(ParseErrorKind::PayloadTooLarge));
    }

    let value: serde_json::Value = serde_json::from_slice(payload)
        .map_err(|_| ParseError::new(ParseErrorKind::InvalidJson))?;
    if !value.is_object() {
        return Err(ParseError::new(ParseErrorKind::InvalidJson));
    }

    let is_envelope = value.get("event_type").is_some()
        || value.get("version").is_some()
        || value.get("mail").is_some();
    if is_envelope {
        parse_envelope(value)
    } else {
        parse_legacy(value)
    }
}

fn parse_envelope(value: serde_json::Value) -> Result<MailNotification, ParseError> {
    let event_type = value
        .get("event_type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ParseError::new(ParseErrorKind::InvalidEnvelope))?;
    if event_type != EVENT_TYPE {
        return Err(ParseError::new(ParseErrorKind::UnsupportedEventType));
    }
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| ParseError::new(ParseErrorKind::InvalidEnvelope))?;
    if version != EVENT_VERSION {
        return Err(ParseError::new(ParseErrorKind::UnsupportedVersion));
    }

    let envelope: MailNotificationEnvelope = serde_json::from_value(value)
        .map_err(|_| ParseError::new(ParseErrorKind::InvalidEnvelope))?;
    if envelope.occurred_at <= 0 || envelope.mail.created_at <= 0 {
        return Err(ParseError::new(ParseErrorKind::InvalidEnvelope));
    }
    validate_non_empty(&envelope.event_id, 128, ParseErrorKind::InvalidEnvelope)?;
    validate_non_empty(&envelope.player_id, 128, ParseErrorKind::InvalidEnvelope)?;
    validate_non_empty(&envelope.mail.mail_id, 64, ParseErrorKind::InvalidEnvelope)?;
    validate_string(&envelope.mail.title, 256, ParseErrorKind::InvalidEnvelope)?;
    validate_non_empty(
        &envelope.mail.from_player_id,
        128,
        ParseErrorKind::InvalidEnvelope,
    )?;
    validate_string(
        &envelope.mail.from_name,
        128,
        ParseErrorKind::InvalidEnvelope,
    )?;
    validate_non_empty(
        &envelope.mail.mail_type,
        32,
        ParseErrorKind::InvalidEnvelope,
    )?;
    if envelope.trace_id.len() != 32
        || !envelope
            .trace_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ParseError::new(ParseErrorKind::InvalidEnvelope));
    }

    Ok(MailNotification {
        event_id: Some(envelope.event_id),
        player_id: envelope.player_id,
        mail_id: envelope.mail.mail_id,
        title: envelope.mail.title,
        from_player_id: envelope.mail.from_player_id,
        mail_type: envelope.mail.mail_type,
        created_at: envelope.mail.created_at,
    })
}

fn parse_legacy(value: serde_json::Value) -> Result<MailNotification, ParseError> {
    let notification: LegacyMailNotification = serde_json::from_value(value)
        .map_err(|_| ParseError::new(ParseErrorKind::InvalidLegacy))?;
    if notification.created_at <= 0 {
        return Err(ParseError::new(ParseErrorKind::InvalidLegacy));
    }
    validate_non_empty(&notification.player_id, 128, ParseErrorKind::InvalidLegacy)?;
    validate_non_empty(&notification.mail_id, 64, ParseErrorKind::InvalidLegacy)?;
    validate_string(&notification.title, 256, ParseErrorKind::InvalidLegacy)?;
    validate_non_empty(&notification.from, 128, ParseErrorKind::InvalidLegacy)?;
    validate_string(&notification.from_name, 128, ParseErrorKind::InvalidLegacy)?;
    validate_non_empty(&notification.mail_type, 32, ParseErrorKind::InvalidLegacy)?;

    Ok(MailNotification {
        event_id: None,
        player_id: notification.player_id,
        mail_id: notification.mail_id,
        title: notification.title,
        from_player_id: notification.from,
        mail_type: notification.mail_type,
        created_at: notification.created_at,
    })
}

fn validate_non_empty(
    value: &str,
    max_bytes: usize,
    error_kind: ParseErrorKind,
) -> Result<(), ParseError> {
    if value.is_empty() || value.len() > max_bytes {
        return Err(ParseError::new(error_kind));
    }
    Ok(())
}

fn validate_string(
    value: &str,
    max_bytes: usize,
    error_kind: ParseErrorKind,
) -> Result<(), ParseError> {
    if value.len() > max_bytes {
        return Err(ParseError::new(error_kind));
    }
    Ok(())
}

async fn push_mail_to_player(
    sessions: &ChatSessionMap,
    player_id: &str,
    notification: &MailNotification,
) -> PushOutcome {
    let session_guard = sessions.read().await;
    let Some(sender) = session_guard.get(player_id) else {
        return PushOutcome::Offline;
    };

    let push = MailNotifyPush {
        mail_id: notification.mail_id.clone(),
        title: notification.title.clone(),
        from_player_id: notification.from_player_id.clone(),
        mail_type: notification.mail_type.clone(),
        created_at: notification.created_at,
    };
    let message = OutboundMessage {
        message_type: MessageType::MailNotifyPush as u16,
        seq: 0,
        body: encode_body(&push),
    };

    match sender.try_send(message) {
        Ok(()) => PushOutcome::Pushed,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => PushOutcome::QueueFull,
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => PushOutcome::QueueClosed,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    fn envelope() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "event_id": "mail.notify:mail_001",
            "event_type": "mail.created",
            "version": 1,
            "occurred_at": 1_783_931_896_000_i64,
            "player_id": "player_001",
            "mail": {
                "mail_id": "mail_001",
                "title": "Reward",
                "from_player_id": "system",
                "from_name": "System",
                "mail_type": "system",
                "created_at": 1_783_931_896_000_i64
            },
            "trace_id": "0123456789abcdef0123456789abcdef"
        }))
        .unwrap()
    }

    fn legacy_payload() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "player_id": "player_001",
            "mail_id": "mail_001",
            "title": "Reward",
            "from": "system",
            "from_name": "System",
            "type": "system",
            "created_at": 1_700_000_000_i64
        }))
        .unwrap()
    }

    #[test]
    fn builds_instance_subject_with_url_safe_base64_without_padding() {
        assert_eq!(
            build_instance_subject("chat.server.001"),
            "myserver.mail.notify.instance.Y2hhdC5zZXJ2ZXIuMDAx"
        );
        assert_eq!(
            build_instance_subject("chat-server-001"),
            "myserver.mail.notify.instance.Y2hhdC1zZXJ2ZXItMDAx"
        );
    }

    #[test]
    fn accepts_v1_envelope_and_legacy_payload() {
        let parsed = parse_notification(&envelope(), DEFAULT_MAX_PAYLOAD_BYTES).unwrap();
        assert_eq!(parsed.event_id.as_deref(), Some("mail.notify:mail_001"));
        assert_eq!(parsed.player_id, "player_001");

        let parsed = parse_notification(&legacy_payload(), DEFAULT_MAX_PAYLOAD_BYTES).unwrap();
        assert_eq!(parsed.event_id, None);
        assert_eq!(parsed.from_player_id, "system");
    }

    #[test]
    fn rejects_payload_before_json_parsing_when_byte_limit_is_exceeded() {
        let error = parse_notification(br#"{"player_id":"player_001"}"#, 8).unwrap_err();
        assert_eq!(error.kind, ParseErrorKind::PayloadTooLarge);
    }

    #[test]
    fn reports_unknown_envelope_version_separately() {
        let mut value: serde_json::Value = serde_json::from_slice(&envelope()).unwrap();
        value["version"] = serde_json::json!(2);
        let payload = serde_json::to_vec(&value).unwrap();
        let error = parse_notification(&payload, DEFAULT_MAX_PAYLOAD_BYTES).unwrap_err();
        assert_eq!(error.kind, ParseErrorKind::UnsupportedVersion);
    }

    #[test]
    fn validates_contract_fields_by_utf8_byte_length() {
        let mut value: serde_json::Value = serde_json::from_slice(&envelope()).unwrap();
        value["mail"]["title"] = serde_json::json!("界".repeat(86));
        let payload = serde_json::to_vec(&value).unwrap();
        let error = parse_notification(&payload, DEFAULT_MAX_PAYLOAD_BYTES).unwrap_err();
        assert_eq!(error.kind, ParseErrorKind::InvalidEnvelope);
    }

    #[test]
    fn rejects_unknown_event_type_and_invalid_trace_id() {
        let mut value: serde_json::Value = serde_json::from_slice(&envelope()).unwrap();
        value["event_type"] = serde_json::json!("mail.updated");
        let payload = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            parse_notification(&payload, DEFAULT_MAX_PAYLOAD_BYTES)
                .unwrap_err()
                .kind,
            ParseErrorKind::UnsupportedEventType
        );

        value["event_type"] = serde_json::json!("mail.created");
        value["trace_id"] = serde_json::json!("0123456789ABCDEF0123456789ABCDEF");
        let payload = serde_json::to_vec(&value).unwrap();
        assert_eq!(
            parse_notification(&payload, DEFAULT_MAX_PAYLOAD_BYTES)
                .unwrap_err()
                .kind,
            ParseErrorKind::InvalidEnvelope
        );
    }

    #[test]
    fn deduplicator_has_fixed_ttl_and_deterministic_capacity_eviction() {
        let started = Instant::now();
        let mut deduplicator = EventDeduplicator::new(2, Duration::from_secs(10));
        assert!(!deduplicator.seen_or_insert("event-a", started));
        assert!(!deduplicator.seen_or_insert("event-b", started + Duration::from_secs(1)));
        assert!(deduplicator.seen_or_insert("event-a", started + Duration::from_secs(2)));

        assert!(!deduplicator.seen_or_insert("event-c", started + Duration::from_secs(3)));
        assert!(!deduplicator.entries.contains_key("event-a"));
        assert!(deduplicator.entries.contains_key("event-b"));
        assert!(deduplicator.entries.contains_key("event-c"));

        assert!(!deduplicator.seen_or_insert("event-a", started + Duration::from_secs(20)));
        assert_eq!(deduplicator.entries.len(), 1);
    }

    #[tokio::test]
    async fn push_only_targets_the_current_player_session() {
        let sessions = crate::chat_service::new_chat_session_map();
        let (target_tx, mut target_rx) = tokio::sync::mpsc::channel(1);
        let (other_tx, mut other_rx) = tokio::sync::mpsc::channel(1);
        sessions
            .write()
            .await
            .insert("player_001".to_string(), target_tx);
        sessions
            .write()
            .await
            .insert("player_002".to_string(), other_tx);
        let notification = parse_notification(&envelope(), DEFAULT_MAX_PAYLOAD_BYTES).unwrap();

        assert_eq!(
            push_mail_to_player(&sessions, "player_001", &notification).await,
            PushOutcome::Pushed
        );
        assert_eq!(
            target_rx.recv().await.unwrap().message_type,
            MessageType::MailNotifyPush as u16
        );
        assert!(other_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn both_subject_routes_share_event_id_deduplication() {
        let sessions = crate::chat_service::new_chat_session_map();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        sessions
            .write()
            .await
            .insert("player_001".to_string(), sender);
        let mut deduplicator =
            EventDeduplicator::new(DEFAULT_DEDUP_CAPACITY, Duration::from_secs(60));
        let payload = envelope();

        handle_notification(
            &sessions,
            &payload,
            "legacy",
            DEFAULT_MAX_PAYLOAD_BYTES,
            &mut deduplicator,
        )
        .await;
        handle_notification(
            &sessions,
            &payload,
            "instance",
            DEFAULT_MAX_PAYLOAD_BYTES,
            &mut deduplicator,
        )
        .await;

        assert!(receiver.recv().await.is_some());
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn push_classifies_offline_full_and_closed_sessions() {
        let notification = parse_notification(&envelope(), DEFAULT_MAX_PAYLOAD_BYTES).unwrap();
        let sessions = crate::chat_service::new_chat_session_map();
        assert_eq!(
            push_mail_to_player(&sessions, "player_001", &notification).await,
            PushOutcome::Offline
        );

        let (full_tx, _full_rx) = tokio::sync::mpsc::channel(1);
        full_tx
            .try_send(OutboundMessage {
                message_type: 1,
                seq: 0,
                body: vec![],
            })
            .unwrap();
        sessions
            .write()
            .await
            .insert("player_001".to_string(), full_tx);
        assert_eq!(
            push_mail_to_player(&sessions, "player_001", &notification).await,
            PushOutcome::QueueFull
        );

        let (closed_tx, closed_rx) = tokio::sync::mpsc::channel(1);
        drop(closed_rx);
        sessions
            .write()
            .await
            .insert("player_001".to_string(), closed_tx);
        assert_eq!(
            push_mail_to_player(&sessions, "player_001", &notification).await,
            PushOutcome::QueueClosed
        );
    }

    #[tokio::test]
    async fn subscriber_exits_without_connecting_when_shutdown_is_already_requested() {
        let (_shutdown_tx, shutdown_rx) = watch::channel(true);
        let result = subscribe_mail_notifications(
            "nats://invalid.invalid:4222".to_string(),
            "chat-server-test".to_string(),
            crate::chat_service::new_chat_session_map(),
            SubscriberConfig::default(),
            shutdown_rx,
        )
        .await;

        assert!(result.is_ok());
    }
}
