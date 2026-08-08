use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::{DependencyRequirement, StartupErrorCode, StartupState};

pub const DEFAULT_STARTUP_CONVERGENCE_WINDOW_SECS: u64 = 120;
pub const DEFAULT_READY_STABILITY_WINDOW_SECS: u64 = 10;
pub const DEFAULT_DEPENDENCY_STALE_WINDOW_SECS: u64 = 60;
pub const MAX_STARTUP_CONVERGENCE_WINDOW_SECS: u64 = 600;
pub const MAX_READY_STABILITY_WINDOW_SECS: u64 = 120;
pub const MAX_DEPENDENCY_STALE_WINDOW_SECS: u64 = 600;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthConfigError {
    variable: &'static str,
    reason: &'static str,
}

impl fmt::Display for HealthConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid health configuration {}: {}",
            self.variable, self.reason
        )
    }
}

impl std::error::Error for HealthConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthConfig {
    pub startup_convergence_window_ms: u64,
    pub ready_stability_window_ms: u64,
    pub dependency_stale_window_ms: u64,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            startup_convergence_window_ms: DEFAULT_STARTUP_CONVERGENCE_WINDOW_SECS * 1_000,
            ready_stability_window_ms: DEFAULT_READY_STABILITY_WINDOW_SECS * 1_000,
            dependency_stale_window_ms: DEFAULT_DEPENDENCY_STALE_WINDOW_SECS * 1_000,
        }
    }
}

impl HealthConfig {
    /// Compatibility parser for services that have not adopted strict startup validation.
    pub fn from_env() -> Self {
        Self::try_from_env().unwrap_or_default()
    }

    pub fn try_from_env() -> Result<Self, HealthConfigError> {
        Self::try_from_values(|name| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(HealthConfigError {
                variable: name,
                reason: "value is not valid Unicode",
            }),
        })
    }

    pub fn for_tests(startup_ms: u64, stability_ms: u64, stale_ms: u64) -> Self {
        Self {
            startup_convergence_window_ms: startup_ms.max(1),
            ready_stability_window_ms: stability_ms,
            dependency_stale_window_ms: stale_ms.max(1),
        }
    }

    pub fn validate_dependency_refresh_cadence(
        &self,
        refresh_variable: &'static str,
        maximum_refresh_interval: Duration,
    ) -> Result<(), HealthConfigError> {
        if u128::from(self.dependency_stale_window_ms) <= maximum_refresh_interval.as_millis() {
            return Err(HealthConfigError {
                variable: refresh_variable,
                reason: "refresh interval plus attempt timeout must be less than the dependency stale window",
            });
        }
        Ok(())
    }

    fn try_from_values(
        mut value: impl FnMut(&'static str) -> Result<Option<String>, HealthConfigError>,
    ) -> Result<Self, HealthConfigError> {
        let startup_secs = parse_seconds(
            "MYSERVER_STARTUP_CONVERGENCE_WINDOW_SECS",
            value("MYSERVER_STARTUP_CONVERGENCE_WINDOW_SECS")?,
            DEFAULT_STARTUP_CONVERGENCE_WINDOW_SECS,
            MAX_STARTUP_CONVERGENCE_WINDOW_SECS,
        )?;
        let stability_secs = parse_seconds(
            "MYSERVER_READY_STABILITY_WINDOW_SECS",
            value("MYSERVER_READY_STABILITY_WINDOW_SECS")?,
            DEFAULT_READY_STABILITY_WINDOW_SECS,
            MAX_READY_STABILITY_WINDOW_SECS,
        )?;
        let stale_secs = parse_seconds(
            "MYSERVER_DEPENDENCY_STALE_WINDOW_SECS",
            value("MYSERVER_DEPENDENCY_STALE_WINDOW_SECS")?,
            DEFAULT_DEPENDENCY_STALE_WINDOW_SECS,
            MAX_DEPENDENCY_STALE_WINDOW_SECS,
        )?;

        if stability_secs > startup_secs {
            return Err(HealthConfigError {
                variable: "MYSERVER_READY_STABILITY_WINDOW_SECS",
                reason: "must not exceed the startup convergence window",
            });
        }
        if stale_secs <= stability_secs {
            return Err(HealthConfigError {
                variable: "MYSERVER_DEPENDENCY_STALE_WINDOW_SECS",
                reason: "must be greater than the ready stability window",
            });
        }

        Ok(Self {
            startup_convergence_window_ms: seconds_to_millis(
                "MYSERVER_STARTUP_CONVERGENCE_WINDOW_SECS",
                startup_secs,
            )?,
            ready_stability_window_ms: seconds_to_millis(
                "MYSERVER_READY_STABILITY_WINDOW_SECS",
                stability_secs,
            )?,
            dependency_stale_window_ms: seconds_to_millis(
                "MYSERVER_DEPENDENCY_STALE_WINDOW_SECS",
                stale_secs,
            )?,
        })
    }
}

fn parse_seconds(
    variable: &'static str,
    value: Option<String>,
    default_secs: u64,
    max_secs: u64,
) -> Result<u64, HealthConfigError> {
    let Some(value) = value else {
        return Ok(default_secs);
    };
    let seconds = value
        .parse::<u64>()
        .map_err(|_| HealthConfigError {
            variable,
            reason: "must be an unsigned integer number of seconds",
        })?;
    if seconds == 0 {
        return Err(HealthConfigError {
            variable,
            reason: "must be greater than zero",
        });
    }
    if seconds > max_secs {
        return Err(HealthConfigError {
            variable,
            reason: "exceeds the supported upper bound",
        });
    }
    Ok(seconds)
}

fn seconds_to_millis(
    variable: &'static str,
    seconds: u64,
) -> Result<u64, HealthConfigError> {
    seconds.checked_mul(1_000).ok_or(HealthConfigError {
        variable,
        reason: "overflows millisecond representation",
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencySpec {
    pub dependency: String,
    pub endpoint: String,
    pub requirement: DependencyRequirement,
    pub stale_detection: bool,
}

impl DependencySpec {
    pub fn required(dependency: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self::new(dependency, endpoint, DependencyRequirement::Required, true)
    }

    pub fn optional(dependency: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self::new(dependency, endpoint, DependencyRequirement::Optional, true)
    }

    pub fn local_required(endpoint: impl Into<String>) -> Self {
        Self::new(
            "local-runtime",
            endpoint,
            DependencyRequirement::Required,
            false,
        )
    }

    pub fn required_without_stale_detection(
        dependency: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self::new(dependency, endpoint, DependencyRequirement::Required, false)
    }

    fn new(
        dependency: impl Into<String>,
        endpoint: impl Into<String>,
        requirement: DependencyRequirement,
        stale_detection: bool,
    ) -> Self {
        Self {
            dependency: dependency.into(),
            endpoint: endpoint.into(),
            requirement,
            stale_detection,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyStatus {
    Pending,
    Ready,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DependencySnapshot {
    pub dependency: String,
    pub endpoint: String,
    pub requirement: DependencyRequirement,
    pub status: DependencyStatus,
    pub error_code: Option<StartupErrorCode>,
    pub last_error_code: Option<StartupErrorCode>,
    pub retry_count: u64,
    pub last_success_at_ms: Option<u64>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HealthSnapshot {
    pub service: String,
    pub instance_id: String,
    pub state: StartupState,
    pub live: bool,
    pub ready: bool,
    pub elapsed_ms: u64,
    pub startup_timed_out: bool,
    pub dependencies: Vec<DependencySnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthMetricsSnapshot {
    pub state_transition_count: u64,
    pub startup_timeout_count: u64,
}

pub trait HealthClock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Debug)]
struct SystemHealthClock;

impl HealthClock for SystemHealthClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[derive(Clone)]
pub struct HealthState {
    inner: Arc<Mutex<HealthInner>>,
    clock: Arc<dyn HealthClock>,
    state_transition_count: Arc<AtomicU64>,
    startup_timeout_count: Arc<AtomicU64>,
}

struct HealthInner {
    service: String,
    instance_id: String,
    config: HealthConfig,
    started_at_ms: u64,
    state: StartupState,
    dependencies: BTreeMap<(String, String), DependencyRecord>,
    required_ready_since_ms: Option<u64>,
    ever_ready: bool,
    startup_timeout_emitted: bool,
    shutting_down: bool,
}

struct DependencyRecord {
    spec: DependencySpec,
    status: DependencyStatus,
    error_code: Option<StartupErrorCode>,
    last_error_code: Option<StartupErrorCode>,
    retry_count: u64,
    last_success_at_ms: Option<u64>,
    updated_at_ms: u64,
}

impl HealthState {
    pub fn new(
        service: impl Into<String>,
        instance_id: impl Into<String>,
        config: HealthConfig,
        dependencies: impl IntoIterator<Item = DependencySpec>,
    ) -> Self {
        Self::with_clock(
            service,
            instance_id,
            config,
            dependencies,
            Arc::new(SystemHealthClock),
        )
    }

    pub fn from_env(
        service: impl Into<String>,
        instance_id: impl Into<String>,
        dependencies: impl IntoIterator<Item = DependencySpec>,
    ) -> Self {
        Self::new(service, instance_id, HealthConfig::from_env(), dependencies)
    }

    pub fn try_from_env(
        service: impl Into<String>,
        instance_id: impl Into<String>,
        dependencies: impl IntoIterator<Item = DependencySpec>,
    ) -> Result<Self, HealthConfigError> {
        Ok(Self::new(
            service,
            instance_id,
            HealthConfig::try_from_env()?,
            dependencies,
        ))
    }

    pub fn with_clock(
        service: impl Into<String>,
        instance_id: impl Into<String>,
        config: HealthConfig,
        dependencies: impl IntoIterator<Item = DependencySpec>,
        clock: Arc<dyn HealthClock>,
    ) -> Self {
        let now_ms = clock.now_ms();
        let mut records = BTreeMap::new();
        for spec in dependencies {
            records.insert(
                (spec.dependency.clone(), spec.endpoint.clone()),
                DependencyRecord {
                    spec,
                    status: DependencyStatus::Pending,
                    error_code: Some(StartupErrorCode::DependencyPending),
                    last_error_code: Some(StartupErrorCode::DependencyPending),
                    retry_count: 0,
                    last_success_at_ms: None,
                    updated_at_ms: now_ms,
                },
            );
        }
        Self {
            inner: Arc::new(Mutex::new(HealthInner {
                service: service.into(),
                instance_id: instance_id.into(),
                config,
                started_at_ms: now_ms,
                state: StartupState::Starting,
                dependencies: records,
                required_ready_since_ms: None,
                ever_ready: false,
                startup_timeout_emitted: false,
                shutting_down: false,
            })),
            clock,
            state_transition_count: Arc::new(AtomicU64::new(0)),
            startup_timeout_count: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn mark_ready(&self, dependency: &str, endpoint: &str) -> bool {
        self.update(dependency, endpoint, DependencyStatus::Ready, None)
    }

    pub fn mark_pending(
        &self,
        dependency: &str,
        endpoint: &str,
        error_code: StartupErrorCode,
    ) -> bool {
        self.update(
            dependency,
            endpoint,
            DependencyStatus::Pending,
            Some(error_code),
        )
    }

    pub fn mark_degraded(
        &self,
        dependency: &str,
        endpoint: &str,
        error_code: StartupErrorCode,
    ) -> bool {
        self.update(
            dependency,
            endpoint,
            DependencyStatus::Degraded,
            Some(error_code),
        )
    }

    pub fn mark_failed(
        &self,
        dependency: &str,
        endpoint: &str,
        error_code: StartupErrorCode,
    ) -> bool {
        self.update(
            dependency,
            endpoint,
            DependencyStatus::Failed,
            Some(error_code),
        )
    }

    pub fn mark_shutting_down(&self) {
        let now_ms = self.clock.now_ms();
        let mut inner = self.inner.lock().expect("health state mutex poisoned");
        inner.shutting_down = true;
        self.recompute(&mut inner, now_ms);
    }

    pub fn snapshot(&self) -> HealthSnapshot {
        let now_ms = self.clock.now_ms();
        let mut inner = self.inner.lock().expect("health state mutex poisoned");
        self.recompute(&mut inner, now_ms)
    }

    pub fn metrics_snapshot(&self) -> HealthMetricsSnapshot {
        HealthMetricsSnapshot {
            state_transition_count: self.state_transition_count.load(Ordering::Relaxed),
            startup_timeout_count: self.startup_timeout_count.load(Ordering::Relaxed),
        }
    }

    fn update(
        &self,
        dependency: &str,
        endpoint: &str,
        status: DependencyStatus,
        error_code: Option<StartupErrorCode>,
    ) -> bool {
        let now_ms = self.clock.now_ms();
        let mut inner = self.inner.lock().expect("health state mutex poisoned");
        let Some(record) = inner
            .dependencies
            .get_mut(&(dependency.to_string(), endpoint.to_string()))
        else {
            return false;
        };
        record.status = status;
        record.error_code = error_code;
        if error_code.is_some() {
            record.last_error_code = error_code;
        }
        record.updated_at_ms = now_ms;
        if status == DependencyStatus::Ready {
            record.last_success_at_ms = Some(now_ms);
            record.retry_count = 0;
        } else {
            record.retry_count = record.retry_count.saturating_add(1);
        }
        self.recompute(&mut inner, now_ms);
        true
    }

    fn recompute(&self, inner: &mut HealthInner, now_ms: u64) -> HealthSnapshot {
        let previous_state = inner.state;
        let elapsed_ms = now_ms.saturating_sub(inner.started_at_ms);
        let effective: Vec<DependencySnapshot> = inner
            .dependencies
            .values()
            .map(|record| effective_dependency(record, &inner.config, now_ms))
            .collect();
        let required_blocked = effective.iter().any(|dependency| {
            dependency.requirement.blocks_readiness()
                && dependency.status != DependencyStatus::Ready
        });
        let optional_degraded = effective.iter().any(|dependency| {
            !dependency.requirement.blocks_readiness()
                && dependency.status != DependencyStatus::Ready
        });

        let ready = if inner.shutting_down {
            inner.required_ready_since_ms = None;
            inner.state = StartupState::ShuttingDown;
            false
        } else if required_blocked {
            inner.required_ready_since_ms = None;
            inner.state = if inner.ever_ready || inner.startup_timeout_emitted {
                StartupState::Degraded
            } else {
                StartupState::WaitingDependencies
            };
            false
        } else {
            let ready_since = *inner.required_ready_since_ms.get_or_insert(now_ms);
            let stable = now_ms.saturating_sub(ready_since)
                >= inner.config.ready_stability_window_ms;
            if stable {
                inner.ever_ready = true;
                inner.state = if optional_degraded {
                    StartupState::Degraded
                } else {
                    StartupState::Ready
                };
                true
            } else {
                inner.state = StartupState::WaitingDependencies;
                false
            }
        };

        if !ready
            && !inner.shutting_down
            && !inner.ever_ready
            && !inner.startup_timeout_emitted
            && elapsed_ms >= inner.config.startup_convergence_window_ms
        {
            inner.startup_timeout_emitted = true;
            inner.state = StartupState::Degraded;
            for record in inner.dependencies.values_mut() {
                if record.status != DependencyStatus::Ready
                    && record.error_code == Some(StartupErrorCode::DependencyPending)
                {
                    record.error_code = Some(StartupErrorCode::DependencyTimeout);
                    record.last_error_code = Some(StartupErrorCode::DependencyTimeout);
                    record.updated_at_ms = now_ms;
                }
            }
            self.startup_timeout_count.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                service = %inner.service,
                instance_id = %inner.instance_id,
                lifecycle_state = ?inner.state,
                error_code = StartupErrorCode::DependencyTimeout.as_str(),
                elapsed_ms,
                metric = "startup_convergence_timeout_total",
                metric_delta = 1,
                "startup convergence window exceeded"
            );
        }

        if previous_state != inner.state {
            self.state_transition_count.fetch_add(1, Ordering::Relaxed);
            tracing::info!(
                service = %inner.service,
                instance_id = %inner.instance_id,
                previous_state = ?previous_state,
                lifecycle_state = ?inner.state,
                ready,
                elapsed_ms,
                "application health state changed"
            );
        }

        let dependencies = inner
            .dependencies
            .values()
            .map(|record| {
                effective_dependency(record, &inner.config, now_ms)
            })
            .collect();
        HealthSnapshot {
            service: inner.service.clone(),
            instance_id: inner.instance_id.clone(),
            state: inner.state,
            live: !inner.shutting_down,
            ready,
            elapsed_ms,
            startup_timed_out: inner.startup_timeout_emitted,
            dependencies,
        }
    }
}

fn effective_dependency(
    record: &DependencyRecord,
    config: &HealthConfig,
    now_ms: u64,
) -> DependencySnapshot {
    let stale = record.spec.stale_detection
        && record.status == DependencyStatus::Ready
        && record
            .last_success_at_ms
            .is_some_and(|last| now_ms.saturating_sub(last) > config.dependency_stale_window_ms);
    let status = if stale {
        DependencyStatus::Degraded
    } else {
        record.status
    };
    let error_code = if stale {
        Some(StartupErrorCode::DependencyTimeout)
    } else {
        record.error_code
    };
    DependencySnapshot {
        dependency: record.spec.dependency.clone(),
        endpoint: record.spec.endpoint.clone(),
        requirement: record.spec.requirement,
        status,
        error_code,
        last_error_code: if stale {
            Some(StartupErrorCode::DependencyTimeout)
        } else {
            record.last_error_code
        },
        retry_count: record.retry_count,
        last_success_at_ms: record.last_success_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct ManualClock(AtomicU64);

    impl ManualClock {
        fn advance(&self, millis: u64) {
            self.0.fetch_add(millis, Ordering::Relaxed);
        }
    }

    impl HealthClock for ManualClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
    }

    fn state(specs: Vec<DependencySpec>) -> (HealthState, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::default());
        (
            HealthState::with_clock(
                "game-server",
                "game-1",
                HealthConfig::for_tests(100, 10, 50),
                specs,
                clock.clone(),
            ),
            clock,
        )
    }

    #[test]
    fn required_dependency_needs_stability_window_before_ready() {
        let (state, clock) = state(vec![DependencySpec::required("match-service", "grpc")]);
        assert!(state.mark_ready("match-service", "grpc"));
        assert!(!state.snapshot().ready);
        clock.advance(10);
        let snapshot = state.snapshot();
        assert!(snapshot.ready);
        assert_eq!(snapshot.state, StartupState::Ready);
    }

    #[test]
    fn optional_capability_degrades_without_blocking_readiness() {
        let (state, clock) = state(vec![
            DependencySpec::local_required("grpc-listener"),
            DependencySpec::optional("game-server", "internal"),
        ]);
        state.mark_ready("local-runtime", "grpc-listener");
        clock.advance(10);
        let snapshot = state.snapshot();
        assert!(snapshot.ready);
        assert_eq!(snapshot.state, StartupState::Degraded);
    }

    #[test]
    fn startup_timeout_is_counted_once_and_recovery_still_works() {
        let (state, clock) = state(vec![DependencySpec::required("match-service", "grpc")]);
        clock.advance(100);
        assert_eq!(state.snapshot().state, StartupState::Degraded);
        assert_eq!(state.snapshot().state, StartupState::Degraded);
        assert_eq!(state.metrics_snapshot().startup_timeout_count, 1);

        state.mark_ready("match-service", "grpc");
        clock.advance(10);
        assert!(state.snapshot().ready);
    }

    #[test]
    fn startup_timeout_preserves_non_pending_error_codes() {
        let (state, clock) = state(vec![
            DependencySpec::required("match-service", "grpc"),
            DependencySpec::optional("service-registry", "lookup"),
            DependencySpec::local_required("worker-lease"),
            DependencySpec::optional("local-socket", "bind"),
            DependencySpec::optional("bootstrap", "phase"),
        ]);
        state.mark_degraded(
            "service-registry",
            "lookup",
            StartupErrorCode::RegistryUnavailable,
        );
        state.mark_failed(
            "local-runtime",
            "worker-lease",
            StartupErrorCode::LeaseLost,
        );
        state.mark_failed(
            "local-socket",
            "bind",
            StartupErrorCode::SocketConflict,
        );
        state.mark_failed(
            "bootstrap",
            "phase",
            StartupErrorCode::StartupPhaseFailure,
        );
        clock.advance(100);

        let snapshot = state.snapshot();
        let error_for = |dependency: &str| {
            snapshot
                .dependencies
                .iter()
                .find(|entry| entry.dependency == dependency)
                .and_then(|entry| entry.error_code)
        };
        assert_eq!(
            error_for("local-runtime"),
            Some(StartupErrorCode::LeaseLost)
        );
        assert_eq!(
            error_for("match-service"),
            Some(StartupErrorCode::DependencyTimeout)
        );
        assert_eq!(
            error_for("service-registry"),
            Some(StartupErrorCode::RegistryUnavailable)
        );
        assert_eq!(
            error_for("local-socket"),
            Some(StartupErrorCode::SocketConflict)
        );
        assert_eq!(
            error_for("bootstrap"),
            Some(StartupErrorCode::StartupPhaseFailure)
        );
    }

    #[test]
    fn dependency_loss_after_timeout_recovery_keeps_the_new_error_code() {
        let (state, clock) = state(vec![DependencySpec::required("match-service", "grpc")]);
        clock.advance(100);
        assert_eq!(
            state.snapshot().dependencies[0].error_code,
            Some(StartupErrorCode::DependencyTimeout)
        );
        state.mark_ready("match-service", "grpc");
        clock.advance(10);
        assert!(state.snapshot().ready);

        state.mark_pending(
            "match-service",
            "grpc",
            StartupErrorCode::DependencyPending,
        );
        assert_eq!(
            state.snapshot().dependencies[0].error_code,
            Some(StartupErrorCode::DependencyPending)
        );

        state.mark_degraded(
            "match-service",
            "grpc",
            StartupErrorCode::RegistryUnavailable,
        );
        assert_eq!(
            state.snapshot().dependencies[0].error_code,
            Some(StartupErrorCode::RegistryUnavailable)
        );
    }

    #[test]
    fn runtime_dependency_loss_is_not_reclassified_as_startup_timeout() {
        let (state, clock) = state(vec![DependencySpec::required("match-service", "grpc")]);
        state.mark_ready("match-service", "grpc");
        clock.advance(10);
        assert!(state.snapshot().ready);
        clock.advance(100);

        state.mark_pending(
            "match-service",
            "grpc",
            StartupErrorCode::DependencyPending,
        );
        let snapshot = state.snapshot();
        assert_eq!(
            snapshot.dependencies[0].error_code,
            Some(StartupErrorCode::DependencyPending)
        );
        assert!(!snapshot.startup_timed_out);
        assert_eq!(state.metrics_snapshot().startup_timeout_count, 0);
    }

    #[test]
    fn recovery_retains_the_last_structured_error_type() {
        let (state, clock) = state(vec![DependencySpec::required("match-service", "grpc")]);
        state.mark_degraded(
            "match-service",
            "grpc",
            StartupErrorCode::RegistryUnavailable,
        );
        state.mark_ready("match-service", "grpc");
        clock.advance(10);

        let dependency = &state.snapshot().dependencies[0];
        assert_eq!(dependency.error_code, None);
        assert_eq!(
            dependency.last_error_code,
            Some(StartupErrorCode::RegistryUnavailable)
        );
    }

    #[test]
    fn stale_required_dependency_drops_readiness_until_stable_recovery() {
        let (state, clock) = state(vec![DependencySpec::required("match-service", "grpc")]);
        state.mark_ready("match-service", "grpc");
        clock.advance(10);
        assert!(state.snapshot().ready);
        clock.advance(51);
        let stale = state.snapshot();
        assert!(!stale.ready);
        assert_eq!(stale.state, StartupState::Degraded);
        assert_eq!(stale.dependencies[0].error_code, Some(StartupErrorCode::DependencyTimeout));

        state.mark_ready("match-service", "grpc");
        clock.advance(10);
        assert!(state.snapshot().ready);
    }

    #[test]
    fn serialized_snapshot_contains_no_connection_or_credential_fields() {
        let (state, _) = state(vec![DependencySpec::required("match-service", "grpc")]);
        let json = serde_json::to_string(&state.snapshot()).unwrap();
        for forbidden in ["url", "host", "port", "socket", "token", "password", "error_message"] {
            assert!(!json.contains(forbidden));
        }
    }

    #[test]
    fn strict_config_uses_defaults_only_for_missing_values() {
        let config = HealthConfig::try_from_values(|_| Ok(None)).unwrap();
        assert_eq!(config, HealthConfig::default());

        for invalid in ["not-a-number", "0", "18446744073709551615"] {
            let error = HealthConfig::try_from_values(|name| {
                Ok((name == "MYSERVER_STARTUP_CONVERGENCE_WINDOW_SECS")
                    .then(|| invalid.to_string()))
            })
            .unwrap_err();
            assert_eq!(
                error.variable,
                "MYSERVER_STARTUP_CONVERGENCE_WINDOW_SECS"
            );
        }
        assert!(
            seconds_to_millis(
                "MYSERVER_STARTUP_CONVERGENCE_WINDOW_SECS",
                u64::MAX
            )
            .is_err()
        );
    }

    #[test]
    fn strict_config_rejects_unsafe_window_relationships() {
        let values = BTreeMap::from([
            (
                "MYSERVER_STARTUP_CONVERGENCE_WINDOW_SECS",
                "10".to_string(),
            ),
            (
                "MYSERVER_READY_STABILITY_WINDOW_SECS",
                "11".to_string(),
            ),
            (
                "MYSERVER_DEPENDENCY_STALE_WINDOW_SECS",
                "12".to_string(),
            ),
        ]);
        assert!(
            HealthConfig::try_from_values(|name| Ok(values.get(name).cloned())).is_err()
        );

        let values = BTreeMap::from([
            (
                "MYSERVER_READY_STABILITY_WINDOW_SECS",
                "60".to_string(),
            ),
            (
                "MYSERVER_DEPENDENCY_STALE_WINDOW_SECS",
                "60".to_string(),
            ),
        ]);
        assert!(
            HealthConfig::try_from_values(|name| Ok(values.get(name).cloned())).is_err()
        );
    }

    #[test]
    fn dependency_stale_window_must_cover_normal_refresh_and_timeout() {
        let config = HealthConfig::for_tests(120_000, 10_000, 36_000);
        config
            .validate_dependency_refresh_cadence(
                "MATCH_SERVICE_REDISCOVERY_INTERVAL_SECS",
                Duration::from_secs(35),
            )
            .unwrap();

        let error = HealthConfig::for_tests(120_000, 10_000, 35_000)
            .validate_dependency_refresh_cadence(
                "MATCH_SERVICE_REDISCOVERY_INTERVAL_SECS",
                Duration::from_secs(35),
            )
            .unwrap_err();
        assert_eq!(error.variable, "MATCH_SERVICE_REDISCOVERY_INTERVAL_SECS");
        let message = error.to_string();
        assert!(message.contains("dependency stale window"));
        for forbidden in ["url", "host", "port", "socket", "token", "password"] {
            assert!(!message.to_ascii_lowercase().contains(forbidden));
        }
    }
}
