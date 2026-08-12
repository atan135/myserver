//! Offline generator calibration and capacity-conclusion gates.
//!
//! Calibration measures only the local load generator. It must never turn a
//! dry-run result into a service capacity claim: server observations are an
//! explicit input and are rejected when the generator saturated first.

use serde::{Deserialize, Serialize};
use std::hint::black_box;
use std::time::{Duration, Instant};

use crate::config::LoadModel;
use crate::resource::{GeneratorResources, ResourceValue};
use crate::scheduler::MonotonicScheduler;

pub const DEFAULT_CPU_UTILIZATION_BASIS_POINTS: u32 = 8_000;
pub const DEFAULT_MAX_WORKING_SET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_SCHEDULER_LAG_MS: u64 = 50;
pub const DEFAULT_RESERVE_PERCENT: u8 = 20;
pub const DEFAULT_LEVEL_WINDOW_MS: u64 = 100;
pub const DEFAULT_TICK_INTERVAL_MS: u64 = 25;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalibrationThresholds {
    pub max_cpu_utilization_basis_points: u32,
    pub max_working_set_bytes: u64,
    pub max_scheduler_lag_ms: u64,
    pub max_metrics_dropped: u64,
    pub reserve_percent: u8,
    pub level_window_ms: u64,
    pub tick_interval_ms: u64,
}

impl Default for CalibrationThresholds {
    fn default() -> Self {
        Self {
            max_cpu_utilization_basis_points: DEFAULT_CPU_UTILIZATION_BASIS_POINTS,
            max_working_set_bytes: DEFAULT_MAX_WORKING_SET_BYTES,
            max_scheduler_lag_ms: DEFAULT_MAX_SCHEDULER_LAG_MS,
            max_metrics_dropped: 0,
            reserve_percent: DEFAULT_RESERVE_PERCENT,
            level_window_ms: DEFAULT_LEVEL_WINDOW_MS,
            tick_interval_ms: DEFAULT_TICK_INTERVAL_MS,
        }
    }
}

impl CalibrationThresholds {
    pub fn validate(&self) -> Result<(), String> {
        if self.max_cpu_utilization_basis_points == 0
            || self.max_cpu_utilization_basis_points > 10_000
        {
            return Err(
                "calibration max_cpu_utilization_basis_points must be within 1..=10000".into(),
            );
        }
        if self.max_working_set_bytes == 0 || self.max_scheduler_lag_ms == 0 {
            return Err("calibration memory and scheduler-lag thresholds must be positive".into());
        }
        if self.reserve_percent >= 100 {
            return Err("calibration reserve_percent must be within 0..=99".into());
        }
        if self.level_window_ms == 0 || self.level_window_ms > 1_000 {
            return Err("calibration level_window_ms must be within 1..=1000".into());
        }
        if self.tick_interval_ms == 0 || self.tick_interval_ms > self.level_window_ms {
            return Err("calibration tick_interval_ms must be within 1..=level_window_ms".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GeneratorLimit {
    CpuThreshold {
        observed_basis_points: u32,
        threshold_basis_points: u32,
    },
    MemoryThreshold {
        observed_bytes: u64,
        threshold_bytes: u64,
    },
    SchedulerLagThreshold {
        observed_ms: u64,
        threshold_ms: u64,
    },
    MetricsDropped {
        observed: u64,
        threshold: u64,
    },
    MeasurementUnavailable {
        resource: String,
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalibrationLevel {
    pub offered_virtual_players: u32,
    pub elapsed_ms: u64,
    pub planned_actions: u64,
    pub scheduled_actions: u64,
    pub dropped_actions: u64,
    pub max_queue_depth: u64,
    pub work_checksum: u64,
    pub cpu_utilization_basis_points: Option<u32>,
    pub working_set_bytes: Option<u64>,
    pub scheduler_lag_ms: Option<u64>,
    pub metrics_dropped: Option<u64>,
    pub limit: Option<GeneratorLimit>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalibrationWorkload {
    pub planned_actions: u64,
    pub scheduled_actions: u64,
    pub dropped_actions: u64,
    pub max_scheduler_lag_ms: u64,
    pub max_queue_depth: u64,
    pub elapsed_ms: u64,
    pub work_checksum: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeneratorCapacity {
    pub highest_stable_virtual_players: u32,
    pub recommended_virtual_players: u32,
    pub reserve_percent: u8,
    pub saturated_at_virtual_players: Option<u32>,
    pub saturation: Option<GeneratorLimit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "availability", rename_all = "snake_case")]
pub enum CapacityConclusion {
    Available { virtual_players: u32 },
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceCapacityEvidence {
    pub stable_virtual_players: u32,
    pub burst_virtual_players: u32,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalibrationReport {
    pub thresholds: CalibrationThresholds,
    pub levels: Vec<CalibrationLevel>,
    pub generator_capacity: CapacityConclusion,
    pub generator_detail: Option<GeneratorCapacity>,
    pub service_stable_capacity: CapacityConclusion,
    pub system_burst_capacity: CapacityConclusion,
}

#[derive(Debug, Clone)]
pub struct CalibrationRun {
    thresholds: CalibrationThresholds,
    levels: Vec<CalibrationLevel>,
    highest_stable_virtual_players: u32,
    stopped: bool,
}

impl CalibrationRun {
    pub fn new(thresholds: CalibrationThresholds) -> Self {
        Self {
            thresholds,
            levels: Vec::new(),
            highest_stable_virtual_players: 0,
            stopped: false,
        }
    }

    pub fn should_continue(&self) -> bool {
        !self.stopped
    }

    pub fn observe(
        &mut self,
        offered_virtual_players: u32,
        workload: CalibrationWorkload,
        before: &GeneratorResources,
        after: &GeneratorResources,
    ) -> CalibrationLevel {
        let limit = evaluate_limit(self.thresholds, workload.elapsed_ms, before, after);
        let level = CalibrationLevel {
            offered_virtual_players,
            elapsed_ms: workload.elapsed_ms,
            planned_actions: workload.planned_actions,
            scheduled_actions: workload.scheduled_actions,
            dropped_actions: workload.dropped_actions,
            max_queue_depth: workload.max_queue_depth,
            work_checksum: workload.work_checksum,
            cpu_utilization_basis_points: cpu_utilization_basis_points(
                workload.elapsed_ms,
                before,
                after,
            )
            .ok(),
            working_set_bytes: available_value(&after.working_set_bytes, "working_set_bytes").ok(),
            scheduler_lag_ms: available_value(
                &after.tokio_scheduler_lag_ms,
                "tokio_scheduler_lag_ms",
            )
            .ok(),
            metrics_dropped: available_value(
                &after.metrics_channel_dropped,
                "metrics_channel_dropped",
            )
            .ok(),
            limit: limit.clone(),
        };
        if limit.is_some() {
            self.stopped = true;
        } else {
            self.highest_stable_virtual_players = offered_virtual_players;
        }
        self.levels.push(level.clone());
        level
    }

    pub fn finish(self, service: Option<ServiceCapacityEvidence>) -> CalibrationReport {
        let saturation = self.levels.iter().find_map(|level| {
            level
                .limit
                .clone()
                .map(|limit| (level.offered_virtual_players, limit))
        });
        let generator_detail = if self.highest_stable_virtual_players == 0 {
            None
        } else {
            Some(GeneratorCapacity {
                highest_stable_virtual_players: self.highest_stable_virtual_players,
                recommended_virtual_players: reserve_capacity(
                    self.highest_stable_virtual_players,
                    self.thresholds.reserve_percent,
                ),
                reserve_percent: self.thresholds.reserve_percent,
                saturated_at_virtual_players: saturation.as_ref().map(|(players, _)| *players),
                saturation: saturation.as_ref().map(|(_, limit)| limit.clone()),
            })
        };
        let generator_capacity = generator_detail.as_ref().map_or_else(
            || CapacityConclusion::Unavailable {
                reason: saturation.as_ref().map_or_else(
                    || "no stable generator calibration level completed".into(),
                    |(_, limit)| {
                        format!(
                            "generator measurement could not establish a stable level: {limit:?}"
                        )
                    },
                ),
            },
            |detail| CapacityConclusion::Available {
                virtual_players: detail.recommended_virtual_players,
            },
        );
        let service_blocker = match (&generator_capacity, service.as_ref()) {
            (CapacityConclusion::Unavailable { reason }, _) => Some(format!(
                "service capacity is unavailable because generator calibration is unavailable: {reason}"
            )),
            (_, None) => Some(
                "service capacity is unavailable because calibration has no service observation".into(),
            ),
            (_, Some(evidence)) if !evidence.complete => Some(
                "service capacity is unavailable because service observation is incomplete".into(),
            ),
            (_, Some(_)) if saturation.is_some() => Some(
                "service capacity is unavailable because the generator saturated before the service conclusion".into(),
            ),
            _ => None,
        };
        let (service_stable_capacity, system_burst_capacity) = if let Some(reason) = service_blocker
        {
            (
                CapacityConclusion::Unavailable {
                    reason: reason.clone(),
                },
                CapacityConclusion::Unavailable { reason },
            )
        } else {
            let evidence = service.expect("service evidence exists after blocker checks");
            (
                CapacityConclusion::Available {
                    virtual_players: evidence.stable_virtual_players,
                },
                CapacityConclusion::Available {
                    virtual_players: evidence.burst_virtual_players,
                },
            )
        };
        CalibrationReport {
            thresholds: self.thresholds,
            levels: self.levels,
            generator_capacity,
            generator_detail,
            service_stable_capacity,
            system_burst_capacity,
        }
    }
}

pub fn bounded_calibration_operations(
    max_virtual_players: u32,
    thresholds: CalibrationThresholds,
) -> u64 {
    progressive_levels(max_virtual_players)
        .into_iter()
        .map(|players| {
            u64::from(players).saturating_mul(
                thresholds
                    .level_window_ms
                    .saturating_add(thresholds.tick_interval_ms.saturating_sub(1))
                    .saturating_div(thresholds.tick_interval_ms),
            )
        })
        .sum()
}

pub fn bounded_calibration_duration_ms(
    max_virtual_players: u32,
    thresholds: CalibrationThresholds,
) -> u64 {
    (progressive_levels(max_virtual_players).len() as u64)
        .saturating_mul(thresholds.level_window_ms)
}

/// Runs one bounded, synthetic generator window. The scheduler is driven at a
/// fixed monotonic cadence and each planned action performs deterministic CPU
/// work only; it has no transport, credential, or service dependency.
pub fn run_local_workload(players: u32, thresholds: CalibrationThresholds) -> CalibrationWorkload {
    let load = LoadModel::ArrivalRate {
        arrivals_per_second: f64::from(players) * 1_000.0 / thresholds.tick_interval_ms as f64,
        duration_secs: 1,
    };
    let mut scheduler = MonotonicScheduler::new(&load, 100, players as usize);
    let started = Instant::now();
    let mut planned_actions = 0_u64;
    let mut scheduled_actions = 0_u64;
    let mut dropped_actions = 0_u64;
    let mut max_scheduler_lag_ms = 0_u64;
    let mut max_queue_depth = 0_u64;
    let mut work_checksum = 0_u64;
    let mut now_ms = 0_u64;
    while now_ms < thresholds.level_window_ms {
        let tick = scheduler.due(now_ms);
        planned_actions = planned_actions
            .saturating_add((tick.actions.len() as u64).saturating_add(tick.dropped));
        scheduled_actions = scheduled_actions.saturating_add(tick.actions.len() as u64);
        dropped_actions = dropped_actions.saturating_add(tick.dropped);
        max_queue_depth = max_queue_depth.max(tick.queue_depth);
        for action in tick.actions {
            max_scheduler_lag_ms = max_scheduler_lag_ms.max(action.scheduler_lag_ms);
            work_checksum = work_checksum.wrapping_add(synthetic_work(
                players,
                action.planned_at_ms,
                action.actual_at_ms,
            ));
        }
        let next_ms = now_ms.saturating_add(thresholds.tick_interval_ms);
        let target = Duration::from_millis(next_ms.min(thresholds.level_window_ms));
        if started.elapsed() < target {
            std::thread::sleep(target - started.elapsed());
        }
        now_ms = next_ms;
    }
    CalibrationWorkload {
        planned_actions,
        scheduled_actions,
        dropped_actions,
        max_scheduler_lag_ms,
        max_queue_depth,
        elapsed_ms: started.elapsed().as_millis().max(1) as u64,
        work_checksum,
    }
}

fn synthetic_work(players: u32, planned_at_ms: u64, actual_at_ms: u64) -> u64 {
    let mut state = u64::from(players)
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(planned_at_ms)
        .wrapping_add(actual_at_ms);
    for _ in 0..8 {
        state = state.rotate_left(7).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    black_box(state)
}

pub fn progressive_levels(max_virtual_players: u32) -> Vec<u32> {
    let mut levels = Vec::new();
    let mut current = 1_u32;
    while current < max_virtual_players {
        levels.push(current);
        current = current.saturating_mul(2);
    }
    if max_virtual_players > 0 && levels.last().copied() != Some(max_virtual_players) {
        levels.push(max_virtual_players);
    }
    levels
}

fn reserve_capacity(stable: u32, reserve_percent: u8) -> u32 {
    let retained = 100_u32.saturating_sub(u32::from(reserve_percent));
    stable.saturating_mul(retained).saturating_div(100).max(1)
}

fn evaluate_limit(
    thresholds: CalibrationThresholds,
    elapsed_ms: u64,
    before: &GeneratorResources,
    after: &GeneratorResources,
) -> Option<GeneratorLimit> {
    let cpu = match cpu_utilization_basis_points(elapsed_ms, before, after) {
        Ok(value) => value,
        Err(limit) => return Some(limit),
    };
    if cpu >= thresholds.max_cpu_utilization_basis_points {
        return Some(GeneratorLimit::CpuThreshold {
            observed_basis_points: cpu,
            threshold_basis_points: thresholds.max_cpu_utilization_basis_points,
        });
    }
    let memory = match available_value(&after.working_set_bytes, "working_set_bytes") {
        Ok(value) => value,
        Err(limit) => return Some(limit),
    };
    if memory >= thresholds.max_working_set_bytes {
        return Some(GeneratorLimit::MemoryThreshold {
            observed_bytes: memory,
            threshold_bytes: thresholds.max_working_set_bytes,
        });
    }
    let scheduler_lag =
        match available_value(&after.tokio_scheduler_lag_ms, "tokio_scheduler_lag_ms") {
            Ok(value) => value,
            Err(limit) => return Some(limit),
        };
    if scheduler_lag >= thresholds.max_scheduler_lag_ms {
        return Some(GeneratorLimit::SchedulerLagThreshold {
            observed_ms: scheduler_lag,
            threshold_ms: thresholds.max_scheduler_lag_ms,
        });
    }
    let metrics_dropped =
        match available_value(&after.metrics_channel_dropped, "metrics_channel_dropped") {
            Ok(value) => value,
            Err(limit) => return Some(limit),
        };
    if metrics_dropped > thresholds.max_metrics_dropped {
        return Some(GeneratorLimit::MetricsDropped {
            observed: metrics_dropped,
            threshold: thresholds.max_metrics_dropped,
        });
    }
    None
}

fn cpu_utilization_basis_points(
    elapsed_ms: u64,
    before: &GeneratorResources,
    after: &GeneratorResources,
) -> Result<u32, GeneratorLimit> {
    if elapsed_ms == 0 {
        return Err(GeneratorLimit::MeasurementUnavailable {
            resource: "calibration_window_ms".into(),
            reason: "calibration sample window must be nonzero".into(),
        });
    }
    let before_cpu = available_value(&before.process_cpu_ms, "process_cpu_ms")?;
    let after_cpu = available_value(&after.process_cpu_ms, "process_cpu_ms")?;
    let consumed = after_cpu.checked_sub(before_cpu).ok_or_else(|| {
        GeneratorLimit::MeasurementUnavailable {
            resource: "process_cpu_ms".into(),
            reason: "process CPU counter moved backwards during calibration".into(),
        }
    })?;
    Ok(consumed
        .saturating_mul(10_000)
        .saturating_div(elapsed_ms)
        .min(u64::from(u32::MAX)) as u32)
}

fn available_value<T: Copy>(
    value: &ResourceValue<T>,
    resource: &'static str,
) -> Result<T, GeneratorLimit> {
    match value {
        ResourceValue::Available { value } => Ok(*value),
        ResourceValue::Unavailable { reason } => Err(GeneratorLimit::MeasurementUnavailable {
            resource: resource.into(),
            reason: reason.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resources(cpu_ms: u64, memory: u64, lag: u64, dropped: u64) -> GeneratorResources {
        GeneratorResources {
            process_cpu_ms: ResourceValue::Available { value: cpu_ms },
            working_set_bytes: ResourceValue::Available { value: memory },
            thread_count: ResourceValue::Available { value: 1 },
            handle_count: ResourceValue::Available { value: 1 },
            network_sent_bytes: ResourceValue::Available { value: 0 },
            network_received_bytes: ResourceValue::Available { value: 0 },
            socket_errors: ResourceValue::Available { value: 0 },
            tokio_scheduler_lag_ms: ResourceValue::Available { value: lag },
            worker_queue_depth: ResourceValue::Available { value: 0 },
            metrics_channel_dropped: ResourceValue::Available { value: dropped },
        }
    }

    fn workload(elapsed_ms: u64) -> CalibrationWorkload {
        CalibrationWorkload {
            planned_actions: 1,
            scheduled_actions: 1,
            dropped_actions: 0,
            max_scheduler_lag_ms: 0,
            max_queue_depth: 0,
            elapsed_ms,
            work_checksum: 1,
        }
    }

    #[test]
    fn progressive_calibration_reserves_headroom_before_generator_conclusion() {
        let mut run = CalibrationRun::new(CalibrationThresholds {
            max_cpu_utilization_basis_points: 8_000,
            max_working_set_bytes: 100,
            max_scheduler_lag_ms: 10,
            max_metrics_dropped: 0,
            reserve_percent: 25,
            ..CalibrationThresholds::default()
        });
        assert_eq!(progressive_levels(7), vec![1, 2, 4, 7]);
        assert!(
            run.observe(
                1,
                workload(100),
                &resources(0, 10, 0, 0),
                &resources(10, 10, 1, 0),
            )
            .limit
            .is_none()
        );
        let saturated = run.observe(
            2,
            workload(100),
            &resources(10, 10, 0, 0),
            &resources(90, 10, 1, 0),
        );
        assert!(matches!(
            saturated.limit,
            Some(GeneratorLimit::CpuThreshold { .. })
        ));
        assert!(!run.should_continue());

        let report = run.finish(None);
        assert_eq!(
            report.generator_detail.unwrap().recommended_virtual_players,
            1
        );
        assert!(matches!(
            report.service_stable_capacity,
            CapacityConclusion::Unavailable { .. }
        ));
    }

    #[test]
    fn unavailable_resources_do_not_become_zero_or_a_capacity_claim() {
        let mut run = CalibrationRun::new(CalibrationThresholds::default());
        let mut after = resources(1, 1, 0, 0);
        after.working_set_bytes = ResourceValue::Unavailable {
            reason: "denied".into(),
        };
        let level = run.observe(1, workload(100), &resources(0, 1, 0, 0), &after);
        assert!(matches!(
            level.limit,
            Some(GeneratorLimit::MeasurementUnavailable { .. })
        ));
        let report = run.finish(None);
        assert!(matches!(
            report.generator_capacity,
            CapacityConclusion::Unavailable { .. }
        ));
    }

    #[test]
    fn service_capacity_requires_complete_observation_and_unsaturated_generator() {
        let mut run = CalibrationRun::new(CalibrationThresholds::default());
        run.observe(
            4,
            workload(100),
            &resources(0, 1, 0, 0),
            &resources(10, 1, 0, 0),
        );
        let report = run.finish(Some(ServiceCapacityEvidence {
            stable_virtual_players: 3,
            burst_virtual_players: 5,
            complete: true,
        }));
        assert_eq!(
            report.service_stable_capacity,
            CapacityConclusion::Available { virtual_players: 3 }
        );
        assert_eq!(
            report.system_burst_capacity,
            CapacityConclusion::Available { virtual_players: 5 }
        );
    }

    #[test]
    fn local_workload_scales_planned_work_with_players_and_uses_a_real_window() {
        let thresholds = CalibrationThresholds {
            level_window_ms: 20,
            tick_interval_ms: 10,
            ..CalibrationThresholds::default()
        };
        let one = run_local_workload(1, thresholds);
        let four = run_local_workload(4, thresholds);
        assert!(one.elapsed_ms >= thresholds.level_window_ms);
        assert!(four.elapsed_ms >= thresholds.level_window_ms);
        assert!(four.planned_actions >= one.planned_actions);
        assert!(four.scheduled_actions >= one.scheduled_actions);
        assert_ne!(one.work_checksum, 0);
        assert_ne!(four.work_checksum, 0);
    }

    #[test]
    fn calibration_work_and_duration_bounds_cover_every_progressive_level() {
        let thresholds = CalibrationThresholds {
            level_window_ms: 25,
            tick_interval_ms: 10,
            ..CalibrationThresholds::default()
        };
        assert_eq!(bounded_calibration_duration_ms(7, thresholds), 100);
        assert_eq!(bounded_calibration_operations(7, thresholds), 42);
    }
}
