//! Bounded offline soak observations and rolling persistence.
//!
//! The runner may collect one observation per fixed window while a long-lived
//! run is active.  This module intentionally does not open transports or
//! sample services: callers provide the already-correlated client metrics and
//! generator resource snapshot.  Persistence is bounded by rotating a small
//! number of newline-delimited JSON files.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::metrics::MetricsSnapshot;
use crate::resource::{GeneratorResources, ResourceValue};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SoakWindow {
    pub window_index: u64,
    pub started_unix_ms: u64,
    pub ended_unix_ms: u64,
    pub metrics: MetricsSnapshot,
    pub resources: GeneratorResources,
}

impl SoakWindow {
    pub fn duration_ms(&self) -> u64 {
        self.ended_unix_ms.saturating_sub(self.started_unix_ms)
    }

    /// Returns the tail latency used for drift checks, when a window observed
    /// operation latency.  Percentiles are computed only from the mergeable
    /// HDR histogram in this window.
    pub fn p99_ms(&self) -> u64 {
        self.metrics
            .histograms
            .get("operation_ms")
            .map_or(0, |histogram| histogram.percentile(0.99))
    }

    pub fn queue_depth(&self) -> Option<u64> {
        match self.resources.worker_queue_depth {
            ResourceValue::Available { value } => Some(value),
            ResourceValue::Unavailable { .. } => None,
        }
    }

    pub fn working_set_bytes(&self) -> Option<u64> {
        match self.resources.working_set_bytes {
            ResourceValue::Available { value } => Some(value),
            ResourceValue::Unavailable { .. } => None,
        }
    }

    pub fn handle_count(&self) -> Option<u32> {
        match self.resources.handle_count {
            ResourceValue::Available { value } => Some(value),
            ResourceValue::Unavailable { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SoakDriftPolicy {
    /// Relative increase allowed for P99 latency, memory, handles and queue.
    /// A value of `0.10` allows a 10% increase over the first window.
    pub max_relative_increase: f64,
    /// Absolute increase allowed for the error rate.
    pub max_error_rate_delta: f64,
}

impl SoakDriftPolicy {
    pub fn validate(self) -> Result<(), SoakError> {
        if !self.max_relative_increase.is_finite()
            || self.max_relative_increase < 0.0
            || self.max_relative_increase > 1.0
        {
            return Err(SoakError::InvalidPolicy("max_relative_increase"));
        }
        if !self.max_error_rate_delta.is_finite()
            || self.max_error_rate_delta < 0.0
            || self.max_error_rate_delta > 1.0
        {
            return Err(SoakError::InvalidPolicy("max_error_rate_delta"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SoakAssessment {
    pub comparable: bool,
    pub windows: usize,
    pub p99_drift_ratio: Option<f64>,
    pub queue_drift_ratio: Option<f64>,
    pub working_set_drift_ratio: Option<f64>,
    pub handle_drift_ratio: Option<f64>,
    pub error_rate_delta: Option<f64>,
    pub violations: Vec<String>,
}

/// Compare the first and last windows. Missing generator/service observations
/// are reported as non-comparable instead of being interpreted as zero.
pub fn assess_soak(
    windows: &[SoakWindow],
    policy: SoakDriftPolicy,
) -> Result<SoakAssessment, SoakError> {
    policy.validate()?;
    if windows.len() < 2 {
        return Ok(SoakAssessment {
            comparable: false,
            windows: windows.len(),
            p99_drift_ratio: None,
            queue_drift_ratio: None,
            working_set_drift_ratio: None,
            handle_drift_ratio: None,
            error_rate_delta: None,
            violations: vec!["at least two soak windows are required".into()],
        });
    }
    let first = &windows[0];
    let last = windows.last().expect("length checked");
    let mut violations = Vec::new();
    let p99 = ratio(first.p99_ms(), last.p99_ms());
    if p99.is_none() {
        violations.push("operation P99 observation is incomplete".into());
    }
    if exceeds(p99, policy.max_relative_increase) {
        violations.push("operation P99 drift exceeded policy".into());
    }
    let queue = optional_ratio(first.queue_depth(), last.queue_depth());
    if exceeds(queue, policy.max_relative_increase) {
        violations.push("generator queue depth drift exceeded policy".into());
    }
    let memory = optional_ratio(first.working_set_bytes(), last.working_set_bytes());
    if exceeds(memory, policy.max_relative_increase) {
        violations.push("working-set drift exceeded policy".into());
    }
    let handles = optional_ratio(
        first.handle_count().map(u64::from),
        last.handle_count().map(u64::from),
    );
    if exceeds(handles, policy.max_relative_increase) {
        violations.push("handle-count drift exceeded policy".into());
    }
    let first_error = error_rate(first);
    let last_error = error_rate(last);
    let error_delta = first_error.zip(last_error).map(|(a, b)| b - a);
    if error_delta.is_none() {
        violations.push("error-rate observation is incomplete".into());
    } else if error_delta.expect("checked above") > policy.max_error_rate_delta {
        violations.push("error-rate drift exceeded policy".into());
    }
    Ok(SoakAssessment {
        comparable: violations.is_empty()
            && first_error.is_some()
            && p99.is_some()
            && queue.is_some()
            && memory.is_some()
            && handles.is_some(),
        windows: windows.len(),
        p99_drift_ratio: p99,
        queue_drift_ratio: queue,
        working_set_drift_ratio: memory,
        handle_drift_ratio: handles,
        error_rate_delta: error_delta,
        violations,
    })
}

fn ratio(first: u64, last: u64) -> Option<f64> {
    (first > 0).then(|| (last as f64 / first as f64) - 1.0)
}

fn optional_ratio(first: Option<u64>, last: Option<u64>) -> Option<f64> {
    match (first, last) {
        (Some(first), Some(last)) if first > 0 => Some(last as f64 / first as f64 - 1.0),
        (Some(0), Some(0)) => Some(0.0),
        _ => None,
    }
}

fn exceeds(value: Option<f64>, allowed: f64) -> bool {
    value.is_some_and(|value| value > allowed)
}

fn error_rate(window: &SoakWindow) -> Option<f64> {
    let operations = window
        .metrics
        .counters
        .get("operations")
        .copied()
        .or_else(|| window.metrics.counters.get("requests").copied())?;
    if operations == 0 {
        return None;
    }
    let failed = window.metrics.counters.get("failed").copied().unwrap_or(0);
    Some(failed as f64 / operations as f64)
}

/// Append bounded newline-delimited soak windows and rotate old files.
#[derive(Debug)]
pub struct RollingSoakWriter {
    path: PathBuf,
    max_bytes: u64,
    max_files: usize,
    current_bytes: u64,
    last_window: Option<u64>,
}

impl RollingSoakWriter {
    pub fn new(
        path: impl Into<PathBuf>,
        max_bytes: u64,
        max_files: usize,
    ) -> Result<Self, SoakError> {
        if max_bytes < 128 {
            return Err(SoakError::InvalidLimit("max_bytes must be at least 128"));
        }
        if max_files == 0 {
            return Err(SoakError::InvalidLimit("max_files must be positive"));
        }
        let path = path.into();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let current_bytes = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        Ok(Self {
            path,
            max_bytes,
            max_files,
            current_bytes,
            last_window: None,
        })
    }

    pub fn append(&mut self, window: &SoakWindow) -> Result<(), SoakError> {
        if self
            .last_window
            .is_some_and(|last| window.window_index <= last)
        {
            return Err(SoakError::WindowOrder);
        }
        let mut line = serde_json::to_vec(window).map_err(SoakError::Serialize)?;
        line.push(b'\n');
        if line.len() as u64 > self.max_bytes {
            return Err(SoakError::WindowTooLarge {
                bytes: line.len() as u64,
                maximum: self.max_bytes,
            });
        }
        if self.current_bytes > 0 && self.current_bytes + line.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(&line)?;
        file.flush()?;
        self.current_bytes += line.len() as u64;
        self.last_window = Some(window.window_index);
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn current_bytes(&self) -> u64 {
        self.current_bytes
    }

    fn rotate(&mut self) -> Result<(), SoakError> {
        if self.max_files > 2 {
            for index in (2..self.max_files).rev() {
                let source = self.suffixed_path(index - 1);
                let destination = self.suffixed_path(index);
                if destination.exists() {
                    fs::remove_file(&destination)?;
                }
                if source.exists() {
                    fs::rename(source, destination)?;
                }
            }
        }
        if self.path.exists() {
            if self.max_files == 1 {
                fs::remove_file(&self.path)?;
            } else {
                let destination = self.suffixed_path(1);
                if destination.exists() {
                    fs::remove_file(&destination)?;
                }
                fs::rename(&self.path, destination)?;
            }
        }
        self.current_bytes = 0;
        Ok(())
    }

    fn suffixed_path(&self, index: usize) -> PathBuf {
        PathBuf::from(format!("{}.{}", self.path.display(), index))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SoakError {
    #[error("invalid soak policy: {0}")]
    InvalidPolicy(&'static str),
    #[error("invalid rolling writer limit: {0}")]
    InvalidLimit(&'static str),
    #[error("soak windows must be appended in strictly increasing order")]
    WindowOrder,
    #[error("soak window is {bytes} bytes, above the rolling limit of {maximum}")]
    WindowTooLarge { bytes: u64, maximum: u64 },
    #[error("could not serialize soak window: {0}")]
    Serialize(serde_json::Error),
    #[error("rolling soak persistence failed: {0}")]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(index: u64, p99: u64, failed: u64, operations: u64) -> SoakWindow {
        let mut metrics = MetricsSnapshot::default();
        metrics.counters.insert("operations".into(), operations);
        metrics.counters.insert("failed".into(), failed);
        let mut histogram = crate::metrics::HistogramSnapshot::default();
        histogram.record(p99);
        metrics.histograms.insert("operation_ms".into(), histogram);
        SoakWindow {
            window_index: index,
            started_unix_ms: index * 1_000,
            ended_unix_ms: index * 1_000 + 999,
            metrics,
            resources: GeneratorResources {
                process_cpu_ms: ResourceValue::Available { value: 1 },
                working_set_bytes: ResourceValue::Available { value: 100 },
                thread_count: ResourceValue::Available { value: 1 },
                handle_count: ResourceValue::Available { value: 10 },
                network_sent_bytes: ResourceValue::Available { value: 1 },
                network_received_bytes: ResourceValue::Available { value: 1 },
                socket_errors: ResourceValue::Available { value: 0 },
                tokio_scheduler_lag_ms: ResourceValue::Available { value: 0 },
                worker_queue_depth: ResourceValue::Available { value: 2 },
                metrics_channel_dropped: ResourceValue::Available { value: 0 },
            },
        }
    }

    #[test]
    fn soak_assessment_detects_tail_and_resource_drift() {
        let first = window(0, 100, 1, 100);
        let mut last = window(1, 130, 4, 100);
        last.resources.working_set_bytes = ResourceValue::Available { value: 130 };
        last.resources.handle_count = ResourceValue::Available { value: 13 };
        last.resources.worker_queue_depth = ResourceValue::Available { value: 3 };
        let assessment = assess_soak(
            &[first, last],
            SoakDriftPolicy {
                max_relative_increase: 0.2,
                max_error_rate_delta: 0.01,
            },
        )
        .unwrap();
        assert!(!assessment.comparable);
        assert!(assessment.violations.len() >= 3);
        assert!((assessment.p99_drift_ratio.unwrap() - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn rolling_writer_keeps_a_bounded_number_of_files() {
        let root = std::env::temp_dir().join(format!("loadtest-soak-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let path = root.join("windows.jsonl");
        let mut writer = RollingSoakWriter::new(&path, 1_200, 2).unwrap();
        for index in 0..10 {
            writer.append(&window(index, 10, 0, 10)).unwrap();
        }
        assert!(writer.current_bytes() <= 1_200);
        assert!(path.is_file());
        assert!(PathBuf::from(format!("{}.1", path.display())).is_file());
        assert!(!PathBuf::from(format!("{}.2", path.display())).exists());
        assert!(writer.append(&window(9, 10, 0, 10)).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_tail_latency_observation_is_not_comparable() {
        let mut first = window(0, 0, 0, 10);
        first.metrics.histograms.clear();
        let last = window(1, 10, 0, 10);
        let assessment = assess_soak(
            &[first, last],
            SoakDriftPolicy {
                max_relative_increase: 0.2,
                max_error_rate_delta: 0.01,
            },
        )
        .unwrap();
        assert!(!assessment.comparable);
        assert!(
            assessment
                .violations
                .iter()
                .any(|reason| reason.contains("P99 observation"))
        );
    }
}
