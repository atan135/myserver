use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AbortReason {
    CtrlC,
    StopFile,
    Deadline,
    ErrorRate,
    ConnectionFailureRate,
    P99Latency,
    GeneratorResource,
    ProtectionUnknown,
    ControllerAbort,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownPhase {
    Running,
    StopNewSessions,
    GracefulDraining,
    ForceRelease,
    FlushMetrics,
    Completed,
}

/// Drives the shutdown contract independently of a transport implementation.
///
/// A future HTTP/KCP worker calls `advance` from its controller loop and maps
/// each phase to its connection pool. The stage-one dry-run advances the same
/// states deterministically without opening a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GracefulShutdown {
    phase: ShutdownPhase,
    graceful_shutdown_ms: u64,
    graceful_deadline_monotonic_ms: Option<u64>,
}

impl GracefulShutdown {
    pub fn new(graceful_shutdown_ms: u64) -> Self {
        Self {
            phase: ShutdownPhase::Running,
            graceful_shutdown_ms: graceful_shutdown_ms.max(1),
            graceful_deadline_monotonic_ms: None,
        }
    }

    pub fn phase(&self) -> ShutdownPhase {
        self.phase
    }

    pub fn begin(&mut self, now_monotonic_ms: u64) {
        if self.phase == ShutdownPhase::Running {
            self.phase = ShutdownPhase::StopNewSessions;
            self.graceful_deadline_monotonic_ms =
                Some(now_monotonic_ms.saturating_add(self.graceful_shutdown_ms));
        }
    }

    /// Advances one controller-loop iteration. `active_sessions` is sampled
    /// after session creation has been stopped.
    pub fn advance(&mut self, now_monotonic_ms: u64, active_sessions: u64) {
        self.phase = match self.phase {
            ShutdownPhase::Running => ShutdownPhase::Running,
            ShutdownPhase::StopNewSessions => ShutdownPhase::GracefulDraining,
            ShutdownPhase::GracefulDraining if active_sessions == 0 => ShutdownPhase::FlushMetrics,
            ShutdownPhase::GracefulDraining
                if now_monotonic_ms
                    >= self
                        .graceful_deadline_monotonic_ms
                        .expect("shutdown deadline is set by begin") =>
            {
                ShutdownPhase::ForceRelease
            }
            ShutdownPhase::GracefulDraining => ShutdownPhase::GracefulDraining,
            ShutdownPhase::ForceRelease => ShutdownPhase::FlushMetrics,
            ShutdownPhase::FlushMetrics => ShutdownPhase::Completed,
            ShutdownPhase::Completed => ShutdownPhase::Completed,
        };
    }
}

#[derive(Debug, Clone, Default)]
pub struct AbortController {
    reason: Option<AbortReason>,
}

impl AbortController {
    pub fn request(&mut self, reason: AbortReason) {
        if self.reason.is_none() {
            self.reason = Some(reason);
        }
    }
    pub fn reason(&self) -> Option<&AbortReason> {
        self.reason.as_ref()
    }
    pub fn should_stop_new_sessions(&self) -> bool {
        self.reason.is_some()
    }
    pub fn check_stop_file(&mut self, path: Option<&Path>) {
        if path.is_some_and(Path::exists) {
            self.request(AbortReason::StopFile);
        }
    }
    pub fn check_ctrl_c(&mut self, flag: &AtomicBool) {
        if flag.load(Ordering::SeqCst) {
            self.request(AbortReason::CtrlC);
        }
    }
    pub fn check_deadline(&mut self, now_unix_ms: u64, deadline_unix_ms: u64) {
        if now_unix_ms >= deadline_unix_ms {
            self.request(AbortReason::Deadline);
        }
    }
    pub fn check_protection(&mut self, protection_confirmed: bool) {
        if !protection_confirmed {
            self.request(AbortReason::ProtectionUnknown);
        }
    }
    pub fn check_thresholds(
        &mut self,
        error_rate: f64,
        connection_failure_rate: f64,
        p99_ms: u64,
        max_error_rate: f64,
        max_connection_failure_rate: f64,
        max_p99_ms: u64,
        generator_healthy: bool,
    ) {
        if !generator_healthy {
            self.request(AbortReason::GeneratorResource);
        } else if error_rate > max_error_rate {
            self.request(AbortReason::ErrorRate);
        } else if connection_failure_rate > max_connection_failure_rate {
            self.request(AbortReason::ConnectionFailureRate);
        } else if p99_ms > max_p99_ms {
            self.request(AbortReason::P99Latency);
        }
    }
}

pub fn install_ctrl_c_flag() -> Result<Arc<AtomicBool>, ctrlc::Error> {
    let requested = Arc::new(AtomicBool::new(false));
    let handler_requested = requested.clone();
    ctrlc::set_handler(move || handler_requested.store(true, Ordering::SeqCst))?;
    Ok(requested)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_abort_sources_share_first_reason_wins_contract() {
        let mut controller = AbortController::default();
        controller.check_thresholds(0.2, 0.0, 1, 0.1, 0.1, 10, true);
        controller.request(AbortReason::CtrlC);
        assert_eq!(controller.reason(), Some(&AbortReason::ErrorRate));
    }

    #[test]
    fn ctrl_c_stop_file_deadline_and_thresholds_trigger_the_same_controller() {
        let ctrl_c = AtomicBool::new(true);
        let mut controller = AbortController::default();
        controller.check_ctrl_c(&ctrl_c);
        assert_eq!(controller.reason(), Some(&AbortReason::CtrlC));

        let stop_file = std::env::temp_dir().join(format!("loadtest-stop-{}", std::process::id()));
        std::fs::write(&stop_file, "stop").unwrap();
        let mut controller = AbortController::default();
        controller.check_stop_file(Some(&stop_file));
        std::fs::remove_file(&stop_file).unwrap();
        assert_eq!(controller.reason(), Some(&AbortReason::StopFile));

        let mut controller = AbortController::default();
        controller.check_deadline(10, 10);
        assert_eq!(controller.reason(), Some(&AbortReason::Deadline));

        let mut controller = AbortController::default();
        controller.check_thresholds(0.0, 0.2, 0, 0.1, 0.1, 1, true);
        assert_eq!(
            controller.reason(),
            Some(&AbortReason::ConnectionFailureRate)
        );
    }

    #[test]
    fn shutdown_contract_stops_sessions_drains_for_a_bound_then_forces_and_flushes() {
        let mut shutdown = GracefulShutdown::new(10);
        shutdown.begin(100);
        assert_eq!(shutdown.phase(), ShutdownPhase::StopNewSessions);
        shutdown.advance(100, 2);
        assert_eq!(shutdown.phase(), ShutdownPhase::GracefulDraining);
        shutdown.advance(110, 2);
        assert_eq!(shutdown.phase(), ShutdownPhase::ForceRelease);
        shutdown.advance(110, 0);
        assert_eq!(shutdown.phase(), ShutdownPhase::FlushMetrics);
        shutdown.advance(110, 0);
        assert_eq!(shutdown.phase(), ShutdownPhase::Completed);
    }
}
