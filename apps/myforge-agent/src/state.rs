use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Mutex;

use tokio::sync::Mutex as AsyncMutex;
use tokio_util::sync::CancellationToken;

use crate::protocol::ProtocolError;
use crate::schemas::CommandRejection;

#[derive(Debug)]
struct ReplayInner {
    entries: HashMap<String, u64>,
    expirations: BinaryHeap<Reverse<(u64, String)>>,
}

#[derive(Debug)]
pub struct ReplayCache {
    capacity: usize,
    inner: Mutex<ReplayInner>,
}

impl ReplayCache {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "replay cache capacity must be positive");
        Self {
            capacity,
            inner: Mutex::new(ReplayInner {
                entries: HashMap::new(),
                expirations: BinaryHeap::new(),
            }),
        }
    }

    pub fn check_and_insert(
        &self,
        key: String,
        expires_at_ms: u64,
        now_ms: u64,
    ) -> Result<(), ProtocolError> {
        let mut inner = self.inner.lock().map_err(|_| {
            ProtocolError::new(
                "MYFORGE_AGENT_BUSY",
                "replay cache is temporarily unavailable",
            )
        })?;
        while let Some(Reverse((expiration, expired_key))) = inner.expirations.peek() {
            if *expiration >= now_ms {
                break;
            }
            let expiration = *expiration;
            let expired_key = expired_key.clone();
            inner.expirations.pop();
            if inner.entries.get(&expired_key) == Some(&expiration) {
                inner.entries.remove(&expired_key);
            }
        }
        if inner.entries.contains_key(&key) {
            return Err(ProtocolError::new(
                "MYFORGE_REPLAY_DETECTED",
                "message nonce was already used",
            ));
        }
        if inner.entries.len() >= self.capacity {
            return Err(ProtocolError::new(
                "MYFORGE_AGENT_BUSY",
                "replay cache capacity is exhausted",
            ));
        }
        inner.entries.insert(key.clone(), expires_at_ms);
        inner.expirations.push(Reverse((expires_at_ms, key)));
        Ok(())
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().unwrap().entries.len()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CachedResponse {
    CommandError(CommandRejection),
    NoReplay,
}

#[derive(Debug)]
struct ActiveRequest {
    connection_id: String,
    digest: String,
    cancellation: CancellationToken,
    started_at_ms: Option<u64>,
    cancel_deadline_at_ms: Option<u64>,
}

#[derive(Debug)]
struct CompletedRequest {
    digest: String,
    response: CachedResponse,
    expires_at_ms: u64,
}

#[derive(Debug)]
enum RequestEntry {
    Active(ActiveRequest),
    Completed(CompletedRequest),
}

#[derive(Clone, Debug)]
pub enum ExecuteDecision {
    New {
        cancellation: CancellationToken,
    },
    DuplicateActive {
        started_at_ms: Option<u64>,
        cancellation: CancellationToken,
    },
    DuplicateCompleted {
        response: CachedResponse,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelDecision {
    First,
    Duplicate,
}

#[derive(Debug)]
pub struct RequestRegistry {
    capacity: usize,
    entries: AsyncMutex<HashMap<String, RequestEntry>>,
}

impl RequestRegistry {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "request registry capacity must be positive");
        Self {
            capacity,
            entries: AsyncMutex::new(HashMap::new()),
        }
    }

    pub async fn begin(
        &self,
        connection_id: &str,
        request_id: &str,
        digest: &str,
        now_ms: u64,
    ) -> Result<ExecuteDecision, ProtocolError> {
        let mut entries = self.entries.lock().await;
        Self::remove_expired(&mut entries, now_ms);
        if let Some(entry) = entries.get(request_id) {
            return match entry {
                RequestEntry::Active(active) if active.digest == digest => {
                    Ok(ExecuteDecision::DuplicateActive {
                        started_at_ms: active.started_at_ms,
                        cancellation: active.cancellation.clone(),
                    })
                }
                RequestEntry::Completed(completed) if completed.digest == digest => {
                    Ok(ExecuteDecision::DuplicateCompleted {
                        response: completed.response.clone(),
                    })
                }
                _ => Err(ProtocolError::new(
                    "MYFORGE_DUPLICATE_REQUEST_CONFLICT",
                    "request conflicts with an existing request",
                )
                .with_request_id(Some(request_id.to_string()))),
            };
        }
        if entries
            .values()
            .any(|entry| matches!(entry, RequestEntry::Active(_)))
        {
            return Err(ProtocolError::new(
                "MYFORGE_AGENT_BUSY",
                "agent already has an active request",
            )
            .with_request_id(Some(request_id.to_string())));
        }
        if entries.len() >= self.capacity {
            return Err(ProtocolError::new(
                "MYFORGE_AGENT_BUSY",
                "request registry capacity is exhausted",
            )
            .with_request_id(Some(request_id.to_string())));
        }
        let cancellation = CancellationToken::new();
        entries.insert(
            request_id.to_string(),
            RequestEntry::Active(ActiveRequest {
                connection_id: connection_id.to_string(),
                digest: digest.to_string(),
                cancellation: cancellation.clone(),
                started_at_ms: None,
                cancel_deadline_at_ms: None,
            }),
        );
        Ok(ExecuteDecision::New { cancellation })
    }

    pub async fn mark_started(
        &self,
        request_id: &str,
        started_at_ms: u64,
    ) -> Result<(), ProtocolError> {
        let mut entries = self.entries.lock().await;
        let Some(RequestEntry::Active(active)) = entries.get_mut(request_id) else {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "request is not active",
            )
            .with_request_id(Some(request_id.to_string())));
        };
        match active.started_at_ms {
            None => active.started_at_ms = Some(started_at_ms),
            Some(existing) if existing == started_at_ms => {}
            Some(_) => {
                return Err(ProtocolError::new(
                    "MYFORGE_DUPLICATE_REQUEST_CONFLICT",
                    "request start time conflicts with the active request",
                )
                .with_request_id(Some(request_id.to_string())));
            }
        }
        Ok(())
    }

    pub async fn complete_error(
        &self,
        request_id: &str,
        response: CommandRejection,
        now_ms: u64,
        retention_ms: u64,
    ) -> Result<(), ProtocolError> {
        let mut entries = self.entries.lock().await;
        let Some(RequestEntry::Active(active)) = entries.remove(request_id) else {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "request is not active",
            )
            .with_request_id(Some(request_id.to_string())));
        };
        entries.insert(
            request_id.to_string(),
            RequestEntry::Completed(CompletedRequest {
                digest: active.digest,
                response: CachedResponse::CommandError(response),
                expires_at_ms: now_ms.saturating_add(retention_ms),
            }),
        );
        Ok(())
    }

    pub async fn cancel(
        &self,
        request_id: &str,
        deadline_at_ms: u64,
    ) -> Result<CancelDecision, ProtocolError> {
        let mut entries = self.entries.lock().await;
        let Some(entry) = entries.get_mut(request_id) else {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "cancel request does not refer to an active command",
            )
            .with_request_id(Some(request_id.to_string())));
        };
        let RequestEntry::Active(active) = entry else {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "cancel request refers to a completed command",
            )
            .with_request_id(Some(request_id.to_string())));
        };
        match active.cancel_deadline_at_ms {
            Some(existing) if existing == deadline_at_ms => Ok(CancelDecision::Duplicate),
            Some(_) => Err(ProtocolError::new(
                "MYFORGE_DUPLICATE_REQUEST_CONFLICT",
                "cancel deadline conflicts with the active request",
            )
            .with_request_id(Some(request_id.to_string()))),
            None => {
                active.cancel_deadline_at_ms = Some(deadline_at_ms);
                active.cancellation.cancel();
                Ok(CancelDecision::First)
            }
        }
    }

    pub async fn active_request_for_connection(&self, connection_id: &str) -> Option<String> {
        let entries = self.entries.lock().await;
        entries.iter().find_map(|(request_id, entry)| match entry {
            RequestEntry::Active(active) if active.connection_id == connection_id => {
                Some(request_id.clone())
            }
            _ => None,
        })
    }

    pub async fn disconnect_connection(&self, connection_id: &str, now_ms: u64, retention_ms: u64) {
        let mut entries = self.entries.lock().await;
        let request_ids = entries
            .iter()
            .filter_map(|(request_id, entry)| match entry {
                RequestEntry::Active(active) if active.connection_id == connection_id => {
                    Some(request_id.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for request_id in request_ids {
            let Some(RequestEntry::Active(active)) = entries.remove(&request_id) else {
                continue;
            };
            active.cancellation.cancel();
            entries.insert(
                request_id,
                RequestEntry::Completed(CompletedRequest {
                    digest: active.digest,
                    response: CachedResponse::NoReplay,
                    expires_at_ms: now_ms.saturating_add(retention_ms),
                }),
            );
        }
    }

    pub async fn cancel_all(&self) {
        let entries = self.entries.lock().await;
        for entry in entries.values() {
            if let RequestEntry::Active(active) = entry {
                active.cancellation.cancel();
            }
        }
    }

    fn remove_expired(entries: &mut HashMap<String, RequestEntry>, now_ms: u64) {
        entries.retain(|_, entry| match entry {
            RequestEntry::Active(_) => true,
            RequestEntry::Completed(completed) => completed.expires_at_ms >= now_ms,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_cache_never_evicts_live_entries() {
        let cache = ReplayCache::new(1);
        cache
            .check_and_insert("first".to_string(), 200, 100)
            .unwrap();
        assert_eq!(
            cache
                .check_and_insert("second".to_string(), 300, 100)
                .unwrap_err()
                .code(),
            "MYFORGE_AGENT_BUSY"
        );
        assert_eq!(
            cache
                .check_and_insert("first".to_string(), 300, 100)
                .unwrap_err()
                .code(),
            "MYFORGE_REPLAY_DETECTED"
        );
        cache
            .check_and_insert("second".to_string(), 300, 201)
            .unwrap();
        assert_eq!(cache.len(), 1);
    }

    #[tokio::test]
    async fn request_registry_is_idempotent_and_detects_conflicts() {
        let registry = RequestRegistry::new(2);
        let first = registry
            .begin("connection", "request", "digest-a", 100)
            .await
            .unwrap();
        let ExecuteDecision::New { cancellation } = first else {
            panic!("expected new request");
        };
        let duplicate = registry
            .begin("connection", "request", "digest-a", 100)
            .await
            .unwrap();
        assert!(matches!(duplicate, ExecuteDecision::DuplicateActive { .. }));
        assert_eq!(
            registry
                .begin("connection", "request", "digest-b", 100)
                .await
                .unwrap_err()
                .code(),
            "MYFORGE_DUPLICATE_REQUEST_CONFLICT"
        );

        assert_eq!(
            registry.cancel("request", 500).await.unwrap(),
            CancelDecision::First
        );
        assert!(cancellation.is_cancelled());
        assert_eq!(
            registry.cancel("request", 500).await.unwrap(),
            CancelDecision::Duplicate
        );
        assert_eq!(
            registry.cancel("request", 501).await.unwrap_err().code(),
            "MYFORGE_DUPLICATE_REQUEST_CONFLICT"
        );

        assert_eq!(
            registry
                .begin("connection", "second-request", "digest-c", 100)
                .await
                .unwrap_err()
                .code(),
            "MYFORGE_AGENT_BUSY"
        );
    }
}
