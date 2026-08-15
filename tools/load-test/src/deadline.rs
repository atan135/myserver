use std::time::{Duration, Instant};

/// A wall-clock deadline could not be represented safely by the monotonic
/// clock used to enforce it at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MonotonicDeadlineError {
    #[error("deadline has already elapsed")]
    Elapsed,
    #[error("deadline cannot be represented by the monotonic clock")]
    Unrepresentable,
}

/// Converts an absolute Unix-millisecond deadline to a monotonic deadline.
///
/// Sample the monotonic clock before the wall clock, so scheduling can only
/// expire slightly early while the values are being observed. The conversion
/// never turns an elapsed deadline into an unbounded future duration.
pub fn monotonic_deadline_from_unix_ms(
    deadline_unix_ms: u64,
    now_unix_ms: u64,
    monotonic_now: Instant,
) -> Result<Instant, MonotonicDeadlineError> {
    monotonic_deadline_from_unix_ms_with(
        deadline_unix_ms,
        now_unix_ms,
        monotonic_now,
        |instant, duration| instant.checked_add(duration),
    )
}

fn monotonic_deadline_from_unix_ms_with<F>(
    deadline_unix_ms: u64,
    now_unix_ms: u64,
    monotonic_now: Instant,
    checked_add: F,
) -> Result<Instant, MonotonicDeadlineError>
where
    F: FnOnce(Instant, Duration) -> Option<Instant>,
{
    if deadline_unix_ms <= now_unix_ms {
        return Err(MonotonicDeadlineError::Elapsed);
    }

    let remaining = Duration::from_millis(deadline_unix_ms - now_unix_ms);
    checked_add(monotonic_now, remaining).ok_or(MonotonicDeadlineError::Unrepresentable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a_near_future_deadline_without_extending_it() {
        let monotonic_now = Instant::now();
        let deadline = monotonic_deadline_from_unix_ms(10_001, 10_000, monotonic_now).unwrap();

        assert_eq!(
            deadline.duration_since(monotonic_now),
            Duration::from_millis(1)
        );
    }

    #[test]
    fn rejects_an_elapsed_deadline_without_creating_a_future_instant() {
        let result = monotonic_deadline_from_unix_ms(9_999, 10_000, Instant::now());

        assert_eq!(result, Err(MonotonicDeadlineError::Elapsed));
    }

    #[test]
    fn rejects_unrepresentable_deadline_without_panicking() {
        // The injected failed checked_add keeps this overflow-path test
        // deterministic across platform-specific Instant ranges.
        let result = std::panic::catch_unwind(|| {
            monotonic_deadline_from_unix_ms_with(u64::MAX, 0, Instant::now(), |_, duration| {
                assert_eq!(duration, Duration::from_millis(u64::MAX));
                None
            })
        });

        assert_eq!(
            result.unwrap(),
            Err(MonotonicDeadlineError::Unrepresentable)
        );
    }
}
