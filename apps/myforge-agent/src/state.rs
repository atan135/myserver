use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, MutexGuard, Notify};

use crate::command::CommandCancellation;
use crate::protocol::ProtocolError;
use crate::schemas::{CommandRejection, CommandResultSemantic};

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
    CommandResult(Box<CommandResultSemantic>),
    NoReplay,
}

#[derive(Debug)]
pub struct DeliveryGeneration {
    current: AtomicU64,
    changed: Notify,
    gate: AsyncMutex<()>,
}

impl DeliveryGeneration {
    fn new() -> Self {
        Self {
            current: AtomicU64::new(2),
            changed: Notify::new(),
            gate: AsyncMutex::new(()),
        }
    }

    pub async fn lock(&self) -> MutexGuard<'_, ()> {
        self.gate.lock().await
    }

    pub fn lease(self: &Arc<Self>) -> DeliveryLease {
        let state = self.current.load(Ordering::Acquire);
        DeliveryLease {
            owner: self.clone(),
            generation: state & !1,
        }
    }

    fn invalidate(&self) {
        self.current.fetch_add(2, Ordering::AcqRel);
        self.changed.notify_waiters();
    }

    fn started_committed(&self) -> bool {
        self.current.load(Ordering::Acquire) & 1 == 1
    }
}

#[derive(Clone, Debug)]
pub struct DeliveryLease {
    owner: Arc<DeliveryGeneration>,
    generation: u64,
}

impl DeliveryLease {
    pub fn is_current(&self) -> bool {
        self.owner.current.load(Ordering::Acquire) & !1 == self.generation
    }

    pub fn try_commit_current(&self) -> bool {
        loop {
            let state = self.owner.current.load(Ordering::Acquire);
            if state & !1 != self.generation {
                return false;
            }
            if self
                .owner
                .current
                .compare_exchange(state, state, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    pub fn try_commit_started(&self) -> bool {
        loop {
            let state = self.owner.current.load(Ordering::Acquire);
            if state & !1 != self.generation {
                return false;
            }
            if self
                .owner
                .current
                .compare_exchange(state, state | 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    pub async fn superseded(&self) {
        loop {
            let notified = self.owner.changed.notified();
            if !self.is_current() {
                return;
            }
            notified.await;
        }
    }
}

#[derive(Debug)]
struct ActiveRequest {
    connection_id: String,
    digest: String,
    cancellation: CommandCancellation,
    started_at_ms: Option<u64>,
    cancel_deadline_at_ms: Option<u64>,
    cancellation_result: Box<CommandResultSemantic>,
    delivery: Arc<DeliveryGeneration>,
}

impl ActiveRequest {
    fn committed_started_at_ms(&self) -> Option<u64> {
        if self.delivery.started_committed() {
            self.started_at_ms
        } else {
            None
        }
    }

    fn started_candidate(&self, started_at_ms: u64) -> Option<StartedDeliveryCandidate> {
        self.cancel_deadline_at_ms
            .is_none()
            .then(|| StartedDeliveryCandidate {
                started_at_ms,
                delivery: self.delivery.clone(),
                lease: self.delivery.lease(),
            })
    }
}

struct CompletedRequest {
    connection_id: String,
    digest: String,
    response: CachedResponse,
    cancel_deadline_at_ms: Option<u64>,
    cancellation_result: Box<CommandResultSemantic>,
    delivery: Arc<DeliveryGeneration>,
    expires_at_ms: u64,
}

enum RequestEntry {
    Active(ActiveRequest),
    Completed(CompletedRequest),
}

#[derive(Clone, Debug)]
pub struct StartedDeliveryCandidate {
    pub(crate) started_at_ms: u64,
    pub(crate) delivery: Arc<DeliveryGeneration>,
    pub(crate) lease: DeliveryLease,
}

#[derive(Clone, Debug)]
pub enum ExecuteDecision {
    New {
        cancellation: CommandCancellation,
    },
    DuplicateActive {
        started: Option<StartedDeliveryCandidate>,
        cancellation: CommandCancellation,
    },
    DuplicateCompleted {
        response: CachedResponse,
        cancel_deadline_at_ms: Option<u64>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CancelDecision {
    First,
    DuplicateActive,
    CompletedNeedsCancellation {
        response: CachedResponse,
        started_at_ms: Option<u64>,
    },
    DuplicateCompleted {
        response: CachedResponse,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompletionDecision {
    Stored,
    CancellationRequired {
        response: CachedResponse,
        started_at_ms: Option<u64>,
    },
}

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
        cancellation_result: CommandResultSemantic,
        now_ms: u64,
    ) -> Result<ExecuteDecision, ProtocolError> {
        let mut entries = self.entries.lock().await;
        Self::remove_expired(&mut entries, now_ms);
        if let Some(entry) = entries.get(request_id) {
            return match entry {
                RequestEntry::Active(active) if active.digest == digest => {
                    Ok(ExecuteDecision::DuplicateActive {
                        started: active
                            .committed_started_at_ms()
                            .and_then(|started_at_ms| active.started_candidate(started_at_ms)),
                        cancellation: active.cancellation.clone(),
                    })
                }
                RequestEntry::Completed(completed) if completed.digest == digest => {
                    Ok(ExecuteDecision::DuplicateCompleted {
                        response: completed.response.clone(),
                        cancel_deadline_at_ms: completed.cancel_deadline_at_ms,
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
        let cancellation = CommandCancellation::new();
        let delivery = Arc::new(DeliveryGeneration::new());
        entries.insert(
            request_id.to_string(),
            RequestEntry::Active(ActiveRequest {
                connection_id: connection_id.to_string(),
                digest: digest.to_string(),
                cancellation: cancellation.clone(),
                started_at_ms: None,
                cancel_deadline_at_ms: None,
                cancellation_result: Box::new(cancellation_result),
                delivery,
            }),
        );
        Ok(ExecuteDecision::New { cancellation })
    }

    pub async fn mark_started(
        &self,
        request_id: &str,
        started_at_ms: u64,
    ) -> Result<Option<StartedDeliveryCandidate>, ProtocolError> {
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
        Ok(active.started_candidate(started_at_ms))
    }

    pub async fn complete_error(
        &self,
        request_id: &str,
        response: CommandRejection,
        now_ms: u64,
        retention_ms: u64,
    ) -> Result<(), ProtocolError> {
        match self
            .complete_response(
                request_id,
                CachedResponse::CommandError(response),
                now_ms,
                retention_ms,
            )
            .await?
        {
            CompletionDecision::Stored => Ok(()),
            CompletionDecision::CancellationRequired { .. } => Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "command cancellation won the completion race",
            )
            .with_request_id(Some(request_id.to_string()))),
        }
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
        if let RequestEntry::Completed(completed) = entry {
            return match completed.cancel_deadline_at_ms {
                Some(existing) if existing == deadline_at_ms => {
                    Ok(CancelDecision::DuplicateCompleted {
                        response: completed.response.clone(),
                    })
                }
                Some(_) => Err(ProtocolError::new(
                    "MYFORGE_DUPLICATE_REQUEST_CONFLICT",
                    "cancel deadline conflicts with the completed request",
                )
                .with_request_id(Some(request_id.to_string()))),
                None if !matches!(&completed.response, CachedResponse::NoReplay) => {
                    completed.delivery.invalidate();
                    completed.cancel_deadline_at_ms = Some(deadline_at_ms);
                    let (response, started_at_ms) = match &completed.response {
                        CachedResponse::CommandResult(result) => {
                            (completed.response.clone(), result.started_at_ms)
                        }
                        CachedResponse::CommandError(_) => (
                            CachedResponse::CommandResult(completed.cancellation_result.clone()),
                            None,
                        ),
                        CachedResponse::NoReplay => unreachable!("NoReplay was excluded by guard"),
                    };
                    Ok(CancelDecision::CompletedNeedsCancellation {
                        response,
                        started_at_ms,
                    })
                }
                None => Err(ProtocolError::new(
                    "MYFORGE_PROTOCOL_STATE_INVALID",
                    "cancel request refers to a completed command",
                )
                .with_request_id(Some(request_id.to_string()))),
            };
        }
        let RequestEntry::Active(active) = entry else {
            unreachable!("completed requests returned above")
        };
        match active.cancel_deadline_at_ms {
            Some(existing) if existing == deadline_at_ms => Ok(CancelDecision::DuplicateActive),
            Some(_) => Err(ProtocolError::new(
                "MYFORGE_DUPLICATE_REQUEST_CONFLICT",
                "cancel deadline conflicts with the active request",
            )
            .with_request_id(Some(request_id.to_string()))),
            None => {
                active.delivery.invalidate();
                active.cancel_deadline_at_ms = Some(deadline_at_ms);
                active.cancellation.cancel_at(deadline_at_ms);
                Ok(CancelDecision::First)
            }
        }
    }

    pub async fn replace_completed_with_cancelled(
        &self,
        request_id: &str,
        response: CommandResultSemantic,
    ) -> Result<(), ProtocolError> {
        let mut entries = self.entries.lock().await;
        let Some(RequestEntry::Completed(completed)) = entries.get_mut(request_id) else {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "request is not completed",
            )
            .with_request_id(Some(request_id.to_string())));
        };
        if completed.cancel_deadline_at_ms.is_none() || response.status != "cancelled" {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "completed request cannot accept this cancellation result",
            )
            .with_request_id(Some(request_id.to_string())));
        }
        completed.response = CachedResponse::CommandResult(Box::new(response));
        Ok(())
    }

    pub async fn complete_result(
        &self,
        request_id: &str,
        response: CommandResultSemantic,
        now_ms: u64,
        retention_ms: u64,
    ) -> Result<(), ProtocolError> {
        match self
            .complete_response(
                request_id,
                CachedResponse::CommandResult(Box::new(response)),
                now_ms,
                retention_ms,
            )
            .await?
        {
            CompletionDecision::Stored => Ok(()),
            CompletionDecision::CancellationRequired { .. } => Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "command cancellation won the completion race",
            )
            .with_request_id(Some(request_id.to_string()))),
        }
    }

    pub async fn complete_response(
        &self,
        request_id: &str,
        response: CachedResponse,
        now_ms: u64,
        retention_ms: u64,
    ) -> Result<CompletionDecision, ProtocolError> {
        let mut entries = self.entries.lock().await;
        let Some(RequestEntry::Active(active)) = entries.get(request_id) else {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "request is not active",
            )
            .with_request_id(Some(request_id.to_string())));
        };
        let response_is_cancelled = matches!(
            &response,
                CachedResponse::CommandResult(result) if result.status == "cancelled"
        );
        if active.cancel_deadline_at_ms.is_some() && !response_is_cancelled {
            return Ok(CompletionDecision::CancellationRequired {
                response,
                started_at_ms: active.committed_started_at_ms(),
            });
        }
        let Some(RequestEntry::Active(active)) = entries.remove(request_id) else {
            unreachable!("active request was inspected while holding the registry lock")
        };
        entries.insert(
            request_id.to_string(),
            RequestEntry::Completed(CompletedRequest {
                connection_id: active.connection_id,
                digest: active.digest,
                response,
                cancel_deadline_at_ms: active.cancel_deadline_at_ms,
                cancellation_result: active.cancellation_result,
                delivery: active.delivery,
                expires_at_ms: now_ms.saturating_add(retention_ms),
            }),
        );
        Ok(CompletionDecision::Stored)
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

    pub async fn cancel_deadline_at_ms(
        &self,
        request_id: &str,
    ) -> Result<Option<u64>, ProtocolError> {
        let entries = self.entries.lock().await;
        let Some(entry) = entries.get(request_id) else {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "request is not registered",
            )
            .with_request_id(Some(request_id.to_string())));
        };
        Ok(match entry {
            RequestEntry::Active(active) => active.cancel_deadline_at_ms,
            RequestEntry::Completed(completed) => completed.cancel_deadline_at_ms,
        })
    }

    pub async fn committed_started_at_ms(
        &self,
        request_id: &str,
    ) -> Result<Option<u64>, ProtocolError> {
        let entries = self.entries.lock().await;
        let Some(entry) = entries.get(request_id) else {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "request is not registered",
            )
            .with_request_id(Some(request_id.to_string())));
        };
        Ok(match entry {
            RequestEntry::Active(active) => active.committed_started_at_ms(),
            RequestEntry::Completed(completed) => match &completed.response {
                CachedResponse::CommandResult(result) => result.started_at_ms,
                CachedResponse::CommandError(_) | CachedResponse::NoReplay => None,
            },
        })
    }

    pub async fn delivery(
        &self,
        request_id: &str,
    ) -> Result<Arc<DeliveryGeneration>, ProtocolError> {
        let entries = self.entries.lock().await;
        let Some(entry) = entries.get(request_id) else {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "request is not registered",
            )
            .with_request_id(Some(request_id.to_string())));
        };
        Ok(match entry {
            RequestEntry::Active(active) => active.delivery.clone(),
            RequestEntry::Completed(completed) => completed.delivery.clone(),
        })
    }

    pub async fn completed_response(
        &self,
        request_id: &str,
    ) -> Result<(CachedResponse, Option<u64>), ProtocolError> {
        let entries = self.entries.lock().await;
        let Some(RequestEntry::Completed(completed)) = entries.get(request_id) else {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "request has no completed response",
            )
            .with_request_id(Some(request_id.to_string())));
        };
        Ok((completed.response.clone(), completed.cancel_deadline_at_ms))
    }

    pub async fn mark_no_replay(&self, request_id: &str) -> Result<(), ProtocolError> {
        let mut entries = self.entries.lock().await;
        let Some(RequestEntry::Completed(completed)) = entries.get_mut(request_id) else {
            return Err(ProtocolError::new(
                "MYFORGE_PROTOCOL_STATE_INVALID",
                "request has no completed response",
            )
            .with_request_id(Some(request_id.to_string())));
        };
        completed.delivery.invalidate();
        completed.response = CachedResponse::NoReplay;
        Ok(())
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
            active.delivery.invalidate();
            active.cancellation.cancel();
            entries.insert(
                request_id,
                RequestEntry::Completed(CompletedRequest {
                    connection_id: active.connection_id,
                    digest: active.digest,
                    response: CachedResponse::NoReplay,
                    cancel_deadline_at_ms: active.cancel_deadline_at_ms,
                    cancellation_result: active.cancellation_result,
                    delivery: active.delivery,
                    expires_at_ms: now_ms.saturating_add(retention_ms),
                }),
            );
        }
        for entry in entries.values_mut() {
            if let RequestEntry::Completed(completed) = entry
                && completed.connection_id == connection_id
            {
                completed.delivery.invalidate();
                completed.response = CachedResponse::NoReplay;
            }
        }
    }

    pub async fn cancel_all(&self) {
        let entries = self.entries.lock().await;
        for entry in entries.values() {
            match entry {
                RequestEntry::Active(active) => {
                    active.delivery.invalidate();
                    active.cancellation.cancel();
                }
                RequestEntry::Completed(completed) => completed.delivery.invalidate(),
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
    use crate::schemas::{ArtifactSummary, AuditSummary};

    fn cancelled_result() -> CommandResultSemantic {
        CommandResultSemantic {
            execution_mode: "codex_exec".to_string(),
            status: "cancelled".to_string(),
            exit_code: None,
            stdout_preview: String::new(),
            stderr_preview: String::new(),
            stdout_bytes: 0,
            stderr_bytes: 0,
            stdout_truncated: false,
            stderr_truncated: false,
            artifact_file: "artifacts/fangyuan/result.ron".to_string(),
            consumer_target_file: None,
            artifact: ArtifactSummary::missing(),
            audit: AuditSummary::skipped("cancelled"),
            error_code: Some("MYFORGE_COMMAND_CANCELLED".to_string()),
            error_message: Some("command was cancelled".to_string()),
            started_at_ms: None,
            completed_at_ms: 500,
        }
    }

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
            .begin("connection", "request", "digest-a", cancelled_result(), 100)
            .await
            .unwrap();
        let ExecuteDecision::New { cancellation } = first else {
            panic!("expected new request");
        };
        let duplicate = registry
            .begin("connection", "request", "digest-a", cancelled_result(), 100)
            .await
            .unwrap();
        assert!(matches!(duplicate, ExecuteDecision::DuplicateActive { .. }));
        assert_eq!(
            registry
                .begin("connection", "request", "digest-b", cancelled_result(), 100,)
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
        assert_eq!(cancellation.deadline_at_ms(), Some(500));
        assert_eq!(
            registry.cancel("request", 500).await.unwrap(),
            CancelDecision::DuplicateActive
        );
        assert_eq!(
            registry.cancel("request", 501).await.unwrap_err().code(),
            "MYFORGE_DUPLICATE_REQUEST_CONFLICT"
        );
        assert_eq!(
            registry
                .begin(
                    "connection",
                    "busy-request",
                    "digest-c",
                    cancelled_result(),
                    500,
                )
                .await
                .unwrap_err()
                .code(),
            "MYFORGE_AGENT_BUSY"
        );

        let result = cancelled_result();
        registry
            .complete_result("request", result.clone(), 500, 1_000)
            .await
            .unwrap();
        assert!(matches!(
            registry.cancel("request", 500).await.unwrap(),
            CancelDecision::DuplicateCompleted {
                response: CachedResponse::CommandResult(response)
            } if *response == result
        ));
        assert_eq!(
            registry.cancel("request", 501).await.unwrap_err().code(),
            "MYFORGE_DUPLICATE_REQUEST_CONFLICT"
        );
        assert!(matches!(
            registry
                .begin(
                    "connection",
                    "request",
                    "digest-a",
                    cancelled_result(),
                    501,
                )
                .await
                .unwrap(),
            ExecuteDecision::DuplicateCompleted {
                response: CachedResponse::CommandResult(response),
                cancel_deadline_at_ms: Some(500),
            } if *response == result
        ));

        assert!(matches!(
            registry
                .begin(
                    "connection",
                    "second-request",
                    "digest-c",
                    cancelled_result(),
                    501,
                )
                .await
                .unwrap(),
            ExecuteDecision::New { .. }
        ));
    }

    #[tokio::test]
    async fn authoritative_cancel_can_replace_a_locally_completed_result_race() {
        let registry = RequestRegistry::new(2);
        registry
            .begin("connection", "request", "digest", cancelled_result(), 100)
            .await
            .unwrap();
        let candidate = registry
            .mark_started("request", 200)
            .await
            .unwrap()
            .expect("uncancelled start must capture a delivery candidate");
        assert!(candidate.lease.try_commit_started());
        let mut completed = cancelled_result();
        completed.status = "completed".to_string();
        completed.exit_code = Some(0);
        completed.audit = AuditSummary::unavailable();
        completed.error_code = None;
        completed.error_message = None;
        completed.started_at_ms = Some(200);
        registry
            .complete_result("request", completed.clone(), 300, 1_000)
            .await
            .unwrap();

        assert!(matches!(
            registry.cancel("request", 500).await.unwrap(),
            CancelDecision::CompletedNeedsCancellation {
                response: CachedResponse::CommandResult(response),
                started_at_ms: Some(200),
            } if *response == completed
        ));
        let mut cancelled = cancelled_result();
        cancelled.started_at_ms = Some(200);
        registry
            .replace_completed_with_cancelled("request", cancelled.clone())
            .await
            .unwrap();
        assert!(matches!(
            registry.cancel("request", 500).await.unwrap(),
            CancelDecision::DuplicateCompleted {
                response: CachedResponse::CommandResult(response),
            } if *response == cancelled
        ));
        assert_eq!(
            registry.cancel("request", 501).await.unwrap_err().code(),
            "MYFORGE_DUPLICATE_REQUEST_CONFLICT"
        );
    }

    #[tokio::test]
    async fn authoritative_cancel_replaces_a_completed_command_error() {
        let registry = RequestRegistry::new(2);
        let template = cancelled_result();
        registry
            .begin("connection", "request", "digest", template.clone(), 100)
            .await
            .unwrap();
        let rejection = CommandRejection::new(
            "MYFORGE_RULES_FILE_MISSING",
            "rules file is unavailable",
            false,
        );
        registry
            .complete_error("request", rejection, 200, 1_000)
            .await
            .unwrap();

        assert!(matches!(
            registry.cancel("request", 500).await.unwrap(),
            CancelDecision::CompletedNeedsCancellation {
                response: CachedResponse::CommandResult(response),
                started_at_ms: None,
            } if *response == template
        ));
        registry
            .replace_completed_with_cancelled("request", template.clone())
            .await
            .unwrap();
        assert!(matches!(
            registry.cancel("request", 500).await.unwrap(),
            CancelDecision::DuplicateCompleted {
                response: CachedResponse::CommandResult(response),
            } if *response == template
        ));
        assert_eq!(
            registry.cancel("request", 501).await.unwrap_err().code(),
            "MYFORGE_DUPLICATE_REQUEST_CONFLICT"
        );
    }
}
