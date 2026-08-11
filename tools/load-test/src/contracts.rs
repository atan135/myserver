use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::metrics::{HistogramSnapshot, MetricsSnapshot};
use crate::{SCHEMA_VERSION, config::HardBudget};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunPlan {
    pub schema_version: u32,
    pub run_id: String,
    pub environment: String,
    pub scenario_name: String,
    pub budget: HardBudget,
    pub planned_start_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerAssignment {
    pub schema_version: u32,
    pub run_id: String,
    pub worker_id: String,
    pub virtual_player_start: u32,
    pub virtual_player_count: u32,
    pub lease_expires_unix_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricBatch {
    pub schema_version: u32,
    pub run_id: String,
    pub worker_id: String,
    pub sequence: u64,
    pub window_start_monotonic_ms: u64,
    pub window_end_monotonic_ms: u64,
    pub counters: BTreeMap<String, u64>,
    pub histograms: BTreeMap<String, HistogramSnapshot>,
    pub checksum: String,
}

impl MetricBatch {
    pub fn new(
        run_id: impl Into<String>,
        worker_id: impl Into<String>,
        sequence: u64,
        window_start_monotonic_ms: u64,
        window_end_monotonic_ms: u64,
        metrics: MetricsSnapshot,
    ) -> Self {
        let run_id = run_id.into();
        let worker_id = worker_id.into();
        let checksum = checksum_parts(&[
            &run_id,
            &worker_id,
            &sequence.to_string(),
            &window_start_monotonic_ms.to_string(),
            &window_end_monotonic_ms.to_string(),
            &serde_json::to_string(&metrics).expect("metrics must serialize"),
        ]);
        Self {
            schema_version: SCHEMA_VERSION,
            run_id,
            worker_id,
            sequence,
            window_start_monotonic_ms,
            window_end_monotonic_ms,
            counters: metrics.counters,
            histograms: metrics.histograms,
            checksum,
        }
    }

    pub fn checksum_is_valid(&self) -> bool {
        let metrics = MetricsSnapshot {
            counters: self.counters.clone(),
            histograms: self.histograms.clone(),
        };
        self.checksum
            == checksum_parts(&[
                &self.run_id,
                &self.worker_id,
                &self.sequence.to_string(),
                &self.window_start_monotonic_ms.to_string(),
                &self.window_end_monotonic_ms.to_string(),
                &serde_json::to_string(&metrics).expect("metrics must serialize"),
            ])
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerHeartbeat {
    pub schema_version: u32,
    pub run_id: String,
    pub worker_id: String,
    pub sequence: u64,
    pub monotonic_ms: u64,
    pub wall_clock_unix_ms: u64,
    pub active_virtual_players: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AbortSignal {
    pub schema_version: u32,
    pub run_id: String,
    pub reason: String,
    pub issued_unix_ms: u64,
    pub graceful_shutdown_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunSummary {
    pub schema_version: u32,
    pub run_id: String,
    pub status: String,
    pub complete: bool,
    pub abort_reason: Option<String>,
    pub metrics: MetricsSnapshot,
    pub missing_workers: Vec<String>,
}

pub fn single_process_assignment(
    plan: &RunPlan,
    players: u32,
    now_unix_ms: u64,
) -> WorkerAssignment {
    WorkerAssignment {
        schema_version: SCHEMA_VERSION,
        run_id: plan.run_id.clone(),
        worker_id: "local-worker-0".to_string(),
        virtual_player_start: 0,
        virtual_player_count: players,
        lease_expires_unix_ms: now_unix_ms.saturating_add(plan.budget.max_duration_secs * 1000),
    }
}

fn checksum_parts(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::Metrics;

    #[test]
    fn metric_batch_is_serializable_and_checksum_protects_the_window() {
        let mut metrics = Metrics::default();
        metrics.increment("requests", 2);
        let batch = MetricBatch::new("r1", "w1", 2, 10, 20, metrics.snapshot());
        assert!(batch.checksum_is_valid());
        let mut changed = batch.clone();
        changed.sequence += 1;
        assert!(!changed.checksum_is_valid());
        assert_eq!(
            serde_json::from_str::<MetricBatch>(&serde_json::to_string(&batch).unwrap()).unwrap(),
            batch
        );
    }
}
