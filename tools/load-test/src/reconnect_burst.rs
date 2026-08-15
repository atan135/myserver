//! Offline reconnect-burst planning.
//!
//! This module produces an admission-controlled trace only. It never creates
//! an HTTP client, resolves a proxy, opens KCP, or carries ticket values.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::abort::{AbortController, AbortReason};
use crate::auth_budget::{
    LIVE_GAMEPLAY_POTENTIAL_WRITES_PER_MESSAGE, auth_operation_potential_writes,
};
use crate::auth_http::{AuthAdmissionError, AuthDispatchAdmission};
use crate::config::{AuthOperation, EnvironmentKind, HardBudget};
use crate::game_kcp::{GameKcpError, ReconnectPolicy};

/// A single bounded retry gives the old KCP session's asynchronous disconnect
/// cleanup a chance to settle before the replacement session asks to recover
/// the approved room. The retry is planned and budgeted even when it is not
/// dispatched, so a temporary server rejection cannot increase live traffic.
pub const ROOM_HANDOFF_RETRY_BACKOFF_MS: u64 = 25;
pub const MAX_ROOM_HANDOFF_RETRY_ATTEMPTS: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReconnectBurstSpec {
    pub virtual_players: u32,
    pub reconnect_attempts_per_player: u32,
    pub start_ms: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReconnectBurstStep {
    DisconnectExisting,
    Login,
    IssueTicket,
    ConnectProxy,
    AuthenticateProxy,
    RecoverRoom,
    RetryRecoverRoom,
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
    pub forced_disconnects: u64,
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

/// Runtime accounting for the guarded live adapter. A reconnect player always
/// sends login, character-list, and ticket requests; remote profiles add one
/// target guard probe immediately before each of those dispatches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveReconnectBurstBudgetEstimate {
    pub guard_probe_operations: u64,
    pub total_operations: u64,
    pub new_connections: u64,
    pub business_messages: u64,
    pub potential_data_writes: u64,
    pub projected_duration_ms: u64,
}

pub fn estimate_live_reconnect_burst(
    plan: &ReconnectBurstPlan,
    budget: &HardBudget,
    include_guard_probes: bool,
) -> Result<LiveReconnectBurstBudgetEstimate, String> {
    let auth_requests = plan
        .login_actions
        .checked_mul(3)
        .ok_or("live reconnect auth request estimate overflowed")?;
    let guard_probe_operations = include_guard_probes.then_some(auth_requests).unwrap_or(0);
    let total_operations = plan
        .total_operations
        .checked_add(guard_probe_operations)
        .ok_or("live reconnect operation estimate overflowed")?;
    let new_connections = plan
        .new_connections
        .checked_add(guard_probe_operations)
        .ok_or("live reconnect connection estimate overflowed")?;
    let proxy_connections = plan
        .new_connections
        .checked_sub(auth_requests)
        .ok_or("live reconnect plan is missing auth connection accounting")?;
    let business_messages = plan
        .total_operations
        .checked_sub(proxy_connections)
        .and_then(|count| count.checked_add(guard_probe_operations))
        .ok_or("live reconnect message estimate overflowed")?;

    let guard_spacing_ms = rate_spacing_ms(budget.max_new_connections_per_second)
        .map_err(|error| error.to_string())?
        .max(
            rate_spacing_ms(budget.max_business_messages_per_second)
                .map_err(|error| error.to_string())?,
        )
        .max(
            rate_spacing_ms(budget.max_messages_per_connection_per_second)
                .map_err(|error| error.to_string())?,
        );
    let guard_duration_ms = guard_probe_operations
        .checked_mul(guard_spacing_ms)
        .ok_or("live reconnect duration estimate overflowed")?;
    let projected_duration_ms = plan
        .latest_action_ms
        .checked_add(guard_duration_ms)
        .ok_or("live reconnect duration estimate overflowed")?;

    Ok(LiveReconnectBurstBudgetEstimate {
        guard_probe_operations,
        total_operations,
        new_connections,
        business_messages,
        potential_data_writes: plan.potential_data_writes,
        projected_duration_ms,
    })
}

pub fn validate_live_reconnect_burst_budget(
    estimate: &LiveReconnectBurstBudgetEstimate,
    budget: &HardBudget,
) -> Result<(), String> {
    if estimate.total_operations > budget.max_total_operations {
        return Err(format!(
            "live reconnect burst estimates {} operations including {} auth guard probes, exceeding max_total_operations {}",
            estimate.total_operations, estimate.guard_probe_operations, budget.max_total_operations
        ));
    }
    if estimate.potential_data_writes > budget.max_data_writes {
        return Err(format!(
            "live reconnect burst estimates {} potential data writes, exceeding max_data_writes {}",
            estimate.potential_data_writes, budget.max_data_writes
        ));
    }
    let maximum_duration_ms = budget.max_duration_secs.saturating_mul(1_000);
    if estimate.projected_duration_ms > maximum_duration_ms {
        return Err(format!(
            "live reconnect burst needs up to {}ms including auth guard probes, exceeding max_duration_secs {}",
            estimate.projected_duration_ms, budget.max_duration_secs
        ));
    }
    Ok(())
}

/// Explicitly separates plan inspection from live reconnect transport. The
/// execution path is deliberately limited to local/test and requires the
/// same named game confirmation as the regular player runner.
#[derive(Debug, Clone, Copy)]
pub struct ReconnectBurstExecutionGate<'a> {
    pub execute_game: bool,
    pub confirm_game: Option<&'a str>,
    pub environment_name: &'a str,
    pub environment_kind: EnvironmentKind,
}

impl ReconnectBurstExecutionGate<'_> {
    pub fn validate(self) -> Result<(), ReconnectBurstExecutionError> {
        if !matches!(
            self.environment_kind,
            EnvironmentKind::Local | EnvironmentKind::Test
        ) {
            return Err(ReconnectBurstExecutionError::Gate(
                "reconnect burst execution is restricted to local/test profiles",
            ));
        }
        if !self.execute_game {
            return Err(ReconnectBurstExecutionError::Gate(
                "reconnect burst execution requires --execute-game",
            ));
        }
        if self.confirm_game != Some(self.environment_name) {
            return Err(ReconnectBurstExecutionError::Gate(
                "reconnect burst execution requires --confirm-game <environment>",
            ));
        }
        Ok(())
    }
}

/// Shared runtime admission exposed to a reconnect action adapter. The plan
/// reserves its primary action before calling the adapter; adapters use these
/// methods for any explicitly required supporting request, such as looking up
/// the prepared character after a successful login.
pub struct ReconnectBurstAdmission<'a> {
    admission: &'a mut AuthDispatchAdmission,
    deadline: Instant,
    abort: &'a mut AbortController,
    checkpoint: &'a mut dyn FnMut(&mut AbortController) -> Result<(), String>,
}

impl ReconnectBurstAdmission<'_> {
    /// Re-runs the controller's environment/stop-signal checkpoint. Live
    /// adapters call this while waiting for a scheduled action so a bounded
    /// reconnect backoff cannot mask Ctrl+C, stop-file, deadline, or target
    /// protection changes.
    pub fn revalidate(&mut self) -> Result<(), ReconnectBurstExecutionError> {
        (self.checkpoint)(self.abort).map_err(ReconnectBurstExecutionError::Checkpoint)?;
        (!self.abort.should_stop_new_sessions())
            .then_some(())
            .ok_or(ReconnectBurstExecutionError::Stopped)
    }

    pub fn admit_auth_operation(
        &mut self,
        operation: AuthOperation,
    ) -> Result<(), ReconnectBurstExecutionError> {
        admit_reconnect(
            self.admission
                .admit_auth_operation(operation, self.deadline, || ensure_not_stopped(self.abort)),
        )
    }

    /// A remote auth guard has the same connection and message accounting as
    /// an auth request, but reserves no mutable data-write budget.
    pub fn admit_guard_probe(&mut self) -> Result<(), ReconnectBurstExecutionError> {
        let result = {
            let admission = &mut *self.admission;
            let deadline = self.deadline;
            let abort = &mut *self.abort;
            let checkpoint = &mut *self.checkpoint;
            admit_reconnect(admission.admit_guard_probe(deadline, || {
                checkpoint(abort)?;
                ensure_not_stopped(abort)
            }))
        };
        match &result {
            Err(ReconnectBurstExecutionError::Admission(AuthAdmissionError::BudgetExceeded(_))) => {
                self.abort.request(AbortReason::BudgetExceeded);
            }
            Err(ReconnectBurstExecutionError::Admission(AuthAdmissionError::DeadlineExceeded)) => {
                self.abort.request(AbortReason::Deadline);
            }
            _ => {}
        }
        result
    }

    pub fn admit_game_connection(&mut self) -> Result<(), ReconnectBurstExecutionError> {
        admit_reconnect(
            self.admission
                .admit_game_connection(self.deadline, || ensure_not_stopped(self.abort)),
        )
    }

    pub fn admit_game_message(&mut self) -> Result<(), ReconnectBurstExecutionError> {
        admit_reconnect(
            self.admission
                .admit_game_message(self.deadline, || ensure_not_stopped(self.abort)),
        )
    }

    pub fn admit_gameplay_message(&mut self) -> Result<(), ReconnectBurstExecutionError> {
        admit_reconnect(self.admission.admit_gameplay_message(
            LIVE_GAMEPLAY_POTENTIAL_WRITES_PER_MESSAGE,
            self.deadline,
            || ensure_not_stopped(self.abort),
        ))
    }

    pub fn remaining(&self) -> Result<std::time::Duration, ReconnectBurstExecutionError> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ReconnectBurstExecutionError::Admission(
                AuthAdmissionError::DeadlineExceeded,
            ));
        }
        Ok(remaining)
    }

    pub fn should_stop(&self) -> bool {
        self.abort.should_stop_new_sessions()
    }

    pub fn mark_protection_failed(&mut self) {
        self.abort.check_protection(false);
    }
}

/// A deterministic transport boundary. The executor receives no tickets,
/// account IDs, endpoint addresses, or packet bodies; a real adapter must own
/// those sensitive values and only reports completion of the planned action.
pub trait ReconnectBurstExecutor {
    fn execute(
        &mut self,
        action: ReconnectBurstAction,
        admission: &mut ReconnectBurstAdmission<'_>,
    ) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconnectBurstExecutionMetrics {
    pub forced_disconnects: u64,
    pub login_actions: u64,
    pub ticket_actions: u64,
    pub proxy_connections: u64,
    pub proxy_authentications: u64,
    pub room_recoveries: u64,
    pub room_recovery_retry_slots: u64,
}

/// Executes an already budget-validated reconnect plan through a caller-owned
/// adapter. Every outbound operation is admitted by the shared hard-budget
/// ledger and every action checks the single run abort controller. The helper
/// itself never opens a socket or sleeps, which makes the control flow fully
/// deterministic in tests; a transport adapter may schedule against `at_ms`.
pub fn execute_reconnect_burst<E, C>(
    plan: &ReconnectBurstPlan,
    budget: &HardBudget,
    gate: ReconnectBurstExecutionGate<'_>,
    admission: &mut AuthDispatchAdmission,
    deadline: Instant,
    abort: &mut AbortController,
    mut checkpoint: C,
    executor: &mut E,
) -> Result<ReconnectBurstExecutionMetrics, ReconnectBurstExecutionError>
where
    E: ReconnectBurstExecutor,
    C: FnMut(&mut AbortController) -> Result<(), String>,
{
    gate.validate()?;
    if plan.total_operations > budget.max_total_operations
        || plan.potential_data_writes > budget.max_data_writes
        || plan.login_actions > u64::from(budget.max_virtual_players)
    {
        return Err(ReconnectBurstExecutionError::BudgetMismatch);
    }
    let mut metrics = ReconnectBurstExecutionMetrics::default();
    for action in &plan.actions {
        let mut action_admission = ReconnectBurstAdmission {
            admission,
            deadline,
            abort,
            checkpoint: &mut checkpoint,
        };
        action_admission.revalidate()?;
        match action.step {
            ReconnectBurstStep::DisconnectExisting => {
                metrics.forced_disconnects = metrics.forced_disconnects.saturating_add(1);
            }
            ReconnectBurstStep::Login => {
                action_admission.admit_auth_operation(AuthOperation::Login)?;
                metrics.login_actions = metrics.login_actions.saturating_add(1);
            }
            ReconnectBurstStep::IssueTicket => {
                action_admission.admit_auth_operation(AuthOperation::IssueTicket)?;
                metrics.ticket_actions = metrics.ticket_actions.saturating_add(1);
            }
            ReconnectBurstStep::ConnectProxy => {
                action_admission.admit_game_connection()?;
                metrics.proxy_connections = metrics.proxy_connections.saturating_add(1);
            }
            ReconnectBurstStep::AuthenticateProxy => {
                action_admission.admit_game_message()?;
                metrics.proxy_authentications = metrics.proxy_authentications.saturating_add(1);
            }
            ReconnectBurstStep::RecoverRoom => {
                action_admission.admit_gameplay_message()?;
                metrics.room_recoveries = metrics.room_recoveries.saturating_add(1);
            }
            ReconnectBurstStep::RetryRecoverRoom => {
                action_admission.admit_gameplay_message()?;
                metrics.room_recovery_retry_slots =
                    metrics.room_recovery_retry_slots.saturating_add(1);
            }
        }
        executor
            .execute(*action, &mut action_admission)
            .map_err(ReconnectBurstExecutionError::Executor)?;
    }
    Ok(metrics)
}

fn ensure_not_stopped(abort: &AbortController) -> Result<(), String> {
    (!abort.should_stop_new_sessions())
        .then_some(())
        .ok_or_else(|| "reconnect burst was stopped".into())
}

fn admit_reconnect(
    result: Result<std::time::Duration, AuthAdmissionError>,
) -> Result<(), ReconnectBurstExecutionError> {
    result
        .map(|_| ())
        .map_err(ReconnectBurstExecutionError::Admission)
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
    // Every player first establishes one online session, then each requested
    // reconnect attempt closes that session and opens a replacement. Keeping
    // the initial connection in the plan makes `DisconnectExisting` an actual
    // lifecycle action instead of an unbound synthetic event.
    let connections_per_player = u64::from(spec.reconnect_attempts_per_player)
        .checked_add(1)
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    let proxy_connections = u64::from(spec.virtual_players)
        .checked_mul(connections_per_player)
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    let forced_disconnects = u64::from(spec.virtual_players)
        .checked_mul(u64::from(spec.reconnect_attempts_per_player))
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    let room_handoff_retry_actions = forced_disconnects
        .checked_mul(u64::from(MAX_ROOM_HANDOFF_RETRY_ATTEMPTS))
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    // Auth dispatch uses `Connection: close`. A Login action also performs
    // one public character-list read to resolve the prepared identity, so it
    // reserves three HTTP connection slots per player (login/list/ticket) in
    // addition to KCP proxy connects.
    let new_connections = login_actions
        .checked_mul(3)
        .and_then(|count| count.checked_add(proxy_connections))
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    let total_operations = login_actions
        .checked_mul(3)
        .and_then(|count| count.checked_add(proxy_connections.saturating_mul(3)))
        .and_then(|count| count.checked_add(room_handoff_retry_actions))
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    // Establishing the approved room and every RoomReconnectReq can mutate
    // room membership or route state, so reserve the same conservative write
    // bound used by the live gameplay runner.
    let room_recovery_requests_per_player = connections_per_player
        .checked_add(
            u64::from(spec.reconnect_attempts_per_player)
                .checked_mul(u64::from(MAX_ROOM_HANDOFF_RETRY_ATTEMPTS))
                .ok_or(ReconnectBurstPlanError::Overflow)?,
        )
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    let room_recovery_writes = room_recovery_requests_per_player
        .checked_mul(LIVE_GAMEPLAY_POTENTIAL_WRITES_PER_MESSAGE)
        .ok_or(ReconnectBurstPlanError::Overflow)?;
    let potential_writes_per_player = auth_operation_potential_writes(AuthOperation::Login)
        .checked_add(auth_operation_potential_writes(AuthOperation::IssueTicket))
        .and_then(|count| count.checked_add(room_recovery_writes))
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
    let mut actions = Vec::with_capacity(
        total_operations
            .checked_add(forced_disconnects)
            .ok_or(ReconnectBurstPlanError::Overflow)? as usize,
    );
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

        let initial_connect_at_ms = after(ticket_at_ms).max(next_connection_ms);
        next_connection_ms = initial_connect_at_ms.saturating_add(connection_spacing_ms);
        actions.push(ReconnectBurstAction {
            at_ms: initial_connect_at_ms,
            player_slot,
            reconnect_attempt: 0,
            step: ReconnectBurstStep::ConnectProxy,
        });
        let mut next_connection_message_ms = after(initial_connect_at_ms);
        let initial_authenticate_at_ms = reserve_business(
            next_connection_message_ms,
            &mut next_business_ms,
            business_spacing_ms,
        );
        next_connection_message_ms =
            initial_authenticate_at_ms.saturating_add(connection_message_spacing_ms);
        actions.push(ReconnectBurstAction {
            at_ms: initial_authenticate_at_ms,
            player_slot,
            reconnect_attempt: 0,
            step: ReconnectBurstStep::AuthenticateProxy,
        });
        let initial_recover_at_ms = reserve_business(
            next_connection_message_ms,
            &mut next_business_ms,
            business_spacing_ms,
        );
        actions.push(ReconnectBurstAction {
            at_ms: initial_recover_at_ms,
            player_slot,
            reconnect_attempt: 0,
            step: ReconnectBurstStep::RecoverRoom,
        });

        let mut reconnect_at_ms = initial_recover_at_ms;
        for reconnect_attempt in 1..=spec.reconnect_attempts_per_player {
            let delay_ms = reconnect_policy
                .delay_for(reconnect_attempt, u64::from(player_slot))
                .map_err(ReconnectBurstPlanError::Policy)?;
            total_backoff_ms = total_backoff_ms
                .checked_add(delay_ms)
                .ok_or(ReconnectBurstPlanError::Overflow)?;
            reconnect_at_ms = reconnect_at_ms.saturating_add(delay_ms);
            actions.push(ReconnectBurstAction {
                at_ms: reconnect_at_ms,
                player_slot,
                reconnect_attempt,
                step: ReconnectBurstStep::DisconnectExisting,
            });
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
            let retry_recover_at_ms = reserve_business(
                recover_at_ms.saturating_add(ROOM_HANDOFF_RETRY_BACKOFF_MS),
                &mut next_business_ms,
                business_spacing_ms,
            );
            actions.push(ReconnectBurstAction {
                at_ms: retry_recover_at_ms,
                player_slot,
                reconnect_attempt,
                step: ReconnectBurstStep::RetryRecoverRoom,
            });
            reconnect_at_ms = retry_recover_at_ms;
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
        forced_disconnects,
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

#[derive(Debug, thiserror::Error)]
pub enum ReconnectBurstExecutionError {
    #[error("reconnect burst execution gate rejected: {0}")]
    Gate(&'static str),
    #[error("reconnect burst plan exceeds the active hard budget")]
    BudgetMismatch,
    #[error("reconnect burst admission rejected: {0}")]
    Admission(AuthAdmissionError),
    #[error("reconnect burst checkpoint failed: {0}")]
    Checkpoint(String),
    #[error("reconnect burst was stopped before the next action")]
    Stopped,
    #[error("reconnect burst executor failed: {0}")]
    Executor(String),
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
            max_total_operations: 28,
            max_error_rate: 0.1,
            max_connection_failure_rate: 0.1,
            max_p99_ms: 1_000,
            max_data_writes: 48,
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
        assert_eq!(plan.forced_disconnects, 4);
        assert_eq!(plan.new_connections, 12);
        assert_eq!(plan.total_operations, 28);
        assert_eq!(plan.potential_data_writes, 48);
        assert_eq!(plan.total_backoff_ms, 600);
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
                && action.at_ms >= 300
        }));
        assert!(plan.actions.iter().any(|action| {
            action.player_slot == 0
                && action.reconnect_attempt == 0
                && action.step == ReconnectBurstStep::RecoverRoom
        }));
        assert_eq!(
            plan.actions
                .iter()
                .filter(|action| action.step == ReconnectBurstStep::RetryRecoverRoom)
                .count(),
            4
        );
        for slot in 0..2 {
            let ticket = plan
                .actions
                .iter()
                .find(|action| {
                    action.player_slot == slot && action.step == ReconnectBurstStep::IssueTicket
                })
                .unwrap();
            let initial_connect = plan
                .actions
                .iter()
                .find(|action| {
                    action.player_slot == slot
                        && action.reconnect_attempt == 0
                        && action.step == ReconnectBurstStep::ConnectProxy
                })
                .unwrap();
            assert!(ticket.at_ms < initial_connect.at_ms);
            assert!(plan.actions.iter().any(|action| {
                action.player_slot == slot
                    && action.reconnect_attempt == 1
                    && action.step == ReconnectBurstStep::DisconnectExisting
            }));
        }
    }

    #[test]
    fn live_remote_estimate_reserves_a_guard_for_every_reconnect_auth_request() {
        let plan = plan_reconnect_burst(
            ReconnectBurstSpec {
                virtual_players: 1,
                reconnect_attempts_per_player: 1,
                start_ms: 0,
            },
            &budget(),
            policy(),
        )
        .unwrap();

        let local = estimate_live_reconnect_burst(&plan, &budget(), false).unwrap();
        assert_eq!(local.guard_probe_operations, 0);
        assert_eq!(local.total_operations, 10);
        assert_eq!(local.new_connections, 5);
        assert_eq!(local.business_messages, 8);

        let remote = estimate_live_reconnect_burst(&plan, &budget(), true).unwrap();
        assert_eq!(remote.guard_probe_operations, 3);
        assert_eq!(remote.total_operations, 13);
        assert_eq!(remote.new_connections, 8);
        assert_eq!(remote.business_messages, 11);
        assert_eq!(remote.potential_data_writes, 16);
        assert!(remote.projected_duration_ms > plan.latest_action_ms);

        let constrained = HardBudget {
            max_total_operations: 12,
            ..budget()
        };
        assert!(
            validate_live_reconnect_burst_budget(&remote, &constrained)
                .unwrap_err()
                .contains("auth guard probes")
        );
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
    fn handoff_retry_budget_rejects_before_executor_transport() {
        let planned_budget = HardBudget {
            max_virtual_players: 1,
            max_total_operations: 10,
            max_data_writes: 16,
            ..budget()
        };
        let plan = plan_reconnect_burst(
            ReconnectBurstSpec {
                virtual_players: 1,
                reconnect_attempts_per_player: 1,
                start_ms: 0,
            },
            &planned_budget,
            policy(),
        )
        .unwrap();
        let constrained_budget = HardBudget {
            max_total_operations: 9,
            ..planned_budget
        };
        let mut admission = AuthDispatchAdmission::new(&constrained_budget).unwrap();
        let mut abort = AbortController::default();
        let mut executor = RecordingExecutor::default();

        let result = execute_reconnect_burst(
            &plan,
            &constrained_budget,
            ReconnectBurstExecutionGate {
                execute_game: true,
                confirm_game: Some("test"),
                environment_name: "test",
                environment_kind: EnvironmentKind::Test,
            },
            &mut admission,
            Instant::now() + std::time::Duration::from_secs(1),
            &mut abort,
            |_| Ok(()),
            &mut executor,
        );

        assert!(matches!(
            result,
            Err(ReconnectBurstExecutionError::BudgetMismatch)
        ));
        assert!(executor.actions.is_empty());
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
                        | ReconnectBurstStep::RetryRecoverRoom
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

    #[derive(Default)]
    struct RecordingExecutor {
        actions: Vec<ReconnectBurstAction>,
    }

    impl ReconnectBurstExecutor for RecordingExecutor {
        fn execute(
            &mut self,
            action: ReconnectBurstAction,
            _admission: &mut ReconnectBurstAdmission<'_>,
        ) -> Result<(), String> {
            self.actions.push(action);
            Ok(())
        }
    }

    #[derive(Default)]
    struct CharacterLookupAdapter {
        actions: Vec<ReconnectBurstAction>,
    }

    impl ReconnectBurstExecutor for CharacterLookupAdapter {
        fn execute(
            &mut self,
            action: ReconnectBurstAction,
            admission: &mut ReconnectBurstAdmission<'_>,
        ) -> Result<(), String> {
            if action.step == ReconnectBurstStep::Login {
                admission
                    .admit_auth_operation(AuthOperation::ListCharacters)
                    .map_err(|error| error.to_string())?;
            }
            self.actions.push(action);
            Ok(())
        }
    }

    #[test]
    fn adapter_supporting_requests_share_the_planned_hard_budget() {
        let fast_budget = HardBudget {
            max_virtual_players: 1,
            max_login_qps: 10_000.0,
            max_new_connections_per_second: 10_000.0,
            max_business_messages_per_second: 10_000.0,
            max_messages_per_connection_per_second: 10_000.0,
            max_duration_secs: 10,
            max_total_operations: 10,
            max_error_rate: 0.1,
            max_connection_failure_rate: 0.1,
            max_p99_ms: 1_000,
            max_data_writes: 16,
        };
        let plan = plan_reconnect_burst(
            ReconnectBurstSpec {
                virtual_players: 1,
                reconnect_attempts_per_player: 1,
                start_ms: 0,
            },
            &fast_budget,
            policy(),
        )
        .unwrap();
        assert_eq!(plan.total_operations, 10);
        let mut admission = AuthDispatchAdmission::new(&fast_budget).unwrap();
        let mut abort = AbortController::default();
        let mut adapter = CharacterLookupAdapter::default();
        execute_reconnect_burst(
            &plan,
            &fast_budget,
            ReconnectBurstExecutionGate {
                execute_game: true,
                confirm_game: Some("local"),
                environment_name: "local",
                environment_kind: EnvironmentKind::Local,
            },
            &mut admission,
            Instant::now() + std::time::Duration::from_secs(1),
            &mut abort,
            |_| Ok(()),
            &mut adapter,
        )
        .unwrap();
        assert_eq!(admission.used_operations(), plan.total_operations);
        assert_eq!(adapter.actions, plan.actions);
    }

    #[test]
    fn deterministic_executor_requires_local_test_gate_and_reuses_admission_abort_and_budget() {
        let fast_budget = HardBudget {
            max_virtual_players: 1,
            max_login_qps: 10_000.0,
            max_new_connections_per_second: 10_000.0,
            max_business_messages_per_second: 10_000.0,
            max_messages_per_connection_per_second: 10_000.0,
            max_duration_secs: 10,
            max_total_operations: 10,
            max_error_rate: 0.1,
            max_connection_failure_rate: 0.1,
            max_p99_ms: 1_000,
            max_data_writes: 16,
        };
        let plan = plan_reconnect_burst(
            ReconnectBurstSpec {
                virtual_players: 1,
                reconnect_attempts_per_player: 1,
                start_ms: 0,
            },
            &fast_budget,
            policy(),
        )
        .unwrap();
        let mut admission = AuthDispatchAdmission::new(&fast_budget).unwrap();
        let mut abort = AbortController::default();
        let mut executor = RecordingExecutor::default();
        let metrics = execute_reconnect_burst(
            &plan,
            &fast_budget,
            ReconnectBurstExecutionGate {
                execute_game: true,
                confirm_game: Some("local"),
                environment_name: "local",
                environment_kind: EnvironmentKind::Local,
            },
            &mut admission,
            Instant::now() + std::time::Duration::from_secs(1),
            &mut abort,
            |_| Ok(()),
            &mut executor,
        )
        .unwrap();
        assert_eq!(metrics.forced_disconnects, 1);
        assert_eq!(metrics.login_actions, 1);
        assert_eq!(metrics.ticket_actions, 1);
        assert_eq!(metrics.proxy_connections, 2);
        assert_eq!(metrics.proxy_authentications, 2);
        assert_eq!(metrics.room_recoveries, 2);
        assert_eq!(metrics.room_recovery_retry_slots, 1);
        assert_eq!(executor.actions, plan.actions);

        let mut executor = RecordingExecutor::default();
        assert!(matches!(
            execute_reconnect_burst(
                &plan,
                &fast_budget,
                ReconnectBurstExecutionGate {
                    execute_game: true,
                    confirm_game: Some("production"),
                    environment_name: "production",
                    environment_kind: EnvironmentKind::Production,
                },
                &mut admission,
                Instant::now() + std::time::Duration::from_secs(1),
                &mut abort,
                |_| Ok(()),
                &mut executor,
            ),
            Err(ReconnectBurstExecutionError::Gate(_))
        ));
        assert!(executor.actions.is_empty());
    }

    #[test]
    fn deterministic_executor_stops_before_transport_when_abort_is_requested() {
        let mut tiny = budget();
        tiny.max_virtual_players = 1;
        tiny.max_total_operations = 10;
        let plan = plan_reconnect_burst(
            ReconnectBurstSpec {
                virtual_players: 1,
                reconnect_attempts_per_player: 1,
                start_ms: 0,
            },
            &tiny,
            policy(),
        )
        .unwrap();
        let mut admission = AuthDispatchAdmission::new(&tiny).unwrap();
        let mut abort = AbortController::default();
        let mut executor = RecordingExecutor::default();
        let result = execute_reconnect_burst(
            &plan,
            &tiny,
            ReconnectBurstExecutionGate {
                execute_game: true,
                confirm_game: Some("test"),
                environment_name: "test",
                environment_kind: EnvironmentKind::Test,
            },
            &mut admission,
            Instant::now() + std::time::Duration::from_secs(1),
            &mut abort,
            |abort| {
                abort.request(crate::abort::AbortReason::StopFile);
                Ok(())
            },
            &mut executor,
        );
        assert!(matches!(result, Err(ReconnectBurstExecutionError::Stopped)));
        assert!(executor.actions.is_empty());
    }
}
