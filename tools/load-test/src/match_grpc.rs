use std::time::{Duration, Instant};

use crate::config::EnvironmentKind;
use crate::match_pb::{
    MatchCancelReq, MatchEvent, MatchEventStreamReq, MatchStartReq, MatchStatusReq,
    match_service_client::MatchServiceClient,
};
use crate::side_services::{
    PlannedSideServiceStep, ServiceDescriptor, SideServiceKind, SideServiceOperation,
};
use futures_util::StreamExt;

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
    pub match_successes: u64,
    pub event_stream_connections: u64,
    pub grpc_statuses: u64,
    pub room_create_closed_ms: u64,
    pub cancellations: u64,
    pub timeouts: u64,
    pub stream_disconnects: u64,
    pub outcomes: std::collections::BTreeMap<MatchGrpcOutcome, u64>,
}

impl MatchGrpcMetrics {
    pub fn merge_into_metrics(&self, metrics: &mut crate::metrics::Metrics) {
        for (key, value) in [
            ("match_grpc_queued_ms", self.queued_ms),
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
        ] {
            metrics.increment(key, value);
        }
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
    RoomCreateClosed,
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
    let mut match_id = String::new();
    for step in steps {
        admit(false)?;
        match step.operation {
            SideServiceOperation::MatchStart => {
                let response = bounded(
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
                let response = bounded(
                    timeout_ms,
                    client.match_event_stream(MatchEventStreamReq {
                        character_id: character_id.into(),
                    }),
                )
                .await?;
                let mut stream = response.into_inner();
                loop {
                    let event = tokio::time::timeout(
                        Duration::from_millis(timeout_ms.max(1)),
                        stream.next(),
                    )
                    .await
                    .map_err(|_| MatchGrpcError::Timeout)?;
                    let Some(event) = event else {
                        metrics.stream_disconnects += 1;
                        return Err(MatchGrpcError::StreamDisconnected);
                    };
                    let event = event.map_err(|status| MatchGrpcError::Grpc(status.to_string()))?;
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
                let response = bounded(
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
                let response = bounded(
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
    fn match_metrics_projection_keeps_fixed_low_cardinality_keys() {
        let metrics = MatchGrpcMetrics {
            match_successes: 1,
            event_stream_connections: 1,
            cancellations: 1,
            timeouts: 1,
            stream_disconnects: 1,
            ..Default::default()
        };
        let mut projected = crate::metrics::Metrics::default();
        metrics.merge_into_metrics(&mut projected);
        let snapshot = projected.snapshot();
        assert_eq!(snapshot.counters["match_grpc_successes"], 1);
        assert_eq!(snapshot.counters["match_grpc_event_stream_connections"], 1);
        assert_eq!(snapshot.counters["match_grpc_cancellations"], 1);
        assert_eq!(snapshot.counters["match_grpc_timeouts"], 1);
        assert_eq!(snapshot.counters["match_grpc_stream_disconnects"], 1);
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
}
