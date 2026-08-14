//! Comparison of completed load-test reports.
//!
//! A baseline comparison is deliberately conservative.  Runs are comparable
//! only when their tool/scenario/environment/account-batch identities match
//! and both reports contain the metrics needed for the selected thresholds.
//! Missing observations produce an explicit non-comparable result rather than
//! a fabricated regression or improvement.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::metrics::MetricsSnapshot;
use crate::resource::{GeneratorResources, ResourceValue};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BaselineThresholds {
    /// Maximum allowed throughput decrease as a fraction of the baseline.
    pub max_throughput_drop: f64,
    /// Maximum allowed P95/P99 increase as a fraction of the baseline.
    pub max_latency_increase: f64,
    /// Maximum allowed absolute error-rate increase.
    pub max_error_rate_delta: f64,
    /// Maximum allowed working-set increase as a fraction of the baseline.
    pub max_resource_increase: f64,
}

impl Default for BaselineThresholds {
    fn default() -> Self {
        Self {
            max_throughput_drop: 0.10,
            max_latency_increase: 0.10,
            max_error_rate_delta: 0.01,
            max_resource_increase: 0.20,
        }
    }
}

impl BaselineThresholds {
    pub fn validate(&self) -> Result<(), BaselineError> {
        for (name, value) in [
            ("max_throughput_drop", self.max_throughput_drop),
            ("max_latency_increase", self.max_latency_increase),
            ("max_error_rate_delta", self.max_error_rate_delta),
            ("max_resource_increase", self.max_resource_increase),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(BaselineError::InvalidThreshold(name));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct RunMetadata {
    schema_version: u32,
    run_id: String,
    status: String,
    environment: String,
    scenario_hash: String,
    tool_git_commit: String,
    account_batch: String,
    started_unix_ms: u64,
    ended_unix_ms: u64,
    #[serde(default)]
    generator_resources: Option<GeneratorResources>,
    /// Populated by callers that enrich reports with service versions.  Older
    /// reports omit it and are considered non-comparable when a version is
    /// required by the caller.
    #[serde(default)]
    service_versions: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineSnapshot {
    pub metadata: BaselineMetadata,
    pub metrics: MetricsSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BaselineMetadata {
    pub schema_version: u32,
    pub run_id: String,
    pub status: String,
    pub environment: String,
    pub scenario_hash: String,
    pub tool_git_commit: String,
    pub account_batch: String,
    pub started_unix_ms: u64,
    pub ended_unix_ms: u64,
    pub service_versions: Option<BTreeMap<String, String>>,
    pub generator_resources: Option<GeneratorResources>,
}

impl BaselineSnapshot {
    pub fn load(report_dir: &Path) -> Result<Self, BaselineError> {
        let metadata: RunMetadata =
            serde_json::from_slice(&fs::read(report_dir.join("run.json"))?)?;
        let metrics = serde_json::from_slice(&fs::read(report_dir.join("metrics.json"))?)?;
        Ok(Self {
            metadata: BaselineMetadata {
                schema_version: metadata.schema_version,
                run_id: metadata.run_id,
                status: metadata.status,
                environment: metadata.environment,
                scenario_hash: metadata.scenario_hash,
                tool_git_commit: metadata.tool_git_commit,
                account_batch: metadata.account_batch,
                started_unix_ms: metadata.started_unix_ms,
                ended_unix_ms: metadata.ended_unix_ms,
                service_versions: metadata.service_versions,
                generator_resources: metadata.generator_resources,
            },
            metrics,
        })
    }

    pub fn from_parts(metadata: BaselineMetadata, metrics: MetricsSnapshot) -> Self {
        Self { metadata, metrics }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BaselineComparison {
    pub comparable: bool,
    pub regression: bool,
    pub reasons: Vec<String>,
    pub baseline_throughput_per_sec: Option<f64>,
    pub candidate_throughput_per_sec: Option<f64>,
    pub throughput_delta_ratio: Option<f64>,
    pub baseline_p95_ms: Option<u64>,
    pub candidate_p95_ms: Option<u64>,
    pub p95_delta_ratio: Option<f64>,
    pub baseline_p99_ms: Option<u64>,
    pub candidate_p99_ms: Option<u64>,
    pub p99_delta_ratio: Option<f64>,
    pub baseline_error_rate: Option<f64>,
    pub candidate_error_rate: Option<f64>,
    pub error_rate_delta: Option<f64>,
    pub working_set_delta_ratio: Option<f64>,
}

pub fn compare(
    baseline: &BaselineSnapshot,
    candidate: &BaselineSnapshot,
    thresholds: BaselineThresholds,
) -> Result<BaselineComparison, BaselineError> {
    thresholds.validate()?;
    let mut reasons = Vec::new();
    for (label, left, right) in [
        (
            "schema version",
            baseline.metadata.schema_version.to_string(),
            candidate.metadata.schema_version.to_string(),
        ),
        (
            "environment",
            baseline.metadata.environment.clone(),
            candidate.metadata.environment.clone(),
        ),
        (
            "scenario hash",
            baseline.metadata.scenario_hash.clone(),
            candidate.metadata.scenario_hash.clone(),
        ),
        (
            "tool commit",
            baseline.metadata.tool_git_commit.clone(),
            candidate.metadata.tool_git_commit.clone(),
        ),
        (
            "account batch",
            baseline.metadata.account_batch.clone(),
            candidate.metadata.account_batch.clone(),
        ),
    ] {
        if left != right {
            reasons.push(format!("{label} differs"));
        }
    }
    if baseline.metadata.status != "completed" || candidate.metadata.status != "completed" {
        reasons.push("both runs must have completed status".into());
    }
    if let (Some(expected), Some(actual)) = (
        &baseline.metadata.service_versions,
        &candidate.metadata.service_versions,
    ) {
        if expected != actual {
            reasons.push("service versions differ".into());
        }
    } else {
        reasons.push("service version observation is incomplete".into());
    }

    let baseline_throughput = throughput(baseline);
    let candidate_throughput = throughput(candidate);
    let throughput_delta = ratio(baseline_throughput, candidate_throughput);
    let baseline_p95 = latency(baseline, 0.95);
    let candidate_p95 = latency(candidate, 0.95);
    let p95_delta = ratio_u64(baseline_p95, candidate_p95);
    let baseline_p99 = latency(baseline, 0.99);
    let candidate_p99 = latency(candidate, 0.99);
    let p99_delta = ratio_u64(baseline_p99, candidate_p99);
    let baseline_errors = error_rate(baseline);
    let candidate_errors = error_rate(candidate);
    let error_delta = baseline_errors.zip(candidate_errors).map(|(a, b)| b - a);
    let working_set_delta = resource_delta(
        baseline.metadata.generator_resources.as_ref(),
        candidate.metadata.generator_resources.as_ref(),
    );
    for (name, present) in [
        ("throughput", throughput_delta.is_some()),
        ("P95 latency", p95_delta.is_some()),
        ("P99 latency", p99_delta.is_some()),
        ("error rate", error_delta.is_some()),
        ("working-set resource", working_set_delta.is_some()),
    ] {
        if !present {
            reasons.push(format!("{name} observation is incomplete"));
        }
    }
    let comparable = reasons.is_empty();
    let mut regression = false;
    if comparable {
        if throughput_delta.expect("validated") < -thresholds.max_throughput_drop {
            regression = true;
            reasons.push("throughput drop exceeded policy".into());
        }
        if p95_delta.expect("validated") > thresholds.max_latency_increase
            || p99_delta.expect("validated") > thresholds.max_latency_increase
        {
            regression = true;
            reasons.push("tail-latency increase exceeded policy".into());
        }
        if error_delta.expect("validated") > thresholds.max_error_rate_delta {
            regression = true;
            reasons.push("error-rate increase exceeded policy".into());
        }
        if working_set_delta.expect("validated") > thresholds.max_resource_increase {
            regression = true;
            reasons.push("working-set increase exceeded policy".into());
        }
    }
    Ok(BaselineComparison {
        comparable,
        regression,
        reasons,
        baseline_throughput_per_sec: baseline_throughput,
        candidate_throughput_per_sec: candidate_throughput,
        throughput_delta_ratio: throughput_delta,
        baseline_p95_ms: baseline_p95,
        candidate_p95_ms: candidate_p95,
        p95_delta_ratio: p95_delta,
        baseline_p99_ms: baseline_p99,
        candidate_p99_ms: candidate_p99,
        p99_delta_ratio: p99_delta,
        baseline_error_rate: baseline_errors,
        candidate_error_rate: candidate_errors,
        error_rate_delta: error_delta,
        working_set_delta_ratio: working_set_delta,
    })
}

fn duration_secs(snapshot: &BaselineSnapshot) -> Option<f64> {
    let duration = snapshot
        .metadata
        .ended_unix_ms
        .saturating_sub(snapshot.metadata.started_unix_ms);
    (duration > 0).then(|| duration as f64 / 1_000.0)
}

fn operation_count(snapshot: &BaselineSnapshot) -> Option<u64> {
    let counters = &snapshot.metrics.counters;
    counters
        .get("operations")
        .copied()
        .or_else(|| counters.get("requests").copied())
        .or_else(|| {
            let sum = counters
                .iter()
                .filter(|(key, _)| key.ends_with("_operations") || *key == "auth_requests")
                .map(|(_, value)| *value)
                .sum();
            (sum > 0).then_some(sum)
        })
}

fn throughput(snapshot: &BaselineSnapshot) -> Option<f64> {
    Some(operation_count(snapshot)? as f64 / duration_secs(snapshot)?)
}

fn latency(snapshot: &BaselineSnapshot, percentile: f64) -> Option<u64> {
    snapshot
        .metrics
        .histograms
        .get("operation_ms")
        .filter(|histogram| histogram.count() > 0)
        .map(|histogram| histogram.percentile(percentile))
}

fn error_rate(snapshot: &BaselineSnapshot) -> Option<f64> {
    let operations = operation_count(snapshot)?;
    if operations == 0 {
        return None;
    }
    let failed = snapshot
        .metrics
        .counters
        .get("failed")
        .copied()
        .or_else(|| snapshot.metrics.counters.get("business_errors").copied())
        .unwrap_or(0);
    Some(failed as f64 / operations as f64)
}

fn ratio(baseline: Option<f64>, candidate: Option<f64>) -> Option<f64> {
    match (baseline, candidate) {
        (Some(base), Some(current)) if base > 0.0 => Some(current / base - 1.0),
        (Some(0.0), Some(0.0)) => Some(0.0),
        _ => None,
    }
}

fn ratio_u64(baseline: Option<u64>, candidate: Option<u64>) -> Option<f64> {
    ratio(
        baseline.map(|value| value as f64),
        candidate.map(|value| value as f64),
    )
}

fn resource_delta(
    baseline: Option<&GeneratorResources>,
    candidate: Option<&GeneratorResources>,
) -> Option<f64> {
    let baseline = match baseline?.working_set_bytes {
        ResourceValue::Available { value } => value,
        ResourceValue::Unavailable { .. } => return None,
    };
    let candidate = match candidate?.working_set_bytes {
        ResourceValue::Available { value } => value,
        ResourceValue::Unavailable { .. } => return None,
    };
    ratio(Some(baseline as f64), Some(candidate as f64))
}

#[derive(Debug, thiserror::Error)]
pub enum BaselineError {
    #[error("invalid baseline threshold: {0}")]
    InvalidThreshold(&'static str),
    #[error("could not read baseline report: {0}")]
    Io(#[from] std::io::Error),
    #[error("baseline report JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::HistogramSnapshot;

    fn snapshot(
        run_id: &str,
        operations: u64,
        p99: u64,
        failed: u64,
        memory: u64,
    ) -> BaselineSnapshot {
        let mut metrics = MetricsSnapshot::default();
        metrics.counters.insert("operations".into(), operations);
        metrics.counters.insert("failed".into(), failed);
        let mut histogram = HistogramSnapshot::default();
        histogram.record(p99);
        metrics.histograms.insert("operation_ms".into(), histogram);
        BaselineSnapshot::from_parts(
            BaselineMetadata {
                schema_version: 1,
                run_id: run_id.into(),
                status: "completed".into(),
                environment: "local".into(),
                scenario_hash: "scenario".into(),
                tool_git_commit: "commit".into(),
                account_batch: "batch".into(),
                started_unix_ms: 0,
                ended_unix_ms: 10_000,
                service_versions: Some(BTreeMap::from([(
                    String::from("game"),
                    String::from("v1"),
                )])),
                generator_resources: Some(GeneratorResources {
                    process_cpu_ms: ResourceValue::Available { value: 1 },
                    working_set_bytes: ResourceValue::Available { value: memory },
                    thread_count: ResourceValue::Available { value: 1 },
                    handle_count: ResourceValue::Available { value: 1 },
                    network_sent_bytes: ResourceValue::Available { value: 1 },
                    network_received_bytes: ResourceValue::Available { value: 1 },
                    socket_errors: ResourceValue::Available { value: 0 },
                    tokio_scheduler_lag_ms: ResourceValue::Available { value: 0 },
                    worker_queue_depth: ResourceValue::Available { value: 0 },
                    metrics_channel_dropped: ResourceValue::Available { value: 0 },
                }),
            },
            metrics,
        )
    }

    #[test]
    fn baseline_compare_reports_threshold_regression() {
        let baseline = snapshot("base", 100, 100, 1, 100);
        let candidate = snapshot("candidate", 80, 130, 4, 130);
        let result = compare(&baseline, &candidate, BaselineThresholds::default()).unwrap();
        assert!(result.comparable);
        assert!(result.regression);
        assert!(
            result
                .reasons
                .iter()
                .any(|reason| reason.contains("throughput"))
        );
    }

    #[test]
    fn baseline_compare_is_not_comparable_without_service_versions() {
        let mut baseline = snapshot("base", 100, 100, 0, 100);
        baseline.metadata.service_versions = None;
        let candidate = snapshot("candidate", 100, 100, 0, 100);
        let result = compare(&baseline, &candidate, BaselineThresholds::default()).unwrap();
        assert!(!result.comparable);
        assert!(!result.regression);
        assert!(
            result
                .reasons
                .iter()
                .any(|reason| reason.contains("service version"))
        );
    }

    #[test]
    fn report_snapshot_loads_run_and_metrics_files() {
        let root = std::env::temp_dir().join(format!("loadtest-baseline-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let snapshot = snapshot("base", 10, 10, 0, 100);
        fs::write(
            root.join("run.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": snapshot.metadata.schema_version,
                "run_id": snapshot.metadata.run_id,
                "status": snapshot.metadata.status,
                "environment": snapshot.metadata.environment,
                "scenario_hash": snapshot.metadata.scenario_hash,
                "tool_git_commit": snapshot.metadata.tool_git_commit,
                "account_batch": snapshot.metadata.account_batch,
                "started_unix_ms": snapshot.metadata.started_unix_ms,
                "ended_unix_ms": snapshot.metadata.ended_unix_ms,
                "generator_resources": snapshot.metadata.generator_resources,
                "service_versions": snapshot.metadata.service_versions,
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("metrics.json"),
            serde_json::to_vec(&snapshot.metrics).unwrap(),
        )
        .unwrap();
        assert_eq!(
            BaselineSnapshot::load(&root).unwrap().metadata.run_id,
            "base"
        );
        let _ = fs::remove_dir_all(root);
    }
}
