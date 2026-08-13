use crate::config::EnvironmentKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneOperation {
    PlayerQuery,
    AuditQuery,
    MetricsOverview,
    MetricsDetail,
    GmCommand,
    Breakglass,
    Ban,
    Kick,
    GrantReward,
    ModifyCharacter,
    Migrate,
    Drain,
    Shutdown,
    Archive,
}

impl ControlPlaneOperation {
    fn is_read_only(self) -> bool {
        matches!(
            self,
            Self::PlayerQuery | Self::AuditQuery | Self::MetricsOverview | Self::MetricsDetail
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ControlPlaneRequest {
    pub operation: ControlPlaneOperation,
    #[serde(default)]
    pub page: u32,
    #[serde(default = "default_page_size")]
    pub page_size: u32,
    #[serde(default)]
    pub range_secs: u64,
    #[serde(default)]
    pub expected_result_bytes: u64,
    #[serde(default)]
    pub timeout_ms: u64,
    #[serde(default)]
    pub concurrent_requests: u32,
}

fn default_page_size() -> u32 {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControlPlaneBudget {
    pub max_concurrency: u32,
    pub max_page_size: u32,
    pub max_range_secs: u64,
    pub max_result_bytes: u64,
    pub max_query_ms: u64,
}

impl Default for ControlPlaneBudget {
    fn default() -> Self {
        Self {
            max_concurrency: 2,
            max_page_size: 100,
            max_range_secs: 86_400,
            max_result_bytes: 1_048_576,
            max_query_ms: 5_000,
        }
    }
}

pub fn validate_read_request(
    request: &ControlPlaneRequest,
    budget: &ControlPlaneBudget,
    environment: EnvironmentKind,
) -> Result<(), String> {
    if !request.operation.is_read_only() {
        return Err("control-plane write operation is forbidden".into());
    }
    if request.concurrent_requests == 0 || request.concurrent_requests > budget.max_concurrency {
        return Err("control-plane concurrency exceeds bounded limit".into());
    }
    if request.page_size == 0 || request.page_size > budget.max_page_size {
        return Err("control-plane page size exceeds bounded limit".into());
    }
    if request.range_secs > budget.max_range_secs {
        return Err("control-plane time range exceeds bounded limit".into());
    }
    if request.expected_result_bytes > budget.max_result_bytes {
        return Err("control-plane result exceeds bounded limit".into());
    }
    if request.timeout_ms > budget.max_query_ms {
        return Err("control-plane query timeout exceeds bounded limit".into());
    }
    if environment == EnvironmentKind::Production && !request.operation.is_read_only() {
        return Err("production control-plane writes are forbidden".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlPlaneOutcome {
    Success,
    RateLimited,
    Slow,
    Timeout,
    Rejected,
}

#[derive(Debug, Clone)]
pub struct DeterministicControlPlaneFake {
    outcomes: std::collections::VecDeque<ControlPlaneOutcome>,
}

impl DeterministicControlPlaneFake {
    pub fn scripted(outcomes: impl IntoIterator<Item = ControlPlaneOutcome>) -> Self {
        Self {
            outcomes: outcomes.into_iter().collect(),
        }
    }

    pub fn execute(
        &mut self,
        request: &ControlPlaneRequest,
        budget: &ControlPlaneBudget,
        environment: EnvironmentKind,
    ) -> ControlPlaneOutcome {
        if validate_read_request(request, budget, environment).is_err() {
            return ControlPlaneOutcome::Rejected;
        }
        self.outcomes
            .pop_front()
            .unwrap_or(ControlPlaneOutcome::Success)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservationSnapshot {
    pub run_id: String,
    pub window_start_unix_ms: u64,
    pub window_end_unix_ms: u64,
    pub source: String,
    pub freshness_ms: u64,
    pub complete: bool,
}

pub fn require_fresh_observation(
    snapshot: &ObservationSnapshot,
    now_unix_ms: u64,
    max_age_ms: u64,
) -> Result<(), String> {
    if snapshot.window_end_unix_ms > now_unix_ms
        || now_unix_ms.saturating_sub(snapshot.window_end_unix_ms) > max_age_ms
    {
        return Err("service observation is stale".into());
    }
    if !snapshot.complete
        || snapshot.window_start_unix_ms >= snapshot.window_end_unix_ms
        || snapshot.run_id.trim().is_empty()
        || snapshot.source.trim().is_empty()
    {
        return Err("service observation is incomplete".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_allowlist_and_bounds_fail_closed() {
        let budget = ControlPlaneBudget::default();
        let request = ControlPlaneRequest {
            operation: ControlPlaneOperation::PlayerQuery,
            page: 0,
            page_size: 50,
            range_secs: 60,
            expected_result_bytes: 100,
            timeout_ms: 100,
            concurrent_requests: 1,
        };
        validate_read_request(&request, &budget, EnvironmentKind::Production).unwrap();
        assert!(
            validate_read_request(
                &ControlPlaneRequest {
                    operation: ControlPlaneOperation::Kick,
                    ..request.clone()
                },
                &budget,
                EnvironmentKind::Production
            )
            .is_err()
        );
        assert!(
            validate_read_request(
                &ControlPlaneRequest {
                    page_size: 101,
                    ..request.clone()
                },
                &budget,
                EnvironmentKind::Local
            )
            .is_err()
        );
    }

    #[test]
    fn stale_or_incomplete_observations_stop_execution() {
        let snapshot = ObservationSnapshot {
            run_id: "run-1".into(),
            window_start_unix_ms: 100,
            window_end_unix_ms: 900,
            source: "redis".into(),
            freshness_ms: 100,
            complete: true,
        };
        require_fresh_observation(&snapshot, 1_000, 200).unwrap();
        assert!(require_fresh_observation(&snapshot, 2_000, 200).is_err());
        assert!(
            require_fresh_observation(
                &ObservationSnapshot {
                    complete: false,
                    ..snapshot
                },
                1_000,
                200
            )
            .is_err()
        );
    }

    #[test]
    fn control_plane_fake_covers_success_limit_slow_timeout_and_rejection() {
        let budget = ControlPlaneBudget::default();
        let request = ControlPlaneRequest {
            operation: ControlPlaneOperation::MetricsOverview,
            page: 0,
            page_size: 20,
            range_secs: 10,
            expected_result_bytes: 100,
            timeout_ms: 100,
            concurrent_requests: 1,
        };
        let mut fake = DeterministicControlPlaneFake::scripted([
            ControlPlaneOutcome::Success,
            ControlPlaneOutcome::RateLimited,
            ControlPlaneOutcome::Slow,
            ControlPlaneOutcome::Timeout,
        ]);
        assert_eq!(
            fake.execute(&request, &budget, EnvironmentKind::Local),
            ControlPlaneOutcome::Success
        );
        assert_eq!(
            fake.execute(&request, &budget, EnvironmentKind::Local),
            ControlPlaneOutcome::RateLimited
        );
        assert_eq!(
            fake.execute(&request, &budget, EnvironmentKind::Local),
            ControlPlaneOutcome::Slow
        );
        assert_eq!(
            fake.execute(&request, &budget, EnvironmentKind::Local),
            ControlPlaneOutcome::Timeout
        );
        assert_eq!(
            fake.execute(
                &ControlPlaneRequest {
                    operation: ControlPlaneOperation::GmCommand,
                    ..request
                },
                &budget,
                EnvironmentKind::Production
            ),
            ControlPlaneOutcome::Rejected
        );
    }
}
