//! Conservative, credential-free accounting for auth preparation and runs.
//!
//! The estimates deliberately include Redis/session/ticket and audit effects
//! in addition to PostgreSQL rows. They are admission bounds, not a claim that
//! every implementation path will materialize every write.

use serde::Serialize;

use crate::abort::{AbortController, AbortReason};
use crate::config::{AuthOperation, HardBudget, LoadModel, Scenario};

const REGISTER_POTENTIAL_WRITES: u64 = 4;
const LOGIN_POTENTIAL_WRITES: u64 = 3;
const CREATE_CHARACTER_POTENTIAL_WRITES: u64 = 2;
const SELECT_CHARACTER_POTENTIAL_WRITES: u64 = 2;
const ISSUE_TICKET_POTENTIAL_WRITES: u64 = 1;
const LOGOUT_POTENTIAL_WRITES: u64 = 2;
const MAX_IDEMPOTENT_REQUEST_ATTEMPTS: u64 = 3;

pub fn register_potential_writes() -> u64 {
    REGISTER_POTENTIAL_WRITES
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrepareCommand {
    Apply,
    Verify,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PrepareBudgetEstimate {
    pub command: PrepareCommand,
    pub account_count: u64,
    pub http_operations: u64,
    pub potential_data_writes: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AuthRunBudgetEstimate {
    pub virtual_player_slots: u32,
    pub scheduled_flows: u64,
    pub http_operations: u64,
    pub login_operations: u64,
    pub potential_data_writes: u64,
    pub scenario_duration_secs: u64,
    pub minimum_login_admission_ms: u64,
    pub minimum_connection_admission_ms: u64,
    pub minimum_business_message_admission_ms: u64,
    pub minimum_per_connection_message_admission_ms: u64,
    pub minimum_dispatch_admission_ms: u64,
}

/// The minimum player-protocol work emitted by the guarded KCP runner:
/// one connection, then `AuthReq` and `PingReq`. These values have no data
/// writes but must be reserved before an auth+game live run begins.
pub const MIN_GAME_CONNECTIONS_PER_FLOW: u64 = 1;
pub const MIN_GAME_MESSAGES_PER_FLOW: u64 = 2;

/// Room join, one or more PlayerInput messages, and leave can append game,
/// room, audit, and Redis state. This is intentionally an operation-level
/// upper bound, not a claim about a particular storage implementation.
pub const LIVE_GAMEPLAY_POTENTIAL_WRITES_PER_MESSAGE: u64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GameRunBudgetEstimate {
    pub kcp_connections_per_flow: u64,
    pub game_messages_per_flow: u64,
    pub gameplay_potential_writes_per_flow: u64,
}

pub fn estimate_game_run(scenario: &Scenario) -> Result<GameRunBudgetEstimate, String> {
    let Some(gameplay) = &scenario.live_gameplay else {
        return Ok(GameRunBudgetEstimate {
            kcp_connections_per_flow: MIN_GAME_CONNECTIONS_PER_FLOW,
            game_messages_per_flow: MIN_GAME_MESSAGES_PER_FLOW,
            gameplay_potential_writes_per_flow: 0,
        });
    };
    let business_messages = u64::from(gameplay.max_frame_inputs)
        .checked_add(2)
        .ok_or("live gameplay message estimate overflowed")?;
    let (extra_connections, extra_messages) = if gameplay.reconnect.is_some() {
        // close/reconnect, AuthReq again, and RoomReconnectReq.
        (1, 2)
    } else {
        (0, 0)
    };
    let game_messages_per_flow = MIN_GAME_MESSAGES_PER_FLOW
        .checked_add(business_messages)
        .and_then(|total| total.checked_add(extra_messages))
        .ok_or("live gameplay message estimate overflowed")?;
    let gameplay_potential_writes_per_flow = business_messages
        .checked_add(u64::from(extra_messages))
        .and_then(|count| count.checked_mul(LIVE_GAMEPLAY_POTENTIAL_WRITES_PER_MESSAGE))
        .ok_or("live gameplay write estimate overflowed")?;
    Ok(GameRunBudgetEstimate {
        kcp_connections_per_flow: MIN_GAME_CONNECTIONS_PER_FLOW + extra_connections,
        game_messages_per_flow,
        gameplay_potential_writes_per_flow,
    })
}

pub fn validate_game_run_budget(
    auth: &AuthRunBudgetEstimate,
    budget: &HardBudget,
) -> Result<(), String> {
    validate_game_run_budget_with_estimate(
        auth,
        budget,
        GameRunBudgetEstimate {
            kcp_connections_per_flow: MIN_GAME_CONNECTIONS_PER_FLOW,
            game_messages_per_flow: MIN_GAME_MESSAGES_PER_FLOW,
            gameplay_potential_writes_per_flow: 0,
        },
    )
}

pub fn validate_game_run_budget_for_scenario(
    auth: &AuthRunBudgetEstimate,
    scenario: &Scenario,
    budget: &HardBudget,
) -> Result<(), String> {
    validate_game_run_budget_with_estimate(auth, budget, estimate_game_run(scenario)?)
}

fn validate_game_run_budget_with_estimate(
    auth: &AuthRunBudgetEstimate,
    budget: &HardBudget,
    game: GameRunBudgetEstimate,
) -> Result<(), String> {
    let kcp_connections = auth
        .scheduled_flows
        .checked_mul(game.kcp_connections_per_flow)
        .ok_or("game connection budget estimate overflowed")?;
    let operations = auth
        .http_operations
        .checked_add(
            kcp_connections
                .checked_add(
                    auth.scheduled_flows
                        .checked_mul(game.game_messages_per_flow)
                        .ok_or("game operation budget estimate overflowed")?,
                )
                .ok_or("game operation budget estimate overflowed")?,
        )
        .ok_or("game operation budget estimate overflowed")?;
    if operations > budget.max_total_operations {
        return Err(format!(
            "auth+game scenario estimates {operations} total operations (including {kcp_connections} KCP connections), exceeding max_total_operations {}",
            budget.max_total_operations
        ));
    }
    let potential_data_writes = auth
        .potential_data_writes
        .checked_add(
            auth.scheduled_flows
                .checked_mul(game.gameplay_potential_writes_per_flow)
                .ok_or("game write budget estimate overflowed")?,
        )
        .ok_or("game write budget estimate overflowed")?;
    if potential_data_writes > budget.max_data_writes {
        return Err(format!(
            "auth+game scenario estimates {potential_data_writes} potential data writes, exceeding max_data_writes {}",
            budget.max_data_writes
        ));
    }
    let available_duration_ms = auth.scenario_duration_secs.saturating_mul(1_000);
    let connection_admission_ms =
        minimum_admission_ms(kcp_connections, budget.max_new_connections_per_second)?;
    let message_admission_ms = minimum_admission_ms(
        auth.http_operations.saturating_add(
            auth.scheduled_flows
                .saturating_mul(game.game_messages_per_flow),
        ),
        budget
            .max_business_messages_per_second
            .min(budget.max_messages_per_connection_per_second),
    )?;
    if connection_admission_ms >= available_duration_ms
        || message_admission_ms >= available_duration_ms
    {
        return Err("auth+game scenario cannot admit its KCP connection and player messages within the duration budget".into());
    }
    Ok(())
}

pub fn is_login_operation(operation: AuthOperation) -> bool {
    matches!(
        operation,
        AuthOperation::Login | AuthOperation::DuplicateLogin | AuthOperation::FailedLogin
    )
}

/// `me` and `list_characters` are the only requests eligible for two bounded
/// retries. Budget estimates reserve all possible transport attempts so a
/// retry cannot make a live run exceed its operation limit.
pub fn auth_operation_max_http_attempts(operation: AuthOperation) -> u64 {
    match operation {
        AuthOperation::Me | AuthOperation::ListCharacters => MAX_IDEMPOTENT_REQUEST_ATTEMPTS,
        _ => 1,
    }
}

/// Upper-bound mutable backend effects for a single request. This intentionally
/// counts session, ticket, audit and Redis side effects, not just SQL rows.
pub fn auth_operation_potential_writes(operation: AuthOperation) -> u64 {
    match operation {
        AuthOperation::Login | AuthOperation::DuplicateLogin => LOGIN_POTENTIAL_WRITES,
        // Authentication failure can still create the same Redis/rate-limit
        // and audit effects as a successful login, so reserve the larger cost.
        AuthOperation::FailedLogin => LOGIN_POTENTIAL_WRITES,
        AuthOperation::Me | AuthOperation::ListCharacters => 0,
        AuthOperation::CreateCharacter => CREATE_CHARACTER_POTENTIAL_WRITES,
        AuthOperation::SelectCharacter => SELECT_CHARACTER_POTENTIAL_WRITES,
        AuthOperation::IssueTicket => ISSUE_TICKET_POTENTIAL_WRITES,
        AuthOperation::Logout => LOGOUT_POTENTIAL_WRITES,
    }
}

pub fn estimate_prepare(
    command: PrepareCommand,
    account_count: u64,
) -> Result<PrepareBudgetEstimate, String> {
    if account_count == 0 {
        return Err("account preparation estimate requires at least one account".into());
    }
    let (http_per_account, writes_per_account) = match command {
        // register, login, list, create-if-missing, select, issue-ticket
        PrepareCommand::Apply => (
            6,
            REGISTER_POTENTIAL_WRITES
                + LOGIN_POTENTIAL_WRITES
                + CREATE_CHARACTER_POTENTIAL_WRITES
                + SELECT_CHARACTER_POTENTIAL_WRITES
                + ISSUE_TICKET_POTENTIAL_WRITES,
        ),
        // login, list, select, issue-ticket
        PrepareCommand::Verify => (
            4,
            LOGIN_POTENTIAL_WRITES
                + SELECT_CHARACTER_POTENTIAL_WRITES
                + ISSUE_TICKET_POTENTIAL_WRITES,
        ),
    };
    Ok(PrepareBudgetEstimate {
        command,
        account_count,
        http_operations: account_count
            .checked_mul(http_per_account)
            .ok_or("account preparation HTTP operation estimate overflowed")?,
        potential_data_writes: account_count
            .checked_mul(writes_per_account)
            .ok_or("account preparation write estimate overflowed")?,
    })
}

pub fn validate_prepare_budget(
    estimate: &PrepareBudgetEstimate,
    budget: &HardBudget,
) -> Result<(), String> {
    if estimate.http_operations > budget.max_total_operations {
        return Err(format!(
            "account preparation worst-case HTTP operations {} exceed max_total_operations {}",
            estimate.http_operations, budget.max_total_operations
        ));
    }
    if estimate.potential_data_writes > budget.max_data_writes {
        return Err(format!(
            "account preparation worst-case data writes {} exceed max_data_writes {}",
            estimate.potential_data_writes, budget.max_data_writes
        ));
    }
    Ok(())
}

pub fn estimate_auth_run(
    scenario: &Scenario,
    budget: &HardBudget,
) -> Result<AuthRunBudgetEstimate, String> {
    let auth = scenario
        .auth
        .as_ref()
        .ok_or("auth budget estimate requires scenario.auth")?;
    let virtual_player_slots = requested_player_slots(&scenario.load, budget.max_virtual_players)?;
    let scheduled_flows = scheduled_flow_count(&scenario.load)?;
    let http_attempts_per_flow = auth.operations.iter().try_fold(0_u64, |total, operation| {
        total
            .checked_add(auth_operation_max_http_attempts(*operation))
            .ok_or("auth HTTP operation estimate overflowed")
    })?;
    let http_operations = scheduled_flows
        .checked_mul(http_attempts_per_flow)
        .ok_or("auth HTTP operation estimate overflowed")?;
    let login_per_flow = auth
        .operations
        .iter()
        .filter(|operation| is_login_operation(**operation))
        .count() as u64;
    let login_operations = scheduled_flows
        .checked_mul(login_per_flow)
        .ok_or("auth login operation estimate overflowed")?;
    let writes_per_flow = auth.operations.iter().try_fold(0_u64, |total, operation| {
        total
            .checked_add(auth_operation_potential_writes(*operation))
            .ok_or("auth write estimate overflowed")
    })?;
    let potential_data_writes = scheduled_flows
        .checked_mul(writes_per_flow)
        .ok_or("auth write estimate overflowed")?;
    let scenario_duration_secs = load_duration_secs(&scenario.load)?;
    let minimum_login_admission_ms = minimum_admission_ms(login_operations, budget.max_login_qps)?;
    let minimum_connection_admission_ms =
        minimum_admission_ms(http_operations, budget.max_new_connections_per_second)?;
    let minimum_business_message_admission_ms =
        minimum_admission_ms(http_operations, budget.max_business_messages_per_second)?;
    let minimum_per_connection_message_admission_ms = minimum_admission_ms(
        http_operations,
        budget.max_messages_per_connection_per_second,
    )?;
    let minimum_dispatch_admission_ms = minimum_connection_admission_ms
        .max(minimum_business_message_admission_ms)
        .max(minimum_per_connection_message_admission_ms);
    Ok(AuthRunBudgetEstimate {
        virtual_player_slots,
        scheduled_flows,
        http_operations,
        login_operations,
        potential_data_writes,
        scenario_duration_secs,
        minimum_login_admission_ms,
        minimum_connection_admission_ms,
        minimum_business_message_admission_ms,
        minimum_per_connection_message_admission_ms,
        minimum_dispatch_admission_ms,
    })
}

pub fn validate_auth_run_budget(
    estimate: &AuthRunBudgetEstimate,
    budget: &HardBudget,
) -> Result<(), String> {
    if estimate.virtual_player_slots > budget.max_virtual_players {
        return Err(format!(
            "auth scenario requires {} virtual player slots but max_virtual_players is {}",
            estimate.virtual_player_slots, budget.max_virtual_players
        ));
    }
    if estimate.http_operations > budget.max_total_operations {
        return Err(format!(
            "auth scenario estimates {} HTTP operations but max_total_operations is {}",
            estimate.http_operations, budget.max_total_operations
        ));
    }
    if estimate.potential_data_writes > budget.max_data_writes {
        return Err(format!(
            "auth scenario estimates {} potential data writes but max_data_writes is {}",
            estimate.potential_data_writes, budget.max_data_writes
        ));
    }
    let available_duration_ms = estimate.scenario_duration_secs.saturating_mul(1_000);
    if estimate.login_operations > 1 && estimate.minimum_login_admission_ms >= available_duration_ms
    {
        return Err(format!(
            "auth scenario needs at least {} ms to admit {} login operations within max_login_qps, but its duration is {} ms",
            estimate.minimum_login_admission_ms, estimate.login_operations, available_duration_ms
        ));
    }
    if estimate.http_operations > 1
        && estimate.minimum_dispatch_admission_ms >= available_duration_ms
    {
        return Err(format!(
            "auth scenario needs at least {} ms to admit {} HTTP attempts within connection, business-message, and per-connection message limits, but its duration is {} ms",
            estimate.minimum_dispatch_admission_ms, estimate.http_operations, available_duration_ms
        ));
    }
    Ok(())
}

/// `staged` auth is a sequence of finite flow waves. Each stage must have
/// enough of its own window to admit every possible HTTP attempt in that wave;
/// the runtime also caps individual transport attempts at the same boundary.
pub fn validate_staged_auth_windows(
    scenario: &Scenario,
    budget: &HardBudget,
) -> Result<(), String> {
    let LoadModel::Staged { stages } = &scenario.load else {
        return Ok(());
    };
    let auth = scenario
        .auth
        .as_ref()
        .ok_or("staged auth window validation requires scenario.auth")?;
    let http_attempts_per_flow = auth.operations.iter().try_fold(0_u64, |total, operation| {
        total
            .checked_add(auth_operation_max_http_attempts(*operation))
            .ok_or("staged auth HTTP operation estimate overflowed")
    })?;
    let login_attempts_per_flow = auth
        .operations
        .iter()
        .filter(|operation| is_login_operation(**operation))
        .count() as u64;

    for stage in stages {
        let flow_count = u64::from(stage.virtual_players);
        let login_operations = flow_count
            .checked_mul(login_attempts_per_flow)
            .ok_or("staged auth login operation estimate overflowed")?;
        let http_operations = flow_count
            .checked_mul(http_attempts_per_flow)
            .ok_or("staged auth HTTP operation estimate overflowed")?;
        let minimum_login_admission_ms =
            minimum_admission_ms(login_operations, budget.max_login_qps)?;
        let minimum_dispatch_admission_ms = minimum_admission_ms(
            http_operations,
            budget
                .max_new_connections_per_second
                .min(budget.max_business_messages_per_second)
                .min(budget.max_messages_per_connection_per_second),
        )?;
        let stage_window_ms = stage.duration_secs.saturating_mul(1_000);
        if login_operations > 1 && minimum_login_admission_ms >= stage_window_ms {
            return Err(format!(
                "staged auth stage '{}' needs at least {} ms to admit {} login operations within max_login_qps, but its window is {} ms",
                stage.name, minimum_login_admission_ms, login_operations, stage_window_ms
            ));
        }
        if http_operations > 1 && minimum_dispatch_admission_ms >= stage_window_ms {
            return Err(format!(
                "staged auth stage '{}' needs at least {} ms to admit {} HTTP attempts within connection, business-message, and per-connection message limits, but its window is {} ms",
                stage.name, minimum_dispatch_admission_ms, http_operations, stage_window_ms
            ));
        }
    }
    Ok(())
}

/// Fails closed before a request if either operation or mutable-effect budget
/// would be exceeded. Callers map that failure into the shared abort state.
#[derive(Debug, Clone)]
pub struct RuntimeAuthQuota {
    max_operations: u64,
    max_data_writes: u64,
    used_operations: u64,
    used_data_writes: u64,
}

impl RuntimeAuthQuota {
    pub fn new(budget: &HardBudget) -> Self {
        Self {
            max_operations: budget.max_total_operations,
            max_data_writes: budget.max_data_writes,
            used_operations: 0,
            used_data_writes: 0,
        }
    }

    pub fn admit(&mut self, operation: AuthOperation) -> Result<(), String> {
        self.admit_potential_writes(auth_operation_potential_writes(operation))
    }

    pub fn admit_potential_writes(&mut self, potential_writes: u64) -> Result<(), String> {
        let next_operations = self
            .used_operations
            .checked_add(1)
            .ok_or("runtime operation quota overflowed")?;
        let next_writes = self
            .used_data_writes
            .checked_add(potential_writes)
            .ok_or("runtime write quota overflowed")?;
        if next_operations > self.max_operations || next_writes > self.max_data_writes {
            return Err("runtime auth quota would exceed configured hard budget".into());
        }
        self.used_operations = next_operations;
        self.used_data_writes = next_writes;
        Ok(())
    }

    pub fn admit_or_abort(
        &mut self,
        operation: AuthOperation,
        abort: &mut AbortController,
    ) -> Result<(), String> {
        self.admit(operation).map_err(|error| {
            abort.request(AbortReason::BudgetExceeded);
            error
        })
    }

    pub fn admit_potential_writes_or_abort(
        &mut self,
        potential_writes: u64,
        abort: &mut AbortController,
    ) -> Result<(), String> {
        self.admit_potential_writes(potential_writes)
            .map_err(|error| {
                abort.request(AbortReason::BudgetExceeded);
                error
            })
    }

    pub fn used_operations(&self) -> u64 {
        self.used_operations
    }

    pub fn used_data_writes(&self) -> u64 {
        self.used_data_writes
    }
}

fn requested_player_slots(model: &LoadModel, arrival_slot_ceiling: u32) -> Result<u32, String> {
    match model {
        LoadModel::FixedConcurrency {
            virtual_players, ..
        } => Ok(*virtual_players),
        LoadModel::Staged { stages } => stages
            .iter()
            .map(|stage| stage.virtual_players)
            .max()
            .ok_or("staged auth scenario has no virtual player slots".into()),
        LoadModel::Burst { burst_size, .. } => Ok(*burst_size),
        // Arrival-rate scenarios do not declare a concurrency target. Reserve
        // the profile ceiling explicitly instead of silently clipping a larger
        // inferred requirement at execution time.
        LoadModel::ArrivalRate { .. } => Ok(arrival_slot_ceiling),
    }
}

fn scheduled_flow_count(model: &LoadModel) -> Result<u64, String> {
    match model {
        LoadModel::FixedConcurrency {
            virtual_players, ..
        } => Ok(u64::from(*virtual_players)),
        LoadModel::Staged { stages } => stages.iter().try_fold(0_u64, |total, stage| {
            total
                .checked_add(u64::from(stage.virtual_players))
                .ok_or("staged auth flow estimate overflowed".into())
        }),
        LoadModel::Burst {
            burst_size,
            every_secs,
            duration_secs,
        } => ceil_div(*duration_secs, *every_secs)
            .checked_mul(u64::from(*burst_size))
            .ok_or("burst auth flow estimate overflowed".into()),
        LoadModel::ArrivalRate {
            arrivals_per_second,
            duration_secs,
        } => {
            let value = arrivals_per_second * *duration_secs as f64;
            if !value.is_finite() || value > u64::MAX as f64 {
                return Err("arrival-rate auth flow estimate is not representable".into());
            }
            Ok(value.ceil() as u64)
        }
    }
}

fn load_duration_secs(model: &LoadModel) -> Result<u64, String> {
    match model {
        LoadModel::FixedConcurrency { duration_secs, .. }
        | LoadModel::ArrivalRate { duration_secs, .. }
        | LoadModel::Burst { duration_secs, .. } => Ok(*duration_secs),
        LoadModel::Staged { stages } => stages.iter().try_fold(0_u64, |total, stage| {
            total
                .checked_add(stage.duration_secs)
                .ok_or("staged auth duration estimate overflowed".into())
        }),
    }
}

fn minimum_admission_ms(login_operations: u64, max_login_qps: f64) -> Result<u64, String> {
    if login_operations <= 1 {
        return Ok(0);
    }
    let duration = ((login_operations - 1) as f64 * 1_000.0 / max_login_qps).ceil();
    if !duration.is_finite() || duration > u64::MAX as f64 {
        return Err("login admission duration estimate is not representable".into());
    }
    Ok(duration as u64)
}

fn ceil_div(numerator: u64, denominator: u64) -> u64 {
    numerator / denominator + u64::from(numerator % denominator != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_http::AuthDispatchAdmission;
    use crate::config::{AuthScenario, LiveGameplayReconnect, LiveGameplayScenario, LoadStage};

    fn budget() -> HardBudget {
        HardBudget {
            max_virtual_players: 2,
            max_login_qps: 2.0,
            max_new_connections_per_second: 2.0,
            max_business_messages_per_second: 2.0,
            max_messages_per_connection_per_second: 2.0,
            max_duration_secs: 10,
            max_total_operations: 10,
            max_error_rate: 0.1,
            max_connection_failure_rate: 0.1,
            max_p99_ms: 1_000,
            max_data_writes: 20,
        }
    }

    fn auth_scenario(load: LoadModel) -> Scenario {
        Scenario {
            name: "auth".into(),
            load,
            steps: Vec::new(),
            writes_data: false,
            auth: Some(AuthScenario {
                operations: vec![AuthOperation::Login],
                allow_same_account_concurrency: false,
                same_account_session_effect: None,
            }),
            reconnect_burst: None,
            live_gameplay: None,
        }
    }

    #[test]
    fn prepare_estimate_rejects_worst_case_operation_and_write_budgets() {
        let mut budget = budget();
        budget.max_data_writes = 11;
        let estimate = estimate_prepare(PrepareCommand::Apply, 1).unwrap();
        assert_eq!(estimate.potential_data_writes, 12);
        assert!(
            validate_prepare_budget(&estimate, &budget)
                .unwrap_err()
                .contains("max_data_writes")
        );
        let operation_budget = HardBudget {
            max_total_operations: 5,
            max_data_writes: 12,
            ..budget.clone()
        };
        assert!(
            validate_prepare_budget(&estimate, &operation_budget)
                .unwrap_err()
                .contains("max_total_operations")
        );
        assert!(
            validate_prepare_budget(
                &estimate_prepare(PrepareCommand::Verify, 1).unwrap(),
                &HardBudget {
                    max_data_writes: 6,
                    ..budget
                },
            )
            .is_ok()
        );
    }

    #[test]
    fn fixed_burst_and_arrival_plans_fail_closed_without_silent_clipping() {
        let mut fixed_budget = budget();
        fixed_budget.max_login_qps = 1.0;
        let fixed = estimate_auth_run(
            &auth_scenario(LoadModel::FixedConcurrency {
                virtual_players: 2,
                duration_secs: 1,
            }),
            &fixed_budget,
        )
        .unwrap();
        assert!(validate_auth_run_budget(&fixed, &fixed_budget).is_err());

        let burst = estimate_auth_run(
            &auth_scenario(LoadModel::Burst {
                burst_size: 3,
                every_secs: 1,
                duration_secs: 1,
            }),
            &budget(),
        )
        .unwrap();
        assert!(validate_auth_run_budget(&burst, &budget()).is_err());

        let mut arrival_budget = budget();
        arrival_budget.max_total_operations = 5;
        let arrival = estimate_auth_run(
            &auth_scenario(LoadModel::ArrivalRate {
                arrivals_per_second: 6.0,
                duration_secs: 1,
            }),
            &arrival_budget,
        )
        .unwrap();
        assert_eq!(arrival.http_operations, 6);
        assert!(validate_auth_run_budget(&arrival, &arrival_budget).is_err());

        let staged = estimate_auth_run(
            &auth_scenario(LoadModel::Staged {
                stages: vec![LoadStage {
                    name: "over".into(),
                    virtual_players: 3,
                    duration_secs: 1,
                }],
            }),
            &budget(),
        )
        .unwrap();
        assert_eq!(staged.virtual_player_slots, 3);
        assert!(validate_auth_run_budget(&staged, &budget()).is_err());
    }

    #[test]
    fn game_runner_reserves_connection_and_two_player_messages_before_execution() {
        let scenario = auth_scenario(LoadModel::FixedConcurrency {
            virtual_players: 1,
            duration_secs: 10,
        });
        let estimate = estimate_auth_run(&scenario, &budget()).unwrap();
        let mut too_small = budget();
        too_small.max_total_operations = estimate.http_operations + 2;
        assert!(
            validate_game_run_budget(&estimate, &too_small)
                .unwrap_err()
                .contains("total operations")
        );

        let mut admitted = AuthDispatchAdmission::new(&budget()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        admitted.admit_game_connection(deadline, || Ok(())).unwrap();
        admitted.admit_game_message(deadline, || Ok(())).unwrap();
        admitted.admit_game_message(deadline, || Ok(())).unwrap();
        assert_eq!(admitted.used_operations(), 3);
        assert_eq!(admitted.used_data_writes(), 0);
    }

    #[test]
    fn game_connection_rate_counts_only_kcp_connections() {
        let scenario = auth_scenario(LoadModel::FixedConcurrency {
            virtual_players: 1,
            duration_secs: 10,
        });
        let estimate = estimate_auth_run(&scenario, &budget()).unwrap();
        let mut constrained = budget();
        constrained.max_total_operations = estimate.http_operations + 3;
        constrained.max_new_connections_per_second = 0.2;
        constrained.max_business_messages_per_second = 1.0;
        constrained.max_messages_per_connection_per_second = 1.0;

        // The existing auth estimate accounts for its HTTP connection. The
        // game extension must reserve only the additional KCP connection.
        assert!(validate_game_run_budget(&estimate, &constrained).is_ok());
    }

    #[test]
    fn live_gameplay_budget_reserves_messages_connections_and_mutable_effects() {
        let mut scenario = auth_scenario(LoadModel::FixedConcurrency {
            virtual_players: 1,
            duration_secs: 30,
        });
        scenario.writes_data = true;
        scenario.live_gameplay = Some(LiveGameplayScenario {
            room_id: "approved-room".into(),
            policy_id: "approved-policy".into(),
            profile: crate::gameplay::PlayerProfile::Normal,
            lockstep_scenario_json: include_str!("../../lockstep-client/scenarios/move_stop.json")
                .into(),
            max_frame_inputs: 1,
            reconnect: Some(LiveGameplayReconnect {
                last_character_push_sequence: 0,
                reconnect_policy: crate::config::ReconnectPolicyConfig {
                    max_attempts: 1,
                    base_delay_ms: 100,
                    max_delay_ms: 100,
                    max_jitter_ms: 0,
                },
            }),
        });
        let estimate = estimate_auth_run(&scenario, &budget()).unwrap();
        let game = estimate_game_run(&scenario).unwrap();
        assert_eq!(game.kcp_connections_per_flow, 2);
        assert_eq!(game.game_messages_per_flow, 7);
        assert_eq!(game.gameplay_potential_writes_per_flow, 20);

        let mut constrained = budget();
        constrained.max_total_operations = estimate.http_operations + 8;
        constrained.max_data_writes = estimate.potential_data_writes + 19;
        assert!(validate_game_run_budget_for_scenario(&estimate, &scenario, &constrained).is_err());
    }

    #[test]
    fn staged_auth_wave_rejects_a_window_that_cannot_admit_its_logins() {
        let mut staged_budget = budget();
        staged_budget.max_virtual_players = 3;
        staged_budget.max_total_operations = 3;
        staged_budget.max_data_writes = 9;
        let scenario = auth_scenario(LoadModel::Staged {
            stages: vec![LoadStage {
                name: "too-fast".into(),
                virtual_players: 3,
                duration_secs: 1,
            }],
        });

        let error = validate_staged_auth_windows(&scenario, &staged_budget).unwrap_err();
        assert!(error.contains("stage 'too-fast'"));
        assert!(error.contains("max_login_qps"));
    }

    #[test]
    fn runtime_quota_rejects_the_next_operation_before_exceeding_budget() {
        let mut quota = RuntimeAuthQuota::new(&HardBudget {
            max_total_operations: 1,
            max_data_writes: 3,
            ..budget()
        });
        quota.admit(AuthOperation::Login).unwrap();
        assert_eq!(quota.used_operations(), 1);
        assert_eq!(quota.used_data_writes(), 3);
        assert!(quota.admit(AuthOperation::Me).is_err());
    }

    #[test]
    fn runtime_quota_uses_the_shared_abort_reason_when_admission_is_exhausted() {
        let mut quota = RuntimeAuthQuota::new(&HardBudget {
            max_total_operations: 1,
            max_data_writes: 3,
            ..budget()
        });
        let mut abort = AbortController::default();
        quota
            .admit_or_abort(AuthOperation::Login, &mut abort)
            .unwrap();
        assert!(quota.admit_or_abort(AuthOperation::Me, &mut abort).is_err());
        assert_eq!(abort.reason(), Some(&AbortReason::BudgetExceeded));
    }

    #[test]
    fn read_retries_are_reserved_by_the_operation_budget() {
        let mut budget = budget();
        budget.max_total_operations = 3;
        let scenario = Scenario {
            auth: Some(AuthScenario {
                operations: vec![AuthOperation::Login, AuthOperation::Me],
                allow_same_account_concurrency: false,
                same_account_session_effect: None,
            }),
            ..auth_scenario(LoadModel::FixedConcurrency {
                virtual_players: 1,
                duration_secs: 10,
            })
        };

        let estimate = estimate_auth_run(&scenario, &budget).unwrap();
        assert_eq!(estimate.http_operations, 4);
        assert!(validate_auth_run_budget(&estimate, &budget).is_err());
    }
}
