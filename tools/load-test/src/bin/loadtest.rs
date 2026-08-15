use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use game_protocol::{MessageType, Packet};
use loadtest_core::SCHEMA_VERSION;
use loadtest_core::abort::{
    AbortController, AbortReason, ContinuousHealthEvaluator, ContinuousHealthObservation,
    GracefulShutdown, ShutdownPhase, install_ctrl_c_flag,
};
use loadtest_core::accounts::{
    AccountLease, AccountLeasePool, EnvironmentSecretProvider, SecretProvider, auth_login_name,
    read_manifest,
};
use loadtest_core::auth_budget::{
    LIVE_GAMEPLAY_POTENTIAL_WRITES_PER_MESSAGE, estimate_auth_run_with_guard_probes,
    validate_auth_run_budget, validate_game_run_budget_for_scenario,
    validate_staged_auth_windows_with_guard_probes,
};
use loadtest_core::auth_http::{
    AuthAdmissionError, AuthDispatchAdmission, AuthHttpRequest, AuthHttpTransport,
    AuthResponseBody, AuthRunMetrics, FakeAuthHttpService, FakeAuthOutcome,
    ReqwestAuthHttpTransport, execute_auth_operations, execute_deferred_logout,
    send_with_bounded_retry_after_admission, split_game_auth_operations,
};
use loadtest_core::calibration::{
    CalibrationRun, bounded_calibration_duration_ms, bounded_calibration_operations,
    progressive_levels, run_local_workload,
};
use loadtest_core::chat_wss::execute_live_chat_steps;
use loadtest_core::config::{
    AuthOperation, BudgetOverride, EnvironmentKind, LiveGameplayCoordination, LoadModel,
    LoadTestConfig, RegistryObservationConfig, RunAccess, load_config, load_private_config,
};
use loadtest_core::contracts::{RunPlan, single_process_assignment};
use loadtest_core::deadline::monotonic_deadline_from_unix_ms;
use loadtest_core::game_kcp::{GameProxyEndpoint, KcpBackpressureMetrics, ReconnectPolicy};
use loadtest_core::game_live::{
    GameExecutionGate, GameLiveError, GameRunnerCheckpoint, GameSessionRunner, LiveKcpConnection,
    LiveKcpTransport,
};
use loadtest_core::lifecycle::{Lifecycle, RunState};
use loadtest_core::match_grpc::{
    MatchGrpcBackpressureMetrics, MatchInternalAdmission, execute_live_match_internal_steps,
    execute_live_match_steps,
};
use loadtest_core::metrics::Metrics;
use loadtest_core::preflight::{summarize_run, summarize_run_with_guard_probes};
use loadtest_core::protection::{
    DryRunProtection, LiveAuthProtection, RuntimeProtection, revalidate_or_abort,
};
use loadtest_core::reconnect_burst::{
    ROOM_HANDOFF_RETRY_BACKOFF_MS, ReconnectBurstAction, ReconnectBurstAdmission,
    ReconnectBurstExecutionGate, ReconnectBurstExecutor, ReconnectBurstSpec, ReconnectBurstStep,
    estimate_live_reconnect_burst, execute_reconnect_burst, plan_reconnect_burst,
    validate_live_reconnect_burst_budget,
};
use loadtest_core::registry_observation::{
    RegistryObservationError, RegistryObservationReport, RegistryObservationRequest,
    collect_runtime_registry_observation, registry_recheck_interval_ms,
};
use loadtest_core::report::{ErrorBuffer, ReportInput, write_report};
use loadtest_core::resource::ResourceSampler;
use loadtest_core::scheduler::MonotonicScheduler;
use loadtest_core::side_http::{
    MailClaimFailure, SideHttpAdmission, SideHttpError, execute_live_mail_announce_steps,
};
use loadtest_core::side_services::{
    AuthServicesPayload, DescriptorChangeTracker, SideServiceKind, SideServiceOperation,
    SideServicesScenario, execute_side_services_dry, resolve_auth_service_descriptors,
};
use loadtest_core::virtual_player::{VirtualPlayerEvent, VirtualPlayerSession};
use prost::Message;

static NEXT_MATCH_INTERNAL_DIAGNOSTIC_ID: AtomicU64 = AtomicU64::new(1);

/// Remote authenticated-player execution has two protection cadences. A
/// credential-free guard probe validates DNS/TLS/health immediately before an
/// admitted auth attempt; high-frequency controller and KCP checkpoints only
/// validate the bounded test window so they cannot emit unbudgeted HTTP.
trait AuthenticatedPlayerProtection: RuntimeProtection {
    fn revalidate_while_waiting(&self) -> Result<(), String> {
        self.revalidate()
    }

    fn revalidate_before_auth_dispatch(&self) -> Result<(), String> {
        self.revalidate()
    }

    fn uses_guard_probe(&self) -> bool {
        false
    }

    fn observe_auth_services(&self, _services: Option<&AuthServicesPayload>) -> Result<(), String> {
        Ok(())
    }
}

impl AuthenticatedPlayerProtection for DryRunProtection<'_> {}

impl AuthenticatedPlayerProtection for LiveAuthProtection<'_> {
    fn revalidate_while_waiting(&self) -> Result<(), String> {
        LiveAuthProtection::revalidate_while_waiting(self)
    }

    fn uses_guard_probe(&self) -> bool {
        true
    }

    fn observe_auth_services(&self, services: Option<&AuthServicesPayload>) -> Result<(), String> {
        LiveAuthProtection::observe_auth_services(self, services)
    }
}

enum RunPlayerProtection<'a> {
    Local(DryRunProtection<'a>),
    Remote(LiveAuthProtection<'a>),
}

impl RuntimeProtection for RunPlayerProtection<'_> {
    fn verify_dns(&self) -> Result<(), String> {
        match self {
            Self::Local(protection) => protection.verify_dns(),
            Self::Remote(protection) => protection.verify_dns(),
        }
    }

    fn verify_certificate(&self) -> Result<(), String> {
        match self {
            Self::Local(protection) => protection.verify_certificate(),
            Self::Remote(protection) => protection.verify_certificate(),
        }
    }

    fn verify_descriptor(&self) -> Result<(), String> {
        match self {
            Self::Local(protection) => protection.verify_descriptor(),
            Self::Remote(protection) => protection.verify_descriptor(),
        }
    }

    fn verify_environment_identity(&self) -> Result<(), String> {
        match self {
            Self::Local(protection) => protection.verify_environment_identity(),
            Self::Remote(protection) => protection.verify_environment_identity(),
        }
    }

    fn revalidate(&self) -> Result<(), String> {
        match self {
            Self::Local(protection) => protection.revalidate(),
            Self::Remote(protection) => protection.revalidate_while_waiting(),
        }
    }
}

impl AuthenticatedPlayerProtection for RunPlayerProtection<'_> {
    fn revalidate_while_waiting(&self) -> Result<(), String> {
        match self {
            Self::Local(protection) => protection.revalidate_while_waiting(),
            Self::Remote(protection) => protection.revalidate_while_waiting(),
        }
    }

    fn revalidate_before_auth_dispatch(&self) -> Result<(), String> {
        match self {
            Self::Local(protection) => protection.revalidate_before_auth_dispatch(),
            Self::Remote(protection) => protection.revalidate_before_auth_dispatch(),
        }
    }

    fn uses_guard_probe(&self) -> bool {
        match self {
            Self::Local(protection) => protection.uses_guard_probe(),
            Self::Remote(protection) => protection.uses_guard_probe(),
        }
    }

    fn observe_auth_services(&self, services: Option<&AuthServicesPayload>) -> Result<(), String> {
        match self {
            Self::Local(protection) => protection.observe_auth_services(services),
            Self::Remote(protection) => protection.observe_auth_services(services),
        }
    }
}

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
        "observe-registry" => observe_registry(&parsed)?,
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

/// Runs the registry/metrics observer without constructing any player, auth,
/// or game transport. Remote collection still establishes the credential-free
/// public auth target baseline before the Redis adapter can connect.
fn observe_registry(cli: &Cli) -> Result<(), String> {
    if cli.dry_run
        || cli.execute_auth
        || cli.execute_game
        || cli.confirm_auth.is_some()
        || cli.confirm_game.is_some()
    {
        return Err(
            "observe-registry is a live read-only command and does not accept --dry-run, --execute-auth, --confirm-auth, --execute-game, or --confirm-game".into(),
        );
    }
    if cli.private_config.is_some() || cli.account_manifest.is_some() {
        return Err(
            "observe-registry does not accept --private-config or --account-manifest because it performs no account operations".into(),
        );
    }

    let config = cli.load()?;
    validate(&config, cli)?;
    let budget = config
        .effective_budget(&cli.budget_override)
        .map_err(|error| error.to_string())?;
    let registry_config = validate_registry_observation_smoke_config(&config, &budget)?;
    let started = unix_ms();
    let deadline_unix_ms = effective_deadline(&config, &budget, cli.deadline_unix_ms, started)?;
    let preflight = summarize_run_with_guard_probes(
        "observe-registry",
        &config,
        &budget,
        RunAccess {
            allow_remote: cli.allow_remote,
            confirmation: cli.confirmation.as_deref(),
        },
        deadline_unix_ms,
        false,
        false,
        config.environment.kind.is_remote(),
    )?;
    println!(
        "preflight={}",
        serde_json::to_string(&preflight).expect("preflight summary serializes")
    );

    let run_id = format!("registry-observation-{}-{started}", std::process::id());
    let ctrl_c = install_ctrl_c_flag()
        .map_err(|error| format!("failed to install Ctrl+C handler: {error}"))?;
    let mut abort = AbortController::default();
    abort.check_ctrl_c(&ctrl_c);
    abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
    abort.check_deadline(unix_ms(), deadline_unix_ms);
    if let Some(reason) = abort.reason() {
        return write_registry_observation_failure(
            &config,
            &budget,
            &run_id,
            started,
            deadline_unix_ms,
            None,
            &format!("{reason:?}"),
            "registry_observation_stopped",
            "read-only registry observation stopped before target verification",
            Default::default(),
        );
    }

    let protection_error = if config.environment.kind.is_remote() {
        let target_protection = LiveAuthProtection::new(&config, Duration::from_secs(5))?;
        revalidate_or_abort(&target_protection, &mut abort)
    } else {
        revalidate_or_abort(&DryRunProtection::new(&config), &mut abort)
    };
    if let Some(error) = protection_error {
        return write_registry_observation_failure(
            &config,
            &budget,
            &run_id,
            started,
            deadline_unix_ms,
            None,
            "ProtectionUnknown",
            "registry_target_protection_unavailable",
            &format!("read-only registry observation target verification failed: {error}"),
            Default::default(),
        );
    }
    abort.check_ctrl_c(&ctrl_c);
    abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
    abort.check_deadline(unix_ms(), deadline_unix_ms);
    if let Some(reason) = abort.reason() {
        return write_registry_observation_failure(
            &config,
            &budget,
            &run_id,
            started,
            deadline_unix_ms,
            None,
            &format!("{reason:?}"),
            "registry_observation_stopped",
            "read-only registry observation stopped before Redis collection",
            Default::default(),
        );
    }

    let observation = match classify_registry_preflight(collect_registry_observation_for_run(
        &run_id,
        started,
        registry_config,
    )) {
        RegistryPreflightDecision::Ready(report) => report,
        RegistryPreflightDecision::Incomplete(report) => {
            return write_registry_observation_failure(
                &config,
                &budget,
                &run_id,
                started,
                deadline_unix_ms,
                Some(&report),
                "MetricsStale",
                "registry_observation_incomplete",
                "read-only registry observation has explicit coverage holes",
                Default::default(),
            );
        }
        RegistryPreflightDecision::Unavailable(error) => {
            return write_registry_observation_failure(
                &config,
                &budget,
                &run_id,
                started,
                deadline_unix_ms,
                None,
                "MetricsStale",
                error.report_category(),
                error.report_message(),
                error.report_context(),
            );
        }
    };
    let mut metrics = Metrics::default();
    observation.merge_into_metrics(&mut metrics);
    let errors = ErrorBuffer::default();
    let report = write_report(
        Path::new(&config.reports_root),
        ReportInput {
            run_id: &run_id,
            config: &config,
            effective_budget: &budget,
            status: "completed",
            abort_reason: None,
            shutdown_phase: None,
            deadline_unix_ms,
            graceful_shutdown_ms: config.graceful_shutdown_ms,
            started_unix_ms: started,
            ended_unix_ms: unix_ms(),
            metrics: metrics.snapshot(),
            resources: ResourceSampler.sample(0, 0, 0),
            errors: &errors,
            auth_metrics: None,
            calibration: None,
            service_versions: None,
            registry_observation: Some(&observation),
        },
    )
    .map_err(|error| error.to_string())?;
    println!(
        "observe-registry completed without player traffic or data writes. report={}",
        report.display()
    );
    Ok(())
}

fn validate_registry_observation_smoke_config(
    config: &LoadTestConfig,
    budget: &loadtest_core::config::HardBudget,
) -> Result<RegistryObservationConfig, String> {
    let registry_config = config
        .scenario
        .registry_observation
        .clone()
        .ok_or("observe-registry requires scenario.registry_observation")?;
    if !matches!(
        config.environment.kind,
        EnvironmentKind::Local | EnvironmentKind::Test
    ) {
        return Err("observe-registry is restricted to explicit local/test diagnostics".into());
    }
    if config.scenario.writes_data || budget.max_data_writes != 0 {
        return Err("observe-registry requires writes_data=false and max_data_writes=0".into());
    }
    if budget.max_virtual_players != 1 || config.account_prepare.account_count != Some(1) {
        return Err("observe-registry requires exactly one declared virtual player/account".into());
    }
    if !matches!(
        &config.scenario.load,
        LoadModel::FixedConcurrency {
            virtual_players: 1,
            ..
        }
    ) {
        return Err("observe-registry requires fixed_concurrency with virtual_players=1".into());
    }
    if !config.scenario.steps.is_empty()
        || config.scenario.auth.is_some()
        || config.scenario.reconnect_burst.is_some()
        || config.scenario.live_gameplay.is_some()
        || config.scenario.side_services.is_some()
    {
        return Err(
            "observe-registry forbids player, auth, gameplay, reconnect, and side-service steps"
                .into(),
        );
    }
    Ok(registry_config)
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
    let mut health_evaluator = ContinuousHealthEvaluator::new(2)
        .map_err(|error| format!("continuous health evaluator rejected: {error}"))?;

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
    observe_controller_health(
        &mut health_evaluator,
        &mut abort,
        protection_error.is_none(),
        tick.dropped,
        tick.dropped,
        tick.queue_depth,
        budget.max_virtual_players as u64,
        None,
    );
    if !abort.should_stop_new_sessions() {
        if let Some(error) = revalidate_or_abort(&protection, &mut abort) {
            protection_error = Some(error);
        }
        observe_controller_health(
            &mut health_evaluator,
            &mut abort,
            protection_error.is_none(),
            tick.dropped,
            tick.dropped,
            tick.queue_depth,
            budget.max_virtual_players as u64,
            None,
        );
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
            metrics.increment(
                "reconnect_burst_forced_disconnects",
                plan.forced_disconnects,
            );
            metrics.increment("reconnect_burst_new_connections", plan.new_connections);
            metrics.increment(
                "reconnect_burst_room_recoveries",
                plan.actions
                    .iter()
                    .filter(|action| action.step == ReconnectBurstStep::RecoverRoom)
                    .count() as u64,
            );
            metrics.increment(
                "reconnect_burst_room_recovery_retry_slots",
                plan.actions
                    .iter()
                    .filter(|action| action.step == ReconnectBurstStep::RetryRecoverRoom)
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
            service_versions: None,
            registry_observation: None,
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
            service_versions: None,
            registry_observation: None,
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
    metrics.increment("auth_guard_probe_attempts", auth.guard_probe_attempts);
    metrics.increment("auth_guard_probe_successes", auth.guard_probe_successes);
    metrics.increment(
        "auth_guard_probe_connection_admissions",
        auth.guard_probe_connection_admissions,
    );
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
    if auth.guard_probe_latency_ms.count() > 0 {
        metrics.merge_latency("auth_guard_probe_ms", &auth.guard_probe_latency_ms);
    }
}

#[allow(clippy::too_many_arguments)]
fn admit_live_auth_request<P: AuthenticatedPlayerProtection>(
    admission: &mut AuthDispatchAdmission,
    request: &AuthHttpRequest,
    action_deadline: Instant,
    protection: &P,
    abort: &mut AbortController,
    ctrl_c: &AtomicBool,
    stop_file: Option<&Path>,
    deadline_unix_ms: u64,
    guard_metrics: &mut AuthRunMetrics,
) -> Result<Duration, String> {
    if protection.uses_guard_probe() {
        admission
            .admit_guard_probe(action_deadline, || {
                check_authenticated_player_checkpoint(
                    protection,
                    abort,
                    ctrl_c,
                    stop_file,
                    deadline_unix_ms,
                )
            })
            .map_err(|error| map_auth_admission_to_string(abort, error))?;
        if confirm_live_auth_guard(protection, guard_metrics).is_err() {
            abort.check_protection(false);
            return Err("remote auth target protection failed before request dispatch".into());
        }
    }
    admission
        .admit(request, action_deadline, || {
            check_authenticated_player_checkpoint(
                protection,
                abort,
                ctrl_c,
                stop_file,
                deadline_unix_ms,
            )
        })
        .map_err(|error| map_auth_admission_to_string(abort, error))
}

/// The full DNS/TLS/health/descriptor probe is shared by ordinary auth flows
/// and reconnect auth. Admission for the probe itself remains caller-owned so
/// reconnect can retain its pre-planned primary-action reservation.
fn confirm_live_auth_guard<P: AuthenticatedPlayerProtection>(
    protection: &P,
    guard_metrics: &mut AuthRunMetrics,
) -> Result<(), String> {
    let started = Instant::now();
    let guard_result = protection.revalidate_before_auth_dispatch();
    guard_metrics.record_guard_probe(started, guard_result.is_ok());
    guard_result.map_err(|_| "remote auth target protection failed before request dispatch".into())
}

fn send_reconnect_auth_with_guard<T, P>(
    transport: &mut T,
    request: AuthHttpRequest,
    admission: &mut ReconnectBurstAdmission<'_>,
    protection: &P,
    auth_metrics: &mut AuthRunMetrics,
) -> Result<loadtest_core::auth_http::AuthSuccess, String>
where
    T: AuthHttpTransport,
    P: AuthenticatedPlayerProtection,
{
    if protection.uses_guard_probe() {
        admission
            .admit_guard_probe()
            .map_err(|error| error.to_string())?;
        if confirm_live_auth_guard(protection, auth_metrics).is_err() {
            admission.mark_protection_failed();
            return Err("remote auth target protection failed before request dispatch".into());
        }
    }
    let mut request_metrics = AuthRunMetrics::default();
    let response = send_with_bounded_retry_after_admission(
        transport,
        request,
        0,
        &mut request_metrics,
        || {
            admission.revalidate().map_err(|error| error.to_string())?;
            admission.remaining().map_err(|error| error.to_string())
        },
    )?;
    auth_metrics.merge(&request_metrics);
    match response.body {
        AuthResponseBody::Success(success)
            if response.status.is_some_and(|status| status < 400) =>
        {
            Ok(success)
        }
        _ => Err("reconnect burst auth request did not succeed".into()),
    }
}

fn check_authenticated_player_checkpoint<P: AuthenticatedPlayerProtection>(
    protection: &P,
    abort: &mut AbortController,
    ctrl_c: &AtomicBool,
    stop_file: Option<&Path>,
    deadline_unix_ms: u64,
) -> Result<(), String> {
    abort.check_ctrl_c(ctrl_c);
    abort.check_stop_file(stop_file);
    abort.check_deadline(unix_ms(), deadline_unix_ms);
    if protection.revalidate_while_waiting().is_err() {
        abort.check_protection(false);
    }
    if abort.should_stop_new_sessions() {
        return Err("auth admission stopped before request dispatch".into());
    }
    Ok(())
}

/// Accumulates only the fixed-cardinality KCP/gRPC pressure state needed by
/// the controller. Individual transport sessions can end before the next
/// controller checkpoint, so a failed observation remains visible for the
/// rest of the run rather than being replaced by a later empty snapshot.
#[derive(Debug, Default)]
struct LiveBackpressureSignals {
    kcp: KcpBackpressureMetrics,
    match_grpc: MatchGrpcBackpressureMetrics,
}

impl LiveBackpressureSignals {
    fn record_kcp(&mut self, observed: KcpBackpressureMetrics) {
        self.kcp.pending_limit_rejections = self
            .kcp
            .pending_limit_rejections
            .max(observed.pending_limit_rejections);
        self.kcp.dropped_pending_requests = self
            .kcp
            .dropped_pending_requests
            .max(observed.dropped_pending_requests);
        self.kcp.disconnects = self.kcp.disconnects.max(observed.disconnects);
    }

    fn record_match_grpc(&mut self, observed: MatchGrpcBackpressureMetrics) {
        self.match_grpc.pending_limit_rejections = self
            .match_grpc
            .pending_limit_rejections
            .max(observed.pending_limit_rejections);
        self.match_grpc.dropped_pending_messages = self
            .match_grpc
            .dropped_pending_messages
            .max(observed.dropped_pending_messages);
        self.match_grpc.stream_disconnects = self
            .match_grpc
            .stream_disconnects
            .max(observed.stream_disconnects);
    }

    fn apply_to_health(&self, health: &mut ContinuousHealthObservation) {
        self.kcp.apply_to_health(health);
        self.match_grpc.apply_to_health(health);
    }
}

/// Applies the continuous controller health contract on each scheduler tick
/// and at live transport completion/checkpoint boundaries. The transport
/// protection result covers readiness and dependencies until a service-specific
/// read-only observer is available; scheduler and actual KCP/gRPC pressure
/// observations provide generator/consumer signals.
fn observe_controller_health(
    evaluator: &mut ContinuousHealthEvaluator,
    abort: &mut AbortController,
    protection_healthy: bool,
    metrics_dropped: u64,
    scheduler_dropped: u64,
    scheduler_queue_depth: u64,
    max_scheduler_queue_depth: u64,
    live_backpressure: Option<&LiveBackpressureSignals>,
) {
    let mut observation = ContinuousHealthObservation {
        readiness_healthy: protection_healthy,
        dependencies_available: protection_healthy,
        metrics_fresh: metrics_dropped == 0,
        sample_continuous: scheduler_dropped == 0,
        generator_healthy: scheduler_queue_depth <= max_scheduler_queue_depth,
        backpressure_healthy: scheduler_queue_depth <= max_scheduler_queue_depth,
    };
    if let Some(signals) = live_backpressure {
        signals.apply_to_health(&mut observation);
    }
    evaluator.observe(abort, observation);
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

const MAX_ROOM_RECOVERY_ASYNC_PUSHES: usize = 16;
const RECONNECT_FAILURE_PREFIX: &str = "reconnect_failure:";
const TEMPORARY_ROOM_HANDOFF_ERROR_CODE: &str = "PLAYER_NOT_OFFLINE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReconnectFailureCategory {
    BudgetExceeded,
    DeadlineExceeded,
    Stopped,
    GateRejected,
    ProtectionOrCheckpointFailed,
    RoomResponseTimeout,
    RoomUnexpectedPacket,
    RoomServerBusinessError,
    RoomBoundaryRejected,
    RoomAsyncPushLimit,
    RoomHandoffRetryExhausted,
    TransportFailed,
    ExecutionFailed,
}

impl ReconnectFailureCategory {
    fn report_category(self) -> &'static str {
        match self {
            Self::BudgetExceeded => "reconnect_burst_budget_exceeded",
            Self::DeadlineExceeded => "reconnect_burst_deadline_exceeded",
            Self::Stopped => "reconnect_burst_stopped",
            Self::GateRejected => "reconnect_burst_gate_rejected",
            Self::ProtectionOrCheckpointFailed => "reconnect_burst_protection_or_checkpoint_failed",
            Self::RoomResponseTimeout => "reconnect_burst_room_response_timeout",
            Self::RoomUnexpectedPacket => "reconnect_burst_room_unexpected_packet",
            Self::RoomServerBusinessError => "reconnect_burst_room_server_business_error",
            Self::RoomBoundaryRejected => "reconnect_burst_room_boundary_rejected",
            Self::RoomAsyncPushLimit => "reconnect_burst_room_async_push_limit",
            Self::RoomHandoffRetryExhausted => "reconnect_burst_room_handoff_retry_exhausted",
            Self::TransportFailed => "reconnect_burst_transport_failed",
            Self::ExecutionFailed => "reconnect_burst_execution_failed",
        }
    }

    fn report_message(self) -> &'static str {
        match self {
            Self::BudgetExceeded => "live reconnect burst exceeded its approved budget",
            Self::DeadlineExceeded => "live reconnect burst exceeded its deadline",
            Self::Stopped => "live reconnect burst was stopped",
            Self::GateRejected => "live reconnect burst execution gate rejected the run",
            Self::ProtectionOrCheckpointFailed => {
                "live reconnect burst protection or checkpoint failed"
            }
            Self::RoomResponseTimeout => "live reconnect burst room recovery response timed out",
            Self::RoomUnexpectedPacket => {
                "live reconnect burst room recovery received an unexpected packet"
            }
            Self::RoomServerBusinessError => {
                "live reconnect burst room recovery received a server business error"
            }
            Self::RoomBoundaryRejected => {
                "live reconnect burst room recovery left the approved room boundary"
            }
            Self::RoomAsyncPushLimit => {
                "live reconnect burst room recovery exceeded the async push limit"
            }
            Self::RoomHandoffRetryExhausted => {
                "live reconnect burst room handoff retry did not converge"
            }
            Self::TransportFailed => "live reconnect burst transport failed",
            Self::ExecutionFailed => "live reconnect burst did not complete",
        }
    }

    fn executor_error(self) -> String {
        format!("{RECONNECT_FAILURE_PREFIX}{}", self.report_category())
    }

    fn from_executor_error(error: &str) -> Option<Self> {
        if let Some(category) = error.strip_prefix(RECONNECT_FAILURE_PREFIX) {
            return match category {
                "reconnect_burst_room_response_timeout" => Some(Self::RoomResponseTimeout),
                "reconnect_burst_room_unexpected_packet" => Some(Self::RoomUnexpectedPacket),
                "reconnect_burst_room_server_business_error" => Some(Self::RoomServerBusinessError),
                "reconnect_burst_room_boundary_rejected" => Some(Self::RoomBoundaryRejected),
                "reconnect_burst_room_async_push_limit" => Some(Self::RoomAsyncPushLimit),
                "reconnect_burst_room_handoff_retry_exhausted" => {
                    Some(Self::RoomHandoffRetryExhausted)
                }
                "reconnect_burst_transport_failed" => Some(Self::TransportFailed),
                _ => None,
            };
        }
        match error {
            "reconnect burst action deadline elapsed" | "auth admission deadline elapsed" => {
                Some(Self::DeadlineExceeded)
            }
            "reconnect burst stopped while waiting for scheduled action"
            | "reconnect burst was stopped" => Some(Self::Stopped),
            "remote auth target protection failed before request dispatch"
            | "auth public game descriptor was rejected before reconnect dispatch" => {
                Some(Self::ProtectionOrCheckpointFailed)
            }
            "reconnect burst KCP transport setup failed"
            | "reconnect burst KCP connect failed"
            | "reconnect burst KCP auth write failed"
            | "reconnect burst KCP auth response failed"
            | "reconnect burst room recovery write failed" => Some(Self::TransportFailed),
            _ => None,
        }
    }
}

fn reconnect_execution_failure_category(
    error: &loadtest_core::reconnect_burst::ReconnectBurstExecutionError,
) -> ReconnectFailureCategory {
    use loadtest_core::reconnect_burst::ReconnectBurstExecutionError;

    match error {
        ReconnectBurstExecutionError::Gate(_) => ReconnectFailureCategory::GateRejected,
        ReconnectBurstExecutionError::BudgetMismatch
        | ReconnectBurstExecutionError::Admission(AuthAdmissionError::BudgetExceeded(_)) => {
            ReconnectFailureCategory::BudgetExceeded
        }
        ReconnectBurstExecutionError::Admission(AuthAdmissionError::DeadlineExceeded) => {
            ReconnectFailureCategory::DeadlineExceeded
        }
        ReconnectBurstExecutionError::Admission(AuthAdmissionError::Stopped(_))
        | ReconnectBurstExecutionError::Stopped => ReconnectFailureCategory::Stopped,
        ReconnectBurstExecutionError::Checkpoint(_) => {
            ReconnectFailureCategory::ProtectionOrCheckpointFailed
        }
        ReconnectBurstExecutionError::Executor(error) => {
            ReconnectFailureCategory::from_executor_error(error)
                .unwrap_or(ReconnectFailureCategory::ExecutionFailed)
        }
    }
}

fn record_reconnect_execution_failure(
    errors: &mut ErrorBuffer,
    error: &loadtest_core::reconnect_burst::ReconnectBurstExecutionError,
) {
    let category = reconnect_execution_failure_category(error);
    errors.push(
        category.report_category(),
        category.report_message(),
        Default::default(),
    );
}

fn is_room_recovery_async_push(message_type: MessageType) -> bool {
    matches!(
        message_type,
        MessageType::RoomStatePush
            | MessageType::GameMessagePush
            | MessageType::FrameBundlePush
            | MessageType::RoomFrameRatePush
            | MessageType::RoomMemberOfflinePush
            | MessageType::MovementSnapshotPush
            | MessageType::MovementRejectPush
    )
}

fn receive_room_recovery_response<R, H>(
    mut receive: R,
    mut handle_async_push: H,
    expected_response: MessageType,
) -> Result<Packet, ReconnectFailureCategory>
where
    R: FnMut() -> Result<Packet, ReconnectFailureCategory>,
    H: FnMut(Packet) -> Result<(), ReconnectFailureCategory>,
{
    let mut async_pushes = 0;
    loop {
        let packet = receive()?;
        let Some(message_type) = packet.message_type() else {
            return Err(ReconnectFailureCategory::RoomUnexpectedPacket);
        };
        if message_type == expected_response {
            return Ok(packet);
        }
        if message_type == MessageType::ErrorRes {
            return Err(ReconnectFailureCategory::RoomServerBusinessError);
        }
        if !is_room_recovery_async_push(message_type) {
            return Err(ReconnectFailureCategory::RoomUnexpectedPacket);
        }
        if async_pushes >= MAX_ROOM_RECOVERY_ASYNC_PUSHES {
            return Err(ReconnectFailureCategory::RoomAsyncPushLimit);
        }
        handle_async_push(packet)?;
        async_pushes = async_pushes.saturating_add(1);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RoomRecoveryResponse {
    Recovered,
    TemporaryHandoffRejected,
}

fn classify_room_recovery_response(
    response: &Packet,
    reconnect_attempt: u32,
    approved_room_id: &str,
) -> Result<RoomRecoveryResponse, ReconnectFailureCategory> {
    let (ok, room_id, error_code) = match reconnect_attempt {
        0 => {
            let response = loadtest_core::pb::RoomJoinRes::decode(response.body.as_slice())
                .map_err(|_| ReconnectFailureCategory::RoomUnexpectedPacket)?;
            (response.ok, response.room_id, response.error_code)
        }
        _ => {
            let response = loadtest_core::pb::RoomReconnectRes::decode(response.body.as_slice())
                .map_err(|_| ReconnectFailureCategory::RoomUnexpectedPacket)?;
            (response.ok, response.room_id, response.error_code)
        }
    };
    if !ok {
        if reconnect_attempt > 0 && error_code == TEMPORARY_ROOM_HANDOFF_ERROR_CODE {
            return Ok(RoomRecoveryResponse::TemporaryHandoffRejected);
        }
        return Err(ReconnectFailureCategory::RoomServerBusinessError);
    }
    if room_id != approved_room_id {
        return Err(ReconnectFailureCategory::RoomBoundaryRejected);
    }
    Ok(RoomRecoveryResponse::Recovered)
}

fn reconnect_room_receive_failure_category(error: GameLiveError) -> ReconnectFailureCategory {
    match error {
        GameLiveError::Transport("KCP read deadline elapsed")
        | GameLiveError::Transport("KCP session deadline elapsed") => {
            ReconnectFailureCategory::RoomResponseTimeout
        }
        _ => ReconnectFailureCategory::TransportFailed,
    }
}

fn wait_for_reconnect_action<R, S>(
    scheduled: Instant,
    deadline: Instant,
    mut revalidate: R,
    mut sleep: S,
) -> Result<(), ReconnectFailureCategory>
where
    R: FnMut() -> Result<(), ReconnectFailureCategory>,
    S: FnMut(Duration),
{
    loop {
        revalidate()?;
        let now = Instant::now();
        if now >= deadline {
            return Err(ReconnectFailureCategory::DeadlineExceeded);
        }
        if now >= scheduled {
            return Ok(());
        }
        sleep((scheduled - now).min(Duration::from_millis(25)));
    }
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

/// Mail claims are routed to the character's authoritative online game
/// session. Keep this phase deliberately narrower than the general post-game
/// side-service composite: exactly one list followed by one claim and its
/// idempotent replay for each of the two already-matched players.
fn requires_online_default_match_mail_claim_phase(
    side: &SideServicesScenario,
) -> Result<bool, String> {
    let Some(mail) = side.mail.as_ref() else {
        return Ok(false);
    };
    if !mail
        .steps
        .iter()
        .any(|step| step.operation == SideServiceOperation::MailClaim)
    {
        return Ok(false);
    }
    if side.chat.is_some() || side.announce.is_some() || side.r#match.is_some() {
        return Err(
            "online default_match mail-claim phase permits mail only; chat, announce, and match side services are forbidden"
                .into(),
        );
    }
    if !mail.live_http || !mail.writes || mail.write_batch.is_none() {
        return Err(
            "online default_match mail-claim phase requires an explicit writable live mail HTTP batch"
                .into(),
        );
    }
    let expected_steps = [
        SideServiceOperation::MailList,
        SideServiceOperation::MailClaim,
        SideServiceOperation::MailClaim,
    ];
    if mail.steps.len() != expected_steps.len()
        || mail
            .steps
            .iter()
            .zip(expected_steps)
            .any(|(actual, expected)| actual.operation != expected || actual.weight != 1)
    {
        return Err(
            "online default_match mail-claim phase requires exactly mail_list, mail_claim, mail_claim with weight=1"
                .into(),
        );
    }
    if side.composition.weights.get(&SideServiceKind::Mail) != Some(&1)
        || side.composition.weights.len() != 1
        || side.composition.max_operations_per_player != 3
        || side
            .composition
            .max_operations_per_service_per_player
            .get(&SideServiceKind::Mail)
            != Some(&3)
        || side.composition.max_operations_per_service_per_player.len() != 1
    {
        return Err(
            "online default_match mail-claim phase requires a mail-only composition capped at three operations per player"
                .into(),
        );
    }
    Ok(true)
}

/// Production has a deliberately narrower execution boundary than an
/// explicitly approved remote `Test` profile. The latter still passes the
/// complete remote `RunAccess`/window/allowlist/protection gates before this
/// point; it merely keeps the already-supported local/test diagnostic
/// transports available to an isolated environment.
fn validate_production_authenticated_player_chain(
    environment: EnvironmentKind,
    game_mode: bool,
    two_player_default_match: bool,
    reconnect_burst_mode: bool,
    live_chat: bool,
    live_match: bool,
    live_match_internal: bool,
    live_http: bool,
) -> Result<(), String> {
    if matches!(environment, EnvironmentKind::Local | EnvironmentKind::Test) {
        return Ok(());
    }
    if environment == EnvironmentKind::Staging {
        return Err(
            "remote live execution outside production is restricted to an explicitly approved test profile"
                .into(),
        );
    }
    if game_mode
        && two_player_default_match
        && !reconnect_burst_mode
        && !(live_chat || live_match || live_match_internal || live_http)
    {
        return Ok(());
    }
    Err(
        "production live execution is restricted to the authenticated two-player default_match player chain; chat, mail, announce, direct match diagnostics, reconnect bursts, and auth-only runs are forbidden"
            .into(),
    )
}

fn check_live_composite_side_controller<P: RuntimeProtection>(
    config: &LoadTestConfig,
    deadline_unix_ms: u64,
    ctrl_c: &AtomicBool,
    protection: &P,
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

fn live_composite_http_enabled(side: &SideServicesScenario, service: SideServiceKind) -> bool {
    match service {
        SideServiceKind::Mail => side.mail.as_ref().is_some_and(|config| config.live_http),
        SideServiceKind::Announce => side
            .announce
            .as_ref()
            .is_some_and(|config| config.live_http),
        _ => false,
    }
}

fn required_auth_side_services(side: &SideServicesScenario) -> BTreeSet<SideServiceKind> {
    [
        (
            SideServiceKind::Chat,
            side.chat
                .as_ref()
                .is_some_and(|config| config.live_websocket),
        ),
        (
            SideServiceKind::Mail,
            side.mail.as_ref().is_some_and(|config| config.live_http),
        ),
        (
            SideServiceKind::Announce,
            side.announce
                .as_ref()
                .is_some_and(|config| config.live_http),
        ),
    ]
    .into_iter()
    .filter_map(|(service, enabled)| enabled.then_some(service))
    .collect()
}

fn resolve_live_side_services(
    side: &SideServicesScenario,
    auth_services: Option<&AuthServicesPayload>,
    tracker: &mut DescriptorChangeTracker,
    metrics: &mut Metrics,
) -> Result<SideServicesScenario, String> {
    let observation_count = tracker.observations().len();
    let resolved = resolve_auth_service_descriptors(
        side,
        auth_services,
        &required_auth_side_services(side),
        tracker,
    )
    .map_err(|error| format!("auth-discovered side-service descriptor rejected: {error}"))?;
    for observation in &tracker.observations()[observation_count..] {
        metrics.increment("side_auth_descriptor_resolutions", 1);
        if observation.changed {
            metrics.increment("side_auth_descriptor_changes", 1);
        }
    }
    Ok(resolved)
}

#[derive(Debug)]
enum LiveGameSideServiceError {
    MailClaim(MailClaimFailure),
    Other,
}

impl From<()> for LiveGameSideServiceError {
    fn from(_: ()) -> Self {
        Self::Other
    }
}

impl LiveGameSideServiceError {
    fn from_side_http(error: SideHttpError) -> Self {
        match error {
            SideHttpError::MailClaimFailed(failure) => Self::MailClaim(failure),
            _ => Self::Other,
        }
    }

    fn report_details(&self) -> (&'static str, &'static str, BTreeMap<String, String>) {
        match self {
            Self::MailClaim(failure) => {
                let (category, message) = match failure.claim_status.as_str() {
                    "processing" => (
                        "online_mail_claim_processing",
                        "online mail claim is still processing",
                    ),
                    "reconciliation_pending" => (
                        "online_mail_claim_reconciliation_pending",
                        "online mail claim is pending reconciliation",
                    ),
                    "retryable_failure" => (
                        "online_mail_claim_retryable_failure",
                        "online mail claim needs a retry",
                    ),
                    "blocked_capacity" => (
                        "online_mail_claim_blocked_capacity",
                        "online mail claim is blocked by inventory capacity",
                    ),
                    "permanent_failure" => (
                        "online_mail_claim_permanent_failure",
                        "online mail claim failed permanently",
                    ),
                    "manual_review" => (
                        "online_mail_claim_manual_review",
                        "online mail claim requires manual review",
                    ),
                    _ => (
                        "online_mail_claim_rejected",
                        "online mail claim was not accepted",
                    ),
                };
                let mut context = BTreeMap::from([
                    ("claim_status".to_string(), failure.claim_status.clone()),
                    ("http_status".to_string(), failure.http_status.to_string()),
                ]);
                if let Some(error) = &failure.error {
                    context.insert("error".to_string(), error.clone());
                }
                (category, message, context)
            }
            Self::Other => (
                "online_mail_claim_execution_failed",
                "online mail claim did not complete",
                BTreeMap::new(),
            ),
        }
    }
}

/// Execute public side-service diagnostics for one authenticated player while
/// the caller holds the lifecycle phase required by that operation. Tickets
/// never cross account boundaries and the synchronous controller retains one
/// global admission ledger.
#[allow(clippy::too_many_arguments)]
fn execute_live_game_side_services<P: RuntimeProtection>(
    config: &LoadTestConfig,
    budget: &loadtest_core::config::HardBudget,
    ticket: &str,
    character_id: &str,
    auth_services: Option<&AuthServicesPayload>,
    descriptor_tracker: &mut DescriptorChangeTracker,
    action_deadline: Instant,
    deadline_unix_ms: u64,
    dispatch_admission: &mut AuthDispatchAdmission,
    abort: &mut AbortController,
    ctrl_c: &AtomicBool,
    protection: &P,
) -> Result<loadtest_core::metrics::MetricsSnapshot, LiveGameSideServiceError> {
    let configured_side = config
        .scenario
        .side_services
        .as_ref()
        .ok_or(LiveGameSideServiceError::Other)?;
    let mut metrics = Metrics::default();
    let side = resolve_live_side_services(
        configured_side,
        auth_services,
        descriptor_tracker,
        &mut metrics,
    )
    .map_err(|_| LiveGameSideServiceError::Other)?;
    let plan = side
        .executable_plan(budget)
        .map_err(|_| LiveGameSideServiceError::Other)?;

    if let Some(chat) = side.chat.as_ref().filter(|chat| chat.live_websocket) {
        let descriptor = chat
            .descriptor
            .as_ref()
            .ok_or(LiveGameSideServiceError::Other)?;
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
            .map_err(|_| LiveGameSideServiceError::Other)?;
        // The concrete chat runner applies every configured think_time before
        // sending that operation. Private/group sends conservatively reserve
        // one potential write before their frame can be dispatched.
        for writes in composite_chat_admission_writes(&chat_steps)
            .map_err(|_| LiveGameSideServiceError::Other)?
        {
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
                .map_err(|_| LiveGameSideServiceError::Other)?;
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|_| LiveGameSideServiceError::Other)?;
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
            .map_err(|_| LiveGameSideServiceError::Other)?;
        chat_metrics.merge_into_metrics(&mut metrics);
    }

    let http_steps = plan
        .steps
        .iter()
        .filter(|step| {
            matches!(
                step.service,
                SideServiceKind::Mail | SideServiceKind::Announce
            ) && live_composite_http_enabled(&side, step.service)
        })
        .cloned()
        .collect::<Vec<_>>();
    if !http_steps.is_empty() {
        let http_metrics = execute_live_mail_announce_steps(
            &side,
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
        .map_err(LiveGameSideServiceError::from_side_http)?;
        http_metrics.merge_into_metrics(&mut metrics);
    }

    Ok(metrics.snapshot())
}

/// The live KCP runner uses a current-thread Tokio runtime. Blocking reqwest
/// clients may own a Tokio runtime internally, whose destructor must not run
/// from that async context. Run a bounded side-service phase on a scoped OS
/// thread and synchronously join it before the KCP runner advances.
fn run_scoped_blocking_side_work<T, E>(work: impl FnOnce() -> Result<T, E> + Send) -> Result<T, E>
where
    T: Send,
    E: Send + From<()>,
{
    std::thread::scope(|scope| match scope.spawn(work).join() {
        Ok(result) => result,
        Err(_) => Err(E::from(())),
    })
}

fn collect_registry_observation_for_run(
    run_id: &str,
    started_unix_ms: u64,
    config: RegistryObservationConfig,
) -> Result<RegistryObservationReport, RegistryObservationError> {
    collect_runtime_registry_observation(&RegistryObservationRequest {
        run_id: run_id.to_string(),
        window_start_unix_ms: started_unix_ms,
        window_end_unix_ms: unix_ms().max(started_unix_ms.saturating_add(1)),
        config,
    })
}

enum RegistryPreflightDecision {
    Ready(RegistryObservationReport),
    Incomplete(RegistryObservationReport),
    Unavailable(RegistryObservationError),
}

fn classify_registry_preflight(
    result: Result<RegistryObservationReport, RegistryObservationError>,
) -> RegistryPreflightDecision {
    match result {
        Ok(report) if report.snapshot.complete => RegistryPreflightDecision::Ready(report),
        Ok(report) => RegistryPreflightDecision::Incomplete(report),
        Err(error) => RegistryPreflightDecision::Unavailable(error),
    }
}

fn write_registry_observation_failure(
    config: &LoadTestConfig,
    budget: &loadtest_core::config::HardBudget,
    run_id: &str,
    started_unix_ms: u64,
    deadline_unix_ms: u64,
    observation: Option<&RegistryObservationReport>,
    abort_reason: &str,
    error_code: &str,
    message: &str,
    context: BTreeMap<String, String>,
) -> Result<(), String> {
    let mut metrics = Metrics::default();
    if let Some(observation) = observation {
        observation.merge_into_metrics(&mut metrics);
    }
    let mut errors = ErrorBuffer::default();
    errors.push(error_code, message, context);
    let report = write_report(
        Path::new(&config.reports_root),
        ReportInput {
            run_id,
            config,
            effective_budget: budget,
            status: "failed",
            abort_reason: Some(abort_reason),
            shutdown_phase: None,
            deadline_unix_ms,
            graceful_shutdown_ms: config.graceful_shutdown_ms,
            started_unix_ms,
            ended_unix_ms: unix_ms(),
            metrics: metrics.snapshot(),
            resources: ResourceSampler.sample(0, 0, 0),
            errors: &errors,
            auth_metrics: None,
            calibration: None,
            service_versions: None,
            registry_observation: observation,
        },
    )
    .map_err(|error| error.to_string())?;
    Err(format!(
        "read-only registry observation failed; report={}",
        report.display()
    ))
}

fn record_registry_observation_result(
    result: Result<RegistryObservationReport, RegistryObservationError>,
    latest_observation: &mut Option<RegistryObservationReport>,
    metrics: &mut Metrics,
    errors: &mut ErrorBuffer,
    abort: &mut AbortController,
    failed: &mut bool,
) {
    match result {
        Ok(report) => {
            let complete = report.snapshot.complete;
            report.merge_into_metrics(metrics);
            *latest_observation = Some(report);
            if !complete {
                errors.push(
                    "registry_observation_incomplete",
                    "read-only registry observation has explicit coverage holes",
                    Default::default(),
                );
                *failed = true;
                abort.request(AbortReason::MetricsStale);
            }
        }
        Err(error) => {
            errors.push(
                error.report_category(),
                error.report_message(),
                error.report_context(),
            );
            *failed = true;
            abort.request(AbortReason::MetricsStale);
        }
    }
}

fn refresh_registry_observation_if_due(
    now_unix_ms: u64,
    next_recheck_unix_ms: &mut u64,
    run_id: &str,
    started_unix_ms: u64,
    config: &RegistryObservationConfig,
    latest_observation: &mut Option<RegistryObservationReport>,
    metrics: &mut Metrics,
    errors: &mut ErrorBuffer,
    abort: &mut AbortController,
    failed: &mut bool,
) {
    if now_unix_ms < *next_recheck_unix_ms {
        return;
    }
    *next_recheck_unix_ms = now_unix_ms.saturating_add(registry_recheck_interval_ms(config));
    record_registry_observation_result(
        collect_registry_observation_for_run(run_id, started_unix_ms, config.clone()),
        latest_observation,
        metrics,
        errors,
        abort,
        failed,
    );
}

fn live_run_terminal_status(abort: &AbortController, failed: bool) -> &'static str {
    if abort.should_stop_new_sessions() {
        "aborted"
    } else if failed {
        "failed"
    } else {
        "completed"
    }
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
    let reconnect_burst_mode = config.scenario.reconnect_burst.is_some();
    let two_player_default_match = !reconnect_burst_mode
        && game_mode
        && config
            .scenario
            .live_gameplay
            .as_ref()
            .is_some_and(|gameplay| {
                gameplay.coordination == LiveGameplayCoordination::TwoPlayerDefaultMatch
            });
    validate_production_authenticated_player_chain(
        config.environment.kind,
        game_mode,
        two_player_default_match,
        reconnect_burst_mode,
        live_chat,
        live_match,
        live_match_internal,
        live_http,
    )?;
    let remote_authenticated_player_chain = config.environment.kind.is_remote();
    if game_mode && !reconnect_burst_mode {
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
    if reconnect_burst_mode {
        validate_live_reconnect_burst_gate(cli, &config, &budget)?;
    }
    let auth_budget_estimate = estimate_auth_run_with_guard_probes(
        &config.scenario,
        &budget,
        remote_authenticated_player_chain,
    )?;
    validate_staged_auth_windows_with_guard_probes(
        &config.scenario,
        &budget,
        remote_authenticated_player_chain,
    )?;
    validate_auth_run_budget(&auth_budget_estimate, &budget)?;
    if game_mode && !reconnect_burst_mode {
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
    let live_game_side_service_composite = validate_live_game_side_service_composite(
        config.environment.kind,
        game_mode,
        two_player_default_match,
        live_chat,
        live_match,
        live_match_internal,
        live_http,
    )?;
    let online_default_match_mail_claim_phase = if live_game_side_service_composite {
        requires_online_default_match_mail_claim_phase(
            config
                .scenario
                .side_services
                .as_ref()
                .expect("live game-side composite has side services"),
        )?
    } else {
        false
    };
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
    let run_id = format!("auth-{}-{started}", std::process::id());
    let profile_deadline_unix_ms =
        effective_deadline(&config, &budget, cli.deadline_unix_ms, started)?;
    let deadline_unix_ms = profile_deadline_unix_ms.min(
        started.saturating_add(
            auth_budget_estimate
                .scenario_duration_secs
                .saturating_mul(1_000),
        ),
    );
    let preflight = summarize_run_with_guard_probes(
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
        remote_authenticated_player_chain,
    )?;
    println!(
        "preflight={}",
        serde_json::to_string(&preflight).expect("preflight summary serializes")
    );

    let registry_observation_config = config.scenario.registry_observation.clone();
    let mut registry_observation = match registry_observation_config.as_ref() {
        Some(registry_config) => match classify_registry_preflight(
            collect_registry_observation_for_run(&run_id, started, registry_config.clone()),
        ) {
            RegistryPreflightDecision::Ready(report) => Some(report),
            RegistryPreflightDecision::Incomplete(report) => {
                return write_registry_observation_failure(
                    &config,
                    &budget,
                    &run_id,
                    started,
                    deadline_unix_ms,
                    Some(&report),
                    "MetricsStale",
                    "registry_observation_incomplete",
                    "read-only registry observation has explicit coverage holes",
                    Default::default(),
                );
            }
            RegistryPreflightDecision::Unavailable(error) => {
                return write_registry_observation_failure(
                    &config,
                    &budget,
                    &run_id,
                    started,
                    deadline_unix_ms,
                    None,
                    "MetricsStale",
                    error.report_category(),
                    error.report_message(),
                    error.report_context(),
                );
            }
        },
        None => None,
    };
    let mut registry_next_recheck_unix_ms =
        registry_observation_config.as_ref().map(|registry_config| {
            unix_ms().saturating_add(registry_recheck_interval_ms(registry_config))
        });

    let account_ids = manifest
        .ready_accounts()
        .map(|entry| entry.logical_account_id.clone())
        .collect::<Vec<_>>();
    let requested_players = config
        .scenario
        .reconnect_burst
        .as_ref()
        .map_or(auth_budget_estimate.virtual_player_slots, |reconnect| {
            reconnect.virtual_players
        });
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
    let monotonic_now = Instant::now();
    let monotonic_deadline =
        monotonic_deadline_from_unix_ms(deadline_unix_ms, unix_ms(), monotonic_now)
            .map_err(|error| format!("run deadline rejected before transport setup: {error}"))?;
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
    let protection = if remote_authenticated_player_chain {
        RunPlayerProtection::Remote(LiveAuthProtection::new(&config, Duration::from_secs(5))?)
    } else {
        RunPlayerProtection::Local(DryRunProtection::new(&config))
    };
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
    if let Some(observation) = registry_observation.as_ref() {
        observation.merge_into_metrics(&mut core_metrics);
    }
    let mut descriptor_tracker = DescriptorChangeTracker::default();
    let mut auth_metrics = AuthRunMetrics::default();
    let mut dispatch_admission = AuthDispatchAdmission::new(&budget)?;
    let mut errors = ErrorBuffer::default();
    let mut abort = AbortController::default();
    let mut health_evaluator = ContinuousHealthEvaluator::new(2)
        .map_err(|error| format!("continuous health evaluator rejected: {error}"))?;
    let live_backpressure = Rc::new(RefCell::new(LiveBackpressureSignals::default()));
    let mut failed = false;

    if let Some(reconnect) = config.scenario.reconnect_burst.as_ref() {
        let plan = plan_reconnect_burst(
            ReconnectBurstSpec {
                virtual_players: reconnect.virtual_players,
                reconnect_attempts_per_player: reconnect.reconnect_attempts_per_player,
                start_ms: 0,
            },
            &budget,
            reconnect.reconnect_policy.into(),
        )
        .map_err(|error| format!("live reconnect burst plan rejected: {error}"))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|_| "could not create guarded reconnect KCP runtime")?;
        let mut adapter = LiveReconnectBurstAdapter::new(
            &mut transport,
            &config,
            &protection,
            RunAccess {
                allow_remote: cli.allow_remote,
                confirmation: cli.confirmation.as_deref(),
            },
            endpoint
                .as_ref()
                .expect("reconnect gate requires game execution endpoint"),
            &mut account_pool,
            &leases,
            &secret_provider,
            reconnect.reconnect_policy.into(),
            monotonic_deadline,
            runtime,
            Rc::clone(&live_backpressure),
        );
        let execution = execute_reconnect_burst(
            &plan,
            &budget,
            ReconnectBurstExecutionGate {
                execute_game: cli.execute_game,
                confirm_game: cli.confirm_game.as_deref(),
                environment_name: &config.environment.name,
                environment_kind: config.environment.kind,
            },
            &mut dispatch_admission,
            monotonic_deadline,
            &mut abort,
            |controller| {
                controller.check_ctrl_c(&ctrl_c);
                controller.check_stop_file(config.stop_file.as_deref().map(Path::new));
                controller.check_deadline(unix_ms(), deadline_unix_ms);
                if let (Some(registry_config), Some(next_recheck_unix_ms)) = (
                    registry_observation_config.as_ref(),
                    registry_next_recheck_unix_ms.as_mut(),
                ) {
                    refresh_registry_observation_if_due(
                        unix_ms(),
                        next_recheck_unix_ms,
                        &run_id,
                        started,
                        registry_config,
                        &mut registry_observation,
                        &mut core_metrics,
                        &mut errors,
                        controller,
                        &mut failed,
                    );
                }
                let protection_healthy = revalidate_or_abort(&protection, controller).is_none();
                observe_controller_health(
                    &mut health_evaluator,
                    controller,
                    protection_healthy,
                    0,
                    0,
                    0,
                    budget.max_virtual_players as u64,
                    Some(&live_backpressure.borrow()),
                );
                if controller.should_stop_new_sessions() {
                    return Err("reconnect burst checkpoint stopped".into());
                }
                Ok(())
            },
            &mut adapter,
        );
        let reconnect_finish = adapter.finish();
        auth_metrics.merge(&reconnect_finish.auth_metrics);
        core_metrics.increment(
            "reconnect_burst_room_handoff_temporary_rejections",
            reconnect_finish.handoff_retry_metrics.temporary_rejections,
        );
        core_metrics.increment(
            "reconnect_burst_room_handoff_retries_dispatched",
            reconnect_finish.handoff_retry_metrics.retries_dispatched,
        );
        core_metrics.increment(
            "reconnect_burst_room_handoff_retry_successes",
            reconnect_finish.handoff_retry_metrics.retry_successes,
        );
        core_metrics.increment(
            "reconnect_burst_room_handoff_retry_exhausted",
            reconnect_finish.handoff_retry_metrics.retry_exhausted,
        );
        match execution {
            Ok(execution) => {
                core_metrics.increment(
                    "reconnect_burst_forced_disconnects",
                    execution.forced_disconnects,
                );
                core_metrics.increment("reconnect_burst_login_actions", execution.login_actions);
                core_metrics.increment(
                    "reconnect_burst_new_connections",
                    execution.proxy_connections,
                );
                core_metrics
                    .increment("reconnect_burst_room_recoveries", execution.room_recoveries);
                core_metrics.increment(
                    "reconnect_burst_room_recovery_retry_slots",
                    execution.room_recovery_retry_slots,
                );
            }
            Err(error) => {
                record_reconnect_execution_failure(&mut errors, &error);
                failed = true;
                match error {
                    loadtest_core::reconnect_burst::ReconnectBurstExecutionError::Admission(
                        AuthAdmissionError::BudgetExceeded(_),
                    ) => abort.request(AbortReason::BudgetExceeded),
                    loadtest_core::reconnect_burst::ReconnectBurstExecutionError::Admission(
                        AuthAdmissionError::DeadlineExceeded,
                    ) => abort.request(AbortReason::Deadline),
                    _ => {}
                }
            }
        }
    }

    while !reconnect_burst_mode && !scheduler.exhausted() && !abort.should_stop_new_sessions() {
        abort.check_ctrl_c(&ctrl_c);
        abort.check_stop_file(config.stop_file.as_deref().map(Path::new));
        abort.check_deadline(unix_ms(), deadline_unix_ms);
        if abort.should_stop_new_sessions() {
            break;
        }
        let protection_healthy = revalidate_or_abort(&protection, &mut abort).is_none();
        observe_controller_health(
            &mut health_evaluator,
            &mut abort,
            protection_healthy,
            0,
            0,
            0,
            budget.max_virtual_players as u64,
            Some(&live_backpressure.borrow()),
        );
        if abort.should_stop_new_sessions() {
            break;
        }
        if let (Some(registry_config), Some(next_recheck_unix_ms)) = (
            registry_observation_config.as_ref(),
            registry_next_recheck_unix_ms.as_mut(),
        ) {
            refresh_registry_observation_if_due(
                unix_ms(),
                next_recheck_unix_ms,
                &run_id,
                started,
                registry_config,
                &mut registry_observation,
                &mut core_metrics,
                &mut errors,
                &mut abort,
                &mut failed,
            );
        }
        if abort.should_stop_new_sessions() {
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
        observe_controller_health(
            &mut health_evaluator,
            &mut abort,
            true,
            tick.dropped,
            tick.dropped,
            tick.queue_depth,
            budget.max_virtual_players as u64,
            Some(&live_backpressure.borrow()),
        );
        if abort.should_stop_new_sessions() {
            break;
        }

        for (index, action) in tick.actions.iter().enumerate() {
            if abort.should_stop_new_sessions() {
                break;
            }
            if let (Some(registry_config), Some(next_recheck_unix_ms)) = (
                registry_observation_config.as_ref(),
                registry_next_recheck_unix_ms.as_mut(),
            ) {
                refresh_registry_observation_if_due(
                    unix_ms(),
                    next_recheck_unix_ms,
                    &run_id,
                    started,
                    registry_config,
                    &mut registry_observation,
                    &mut core_metrics,
                    &mut errors,
                    &mut abort,
                    &mut failed,
                );
            }
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
                            admit_live_auth_request(
                                &mut dispatch_admission,
                                request,
                                action_deadline,
                                &protection,
                                &mut abort,
                                &ctrl_c,
                                config.stop_file.as_deref().map(Path::new),
                                deadline_unix_ms,
                                &mut auth_metrics,
                            )
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
                                let auth_services = execution.take_side_services();
                                if protection
                                    .observe_auth_services(auth_services.as_ref())
                                    .is_err()
                                {
                                    errors.push(
                                        "auth_game_descriptor_rejected",
                                        "auth public game descriptor was rejected before KCP setup",
                                        Default::default(),
                                    );
                                    abort.check_protection(false);
                                    failed = true;
                                    break;
                                }
                                // The same ticket remains local to its own
                                // player and is only reused after that
                                // player's KCP session has reached Closed.
                                side_credentials.push((
                                    ticket.clone(),
                                    character_id,
                                    auth_services,
                                ));
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
                        let mut online_mail_claim_error_reported = false;
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
                                        if checkpoint
                                            == GameRunnerCheckpoint::OnlinePairReadyStarted
                                        {
                                            if online_default_match_mail_claim_phase {
                                                let online_result: Result<
                                                    Vec<loadtest_core::metrics::MetricsSnapshot>,
                                                    LiveGameSideServiceError,
                                                > = run_scoped_blocking_side_work(|| {
                                                    check_live_composite_side_controller(
                                                        &config,
                                                        deadline_unix_ms,
                                                        &ctrl_c,
                                                        &protection,
                                                        &mut abort,
                                                    )
                                                    .map_err(|_| LiveGameSideServiceError::Other)?;
                                                    let mut snapshots =
                                                        Vec::with_capacity(side_credentials.len());
                                                    for (ticket, character_id, auth_services) in
                                                        &side_credentials
                                                    {
                                                        snapshots.push(
                                                            execute_live_game_side_services(
                                                                &config,
                                                                &budget,
                                                                ticket,
                                                                character_id,
                                                                auth_services.as_ref(),
                                                                &mut descriptor_tracker,
                                                                action_deadline,
                                                                deadline_unix_ms,
                                                                &mut dispatch_admission,
                                                                &mut abort,
                                                                &ctrl_c,
                                                                &protection,
                                                            )?,
                                                        );
                                                    }
                                                    Ok(snapshots)
                                                });
                                                match online_result {
                                                    Ok(snapshots) => {
                                                        for metrics in snapshots {
                                                            core_metrics.merge_snapshot(&metrics);
                                                        }
                                                    }
                                                    Err(error) => {
                                                        let (category, message, context) =
                                                            error.report_details();
                                                        errors.push(category, message, context);
                                                        online_mail_claim_error_reported = true;
                                                        return Err(
                                                            GameLiveError::GameplayFailed {
                                                                message: message.to_string(),
                                                                metrics: Default::default(),
                                                                failure_category: Some(category),
                                                            },
                                                        );
                                                    }
                                                }
                                            }
                                            return Ok(());
                                        }
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
                                            GameRunnerCheckpoint::OnlinePairReadyStarted => {
                                                unreachable!(
                                                    "online mail-claim phase returned above"
                                                )
                                            }
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
                                {
                                    let mut signals = live_backpressure.borrow_mut();
                                    for player in &result.players {
                                        signals.record_kcp(player.backpressure);
                                    }
                                }
                                for player in result.players {
                                    if let Some(metrics) = player.gameplay_metrics.as_ref() {
                                        core_metrics.merge_snapshot(metrics);
                                    }
                                    completed_game_sessions += 1;
                                }
                                let protection_healthy =
                                    revalidate_or_abort(&protection, &mut abort).is_none();
                                observe_controller_health(
                                    &mut health_evaluator,
                                    &mut abort,
                                    protection_healthy,
                                    0,
                                    0,
                                    0,
                                    budget.max_virtual_players as u64,
                                    Some(&live_backpressure.borrow()),
                                );
                                if abort.should_stop_new_sessions() {
                                    failed = true;
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
                                if !online_mail_claim_error_reported {
                                    errors.push(
                                        game_failure_category(&error),
                                        "two-player KCP game session did not complete",
                                        Default::default(),
                                    );
                                }
                                failed = true;
                            }
                        }
                    }
                    if !failed
                        && live_game_side_service_composite
                        && !online_default_match_mail_claim_phase
                    {
                        // Non-claim side-service diagnostics run after the
                        // KCP pair closes. Process each player in account
                        // order so chat/mail/announce never create a second
                        // concurrent load stream.
                        for (ticket, character_id, auth_services) in side_credentials {
                            match execute_live_game_side_services(
                                &config,
                                &budget,
                                &ticket,
                                &character_id,
                                auth_services.as_ref(),
                                &mut descriptor_tracker,
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
                            admit_live_auth_request(
                                &mut dispatch_admission,
                                request,
                                action_deadline,
                                &protection,
                                &mut abort,
                                &ctrl_c,
                                config.stop_file.as_deref().map(Path::new),
                                deadline_unix_ms,
                                &mut auth_metrics,
                            )
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
            let auth_execution_failed = execution.error.is_some();
            let mut execution_failed = auth_execution_failed;
            let pre_game_auth_completed = !auth_execution_failed;
            let mut side_credentials = if !execution_failed
                && !game_mode
                && (live_chat || live_match || live_match_internal || live_http)
            {
                execution.take_game_credentials()
            } else {
                None
            };
            let auth_side_services = execution.take_side_services();
            let resolved_side_services = if !execution_failed
                && !game_mode
                && (live_chat || live_match || live_match_internal || live_http)
            {
                if protection
                    .observe_auth_services(auth_side_services.as_ref())
                    .is_err()
                {
                    errors.push(
                        "auth_service_descriptor_rejected",
                        "auth public service descriptors were rejected before side-service setup",
                        Default::default(),
                    );
                    failed = true;
                    execution_failed = true;
                    None
                } else {
                    match config
                        .scenario
                        .side_services
                        .as_ref()
                        .ok_or("live side-service configuration disappeared after validation")
                    {
                        Ok(side) => match resolve_live_side_services(
                            side,
                            auth_side_services.as_ref(),
                            &mut descriptor_tracker,
                            &mut core_metrics,
                        ) {
                            Ok(side) => Some(side),
                            Err(_) => {
                                errors.push(
                                    "auth_service_descriptor_rejected",
                                    "auth-discovered side-service descriptor was rejected",
                                    Default::default(),
                                );
                                failed = true;
                                execution_failed = true;
                                None
                            }
                        },
                        Err(_) => {
                            errors.push(
                                "side_service_configuration_missing",
                                "live side-service configuration disappeared after validation",
                                Default::default(),
                            );
                            failed = true;
                            execution_failed = true;
                            None
                        }
                    }
                }
            } else {
                None
            };
            if auth_execution_failed {
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
                                            GameRunnerCheckpoint::OnlinePairReadyStarted => {
                                                return Err(GameLiveError::Transport(
                                                    "two-player online checkpoint reached the single-player runner",
                                                ));
                                            }
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
                                live_backpressure
                                    .borrow_mut()
                                    .record_kcp(result.backpressure);
                                if let Some(gameplay_metrics) = result.gameplay_metrics.as_ref() {
                                    core_metrics.merge_snapshot(gameplay_metrics);
                                }
                                completed_game_session = true;
                                let protection_healthy =
                                    revalidate_or_abort(&protection, &mut abort).is_none();
                                observe_controller_health(
                                    &mut health_evaluator,
                                    &mut abort,
                                    protection_healthy,
                                    0,
                                    0,
                                    0,
                                    budget.max_virtual_players as u64,
                                    Some(&live_backpressure.borrow()),
                                );
                                if abort.should_stop_new_sessions() {
                                    execution_failed = true;
                                }
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
                let side = resolved_side_services
                    .as_ref()
                    .ok_or("resolved live side-service configuration disappeared")?;
                let chat = side
                    .chat
                    .as_ref()
                    .ok_or("live chat configuration disappeared after validation")?;
                let descriptor = chat
                    .descriptor
                    .as_ref()
                    .ok_or("live chat requires an explicit descriptor")?;
                let plan = side
                    .executable_plan(&budget)
                    .map_err(|error| format!("live chat plan rejected: {error}"))?;
                let Some((ticket, character_id)) = side_credentials.clone() else {
                    errors.push(
                        "chat_ticket_missing",
                        "auth completed without a transferable chat ticket",
                        Default::default(),
                    );
                    failed = true;
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
                let side = resolved_side_services
                    .as_ref()
                    .ok_or("resolved live side-service configuration disappeared")?;
                let matcher = side
                    .r#match
                    .as_ref()
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
                    finish_live_action(&mut auth_metrics, &execution.metrics, &mut abort, &budget);
                    continue;
                };
                let plan = side
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
                    Ok(match_metrics) => {
                        live_backpressure
                            .borrow_mut()
                            .record_match_grpc(match_metrics.backpressure);
                        match_metrics.merge_into_metrics(&mut core_metrics);
                        let protection_healthy =
                            revalidate_or_abort(&protection, &mut abort).is_none();
                        observe_controller_health(
                            &mut health_evaluator,
                            &mut abort,
                            protection_healthy,
                            0,
                            0,
                            0,
                            budget.max_virtual_players as u64,
                            Some(&live_backpressure.borrow()),
                        );
                        if abort.should_stop_new_sessions() {
                            execution_failed = true;
                        }
                    }
                    Err(error) => {
                        if let Some(backpressure) = error.backpressure_metrics() {
                            live_backpressure
                                .borrow_mut()
                                .record_match_grpc(backpressure);
                            let protection_healthy =
                                revalidate_or_abort(&protection, &mut abort).is_none();
                            observe_controller_health(
                                &mut health_evaluator,
                                &mut abort,
                                protection_healthy,
                                0,
                                0,
                                0,
                                budget.max_virtual_players as u64,
                                Some(&live_backpressure.borrow()),
                            );
                        }
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
                let side = resolved_side_services
                    .as_ref()
                    .ok_or("resolved live side-service configuration disappeared")?;
                let matcher = side
                    .r#match
                    .as_ref()
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
                let side = resolved_side_services
                    .as_ref()
                    .ok_or("resolved live HTTP side-service configuration disappeared")?;
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
    if let Some(registry_config) = registry_observation_config.as_ref() {
        record_registry_observation_result(
            collect_registry_observation_for_run(&run_id, started, registry_config.clone()),
            &mut registry_observation,
            &mut core_metrics,
            &mut errors,
            &mut abort,
            &mut failed,
        );
    }
    core_metrics.increment(
        "auth_potential_data_writes",
        dispatch_admission.used_data_writes(),
    );
    let status = match live_run_terminal_status(&abort, failed) {
        "aborted" => {
            lifecycle.transition(RunState::Aborting).unwrap();
            lifecycle.transition(RunState::Aborted).unwrap();
            "aborted"
        }
        "failed" => {
            lifecycle.transition(RunState::Failed).unwrap();
            "failed"
        }
        "completed" => {
            lifecycle.transition(RunState::CoolingDown).unwrap();
            lifecycle.transition(RunState::Completed).unwrap();
            "completed"
        }
        _ => unreachable!("live run status is closed"),
    };
    let abort_reason = abort.reason().map(|reason| format!("{reason:?}"));
    let report = write_report(
        Path::new(&config.reports_root),
        ReportInput {
            run_id: &run_id,
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
            service_versions: None,
            registry_observation: registry_observation.as_ref(),
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

/// Real action adapter used only by the guarded reconnect-burst CLI path.
/// Opaque credentials remain in this short-lived object and never enter the
/// plan, metric labels, error buffer, or report.
struct LiveReconnectBurstAdapter<'a> {
    auth_transport: &'a mut ReqwestAuthHttpTransport,
    config: &'a LoadTestConfig,
    protection: &'a RunPlayerProtection<'a>,
    access: RunAccess<'a>,
    endpoint: &'a GameProxyEndpoint,
    account_pool: &'a mut AccountLeasePool,
    leases: &'a [AccountLease],
    secret_provider: &'a EnvironmentSecretProvider<'a>,
    reconnect_policy: ReconnectPolicy,
    deadline: Instant,
    started: Instant,
    runtime: tokio::runtime::Runtime,
    players: Vec<LiveReconnectBurstPlayer>,
    auth_metrics: AuthRunMetrics,
    handoff_retry_metrics: RoomHandoffRetryMetrics,
    live_backpressure: Rc<RefCell<LiveBackpressureSignals>>,
}

struct LiveReconnectBurstPlayer {
    access_token: Option<String>,
    character_id: Option<String>,
    ticket: Option<String>,
    transport: Option<LiveKcpTransport>,
    session: Option<VirtualPlayerSession>,
    pending_room_handoff_retry: Option<PendingRoomHandoffRetry>,
}

#[derive(Debug, Clone, Copy)]
struct PendingRoomHandoffRetry {
    reconnect_attempt: u32,
    observed_at: Instant,
}

#[derive(Debug, Clone, Copy, Default)]
struct RoomHandoffRetryMetrics {
    temporary_rejections: u64,
    retries_dispatched: u64,
    retry_successes: u64,
    retry_exhausted: u64,
}

struct LiveReconnectBurstFinish {
    auth_metrics: AuthRunMetrics,
    handoff_retry_metrics: RoomHandoffRetryMetrics,
}

impl<'a> LiveReconnectBurstAdapter<'a> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        auth_transport: &'a mut ReqwestAuthHttpTransport,
        config: &'a LoadTestConfig,
        protection: &'a RunPlayerProtection<'a>,
        access: RunAccess<'a>,
        endpoint: &'a GameProxyEndpoint,
        account_pool: &'a mut AccountLeasePool,
        leases: &'a [AccountLease],
        secret_provider: &'a EnvironmentSecretProvider<'a>,
        reconnect_policy: ReconnectPolicy,
        deadline: Instant,
        runtime: tokio::runtime::Runtime,
        live_backpressure: Rc<RefCell<LiveBackpressureSignals>>,
    ) -> Self {
        Self {
            auth_transport,
            config,
            protection,
            access,
            endpoint,
            account_pool,
            leases,
            secret_provider,
            reconnect_policy,
            deadline,
            started: Instant::now(),
            runtime,
            players: (0..leases.len())
                .map(|_| LiveReconnectBurstPlayer {
                    access_token: None,
                    character_id: None,
                    ticket: None,
                    transport: None,
                    session: None,
                    pending_room_handoff_retry: None,
                })
                .collect(),
            auth_metrics: AuthRunMetrics::default(),
            handoff_retry_metrics: RoomHandoffRetryMetrics::default(),
            live_backpressure,
        }
    }

    fn finish(mut self) -> LiveReconnectBurstFinish {
        for player in &mut self.players {
            if let Some(transport) = player.transport.as_mut() {
                transport.close();
            }
            if let Some(session) = player.session.as_mut() {
                session.close(self.account_pool);
            }
        }
        LiveReconnectBurstFinish {
            auth_metrics: self.auth_metrics,
            handoff_retry_metrics: self.handoff_retry_metrics,
        }
    }

    fn player_index(&self, player_slot: u32) -> Result<usize, String> {
        let player_index = usize::try_from(player_slot)
            .map_err(|_| "reconnect burst player slot is out of range")?;
        (player_index < self.players.len())
            .then_some(player_index)
            .ok_or_else(|| "reconnect burst player slot is not leased".into())
    }

    fn record_player_backpressure(&self, player_index: usize) {
        if let Some(session) = self
            .players
            .get(player_index)
            .and_then(|player| player.session.as_ref())
        {
            self.live_backpressure
                .borrow_mut()
                .record_kcp(session.backpressure_metrics());
        }
    }

    fn wait_for_action(
        &self,
        action: ReconnectBurstAction,
        admission: &mut ReconnectBurstAdmission<'_>,
    ) -> Result<(), String> {
        self.wait_until(
            self.started + Duration::from_millis(action.at_ms),
            admission,
        )
    }

    /// Waits only for a pre-planned action boundary. Every short wait returns
    /// to the shared controller so stop, deadline, and target checks remain
    /// authoritative while a server-side session handoff settles.
    fn wait_until(
        &self,
        scheduled: Instant,
        admission: &mut ReconnectBurstAdmission<'_>,
    ) -> Result<(), String> {
        wait_for_reconnect_action(
            scheduled,
            self.deadline,
            || {
                admission
                    .revalidate()
                    .map_err(|error| reconnect_execution_failure_category(&error))?;
                if admission.should_stop() {
                    return Err(ReconnectFailureCategory::Stopped);
                }
                Ok(())
            },
            std::thread::sleep,
        )
        .map_err(ReconnectFailureCategory::executor_error)
    }

    fn send_auth(
        &mut self,
        request: AuthHttpRequest,
        admission: &mut ReconnectBurstAdmission<'_>,
    ) -> Result<loadtest_core::auth_http::AuthSuccess, String> {
        send_reconnect_auth_with_guard(
            self.auth_transport,
            request,
            admission,
            self.protection,
            &mut self.auth_metrics,
        )
    }

    fn login(
        &mut self,
        player_index: usize,
        admission: &mut ReconnectBurstAdmission<'_>,
    ) -> Result<(), String> {
        let lease = self
            .leases
            .get(player_index)
            .ok_or("reconnect burst player has no account lease")?;
        let login_name = auth_login_name(&lease.logical_account_id)?;
        let password = self
            .secret_provider
            .password_for(&lease.logical_account_id)?;
        let success = self.send_auth(
            AuthHttpRequest::Login {
                login_name,
                password,
            },
            admission,
        )?;
        if self
            .protection
            .observe_auth_services(success.services.as_ref())
            .is_err()
        {
            admission.mark_protection_failed();
            return Err(
                "auth public game descriptor was rejected before reconnect dispatch".into(),
            );
        }
        let access_token = success
            .access_token
            .ok_or("reconnect burst login did not return an access token")?;

        // The primary Login action admission was consumed by the executor.
        // Character discovery is a distinct public HTTP request and therefore
        // reserves its own shared hard-budget slot before dispatch.
        admission
            .admit_auth_operation(AuthOperation::ListCharacters)
            .map_err(|error| error.to_string())?;
        let listed = self.send_auth(
            AuthHttpRequest::ListCharacters {
                access_token: access_token.clone(),
            },
            admission,
        )?;
        let character_id = listed
            .character_id
            .ok_or("reconnect burst account has no prepared character")?;
        let player = self
            .players
            .get_mut(player_index)
            .ok_or("reconnect burst player state is unavailable")?;
        player.access_token = Some(access_token);
        player.character_id = Some(character_id);
        Ok(())
    }

    fn issue_ticket(
        &mut self,
        player_index: usize,
        admission: &mut ReconnectBurstAdmission<'_>,
    ) -> Result<(), String> {
        let player = self
            .players
            .get(player_index)
            .ok_or("reconnect burst player state is unavailable")?;
        let access_token = player
            .access_token
            .clone()
            .ok_or("reconnect burst ticket action requires a successful login")?;
        let character_id = player
            .character_id
            .clone()
            .ok_or("reconnect burst ticket action requires a prepared character")?;
        let success = self.send_auth(
            AuthHttpRequest::IssueTicket {
                access_token,
                character_id,
            },
            admission,
        )?;
        let ticket = success
            .ticket
            .ok_or("reconnect burst ticket action did not return a ticket")?;
        self.players[player_index].ticket = Some(ticket);
        Ok(())
    }

    fn connect_proxy(&mut self, player_index: usize) -> Result<(), String> {
        if self.players[player_index].transport.is_some() {
            return Err("reconnect burst KCP transport is already connected".into());
        }
        let mut transport = LiveKcpTransport::new(self.deadline, 1024 * 1024)
            .map_err(|_| "reconnect burst KCP transport setup failed")?;
        self.runtime
            .block_on(transport.connect(self.config, self.access, self.endpoint))
            .map_err(|_| "reconnect burst KCP connect failed")?;
        self.players[player_index].transport = Some(transport);
        Ok(())
    }

    fn authenticate_proxy(&mut self, player_index: usize) -> Result<(), String> {
        let lease = self
            .leases
            .get(player_index)
            .cloned()
            .ok_or("reconnect burst player has no account lease")?;
        let ticket = self.players[player_index]
            .ticket
            .as_deref()
            .ok_or("reconnect burst KCP auth requires an issued ticket")?
            .to_string();
        let mut session = self.players[player_index]
            .session
            .take()
            .unwrap_or_else(|| {
                VirtualPlayerSession::new(lease, 1024 * 1024, self.reconnect_policy)
                    .expect("reconnect policy was validated before adapter creation")
            });
        if session.state()
            == loadtest_core::virtual_player::VirtualPlayerSessionState::AccountLeased
        {
            session
                .mark_logged_in(self.account_pool)
                .map_err(|_| "reconnect burst virtual-player login transition failed")?;
            session
                .mark_character_selected(self.account_pool)
                .map_err(|_| "reconnect burst virtual-player character transition failed")?;
            session
                .mark_ticket_issued(self.account_pool)
                .map_err(|_| "reconnect burst virtual-player ticket transition failed")?;
        }
        let auth = session
            .connect_and_begin_auth(self.account_pool, LiveKcpConnection, &ticket)
            .map_err(|_| "reconnect burst KCP auth lifecycle setup failed")?;
        let transport = self.players[player_index]
            .transport
            .as_mut()
            .ok_or("reconnect burst KCP auth requires a connected transport")?;
        self.runtime
            .block_on(transport.send(&auth))
            .map_err(|_| "reconnect burst KCP auth write failed")?;
        let response = self
            .runtime
            .block_on(transport.receive())
            .map_err(|_| "reconnect burst KCP auth response failed")?;
        match session
            .handle_packet(self.account_pool, response)
            .map_err(|_| "reconnect burst KCP auth lifecycle response failed")?
        {
            VirtualPlayerEvent::GameAuthenticated => {
                session
                    .activate(self.account_pool)
                    .map_err(|_| "reconnect burst KCP activation failed")?;
            }
            _ => return Err("reconnect burst KCP authentication was rejected".into()),
        }
        self.players[player_index].session = Some(session);
        Ok(())
    }

    fn recover_room(
        &mut self,
        player_index: usize,
        reconnect_attempt: u32,
    ) -> Result<RoomRecoveryResponse, String> {
        let gameplay = self
            .config
            .scenario
            .live_gameplay
            .as_ref()
            .expect("reconnect live gate requires an approved single-player room");
        let (request_type, expected_response, body) = if reconnect_attempt == 0 {
            (
                game_protocol::MessageType::RoomJoinReq,
                game_protocol::MessageType::RoomJoinRes,
                game_protocol::encode_body(&loadtest_core::pb::RoomJoinReq {
                    room_id: gameplay.room_id.clone(),
                    policy_id: gameplay.policy_id.clone(),
                }),
            )
        } else {
            let cursor = gameplay
                .reconnect
                .expect("reconnect live gate requires an explicit reconnect cursor")
                .last_character_push_sequence;
            (
                game_protocol::MessageType::RoomReconnectReq,
                game_protocol::MessageType::RoomReconnectRes,
                game_protocol::encode_body(&loadtest_core::pb::RoomReconnectReq {
                    last_character_push_sequence: cursor,
                }),
            )
        };
        let player = self
            .players
            .get_mut(player_index)
            .ok_or("reconnect burst player state is unavailable")?;
        let (session, transport) = match (&mut player.session, &mut player.transport) {
            (Some(session), Some(transport)) => (session, transport),
            (None, _) => {
                return Err(
                    "reconnect burst room recovery requires an authenticated player".into(),
                );
            }
            (_, None) => {
                return Err("reconnect burst room recovery requires a connected transport".into());
            }
        };
        let request = session
            .begin_gameplay_request(self.account_pool, request_type, expected_response, &body)
            .map_err(|_| "reconnect burst room recovery lifecycle setup failed")?;
        self.runtime
            .block_on(transport.send(&request))
            .map_err(|_| "reconnect burst room recovery write failed")?;
        let response = receive_room_recovery_response(
            || {
                self.runtime
                    .block_on(transport.receive())
                    .map_err(reconnect_room_receive_failure_category)
            },
            |packet| match session.handle_packet(self.account_pool, packet) {
                Ok(VirtualPlayerEvent::Push { message_type, .. })
                    if is_room_recovery_async_push(message_type) =>
                {
                    Ok(())
                }
                _ => Err(ReconnectFailureCategory::RoomUnexpectedPacket),
            },
            expected_response,
        )
        .map_err(ReconnectFailureCategory::executor_error)?;
        let recovery =
            classify_room_recovery_response(&response, reconnect_attempt, &gameplay.room_id)
                .map_err(ReconnectFailureCategory::executor_error)?;
        match session
            .handle_packet(self.account_pool, response)
            .map_err(|_| ReconnectFailureCategory::RoomUnexpectedPacket.executor_error())?
        {
            VirtualPlayerEvent::Response { message_type, .. }
                if message_type == expected_response =>
            {
                Ok(recovery)
            }
            _ => Err(ReconnectFailureCategory::RoomUnexpectedPacket.executor_error()),
        }
    }

    fn recover_room_step(
        &mut self,
        player_index: usize,
        reconnect_attempt: u32,
    ) -> Result<(), String> {
        match self.recover_room(player_index, reconnect_attempt)? {
            RoomRecoveryResponse::Recovered => Ok(()),
            RoomRecoveryResponse::TemporaryHandoffRejected => {
                let player = self
                    .players
                    .get_mut(player_index)
                    .ok_or("reconnect burst player state is unavailable")?;
                player.pending_room_handoff_retry = Some(PendingRoomHandoffRetry {
                    reconnect_attempt,
                    observed_at: Instant::now(),
                });
                self.handoff_retry_metrics.temporary_rejections = self
                    .handoff_retry_metrics
                    .temporary_rejections
                    .saturating_add(1);
                Ok(())
            }
        }
    }

    fn retry_room_recovery(
        &mut self,
        player_index: usize,
        reconnect_attempt: u32,
        admission: &mut ReconnectBurstAdmission<'_>,
    ) -> Result<(), String> {
        let pending = self
            .players
            .get(player_index)
            .ok_or("reconnect burst player state is unavailable")?
            .pending_room_handoff_retry;
        let Some(pending) = pending else {
            return Ok(());
        };
        if pending.reconnect_attempt != reconnect_attempt {
            return Err(ReconnectFailureCategory::RoomUnexpectedPacket.executor_error());
        }

        self.wait_until(
            pending.observed_at + Duration::from_millis(ROOM_HANDOFF_RETRY_BACKOFF_MS),
            admission,
        )?;
        self.handoff_retry_metrics.retries_dispatched = self
            .handoff_retry_metrics
            .retries_dispatched
            .saturating_add(1);
        match self.recover_room(player_index, reconnect_attempt)? {
            RoomRecoveryResponse::Recovered => {
                self.players[player_index].pending_room_handoff_retry = None;
                self.handoff_retry_metrics.retry_successes =
                    self.handoff_retry_metrics.retry_successes.saturating_add(1);
                Ok(())
            }
            RoomRecoveryResponse::TemporaryHandoffRejected => {
                self.players[player_index].pending_room_handoff_retry = None;
                self.handoff_retry_metrics.retry_exhausted =
                    self.handoff_retry_metrics.retry_exhausted.saturating_add(1);
                Err(ReconnectFailureCategory::RoomHandoffRetryExhausted.executor_error())
            }
        }
    }

    fn force_disconnect(&mut self, player_index: usize, attempt: u32) -> Result<(), String> {
        let transport = self.players[player_index]
            .transport
            .as_mut()
            .ok_or("reconnect burst forced disconnect requires an active transport")?;
        transport.close();
        self.players[player_index].transport = None;
        let session = self.players[player_index]
            .session
            .as_mut()
            .ok_or("reconnect burst forced disconnect requires an active player")?;
        match session
            .handle_disconnect(self.account_pool, 0)
            .map_err(|_| "reconnect burst forced disconnect lifecycle failed")?
        {
            VirtualPlayerEvent::ReconnectScheduled {
                attempt: scheduled_attempt,
                ..
            } if scheduled_attempt == attempt => Ok(()),
            _ => Err("reconnect burst forced disconnect did not schedule the planned retry".into()),
        }
    }
}

impl ReconnectBurstExecutor for LiveReconnectBurstAdapter<'_> {
    fn execute(
        &mut self,
        action: ReconnectBurstAction,
        admission: &mut ReconnectBurstAdmission<'_>,
    ) -> Result<(), String> {
        self.wait_for_action(action, admission)?;
        let player_index = self.player_index(action.player_slot)?;
        let result = match action.step {
            ReconnectBurstStep::DisconnectExisting => {
                self.force_disconnect(player_index, action.reconnect_attempt)
            }
            ReconnectBurstStep::Login => self.login(player_index, admission),
            ReconnectBurstStep::IssueTicket => self.issue_ticket(player_index, admission),
            ReconnectBurstStep::ConnectProxy => self.connect_proxy(player_index),
            ReconnectBurstStep::AuthenticateProxy => self.authenticate_proxy(player_index),
            ReconnectBurstStep::RecoverRoom => {
                self.recover_room_step(player_index, action.reconnect_attempt)
            }
            ReconnectBurstStep::RetryRecoverRoom => {
                self.retry_room_recovery(player_index, action.reconnect_attempt, admission)
            }
        };
        self.record_player_backpressure(player_index);
        result
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

/// Live reconnect execution is a separate KCP/auth adapter boundary. Validate
/// its plan and profile gate before constructing any HTTP or KCP client; the
/// transport adapter receives only this already-bounded plan.
fn validate_live_reconnect_burst_gate(
    cli: &Cli,
    config: &LoadTestConfig,
    budget: &loadtest_core::config::HardBudget,
) -> Result<(), String> {
    let reconnect = config
        .scenario
        .reconnect_burst
        .as_ref()
        .ok_or("live reconnect burst requires scenario.reconnect_burst")?;
    ReconnectBurstExecutionGate {
        execute_game: cli.execute_game,
        confirm_game: cli.confirm_game.as_deref(),
        environment_name: &config.environment.name,
        environment_kind: config.environment.kind,
    }
    .validate()
    .map_err(|error| error.to_string())?;
    let gameplay = config.scenario.live_gameplay.as_ref().ok_or(
        "live reconnect burst requires scenario.live_gameplay with an approved room boundary",
    )?;
    if gameplay.coordination != LiveGameplayCoordination::SinglePlayer {
        return Err(
            "live reconnect burst currently requires single_player room coordination".into(),
        );
    }
    if gameplay.reconnect.is_none() {
        return Err(
            "live reconnect burst requires an explicit live_gameplay reconnect cursor and policy"
                .into(),
        );
    }
    let plan = plan_reconnect_burst(
        ReconnectBurstSpec {
            virtual_players: reconnect.virtual_players,
            reconnect_attempts_per_player: reconnect.reconnect_attempts_per_player,
            start_ms: 0,
        },
        budget,
        reconnect.reconnect_policy.into(),
    )
    .map_err(|error| error.to_string())?;
    let live_estimate =
        estimate_live_reconnect_burst(&plan, budget, config.environment.kind.is_remote())?;
    validate_live_reconnect_burst_budget(&live_estimate, budget)
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
    "usage: loadtest validate|calibrate --config <file> [--private-config <file>] [--allow-remote --confirm <environment>] [--dry-run]\n       loadtest observe-registry --config <file> [--allow-remote --confirm <environment>]\n       loadtest run --config <file> --dry-run\n       loadtest run --config <file> --execute-auth --confirm-auth <environment> --account-manifest <file> --private-config <file> [--allow-remote --confirm <environment>]\n       loadtest run --config <file> --execute-auth --confirm-auth <environment> --execute-game --confirm-game <environment> --account-manifest <file> --private-config <file> [--allow-remote --confirm <environment>]\n       loadtest report --report-dir <reports/run-id>".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    use loadtest_core::abort::AbortReason;
    use loadtest_core::auth_http::{AuthHttpStatusCategory, AuthOutcomeCategory};
    use loadtest_core::control_plane::ObservationSnapshot;

    fn registry_observer_config() -> LoadTestConfig {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "environment": { "name": "local", "kind": "local" },
            "targets": {
                "auth_http": "http://127.0.0.1:3000",
                "game_proxy": "kcp://127.0.0.1:4000"
            },
            "budget": {
                "max_virtual_players": 1,
                "max_login_qps": 1.0,
                "max_new_connections_per_second": 1.0,
                "max_business_messages_per_second": 1.0,
                "max_messages_per_connection_per_second": 1.0,
                "max_duration_secs": 1,
                "max_total_operations": 1,
                "max_error_rate": 0.1,
                "max_connection_failure_rate": 0.1,
                "max_p99_ms": 100,
                "max_data_writes": 0
            },
            "scenario": {
                "name": "registry-observer",
                "load": {
                    "type": "fixed_concurrency",
                    "virtual_players": 1,
                    "duration_secs": 1
                },
                "writes_data": false,
                "registry_observation": {
                    "read_only": true,
                    "max_heartbeat_age_ms": 5000,
                    "max_discovery_latency_ms": 500,
                    "max_stale_cleanup_latency_ms": 5000,
                    "max_metric_age_ms": 5000
                }
            },
            "reports_root": "reports",
            "prepare_reports_root": "prepare",
            "account_prepare": {
                "batch": "registry-smoke",
                "account_count": 1
            }
        }))
        .unwrap()
    }

    fn registry_report(complete: bool) -> RegistryObservationReport {
        RegistryObservationReport {
            snapshot: ObservationSnapshot {
                run_id: "registry-run".into(),
                window_start_unix_ms: 1,
                window_end_unix_ms: 2,
                source: "registry_readonly_v1".into(),
                freshness_ms: 0,
                complete,
            },
            registry_instance_count: 0,
            instance_metric_count: 0,
            instances: Vec::new(),
            stale_cleanups: Vec::new(),
            routes: Vec::new(),
            holes: Default::default(),
        }
    }

    #[test]
    fn registry_preflight_and_runtime_fail_closed_before_completed_status() {
        assert!(matches!(
            classify_registry_preflight(Ok(registry_report(true))),
            RegistryPreflightDecision::Ready(_)
        ));
        assert!(matches!(
            classify_registry_preflight(Ok(registry_report(false))),
            RegistryPreflightDecision::Incomplete(_)
        ));
        assert!(matches!(
            classify_registry_preflight(Err(RegistryObservationError::TransportUnavailable)),
            RegistryPreflightDecision::Unavailable(RegistryObservationError::TransportUnavailable)
        ));

        let mut latest = None;
        let mut metrics = Metrics::default();
        let mut errors = ErrorBuffer::default();
        let mut abort = AbortController::default();
        let mut failed = false;
        record_registry_observation_result(
            Ok(registry_report(false)),
            &mut latest,
            &mut metrics,
            &mut errors,
            &mut abort,
            &mut failed,
        );
        assert!(latest.is_some());
        assert!(failed);
        assert_eq!(abort.reason(), Some(&AbortReason::MetricsStale));
        assert_eq!(live_run_terminal_status(&abort, failed), "aborted");
        assert_eq!(
            errors.samples()[0].category,
            "registry_observation_incomplete"
        );

        let mut latest = None;
        let mut metrics = Metrics::default();
        let mut errors = ErrorBuffer::default();
        let mut abort = AbortController::default();
        let mut failed = false;
        record_registry_observation_result(
            Err(RegistryObservationError::TransportUnavailable),
            &mut latest,
            &mut metrics,
            &mut errors,
            &mut abort,
            &mut failed,
        );
        assert!(latest.is_none());
        assert!(failed);
        assert_eq!(live_run_terminal_status(&abort, failed), "aborted");
        assert_eq!(
            errors.samples()[0].category,
            "registry_observation_transport_unavailable"
        );
        assert!(errors.samples()[0].context.is_empty());

        let mut latest = None;
        let mut metrics = Metrics::default();
        let mut errors = ErrorBuffer::default();
        let mut abort = AbortController::default();
        let mut failed = false;
        record_registry_observation_result(
            Err(RegistryObservationError::RedisConnectionFailed {
                class: loadtest_core::registry_observation::RegistryRedisConnectionErrorClass::Io,
            }),
            &mut latest,
            &mut metrics,
            &mut errors,
            &mut abort,
            &mut failed,
        );
        assert!(latest.is_none());
        assert!(failed);
        assert_eq!(
            errors.samples()[0].category,
            "registry_redis_connection_failed"
        );
        assert_eq!(
            errors.samples()[0].context,
            BTreeMap::from([("redis_connection_error_class".into(), "io".into())])
        );
        assert!(!errors.samples()[0].context.contains_key("redis_command"));

        let mut latest = None;
        let mut metrics = Metrics::default();
        let mut errors = ErrorBuffer::default();
        let mut abort = AbortController::default();
        let mut failed = false;
        record_registry_observation_result(
            Err(RegistryObservationError::RedisCommandRejected {
                command: loadtest_core::registry_observation::RegistryRedisCommand::Zrange,
            }),
            &mut latest,
            &mut metrics,
            &mut errors,
            &mut abort,
            &mut failed,
        );
        assert!(latest.is_none());
        assert!(failed);
        assert_eq!(
            errors.samples()[0].category,
            "registry_redis_command_rejected"
        );
        assert_eq!(
            errors.samples()[0].context,
            BTreeMap::from([
                ("redis_command".into(), "zrange".into()),
                ("redis_error_class".into(), "permission_denied".into()),
            ])
        );
    }

    #[test]
    fn registry_observation_failure_report_keeps_redis_connection_details_redacted() {
        let root = std::env::temp_dir().join(format!(
            "loadtest-registry-error-report-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = registry_observer_config();
        config.reports_root = root.to_string_lossy().into_owned();
        let budget = config.effective_budget(&BudgetOverride::default()).unwrap();
        let error = RegistryObservationError::RedisConnectionFailed {
            class:
                loadtest_core::registry_observation::RegistryRedisConnectionErrorClass::AuthenticationFailed,
        };

        assert!(
            write_registry_observation_failure(
                &config,
                &budget,
                "registry-error-redaction",
                1,
                2,
                None,
                "MetricsStale",
                error.report_category(),
                error.report_message(),
                error.report_context(),
            )
            .is_err()
        );

        let mut entries = std::fs::read_dir(&root).unwrap();
        let report_dir = entries.next().unwrap().unwrap().path();
        assert!(entries.next().is_none());
        let artifacts = [
            "run.json",
            "errors.jsonl",
            "metrics.json",
            "summary.md",
            "timeseries.csv",
        ]
        .into_iter()
        .map(|name| std::fs::read_to_string(report_dir.join(name)).unwrap())
        .collect::<Vec<_>>()
        .join("\n");

        assert!(artifacts.contains("registry_redis_connection_failed"));
        assert!(artifacts.contains("\"redis_connection_error_class\":\"authentication_failed\""));
        assert!(!artifacts.contains("redis_command"));
        for forbidden in [
            "redis://observer:secret@host",
            "top-secret",
            "observer-user",
            "service:private",
            "metrics:v2:latest:private",
            "WRONGTYPE",
        ] {
            assert!(
                !artifacts.contains(forbidden),
                "report artifact leaked Redis detail: {forbidden}"
            );
        }

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn registry_observation_smoke_is_single_account_zero_write_and_player_free() {
        let config = registry_observer_config();
        let budget = config.effective_budget(&BudgetOverride::default()).unwrap();
        assert!(validate_registry_observation_smoke_config(&config, &budget).is_ok());

        let mut writable = config.clone();
        writable.scenario.writes_data = true;
        assert!(
            validate_registry_observation_smoke_config(&writable, &budget)
                .unwrap_err()
                .contains("max_data_writes")
        );

        let mut multi_account = config.clone();
        multi_account.account_prepare.account_count = Some(2);
        assert!(
            validate_registry_observation_smoke_config(&multi_account, &budget)
                .unwrap_err()
                .contains("exactly one")
        );

        let mut player_scenario = config.clone();
        player_scenario.scenario.side_services = Some(Default::default());
        assert!(
            validate_registry_observation_smoke_config(&player_scenario, &budget)
                .unwrap_err()
                .contains("forbids player")
        );

        let mut missing_observer = config;
        missing_observer.scenario.registry_observation = None;
        assert!(
            validate_registry_observation_smoke_config(&missing_observer, &budget)
                .unwrap_err()
                .contains("registry_observation")
        );
    }

    #[test]
    fn registry_observation_command_rejects_auth_and_private_inputs_before_config_load() {
        let auth = execute(vec![
            "observe-registry".into(),
            "--config".into(),
            "must-not-be-read.json".into(),
            "--execute-auth".into(),
        ])
        .unwrap_err();
        assert!(auth.contains("does not accept --dry-run"));

        let private = execute(vec![
            "observe-registry".into(),
            "--config".into(),
            "must-not-be-read.json".into(),
            "--private-config".into(),
            "must-not-be-read-private.json".into(),
        ])
        .unwrap_err();
        assert!(private.contains("does not accept --private-config"));
    }

    #[test]
    fn controller_health_tick_aborts_the_shared_controller_before_a_later_session() {
        let mut evaluator = ContinuousHealthEvaluator::new(2).unwrap();
        let mut abort = AbortController::default();
        observe_controller_health(&mut evaluator, &mut abort, true, 1, 1, 0, 1, None);
        assert!(!abort.should_stop_new_sessions());
        observe_controller_health(&mut evaluator, &mut abort, true, 1, 1, 0, 1, None);
        assert_eq!(abort.reason(), Some(&AbortReason::MetricsStale));
        assert!(abort.should_stop_new_sessions());
    }

    #[test]
    fn sustained_transport_backpressure_stops_reconnect_before_a_later_action() {
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

        let budget = loadtest_core::config::HardBudget {
            max_virtual_players: 1,
            max_login_qps: 10.0,
            max_new_connections_per_second: 10.0,
            max_business_messages_per_second: 10.0,
            max_messages_per_connection_per_second: 10.0,
            max_duration_secs: 10,
            max_total_operations: 2,
            max_error_rate: 1.0,
            max_connection_failure_rate: 1.0,
            max_p99_ms: 1_000,
            max_data_writes: 10,
        };
        let plan = loadtest_core::reconnect_burst::ReconnectBurstPlan {
            actions: vec![
                ReconnectBurstAction {
                    at_ms: 0,
                    player_slot: 0,
                    reconnect_attempt: 0,
                    step: ReconnectBurstStep::Login,
                },
                ReconnectBurstAction {
                    at_ms: 1,
                    player_slot: 0,
                    reconnect_attempt: 0,
                    step: ReconnectBurstStep::IssueTicket,
                },
            ],
            forced_disconnects: 0,
            login_actions: 1,
            new_connections: 0,
            total_operations: 2,
            potential_data_writes: 0,
            total_backoff_ms: 0,
            latest_action_ms: 1,
        };
        let signals = LiveBackpressureSignals {
            kcp: KcpBackpressureMetrics {
                pending_limit_rejections: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut evaluator = ContinuousHealthEvaluator::new(2).unwrap();
        let mut abort = AbortController::default();
        let mut admission = AuthDispatchAdmission::new(&budget).unwrap();
        let mut executor = RecordingExecutor::default();

        let outcome = execute_reconnect_burst(
            &plan,
            &budget,
            ReconnectBurstExecutionGate {
                execute_game: true,
                confirm_game: Some("local"),
                environment_name: "local",
                environment_kind: EnvironmentKind::Local,
            },
            &mut admission,
            Instant::now() + Duration::from_secs(1),
            &mut abort,
            |controller| {
                observe_controller_health(
                    &mut evaluator,
                    controller,
                    true,
                    0,
                    0,
                    0,
                    1,
                    Some(&signals),
                );
                Ok(())
            },
            &mut executor,
        );

        assert!(matches!(
            outcome,
            Err(loadtest_core::reconnect_burst::ReconnectBurstExecutionError::Stopped)
        ));
        assert_eq!(abort.reason(), Some(&AbortReason::Backpressure));
        assert_eq!(executor.actions.len(), 1);
        assert_eq!(executor.actions[0].step, ReconnectBurstStep::Login);
    }

    #[test]
    fn live_backpressure_signals_project_kcp_and_grpc_snapshots() {
        let mut signals = LiveBackpressureSignals::default();
        signals.record_kcp(KcpBackpressureMetrics {
            dropped_pending_requests: 1,
            ..Default::default()
        });
        signals.record_match_grpc(MatchGrpcBackpressureMetrics {
            dropped_pending_messages: 1,
            ..Default::default()
        });
        let mut observation = ContinuousHealthObservation::healthy();

        signals.apply_to_health(&mut observation);

        assert!(!observation.backpressure_healthy);
    }

    #[test]
    fn live_reconnect_burst_preflight_requires_the_game_gate_before_transport_setup() {
        let config: LoadTestConfig = serde_json::from_value(serde_json::json!({
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
                "max_total_operations": 10,
                "max_error_rate": 0.1,
                "max_connection_failure_rate": 0.1,
                "max_p99_ms": 100,
                "max_data_writes": 16
            },
            "scenario": {
                "name": "reconnect",
                "load": {"type": "fixed_concurrency", "virtual_players": 1, "duration_secs": 1},
                "writes_data": true,
                "reconnect_burst": {
                    "virtual_players": 1,
                    "reconnect_attempts_per_player": 1,
                    "reconnect_policy": {"max_attempts": 1, "base_delay_ms": 10, "max_delay_ms": 10}
                },
                "live_gameplay": {
                    "room_id": "approved-room",
                    "policy_id": "lockstep_sim_demo",
                    "profile": "normal",
                    "lockstep_scenario_json": "{}",
                    "max_frame_inputs": 1,
                    "reconnect": {
                        "last_character_push_sequence": 0,
                        "reconnect_policy": {"max_attempts": 1, "base_delay_ms": 10, "max_delay_ms": 10}
                    }
                }
            },
            "reports_root": "reports",
            "prepare_reports_root": "prepare"
        }))
        .unwrap();
        let budget = config.budget.clone();
        let mut cli = Cli::parse(vec!["run".into(), "--config".into(), "ignored".into()]).unwrap();
        assert!(
            validate_live_reconnect_burst_gate(&cli, &config, &budget)
                .unwrap_err()
                .contains("--execute-game")
        );
        cli.execute_game = true;
        cli.confirm_game = Some("local".into());
        validate_live_reconnect_burst_gate(&cli, &config, &budget).unwrap();
        let mut missing_room = config.clone();
        missing_room.scenario.live_gameplay = None;
        assert!(
            validate_live_reconnect_burst_gate(&cli, &missing_room, &budget)
                .unwrap_err()
                .contains("live_gameplay")
        );
        cli.confirm_game = Some("other".into());
        assert!(validate_live_reconnect_burst_gate(&cli, &config, &budget).is_err());
    }

    struct CountingReconnectAuthTransport {
        dispatched: u32,
        attempt_timeouts: Vec<Duration>,
    }

    impl AuthHttpTransport for CountingReconnectAuthTransport {
        fn send(
            &mut self,
            _request: AuthHttpRequest,
        ) -> loadtest_core::auth_http::AuthHttpResponse {
            self.dispatched = self.dispatched.saturating_add(1);
            loadtest_core::auth_http::AuthHttpResponse {
                status: Some(200),
                retry_after_secs: None,
                body: AuthResponseBody::Success(loadtest_core::auth_http::AuthSuccess {
                    access_token: Some("in-memory-access-token".into()),
                    ticket: None,
                    character_id: None,
                    services: None,
                }),
            }
        }

        fn set_attempt_timeout(&mut self, timeout: Duration) {
            self.attempt_timeouts.push(timeout);
        }
    }

    struct ReconnectRemoteProtection {
        fail_guard: bool,
        guard_checks: Cell<u32>,
    }

    impl RuntimeProtection for ReconnectRemoteProtection {
        fn verify_dns(&self) -> Result<(), String> {
            Ok(())
        }

        fn verify_certificate(&self) -> Result<(), String> {
            Ok(())
        }

        fn verify_descriptor(&self) -> Result<(), String> {
            Ok(())
        }

        fn verify_environment_identity(&self) -> Result<(), String> {
            Ok(())
        }

        fn revalidate(&self) -> Result<(), String> {
            Ok(())
        }
    }

    impl AuthenticatedPlayerProtection for ReconnectRemoteProtection {
        fn revalidate_before_auth_dispatch(&self) -> Result<(), String> {
            self.guard_checks
                .set(self.guard_checks.get().saturating_add(1));
            if self.fail_guard {
                Err("guard rejected test target".into())
            } else {
                Ok(())
            }
        }

        fn uses_guard_probe(&self) -> bool {
            true
        }
    }

    fn reconnect_guard_budget(max_total_operations: u64) -> loadtest_core::config::HardBudget {
        loadtest_core::config::HardBudget {
            max_virtual_players: 1,
            max_login_qps: 100.0,
            max_new_connections_per_second: 100.0,
            max_business_messages_per_second: 100.0,
            max_messages_per_connection_per_second: 100.0,
            max_duration_secs: 10,
            max_total_operations,
            max_error_rate: 1.0,
            max_connection_failure_rate: 1.0,
            max_p99_ms: 1_000,
            max_data_writes: 3,
        }
    }

    fn reconnect_auth_plan(
        with_later_connect: bool,
    ) -> loadtest_core::reconnect_burst::ReconnectBurstPlan {
        let mut actions = vec![ReconnectBurstAction {
            at_ms: 0,
            player_slot: 0,
            reconnect_attempt: 0,
            step: ReconnectBurstStep::Login,
        }];
        if with_later_connect {
            actions.push(ReconnectBurstAction {
                at_ms: 1,
                player_slot: 0,
                reconnect_attempt: 0,
                step: ReconnectBurstStep::ConnectProxy,
            });
        }
        loadtest_core::reconnect_burst::ReconnectBurstPlan {
            actions,
            forced_disconnects: 0,
            login_actions: 1,
            new_connections: if with_later_connect { 2 } else { 1 },
            total_operations: if with_later_connect { 2 } else { 1 },
            potential_data_writes: 3,
            total_backoff_ms: 0,
            latest_action_ms: u64::from(with_later_connect),
        }
    }

    struct GuardedReconnectAuthExecutor<'a> {
        transport: &'a mut CountingReconnectAuthTransport,
        protection: &'a ReconnectRemoteProtection,
        metrics: AuthRunMetrics,
        executed: Vec<ReconnectBurstStep>,
    }

    impl ReconnectBurstExecutor for GuardedReconnectAuthExecutor<'_> {
        fn execute(
            &mut self,
            action: ReconnectBurstAction,
            admission: &mut ReconnectBurstAdmission<'_>,
        ) -> Result<(), String> {
            self.executed.push(action.step);
            if action.step == ReconnectBurstStep::Login {
                send_reconnect_auth_with_guard(
                    self.transport,
                    AuthHttpRequest::Login {
                        login_name: "loadtest_account".into(),
                        password: "in-memory-only".into(),
                    },
                    admission,
                    self.protection,
                    &mut self.metrics,
                )?;
            }
            Ok(())
        }
    }

    #[test]
    fn reconnect_remote_guard_failure_sends_no_auth_and_stops_later_transport() {
        let budget = reconnect_guard_budget(3);
        let mut admission = AuthDispatchAdmission::new(&budget).unwrap();
        let mut abort = AbortController::default();
        let mut transport = CountingReconnectAuthTransport {
            dispatched: 0,
            attempt_timeouts: Vec::new(),
        };
        let protection = ReconnectRemoteProtection {
            fail_guard: true,
            guard_checks: Cell::new(0),
        };
        let mut executor = GuardedReconnectAuthExecutor {
            transport: &mut transport,
            protection: &protection,
            metrics: AuthRunMetrics::default(),
            executed: Vec::new(),
        };

        let result = execute_reconnect_burst(
            &reconnect_auth_plan(true),
            &budget,
            ReconnectBurstExecutionGate {
                execute_game: true,
                confirm_game: Some("local"),
                environment_name: "local",
                environment_kind: EnvironmentKind::Local,
            },
            &mut admission,
            Instant::now() + Duration::from_secs(1),
            &mut abort,
            |_| Ok(()),
            &mut executor,
        );

        assert!(matches!(
            result,
            Err(loadtest_core::reconnect_burst::ReconnectBurstExecutionError::Executor(_))
        ));
        assert_eq!(protection.guard_checks.get(), 1);
        assert_eq!(executor.transport.dispatched, 0);
        assert!(executor.transport.attempt_timeouts.is_empty());
        assert_eq!(executor.executed, vec![ReconnectBurstStep::Login]);
        assert_eq!(executor.metrics.guard_probe_attempts, 1);
        assert_eq!(executor.metrics.guard_probe_successes, 0);
        assert_eq!(abort.reason(), Some(&AbortReason::ProtectionUnknown));
    }

    #[test]
    fn reconnect_auth_dispatch_uses_finite_timeout_after_checkpoint() {
        let budget = reconnect_guard_budget(2);
        let mut admission = AuthDispatchAdmission::new(&budget).unwrap();
        let mut abort = AbortController::default();
        let mut transport = CountingReconnectAuthTransport {
            dispatched: 0,
            attempt_timeouts: Vec::new(),
        };
        let protection = ReconnectRemoteProtection {
            fail_guard: false,
            guard_checks: Cell::new(0),
        };
        let mut executor = GuardedReconnectAuthExecutor {
            transport: &mut transport,
            protection: &protection,
            metrics: AuthRunMetrics::default(),
            executed: Vec::new(),
        };

        let mut checkpoint_calls = 0;
        execute_reconnect_burst(
            &reconnect_auth_plan(false),
            &budget,
            ReconnectBurstExecutionGate {
                execute_game: true,
                confirm_game: Some("local"),
                environment_name: "local",
                environment_kind: EnvironmentKind::Local,
            },
            &mut admission,
            Instant::now() + Duration::from_secs(1),
            &mut abort,
            |_| {
                checkpoint_calls += 1;
                Ok(())
            },
            &mut executor,
        )
        .unwrap();

        assert_eq!(protection.guard_checks.get(), 1);
        assert_eq!(executor.transport.dispatched, 1);
        assert_eq!(executor.metrics.guard_probe_attempts, 1);
        assert_eq!(executor.metrics.guard_probe_successes, 1);
        assert_eq!(executor.metrics.guard_probe_connection_admissions, 1);
        assert_eq!(executor.metrics.requests, 1);
        assert_eq!(admission.used_operations(), 2);
        // Admission may re-check while waiting for a rate slot. The three
        // required checkpoints are action entry, guard admission, and the
        // per-dispatch timeout callback.
        assert!(checkpoint_calls >= 3);
        assert_eq!(executor.transport.attempt_timeouts.len(), 1);
        assert!(!executor.transport.attempt_timeouts[0].is_zero());
        assert_ne!(executor.transport.attempt_timeouts[0], Duration::MAX);
    }

    #[test]
    fn reconnect_expired_deadline_rejects_before_auth_transport() {
        let budget = reconnect_guard_budget(1);
        let mut admission = AuthDispatchAdmission::new(&budget).unwrap();
        let mut abort = AbortController::default();
        let mut transport = CountingReconnectAuthTransport {
            dispatched: 0,
            attempt_timeouts: Vec::new(),
        };
        let protection = ReconnectRemoteProtection {
            fail_guard: false,
            guard_checks: Cell::new(0),
        };
        let mut executor = GuardedReconnectAuthExecutor {
            transport: &mut transport,
            protection: &protection,
            metrics: AuthRunMetrics::default(),
            executed: Vec::new(),
        };

        let result = execute_reconnect_burst(
            &reconnect_auth_plan(false),
            &budget,
            ReconnectBurstExecutionGate {
                execute_game: true,
                confirm_game: Some("local"),
                environment_name: "local",
                environment_kind: EnvironmentKind::Local,
            },
            &mut admission,
            Instant::now()
                .checked_sub(Duration::from_millis(1))
                .unwrap(),
            &mut abort,
            |_| Ok(()),
            &mut executor,
        );

        assert!(matches!(
            result,
            Err(
                loadtest_core::reconnect_burst::ReconnectBurstExecutionError::Admission(
                    AuthAdmissionError::DeadlineExceeded
                )
            )
        ));
        assert!(executor.executed.is_empty());
        assert_eq!(executor.transport.dispatched, 0);
        assert!(executor.transport.attempt_timeouts.is_empty());
    }

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

    fn room_recovery_packet(message_type: MessageType, sequence: u32, body: Vec<u8>) -> Packet {
        Packet::new(
            game_protocol::PacketHeader {
                msg_type: message_type as u16,
                seq: sequence,
                body_len: body.len() as u32,
            },
            body,
        )
    }

    #[test]
    fn room_recovery_accepts_approved_pushes_before_expected_response() {
        let mut packets = vec![
            room_recovery_packet(MessageType::RoomStatePush, 1, Vec::new()),
            room_recovery_packet(MessageType::RoomFrameRatePush, 2, Vec::new()),
            room_recovery_packet(
                MessageType::RoomJoinRes,
                3,
                game_protocol::encode_body(&loadtest_core::pb::RoomJoinRes {
                    ok: true,
                    room_id: "approved-room".into(),
                    error_code: String::new(),
                }),
            ),
        ]
        .into_iter();
        let mut pushed = Vec::new();

        let response = receive_room_recovery_response(
            || Ok(packets.next().expect("test packet")),
            |packet| {
                pushed.push(packet.message_type().expect("known push"));
                Ok(())
            },
            MessageType::RoomJoinRes,
        )
        .unwrap();

        assert_eq!(response.message_type(), Some(MessageType::RoomJoinRes));
        assert_eq!(
            pushed,
            vec![MessageType::RoomStatePush, MessageType::RoomFrameRatePush]
        );
    }

    #[test]
    fn room_recovery_rejects_unexpected_and_business_packets() {
        let unexpected = receive_room_recovery_response(
            || Ok(room_recovery_packet(MessageType::AuthRes, 1, Vec::new())),
            |_| Ok(()),
            MessageType::RoomJoinRes,
        );
        assert_eq!(
            unexpected,
            Err(ReconnectFailureCategory::RoomUnexpectedPacket)
        );

        let business_error = receive_room_recovery_response(
            || {
                Ok(room_recovery_packet(
                    MessageType::ErrorRes,
                    2,
                    game_protocol::encode_body(&loadtest_core::pb::ErrorRes {
                        error_code: "ROOM_REJECTED".into(),
                        message: "must not reach reports".into(),
                    }),
                ))
            },
            |_| Ok(()),
            MessageType::RoomJoinRes,
        );
        assert_eq!(
            business_error,
            Err(ReconnectFailureCategory::RoomServerBusinessError)
        );

        let rejected_join = room_recovery_packet(
            MessageType::RoomJoinRes,
            3,
            game_protocol::encode_body(&loadtest_core::pb::RoomJoinRes {
                ok: false,
                room_id: "approved-room".into(),
                error_code: "ROOM_REJECTED".into(),
            }),
        );
        assert_eq!(
            classify_room_recovery_response(&rejected_join, 0, "approved-room"),
            Err(ReconnectFailureCategory::RoomServerBusinessError)
        );

        let wrong_room = room_recovery_packet(
            MessageType::RoomJoinRes,
            4,
            game_protocol::encode_body(&loadtest_core::pb::RoomJoinRes {
                ok: true,
                room_id: "other-room".into(),
                error_code: String::new(),
            }),
        );
        assert_eq!(
            classify_room_recovery_response(&wrong_room, 0, "approved-room"),
            Err(ReconnectFailureCategory::RoomBoundaryRejected)
        );
    }

    #[test]
    fn room_handoff_retry_is_limited_to_the_known_temporary_reconnect_rejection() {
        let temporary_rejection = room_recovery_packet(
            MessageType::RoomReconnectRes,
            1,
            game_protocol::encode_body(&loadtest_core::pb::RoomReconnectRes {
                ok: false,
                room_id: String::new(),
                error_code: TEMPORARY_ROOM_HANDOFF_ERROR_CODE.into(),
                snapshot: None,
                ..Default::default()
            }),
        );
        assert_eq!(
            classify_room_recovery_response(&temporary_rejection, 1, "approved-room"),
            Ok(RoomRecoveryResponse::TemporaryHandoffRejected)
        );

        let retry_success = room_recovery_packet(
            MessageType::RoomReconnectRes,
            2,
            game_protocol::encode_body(&loadtest_core::pb::RoomReconnectRes {
                ok: true,
                room_id: "approved-room".into(),
                error_code: String::new(),
                snapshot: None,
                ..Default::default()
            }),
        );
        assert_eq!(
            classify_room_recovery_response(&retry_success, 1, "approved-room"),
            Ok(RoomRecoveryResponse::Recovered)
        );

        let non_temporary_rejection = room_recovery_packet(
            MessageType::RoomReconnectRes,
            3,
            game_protocol::encode_body(&loadtest_core::pb::RoomReconnectRes {
                ok: false,
                room_id: String::new(),
                error_code: "ROOM_NOT_FOUND".into(),
                snapshot: None,
                ..Default::default()
            }),
        );
        assert_eq!(
            classify_room_recovery_response(&non_temporary_rejection, 1, "approved-room"),
            Err(ReconnectFailureCategory::RoomServerBusinessError)
        );

        let join_rejection = room_recovery_packet(
            MessageType::RoomJoinRes,
            4,
            game_protocol::encode_body(&loadtest_core::pb::RoomJoinRes {
                ok: false,
                room_id: String::new(),
                error_code: TEMPORARY_ROOM_HANDOFF_ERROR_CODE.into(),
            }),
        );
        assert_eq!(
            classify_room_recovery_response(&join_rejection, 0, "approved-room"),
            Err(ReconnectFailureCategory::RoomServerBusinessError)
        );

        let error = loadtest_core::reconnect_burst::ReconnectBurstExecutionError::Executor(
            ReconnectFailureCategory::RoomHandoffRetryExhausted.executor_error(),
        );
        let mut errors = ErrorBuffer::default();
        record_reconnect_execution_failure(&mut errors, &error);
        let sample = errors.samples().first().expect("handoff failure sample");
        assert_eq!(
            sample.category,
            "reconnect_burst_room_handoff_retry_exhausted"
        );
        assert!(!sample.message.contains(TEMPORARY_ROOM_HANDOFF_ERROR_CODE));
        assert!(sample.context.is_empty());
    }

    #[test]
    fn room_handoff_wait_stops_or_expires_before_retry_dispatch() {
        let now = Instant::now();
        let stopped = wait_for_reconnect_action(
            now + Duration::from_secs(1),
            now + Duration::from_secs(2),
            || Err(ReconnectFailureCategory::Stopped),
            |_| panic!("stopped retry must not sleep or dispatch"),
        );
        assert_eq!(stopped, Err(ReconnectFailureCategory::Stopped));

        let expired = wait_for_reconnect_action(
            now + Duration::from_secs(1),
            now.checked_sub(Duration::from_millis(1)).unwrap(),
            || Ok(()),
            |_| panic!("expired retry must not sleep or dispatch"),
        );
        assert_eq!(expired, Err(ReconnectFailureCategory::DeadlineExceeded));
    }

    #[test]
    fn room_recovery_preserves_deadline_timeout_and_caps_async_pushes() {
        let timeout = receive_room_recovery_response(
            || Err(ReconnectFailureCategory::RoomResponseTimeout),
            |_| panic!("a timed-out read must not invoke the push handler"),
            MessageType::RoomJoinRes,
        );
        assert_eq!(timeout, Err(ReconnectFailureCategory::RoomResponseTimeout));
        assert_eq!(
            reconnect_room_receive_failure_category(GameLiveError::Transport(
                "KCP session deadline elapsed"
            )),
            ReconnectFailureCategory::RoomResponseTimeout
        );

        let mut received = 0;
        let mut handled = 0;
        let push_limit = receive_room_recovery_response(
            || {
                received += 1;
                Ok(room_recovery_packet(
                    MessageType::RoomStatePush,
                    received,
                    Vec::new(),
                ))
            },
            |_| {
                handled += 1;
                Ok(())
            },
            MessageType::RoomJoinRes,
        );
        assert_eq!(
            push_limit,
            Err(ReconnectFailureCategory::RoomAsyncPushLimit)
        );
        assert_eq!(handled, MAX_ROOM_RECOVERY_ASYNC_PUSHES);
        assert_eq!(received, (MAX_ROOM_RECOVERY_ASYNC_PUSHES + 1) as u32);
    }

    #[test]
    fn reconnect_failure_reports_use_static_precise_categories() {
        use loadtest_core::reconnect_burst::ReconnectBurstExecutionError;

        let cases = [
            (
                ReconnectBurstExecutionError::Executor(
                    ReconnectFailureCategory::RoomServerBusinessError.executor_error(),
                ),
                ReconnectFailureCategory::RoomServerBusinessError,
            ),
            (
                ReconnectBurstExecutionError::Admission(AuthAdmissionError::DeadlineExceeded),
                ReconnectFailureCategory::DeadlineExceeded,
            ),
            (
                ReconnectBurstExecutionError::Checkpoint("endpoint=secret".into()),
                ReconnectFailureCategory::ProtectionOrCheckpointFailed,
            ),
            (
                ReconnectBurstExecutionError::Executor("reconnect burst KCP connect failed".into()),
                ReconnectFailureCategory::TransportFailed,
            ),
            (
                ReconnectBurstExecutionError::Executor(
                    "remote auth target protection failed before request dispatch".into(),
                ),
                ReconnectFailureCategory::ProtectionOrCheckpointFailed,
            ),
            (
                ReconnectBurstExecutionError::Executor(
                    ReconnectFailureCategory::RoomHandoffRetryExhausted.executor_error(),
                ),
                ReconnectFailureCategory::RoomHandoffRetryExhausted,
            ),
        ];

        for (error, expected) in cases {
            let mut errors = ErrorBuffer::default();
            record_reconnect_execution_failure(&mut errors, &error);
            let sample = errors.samples().first().expect("reported failure sample");
            assert_eq!(sample.category, expected.report_category());
            assert_eq!(sample.message, expected.report_message());
            assert!(sample.context.is_empty());
            assert!(!sample.message.contains("secret"));
        }
    }

    #[test]
    fn online_mail_claim_failures_keep_public_contract_context_and_fixed_categories() {
        for (http_status, claim_status, error, category) in [
            (
                409,
                "manual_review",
                Some("MAIL_CLAIM_ROUTE_UNAVAILABLE"),
                "online_mail_claim_manual_review",
            ),
            (202, "processing", None, "online_mail_claim_processing"),
            (
                202,
                "reconciliation_pending",
                Some("MAIL_CLAIM_RECONCILIATION_PENDING"),
                "online_mail_claim_reconciliation_pending",
            ),
        ] {
            let failure = LiveGameSideServiceError::from_side_http(SideHttpError::MailClaimFailed(
                MailClaimFailure {
                    http_status,
                    claim_status: claim_status.into(),
                    error: error.map(str::to_owned),
                },
            ));
            let (actual_category, _message, context) = failure.report_details();
            assert_eq!(actual_category, category);
            assert_eq!(context.get("http_status"), Some(&http_status.to_string()));
            assert_eq!(context.get("claim_status"), Some(&claim_status.to_string()));
            assert_eq!(context.get("error").map(String::as_str), error);
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
                    "max_total_operations": 14,
                    "max_error_rate": 0.1,
                    "max_connection_failure_rate": 0.1,
                    "max_p99_ms": 100,
                    "max_data_writes": 24
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
        assert_eq!(metrics.counters["reconnect_burst_forced_disconnects"], 2);
        assert_eq!(metrics.counters["reconnect_burst_new_connections"], 6);
        assert_eq!(metrics.counters["reconnect_burst_room_recoveries"], 3);
        assert_eq!(
            metrics.counters["reconnect_burst_room_recovery_retry_slots"],
            2
        );
        assert_eq!(metrics.counters["reconnect_burst_backoff_ms"], 300);
        assert_eq!(
            metrics.counters["reconnect_burst_potential_data_writes"],
            24
        );
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
    fn online_default_match_mail_claim_phase_is_exactly_bounded() {
        let side: SideServicesScenario = serde_json::from_value(serde_json::json!({
            "mail": {
                "steps": [
                    { "operation": "mail_list", "weight": 1 },
                    { "operation": "mail_claim", "weight": 1 },
                    { "operation": "mail_claim", "weight": 1 }
                ],
                "writes": true,
                "live_http": true,
                "write_batch": "batch"
            },
            "composition": {
                "weights": { "mail": 1 },
                "max_operations_per_player": 3,
                "max_operations_per_service_per_player": { "mail": 3 }
            }
        }))
        .unwrap();
        assert!(requires_online_default_match_mail_claim_phase(&side).unwrap());

        let mut reordered = side.clone();
        reordered.mail.as_mut().unwrap().steps.swap(0, 1);
        assert!(
            requires_online_default_match_mail_claim_phase(&reordered)
                .unwrap_err()
                .contains("mail_list, mail_claim, mail_claim")
        );

        let mut mixed_service = side;
        mixed_service.announce = Some(Default::default());
        assert!(
            requires_online_default_match_mail_claim_phase(&mixed_service)
                .unwrap_err()
                .contains("mail only")
        );
    }

    #[test]
    fn scoped_blocking_side_work_drops_tokio_runtime_outside_kcp_runtime() {
        let caller_thread = std::thread::current().id();
        let kcp_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        let worker_thread = kcp_runtime
            .block_on(async {
                run_scoped_blocking_side_work(|| {
                    let side_runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()
                        .map_err(|_| ())?;
                    side_runtime.block_on(async {});
                    drop(side_runtime);
                    Ok::<_, ()>(std::thread::current().id())
                })
            })
            .unwrap();

        assert_ne!(worker_thread, caller_thread);
    }

    #[test]
    fn production_execution_is_limited_to_the_two_player_player_chain() {
        assert!(
            validate_production_authenticated_player_chain(
                EnvironmentKind::Production,
                true,
                true,
                false,
                false,
                false,
                false,
                false,
            )
            .is_ok()
        );
        for rejected in [
            (false, true, false, false, false, false, false),
            (true, false, false, false, false, false, false),
            (true, true, true, false, false, false, false),
            (true, true, false, true, false, false, false),
            (true, true, false, false, false, false, true),
        ] {
            assert!(
                validate_production_authenticated_player_chain(
                    EnvironmentKind::Production,
                    rejected.0,
                    rejected.1,
                    rejected.2,
                    rejected.3,
                    rejected.4,
                    rejected.5,
                    rejected.6,
                )
                .is_err()
            );
        }
        assert!(
            validate_production_authenticated_player_chain(
                EnvironmentKind::Local,
                false,
                false,
                true,
                true,
                true,
                true,
                true,
            )
            .is_ok()
        );
        assert!(
            validate_production_authenticated_player_chain(
                EnvironmentKind::Test,
                false,
                false,
                true,
                true,
                true,
                true,
                true,
            )
            .is_ok()
        );
        assert!(
            validate_production_authenticated_player_chain(
                EnvironmentKind::Staging,
                true,
                true,
                false,
                false,
                false,
                false,
                false,
            )
            .is_err()
        );
    }

    struct CountingRemoteProtection {
        waiting_checks: Cell<u32>,
        guard_probes: Cell<u32>,
    }

    impl RuntimeProtection for CountingRemoteProtection {
        fn verify_dns(&self) -> Result<(), String> {
            Ok(())
        }

        fn verify_certificate(&self) -> Result<(), String> {
            Ok(())
        }

        fn verify_descriptor(&self) -> Result<(), String> {
            Ok(())
        }

        fn verify_environment_identity(&self) -> Result<(), String> {
            Ok(())
        }
    }

    impl AuthenticatedPlayerProtection for CountingRemoteProtection {
        fn revalidate_while_waiting(&self) -> Result<(), String> {
            self.waiting_checks
                .set(self.waiting_checks.get().saturating_add(1));
            Ok(())
        }

        fn revalidate_before_auth_dispatch(&self) -> Result<(), String> {
            self.guard_probes
                .set(self.guard_probes.get().saturating_add(1));
            Ok(())
        }

        fn uses_guard_probe(&self) -> bool {
            true
        }
    }

    #[test]
    fn guarded_auth_admission_budgets_one_probe_and_keeps_wait_checks_local() {
        let budget = loadtest_core::config::HardBudget {
            max_virtual_players: 1,
            max_login_qps: 10.0,
            max_new_connections_per_second: 10.0,
            max_business_messages_per_second: 10.0,
            max_messages_per_connection_per_second: 10.0,
            max_duration_secs: 2,
            max_total_operations: 8,
            max_error_rate: 1.0,
            max_connection_failure_rate: 1.0,
            max_p99_ms: 1_000,
            max_data_writes: 3,
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut admission = AuthDispatchAdmission::new(&budget).unwrap();
        admission
            .admit(
                &AuthHttpRequest::Me {
                    access_token: "in-memory-only".into(),
                },
                deadline,
                || Ok(()),
            )
            .unwrap();
        let protection = CountingRemoteProtection {
            waiting_checks: Cell::new(0),
            guard_probes: Cell::new(0),
        };
        let mut abort = AbortController::default();
        let ctrl_c = AtomicBool::new(false);
        let mut auth_metrics = AuthRunMetrics::default();

        admit_live_auth_request(
            &mut admission,
            &AuthHttpRequest::Login {
                login_name: "loadtest-account".into(),
                password: "in-memory-only".into(),
            },
            deadline,
            &protection,
            &mut abort,
            &ctrl_c,
            None,
            u64::MAX,
            &mut auth_metrics,
        )
        .unwrap();

        assert!(protection.waiting_checks.get() > 1);
        assert_eq!(protection.guard_probes.get(), 1);
        assert_eq!(admission.used_operations(), 3);
        assert_eq!(auth_metrics.guard_probe_attempts, 1);
        assert_eq!(auth_metrics.guard_probe_successes, 1);
        assert_eq!(auth_metrics.guard_probe_connection_admissions, 1);
        let mut core_metrics = Metrics::default();
        record_auth_metrics(&mut core_metrics, &auth_metrics);
        let snapshot = core_metrics.snapshot();
        assert_eq!(snapshot.counters["auth_guard_probe_attempts"], 1);
        assert_eq!(
            snapshot.counters["auth_guard_probe_connection_admissions"],
            1
        );
        assert_eq!(snapshot.histograms["auth_guard_probe_ms"].count(), 1);
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
