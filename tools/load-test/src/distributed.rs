use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::contracts::{AbortSignal, MetricBatch, RunPlan, RunSummary, WorkerAssignment};
use crate::metrics::MetricsSnapshot;

pub const DISTRIBUTED_SCHEMA_VERSION: u32 = 1;
pub const MIN_DISTRIBUTED_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DistributedError {
    UnsupportedSchema(u32),
    InvalidPlan(String),
    InvalidAssignment(String),
    InvalidBatch(String),
    PendingBatchLimit,
    CredentialRejected(String),
    ControllerDisconnected,
}

impl std::fmt::Display for DistributedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedSchema(version) => {
                write!(f, "unsupported distributed schema version {version}")
            }
            Self::InvalidPlan(reason) => write!(f, "invalid run plan: {reason}"),
            Self::InvalidAssignment(reason) => write!(f, "invalid worker assignment: {reason}"),
            Self::InvalidBatch(reason) => write!(f, "invalid metric batch: {reason}"),
            Self::PendingBatchLimit => write!(f, "worker pending metric batch limit exceeded"),
            Self::CredentialRejected(reason) => write!(f, "worker credential rejected: {reason}"),
            Self::ControllerDisconnected => {
                write!(f, "controller connection lost; worker stopped fail-closed")
            }
        }
    }
}

pub fn validate_schema_version(version: u32) -> Result<(), DistributedError> {
    if !(MIN_DISTRIBUTED_SCHEMA_VERSION..=DISTRIBUTED_SCHEMA_VERSION).contains(&version) {
        return Err(DistributedError::UnsupportedSchema(version));
    }
    Ok(())
}

pub fn validate_run_plan(plan: &RunPlan) -> Result<(), DistributedError> {
    validate_schema_version(plan.schema_version)?;
    if plan.run_id.trim().is_empty()
        || plan.environment.trim().is_empty()
        || plan.scenario_name.trim().is_empty()
    {
        return Err(DistributedError::InvalidPlan(
            "run_id, environment and scenario are required".into(),
        ));
    }
    plan.budget
        .validate()
        .map_err(|error| DistributedError::InvalidPlan(error.to_string()))
}

pub fn validate_assignment(
    assignment: &WorkerAssignment,
    plan: &RunPlan,
) -> Result<(), DistributedError> {
    validate_schema_version(assignment.schema_version)?;
    if assignment.run_id != plan.run_id
        || assignment.worker_id.trim().is_empty()
        || assignment.virtual_player_count == 0
    {
        return Err(DistributedError::InvalidAssignment(
            "assignment identity or player count is invalid".into(),
        ));
    }
    let end = assignment
        .virtual_player_start
        .checked_add(assignment.virtual_player_count)
        .ok_or_else(|| DistributedError::InvalidAssignment("player range overflowed".into()))?;
    if end > plan.budget.max_virtual_players {
        return Err(DistributedError::InvalidAssignment(
            "assignment exceeds global player budget".into(),
        ));
    }
    Ok(())
}

pub fn slice_plan(
    plan: &RunPlan,
    worker_count: u32,
    now_unix_ms: u64,
) -> Result<Vec<WorkerAssignment>, DistributedError> {
    validate_run_plan(plan)?;
    if worker_count == 0 {
        return Err(DistributedError::InvalidPlan(
            "worker_count must be positive".into(),
        ));
    }
    let workers = worker_count.min(plan.budget.max_virtual_players.max(1));
    let base = plan.budget.max_virtual_players / workers;
    let remainder = plan.budget.max_virtual_players % workers;
    let mut start = 0;
    let mut assignments = Vec::new();
    for index in 0..workers {
        let count = base + if index < remainder { 1 } else { 0 };
        if count == 0 {
            continue;
        }
        let assignment = WorkerAssignment {
            schema_version: DISTRIBUTED_SCHEMA_VERSION,
            run_id: plan.run_id.clone(),
            worker_id: format!("worker-{index:04}"),
            virtual_player_start: start,
            virtual_player_count: count,
            lease_expires_unix_ms: now_unix_ms
                .saturating_add(plan.budget.max_duration_secs.saturating_mul(1000)),
        };
        validate_assignment(&assignment, plan)?;
        assignments.push(assignment);
        start = start.saturating_add(count);
    }
    Ok(assignments)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogicalAccountRef {
    pub logical_account_id: String,
    pub batch: String,
}

pub fn shard_account_refs(
    accounts: &[LogicalAccountRef],
    assignments: &[WorkerAssignment],
) -> Result<BTreeMap<String, Vec<LogicalAccountRef>>, DistributedError> {
    if assignments.is_empty() {
        return Err(DistributedError::InvalidAssignment(
            "at least one worker assignment is required".into(),
        ));
    }
    let mut result = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for (worker_index, assignment) in assignments.iter().enumerate() {
        validate_schema_version(assignment.schema_version)?;
        let refs = accounts
            .iter()
            .enumerate()
            .filter(|(index, _)| *index % assignments.len() == worker_index)
            .map(|(_, account)| account.clone())
            .collect::<Vec<_>>();
        if refs.len() as u32 > assignment.virtual_player_count {
            return Err(DistributedError::InvalidAssignment(
                "worker account shard exceeds assigned player count".into(),
            ));
        }
        for account in &refs {
            if account.logical_account_id.trim().is_empty()
                || !seen.insert(account.logical_account_id.clone())
            {
                return Err(DistributedError::InvalidAssignment(
                    "account refs overlap or are empty".into(),
                ));
            }
        }
        result.insert(assignment.worker_id.clone(), refs);
    }
    Ok(result)
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct MonotonicClockMapping {
    pub controller_monotonic_ms: u64,
    pub controller_wall_unix_ms: u64,
    pub worker_monotonic_ms: u64,
    pub worker_wall_unix_ms: u64,
}

impl MonotonicClockMapping {
    pub fn controller_wall_for_worker(&self, worker_monotonic_ms: u64) -> u64 {
        self.controller_wall_unix_ms
            .saturating_add(worker_monotonic_ms.saturating_sub(self.worker_monotonic_ms))
    }

    pub fn scheduler_lag_ms(&self, planned_worker_ms: u64, actual_worker_ms: u64) -> u64 {
        actual_worker_ms.saturating_sub(planned_worker_ms)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchDisposition {
    Accepted,
    Duplicate,
    OutOfOrder,
    MissingGap,
}

#[derive(Debug, Default)]
pub struct MetricBatchLedger {
    seen: BTreeMap<String, BTreeSet<u64>>,
    next_expected: BTreeMap<String, u64>,
    missing: BTreeMap<String, BTreeSet<u64>>,
}

impl MetricBatchLedger {
    pub fn ingest(
        &mut self,
        batch: &MetricBatch,
        plan: &RunPlan,
    ) -> Result<BatchDisposition, DistributedError> {
        validate_schema_version(batch.schema_version)?;
        if batch.run_id != plan.run_id
            || batch.window_start_monotonic_ms >= batch.window_end_monotonic_ms
            || !batch.checksum_is_valid()
        {
            return Err(DistributedError::InvalidBatch(
                "identity, time window or checksum invalid".into(),
            ));
        }
        let seen = self.seen.entry(batch.worker_id.clone()).or_default();
        if !seen.insert(batch.sequence) {
            return Ok(BatchDisposition::Duplicate);
        }
        let expected = self
            .next_expected
            .entry(batch.worker_id.clone())
            .or_insert(0);
        let disposition = if batch.sequence == *expected {
            *expected = expected.saturating_add(1);
            BatchDisposition::Accepted
        } else if batch.sequence > *expected {
            for sequence in *expected..batch.sequence {
                self.missing
                    .entry(batch.worker_id.clone())
                    .or_default()
                    .insert(sequence);
            }
            *expected = batch.sequence.saturating_add(1);
            BatchDisposition::MissingGap
        } else {
            BatchDisposition::OutOfOrder
        };
        Ok(disposition)
    }

    pub fn missing_sequences(&self, worker_id: &str) -> Vec<u64> {
        self.missing
            .get(worker_id)
            .map(|values| values.iter().copied().collect())
            .unwrap_or_default()
    }
}

/// Controller-side merge boundary for worker metric batches.
///
/// Workers send mergeable HDR payloads and raw counters/time-series samples.
/// The controller aligns batches by sequence and requires every worker to use
/// the same monotonic window for that sequence. Percentiles are intentionally
/// absent from the wire contract and can only be calculated from the merged
/// HDR distributions after ingestion.
#[derive(Debug, Default)]
pub struct DistributedMetricsAggregator {
    ledger: MetricBatchLedger,
    windows: BTreeMap<u64, (u64, u64)>,
    snapshot: MetricsSnapshot,
}

impl DistributedMetricsAggregator {
    pub fn ingest(
        &mut self,
        batch: &MetricBatch,
        plan: &RunPlan,
    ) -> Result<BatchDisposition, DistributedError> {
        validate_series_boundaries(batch)?;
        if let Some((start, end)) = self.windows.get(&batch.sequence)
            && (*start != batch.window_start_monotonic_ms || *end != batch.window_end_monotonic_ms)
        {
            return Err(DistributedError::InvalidBatch(
                "workers used different metric boundaries for the same sequence".into(),
            ));
        }
        let disposition = self.ledger.ingest(batch, plan)?;
        if disposition != BatchDisposition::Duplicate {
            self.windows.entry(batch.sequence).or_insert((
                batch.window_start_monotonic_ms,
                batch.window_end_monotonic_ms,
            ));
            self.snapshot.merge(&MetricsSnapshot {
                counters: batch.counters.clone(),
                histograms: batch.histograms.clone(),
                time_series: batch.time_series.clone(),
            });
        }
        Ok(disposition)
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        self.snapshot.clone()
    }

    pub fn missing_sequences(&self, worker_id: &str) -> Vec<u64> {
        self.ledger.missing_sequences(worker_id)
    }
}

fn validate_series_boundaries(batch: &MetricBatch) -> Result<(), DistributedError> {
    for (boundary_ms, samples) in &batch.time_series {
        if *boundary_ms < batch.window_start_monotonic_ms
            || *boundary_ms > batch.window_end_monotonic_ms
        {
            return Err(DistributedError::InvalidBatch(
                "time-series sample falls outside its metric window".into(),
            ));
        }
        for key in samples {
            if !crate::metrics::is_low_cardinality_key(key.0) {
                return Err(DistributedError::InvalidBatch(
                    "time-series key is not low-cardinality".into(),
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct PendingBatchStore {
    max_batches: usize,
    pending: VecDeque<MetricBatch>,
}

impl PendingBatchStore {
    pub fn new(max_batches: usize) -> Self {
        Self {
            max_batches: max_batches.max(1),
            pending: VecDeque::new(),
        }
    }
    pub fn push(&mut self, batch: MetricBatch) -> Result<(), DistributedError> {
        if self.pending.len() >= self.max_batches {
            return Err(DistributedError::PendingBatchLimit);
        }
        self.pending.push_back(batch);
        Ok(())
    }
    pub fn drain(&mut self) -> Vec<MetricBatch> {
        self.pending.drain(..).collect()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum CertificateState {
    Valid,
    Expired,
    NotYetValid,
    Revoked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerCredential {
    pub worker_id: String,
    pub certificate_fingerprint: String,
    pub certificate_state: CertificateState,
}

pub fn validate_worker_credential(
    credential: &WorkerCredential,
    expected_worker_id: &str,
) -> Result<(), DistributedError> {
    if credential.worker_id != expected_worker_id
        || credential.certificate_fingerprint.trim().is_empty()
        || credential.certificate_state != CertificateState::Valid
    {
        return Err(DistributedError::CredentialRejected(
            "worker identity or certificate state rejected".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerControlState {
    Running,
    Aborting,
    Completed,
    DisconnectedFailClosed,
}

#[derive(Debug, Default)]
pub struct AbortCoordinator {
    signal: Option<AbortSignal>,
    acknowledgements: BTreeSet<String>,
    workers: BTreeSet<String>,
}

impl AbortCoordinator {
    pub fn register(&mut self, worker_id: impl Into<String>) {
        self.workers.insert(worker_id.into());
    }
    pub fn issue(&mut self, signal: AbortSignal) {
        self.signal = Some(signal);
    }
    pub fn acknowledge(&mut self, worker_id: &str) {
        if self.workers.contains(worker_id) {
            self.acknowledgements.insert(worker_id.into());
        }
    }
    pub fn signal(&self) -> Option<&AbortSignal> {
        self.signal.as_ref()
    }
    pub fn missing_workers(&self) -> Vec<String> {
        self.workers
            .difference(&self.acknowledgements)
            .cloned()
            .collect()
    }
    pub fn summary(&self, run_id: impl Into<String>) -> RunSummary {
        RunSummary {
            schema_version: DISTRIBUTED_SCHEMA_VERSION,
            run_id: run_id.into(),
            status: if self.signal.is_some() {
                "aborted".into()
            } else {
                "running".into()
            },
            complete: self.signal.is_some() && self.missing_workers().is_empty(),
            abort_reason: self.signal.as_ref().map(|s| s.reason.clone()),
            metrics: Default::default(),
            missing_workers: self.missing_workers(),
        }
    }
}

pub fn worker_state_after_disconnect(state: WorkerControlState) -> WorkerControlState {
    match state {
        WorkerControlState::Running | WorkerControlState::Aborting => {
            WorkerControlState::DisconnectedFailClosed
        }
        other => other,
    }
}

pub fn checksum_for_secret_ref(reference: &str) -> String {
    format!("{:x}", Sha256::digest(reference.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HardBudget;
    use crate::contracts::{MetricBatch, RunPlan};
    use crate::metrics::Metrics;

    fn plan() -> RunPlan {
        RunPlan {
            schema_version: DISTRIBUTED_SCHEMA_VERSION,
            run_id: "r1".into(),
            environment: "local".into(),
            scenario_name: "s".into(),
            budget: HardBudget {
                max_virtual_players: 5,
                max_login_qps: 10.0,
                max_new_connections_per_second: 10.0,
                max_business_messages_per_second: 10.0,
                max_messages_per_connection_per_second: 10.0,
                max_duration_secs: 10,
                max_total_operations: 100,
                max_error_rate: 0.1,
                max_connection_failure_rate: 0.1,
                max_p99_ms: 1000,
                max_data_writes: 0,
            },
            planned_start_unix_ms: 1,
        }
    }

    #[test]
    fn plan_slices_and_account_refs_do_not_overlap() {
        let p = plan();
        let assignments = slice_plan(&p, 2, 10).unwrap();
        assert_eq!(
            assignments
                .iter()
                .map(|a| a.virtual_player_count)
                .sum::<u32>(),
            5
        );
        let refs = (0..5)
            .map(|index| LogicalAccountRef {
                logical_account_id: format!("a{index}"),
                batch: "b".into(),
            })
            .collect::<Vec<_>>();
        let shards = shard_account_refs(&refs, &assignments).unwrap();
        assert_eq!(shards.values().map(Vec::len).sum::<usize>(), 5);
    }

    #[test]
    fn batch_ledger_is_idempotent_and_detects_gaps() {
        let p = plan();
        let mut metrics = Metrics::default();
        metrics.increment("requests", 1);
        let first = MetricBatch::new("r1", "worker-0000", 0, 1, 2, metrics.snapshot());
        let second = MetricBatch::new("r1", "worker-0000", 2, 3, 4, metrics.snapshot());
        let mut ledger = MetricBatchLedger::default();
        assert_eq!(
            ledger.ingest(&first, &p).unwrap(),
            BatchDisposition::Accepted
        );
        assert_eq!(
            ledger.ingest(&first, &p).unwrap(),
            BatchDisposition::Duplicate
        );
        assert_eq!(
            ledger.ingest(&second, &p).unwrap(),
            BatchDisposition::MissingGap
        );
        assert_eq!(ledger.missing_sequences("worker-0000"), vec![1]);
    }

    #[test]
    fn distributed_aggregator_merges_raw_values_and_hdr_once() {
        let p = plan();
        let mut worker_a = Metrics::default();
        worker_a.increment("requests", 2);
        worker_a.observe_latency("operation_ms", 10);
        worker_a.record_time_series(100, "virtual_players", 3);
        let mut worker_b = Metrics::default();
        worker_b.increment("requests", 5);
        worker_b.observe_latency("operation_ms", 1_000);
        worker_b.record_time_series(100, "virtual_players", 4);
        let first = MetricBatch::new("r1", "worker-0000", 0, 0, 100, worker_a.snapshot());
        let second = MetricBatch::new("r1", "worker-0001", 0, 0, 100, worker_b.snapshot());
        let mut aggregator = DistributedMetricsAggregator::default();
        assert_eq!(
            aggregator.ingest(&first, &p).unwrap(),
            BatchDisposition::Accepted
        );
        assert_eq!(
            aggregator.ingest(&second, &p).unwrap(),
            BatchDisposition::Accepted
        );
        assert_eq!(
            aggregator.ingest(&second, &p).unwrap(),
            BatchDisposition::Duplicate
        );
        let snapshot = aggregator.snapshot();
        assert_eq!(snapshot.counters["requests"], 7);
        assert_eq!(snapshot.histograms["operation_ms"].count(), 2);
        assert_eq!(snapshot.histograms["operation_ms"].percentile(0.99), 1_000);
        assert_eq!(snapshot.time_series[&100]["virtual_players"], 7);
    }

    #[test]
    fn distributed_aggregator_rejects_boundary_drift_and_out_of_window_samples() {
        let p = plan();
        let mut first_metrics = Metrics::default();
        first_metrics.record_time_series(100, "virtual_players", 1);
        let first = MetricBatch::new("r1", "worker-0000", 0, 0, 100, first_metrics.snapshot());
        let mut drift_metrics = Metrics::default();
        drift_metrics.record_time_series(110, "virtual_players", 1);
        let drift = MetricBatch::new("r1", "worker-0001", 0, 10, 110, drift_metrics.snapshot());
        let mut aggregator = DistributedMetricsAggregator::default();
        aggregator.ingest(&first, &p).unwrap();
        assert!(matches!(
            aggregator.ingest(&drift, &p),
            Err(DistributedError::InvalidBatch(reason))
                if reason.contains("different metric boundaries")
        ));

        let mut outside_metrics = Metrics::default();
        outside_metrics.record_time_series(200, "virtual_players", 1);
        let outside =
            MetricBatch::new("r1", "worker-0002", 1, 100, 150, outside_metrics.snapshot());
        assert!(matches!(
            aggregator.ingest(&outside, &p),
            Err(DistributedError::InvalidBatch(reason))
                if reason.contains("outside its metric window")
        ));
    }

    #[test]
    fn credentials_and_abort_fail_closed() {
        let credential = WorkerCredential {
            worker_id: "w1".into(),
            certificate_fingerprint: "fp".into(),
            certificate_state: CertificateState::Valid,
        };
        validate_worker_credential(&credential, "w1").unwrap();
        assert!(
            validate_worker_credential(
                &WorkerCredential {
                    certificate_state: CertificateState::Expired,
                    ..credential.clone()
                },
                "w1"
            )
            .is_err()
        );
        assert_eq!(
            worker_state_after_disconnect(WorkerControlState::Running),
            WorkerControlState::DisconnectedFailClosed
        );
        let mut coordinator = AbortCoordinator::default();
        coordinator.register("w1");
        coordinator.register("w2");
        coordinator.issue(AbortSignal {
            schema_version: 1,
            run_id: "r1".into(),
            reason: "threshold".into(),
            issued_unix_ms: 1,
            graceful_shutdown_ms: 10,
        });
        coordinator.acknowledge("w1");
        assert_eq!(coordinator.missing_workers(), vec!["w2"]);
        assert!(!coordinator.summary("r1").complete);
    }

    #[test]
    fn bounded_pending_store_rejects_overflow() {
        let batch = MetricBatch::new("r1", "w1", 0, 1, 2, Metrics::default().snapshot());
        let mut store = PendingBatchStore::new(1);
        store.push(batch.clone()).unwrap();
        assert_eq!(
            store.push(batch).unwrap_err(),
            DistributedError::PendingBatchLimit
        );
    }

    #[test]
    fn schema_and_observation_validation_are_strict() {
        assert!(validate_schema_version(99).is_err());
        let snapshot = crate::control_plane::ObservationSnapshot {
            run_id: "".into(),
            window_start_unix_ms: 1,
            window_end_unix_ms: 2,
            source: "redis".into(),
            freshness_ms: 1,
            complete: true,
        };
        assert!(crate::control_plane::require_fresh_observation(&snapshot, 2, 10).is_err());
        assert_eq!(
            checksum_for_secret_ref("ref"),
            checksum_for_secret_ref("ref")
        );
    }
}
