use std::time::{Duration, Instant};

use crate::config::EnvironmentKind;
use crate::match_pb::{
    CreateRoomAndJoinReq, MatchCancelReq, MatchEndReq, MatchEvent, MatchEventStreamReq,
    MatchStartReq, MatchStatusReq, PlayerJoinedReq, PlayerLeftReq,
    match_internal_client::MatchInternalClient, match_service_client::MatchServiceClient,
};
use crate::side_services::{
    PlannedSideServiceStep, ServiceDescriptor, SideServiceKind, SideServiceOperation,
};
use futures_util::StreamExt;
use tonic::transport::Endpoint;

/// Direct gRPC diagnostics never need an unbounded response backlog. The
/// tracker is also used by streaming consumers so a stalled local consumer is
/// classified before it can accumulate arbitrary messages in the generator.
pub const DEFAULT_MAX_MATCH_GRPC_PENDING: usize = 16;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MatchGrpcBackpressureMetrics {
    pub pending_limit_rejections: u64,
    pub dropped_pending_messages: u64,
    pub stream_disconnects: u64,
}

impl MatchGrpcBackpressureMetrics {
    pub fn merge_into_metrics(&self, metrics: &mut crate::metrics::Metrics) {
        for (key, value) in [
            (
                "match_grpc_backpressure_pending_limit_rejections",
                self.pending_limit_rejections,
            ),
            (
                "match_grpc_backpressure_dropped_pending_messages",
                self.dropped_pending_messages,
            ),
            (
                "match_grpc_backpressure_stream_disconnects",
                self.stream_disconnects,
            ),
        ] {
            metrics.increment(key, value);
        }
    }

    pub fn is_healthy(&self) -> bool {
        self.pending_limit_rejections == 0
            && self.dropped_pending_messages == 0
            && self.stream_disconnects == 0
    }

    pub fn apply_to_health(&self, health: &mut crate::abort::ContinuousHealthObservation) {
        health.backpressure_healthy &= self.is_healthy();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchGrpcPending {
    maximum: usize,
    pending: usize,
    metrics: MatchGrpcBackpressureMetrics,
}

impl MatchGrpcPending {
    pub fn new(maximum: usize) -> Result<Self, MatchGrpcError> {
        if maximum == 0 {
            return Err(MatchGrpcError::InvalidPendingLimit);
        }
        Ok(Self {
            maximum,
            pending: 0,
            metrics: MatchGrpcBackpressureMetrics::default(),
        })
    }

    pub fn begin(&mut self) -> Result<(), MatchGrpcError> {
        if self.pending >= self.maximum {
            self.metrics.pending_limit_rejections =
                self.metrics.pending_limit_rejections.saturating_add(1);
            return Err(MatchGrpcError::PendingLimit {
                maximum: self.maximum,
            });
        }
        self.pending = self.pending.saturating_add(1);
        Ok(())
    }

    pub fn complete(&mut self) -> Result<(), MatchGrpcError> {
        if self.pending == 0 {
            return Err(MatchGrpcError::PendingUnderflow);
        }
        self.pending -= 1;
        Ok(())
    }

    pub fn disconnect(&mut self) {
        self.metrics.stream_disconnects = self.metrics.stream_disconnects.saturating_add(1);
        self.metrics.dropped_pending_messages = self
            .metrics
            .dropped_pending_messages
            .saturating_add(self.pending as u64);
        self.pending = 0;
    }

    pub fn pending(&self) -> usize {
        self.pending
    }

    pub fn metrics(&self) -> MatchGrpcBackpressureMetrics {
        self.metrics
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchGrpcOutcome {
    Success,
    GrpcStatus,
    Timeout,
    Cancelled,
    StreamDisconnected,
    RoomCreateClosed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatchGrpcMetrics {
    pub queued_ms: u64,
    pub match_attempts: u64,
    pub match_successes: u64,
    pub event_stream_connections: u64,
    pub grpc_statuses: u64,
    pub room_create_closed_ms: u64,
    /// Direct match-service gRPC does not expose the game-server room-create
    /// callback timestamp. Keep this explicit so reports cannot infer it from
    /// client-side match latency.
    pub room_create_closed_observed: bool,
    pub cancellations: u64,
    pub timeouts: u64,
    pub stream_disconnects: u64,
    pub outcomes: std::collections::BTreeMap<MatchGrpcOutcome, u64>,
    pub backpressure: MatchGrpcBackpressureMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchInternalAdmission {
    Connection,
    Message { writes: bool },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatchInternalMetrics {
    pub operations: u64,
    pub successes: u64,
    pub statuses: u64,
    pub connections: u64,
    pub messages: u64,
    pub writes: u64,
    pub timeouts: u64,
    pub grpc_errors: u64,
    pub business_errors: u64,
    pub latency_ms: u64,
    /// MatchInternal has no server-side room-create timestamp. Keep this
    /// explicit instead of fabricating a duration from client observations.
    pub room_create_closed_observed: bool,
}

pub fn match_internal_admission_plan(
    role_count: usize,
) -> Result<Vec<MatchInternalAdmission>, MatchGrpcError> {
    if !(1..=2).contains(&role_count) {
        return Err(MatchGrpcError::Business(
            "match_internal_roles_invalid".into(),
        ));
    }
    let mut plan = vec![
        MatchInternalAdmission::Connection,
        MatchInternalAdmission::Message { writes: true },
    ];
    plan.extend(std::iter::repeat_n(
        MatchInternalAdmission::Message { writes: true },
        role_count,
    ));
    plan.push(MatchInternalAdmission::Message { writes: false });
    plan.extend(std::iter::repeat_n(
        MatchInternalAdmission::Message { writes: true },
        role_count,
    ));
    plan.push(MatchInternalAdmission::Message { writes: true });
    plan.push(MatchInternalAdmission::Message { writes: false });
    Ok(plan)
}

impl MatchInternalMetrics {
    pub fn merge_into_metrics(&self, metrics: &mut crate::metrics::Metrics) {
        for (key, value) in [
            ("match_internal_operations", self.operations),
            ("match_internal_successes", self.successes),
            ("match_internal_statuses", self.statuses),
            ("match_internal_connections", self.connections),
            ("match_internal_messages", self.messages),
            ("match_internal_writes", self.writes),
            ("match_internal_timeouts", self.timeouts),
            ("match_internal_grpc_errors", self.grpc_errors),
            ("match_internal_business_errors", self.business_errors),
            (
                "match_internal_room_create_closed_unobserved",
                u64::from(!self.room_create_closed_observed),
            ),
        ] {
            metrics.increment(key, value);
        }
        metrics.observe_latency("match_internal_ms", self.latency_ms);
    }
}

impl MatchGrpcMetrics {
    pub fn merge_into_metrics(&self, metrics: &mut crate::metrics::Metrics) {
        for (key, value) in [
            ("match_grpc_queued_ms", self.queued_ms),
            ("match_grpc_attempts", self.match_attempts),
            ("match_grpc_successes", self.match_successes),
            (
                "match_grpc_event_stream_connections",
                self.event_stream_connections,
            ),
            ("match_grpc_statuses", self.grpc_statuses),
            (
                "match_grpc_room_create_closed_ms",
                self.room_create_closed_ms,
            ),
            ("match_grpc_cancellations", self.cancellations),
            ("match_grpc_timeouts", self.timeouts),
            ("match_grpc_stream_disconnects", self.stream_disconnects),
            (
                "match_grpc_room_create_closed_unobserved",
                u64::from(!self.room_create_closed_observed),
            ),
        ] {
            metrics.increment(key, value);
        }
        if self.queued_ms > 0 {
            metrics.observe_latency("match_grpc_queue_ms", self.queued_ms);
        }
        if self.room_create_closed_observed && self.room_create_closed_ms > 0 {
            metrics.observe_latency("match_grpc_room_create_ms", self.room_create_closed_ms);
        }
        self.backpressure.merge_into_metrics(metrics);
    }
}

#[derive(Debug)]
pub enum MatchGrpcError {
    LiveTransportForbidden,
    LiveTransportNotEnabled,
    DescriptorRejected,
    Grpc(String),
    Timeout,
    StreamDisconnected,
    Business(String),
    InvalidPlan,
    InvalidPendingLimit,
    PendingLimit {
        maximum: usize,
    },
    PendingUnderflow,
    RoomCreateClosed,
    ExecutionFailed {
        source: Box<MatchGrpcError>,
        backpressure: MatchGrpcBackpressureMetrics,
    },
}

impl MatchGrpcError {
    /// Keeps bounded-pending observations available to the controller even
    /// when the live RPC fails before it can return normal metrics.
    pub fn backpressure_metrics(&self) -> Option<MatchGrpcBackpressureMetrics> {
        match self {
            Self::ExecutionFailed { backpressure, .. } => Some(*backpressure),
            _ => None,
        }
    }
}

pub fn character_id_from_credentials<'a>(
    credentials: Option<&'a (String, String)>,
) -> Result<&'a str, MatchGrpcError> {
    let character_id = credentials
        .map(|(_, character_id)| character_id.as_str())
        .filter(|character_id| !character_id.is_empty())
        .ok_or_else(|| MatchGrpcError::Business("character_id_missing".into()))?;
    Ok(character_id)
}

impl std::fmt::Display for MatchGrpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

pub async fn execute_live_match_steps(
    descriptor: &ServiceDescriptor,
    environment: EnvironmentKind,
    live_grpc: bool,
    character_id: &str,
    steps: &[PlannedSideServiceStep],
    timeout_ms: u64,
    mut admit: impl FnMut(bool) -> Result<(), MatchGrpcError>,
) -> Result<MatchGrpcMetrics, MatchGrpcError> {
    if !matches!(environment, EnvironmentKind::Local | EnvironmentKind::Test) {
        return Err(MatchGrpcError::LiveTransportForbidden);
    }
    if !live_grpc {
        return Err(MatchGrpcError::LiveTransportNotEnabled);
    }
    if descriptor.protocol != crate::side_services::SideTransportKind::Grpc {
        return Err(MatchGrpcError::DescriptorRejected);
    }
    if steps
        .iter()
        .any(|step| step.service != SideServiceKind::Match)
    {
        return Err(MatchGrpcError::InvalidPlan);
    }
    admit(true)?;
    let endpoint = format!("http://{}:{}", descriptor.host, descriptor.port);
    let mut client = MatchServiceClient::connect(endpoint)
        .await
        .map_err(|error| MatchGrpcError::Grpc(error.to_string()))?;
    let started_at = Instant::now();
    let mut metrics = MatchGrpcMetrics::default();
    let mut pending = MatchGrpcPending::new(DEFAULT_MAX_MATCH_GRPC_PENDING)?;
    let mut match_id = String::new();
    let execution: Result<(), MatchGrpcError> = async {
        for step in steps {
            if step.think_time_ms > 0 {
                tokio::time::timeout(
                    Duration::from_millis(timeout_ms.max(1)),
                    tokio::time::sleep(Duration::from_millis(step.think_time_ms)),
                )
                .await
                .map_err(|_| MatchGrpcError::Timeout)?;
            }
            admit(false)?;
            match step.operation {
                SideServiceOperation::MatchStart => {
                    metrics.match_attempts = metrics.match_attempts.saturating_add(1);
                    let response = bounded_pending(
                        &mut pending,
                        timeout_ms,
                        client.match_start(MatchStartReq {
                            character_id: character_id.into(),
                            mode: "1v1".into(),
                            rank_tier: 0,
                        }),
                    )
                    .await?
                    .into_inner();
                    if !response.ok {
                        return Err(MatchGrpcError::Business(response.error_code));
                    }
                    match_id = response.match_id;
                    metrics.queued_ms = started_at.elapsed().as_millis() as u64;
                }
                SideServiceOperation::MatchEventStream => {
                    metrics.event_stream_connections += 1;
                    let response = bounded_pending(
                        &mut pending,
                        timeout_ms,
                        client.match_event_stream(MatchEventStreamReq {
                            character_id: character_id.into(),
                        }),
                    )
                    .await?;
                    let mut stream = response.into_inner();
                    loop {
                        let event = match tokio::time::timeout(
                            Duration::from_millis(timeout_ms.max(1)),
                            stream.next(),
                        )
                        .await
                        {
                            Ok(event) => event,
                            Err(_) => {
                                pending.disconnect();
                                return Err(MatchGrpcError::Timeout);
                            }
                        };
                        let Some(event) = event else {
                            metrics.stream_disconnects += 1;
                            pending.disconnect();
                            return Err(MatchGrpcError::StreamDisconnected);
                        };
                        let event = match event {
                            Ok(event) => event,
                            Err(status) => {
                                pending.disconnect();
                                return Err(MatchGrpcError::Grpc(status.to_string()));
                            }
                        };
                        if event.event == "matched" {
                            metrics.match_successes += 1;
                            break;
                        }
                        if event.event == "match_cancelled" {
                            metrics.cancellations += 1;
                            return Err(terminal_event_error(&event));
                        }
                        if event.event == "match_failed" {
                            if event.error_code == "MATCH_TIMEOUT" {
                                metrics.timeouts += 1;
                                return Err(MatchGrpcError::Timeout);
                            }
                            return Err(terminal_event_error(&event));
                        }
                    }
                }
                SideServiceOperation::MatchStatus => {
                    let response = bounded_pending(
                        &mut pending,
                        timeout_ms,
                        client.match_status(MatchStatusReq {
                            character_id: character_id.into(),
                        }),
                    )
                    .await?
                    .into_inner();
                    validate_match_status(&response.status)?;
                    metrics.grpc_statuses += 1;
                }
                SideServiceOperation::MatchCancel => {
                    let response = bounded_pending(
                        &mut pending,
                        timeout_ms,
                        client.match_cancel(MatchCancelReq {
                            character_id: character_id.into(),
                            match_id: match_id.clone(),
                        }),
                    )
                    .await?
                    .into_inner();
                    if !response.ok {
                        return Err(MatchGrpcError::Business(response.error_code));
                    }
                    metrics.cancellations += 1;
                }
                _ => return Err(MatchGrpcError::InvalidPlan),
            }
        }
        Ok(())
    }
    .await;
    metrics.backpressure = pending.metrics();
    let backpressure = metrics.backpressure;
    match execution {
        Ok(()) => Ok(metrics),
        Err(source) => Err(MatchGrpcError::ExecutionFailed {
            source: Box::new(source),
            backpressure,
        }),
    }
}

/// Runs the bounded, local/test-only MatchInternal lifecycle used to diagnose
/// the game-server callback contract. It intentionally accepts at most two
/// authenticated role identities and never invents room-create timing.
pub async fn execute_live_match_internal_steps(
    descriptor: &ServiceDescriptor,
    environment: EnvironmentKind,
    live_internal: bool,
    roles: &[String],
    match_id: &str,
    room_id: &str,
    timeout_ms: u64,
    mut admit: impl FnMut(MatchInternalAdmission) -> Result<(), MatchGrpcError>,
) -> Result<MatchInternalMetrics, MatchGrpcError> {
    if !matches!(environment, EnvironmentKind::Local | EnvironmentKind::Test) {
        return Err(MatchGrpcError::LiveTransportForbidden);
    }
    if !live_internal {
        return Err(MatchGrpcError::LiveTransportNotEnabled);
    }
    if descriptor.protocol != crate::side_services::SideTransportKind::Grpc {
        return Err(MatchGrpcError::DescriptorRejected);
    }
    if roles.is_empty() || roles.len() > 2 || roles.iter().any(|role| role.is_empty()) {
        return Err(MatchGrpcError::Business(
            "match_internal_roles_invalid".into(),
        ));
    }
    let mut unique_roles = std::collections::BTreeSet::new();
    if roles.iter().any(|role| !unique_roles.insert(role)) {
        return Err(MatchGrpcError::Business(
            "match_internal_duplicate_role".into(),
        ));
    }
    if match_id.is_empty() || room_id.is_empty() {
        return Err(MatchGrpcError::Business(
            "match_internal_room_identity_missing".into(),
        ));
    }

    admit(MatchInternalAdmission::Connection)?;
    let mut metrics = MatchInternalMetrics {
        connections: 1,
        ..Default::default()
    };
    let started_at = Instant::now();
    let endpoint = format!("http://{}:{}", descriptor.host, descriptor.port);
    let channel = tokio::time::timeout(
        Duration::from_millis(timeout_ms.max(1)),
        Endpoint::from_shared(endpoint)
            .map_err(|error| MatchGrpcError::Grpc(error.to_string()))?
            .connect(),
    )
    .await
    .map_err(|_| MatchGrpcError::Timeout)?
    .map_err(|error| MatchGrpcError::Grpc(error.to_string()))?;
    let mut internal = MatchInternalClient::new(channel.clone());
    let mut service = MatchServiceClient::new(channel);

    let mut admit_message = |writes| -> Result<(), MatchGrpcError> {
        admit(MatchInternalAdmission::Message { writes })?;
        metrics.messages = metrics.messages.saturating_add(1);
        metrics.operations = metrics.operations.saturating_add(1);
        if writes {
            metrics.writes = metrics.writes.saturating_add(1);
        }
        Ok(())
    };
    let mut record_error = |error: &MatchGrpcError| match error {
        MatchGrpcError::Timeout => metrics.timeouts = metrics.timeouts.saturating_add(1),
        MatchGrpcError::Grpc(_) | MatchGrpcError::StreamDisconnected => {
            metrics.grpc_errors = metrics.grpc_errors.saturating_add(1)
        }
        MatchGrpcError::Business(_) => {
            metrics.business_errors = metrics.business_errors.saturating_add(1)
        }
        _ => {}
    };

    admit_message(true)?;
    let response = match bounded(
        timeout_ms,
        internal.create_room_and_join(CreateRoomAndJoinReq {
            match_id: match_id.into(),
            room_id: room_id.into(),
            character_ids: roles.to_vec(),
            mode: "1v1".into(),
        }),
    )
    .await
    {
        Ok(response) => response.into_inner(),
        Err(error) => {
            record_error(&error);
            return Err(error);
        }
    };
    if !response.ok {
        let error = MatchGrpcError::Business(if response.error_code.is_empty() {
            "match_internal_create_room_failed".into()
        } else {
            response.error_code
        });
        record_error(&error);
        return Err(error);
    }
    metrics.successes = metrics.successes.saturating_add(1);

    for role in roles {
        admit_message(true)?;
        let response = match bounded(
            timeout_ms,
            internal.player_joined(PlayerJoinedReq {
                match_id: match_id.into(),
                character_id: role.clone(),
                room_id: room_id.into(),
            }),
        )
        .await
        {
            Ok(response) => response.into_inner(),
            Err(error) => {
                record_error(&error);
                return Err(error);
            }
        };
        if !response.ok {
            let error = MatchGrpcError::Business(if response.error_code.is_empty() {
                "match_internal_player_joined_failed".into()
            } else {
                response.error_code
            });
            record_error(&error);
            return Err(error);
        }
        metrics.successes = metrics.successes.saturating_add(1);
    }

    admit_message(false)?;
    let status = match bounded(
        timeout_ms,
        service.match_status(MatchStatusReq {
            character_id: roles[0].clone(),
        }),
    )
    .await
    {
        Ok(response) => response.into_inner(),
        Err(error) => {
            record_error(&error);
            return Err(error);
        }
    };
    metrics.statuses = metrics.statuses.saturating_add(1);
    if status.status != "in_room" {
        let error = MatchGrpcError::Business("match_internal_status_not_in_room".into());
        record_error(&error);
        return Err(error);
    }

    for role in roles {
        admit_message(true)?;
        let response = match bounded(
            timeout_ms,
            internal.player_left(PlayerLeftReq {
                match_id: match_id.into(),
                character_id: role.clone(),
                reason: "normal".into(),
            }),
        )
        .await
        {
            Ok(response) => response.into_inner(),
            Err(error) => {
                record_error(&error);
                return Err(error);
            }
        };
        if !response.ok {
            let error = MatchGrpcError::Business(if response.error_code.is_empty() {
                "match_internal_player_left_failed".into()
            } else {
                response.error_code
            });
            record_error(&error);
            return Err(error);
        }
        metrics.successes = metrics.successes.saturating_add(1);
    }

    admit_message(true)?;
    let response = match bounded(
        timeout_ms,
        internal.match_end(MatchEndReq {
            match_id: match_id.into(),
            room_id: room_id.into(),
            reason: "game_over".into(),
        }),
    )
    .await
    {
        Ok(response) => response.into_inner(),
        Err(error) => {
            record_error(&error);
            return Err(error);
        }
    };
    if !response.ok {
        let error = MatchGrpcError::Business(if response.error_code.is_empty() {
            "match_internal_end_failed".into()
        } else {
            response.error_code
        });
        record_error(&error);
        return Err(error);
    }
    metrics.successes = metrics.successes.saturating_add(1);

    admit_message(false)?;
    let status = match bounded(
        timeout_ms,
        service.match_status(MatchStatusReq {
            character_id: roles[0].clone(),
        }),
    )
    .await
    {
        Ok(response) => response.into_inner(),
        Err(error) => {
            record_error(&error);
            return Err(error);
        }
    };
    metrics.statuses = metrics.statuses.saturating_add(1);
    if status.status != "idle" {
        let error = MatchGrpcError::Business("match_internal_status_not_idle".into());
        record_error(&error);
        return Err(error);
    }
    metrics.successes = metrics.successes.saturating_add(1);
    metrics.latency_ms = started_at.elapsed().as_millis() as u64;
    Ok(metrics)
}

fn validate_match_status(status: &str) -> Result<(), MatchGrpcError> {
    match status {
        "idle" | "matching" | "matched" | "in_room" => Ok(()),
        value if value.is_empty() => Err(MatchGrpcError::Business("match_status_empty".into())),
        value => Err(MatchGrpcError::Business(format!("match_status:{value}"))),
    }
}

fn terminal_event_error(event: &MatchEvent) -> MatchGrpcError {
    let code = if event.error_code.is_empty() {
        event.event.as_str()
    } else {
        event.error_code.as_str()
    };
    MatchGrpcError::Business(code.to_string())
}

async fn bounded<T>(
    timeout_ms: u64,
    future: impl std::future::Future<Output = Result<T, tonic::Status>>,
) -> Result<T, MatchGrpcError> {
    tokio::time::timeout(Duration::from_millis(timeout_ms.max(1)), future)
        .await
        .map_err(|_| MatchGrpcError::Timeout)?
        .map_err(|status| MatchGrpcError::Grpc(status.to_string()))
}

async fn bounded_pending<T>(
    pending: &mut MatchGrpcPending,
    timeout_ms: u64,
    future: impl std::future::Future<Output = Result<T, tonic::Status>>,
) -> Result<T, MatchGrpcError> {
    pending.begin()?;
    match bounded(timeout_ms, future).await {
        Ok(response) => {
            pending.complete()?;
            Ok(response)
        }
        Err(error) => {
            pending.disconnect();
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::side_services::SideTransportKind;

    fn descriptor(protocol: SideTransportKind) -> ServiceDescriptor {
        ServiceDescriptor {
            host: "127.0.0.1".into(),
            port: 9002,
            protocol,
        }
    }

    #[test]
    fn live_match_gate_rejects_remote_without_network() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let result = runtime.block_on(execute_live_match_steps(
            &descriptor(SideTransportKind::Grpc),
            EnvironmentKind::Production,
            true,
            "character",
            &[],
            1,
            |_| Ok(()),
        ));
        assert!(matches!(
            result,
            Err(MatchGrpcError::LiveTransportForbidden)
        ));
    }

    #[test]
    fn grpc_pending_contract_bounds_slow_consumers_and_projects_low_cardinality_metrics() {
        let mut pending = MatchGrpcPending::new(1).unwrap();
        pending.begin().unwrap();
        assert!(matches!(
            pending.begin(),
            Err(MatchGrpcError::PendingLimit { maximum: 1 })
        ));
        pending.disconnect();
        assert_eq!(pending.pending(), 0);
        let metrics = pending.metrics();
        assert_eq!(metrics.pending_limit_rejections, 1);
        assert_eq!(metrics.dropped_pending_messages, 1);
        assert_eq!(metrics.stream_disconnects, 1);
        assert!(!metrics.is_healthy());
        let mut projected = crate::metrics::Metrics::default();
        metrics.merge_into_metrics(&mut projected);
        let snapshot = projected.snapshot();
        assert_eq!(
            snapshot.counters["match_grpc_backpressure_pending_limit_rejections"],
            1
        );
        assert_eq!(
            snapshot.counters["match_grpc_backpressure_dropped_pending_messages"],
            1
        );
        let mut health = crate::abort::ContinuousHealthObservation::healthy();
        metrics.apply_to_health(&mut health);
        assert!(!health.backpressure_healthy);
    }

    #[test]
    fn grpc_pending_contract_rejects_zero_limit_and_completion_underflow() {
        assert!(matches!(
            MatchGrpcPending::new(0),
            Err(MatchGrpcError::InvalidPendingLimit)
        ));
        let mut pending = MatchGrpcPending::new(1).unwrap();
        assert!(matches!(
            pending.complete(),
            Err(MatchGrpcError::PendingUnderflow)
        ));
    }

    #[test]
    fn match_metrics_projection_keeps_fixed_low_cardinality_keys() {
        let metrics = MatchGrpcMetrics {
            match_attempts: 1,
            match_successes: 1,
            event_stream_connections: 1,
            queued_ms: 23,
            cancellations: 1,
            timeouts: 1,
            stream_disconnects: 1,
            ..Default::default()
        };
        let mut projected = crate::metrics::Metrics::default();
        metrics.merge_into_metrics(&mut projected);
        let snapshot = projected.snapshot();
        assert_eq!(snapshot.counters["match_grpc_successes"], 1);
        assert_eq!(snapshot.counters["match_grpc_attempts"], 1);
        assert_eq!(snapshot.counters["match_grpc_event_stream_connections"], 1);
        assert_eq!(snapshot.counters["match_grpc_cancellations"], 1);
        assert_eq!(snapshot.counters["match_grpc_timeouts"], 1);
        assert_eq!(snapshot.counters["match_grpc_stream_disconnects"], 1);
        assert_eq!(
            snapshot.counters["match_grpc_room_create_closed_unobserved"],
            1
        );
        assert_eq!(snapshot.histograms["match_grpc_queue_ms"].count(), 1);
    }

    #[test]
    fn match_plan_rejects_non_match_operations_before_transport() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let result = runtime.block_on(execute_live_match_steps(
            &descriptor(SideTransportKind::Grpc),
            EnvironmentKind::Local,
            true,
            "character",
            &[PlannedSideServiceStep {
                service: SideServiceKind::Chat,
                operation: SideServiceOperation::ChatAuth,
                weight: 1,
                think_time_ms: 0,
            }],
            1,
            |_| Ok(()),
        ));
        assert!(matches!(result, Err(MatchGrpcError::InvalidPlan)));
    }

    #[test]
    fn cancelled_terminal_event_is_a_business_failure() {
        let error = terminal_event_error(&MatchEvent {
            event: "match_cancelled".into(),
            ..Default::default()
        });
        assert!(matches!(error, MatchGrpcError::Business(code) if code == "match_cancelled"));
    }

    #[test]
    fn failed_terminal_event_preserves_server_error_code() {
        let error = terminal_event_error(&MatchEvent {
            event: "match_failed".into(),
            error_code: "ROOM_CREATE_FAILED".into(),
            ..Default::default()
        });
        assert!(matches!(error, MatchGrpcError::Business(code) if code == "ROOM_CREATE_FAILED"));
    }

    #[test]
    fn match_status_rejects_unknown_business_state() {
        let error = validate_match_status("server_error").unwrap_err();
        assert!(
            matches!(error, MatchGrpcError::Business(code) if code == "match_status:server_error")
        );
    }

    #[test]
    fn side_credentials_preserve_character_identity_for_match_runner() {
        let credentials = Some(("ticket-only-in-memory".into(), "character-real-42".into()));
        assert_eq!(
            character_id_from_credentials(credentials.as_ref()).unwrap(),
            "character-real-42"
        );
        assert!(matches!(
            character_id_from_credentials(None),
            Err(MatchGrpcError::Business(code)) if code == "character_id_missing"
        ));
    }

    #[test]
    fn match_internal_metrics_mark_room_create_timing_unobserved() {
        let metrics = MatchInternalMetrics {
            operations: 8,
            successes: 8,
            statuses: 2,
            connections: 1,
            messages: 8,
            writes: 6,
            ..Default::default()
        };
        let mut projected = crate::metrics::Metrics::default();
        metrics.merge_into_metrics(&mut projected);
        let snapshot = projected.snapshot();
        assert_eq!(snapshot.counters["match_internal_operations"], 8);
        assert_eq!(snapshot.counters["match_internal_writes"], 6);
        assert_eq!(
            snapshot.counters["match_internal_room_create_closed_unobserved"],
            1
        );
        assert_eq!(snapshot.histograms["match_internal_ms"].count(), 1);
    }

    #[test]
    fn match_internal_gate_rejects_remote_and_invalid_roles_without_network() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let roles = vec!["character-a".into(), "character-b".into()];
        let result = runtime.block_on(execute_live_match_internal_steps(
            &descriptor(SideTransportKind::Grpc),
            EnvironmentKind::Production,
            true,
            &roles,
            "match-id",
            "room-id",
            1,
            |_| Ok(()),
        ));
        assert!(matches!(
            result,
            Err(MatchGrpcError::LiveTransportForbidden)
        ));

        let result = runtime.block_on(execute_live_match_internal_steps(
            &descriptor(SideTransportKind::Grpc),
            EnvironmentKind::Local,
            true,
            &[],
            "match-id",
            "room-id",
            1,
            |_| panic!("invalid roles must fail before admission"),
        ));
        assert!(matches!(
            result,
            Err(MatchGrpcError::Business(code)) if code == "match_internal_roles_invalid"
        ));
    }

    #[test]
    fn match_internal_admission_plan_supports_two_roles_and_write_budget() {
        let plan = match_internal_admission_plan(2).unwrap();
        assert_eq!(plan.len(), 9);
        assert_eq!(plan[0], MatchInternalAdmission::Connection);
        assert_eq!(
            plan.iter()
                .filter(|admission| {
                    matches!(admission, MatchInternalAdmission::Message { writes: true })
                })
                .count(),
            6
        );
        assert!(matches!(
            match_internal_admission_plan(3),
            Err(MatchGrpcError::Business(code)) if code == "match_internal_roles_invalid"
        ));
    }
}
