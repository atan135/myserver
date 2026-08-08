use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::sync::RwLock;

use crate::StartupErrorCode;

const DEFAULT_RETRY_INITIAL: Duration = Duration::from_millis(250);
const DEFAULT_RETRY_MAX: Duration = Duration::from_secs(10);
const DEFAULT_STEADY_INTERVAL: Duration = Duration::from_secs(10);
const DEFAULT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_JITTER_PERCENT: u8 = 20;
static JITTER_SEED_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConvergenceConfig {
    pub retry_initial: Duration,
    pub retry_max: Duration,
    pub steady_interval: Duration,
    pub attempt_timeout: Duration,
    pub jitter_percent: u8,
}

impl Default for ConvergenceConfig {
    fn default() -> Self {
        Self {
            retry_initial: DEFAULT_RETRY_INITIAL,
            retry_max: DEFAULT_RETRY_MAX,
            steady_interval: DEFAULT_STEADY_INTERVAL,
            attempt_timeout: DEFAULT_ATTEMPT_TIMEOUT,
            jitter_percent: DEFAULT_JITTER_PERCENT,
        }
    }
}

impl ConvergenceConfig {
    fn normalized(self) -> Self {
        let retry_initial = nonzero_duration(self.retry_initial);
        Self {
            retry_initial,
            retry_max: self.retry_max.max(retry_initial),
            steady_interval: nonzero_duration(self.steady_interval),
            attempt_timeout: nonzero_duration(self.attempt_timeout),
            jitter_percent: self.jitter_percent.min(50),
        }
    }

    pub fn maximum_success_refresh_interval(self) -> Duration {
        let config = self.normalized();
        config
            .steady_interval
            .saturating_add(config.attempt_timeout)
    }
}

fn nonzero_duration(duration: Duration) -> Duration {
    if duration.is_zero() {
        Duration::from_millis(1)
    } else {
        duration
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConvergenceAttempt {
    Converged,
    Retry(StartupErrorCode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConvergencePhase {
    Pending,
    Ready,
    Degraded,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConvergenceSnapshot {
    pub phase: ConvergencePhase,
    pub last_error_code: Option<StartupErrorCode>,
    pub consecutive_failures: u32,
    pub attempts: u64,
    pub next_delay_ms: u64,
}

impl Default for ConvergenceSnapshot {
    fn default() -> Self {
        Self {
            phase: ConvergencePhase::Pending,
            last_error_code: None,
            consecutive_failures: 0,
            attempts: 0,
            next_delay_ms: 0,
        }
    }
}

pub trait ConvergenceJitter: Send + Sync {
    fn apply(&self, base: Duration, maximum: Duration, attempt: u32) -> Duration;
}

#[derive(Debug, Default)]
struct ConvergenceProgress {
    ever_converged: bool,
    consecutive_failures: u32,
}

impl ConvergenceProgress {
    fn record(
        &mut self,
        result: ConvergenceAttempt,
        config: &ConvergenceConfig,
        jitter: &dyn ConvergenceJitter,
    ) -> (ConvergencePhase, Option<StartupErrorCode>, Duration) {
        match result {
            ConvergenceAttempt::Converged => {
                self.ever_converged = true;
                self.consecutive_failures = 0;
                (ConvergencePhase::Ready, None, config.steady_interval)
            }
            ConvergenceAttempt::Retry(error_code) => {
                let delay = bounded_exponential_delay(
                    config.retry_initial,
                    config.retry_max,
                    self.consecutive_failures,
                );
                let delay = jitter
                    .apply(delay, config.retry_max, self.consecutive_failures)
                    .min(config.retry_max);
                self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                (
                    if self.ever_converged {
                        ConvergencePhase::Degraded
                    } else {
                        ConvergencePhase::Pending
                    },
                    Some(error_code),
                    nonzero_duration(delay),
                )
            }
        }
    }
}

#[derive(Debug)]
struct PercentageJitter {
    percent: u8,
    state: AtomicU64,
}

impl PercentageJitter {
    fn new(percent: u8) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0);
        let seed = nanos
            ^ u64::from(std::process::id()).rotate_left(17)
            ^ JITTER_SEED_COUNTER
                .fetch_add(1, Ordering::Relaxed)
                .rotate_left(31);
        Self {
            percent,
            state: AtomicU64::new(seed.max(1)),
        }
    }

    fn next(&self) -> u64 {
        let mut current = self.state.load(Ordering::Relaxed);
        loop {
            let mut next = current;
            next ^= next << 13;
            next ^= next >> 7;
            next ^= next << 17;
            next = next.max(1);
            match self.state.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(actual) => current = actual,
            }
        }
    }
}

impl ConvergenceJitter for PercentageJitter {
    fn apply(&self, base: Duration, maximum: Duration, _attempt: u32) -> Duration {
        if self.percent == 0 {
            return base.min(maximum);
        }
        let spread = u64::from(self.percent) * 2 + 1;
        let offset = (self.next() % spread) as i64 - i64::from(self.percent);
        scale_duration(base, 100 + offset, maximum)
    }
}

fn scale_duration(base: Duration, percent: i64, maximum: Duration) -> Duration {
    let millis = base
        .as_millis()
        .saturating_mul(percent.max(0) as u128)
        .saturating_div(100)
        .min(maximum.as_millis())
        .min(u128::from(u64::MAX));
    Duration::from_millis(millis as u64)
}

pub struct ConvergenceTask {
    snapshot: Arc<RwLock<ConvergenceSnapshot>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ConvergenceTask {
    pub async fn snapshot(&self) -> ConvergenceSnapshot {
        self.snapshot.read().await.clone()
    }

    pub fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    pub async fn stop_and_wait(self) {
        let _ = self.stop_and_wait_result().await;
    }

    pub async fn stop_and_wait_result(mut self) -> Result<(), tokio::task::JoinError> {
        if let Some(task) = self.task.take() {
            task.abort();
            task.await
        } else {
            Ok(())
        }
    }
}

impl Drop for ConvergenceTask {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub fn spawn_convergence<F, Fut>(config: ConvergenceConfig, attempt: F) -> ConvergenceTask
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ConvergenceAttempt> + Send + 'static,
{
    let jitter = Arc::new(PercentageJitter::new(config.jitter_percent.min(50)));
    spawn_convergence_with_jitter(config, jitter, attempt)
}

pub fn spawn_convergence_with_jitter<F, Fut>(
    config: ConvergenceConfig,
    jitter: Arc<dyn ConvergenceJitter>,
    attempt: F,
) -> ConvergenceTask
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ConvergenceAttempt> + Send + 'static,
{
    let config = config.normalized();
    let snapshot = Arc::new(RwLock::new(ConvergenceSnapshot::default()));
    let task_snapshot = Arc::clone(&snapshot);
    let task = tokio::spawn(async move {
        let mut progress = ConvergenceProgress::default();
        loop {
            let result = tokio::time::timeout(config.attempt_timeout, attempt())
                .await
                .unwrap_or(ConvergenceAttempt::Retry(
                    StartupErrorCode::DependencyTimeout,
                ));

            let (phase, error_code, next_delay) = progress.record(result, &config, jitter.as_ref());

            {
                let mut current = task_snapshot.write().await;
                current.phase = phase;
                current.last_error_code = error_code;
                current.consecutive_failures = progress.consecutive_failures;
                current.attempts = current.attempts.saturating_add(1);
                current.next_delay_ms = duration_millis(next_delay);
            }
            tokio::time::sleep(next_delay).await;
        }
    });

    ConvergenceTask {
        snapshot,
        task: Some(task),
    }
}

fn bounded_exponential_delay(initial: Duration, maximum: Duration, attempt: u32) -> Duration {
    let multiplier = 1_u128 << attempt.min(63);
    let millis = initial
        .as_millis()
        .saturating_mul(multiplier)
        .min(maximum.as_millis())
        .min(u128::from(u64::MAX));
    Duration::from_millis(millis as u64)
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FixedJitter(Duration);

    impl ConvergenceJitter for FixedJitter {
        fn apply(&self, _base: Duration, maximum: Duration, _attempt: u32) -> Duration {
            self.0.min(maximum)
        }
    }

    #[derive(Debug)]
    struct NoJitter;

    impl ConvergenceJitter for NoJitter {
        fn apply(&self, base: Duration, maximum: Duration, _attempt: u32) -> Duration {
            base.min(maximum)
        }
    }

    #[test]
    fn exponential_delay_is_bounded_and_saturating() {
        let initial = Duration::from_millis(250);
        let maximum = Duration::from_secs(10);
        assert_eq!(bounded_exponential_delay(initial, maximum, 0), initial);
        assert_eq!(
            bounded_exponential_delay(initial, maximum, 1),
            Duration::from_millis(500)
        );
        assert_eq!(bounded_exponential_delay(initial, maximum, 6), maximum);
        assert_eq!(
            bounded_exponential_delay(initial, maximum, u32::MAX),
            maximum
        );
    }

    #[test]
    fn percentage_scaling_never_exceeds_maximum() {
        let maximum = Duration::from_secs(10);
        assert_eq!(
            scale_duration(Duration::from_secs(5), 80, maximum),
            Duration::from_secs(4)
        );
        assert_eq!(
            scale_duration(Duration::from_secs(10), 120, maximum),
            maximum
        );
    }

    #[test]
    fn maximum_success_refresh_interval_includes_attempt_timeout() {
        let config = ConvergenceConfig {
            steady_interval: Duration::from_secs(30),
            attempt_timeout: Duration::from_secs(5),
            ..ConvergenceConfig::default()
        };

        assert_eq!(
            config.maximum_success_refresh_interval(),
            Duration::from_secs(35)
        );
    }

    #[test]
    fn convergence_progress_unifies_initial_loss_and_runtime_recovery() {
        let config = ConvergenceConfig::default().normalized();
        let mut progress = ConvergenceProgress::default();

        let (phase, error, delay) = progress.record(
            ConvergenceAttempt::Retry(StartupErrorCode::DependencyPending),
            &config,
            &NoJitter,
        );
        assert_eq!(phase, ConvergencePhase::Pending);
        assert_eq!(error, Some(StartupErrorCode::DependencyPending));
        assert_eq!(delay, config.retry_initial);

        let (phase, error, delay) =
            progress.record(ConvergenceAttempt::Converged, &config, &NoJitter);
        assert_eq!(phase, ConvergencePhase::Ready);
        assert_eq!(error, None);
        assert_eq!(delay, config.steady_interval);

        let (phase, _, delay) = progress.record(
            ConvergenceAttempt::Retry(StartupErrorCode::RegistryUnavailable),
            &config,
            &NoJitter,
        );
        assert_eq!(phase, ConvergencePhase::Degraded);
        assert_eq!(delay, config.retry_initial);
        assert_eq!(progress.consecutive_failures, 1);

        let (phase, _, _) = progress.record(ConvergenceAttempt::Converged, &config, &NoJitter);
        assert_eq!(phase, ConvergencePhase::Ready);
        assert_eq!(progress.consecutive_failures, 0);
    }

    #[test]
    fn custom_jitter_cannot_escape_retry_bound() {
        let config = ConvergenceConfig::default().normalized();
        let mut progress = ConvergenceProgress::default();
        let (_, _, delay) = progress.record(
            ConvergenceAttempt::Retry(StartupErrorCode::DependencyPending),
            &config,
            &FixedJitter(Duration::from_secs(3600)),
        );
        assert_eq!(delay, config.retry_max);
    }

    #[tokio::test]
    async fn runner_attempts_immediately_and_reports_safe_snapshot() {
        let task = spawn_convergence_with_jitter(
            ConvergenceConfig {
                retry_initial: Duration::from_secs(60),
                retry_max: Duration::from_secs(60),
                steady_interval: Duration::from_secs(60),
                attempt_timeout: Duration::from_secs(1),
                jitter_percent: 0,
            },
            Arc::new(FixedJitter(Duration::from_secs(60))),
            || async { ConvergenceAttempt::Retry(StartupErrorCode::RegistryUnavailable) },
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            while task.snapshot().await.attempts == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let snapshot = task.snapshot().await;
        assert_eq!(snapshot.phase, ConvergencePhase::Pending);
        assert_eq!(
            snapshot.last_error_code,
            Some(StartupErrorCode::RegistryUnavailable)
        );
        let json = serde_json::to_string(&snapshot).unwrap();
        for forbidden in ["url", "host", "port", "socket", "token", "password"] {
            assert!(!json.to_ascii_lowercase().contains(forbidden));
        }
    }

    #[tokio::test]
    async fn attempt_timeout_is_a_retryable_safe_code() {
        let task = spawn_convergence_with_jitter(
            ConvergenceConfig {
                retry_initial: Duration::from_secs(60),
                retry_max: Duration::from_secs(60),
                steady_interval: Duration::from_secs(60),
                attempt_timeout: Duration::from_millis(1),
                jitter_percent: 0,
            },
            Arc::new(FixedJitter(Duration::from_secs(60))),
            || async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                ConvergenceAttempt::Converged
            },
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = task.snapshot().await;
                if snapshot.attempts > 0 {
                    assert_eq!(
                        snapshot.last_error_code,
                        Some(StartupErrorCode::DependencyTimeout)
                    );
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
}
