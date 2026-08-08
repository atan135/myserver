use std::fmt;
use std::future::Future;
use std::time::Duration;

use service_registry::StartupErrorCode;

const DEFAULT_WAIT_TIMEOUT_SECS: u64 = 120;
const DEFAULT_RETRY_INITIAL_MS: u64 = 250;
const DEFAULT_RETRY_MAX_MS: u64 = 5_000;
const MAX_WAIT_TIMEOUT_SECS: u64 = 600;
const MAX_RETRY_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeaseWaitConfig {
    pub timeout: Duration,
    pub retry_initial: Duration,
    pub retry_max: Duration,
}

impl LeaseWaitConfig {
    pub fn try_from_env() -> Result<Self, LeaseWaitConfigError> {
        Self::try_from_values(
            std::env::var("GLOBAL_ID_WORKER_LEASE_WAIT_TIMEOUT_SECS").ok(),
            std::env::var("GLOBAL_ID_WORKER_LEASE_RETRY_INITIAL_MS").ok(),
            std::env::var("GLOBAL_ID_WORKER_LEASE_RETRY_MAX_MS").ok(),
        )
    }

    fn try_from_values(
        timeout_secs: Option<String>,
        retry_initial_ms: Option<String>,
        retry_max_ms: Option<String>,
    ) -> Result<Self, LeaseWaitConfigError> {
        let timeout_secs = parse_bounded(
            "GLOBAL_ID_WORKER_LEASE_WAIT_TIMEOUT_SECS",
            timeout_secs,
            DEFAULT_WAIT_TIMEOUT_SECS,
            1,
            MAX_WAIT_TIMEOUT_SECS,
        )?;
        let retry_initial_ms = parse_bounded(
            "GLOBAL_ID_WORKER_LEASE_RETRY_INITIAL_MS",
            retry_initial_ms,
            DEFAULT_RETRY_INITIAL_MS,
            1,
            MAX_RETRY_MS,
        )?;
        let retry_max_ms = parse_bounded(
            "GLOBAL_ID_WORKER_LEASE_RETRY_MAX_MS",
            retry_max_ms,
            DEFAULT_RETRY_MAX_MS,
            1,
            MAX_RETRY_MS,
        )?;
        if retry_initial_ms > retry_max_ms {
            return Err(LeaseWaitConfigError(
                "GLOBAL_ID_WORKER_LEASE_RETRY_INITIAL_MS must not exceed GLOBAL_ID_WORKER_LEASE_RETRY_MAX_MS"
                    .to_string(),
            ));
        }
        Ok(Self {
            timeout: Duration::from_secs(timeout_secs),
            retry_initial: Duration::from_millis(retry_initial_ms),
            retry_max: Duration::from_millis(retry_max_ms),
        })
    }
}

fn parse_bounded(
    name: &str,
    value: Option<String>,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<u64, LeaseWaitConfigError> {
    let Some(value) = value else {
        return Ok(default);
    };
    let parsed = value.trim().parse::<u64>().map_err(|_| {
        LeaseWaitConfigError(format!(
            "{name} must be an integer in {minimum}..={maximum}"
        ))
    })?;
    if !(minimum..=maximum).contains(&parsed) {
        return Err(LeaseWaitConfigError(format!(
            "{name} must be in {minimum}..={maximum}"
        )));
    }
    Ok(parsed)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseWaitConfigError(String);

impl fmt::Display for LeaseWaitConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for LeaseWaitConfigError {}

#[derive(Debug, PartialEq, Eq)]
pub enum LeaseWaitError<E> {
    TimedOut {
        attempts: u64,
        last_error: Option<E>,
    },
    Cancelled {
        attempts: u64,
    },
}

impl<E: fmt::Display> fmt::Display for LeaseWaitError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut { attempts, .. } => {
                write!(
                    formatter,
                    "worker lease wait timed out after {attempts} attempts"
                )
            }
            Self::Cancelled { attempts } => {
                write!(
                    formatter,
                    "worker lease wait cancelled after {attempts} attempts"
                )
            }
        }
    }
}

pub async fn wait_for_worker_lease<T, E, Attempt, AttemptFuture, Cancel>(
    config: LeaseWaitConfig,
    mut attempt: Attempt,
    cancel: Cancel,
) -> Result<T, LeaseWaitError<E>>
where
    Attempt: FnMut() -> AttemptFuture,
    AttemptFuture: Future<Output = Result<T, E>>,
    Cancel: Future<Output = ()>,
{
    tokio::pin!(cancel);
    let started_at = tokio::time::Instant::now();
    let deadline = started_at + config.timeout;
    let mut attempts = 0_u64;
    let mut retry_delay = config.retry_initial;

    loop {
        attempts = attempts.saturating_add(1);
        let attempt_future = attempt();
        tokio::pin!(attempt_future);
        let error = match tokio::select! {
            result = &mut attempt_future => Some(result),
            _ = tokio::time::sleep_until(deadline) => None,
            _ = &mut cancel => return Err(LeaseWaitError::Cancelled { attempts }),
        } {
            Some(Ok(value)) => return Ok(value),
            Some(Err(error)) => error,
            None => {
                return Err(LeaseWaitError::TimedOut {
                    attempts,
                    last_error: None,
                });
            }
        };
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(LeaseWaitError::TimedOut {
                attempts,
                last_error: Some(error),
            });
        }

        let sleep_for = retry_delay.min(deadline.saturating_duration_since(now));
        tracing::warn!(
            error_code = "LEASE_UNAVAILABLE",
            retry_count = attempts,
            next_delay_ms = sleep_for.as_millis() as u64,
            elapsed_ms = now.saturating_duration_since(started_at).as_millis() as u64,
            "global id worker lease unavailable; waiting for ownership"
        );
        tokio::select! {
            _ = tokio::time::sleep(sleep_for) => {}
            _ = &mut cancel => return Err(LeaseWaitError::Cancelled { attempts }),
        }
        retry_delay = retry_delay.saturating_mul(2).min(config.retry_max);
    }
}

pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("failed to install SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnedResource {
    WorkerLease,
    NetworkListeners,
    LocalSockets,
}

#[derive(Debug, Default)]
pub struct StartupOwnership {
    lease_owned: bool,
}

impl StartupOwnership {
    pub fn claim(&mut self, resource: OwnedResource) -> Result<(), StartupOwnershipError> {
        if matches!(
            resource,
            OwnedResource::NetworkListeners | OwnedResource::LocalSockets
        ) && !self.lease_owned
        {
            return Err(StartupOwnershipError(resource));
        }
        if resource == OwnedResource::WorkerLease {
            self.lease_owned = true;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartupOwnershipError(OwnedResource);

impl fmt::Display for StartupOwnershipError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "cannot acquire {:?} before worker lease ownership",
            self.0
        )
    }
}

impl std::error::Error for StartupOwnershipError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupStep {
    StopBackgroundTasks,
    ReleaseListenersAndSockets,
    DeregisterInstance,
    ReleaseWorkerLease,
    CloseStores,
}

const CLEANUP_STEPS: [CleanupStep; 5] = [
    CleanupStep::StopBackgroundTasks,
    CleanupStep::ReleaseListenersAndSockets,
    CleanupStep::DeregisterInstance,
    CleanupStep::ReleaseWorkerLease,
    CleanupStep::CloseStores,
];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub attempted: Vec<CleanupStep>,
    pub failures: Vec<(CleanupStep, String)>,
}

pub trait CleanupExecutor {
    async fn execute(&mut self, step: CleanupStep) -> Result<(), String>;
}

pub async fn run_cleanup<E: CleanupExecutor>(executor: &mut E) -> CleanupReport {
    let mut report = CleanupReport::default();
    for step in CLEANUP_STEPS {
        report.attempted.push(step);
        if let Err(error) = executor.execute(step).await {
            report.failures.push((step, error));
        }
    }
    report
}

pub async fn run_then_cleanup<T, E, Executor, Run>(
    run: Run,
    executor: &mut Executor,
) -> (Result<T, E>, CleanupReport)
where
    Executor: CleanupExecutor,
    Run: Future<Output = Result<T, E>>,
{
    let run_result = run.await;
    let cleanup_report = run_cleanup(executor).await;
    (run_result, cleanup_report)
}

pub fn match_pending_is_recoverable(error_code: StartupErrorCode) -> bool {
    error_code == StartupErrorCode::DependencyPending
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn invalid_lease_wait_config_is_rejected_before_bootstrap() {
        let error = LeaseWaitConfig::try_from_values(
            Some("0".to_string()),
            Some("5000".to_string()),
            Some("250".to_string()),
        )
        .expect_err("invalid wait configuration must be fatal");
        assert!(error.to_string().contains("WAIT_TIMEOUT"));
    }

    #[tokio::test]
    async fn lease_wait_retries_until_previous_owner_releases() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&attempts);
        let result = wait_for_worker_lease(
            LeaseWaitConfig {
                timeout: Duration::from_secs(1),
                retry_initial: Duration::from_millis(1),
                retry_max: Duration::from_millis(4),
            },
            move || {
                let attempt = observed.fetch_add(1, Ordering::SeqCst);
                async move {
                    if attempt < 2 {
                        Err("occupied")
                    } else {
                        Ok("lease")
                    }
                }
            },
            std::future::pending(),
        )
        .await;
        assert_eq!(result, Ok("lease"));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn shutdown_interrupts_lease_wait_without_listener_ownership() {
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(wait_for_worker_lease(
            LeaseWaitConfig {
                timeout: Duration::from_secs(1),
                retry_initial: Duration::from_millis(50),
                retry_max: Duration::from_millis(50),
            },
            || async { Err::<(), _>("occupied") },
            async move {
                let _ = cancel_rx.await;
            },
        ));
        tokio::task::yield_now().await;
        cancel_tx.send(()).unwrap();
        assert_eq!(
            task.await.unwrap(),
            Err(LeaseWaitError::Cancelled { attempts: 1 })
        );
    }

    #[tokio::test]
    async fn lease_wait_deadline_bounds_a_hanging_attempt() {
        let result = wait_for_worker_lease(
            LeaseWaitConfig {
                timeout: Duration::from_millis(10),
                retry_initial: Duration::from_millis(1),
                retry_max: Duration::from_millis(1),
            },
            || async {
                std::future::pending::<()>().await;
                Ok::<(), &str>(())
            },
            std::future::pending(),
        )
        .await;
        assert_eq!(
            result,
            Err(LeaseWaitError::TimedOut {
                attempts: 1,
                last_error: None,
            })
        );
    }

    #[test]
    fn listener_and_socket_claims_require_worker_lease() {
        let mut ownership = StartupOwnership::default();
        assert!(ownership.claim(OwnedResource::NetworkListeners).is_err());
        assert!(ownership.claim(OwnedResource::LocalSockets).is_err());
        ownership.claim(OwnedResource::WorkerLease).unwrap();
        ownership.claim(OwnedResource::NetworkListeners).unwrap();
        ownership.claim(OwnedResource::LocalSockets).unwrap();
    }

    struct InjectedCleanup {
        failures: VecDeque<CleanupStep>,
    }

    impl CleanupExecutor for InjectedCleanup {
        async fn execute(&mut self, step: CleanupStep) -> Result<(), String> {
            if self.failures.front() == Some(&step) {
                self.failures.pop_front();
                Err(format!("injected {step:?} failure"))
            } else {
                Ok(())
            }
        }
    }

    async fn assert_run_error_is_preserved_and_cleanup_completes(expected: &str) {
        let mut cleanup = InjectedCleanup {
            failures: VecDeque::from([CleanupStep::ReleaseWorkerLease]),
        };
        let run = async { Err::<(), _>(expected.to_string()) };

        let (run_result, report) = run_then_cleanup(run, &mut cleanup).await;

        assert_eq!(run_result.unwrap_err(), expected);
        assert_eq!(report.attempted, CLEANUP_STEPS);
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].0, CleanupStep::ReleaseWorkerLease);
    }

    #[tokio::test]
    async fn database_initialization_error_runs_full_cleanup_and_remains_primary() {
        assert_run_error_is_preserved_and_cleanup_completes("database initialization failed").await;
    }

    #[tokio::test]
    async fn readiness_bind_error_runs_full_cleanup_and_remains_primary() {
        assert_run_error_is_preserved_and_cleanup_completes("readiness bind failed").await;
    }

    #[tokio::test]
    async fn shutdown_interruption_runs_full_cleanup_and_remains_primary() {
        assert_run_error_is_preserved_and_cleanup_completes("shutdown interrupted active run")
            .await;
    }

    #[tokio::test]
    async fn lease_release_failure_does_not_skip_store_close() {
        let mut cleanup = InjectedCleanup {
            failures: VecDeque::from([CleanupStep::ReleaseWorkerLease]),
        };
        let report = run_cleanup(&mut cleanup).await;
        assert_eq!(report.attempted.last(), Some(&CleanupStep::CloseStores));
        assert_eq!(report.failures[0].0, CleanupStep::ReleaseWorkerLease);
    }

    #[test]
    fn match_pending_is_not_a_bootstrap_failure() {
        assert!(match_pending_is_recoverable(
            StartupErrorCode::DependencyPending
        ));
        assert!(!match_pending_is_recoverable(
            StartupErrorCode::StartupPhaseFailure
        ));
    }
}
