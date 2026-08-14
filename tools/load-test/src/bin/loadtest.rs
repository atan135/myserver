use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use loadtest_core::SCHEMA_VERSION;
use loadtest_core::abort::{
    AbortController, AbortReason, GracefulShutdown, ShutdownPhase, install_ctrl_c_flag,
};
use loadtest_core::accounts::{
    AccountLeasePool, EnvironmentSecretProvider, SecretProvider, read_manifest,
};
use loadtest_core::auth_budget::{
    LIVE_GAMEPLAY_POTENTIAL_WRITES_PER_MESSAGE, estimate_auth_run, validate_auth_run_budget,
    validate_game_run_budget_for_scenario, validate_staged_auth_windows,
};
use loadtest_core::auth_http::{
    AuthAdmissionError, AuthDispatchAdmission, AuthRunMetrics, FakeAuthHttpService,
    FakeAuthOutcome, ReqwestAuthHttpTransport, execute_auth_operations, execute_deferred_logout,
    split_game_auth_operations,
};
use loadtest_core::calibration::{
    CalibrationRun, bounded_calibration_duration_ms, bounded_calibration_operations,
    progressive_levels, run_local_workload,
};
use loadtest_core::chat_wss::execute_live_chat_steps;
use loadtest_core::config::{
    BudgetOverride, EnvironmentKind, LiveGameplayCoordination, LoadModel, LoadTestConfig,
    RunAccess, load_config, load_private_config,
};
use loadtest_core::contracts::{RunPlan, single_process_assignment};
use loadtest_core::game_kcp::{GameProxyEndpoint, ReconnectPolicy};
use loadtest_core::game_live::{
    GameExecutionGate, GameLiveError, GameRunnerCheckpoint, GameSessionRunner, LiveKcpTransport,
};
use loadtest_core::lifecycle::{Lifecycle, RunState};
use loadtest_core::match_grpc::{
    MatchInternalAdmission, execute_live_match_internal_steps, execute_live_match_steps,
};
use loadtest_core::metrics::Metrics;
use loadtest_core::preflight::summarize_run;
use loadtest_core::protection::{DryRunProtection, revalidate_or_abort};
use loadtest_core::reconnect_burst::{
    ReconnectBurstSpec, ReconnectBurstStep, plan_reconnect_burst,
};
use loadtest_core::report::{ErrorBuffer, ReportInput, write_report};
use loadtest_core::resource::ResourceSampler;
use loadtest_core::scheduler::MonotonicScheduler;
use loadtest_core::side_http::{SideHttpAdmission, execute_live_mail_announce_steps};
use loadtest_core::side_services::SideServiceKind;
use loadtest_core::side_services::execute_side_services_dry;

static NEXT_MATCH_INTERNAL_DIAGNOSTIC_ID: AtomicU64 = AtomicU64::new(1);

fn main() -> ExitCode {
    match execute(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("loadtest: {error}");
            ExitCode::from(2)
        }
    }
}

fn execute(arguments: Vec<String>) -> Result<(), String> {
    let parsed = Cli::parse(arguments)?;
    match parsed.command.as_str() {
        "validate" => {
            let config = parsed.load()?;
            validate(&config, &parsed)?;
            println!("configuration is valid for {}", config.environment.name);
        }
        "run" => run(&parsed)?,
        "calibrate" => calibrate_dry(&parsed)?,
        "report" => show_report(
            parsed
                .report_dir
                .as_deref()
                .ok_or("report requires --report-dir <directory>")?,
        )?,
        _ => return Err(usage()),
    }
    Ok(())
}

fn run(cli: &Cli) -> Result<(), String> {
    if cli.dry_run {
        run_dry(cli)
    } else {
        run_live(cli)
    }
}

fn validate(config: &LoadTestConfig, cli: &Cli) -> Result<(), String> {
    config
        .validate_access(RunAccess {
            allow_remote: cli.allow_remote,
            confirmation: cli.confirmation.as_deref(),
        })
        .map_err(|error| error.to_string())
}

fn run_dry(cli: &Cli) -> Result<(), String> {
    if !cli.dry_run {
        return Err(
            "calibrate requires --dry-run; auth run requires --dry-run or the explicit --execute-auth gate"
                .into(),
        );
    }
    let config = cli.load()?;
    validate(&config, cli)?;
    let budget = config
        .effective_budget(&cli.budget_override)
        .map_err(|error| error.to_string())?;
    let started = unix_ms();
    let deadline_unix_ms = effective_deadline(&config, &budget, cli.deadline_unix_ms, started)?;
    let preflight = summarize_run(
        &cli.command,
        &config,
        &budget,
        RunAccess {
            allow_remote: cli.allow_remote,
            confirmation: cli.confirmation.as_deref(),
        },
        deadline_unix_ms,
        cli.dry_run,
        false,
    )?;
    println!(
        "preflight={}",
        serde_json::to_string(&preflight).expect("preflight summary serializes")
    );
    let run_id = format!(
        "{}-{}-{}",
        if cli.command == "calibrate" {
            "calibrate"
        } else {
            "dry-run"
        },
        std::process::id(),
        started
    );
    let plan = RunPlan {
        schema_version: SCHEMA_VERSION,
        run_id: run_id.clone(),
        environment: config.environment.name.clone(),
        scenario_name: config.scenario.name.clone(),
        budget: budget.clone(),
        planned_start_unix_ms: started,
    };
    let assignment = single_process_assignment(&plan, budget.max_virtual_players, started);
    let mut lifecycle = Lifecycle::default();
    lifecycle.transition(RunState::Validated).unwrap();
    lifecycle.transition(RunState::WarmingUp).unwrap();
    let mut metrics = Metrics::default();
    metrics.increment("virtual_players", assignment.virtual_player_count as u64);
    let ctrl_c = install_ctrl_c_flag()
        .map_err(|error| format!("failed to install Ctrl+C handler: {error}"))?;
    let mut abort = AbortController::default();
    abort.check_ctrl_c(&ctrl_c);
    abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
    abort.check_deadline(unix_ms(), deadline_unix_ms);
    let protection = DryRunProtection::new(&config);
    let mut protection_error = revalidate_or_abort(&protection, &mut abort);

    let mut scheduler = MonotonicScheduler::new(
        &config.scenario.load,
        100,
        budget.max_virtual_players as usize,
    );
    let tick = if abort.should_stop_new_sessions() {
        Default::default()
    } else {
        lifecycle.transition(RunState::Ramping).unwrap();
        lifecycle.transition(RunState::Steady).unwrap();
        scheduler.due(0)
    };
    metrics.increment("started", tick.actions.len() as u64);
    metrics.increment(
        "scheduler_lag_ms",
        tick.actions
            .iter()
            .map(|action| action.scheduler_lag_ms)
            .sum(),
    );
    metrics.increment("scheduler_queue_depth", tick.queue_depth);
    metrics.increment("metrics_dropped", tick.dropped);
    if !abort.should_stop_new_sessions() {
        if let Some(error) = revalidate_or_abort(&protection, &mut abort) {
            protection_error = Some(error);
        }
    }
    let auth_metrics = if !abort.should_stop_new_sessions() {
        dry_run_auth_metrics(&config, &protection, &mut abort)?
    } else {
        None
    };
    if let Some(auth) = &auth_metrics {
        record_auth_metrics(&mut metrics, auth);
    }
    if !abort.should_stop_new_sessions() {
        if let Some(side_services) = &config.scenario.side_services {
            execute_side_services_dry(side_services, &budget, &mut metrics)?;
        }
    }
    if !abort.should_stop_new_sessions() {
        if let Some(reconnect) = &config.scenario.reconnect_burst {
            let plan = plan_reconnect_burst(
                ReconnectBurstSpec {
                    virtual_players: reconnect.virtual_players,
                    reconnect_attempts_per_player: reconnect.reconnect_attempts_per_player,
                    start_ms: 0,
                },
                &budget,
                reconnect.reconnect_policy.into(),
            )
            .map_err(|error| error.to_string())?;
            metrics.increment("reconnect_burst_login_actions", plan.login_actions);
            metrics.increment("reconnect_burst_new_connections", plan.new_connections);
            metrics.increment(
                "reconnect_burst_room_recoveries",
                plan.actions
                    .iter()
                    .filter(|action| action.step == ReconnectBurstStep::RecoverRoom)
                    .count() as u64,
            );
            metrics.increment("reconnect_burst_backoff_ms", plan.total_backoff_ms);
            metrics.increment(
                "reconnect_burst_potential_data_writes",
                plan.potential_data_writes,
            );
            println!(
                "reconnect_burst_plan={} ",
                serde_json::to_string(&plan).expect("reconnect burst plan serializes")
            );
        }
    }

    let (status, shutdown_phase) = if abort.should_stop_new_sessions() {
        lifecycle.transition(RunState::Aborting).unwrap();
        let mut shutdown = GracefulShutdown::new(config.graceful_shutdown_ms);
        shutdown.begin(0);
        while shutdown.phase() != ShutdownPhase::Completed {
            let now = match shutdown.phase() {
                ShutdownPhase::GracefulDraining => config.graceful_shutdown_ms,
                _ => 0,
            };
            let active_sessions = u64::from(tick.actions.len() as u32);
            shutdown.advance(now, active_sessions);
        }
        lifecycle.transition(RunState::Aborted).unwrap();
        ("aborted", Some(format!("{:?}", shutdown.phase())))
    } else {
        lifecycle.transition(RunState::CoolingDown).unwrap();
        lifecycle.transition(RunState::Completed).unwrap();
        ("completed", None)
    };
    let resources = ResourceSampler.sample(0, tick.queue_depth, tick.dropped);
    let mut errors = ErrorBuffer::default();
    if let Some(reason) = abort.reason() {
        errors.push(
            "run_abort",
            format!("dry run stopped: {reason:?}"),
            Default::default(),
        );
    }
    if protection_error.is_some() {
        errors.push(
            "protection_unknown",
            "target protection revalidation could not be confirmed",
            Default::default(),
        );
    }
    let abort_reason = abort.reason().map(|reason| format!("{reason:?}"));
    let report = write_report(
        Path::new(&config.reports_root),
        ReportInput {
            run_id: &run_id,
            config: &config,
            effective_budget: &budget,
            status,
            abort_reason: abort_reason.as_deref(),
            shutdown_phase: shutdown_phase.as_deref(),
            deadline_unix_ms,
            graceful_shutdown_ms: config.graceful_shutdown_ms,
            started_unix_ms: started,
            ended_unix_ms: unix_ms(),
            metrics: metrics.snapshot(),
            resources,
            errors: &errors,
            auth_metrics: auth_metrics.as_ref(),
            calibration: None,
        },
    )
    .map_err(|error| error.to_string())?;
    println!(
        "{} finished without connecting to services (status={status}). report={}",
        cli.command,
        report.display()
    );
    Ok(())
}

fn calibrate_dry(cli: &Cli) -> Result<(), String> {
    if !cli.dry_run {
        return Err("calibrate requires --dry-run".into());
    }
    let config = cli.load()?;
    validate(&config, cli)?;
    let budget = config
        .effective_budget(&cli.budget_override)
        .map_err(|error| error.to_string())?;
    let planned_calibration_operations =
        bounded_calibration_operations(budget.max_virtual_players, config.calibration);
    if planned_calibration_operations > budget.max_total_operations {
        return Err(format!(
            "calibration would schedule {planned_calibration_operations} synthetic operations, exceeding max_total_operations {}",
            budget.max_total_operations
        ));
    }
    let calibration_duration_ms =
        bounded_calibration_duration_ms(budget.max_virtual_players, config.calibration);
    if calibration_duration_ms > budget.max_duration_secs.saturating_mul(1_000) {
        return Err(format!(
            "calibration would run for {calibration_duration_ms}ms, exceeding max_duration_secs {}",
            budget.max_duration_secs
        ));
    }
    let started = unix_ms();
    let deadline_unix_ms = effective_deadline(&config, &budget, cli.deadline_unix_ms, started)?;
    let preflight = summarize_run(
        "calibrate",
        &config,
        &budget,
        RunAccess {
            allow_remote: cli.allow_remote,
            confirmation: cli.confirmation.as_deref(),
        },
        deadline_unix_ms,
        true,
        false,
    )?;
    println!(
        "preflight={}",
        serde_json::to_string(&preflight).expect("preflight summary serializes")
    );

    let sampler = ResourceSampler;
    let mut calibration = CalibrationRun::new(config.calibration);
    let mut metrics = Metrics::default();
    let mut final_resources = sampler.sample(0, 0, 0);
    let protection = DryRunProtection::new(&config);
    let mut abort = AbortController::default();
    let mut errors = ErrorBuffer::default();
    let ctrl_c = install_ctrl_c_flag()
        .map_err(|error| format!("failed to install Ctrl+C handler: {error}"))?;
    for players in progressive_levels(budget.max_virtual_players) {
        abort.check_deadline(unix_ms(), deadline_unix_ms);
        abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
        abort.check_ctrl_c(&ctrl_c);
        if !calibration.should_continue() || abort.should_stop_new_sessions() {
            break;
        }
        if let Some(error) = revalidate_or_abort(&protection, &mut abort) {
            errors.push("protection_unknown", error, Default::default());
            break;
        }
        let before = sampler.sample(0, 0, 0);
        let workload = run_local_workload(players, config.calibration);
        let resources = sampler.sample(
            workload.max_scheduler_lag_ms,
            workload.max_queue_depth,
            workload.dropped_actions,
        );
        let level = calibration.observe(players, workload, &before, &resources);
        metrics.increment("virtual_players", players as u64);
        metrics.increment("started", level.scheduled_actions);
        metrics.increment("scheduler_lag_ms", level.scheduler_lag_ms.unwrap_or(0));
        metrics.increment("scheduler_queue_depth", level.max_queue_depth);
        metrics.increment("metrics_dropped", level.dropped_actions);
        println!(
            "calibration_level={}",
            serde_json::to_string(&level).expect("calibration level serializes")
        );
        final_resources = resources;
    }
    let calibration = calibration.finish(None);
    let abort_reason = abort.reason().map(|reason| format!("{reason:?}"));
    let run_id = format!("calibrate-{}-{}", std::process::id(), started);
    let report = write_report(
        Path::new(&config.reports_root),
        ReportInput {
            run_id: &run_id,
            config: &config,
            effective_budget: &budget,
            status: if abort.should_stop_new_sessions() {
                "aborted"
            } else {
                "completed"
            },
            abort_reason: abort_reason.as_deref(),
            shutdown_phase: None,
            deadline_unix_ms,
            graceful_shutdown_ms: config.graceful_shutdown_ms,
            started_unix_ms: started,
            ended_unix_ms: unix_ms(),
            metrics: metrics.snapshot(),
            resources: final_resources,
            errors: &errors,
            auth_metrics: None,
            calibration: Some(&calibration),
        },
    )
    .map_err(|error| error.to_string())?;
    println!(
        "calibrate finished without connecting to services. report={}",
        report.display()
    );
    Ok(())
}

fn dry_run_auth_metrics(
    config: &LoadTestConfig,
    protection: &DryRunProtection<'_>,
    abort: &mut AbortController,
) -> Result<Option<AuthRunMetrics>, String> {
    let Some(auth) = &config.scenario.auth else {
        return Ok(None);
    };
    if revalidate_or_abort(protection, abort).is_some() || abort.should_stop_new_sessions() {
        return Ok(None);
    }
    let outcomes = auth.operations.iter().map(|operation| {
        if matches!(operation, loadtest_core::config::AuthOperation::FailedLogin) {
            FakeAuthOutcome::BusinessError
        } else {
            FakeAuthOutcome::Success
        }
    });
    let mut transport = FakeAuthHttpService::scripted(outcomes);
    let started = Instant::now();
    let mut execution = execute_auth_operations(
        &mut transport,
        &auth.operations,
        &format!("{}_dry", config.account_prepare.character_name_prefix),
        "loadtest_dry_run",
        "offline-only-password",
        |_, _| Ok(Duration::MAX),
    );
    execution
        .metrics
        .set_wall_clock_window_ms(started.elapsed().as_millis() as u64);
    if let Some(error) = execution.error {
        return Err(error);
    }
    if revalidate_or_abort(protection, abort).is_some() {
        return Ok(None);
    }
    Ok(Some(execution.metrics))
}

fn record_auth_metrics(metrics: &mut Metrics, auth: &AuthRunMetrics) {
    metrics.increment("auth_requests", auth.requests);
    metrics.increment("auth_login_requests", auth.login_requests);
    metrics.increment("auth_login_successes", auth.login_successes);
    metrics.increment("auth_connection_failures", auth.connection_failures);
    metrics.increment("auth_ticket_attempts", auth.ticket_attempts);
    metrics.increment("auth_ticket_successes", auth.ticket_successes);
    metrics.increment("auth_rate_limited", auth.rate_limited);
    if auth.login_latency_ms.count() > 0 {
        metrics.merge_latency("login_ms", &auth.login_latency_ms);
    }
    if auth.ticket_latency_ms.count() > 0 {
        metrics.merge_latency("ticket_ms", &auth.ticket_latency_ms);
    }
    if auth.latency_ms.count() > 0 {
        metrics.merge_latency("auth_operation_ms", &auth.latency_ms);
    }
}

/// Every scheduled action has already performed some auth work before it can
/// reach a game follow-up failure. Keep the merge and threshold check in one
/// terminal path so a missing ticket or rejected KCP admission cannot make
/// completed auth requests disappear from the report.
fn finish_live_action(
    aggregate: &mut AuthRunMetrics,
    action: &AuthRunMetrics,
    abort: &mut AbortController,
    budget: &loadtest_core::config::HardBudget,
) {
    aggregate.merge(action);
    let successes = aggregate
        .outcomes
        .get(&loadtest_core::auth_http::AuthOutcomeCategory::Success)
        .copied()
        .unwrap_or(0);
    let error_rate = if aggregate.requests == 0 {
        0.0
    } else {
        1.0 - successes as f64 / aggregate.requests as f64
    };
    abort.check_thresholds(
        error_rate,
        aggregate.connection_failure_rate(),
        aggregate.p99_ms(),
        budget.max_error_rate,
        budget.max_connection_failure_rate,
        budget.max_p99_ms,
        true,
    );
}

fn can_attempt_deferred_logout(
    deferred_logout: bool,
    pre_game_auth_completed: bool,
    abort: &AbortController,
) -> bool {
    deferred_logout && pre_game_auth_completed && !abort.should_stop_new_sessions()
}

fn deferred_logout_skip_message(abort: &AbortController) -> &'static str {
    match abort.reason() {
        Some(AbortReason::BudgetExceeded) => {
            "post-game logout was not dispatched because the operation budget was exhausted"
        }
        Some(AbortReason::Deadline) => {
            "post-game logout was not dispatched because the action deadline elapsed"
        }
        Some(AbortReason::ProtectionUnknown) => {
            "post-game logout was not dispatched because target protection could not be confirmed"
        }
        Some(AbortReason::CtrlC) => {
            "post-game logout was not dispatched because Ctrl+C stopped execution"
        }
        Some(AbortReason::StopFile) => {
            "post-game logout was not dispatched because the stop file stopped execution"
        }
        Some(_) => "post-game logout was not dispatched because execution was already aborting",
        None => "post-game logout was not dispatched",
    }
}

fn record_completed_game_session_metrics(core_metrics: &mut Metrics) {
    core_metrics.increment("game_sessions_completed", 1);
    core_metrics.increment("game_auth_requests", 1);
    core_metrics.increment("game_heartbeat_requests", 1);
}

fn game_failure_category(error: &GameLiveError) -> &'static str {
    error
        .reportable_failure_category()
        .unwrap_or("game_runner_transport_or_contract_failed")
}

fn finish_game_action_after_cleanup<C, R>(
    completed_game_session: bool,
    cleanup: C,
    record_completed_session: R,
) where
    C: FnOnce(),
    R: FnOnce(),
{
    cleanup();
    if completed_game_session {
        record_completed_session();
    }
}

/// A player-facing KCP match can share the ticket obtained during the same
/// auth flow with public chat/mail/announce calls, but only in an explicitly
/// local or test two-player flow. Direct match-service diagnostics remain a
/// separate transport and cannot be mixed with the formal player path.
fn validate_live_game_side_service_composite(
    environment: EnvironmentKind,
    game_mode: bool,
    two_player_default_match: bool,
    live_chat: bool,
    live_match: bool,
    live_match_internal: bool,
    live_http: bool,
) -> Result<bool, String> {
    if !game_mode {
        return Ok(false);
    }
    if live_match || live_match_internal {
        return Err(
            "direct match-service gRPC diagnostics cannot be combined with --execute-game; use the player KCP match path"
                .into(),
        );
    }
    if !(live_chat || live_http) {
        return Ok(false);
    }
    if !matches!(environment, EnvironmentKind::Local | EnvironmentKind::Test) {
        return Err(
            "live game + side-service composite execution is limited to local/test environments"
                .into(),
        );
    }
    if !two_player_default_match {
        return Err(
            "live game + side-service composite execution requires two-player default_match coordination"
                .into(),
        );
    }
    Ok(true)
}

fn check_live_composite_side_controller(
    config: &LoadTestConfig,
    deadline_unix_ms: u64,
    ctrl_c: &AtomicBool,
    protection: &DryRunProtection<'_>,
    abort: &mut AbortController,
) -> Result<(), String> {
    abort.check_ctrl_c(ctrl_c);
    abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
    abort.check_deadline(unix_ms(), deadline_unix_ms);
    if abort.should_stop_new_sessions() || revalidate_or_abort(protection, abort).is_some() {
        return Err("composite side-service admission stopped".into());
    }
    Ok(())
}

fn composite_chat_admission_writes(
    steps: &[loadtest_core::side_services::PlannedSideServiceStep],
) -> Result<Vec<u64>, String> {
    if steps
        .iter()
        .any(|step| step.service != SideServiceKind::Chat)
    {
        return Err("composite chat admission received a non-chat operation".into());
    }
    // `execute_live_chat_steps` always sends one authentication request. A
    // declared ChatAuth represents that same request; otherwise reserve it
    // explicitly before the configured operations.
    let mut writes = Vec::with_capacity(steps.len().saturating_add(1));
    if !steps.iter().any(|step| {
        matches!(
            step.operation,
            loadtest_core::side_services::SideServiceOperation::ChatAuth
        )
    }) {
        writes.push(0);
    }
    writes.extend(
        steps
            .iter()
            .map(|step| u64::from(step.operation.is_write())),
    );
    Ok(writes)
}

fn live_composite_http_enabled(
    side: &loadtest_core::side_services::SideServicesScenario,
    service: SideServiceKind,
) -> bool {
    match service {
        SideServiceKind::Mail => side.mail.as_ref().is_some_and(|config| config.live_http),
        SideServiceKind::Announce => side
            .announce
            .as_ref()
            .is_some_and(|config| config.live_http),
        _ => false,
    }
}

/// Execute public side-service diagnostics after both KCP players have closed
/// their shared `default_match` room. Each caller invokes this once per
/// authenticated player, so tickets never cross account boundaries and the
/// synchronous controller retains one global admission ledger.
#[allow(clippy::too_many_arguments)]
fn execute_live_game_side_services(
    config: &LoadTestConfig,
    budget: &loadtest_core::config::HardBudget,
    ticket: &str,
    character_id: &str,
    action_deadline: Instant,
    deadline_unix_ms: u64,
    dispatch_admission: &mut AuthDispatchAdmission,
    abort: &mut AbortController,
    ctrl_c: &AtomicBool,
    protection: &DryRunProtection<'_>,
) -> Result<loadtest_core::metrics::MetricsSnapshot, String> {
    let side = config
        .scenario
        .side_services
        .as_ref()
        .ok_or("live game-side composite requires side_services")?;
    let plan = side
        .executable_plan(budget)
        .map_err(|error| format!("live composite side-service plan rejected: {error}"))?;
    let mut metrics = Metrics::default();

    if let Some(chat) = side.chat.as_ref().filter(|chat| chat.live_websocket) {
        let descriptor = chat
            .descriptor
            .as_ref()
            .ok_or("live composite chat requires an explicit descriptor")?;
        let chat_steps = plan
            .steps
            .iter()
            .filter(|step| step.service == SideServiceKind::Chat)
            .cloned()
            .collect::<Vec<_>>();
        dispatch_admission
            .admit_side_connection(action_deadline, || {
                check_live_composite_side_controller(
                    config,
                    deadline_unix_ms,
                    ctrl_c,
                    protection,
                    abort,
                )
            })
            .map_err(|error| map_auth_admission_to_string(abort, error))?;
        // The concrete chat runner applies every configured think_time before
        // sending that operation. Private/group sends conservatively reserve
        // one potential write before their frame can be dispatched.
        for writes in composite_chat_admission_writes(&chat_steps)? {
            dispatch_admission
                .admit_side_message_with_writes(writes, action_deadline, || {
                    check_live_composite_side_controller(
                        config,
                        deadline_unix_ms,
                        ctrl_c,
                        protection,
                        abort,
                    )
                })
                .map_err(|error| map_auth_admission_to_string(abort, error))?;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|_| "could not create guarded composite chat runtime")?;
        let chat_metrics = runtime
            .block_on(execute_live_chat_steps(
                descriptor,
                config.environment.kind,
                chat.live_websocket,
                ticket.to_owned(),
                character_id,
                &chat_steps,
                action_deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis() as u64,
                ReconnectPolicy {
                    max_attempts: 1,
                    base_delay_ms: 100,
                    max_delay_ms: 500,
                    max_jitter_ms: 50,
                },
            ))
            .map_err(|error| format!("composite chat WebSocket execution failed: {error:?}"))?;
        chat_metrics.merge_into_metrics(&mut metrics);
    }

    let http_steps = plan
        .steps
        .iter()
        .filter(|step| {
            matches!(
                step.service,
                SideServiceKind::Mail | SideServiceKind::Announce
            ) && live_composite_http_enabled(side, step.service)
        })
        .cloned()
        .collect::<Vec<_>>();
    if !http_steps.is_empty() {
        let http_metrics = execute_live_mail_announce_steps(
            side,
            config.environment.kind,
            &config.account_prepare.batch,
            ticket,
            &http_steps,
            action_deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as u64,
            |admission| match admission {
                SideHttpAdmission::Connection => dispatch_admission
                    .admit_side_connection(action_deadline, || {
                        check_live_composite_side_controller(
                            config,
                            deadline_unix_ms,
                            ctrl_c,
                            protection,
                            abort,
                        )
                    })
                    .map(|_| ())
                    .map_err(|error| {
                        loadtest_core::side_http::SideHttpError::Admission(
                            map_auth_admission_to_string(abort, error),
                        )
                    }),
                SideHttpAdmission::Message { writes } => dispatch_admission
                    .admit_side_message_with_writes(u64::from(writes), action_deadline, || {
                        check_live_composite_side_controller(
                            config,
                            deadline_unix_ms,
                            ctrl_c,
                            protection,
                            abort,
                        )
                    })
                    .map(|_| ())
                    .map_err(|error| {
                        loadtest_core::side_http::SideHttpError::Admission(
                            map_auth_admission_to_string(abort, error),
                        )
                    }),
            },
        )
        .map_err(|error| format!("composite mail/announce HTTP execution failed: {error:?}"))?;
        http_metrics.merge_into_metrics(&mut metrics);
    }

    Ok(metrics.snapshot())
}

fn run_live(cli: &Cli) -> Result<(), String> {
    if !cli.execute_auth {
        return Err(
            "real auth-http execution requires --execute-auth; use --dry-run for the offline fake"
                .into(),
        );
    }
    let config = cli.load()?;
    validate(&config, cli)?;
    if cli.confirm_auth.as_deref() != Some(config.environment.name.as_str()) {
        return Err("real auth-http execution requires --confirm-auth <environment>".into());
    }
    let game_mode = cli.execute_game;
    let live_chat = config
        .scenario
        .side_services
        .as_ref()
        .and_then(|side| side.chat.as_ref())
        .is_some_and(|chat| chat.live_websocket);
    let live_match = config
        .scenario
        .side_services
        .as_ref()
        .and_then(|side| side.r#match.as_ref())
        .is_some_and(|matcher| matcher.live_grpc);
    let live_match_internal = config
        .scenario
        .side_services
        .as_ref()
        .and_then(|side| side.r#match.as_ref())
        .is_some_and(|matcher| matcher.live_internal);
    let live_http = config.scenario.side_services.as_ref().is_some_and(|side| {
        side.mail.as_ref().is_some_and(|mail| mail.live_http)
            || side
                .announce
                .as_ref()
                .is_some_and(|announce| announce.live_http)
    });
    if game_mode {
        validate_game_execution_gate(cli, &config)?;
    }
    let auth = config
        .scenario
        .auth
        .as_ref()
        .ok_or("--execute-auth requires scenario.auth operations")?;
    if game_mode {
        validate_live_game_load_model(
            &config.scenario.load,
            config
                .scenario
                .live_gameplay
                .as_ref()
                .map(|gameplay| gameplay.coordination)
                .unwrap_or_default(),
        )?;
    } else {
        validate_live_auth_load_model(&config.scenario.load)?;
    }
    let budget = config
        .effective_budget(&cli.budget_override)
        .map_err(|error| error.to_string())?;
    let auth_budget_estimate = estimate_auth_run(&config.scenario, &budget)?;
    validate_staged_auth_windows(&config.scenario, &budget)?;
    validate_auth_run_budget(&auth_budget_estimate, &budget)?;
    if game_mode {
        validate_game_run_budget_for_scenario(&auth_budget_estimate, &config.scenario, &budget)?;
    }
    let manifest_path = cli
        .account_manifest
        .as_deref()
        .ok_or("--execute-auth requires --account-manifest <credential-free manifest>")?;
    let private_path = cli
        .private_config
        .as_deref()
        .ok_or("--execute-auth requires --private-config with secret references")?;
    let private = load_private_config(private_path).map_err(|error| error.to_string())?;
    let manifest = read_manifest(manifest_path)?;
    if manifest.environment != config.environment.name
        || manifest.batch != config.account_prepare.batch
    {
        return Err(
            "account manifest environment or batch does not match the selected config".into(),
        );
    }
    let (game_auth_operations, deferred_logout) = if game_mode {
        split_game_auth_operations(&auth.operations)?
    } else {
        (
            if live_chat || live_match || live_match_internal || live_http {
                loadtest_core::auth_http::split_game_auth_operations(&auth.operations)?.0
            } else {
                auth.operations.clone()
            },
            if live_chat || live_match || live_match_internal || live_http {
                auth.operations.last().is_some_and(|operation| {
                    matches!(operation, loadtest_core::config::AuthOperation::Logout)
                })
            } else {
                false
            },
        )
    };
    let two_player_default_match = game_mode
        && config
            .scenario
            .live_gameplay
            .as_ref()
            .is_some_and(|gameplay| {
                gameplay.coordination == LiveGameplayCoordination::TwoPlayerDefaultMatch
            });
    let live_game_side_service_composite = validate_live_game_side_service_composite(
        config.environment.kind,
        game_mode,
        two_player_default_match,
        live_chat,
        live_match,
        live_match_internal,
        live_http,
    )?;
    if game_mode
        && !game_auth_operations.iter().any(|operation| {
            matches!(
                operation,
                loadtest_core::config::AuthOperation::IssueTicket
                    | loadtest_core::config::AuthOperation::SelectCharacter
            )
        })
    {
        return Err("--execute-game requires a ticket-producing scenario.auth operation".into());
    }

    let started = unix_ms();
    let profile_deadline_unix_ms =
        effective_deadline(&config, &budget, cli.deadline_unix_ms, started)?;
    let deadline_unix_ms = profile_deadline_unix_ms.min(
        started.saturating_add(
            auth_budget_estimate
                .scenario_duration_secs
                .saturating_mul(1_000),
        ),
    );
    let preflight = summarize_run(
        "run",
        &config,
        &budget,
        RunAccess {
            allow_remote: cli.allow_remote,
            confirmation: cli.confirmation.as_deref(),
        },
        deadline_unix_ms,
        false,
        true,
    )?;
    println!(
        "preflight={}",
        serde_json::to_string(&preflight).expect("preflight summary serializes")
    );

    let account_ids = manifest
        .ready_accounts()
        .map(|entry| entry.logical_account_id.clone())
        .collect::<Vec<_>>();
    let requested_players = auth_budget_estimate.virtual_player_slots;
    let mut account_pool = AccountLeasePool::default();
    let leases = account_pool.assign_players(
        &account_ids,
        requested_players,
        "auth-run",
        0,
        budget
            .max_duration_secs
            .saturating_mul(1_000)
            .saturating_add(60_000),
        auth.allow_same_account_concurrency,
        auth.same_account_session_effect,
    )?;

    // This constructor makes no request. It is deliberately below every CLI,
    // profile, manifest, and secret-reference gate above.
    let monotonic_deadline =
        Instant::now() + Duration::from_millis(deadline_unix_ms.saturating_sub(unix_ms()));
    let mut transport =
        ReqwestAuthHttpTransport::new(&config.targets.auth_http, Duration::from_millis(1))?;
    let endpoint = if game_mode {
        Some(
            GameProxyEndpoint::parse(&config.targets.game_proxy)
                .map_err(|error| error.to_string())?,
        )
    } else {
        None
    };
    let game_runner = GameSessionRunner {
        max_body_len: 1024 * 1024,
        reconnect_policy: ReconnectPolicy {
            max_attempts: 1,
            base_delay_ms: 100,
            max_delay_ms: 100,
            max_jitter_ms: 0,
        },
    };
    let secret_provider = EnvironmentSecretProvider::new(&private);
    let protection = DryRunProtection::new(&config);
    let ctrl_c = install_ctrl_c_flag()
        .map_err(|error| format!("failed to install Ctrl+C handler: {error}"))?;
    let mut lifecycle = Lifecycle::default();
    lifecycle.transition(RunState::Validated).unwrap();
    lifecycle.transition(RunState::WarmingUp).unwrap();
    lifecycle.transition(RunState::Ramping).unwrap();
    lifecycle.transition(RunState::Steady).unwrap();
    let monotonic_started = Instant::now();
    let mut scheduler = MonotonicScheduler::new(
        &config.scenario.load,
        100,
        budget.max_virtual_players as usize,
    );
    let mut core_metrics = Metrics::default();
    core_metrics.increment("virtual_players", requested_players as u64);
    let mut auth_metrics = AuthRunMetrics::default();
    let mut dispatch_admission = AuthDispatchAdmission::new(&budget)?;
    let mut errors = ErrorBuffer::default();
    let mut abort = AbortController::default();
    let mut failed = false;

    while !scheduler.exhausted() && !abort.should_stop_new_sessions() {
        abort.check_ctrl_c(&ctrl_c);
        abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
        abort.check_deadline(unix_ms(), deadline_unix_ms);
        if abort.should_stop_new_sessions() {
            break;
        }
        if let Some(_) = revalidate_or_abort(&protection, &mut abort) {
            break;
        }
        let elapsed_ms = monotonic_started.elapsed().as_millis() as u64;
        let tick = scheduler.due(elapsed_ms);
        core_metrics.increment(
            "scheduler_lag_ms",
            tick.actions
                .iter()
                .map(|action| action.scheduler_lag_ms)
                .sum(),
        );
        core_metrics.increment("scheduler_queue_depth", tick.queue_depth);
        core_metrics.increment("metrics_dropped", tick.dropped);

        for (index, action) in tick.actions.iter().enumerate() {
            if abort.should_stop_new_sessions() {
                break;
            }
            if revalidate_or_abort(&protection, &mut abort).is_some() {
                break;
            }
            if two_player_default_match {
                // The structural gate permits exactly one staged wave with two
                // players. Consume it as a single owned room lifecycle rather
                // than silently turning it into two independent game flows.
                if index != 0 {
                    continue;
                }
                let Some(second_action) = tick.actions.get(1) else {
                    errors.push(
                        "two_player_match_wave_invalid",
                        "two-player default_match wave did not contain both players",
                        Default::default(),
                    );
                    failed = true;
                    break;
                };
                let action_deadline = [action, second_action]
                    .iter()
                    .filter_map(|action| {
                        action.window_end_ms.map(|window_end_ms| {
                            monotonic_started + Duration::from_millis(window_end_ms)
                        })
                    })
                    .fold(monotonic_deadline, |deadline, stage_deadline| {
                        deadline.min(stage_deadline)
                    });
                let mut executions = Vec::with_capacity(2);
                let mut pre_game_auth_completed = Vec::with_capacity(2);
                for lease in leases.iter().take(2) {
                    let password = match secret_provider.password_for(&lease.logical_account_id) {
                        Ok(password) => password,
                        Err(_) => {
                            errors.push(
                                "auth_secret_unavailable",
                                "secret provider could not resolve a required credential",
                                Default::default(),
                            );
                            failed = true;
                            break;
                        }
                    };
                    let execution = execute_auth_operations(
                        &mut transport,
                        &game_auth_operations,
                        &format!("{}_auth", config.account_prepare.character_name_prefix),
                        &lease.logical_account_id,
                        &password,
                        |_, request| {
                            dispatch_admission
                                .admit(request, action_deadline, || {
                                    abort.check_ctrl_c(&ctrl_c);
                                    abort.check_stop_file(
                                        config.stop_file.as_deref().map(Path::new),
                                    );
                                    abort.check_deadline(unix_ms(), deadline_unix_ms);
                                    if abort.should_stop_new_sessions()
                                        || revalidate_or_abort(&protection, &mut abort).is_some()
                                    {
                                        return Err(
                                            "auth admission stopped before request dispatch".into(),
                                        );
                                    }
                                    Ok(())
                                })
                                .map_err(|error| map_auth_admission_to_string(&mut abort, error))
                        },
                    );
                    let completed = execution.error.is_none();
                    if !completed {
                        errors.push(
                            "auth_operation_failed",
                            "an auth operation failed; report categories contain no identity or secret",
                            Default::default(),
                        );
                        failed = true;
                    }
                    pre_game_auth_completed.push(completed);
                    executions.push(execution);
                    if !completed {
                        break;
                    }
                }

                let mut completed_game_sessions = 0_u64;
                if executions.len() == 2
                    && pre_game_auth_completed.iter().all(|completed| *completed)
                {
                    let mut tickets = Vec::with_capacity(2);
                    let mut side_credentials = Vec::with_capacity(2);
                    for execution in &mut executions {
                        match execution.take_game_credentials() {
                            Some((ticket, character_id)) => {
                                // The same ticket remains local to its own
                                // player and is only reused after that
                                // player's KCP session has reached Closed.
                                side_credentials.push((ticket.clone(), character_id));
                                tickets.push(ticket);
                            }
                            None => {
                                errors.push(
                                    "game_ticket_missing",
                                    "auth completed without a transferable game ticket",
                                    Default::default(),
                                );
                                failed = true;
                            }
                        }
                    }
                    for _ in 0..2 {
                        if failed {
                            break;
                        }
                        if let Err(error) =
                            dispatch_admission.admit_game_connection(action_deadline, || {
                                abort.check_ctrl_c(&ctrl_c);
                                abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
                                abort.check_deadline(unix_ms(), deadline_unix_ms);
                                if abort.should_stop_new_sessions()
                                    || revalidate_or_abort(&protection, &mut abort).is_some()
                                {
                                    return Err("game connection admission stopped".into());
                                }
                                Ok(())
                            })
                        {
                            let _ = map_auth_admission_to_game_error(&mut abort, error);
                            errors.push(
                                "game_connection_admission_failed",
                                "game connection was rejected before KCP setup",
                                Default::default(),
                            );
                            failed = true;
                        }
                    }
                    if !failed && tickets.len() == 2 {
                        let remaining = action_deadline.saturating_duration_since(Instant::now());
                        let game_result = match (
                            LiveKcpTransport::new(
                                Instant::now() + remaining,
                                game_runner.max_body_len,
                            ),
                            LiveKcpTransport::new(
                                Instant::now() + remaining,
                                game_runner.max_body_len,
                            ),
                            tokio::runtime::Builder::new_current_thread()
                                .enable_io()
                                .enable_time()
                                .build(),
                        ) {
                            (Ok(mut first), Ok(mut second), Ok(runtime)) => {
                                runtime.block_on(game_runner.run_live_two_player_default_match_kcp(
                                    GameExecutionGate {
                                        execute_game: cli.execute_game,
                                        confirm_game: cli.confirm_game.as_deref(),
                                        environment: &config.environment.name,
                                        account_manifest_supplied: cli.account_manifest.is_some(),
                                        private_config_supplied: cli.private_config.is_some(),
                                    },
                                    [&mut first, &mut second],
                                    &config,
                                    RunAccess {
                                        allow_remote: cli.allow_remote,
                                        confirmation: cli.confirmation.as_deref(),
                                    },
                                    endpoint.as_ref().expect("game mode creates endpoint"),
                                    &mut account_pool,
                                    [leases[0].clone(), leases[1].clone()],
                                    [&tickets[0], &tickets[1]],
                                    |checkpoint| {
                                        let mut control = || -> Result<(), String> {
                                            abort.check_ctrl_c(&ctrl_c);
                                            abort.check_stop_file(
                                                config.stop_file.as_deref().map(Path::new),
                                            );
                                            abort.check_deadline(unix_ms(), deadline_unix_ms);
                                            if abort.should_stop_new_sessions()
                                                || revalidate_or_abort(&protection, &mut abort)
                                                    .is_some()
                                            {
                                                return Err(
                                                    "game execution checkpoint stopped".into()
                                                );
                                            }
                                            Ok(())
                                        };
                                        control().map_err(|_| {
                                            GameLiveError::Transport(
                                                "game execution checkpoint stopped",
                                            )
                                        })?;
                                        match checkpoint {
                                            GameRunnerCheckpoint::OutboundMessage => {
                                                dispatch_admission
                                                    .admit_game_message(action_deadline, control)
                                                    .map_err(|error| {
                                                        map_auth_admission_to_game_error(
                                                            &mut abort, error,
                                                        )
                                                    })?;
                                            }
                                            GameRunnerCheckpoint::GameplayOutboundMessage => {
                                                dispatch_admission
                                                    .admit_gameplay_message(
                                                        LIVE_GAMEPLAY_POTENTIAL_WRITES_PER_MESSAGE,
                                                        action_deadline,
                                                        control,
                                                    )
                                                    .map_err(|error| {
                                                        map_auth_admission_to_game_error(
                                                            &mut abort, error,
                                                        )
                                                    })?;
                                            }
                                            GameRunnerCheckpoint::ReconnectConnection => {
                                                dispatch_admission
                                                    .admit_game_connection(action_deadline, control)
                                                    .map_err(|error| {
                                                        map_auth_admission_to_game_error(
                                                            &mut abort, error,
                                                        )
                                                    })?;
                                            }
                                            GameRunnerCheckpoint::Control => {}
                                        }
                                        Ok(())
                                    },
                                ))
                            }
                            _ => Err(GameLiveError::Transport(
                                "could not create guarded two-player KCP transport or runtime",
                            )),
                        };
                        match game_result {
                            Ok(result) if result.players.iter().all(|player| {
                                player.terminal_state
                                    == loadtest_core::virtual_player::VirtualPlayerSessionState::Closed
                            }) => {
                                for player in result.players {
                                    if let Some(metrics) = player.gameplay_metrics.as_ref() {
                                        core_metrics.merge_snapshot(metrics);
                                    }
                                    completed_game_sessions += 1;
                                }
                            }
                            Ok(_) => {
                                errors.push(
                                    "game_session_failed",
                                    "two-player KCP game session did not complete",
                                    Default::default(),
                                );
                                failed = true;
                            }
                            Err(error) => {
                                if let Some(metrics) = error.gameplay_metrics() {
                                    core_metrics.merge_snapshot(metrics);
                                }
                                errors.push(
                                    game_failure_category(&error),
                                    "two-player KCP game session did not complete",
                                    Default::default(),
                                );
                                failed = true;
                            }
                        }
                    }
                    if !failed && live_game_side_service_composite {
                        // The KCP pair is complete before any public
                        // side-service request. Process each authenticated
                        // player in account order so chat/mail/announce never
                        // create a second concurrent load stream.
                        for (ticket, character_id) in side_credentials {
                            match execute_live_game_side_services(
                                &config,
                                &budget,
                                &ticket,
                                &character_id,
                                action_deadline,
                                deadline_unix_ms,
                                &mut dispatch_admission,
                                &mut abort,
                                &ctrl_c,
                                &protection,
                            ) {
                                Ok(metrics) => core_metrics.merge_snapshot(&metrics),
                                Err(_) => {
                                    errors.push(
                                        "game_side_service_execution_failed",
                                        "post-game chat/mail/announce execution did not complete",
                                        Default::default(),
                                    );
                                    failed = true;
                                    break;
                                }
                            }
                        }
                    }
                }
                for (execution, pre_game_auth_completed) in
                    executions.iter_mut().zip(pre_game_auth_completed)
                {
                    if deferred_logout && pre_game_auth_completed {
                        if !can_attempt_deferred_logout(
                            deferred_logout,
                            pre_game_auth_completed,
                            &abort,
                        ) {
                            errors.push(
                                "deferred_logout_not_dispatched",
                                deferred_logout_skip_message(&abort),
                                Default::default(),
                            );
                            failed = true;
                        } else if execute_deferred_logout(&mut transport, execution, |request| {
                            dispatch_admission
                                .admit(request, action_deadline, || {
                                    abort.check_ctrl_c(&ctrl_c);
                                    abort.check_stop_file(
                                        config.stop_file.as_deref().map(Path::new),
                                    );
                                    abort.check_deadline(unix_ms(), deadline_unix_ms);
                                    if abort.should_stop_new_sessions()
                                        || revalidate_or_abort(&protection, &mut abort).is_some()
                                    {
                                        return Err("deferred logout stopped".into());
                                    }
                                    Ok(())
                                })
                                .map_err(|error| map_auth_admission_to_string(&mut abort, error))
                        })
                        .is_err()
                        {
                            errors.push(
                                "deferred_logout_failed",
                                "post-game logout failed",
                                Default::default(),
                            );
                            failed = true;
                        }
                    }
                    finish_live_action(&mut auth_metrics, &execution.metrics, &mut abort, &budget);
                }
                for _ in 0..completed_game_sessions {
                    record_completed_game_session_metrics(&mut core_metrics);
                }
                if failed {
                    break;
                }
                continue;
            }
            let lease = &leases[index % leases.len()];
            let action_deadline = action
                .window_end_ms
                .map(|window_end_ms| monotonic_started + Duration::from_millis(window_end_ms))
                .map_or(monotonic_deadline, |stage_deadline| {
                    stage_deadline.min(monotonic_deadline)
                });
            let password = match secret_provider.password_for(&lease.logical_account_id) {
                Ok(password) => password,
                Err(_) => {
                    errors.push(
                        "auth_secret_unavailable",
                        "secret provider could not resolve a required credential",
                        Default::default(),
                    );
                    failed = true;
                    break;
                }
            };
            let mut execution = execute_auth_operations(
                &mut transport,
                &game_auth_operations,
                &format!("{}_auth", config.account_prepare.character_name_prefix),
                &lease.logical_account_id,
                &password,
                |_, request| {
                    dispatch_admission
                        .admit(request, action_deadline, || {
                            abort.check_ctrl_c(&ctrl_c);
                            abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
                            abort.check_deadline(unix_ms(), deadline_unix_ms);
                            if abort.should_stop_new_sessions()
                                || revalidate_or_abort(&protection, &mut abort).is_some()
                            {
                                return Err("auth admission stopped before request dispatch".into());
                            }
                            Ok(())
                        })
                        .map_err(|error| map_auth_admission_to_string(&mut abort, error))
                },
            );
            let mut execution_failed = execution.error.is_some();
            let pre_game_auth_completed = !execution_failed;
            let mut side_credentials = if !execution_failed
                && !game_mode
                && (live_chat || live_match || live_match_internal || live_http)
            {
                execution.take_game_credentials()
            } else {
                None
            };
            if execution_failed {
                errors.push(
                    "auth_operation_failed",
                    "an auth operation failed; report categories contain no identity or secret",
                    Default::default(),
                );
                failed = true;
            }
            if !execution_failed && game_mode {
                let mut completed_game_session = false;
                let mut game_ticket = None;
                match execution.take_game_credentials() {
                    None => {
                        errors.push(
                            "game_ticket_missing",
                            "auth completed without a transferable game ticket",
                            Default::default(),
                        );
                        failed = true;
                        execution_failed = true;
                    }
                    Some((ticket, _character_id)) => {
                        game_ticket = Some(ticket);
                        let game_admission =
                            dispatch_admission.admit_game_connection(action_deadline, || {
                                abort.check_ctrl_c(&ctrl_c);
                                abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
                                abort.check_deadline(unix_ms(), deadline_unix_ms);
                                if abort.should_stop_new_sessions()
                                    || revalidate_or_abort(&protection, &mut abort).is_some()
                                {
                                    return Err("game connection admission stopped".into());
                                }
                                Ok(())
                            });
                        if let Err(error) = game_admission {
                            let _ = map_auth_admission_to_game_error(&mut abort, error);
                            errors.push(
                                "game_connection_admission_failed",
                                "game connection was rejected before KCP setup",
                                Default::default(),
                            );
                            failed = true;
                            execution_failed = true;
                        }
                    }
                }
                if !execution_failed {
                    let remaining = action_deadline.saturating_duration_since(Instant::now());
                    let mut game_transport = match LiveKcpTransport::new(
                        Instant::now() + remaining,
                        game_runner.max_body_len,
                    ) {
                        Ok(transport) => Some(transport),
                        Err(_) => {
                            errors.push(
                                "game_transport_setup_failed",
                                "game transport could not be set up",
                                Default::default(),
                            );
                            failed = true;
                            execution_failed = true;
                            None
                        }
                    };
                    if let Some(game_transport) = game_transport.as_mut() {
                        // Auth and game are one player flow. The virtual game
                        // session takes this exact lease and releases it on
                        // every terminal path; no second lease is acquired.
                        let game_lease = lease.clone();
                        let game_result =
                            match tokio::runtime::Builder::new_current_thread()
                                .enable_io()
                                .enable_time()
                                .build()
                            {
                                Ok(runtime) => runtime.block_on(game_runner.run_live_kcp(
                                    GameExecutionGate {
                                        execute_game: cli.execute_game,
                                        confirm_game: cli.confirm_game.as_deref(),
                                        environment: &config.environment.name,
                                        account_manifest_supplied: cli.account_manifest.is_some(),
                                        private_config_supplied: cli.private_config.is_some(),
                                    },
                                    game_transport,
                                    &config,
                                    RunAccess {
                                        allow_remote: cli.allow_remote,
                                        confirmation: cli.confirmation.as_deref(),
                                    },
                                    endpoint.as_ref().expect("game mode creates endpoint"),
                                    &mut account_pool,
                                    game_lease,
                                    game_ticket.as_deref().expect(
                                        "successful game admission retains the local ticket",
                                    ),
                                    |checkpoint| {
                                        let mut control = || -> Result<(), String> {
                                            abort.check_ctrl_c(&ctrl_c);
                                            abort.check_stop_file(
                                                config.stop_file.as_deref().map(Path::new),
                                            );
                                            abort.check_deadline(unix_ms(), deadline_unix_ms);
                                            if abort.should_stop_new_sessions()
                                                || revalidate_or_abort(&protection, &mut abort)
                                                    .is_some()
                                            {
                                                return Err(
                                                    "game execution checkpoint stopped".into()
                                                );
                                            }
                                            Ok(())
                                        };
                                        control().map_err(|_| {
                                            GameLiveError::Transport(
                                                "game execution checkpoint stopped",
                                            )
                                        })?;
                                        match checkpoint {
                                            GameRunnerCheckpoint::OutboundMessage => {
                                                dispatch_admission
                                                    .admit_game_message(action_deadline, control)
                                                    .map_err(|error| {
                                                        map_auth_admission_to_game_error(
                                                            &mut abort, error,
                                                        )
                                                    })?;
                                            }
                                            GameRunnerCheckpoint::GameplayOutboundMessage => {
                                                dispatch_admission
                                                    .admit_gameplay_message(
                                                        LIVE_GAMEPLAY_POTENTIAL_WRITES_PER_MESSAGE,
                                                        action_deadline,
                                                        control,
                                                    )
                                                    .map_err(|error| {
                                                        map_auth_admission_to_game_error(
                                                            &mut abort, error,
                                                        )
                                                    })?;
                                            }
                                            GameRunnerCheckpoint::ReconnectConnection => {
                                                dispatch_admission
                                                    .admit_game_connection(action_deadline, control)
                                                    .map_err(|error| {
                                                        map_auth_admission_to_game_error(
                                                            &mut abort, error,
                                                        )
                                                    })?;
                                            }
                                            GameRunnerCheckpoint::Control => {}
                                        }
                                        Ok(())
                                    },
                                )),
                                Err(_) => Err(GameLiveError::Transport(
                                    "could not create guarded KCP runtime",
                                )),
                            };
                        match game_result {
                            Ok(result)
                                if result.terminal_state
                                    == loadtest_core::virtual_player::VirtualPlayerSessionState::Closed =>
                            {
                                if let Some(gameplay_metrics) = result.gameplay_metrics.as_ref() {
                                    core_metrics.merge_snapshot(gameplay_metrics);
                                }
                                completed_game_session = true;
                            }
                            Ok(_) => {
                                errors.push(
                                    "game_runner_transport_or_contract_failed",
                                    "KCP game session did not complete",
                                    Default::default(),
                                );
                                failed = true;
                                execution_failed = true;
                            }
                            Err(error) => {
                                if let Some(gameplay_metrics) = error.gameplay_metrics() {
                                    core_metrics.merge_snapshot(gameplay_metrics);
                                }
                                errors.push(
                                    game_failure_category(&error),
                                    "KCP game session did not complete",
                                    Default::default(),
                                );
                                failed = true;
                                execution_failed = true;
                            }
                        }
                    }
                }
                finish_game_action_after_cleanup(
                    completed_game_session,
                    || {
                        if deferred_logout && pre_game_auth_completed {
                            if !can_attempt_deferred_logout(
                                deferred_logout,
                                pre_game_auth_completed,
                                &abort,
                            ) {
                                errors.push(
                                    "deferred_logout_not_dispatched",
                                    deferred_logout_skip_message(&abort),
                                    Default::default(),
                                );
                            } else if execute_deferred_logout(
                                &mut transport,
                                &mut execution,
                                |request| {
                                    dispatch_admission
                                        .admit(request, action_deadline, || {
                                            abort.check_ctrl_c(&ctrl_c);
                                            abort.check_stop_file(
                                                config.stop_file.as_deref().map(Path::new),
                                            );
                                            abort.check_deadline(unix_ms(), deadline_unix_ms);
                                            if abort.should_stop_new_sessions()
                                                || revalidate_or_abort(&protection, &mut abort)
                                                    .is_some()
                                            {
                                                return Err("deferred logout stopped".into());
                                            }
                                            Ok(())
                                        })
                                        .map_err(|error| {
                                            map_auth_admission_to_string(&mut abort, error)
                                        })
                                },
                            )
                            .is_err()
                            {
                                let category = if abort.should_stop_new_sessions() {
                                    "deferred_logout_not_dispatched"
                                } else {
                                    "deferred_logout_failed"
                                };
                                let message = if abort.should_stop_new_sessions() {
                                    deferred_logout_skip_message(&abort)
                                } else {
                                    "post-game logout failed"
                                };
                                errors.push(category, message, Default::default());
                                failed = true;
                                execution_failed = true;
                            }
                        }
                    },
                    || record_completed_game_session_metrics(&mut core_metrics),
                );
            }
            if !execution_failed && live_chat {
                let chat = config
                    .scenario
                    .side_services
                    .as_ref()
                    .and_then(|side| side.chat.as_ref())
                    .ok_or("live chat configuration disappeared after validation")?;
                let descriptor = chat
                    .descriptor
                    .as_ref()
                    .ok_or("live chat requires an explicit descriptor")?;
                let plan = config
                    .scenario
                    .side_services
                    .as_ref()
                    .expect("live chat configuration exists")
                    .executable_plan(&budget)
                    .map_err(|error| format!("live chat plan rejected: {error}"))?;
                let Some((ticket, character_id)) = side_credentials.clone() else {
                    errors.push(
                        "chat_ticket_missing",
                        "auth completed without a transferable chat ticket",
                        Default::default(),
                    );
                    failed = true;
                    execution_failed = true;
                    finish_live_action(&mut auth_metrics, &execution.metrics, &mut abort, &budget);
                    continue;
                };
                let chat_steps = plan
                    .steps
                    .iter()
                    .filter(|step| step.service == SideServiceKind::Chat)
                    .cloned()
                    .collect::<Vec<_>>();
                let chat_deadline = action_deadline.saturating_duration_since(Instant::now());
                let admitted = dispatch_admission.admit_side_connection(action_deadline, || {
                    abort.check_ctrl_c(&ctrl_c);
                    abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
                    abort.check_deadline(unix_ms(), deadline_unix_ms);
                    if abort.should_stop_new_sessions()
                        || revalidate_or_abort(&protection, &mut abort).is_some()
                    {
                        return Err("chat connection admission stopped".into());
                    }
                    Ok(())
                });
                let mut chat_admission_failed = false;
                if admitted.is_ok() {
                    for _ in 0..chat_steps.len().saturating_add(1) {
                        if let Err(error) =
                            dispatch_admission.admit_side_message(action_deadline, || {
                                abort.check_ctrl_c(&ctrl_c);
                                abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
                                abort.check_deadline(unix_ms(), deadline_unix_ms);
                                if abort.should_stop_new_sessions()
                                    || revalidate_or_abort(&protection, &mut abort).is_some()
                                {
                                    return Err("chat message admission stopped".into());
                                }
                                Ok(())
                            })
                        {
                            let _ = map_auth_admission_to_game_error(&mut abort, error);
                            failed = true;
                            execution_failed = true;
                            chat_admission_failed = true;
                            break;
                        }
                    }
                }
                if let Err(error) = admitted {
                    let _ = map_auth_admission_to_game_error(&mut abort, error);
                    errors.push(
                        "chat_connection_admission_failed",
                        "chat WebSocket connection was rejected before setup",
                        Default::default(),
                    );
                    failed = true;
                    execution_failed = true;
                } else if !chat_admission_failed {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_io()
                        .enable_time()
                        .build()
                        .map_err(|_| "could not create guarded chat WebSocket runtime")?;
                    match runtime.block_on(execute_live_chat_steps(
                        descriptor,
                        config.environment.kind,
                        chat.live_websocket,
                        ticket,
                        &character_id,
                        &chat_steps,
                        chat_deadline.as_millis() as u64,
                        ReconnectPolicy {
                            max_attempts: 1,
                            base_delay_ms: 100,
                            max_delay_ms: 500,
                            max_jitter_ms: 50,
                        },
                    )) {
                        Ok(chat_metrics) => chat_metrics.merge_into_metrics(&mut core_metrics),
                        Err(error) => {
                            errors.push(
                                "chat_wss_execution_failed",
                                format!("chat WebSocket execution failed: {error:?}"),
                                Default::default(),
                            );
                            failed = true;
                            execution_failed = true;
                        }
                    }
                }
            }
            if !execution_failed && live_match {
                let matcher = config
                    .scenario
                    .side_services
                    .as_ref()
                    .and_then(|side| side.r#match.as_ref())
                    .ok_or("live match configuration disappeared after validation")?;
                let descriptor = matcher
                    .descriptor
                    .as_ref()
                    .ok_or("live match requires an explicit descriptor")?;
                let Some((_, character_id)) = side_credentials.as_ref() else {
                    errors.push(
                        "match_character_id_missing",
                        "auth completed without a transferable character identity",
                        Default::default(),
                    );
                    failed = true;
                    execution_failed = true;
                    finish_live_action(&mut auth_metrics, &execution.metrics, &mut abort, &budget);
                    continue;
                };
                let plan = config
                    .scenario
                    .side_services
                    .as_ref()
                    .expect("live match configuration exists")
                    .executable_plan(&budget)
                    .map_err(|error| format!("live match plan rejected: {error}"))?;
                let match_steps = plan
                    .steps
                    .iter()
                    .filter(|step| step.service == SideServiceKind::Match)
                    .cloned()
                    .collect::<Vec<_>>();
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                    .map_err(|_| "could not create guarded match gRPC runtime")?;
                match runtime.block_on(execute_live_match_steps(
                    descriptor,
                    config.environment.kind,
                    matcher.live_grpc,
                    character_id,
                    &match_steps,
                    action_deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64,
                    |is_connection| {
                        let admission = if is_connection {
                            dispatch_admission.admit_side_connection(action_deadline, || {
                                abort.check_ctrl_c(&ctrl_c);
                                abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
                                abort.check_deadline(unix_ms(), deadline_unix_ms);
                                if abort.should_stop_new_sessions()
                                    || revalidate_or_abort(&protection, &mut abort).is_some()
                                {
                                    return Err("match gRPC connection admission stopped".into());
                                }
                                Ok(())
                            })
                        } else {
                            dispatch_admission.admit_side_message(action_deadline, || {
                                abort.check_ctrl_c(&ctrl_c);
                                abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
                                abort.check_deadline(unix_ms(), deadline_unix_ms);
                                if abort.should_stop_new_sessions()
                                    || revalidate_or_abort(&protection, &mut abort).is_some()
                                {
                                    return Err("match gRPC message admission stopped".into());
                                }
                                Ok(())
                            })
                        };
                        admission.map(|_| ()).map_err(|error| {
                            loadtest_core::match_grpc::MatchGrpcError::Grpc(error.to_string())
                        })
                    },
                )) {
                    Ok(match_metrics) => match_metrics.merge_into_metrics(&mut core_metrics),
                    Err(error) => {
                        errors.push(
                            "match_grpc_execution_failed",
                            format!("match gRPC execution failed: {error:?}"),
                            Default::default(),
                        );
                        failed = true;
                        execution_failed = true;
                    }
                }
            }
            if !execution_failed && live_match_internal {
                let matcher = config
                    .scenario
                    .side_services
                    .as_ref()
                    .and_then(|side| side.r#match.as_ref())
                    .ok_or("live MatchInternal configuration disappeared after validation")?;
                let descriptor = matcher
                    .descriptor
                    .as_ref()
                    .ok_or("live MatchInternal requires an explicit descriptor")?;
                let Some((_, character_id)) = side_credentials.as_ref() else {
                    errors.push(
                        "match_internal_character_id_missing",
                        "auth completed without a transferable character identity",
                        Default::default(),
                    );
                    failed = true;
                    execution_failed = true;
                    finish_live_action(&mut auth_metrics, &execution.metrics, &mut abort, &budget);
                    continue;
                };
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_io()
                    .enable_time()
                    .build()
                    .map_err(|_| "could not create guarded MatchInternal runtime")?;
                let roles = vec![character_id.clone()];
                let diagnostic_id =
                    NEXT_MATCH_INTERNAL_DIAGNOSTIC_ID.fetch_add(1, Ordering::Relaxed);
                let match_id = format!("loadtest-internal-match-{diagnostic_id}");
                let room_id = format!("loadtest-internal-room-{diagnostic_id}");
                match runtime.block_on(execute_live_match_internal_steps(
                    descriptor,
                    config.environment.kind,
                    matcher.live_internal,
                    &roles,
                    &match_id,
                    &room_id,
                    action_deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64,
                    |admission| match admission {
                        MatchInternalAdmission::Connection => dispatch_admission
                            .admit_side_connection(action_deadline, || {
                                abort.check_ctrl_c(&ctrl_c);
                                abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
                                abort.check_deadline(unix_ms(), deadline_unix_ms);
                                if abort.should_stop_new_sessions()
                                    || revalidate_or_abort(&protection, &mut abort).is_some()
                                {
                                    return Err("MatchInternal connection admission stopped".into());
                                }
                                Ok(())
                            })
                            .map(|_| ())
                            .map_err(|error| {
                                loadtest_core::match_grpc::MatchGrpcError::Grpc(error.to_string())
                            }),
                        MatchInternalAdmission::Message { writes } => dispatch_admission
                            .admit_side_message_with_writes(
                                u64::from(writes),
                                action_deadline,
                                || {
                                    abort.check_ctrl_c(&ctrl_c);
                                    abort.check_stop_file(
                                        config.stop_file.as_deref().map(Path::new),
                                    );
                                    abort.check_deadline(unix_ms(), deadline_unix_ms);
                                    if abort.should_stop_new_sessions()
                                        || revalidate_or_abort(&protection, &mut abort).is_some()
                                    {
                                        return Err(
                                            "MatchInternal message admission stopped".into()
                                        );
                                    }
                                    Ok(())
                                },
                            )
                            .map(|_| ())
                            .map_err(|error| {
                                loadtest_core::match_grpc::MatchGrpcError::Grpc(error.to_string())
                            }),
                    },
                )) {
                    Ok(internal_metrics) => internal_metrics.merge_into_metrics(&mut core_metrics),
                    Err(error) => {
                        errors.push(
                            "match_internal_execution_failed",
                            format!("MatchInternal diagnostic failed: {error:?}"),
                            Default::default(),
                        );
                        failed = true;
                        execution_failed = true;
                    }
                }
            }
            if !execution_failed && live_http {
                let side =
                    config.scenario.side_services.as_ref().ok_or(
                        "live HTTP side-service configuration disappeared after validation",
                    )?;
                let plan = side
                    .executable_plan(&budget)
                    .map_err(|error| format!("live HTTP side-service plan rejected: {error}"))?;
                let http_steps = plan
                    .steps
                    .iter()
                    .filter(|step| {
                        matches!(
                            step.service,
                            SideServiceKind::Mail | SideServiceKind::Announce
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                let Some((ticket, _)) = side_credentials.as_ref() else {
                    errors.push(
                        "side_http_ticket_missing",
                        "auth completed without a transferable side-service ticket",
                        Default::default(),
                    );
                    failed = true;
                    execution_failed = true;
                    finish_live_action(&mut auth_metrics, &execution.metrics, &mut abort, &budget);
                    continue;
                };
                match execute_live_mail_announce_steps(
                    side,
                    config.environment.kind,
                    &config.account_prepare.batch,
                    ticket,
                    &http_steps,
                    action_deadline
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64,
                    |admission| match admission {
                        SideHttpAdmission::Connection => dispatch_admission
                            .admit_side_connection(action_deadline, || {
                                abort.check_ctrl_c(&ctrl_c);
                                abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
                                abort.check_deadline(unix_ms(), deadline_unix_ms);
                                if abort.should_stop_new_sessions()
                                    || revalidate_or_abort(&protection, &mut abort).is_some()
                                {
                                    return Err("side HTTP connection admission stopped".into());
                                }
                                Ok(())
                            })
                            .map(|_| ())
                            .map_err(|error| {
                                loadtest_core::side_http::SideHttpError::Admission(
                                    error.to_string(),
                                )
                            }),
                        SideHttpAdmission::Message { writes } => dispatch_admission
                            .admit_side_message_with_writes(
                                u64::from(writes),
                                action_deadline,
                                || {
                                    abort.check_ctrl_c(&ctrl_c);
                                    abort.check_stop_file(
                                        config.stop_file.as_deref().map(Path::new),
                                    );
                                    abort.check_deadline(unix_ms(), deadline_unix_ms);
                                    if abort.should_stop_new_sessions()
                                        || revalidate_or_abort(&protection, &mut abort).is_some()
                                    {
                                        return Err("side HTTP admission stopped".into());
                                    }
                                    Ok(())
                                },
                            )
                            .map(|_| ())
                            .map_err(|error| {
                                loadtest_core::side_http::SideHttpError::Admission(
                                    error.to_string(),
                                )
                            }),
                    },
                ) {
                    Ok(http_metrics) => http_metrics.merge_into_metrics(&mut core_metrics),
                    Err(error) => {
                        errors.push(
                            "side_http_execution_failed",
                            format!("mail/announce HTTP execution failed: {error:?}"),
                            Default::default(),
                        );
                        failed = true;
                        execution_failed = true;
                    }
                }
                side_credentials = None;
            }
            if (live_chat || live_match || live_match_internal || live_http)
                && deferred_logout
                && pre_game_auth_completed
            {
                if !can_attempt_deferred_logout(deferred_logout, pre_game_auth_completed, &abort) {
                    errors.push(
                        "deferred_logout_not_dispatched",
                        deferred_logout_skip_message(&abort),
                        Default::default(),
                    );
                    failed = true;
                } else if execute_deferred_logout(&mut transport, &mut execution, |request| {
                    dispatch_admission
                        .admit(request, action_deadline, || {
                            abort.check_ctrl_c(&ctrl_c);
                            abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
                            abort.check_deadline(unix_ms(), deadline_unix_ms);
                            if abort.should_stop_new_sessions()
                                || revalidate_or_abort(&protection, &mut abort).is_some()
                            {
                                return Err("side-service deferred logout stopped".into());
                            }
                            Ok(())
                        })
                        .map_err(|error| map_auth_admission_to_string(&mut abort, error))
                })
                .is_err()
                {
                    errors.push(
                        "deferred_logout_failed",
                        "post-side-service logout failed",
                        Default::default(),
                    );
                    failed = true;
                }
            }
            finish_live_action(&mut auth_metrics, &execution.metrics, &mut abort, &budget);
            if execution_failed {
                break;
            }
        }
        if tick.actions.is_empty() && !scheduler.exhausted() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    for lease in &leases {
        let _ = account_pool.release(lease);
    }
    auth_metrics.set_wall_clock_window_ms(monotonic_started.elapsed().as_millis() as u64);
    record_auth_metrics(&mut core_metrics, &auth_metrics);
    core_metrics.increment(
        "auth_potential_data_writes",
        dispatch_admission.used_data_writes(),
    );
    let status = if abort.should_stop_new_sessions() {
        lifecycle.transition(RunState::Aborting).unwrap();
        lifecycle.transition(RunState::Aborted).unwrap();
        "aborted"
    } else if failed {
        lifecycle.transition(RunState::Failed).unwrap();
        "failed"
    } else {
        lifecycle.transition(RunState::CoolingDown).unwrap();
        lifecycle.transition(RunState::Completed).unwrap();
        "completed"
    };
    let abort_reason = abort.reason().map(|reason| format!("{reason:?}"));
    let report = write_report(
        Path::new(&config.reports_root),
        ReportInput {
            run_id: &format!("auth-{}-{}", std::process::id(), started),
            config: &config,
            effective_budget: &budget,
            status,
            abort_reason: abort_reason.as_deref(),
            shutdown_phase: None,
            deadline_unix_ms,
            graceful_shutdown_ms: config.graceful_shutdown_ms,
            started_unix_ms: started,
            ended_unix_ms: unix_ms(),
            metrics: core_metrics.snapshot(),
            resources: ResourceSampler.sample(0, 0, 0),
            errors: &errors,
            auth_metrics: Some(&auth_metrics),
            calibration: None,
        },
    )
    .map_err(|error| error.to_string())?;
    if status != "completed" {
        return Err(format!(
            "auth run ended with status={status}; report={}",
            report.display()
        ));
    }
    println!("auth run completed. report={}", report.display());
    Ok(())
}

fn validate_live_auth_load_model(model: &LoadModel) -> Result<(), String> {
    match model {
        LoadModel::ArrivalRate { .. } | LoadModel::Staged { .. } | LoadModel::Burst { .. } => {
            Ok(())
        }
        LoadModel::FixedConcurrency { .. } => Err(
            "live auth does not support fixed_concurrency: the synchronous executor cannot maintain declared concurrent flows; use arrival_rate, staged, or burst"
                .into(),
        ),
    }
}

fn validate_live_game_load_model(
    model: &LoadModel,
    coordination: LiveGameplayCoordination,
) -> Result<(), String> {
    match (coordination, model) {
        (
            LiveGameplayCoordination::TwoPlayerDefaultMatch,
            LoadModel::Staged { stages },
        ) if stages.len() == 1 && stages[0].virtual_players == 2 => Ok(()),
        (LiveGameplayCoordination::TwoPlayerDefaultMatch, _) => Err(
            "two-player live game runner requires one staged wave with virtual_players=2".into(),
        ),
        (_, LoadModel::FixedConcurrency {
            virtual_players: 1,
            ..
        }) => Ok(()),
        (_, LoadModel::Staged { stages })
            if stages.len() == 1 && stages[0].virtual_players == 1 =>
        {
            Ok(())
        }
        _ => Err(
            "live game runner currently requires one bounded virtual-player flow; use fixed_concurrency=1 or one staged wave with virtual_players=1"
                .into(),
        ),
    }
}

fn validate_game_execution_gate(cli: &Cli, config: &LoadTestConfig) -> Result<(), String> {
    GameExecutionGate {
        execute_game: cli.execute_game,
        confirm_game: cli.confirm_game.as_deref(),
        environment: &config.environment.name,
        account_manifest_supplied: cli.account_manifest.is_some(),
        private_config_supplied: cli.private_config.is_some(),
    }
    .validate()
    .map_err(|error| error.to_string())
}

fn map_auth_admission_to_string(abort: &mut AbortController, error: AuthAdmissionError) -> String {
    match error {
        AuthAdmissionError::BudgetExceeded(error) => {
            abort.request(AbortReason::BudgetExceeded);
            error
        }
        AuthAdmissionError::DeadlineExceeded => {
            abort.request(AbortReason::Deadline);
            "auth admission deadline elapsed before request dispatch".into()
        }
        AuthAdmissionError::Stopped(error) => error,
    }
}

fn map_auth_admission_to_game_error(
    abort: &mut AbortController,
    error: AuthAdmissionError,
) -> GameLiveError {
    match error {
        AuthAdmissionError::BudgetExceeded(_) => {
            abort.request(AbortReason::BudgetExceeded);
            GameLiveError::Transport("game budget exhausted")
        }
        AuthAdmissionError::DeadlineExceeded => {
            abort.request(AbortReason::Deadline);
            GameLiveError::Transport("game deadline elapsed")
        }
        AuthAdmissionError::Stopped(_) => GameLiveError::Transport("game admission stopped"),
    }
}

fn effective_deadline(
    config: &LoadTestConfig,
    budget: &loadtest_core::config::HardBudget,
    cli_deadline_unix_ms: Option<u64>,
    started_unix_ms: u64,
) -> Result<u64, String> {
    let budget_deadline =
        started_unix_ms.saturating_add(budget.max_duration_secs.saturating_mul(1_000));
    let configured_deadline = config.deadline_unix_ms.unwrap_or(budget_deadline);
    if configured_deadline > budget_deadline {
        return Err("deadline_unix_ms may not exceed the profile duration budget".into());
    }
    if let Some(cli_deadline) = cli_deadline_unix_ms {
        if cli_deadline > configured_deadline {
            return Err("--deadline-unix-ms may only tighten the configured deadline".into());
        }
        return Ok(cli_deadline);
    }
    Ok(configured_deadline)
}

fn show_report(directory: &Path) -> Result<(), String> {
    let summary = directory.join("summary.md");
    if !directory.join("run.json").is_file()
        || !directory.join("metrics.json").is_file()
        || !summary.is_file()
    {
        return Err(
            "report directory is incomplete; expected run.json, metrics.json and summary.md".into(),
        );
    }
    print!(
        "{}",
        std::fs::read_to_string(summary).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug)]
struct Cli {
    command: String,
    config: Option<PathBuf>,
    private_config: Option<PathBuf>,
    report_dir: Option<PathBuf>,
    account_manifest: Option<PathBuf>,
    allow_remote: bool,
    confirmation: Option<String>,
    dry_run: bool,
    execute_auth: bool,
    confirm_auth: Option<String>,
    execute_game: bool,
    confirm_game: Option<String>,
    deadline_unix_ms: Option<u64>,
    budget_override: BudgetOverride,
}

impl Cli {
    fn parse(arguments: Vec<String>) -> Result<Self, String> {
        let mut values = arguments.into_iter();
        let command = values.next().ok_or_else(usage)?;
        let mut cli = Self {
            command,
            config: None,
            private_config: None,
            report_dir: None,
            account_manifest: None,
            allow_remote: false,
            confirmation: None,
            dry_run: false,
            execute_auth: false,
            confirm_auth: None,
            execute_game: false,
            confirm_game: None,
            deadline_unix_ms: None,
            budget_override: BudgetOverride::default(),
        };
        while let Some(argument) = values.next() {
            match argument.as_str() {
                "--config" => {
                    cli.config = Some(PathBuf::from(
                        values.next().ok_or("--config requires a path")?,
                    ))
                }
                "--private-config" => {
                    cli.private_config = Some(PathBuf::from(
                        values.next().ok_or("--private-config requires a path")?,
                    ))
                }
                "--report-dir" => {
                    cli.report_dir = Some(PathBuf::from(
                        values.next().ok_or("--report-dir requires a path")?,
                    ))
                }
                "--account-manifest" => {
                    cli.account_manifest = Some(PathBuf::from(
                        values.next().ok_or("--account-manifest requires a path")?,
                    ))
                }
                "--allow-remote" => cli.allow_remote = true,
                "--confirm" => {
                    cli.confirmation = Some(
                        values
                            .next()
                            .ok_or("--confirm requires the environment name")?,
                    )
                }
                "--dry-run" => cli.dry_run = true,
                "--execute-auth" => cli.execute_auth = true,
                "--confirm-auth" => {
                    cli.confirm_auth = Some(
                        values
                            .next()
                            .ok_or("--confirm-auth requires the environment name")?,
                    )
                }
                "--execute-game" => cli.execute_game = true,
                "--confirm-game" => {
                    cli.confirm_game = Some(
                        values
                            .next()
                            .ok_or("--confirm-game requires the environment name")?,
                    )
                }
                "--deadline-unix-ms" => {
                    cli.deadline_unix_ms = Some(parse_value(values.next(), "--deadline-unix-ms")?)
                }
                "--max-virtual-players" => {
                    cli.budget_override.max_virtual_players =
                        Some(parse_value(values.next(), "--max-virtual-players")?)
                }
                "--max-login-qps" => {
                    cli.budget_override.max_login_qps =
                        Some(parse_value(values.next(), "--max-login-qps")?)
                }
                "--max-duration-secs" => {
                    cli.budget_override.max_duration_secs =
                        Some(parse_value(values.next(), "--max-duration-secs")?)
                }
                _ => return Err(format!("unknown argument {argument}\n{}", usage())),
            }
        }
        if cli.command != "report" && cli.config.is_none() {
            return Err("--config is required".into());
        }
        Ok(cli)
    }
    fn load(&self) -> Result<LoadTestConfig, String> {
        load_config(
            self.config.as_deref().expect("checked by parser"),
            self.private_config.as_deref(),
        )
        .map_err(|error| error.to_string())
    }
}

fn parse_value<T: std::str::FromStr>(value: Option<String>, flag: &str) -> Result<T, String> {
    value
        .ok_or_else(|| format!("{flag} requires a value"))?
        .parse()
        .map_err(|_| format!("{flag} has an invalid value"))
}
fn usage() -> String {
    "usage: loadtest validate|calibrate --config <file> [--private-config <file>] [--allow-remote --confirm <environment>] [--dry-run]\n       loadtest run --config <file> --dry-run\n       loadtest run --config <file> --execute-auth --confirm-auth <environment> --account-manifest <file> --private-config <file> [--allow-remote --confirm <environment>]\n       loadtest run --config <file> --execute-auth --confirm-auth <environment> --execute-game --confirm-game <environment> --account-manifest <file> --private-config <file> [--allow-remote --confirm <environment>]\n       loadtest report --report-dir <reports/run-id>".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use loadtest_core::abort::AbortReason;
    use loadtest_core::auth_http::{AuthHttpStatusCategory, AuthOutcomeCategory};

    #[test]
    fn game_failure_categories_are_closed_and_public_safe() {
        let categorized = GameLiveError::GameplayFailed {
            message: "ignored".into(),
            metrics: Default::default(),
            failure_category: Some("gameplay_input_timestamp_skew"),
        };
        assert_eq!(
            game_failure_category(&categorized),
            "gameplay_input_timestamp_skew"
        );
        assert_eq!(
            game_failure_category(&GameLiveError::Transport("ignored")),
            "game_runner_transport_or_contract_failed"
        );
        for (phase, expected) in [
            (
                loadtest_core::game_live::GameRunnerFailurePhase::ReconnectConnectionAdmission,
                "game_reconnect_connection_admission_failed",
            ),
            (
                loadtest_core::game_live::GameRunnerFailurePhase::ReconnectKcpConnect,
                "game_reconnect_kcp_connect_failed",
            ),
            (
                loadtest_core::game_live::GameRunnerFailurePhase::ReconnectDeadline,
                "game_reconnect_deadline_failed",
            ),
            (
                loadtest_core::game_live::GameRunnerFailurePhase::ReconnectAuth,
                "game_reconnect_auth_failed",
            ),
            (
                loadtest_core::game_live::GameRunnerFailurePhase::RoomReconnect,
                "game_room_reconnect_failed",
            ),
            (
                loadtest_core::game_live::GameRunnerFailurePhase::RoomLeave,
                "game_room_leave_failed",
            ),
        ] {
            let error = GameLiveError::RunnerFailed {
                phase,
                source: Box::new(GameLiveError::Transport("ticket=secret")),
            };
            assert_eq!(game_failure_category(&error), expected);
        }
    }

    #[test]
    fn dry_run_reconnect_burst_writes_offline_plan_metrics_without_transport() {
        let temp_root = std::env::temp_dir().join(format!(
            "loadtest-reconnect-dry-{}-{}",
            std::process::id(),
            unix_ms()
        ));
        let reports_root = temp_root.join("reports");
        let config_path = temp_root.join("reconnect.json");
        std::fs::create_dir_all(&temp_root).unwrap();
        std::fs::write(
            &config_path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "environment": {"name": "local", "kind": "local"},
                "targets": {"auth_http": "http://127.0.0.1:3000", "game_proxy": "kcp://127.0.0.1:4000"},
                "budget": {
                    "max_virtual_players": 1,
                    "max_login_qps": 10.0,
                    "max_new_connections_per_second": 10.0,
                    "max_business_messages_per_second": 10.0,
                    "max_messages_per_connection_per_second": 10.0,
                    "max_duration_secs": 10,
                    "max_total_operations": 8,
                    "max_error_rate": 0.1,
                    "max_connection_failure_rate": 0.1,
                    "max_p99_ms": 100,
                    "max_data_writes": 4
                },
                "scenario": {
                    "name": "offline-reconnect",
                    "load": {"type": "fixed_concurrency", "virtual_players": 1, "duration_secs": 1},
                    "reconnect_burst": {
                        "virtual_players": 1,
                        "reconnect_attempts_per_player": 2,
                        "reconnect_policy": {
                            "max_attempts": 2,
                            "base_delay_ms": 100,
                            "max_delay_ms": 500
                        }
                    }
                },
                "reports_root": reports_root,
                "prepare_reports_root": temp_root.join("prepare")
            }))
            .unwrap(),
        )
        .unwrap();
        execute(vec![
            "run".into(),
            "--config".into(),
            config_path.display().to_string(),
            "--dry-run".into(),
        ])
        .unwrap();
        let report_dir = std::fs::read_dir(&reports_root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let metrics: loadtest_core::metrics::MetricsSnapshot =
            serde_json::from_slice(&std::fs::read(report_dir.join("metrics.json")).unwrap())
                .unwrap();
        assert_eq!(metrics.counters["reconnect_burst_login_actions"], 1);
        assert_eq!(metrics.counters["reconnect_burst_new_connections"], 4);
        assert_eq!(metrics.counters["reconnect_burst_room_recoveries"], 2);
        assert_eq!(metrics.counters["reconnect_burst_backoff_ms"], 100);
        assert_eq!(metrics.counters["reconnect_burst_potential_data_writes"], 4);
        std::fs::remove_dir_all(temp_root).unwrap();
    }

    #[test]
    fn game_follow_up_failure_keeps_completed_auth_metrics_in_the_action_total() {
        let budget = loadtest_core::config::HardBudget {
            max_virtual_players: 1,
            max_login_qps: 1.0,
            max_new_connections_per_second: 1.0,
            max_business_messages_per_second: 1.0,
            max_messages_per_connection_per_second: 1.0,
            max_duration_secs: 1,
            max_total_operations: 3,
            max_error_rate: 1.0,
            max_connection_failure_rate: 1.0,
            max_p99_ms: 1_000,
            max_data_writes: 0,
        };
        let action = AuthRunMetrics {
            requests: 2,
            login_requests: 1,
            login_successes: 1,
            http_statuses: [(AuthHttpStatusCategory::Http2xx, 2)].into(),
            outcomes: [(AuthOutcomeCategory::Success, 2)].into(),
            ..Default::default()
        };
        let mut aggregate = AuthRunMetrics::default();
        let mut abort = AbortController::default();

        // This is the action finalizer used after an auth-complete game
        // ticket-missing or KCP-admission failure; it always retains auth data.
        finish_live_action(&mut aggregate, &action, &mut abort, &budget);

        assert_eq!(aggregate.requests, 2);
        assert_eq!(aggregate.login_requests, 1);
        assert_eq!(aggregate.login_successes, 1);
        assert_eq!(aggregate.outcomes[&AuthOutcomeCategory::Success], 2);
        assert!(!abort.should_stop_new_sessions());
    }

    #[test]
    fn game_admission_budget_and_deadline_failures_abort_consistently() {
        let mut budget_abort = AbortController::default();
        let budget_error = map_auth_admission_to_game_error(
            &mut budget_abort,
            AuthAdmissionError::BudgetExceeded("quota exhausted".into()),
        );
        assert_eq!(budget_abort.reason(), Some(&AbortReason::BudgetExceeded));
        assert!(budget_error.to_string().contains("budget"));

        let mut deadline_abort = AbortController::default();
        let deadline_error = map_auth_admission_to_game_error(
            &mut deadline_abort,
            AuthAdmissionError::DeadlineExceeded,
        );
        assert_eq!(deadline_abort.reason(), Some(&AbortReason::Deadline));
        assert!(deadline_error.to_string().contains("deadline"));
    }

    #[test]
    fn deferred_logout_cleanup_never_overrides_an_existing_abort_reason() {
        let mut budget_abort = AbortController::default();
        budget_abort.request(AbortReason::BudgetExceeded);
        assert!(!can_attempt_deferred_logout(true, true, &budget_abort));
        assert!(deferred_logout_skip_message(&budget_abort).contains("budget"));

        let mut deadline_abort = AbortController::default();
        deadline_abort.request(AbortReason::Deadline);
        assert!(!can_attempt_deferred_logout(true, true, &deadline_abort));
        assert!(deferred_logout_skip_message(&deadline_abort).contains("deadline"));

        let clean = AbortController::default();
        assert!(can_attempt_deferred_logout(true, true, &clean));
        assert!(!can_attempt_deferred_logout(true, false, &clean));
    }

    #[test]
    fn completed_game_session_metrics_are_recorded_after_cleanup() {
        let events = std::cell::RefCell::new(Vec::new());

        finish_game_action_after_cleanup(
            true,
            || events.borrow_mut().push("cleanup"),
            || events.borrow_mut().push("game_metrics"),
        );

        assert_eq!(events.into_inner(), vec!["cleanup", "game_metrics"]);
    }

    #[test]
    fn local_default_match_composite_is_sequential_and_rejects_non_player_match_paths() {
        assert!(
            validate_live_game_side_service_composite(
                EnvironmentKind::Local,
                true,
                true,
                true,
                false,
                false,
                true,
            )
            .unwrap()
        );
        assert!(
            validate_live_game_side_service_composite(
                EnvironmentKind::Test,
                true,
                true,
                false,
                false,
                false,
                true,
            )
            .unwrap()
        );
        assert!(
            validate_live_game_side_service_composite(
                EnvironmentKind::Production,
                true,
                true,
                true,
                false,
                false,
                false,
            )
            .is_err()
        );
        assert!(
            validate_live_game_side_service_composite(
                EnvironmentKind::Local,
                true,
                false,
                true,
                false,
                false,
                false,
            )
            .is_err()
        );
        assert!(
            validate_live_game_side_service_composite(
                EnvironmentKind::Local,
                true,
                true,
                false,
                true,
                false,
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn composite_chat_admission_reserves_private_and_group_writes() {
        use loadtest_core::side_services::{PlannedSideServiceStep, SideServiceOperation};

        let steps = [
            PlannedSideServiceStep {
                service: SideServiceKind::Chat,
                operation: SideServiceOperation::ChatAuth,
                weight: 1,
                think_time_ms: 0,
            },
            PlannedSideServiceStep {
                service: SideServiceKind::Chat,
                operation: SideServiceOperation::ChatPrivate,
                weight: 1,
                think_time_ms: 0,
            },
            PlannedSideServiceStep {
                service: SideServiceKind::Chat,
                operation: SideServiceOperation::ChatHistory,
                weight: 1,
                think_time_ms: 0,
            },
        ];
        assert_eq!(composite_chat_admission_writes(&steps).unwrap(), [0, 1, 0]);

        let implicit_auth = [PlannedSideServiceStep {
            service: SideServiceKind::Chat,
            operation: SideServiceOperation::ChatGroup,
            weight: 1,
            think_time_ms: 0,
        }];
        assert_eq!(
            composite_chat_admission_writes(&implicit_auth).unwrap(),
            [0, 1]
        );
    }

    #[test]
    fn composite_http_filter_uses_each_service_live_gate() {
        let side = loadtest_core::side_services::SideServicesScenario {
            mail: Some(loadtest_core::side_services::SideServiceConfig {
                live_http: false,
                ..Default::default()
            }),
            announce: Some(loadtest_core::side_services::SideServiceConfig {
                live_http: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(!live_composite_http_enabled(&side, SideServiceKind::Mail));
        assert!(live_composite_http_enabled(
            &side,
            SideServiceKind::Announce
        ));
    }

    #[test]
    fn real_auth_run_requires_execute_and_exact_environment_confirmation_before_transport_setup() {
        let missing_execute = execute(vec![
            "run".into(),
            "--config".into(),
            "not-read-without-execute.json".into(),
        ])
        .unwrap_err();
        assert!(missing_execute.contains("--execute-auth"));

        let config_path = std::env::temp_dir().join(format!(
            "loadtest-auth-gate-{}-{}.json",
            std::process::id(),
            unix_ms()
        ));
        std::fs::write(
            &config_path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "environment": {"name": "local", "kind": "local"},
                "targets": {"auth_http": "http://127.0.0.1:3000", "game_proxy": "kcp://127.0.0.1:4000"},
                "budget": {"max_virtual_players": 1, "max_login_qps": 1.0, "max_new_connections_per_second": 1.0, "max_business_messages_per_second": 1.0, "max_messages_per_connection_per_second": 1.0, "max_duration_secs": 1, "max_total_operations": 1, "max_error_rate": 0.1, "max_connection_failure_rate": 0.1, "max_p99_ms": 100, "max_data_writes": 0},
                "scenario": {"name": "auth", "load": {"type": "fixed_concurrency", "virtual_players": 1, "duration_secs": 1}, "auth": {"operations": ["login"]}},
                "reports_root": "reports",
                "prepare_reports_root": "prepare"
            }))
            .unwrap(),
        )
        .unwrap();
        let confirmation_missing = execute(vec![
            "run".into(),
            "--config".into(),
            config_path.display().to_string(),
            "--execute-auth".into(),
        ])
        .unwrap_err();
        assert!(confirmation_missing.contains("--confirm-auth"));

        let unsupported_model = execute(vec![
            "run".into(),
            "--config".into(),
            config_path.display().to_string(),
            "--execute-auth".into(),
            "--confirm-auth".into(),
            "local".into(),
        ])
        .unwrap_err();
        assert!(unsupported_model.contains("does not support fixed_concurrency"));
        assert!(!unsupported_model.contains("--account-manifest"));

        std::fs::write(
            &config_path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "environment": {"name": "local", "kind": "local"},
                "targets": {"auth_http": "http://127.0.0.1:3000", "game_proxy": "kcp://127.0.0.1:4000"},
                "budget": {"max_virtual_players": 1, "max_login_qps": 1.0, "max_new_connections_per_second": 1.0, "max_business_messages_per_second": 1.0, "max_messages_per_connection_per_second": 1.0, "max_duration_secs": 2, "max_total_operations": 1, "max_error_rate": 0.1, "max_connection_failure_rate": 0.1, "max_p99_ms": 100, "max_data_writes": 3},
                "scenario": {"name": "auth", "load": {"type": "staged", "stages": [{"name": "wave", "virtual_players": 1, "duration_secs": 1}]}, "auth": {"operations": ["login"]}},
                "reports_root": "reports",
                "prepare_reports_root": "prepare"
            }))
            .unwrap(),
        )
        .unwrap();
        let staged_model = execute(vec![
            "run".into(),
            "--config".into(),
            config_path.display().to_string(),
            "--execute-auth".into(),
            "--confirm-auth".into(),
            "local".into(),
        ])
        .unwrap_err();
        assert!(staged_model.contains("--account-manifest"));
        assert!(!staged_model.contains("does not support staged"));

        std::fs::write(
            &config_path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "environment": {"name": "local", "kind": "local"},
                "targets": {"auth_http": "http://127.0.0.1:3000", "game_proxy": "kcp://127.0.0.1:4000"},
                "budget": {"max_virtual_players": 3, "max_login_qps": 2.0, "max_new_connections_per_second": 2.0, "max_business_messages_per_second": 2.0, "max_messages_per_connection_per_second": 2.0, "max_duration_secs": 1, "max_total_operations": 3, "max_error_rate": 0.1, "max_connection_failure_rate": 0.1, "max_p99_ms": 100, "max_data_writes": 9},
                "scenario": {"name": "auth", "load": {"type": "staged", "stages": [{"name": "too-fast", "virtual_players": 3, "duration_secs": 1}]}, "auth": {"operations": ["login"]}},
                "reports_root": "reports",
                "prepare_reports_root": "prepare"
            }))
            .unwrap(),
        )
        .unwrap();
        let stage_window = execute(vec![
            "run".into(),
            "--config".into(),
            config_path.display().to_string(),
            "--execute-auth".into(),
            "--confirm-auth".into(),
            "local".into(),
        ])
        .unwrap_err();
        assert!(stage_window.contains("stage 'too-fast'"));
        assert!(!stage_window.contains("--account-manifest"));
        std::fs::remove_file(config_path).unwrap();
    }

    #[test]
    fn game_execution_requires_both_explicit_gates_before_any_transport_setup() {
        let config_path = std::env::temp_dir().join(format!(
            "loadtest-game-gate-{}-{}.json",
            std::process::id(),
            unix_ms()
        ));
        std::fs::write(
            &config_path,
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "environment": {"name": "local", "kind": "local"},
                "targets": {"auth_http": "http://127.0.0.1:3000", "game_proxy": "kcp://127.0.0.1:4000"},
                "budget": {"max_virtual_players": 1, "max_login_qps": 10.0, "max_new_connections_per_second": 10.0, "max_business_messages_per_second": 10.0, "max_messages_per_connection_per_second": 10.0, "max_duration_secs": 10, "max_total_operations": 16, "max_error_rate": 0.1, "max_connection_failure_rate": 0.1, "max_p99_ms": 100, "max_data_writes": 16},
                "scenario": {"name": "game", "load": {"type": "fixed_concurrency", "virtual_players": 1, "duration_secs": 10}, "auth": {"operations": ["login", "list_characters", "select_character", "issue_ticket", "logout"]}},
                "reports_root": "reports",
                "prepare_reports_root": "prepare"
            }))
            .unwrap(),
        )
        .unwrap();

        let auth_missing = execute(vec![
            "run".into(),
            "--config".into(),
            config_path.display().to_string(),
            "--execute-game".into(),
        ])
        .unwrap_err();
        assert!(auth_missing.contains("--execute-auth"));

        let game_confirmation_missing = execute(vec![
            "run".into(),
            "--config".into(),
            config_path.display().to_string(),
            "--execute-auth".into(),
            "--confirm-auth".into(),
            "local".into(),
            "--execute-game".into(),
        ])
        .unwrap_err();
        assert!(game_confirmation_missing.contains("--confirm-game"));

        let manifest_missing = execute(vec![
            "run".into(),
            "--config".into(),
            config_path.display().to_string(),
            "--execute-auth".into(),
            "--confirm-auth".into(),
            "local".into(),
            "--execute-game".into(),
            "--confirm-game".into(),
            "local".into(),
        ])
        .unwrap_err();
        assert!(manifest_missing.contains("--account-manifest"));
        assert!(!manifest_missing.contains("does not support fixed_concurrency"));
        std::fs::remove_file(config_path).unwrap();
    }
}
