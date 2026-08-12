//! Offline reconnect-burst planning.
//!
//! This module produces an admission-controlled trace only. It never creates
//! an HTTP client, resolves a proxy, opens KCP, or carries ticket values.

use serde::{Deserialize, Serialize};

use crate::auth_budget::auth_operation_potential_writes;
use crate::config::{AuthOperation, HardBudget};
use crate::game_kcp::{GameKcpError, ReconnectPolicy};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconnectBurstSpec {
    pub virtual_players: u32,
    pub reconnect_attempts_per_player: u32,
    pub start_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReconnectBurstStep {
    Login,
    IssueTicket,
    ConnectProxy,
    AuthenticateProxy,
    RecoverRoom,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconnectBurstAction {
    pub at_ms: u64,
    pub player_slot: u32,
    pub reconnect_attempt: u32,
    pub step: ReconnectBurstStep,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconnectBurstPlan {
    pub actions: Vec<ReconnectBurstAction>,
    pub login_actions: u64,
    pub new_connections: u64,
    pub total_operations: u64,
    /// Login and ticket issuance can create or mutate session state. Keep the
    /// upper bound explicit even though this planner never sends a request.
    pub potential_data_writes: u64,
    /// Sum of delays returned by the shared reconnect policy. This excludes
    /// admission waits introduced by rate limiting.
    pub total_backoff_ms: u64,
    pub latest_action_ms: u64,
}

pub fn plan_reconnect_burst(
    spec: ReconnectBurstSpec,
    budget: &HardBudget,
    reconnect_policy: ReconnectPolicy,
) -> Result<ReconnectBurstPlan, ReconnectBurstPlanError> {
    reconnect_policy
        .validate()
        .map_err(ReconnectBurstPlanError::Policy)?;
    if spec.virtual_players == 0 || spec.virtual_players > budget.max_virtual_players {
        return Err(ReconnectBurstPlanError::VirtualPlayers {
            requested: spec.virtual_players,
            maximum: budget.max_virtual_players,
        });
    }
    if spec.reconnect_attempts_per_player == 0
        || spec.reconnect_attempts_per_player > reconnect_policy.max_attempts
    {
        return Err(ReconnectBurstPlanError::Attempts {
            requested: spec.reconnect_attempts_per_player,
            maximum: reconnect_policy.max_attempts,
        });
    }

    let login_actions = u64::from(spec.virtual_players);
    let proxy_connections = u64::from(spec.virtual_players)
        .checked_mul(u64::from(spec.reconnect_attempts_per_player))
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    // Auth dispatch uses `Connection: close`, so Login and IssueTicket each
    // reserve one global connection slot in addition to KCP proxy connects.
    let new_connections = login_actions
        .checked_mul(2)
        .and_then(|count| count.checked_add(proxy_connections))
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    let total_operations = login_actions
        .checked_mul(2)
        .and_then(|count| count.checked_add(proxy_connections.saturating_mul(3)))
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    let potential_writes_per_player = auth_operation_potential_writes(AuthOperation::Login)
        .checked_add(auth_operation_potential_writes(AuthOperation::IssueTicket))
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    let potential_data_writes = login_actions
        .checked_mul(potential_writes_per_player)
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    if total_operations > budget.max_total_operations {
        return Err(ReconnectBurstPlanError::Operations {
            planned: total_operations,
            maximum: budget.max_total_operations,
        });
    }
    if potential_data_writes > budget.max_data_writes {
        return Err(ReconnectBurstPlanError::DataWrites {
            planned: potential_data_writes,
            maximum: budget.max_data_writes,
        });
    }

    let login_spacing_ms = rate_spacing_ms(budget.max_login_qps)?;
    let connection_spacing_ms = rate_spacing_ms(budget.max_new_connections_per_second)?;
    let business_spacing_ms = rate_spacing_ms(budget.max_business_messages_per_second)?;
    let connection_message_spacing_ms =
        rate_spacing_ms(budget.max_messages_per_connection_per_second)?;
    let mut next_login_ms = spec.start_ms;
    let mut next_connection_ms = spec.start_ms;
    let mut next_business_ms = spec.start_ms;
    let mut total_backoff_ms = 0_u64;
    let mut actions = Vec::with_capacity(total_operations as usize);
    for player_slot in 0..spec.virtual_players {
        let login_at_ms = next_login_ms.max(next_connection_ms).max(next_business_ms);
        next_login_ms = next_login_ms.saturating_add(login_spacing_ms);
        next_connection_ms = login_at_ms.saturating_add(connection_spacing_ms);
        next_business_ms = login_at_ms.saturating_add(business_spacing_ms);
        actions.push(ReconnectBurstAction {
            at_ms: login_at_ms,
            player_slot,
            reconnect_attempt: 0,
            step: ReconnectBurstStep::Login,
        });
        let ticket_at_ms = after(login_at_ms)
            .max(next_connection_ms)
            .max(next_business_ms);
        next_connection_ms = ticket_at_ms.saturating_add(connection_spacing_ms);
        next_business_ms = ticket_at_ms.saturating_add(business_spacing_ms);
        actions.push(ReconnectBurstAction {
            at_ms: ticket_at_ms,
            player_slot,
            reconnect_attempt: 0,
            step: ReconnectBurstStep::IssueTicket,
        });

        let mut reconnect_at_ms = ticket_at_ms;
        for reconnect_attempt in 1..=spec.reconnect_attempts_per_player {
            if reconnect_attempt > 1 {
                let delay_ms = reconnect_policy
                    .delay_for(reconnect_attempt - 1, u64::from(player_slot))
                    .map_err(ReconnectBurstPlanError::Policy)?;
                total_backoff_ms = total_backoff_ms
                    .checked_add(delay_ms)
                    .ok_or(ReconnectBurstPlanError::Overflow)?;
                reconnect_at_ms = reconnect_at_ms.saturating_add(delay_ms);
            }
            let connect_at_ms = after(reconnect_at_ms).max(next_connection_ms);
            next_connection_ms = connect_at_ms.saturating_add(connection_spacing_ms);
            actions.push(ReconnectBurstAction {
                at_ms: connect_at_ms,
                player_slot,
                reconnect_attempt,
                step: ReconnectBurstStep::ConnectProxy,
            });
            let mut next_connection_message_ms = after(connect_at_ms);
            let authenticate_at_ms = reserve_business(
                next_connection_message_ms,
                &mut next_business_ms,
                business_spacing_ms,
            );
            next_connection_message_ms =
                authenticate_at_ms.saturating_add(connection_message_spacing_ms);
            actions.push(ReconnectBurstAction {
                at_ms: authenticate_at_ms,
                player_slot,
                reconnect_attempt,
                step: ReconnectBurstStep::AuthenticateProxy,
            });
            let recover_at_ms = reserve_business(
                next_connection_message_ms,
                &mut next_business_ms,
                business_spacing_ms,
            );
            actions.push(ReconnectBurstAction {
                at_ms: recover_at_ms,
                player_slot,
                reconnect_attempt,
                step: ReconnectBurstStep::RecoverRoom,
            });
            reconnect_at_ms = recover_at_ms;
        }
    }
    actions.sort_by_key(|action| (action.at_ms, action.player_slot, action.reconnect_attempt));
    let latest_action_ms = actions.last().map_or(spec.start_ms, |action| action.at_ms);
    let duration_ms = latest_action_ms.saturating_sub(spec.start_ms);
    if duration_ms > budget.max_duration_secs.saturating_mul(1_000) {
        return Err(ReconnectBurstPlanError::Duration {
            planned_ms: duration_ms,
            maximum_ms: budget.max_duration_secs.saturating_mul(1_000),
        });
    }
    Ok(ReconnectBurstPlan {
        actions,
        login_actions,
        new_connections,
        total_operations,
        potential_data_writes,
        total_backoff_ms,
        latest_action_ms,
    })
}

fn after(at_ms: u64) -> u64 {
    at_ms.saturating_add(1)
}

fn reserve_business(earliest_ms: u64, next_business_ms: &mut u64, spacing_ms: u64) -> u64 {
    let at_ms = earliest_ms.max(*next_business_ms);
    *next_business_ms = at_ms.saturating_add(spacing_ms);
    at_ms
}

fn rate_spacing_ms(rate_per_second: f64) -> Result<u64, ReconnectBurstPlanError> {
    if !rate_per_second.is_finite() || rate_per_second <= 0.0 {
        return Err(ReconnectBurstPlanError::InvalidRate);
    }
    Ok((1_000.0 / rate_per_second).ceil().max(1.0) as u64)
}

#[derive(Debug, thiserror::Error)]
pub enum ReconnectBurstPlanError {
    #[error(
        "reconnect burst requires {requested} virtual players but the hard budget allows {maximum}"
    )]
    VirtualPlayers { requested: u32, maximum: u32 },
    #[error("reconnect burst requires {requested} attempts but the policy allows {maximum}")]
    Attempts { requested: u32, maximum: u32 },
    #[error("reconnect burst plans {planned} operations but the hard budget allows {maximum}")]
    Operations { planned: u64, maximum: u64 },
    #[error("reconnect burst may write {planned} records but the hard budget allows {maximum}")]
    DataWrites { planned: u64, maximum: u64 },
    #[error("reconnect burst spans {planned_ms}ms but the hard budget allows {maximum_ms}ms")]
    Duration { planned_ms: u64, maximum_ms: u64 },
    #[error("reconnect burst rate is invalid")]
    InvalidRate,
    #[error("reconnect burst arithmetic overflowed")]
    Overflow,
    #[error("reconnect policy is invalid: {0}")]
    Policy(GameKcpError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> HardBudget {
        HardBudget {
            max_virtual_players: 3,
            max_login_qps: 2.0,
            max_new_connections_per_second: 2.0,
            max_business_messages_per_second: 10.0,
            max_messages_per_connection_per_second: 10.0,
            max_duration_secs: 10,
            max_total_operations: 24,
            max_error_rate: 0.1,
            max_connection_failure_rate: 0.1,
            max_p99_ms: 1_000,
            max_data_writes: 24,
        }
    }

    fn policy() -> ReconnectPolicy {
        ReconnectPolicy {
            max_attempts: 2,
            base_delay_ms: 100,
            max_delay_ms: 500,
            max_jitter_ms: 0,
        }
    }

    #[test]
    fn offline_burst_limits_login_connections_and_applies_backoff() {
        let plan = plan_reconnect_burst(
            ReconnectBurstSpec {
                virtual_players: 2,
                reconnect_attempts_per_player: 2,
                start_ms: 0,
            },
            &budget(),
            policy(),
        )
        .unwrap();
        assert_eq!(plan.login_actions, 2);
        assert_eq!(plan.new_connections, 8);
        assert_eq!(plan.total_operations, 16);
        assert_eq!(plan.potential_data_writes, 8);
        assert_eq!(plan.total_backoff_ms, 200);
        let logins = plan
            .actions
            .iter()
            .filter(|action| action.step == ReconnectBurstStep::Login)
            .collect::<Vec<_>>();
        assert!(logins[1].at_ms - logins[0].at_ms >= 500);
        let connections = plan
            .actions
            .iter()
            .filter(|action| action.step == ReconnectBurstStep::ConnectProxy)
            .collect::<Vec<_>>();
        assert!(
            connections
                .windows(2)
                .all(|window| window[1].at_ms - window[0].at_ms >= 500)
        );
        assert!(plan.actions.iter().any(|action| {
            action.player_slot == 0
                && action.reconnect_attempt == 2
                && action.step == ReconnectBurstStep::ConnectProxy
                && action.at_ms >= 100
        }));
        assert!(plan.actions.iter().all(|action| {
            !matches!(action.step, ReconnectBurstStep::RecoverRoom) || action.reconnect_attempt > 0
        }));
        for slot in 0..2 {
            let ticket = plan
                .actions
                .iter()
                .find(|action| {
                    action.player_slot == slot && action.step == ReconnectBurstStep::IssueTicket
                })
                .unwrap();
            let first_connect = plan
                .actions
                .iter()
                .find(|action| {
                    action.player_slot == slot
                        && action.reconnect_attempt == 1
                        && action.step == ReconnectBurstStep::ConnectProxy
                })
                .unwrap();
            assert!(ticket.at_ms < first_connect.at_ms);
        }
    }

    #[test]
    fn reconnect_burst_fails_closed_at_hard_budgets_and_policy_limit() {
        assert!(matches!(
            plan_reconnect_burst(
                ReconnectBurstSpec {
                    virtual_players: 3,
                    reconnect_attempts_per_player: 2,
                    start_ms: 0,
                },
                &HardBudget {
                    max_total_operations: 1,
                    ..budget()
                },
                policy(),
            ),
            Err(ReconnectBurstPlanError::Operations { .. })
        ));
        assert!(matches!(
            plan_reconnect_burst(
                ReconnectBurstSpec {
                    virtual_players: 1,
                    reconnect_attempts_per_player: 3,
                    start_ms: 0,
                },
                &budget(),
                policy(),
            ),
            Err(ReconnectBurstPlanError::Attempts { .. })
        ));
        assert!(matches!(
            plan_reconnect_burst(
                ReconnectBurstSpec {
                    virtual_players: 2,
                    reconnect_attempts_per_player: 1,
                    start_ms: 0,
                },
                &HardBudget {
                    max_data_writes: 7,
                    ..budget()
                },
                policy(),
            ),
            Err(ReconnectBurstPlanError::DataWrites { .. })
        ));
    }

    #[test]
    fn reconnect_burst_serializes_business_and_connection_messages_under_hard_rates() {
        let plan = plan_reconnect_burst(
            ReconnectBurstSpec {
                virtual_players: 1,
                reconnect_attempts_per_player: 2,
                start_ms: 0,
            },
            &HardBudget {
                max_business_messages_per_second: 2.0,
                max_messages_per_connection_per_second: 1.0,
                ..budget()
            },
            policy(),
        )
        .unwrap();
        let business = plan
            .actions
            .iter()
            .filter(|action| {
                matches!(
                    action.step,
                    ReconnectBurstStep::Login
                        | ReconnectBurstStep::IssueTicket
                        | ReconnectBurstStep::AuthenticateProxy
                        | ReconnectBurstStep::RecoverRoom
                )
            })
            .collect::<Vec<_>>();
        assert!(
            business
                .windows(2)
                .all(|window| window[1].at_ms - window[0].at_ms >= 500)
        );
        let auth = plan
            .actions
            .iter()
            .find(|action| {
                action.reconnect_attempt == 1
                    && action.step == ReconnectBurstStep::AuthenticateProxy
            })
            .unwrap();
        let recover = plan
            .actions
            .iter()
            .find(|action| {
                action.reconnect_attempt == 1 && action.step == ReconnectBurstStep::RecoverRoom
            })
            .unwrap();
        assert!(recover.at_ms - auth.at_ms >= 1_000);
    }
}
