use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use prost::Message;
use tokio::sync::Mutex;

use crate::core::context::ConnectionContext;
use crate::pb::{
    CharacterDisciplineChangePush, CharacterElementsChangePush, CharacterPushMeta,
    CharacterTitleChangePush,
};
use crate::protocol::{MessageType, encode_body};

const DEFAULT_MAX_EVENTS_PER_CHARACTER: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterPushSource {
    pub source_type: String,
    pub source_id: String,
    pub action: String,
    pub summary: String,
}

impl CharacterPushSource {
    pub fn new(
        source_type: impl Into<String>,
        source_id: impl Into<String>,
        action: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            source_type: source_type.into(),
            source_id: source_id.into(),
            action: action.into(),
            summary: summary.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterPushRecord {
    pub character_id: String,
    pub sequence: u64,
    pub revision: u64,
    pub message_type: MessageType,
    pub body: Vec<u8>,
}

#[derive(Clone)]
pub struct CharacterPushService {
    inner: Arc<Mutex<CharacterPushState>>,
}

impl CharacterPushService {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_MAX_EVENTS_PER_CHARACTER)
    }

    pub fn with_capacity(max_events_per_character: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(CharacterPushState {
                max_events_per_character: max_events_per_character.max(1),
                ..CharacterPushState::default()
            })),
        }
    }

    pub async fn record_elements_change(
        &self,
        character_id: &str,
        source: CharacterPushSource,
        mut push: CharacterElementsChangePush,
    ) -> CharacterPushRecord {
        self.record(
            character_id,
            source,
            MessageType::CharacterElementsChangePush,
            |meta| {
                push.meta = Some(meta);
                push
            },
        )
        .await
    }

    pub async fn record_title_change(
        &self,
        character_id: &str,
        source: CharacterPushSource,
        mut push: CharacterTitleChangePush,
    ) -> CharacterPushRecord {
        self.record(
            character_id,
            source,
            MessageType::CharacterTitleChangePush,
            |meta| {
                push.meta = Some(meta);
                push
            },
        )
        .await
    }

    pub async fn record_discipline_change(
        &self,
        character_id: &str,
        source: CharacterPushSource,
        mut push: CharacterDisciplineChangePush,
    ) -> CharacterPushRecord {
        self.record(
            character_id,
            source,
            MessageType::CharacterDisciplineChangePush,
            |meta| {
                push.meta = Some(meta);
                push
            },
        )
        .await
    }

    pub async fn events_after(
        &self,
        character_id: &str,
        sequence: u64,
    ) -> Vec<CharacterPushRecord> {
        let state = self.inner.lock().await;
        state
            .events
            .get(character_id)
            .map(|events| {
                events
                    .iter()
                    .filter(|event| event.sequence > sequence)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub async fn compensation_events_after(
        &self,
        character_id: &str,
        sequence: u64,
    ) -> Vec<CharacterPushRecord> {
        self.events_after(character_id, sequence)
            .await
            .into_iter()
            .filter_map(mark_record_as_snapshot_compensation)
            .collect()
    }

    pub async fn latest_revision(&self, character_id: &str) -> u64 {
        self.inner
            .lock()
            .await
            .next_sequence
            .get(character_id)
            .copied()
            .unwrap_or(0)
    }

    async fn record<M, F>(
        &self,
        character_id: &str,
        source: CharacterPushSource,
        message_type: MessageType,
        build_message: F,
    ) -> CharacterPushRecord
    where
        M: Message,
        F: FnOnce(CharacterPushMeta) -> M,
    {
        let mut state = self.inner.lock().await;
        let sequence = {
            let next = state
                .next_sequence
                .entry(character_id.to_string())
                .or_insert(0);
            *next += 1;
            *next
        };
        let revision = sequence;
        let meta = CharacterPushMeta {
            character_id: character_id.to_string(),
            sequence,
            revision,
            source_type: source.source_type,
            source_id: source.source_id,
            action: source.action,
            summary: source.summary,
            snapshot_compensation: false,
        };
        let message = build_message(meta);

        let record = CharacterPushRecord {
            character_id: character_id.to_string(),
            sequence,
            revision,
            message_type,
            body: encode_body(&message),
        };
        state.push(record.clone());
        record
    }
}

fn mark_record_as_snapshot_compensation(
    mut record: CharacterPushRecord,
) -> Option<CharacterPushRecord> {
    match record.message_type {
        MessageType::CharacterElementsChangePush => {
            let mut push = CharacterElementsChangePush::decode(record.body.as_slice()).ok()?;
            if let Some(meta) = push.meta.as_mut() {
                meta.snapshot_compensation = true;
            }
            record.body = encode_body(&push);
            Some(record)
        }
        MessageType::CharacterTitleChangePush => {
            let mut push = CharacterTitleChangePush::decode(record.body.as_slice()).ok()?;
            if let Some(meta) = push.meta.as_mut() {
                meta.snapshot_compensation = true;
            }
            record.body = encode_body(&push);
            Some(record)
        }
        MessageType::CharacterDisciplineChangePush => {
            let mut push = CharacterDisciplineChangePush::decode(record.body.as_slice()).ok()?;
            if let Some(meta) = push.meta.as_mut() {
                meta.snapshot_compensation = true;
            }
            record.body = encode_body(&push);
            Some(record)
        }
        _ => None,
    }
}

impl Default for CharacterPushService {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Default)]
struct CharacterPushState {
    max_events_per_character: usize,
    next_sequence: BTreeMap<String, u64>,
    events: BTreeMap<String, VecDeque<CharacterPushRecord>>,
}

impl CharacterPushState {
    fn push(&mut self, record: CharacterPushRecord) {
        let events = self.events.entry(record.character_id.clone()).or_default();
        events.push_back(record);
        while events.len() > self.max_events_per_character {
            events.pop_front();
        }
    }
}

pub fn queue_character_push(
    connection: &ConnectionContext,
    identity_character_id: &str,
    record: &CharacterPushRecord,
) -> Result<(), std::io::Error> {
    ensure_character_push_receiver(identity_character_id, record)?;
    connection.queue_raw_message(record.message_type, 0, record.body.clone())
}

pub fn ensure_character_push_receiver(
    identity_character_id: &str,
    record: &CharacterPushRecord,
) -> Result<(), std::io::Error> {
    if record.character_id != identity_character_id {
        return Err(std::io::Error::other("character push receiver mismatch"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pb::{CharacterElements, ElementValues};

    fn elements() -> CharacterElements {
        CharacterElements {
            affinity: Some(ElementValues {
                earth: 2500,
                fire: 2500,
                water: 2500,
                wind: 2500,
            }),
            mastery: Some(ElementValues {
                earth: 0,
                fire: 1,
                water: 2,
                wind: 3,
            }),
        }
    }

    #[tokio::test]
    async fn record_assigns_per_character_sequence_and_filters_compensation() {
        let service = CharacterPushService::with_capacity(2);
        let source = CharacterPushSource::new("gm", "debug", "element_change", "debug update");

        let first = service
            .record_elements_change(
                "chr_1",
                source.clone(),
                CharacterElementsChangePush {
                    meta: None,
                    before: Some(elements()),
                    after: Some(elements()),
                },
            )
            .await;
        let second = service
            .record_elements_change(
                "chr_1",
                source.clone(),
                CharacterElementsChangePush {
                    meta: None,
                    before: Some(elements()),
                    after: Some(elements()),
                },
            )
            .await;
        let other = service
            .record_elements_change(
                "chr_2",
                source,
                CharacterElementsChangePush {
                    meta: None,
                    before: Some(elements()),
                    after: Some(elements()),
                },
            )
            .await;

        assert_eq!(first.sequence, 1);
        assert_eq!(second.sequence, 2);
        assert_eq!(other.sequence, 1);
        assert_eq!(service.latest_revision("chr_1").await, 2);
        assert_eq!(
            service
                .events_after("chr_1", 1)
                .await
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[tokio::test]
    async fn bounded_outbox_keeps_latest_events() {
        let service = CharacterPushService::with_capacity(1);
        let source = CharacterPushSource::new("gm", "debug", "element_change", "debug update");
        for _ in 0..2 {
            service
                .record_elements_change(
                    "chr_1",
                    source.clone(),
                    CharacterElementsChangePush {
                        meta: None,
                        before: Some(elements()),
                        after: Some(elements()),
                    },
                )
                .await;
        }

        let events = service.events_after("chr_1", 0).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence, 2);
    }

    #[tokio::test]
    async fn compensation_events_mark_meta_without_mutating_outbox() {
        let service = CharacterPushService::new();
        let record = service
            .record_elements_change(
                "chr_1",
                CharacterPushSource::new("gm", "debug", "element_change", "debug update"),
                CharacterElementsChangePush {
                    meta: None,
                    before: Some(elements()),
                    after: Some(elements()),
                },
            )
            .await;

        let original =
            CharacterElementsChangePush::decode(record.body.as_slice()).expect("decode original");
        assert!(
            !original
                .meta
                .as_ref()
                .expect("original meta")
                .snapshot_compensation
        );

        let compensation = service.compensation_events_after("chr_1", 0).await;
        assert_eq!(compensation.len(), 1);
        let replay = CharacterElementsChangePush::decode(compensation[0].body.as_slice())
            .expect("decode compensation");
        assert!(
            replay
                .meta
                .as_ref()
                .expect("compensation meta")
                .snapshot_compensation
        );

        let outbox = service.events_after("chr_1", 0).await;
        let after = CharacterElementsChangePush::decode(outbox[0].body.as_slice())
            .expect("decode outbox after compensation");
        assert!(
            !after
                .meta
                .as_ref()
                .expect("outbox meta")
                .snapshot_compensation
        );
    }

    #[tokio::test]
    async fn compensation_events_are_scoped_to_requested_character() {
        let service = CharacterPushService::new();
        let source = CharacterPushSource::new("gm", "debug", "element_change", "debug update");
        for character_id in ["chr_1", "chr_2"] {
            service
                .record_elements_change(
                    character_id,
                    source.clone(),
                    CharacterElementsChangePush {
                        meta: None,
                        before: Some(elements()),
                        after: Some(elements()),
                    },
                )
                .await;
        }

        let compensation = service.compensation_events_after("chr_2", 0).await;

        assert_eq!(compensation.len(), 1);
        assert_eq!(compensation[0].character_id, "chr_2");
        let replay = CharacterElementsChangePush::decode(compensation[0].body.as_slice())
            .expect("decode compensation");
        assert_eq!(
            replay
                .meta
                .as_ref()
                .expect("compensation meta")
                .character_id,
            "chr_2"
        );
    }

    #[test]
    fn character_push_receiver_mismatch_returns_error() {
        let record = CharacterPushRecord {
            character_id: "chr_1".to_string(),
            sequence: 1,
            revision: 1,
            message_type: MessageType::CharacterElementsChangePush,
            body: Vec::new(),
        };

        let error = ensure_character_push_receiver("chr_2", &record).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::Other);
    }
}
