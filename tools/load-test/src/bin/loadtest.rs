use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use loadtest_core::SCHEMA_VERSION;
use loadtest_core::abort::{AbortController, GracefulShutdown, ShutdownPhase, install_ctrl_c_flag};
use loadtest_core::config::{BudgetOverride, LoadTestConfig, RunAccess, load_config};
use loadtest_core::contracts::{RunPlan, single_process_assignment};
use loadtest_core::lifecycle::{Lifecycle, RunState};
use loadtest_core::metrics::Metrics;
use loadtest_core::preflight::summarize_run;
use loadtest_core::protection::{DryRunProtection, revalidate_or_abort};
use loadtest_core::report::{ErrorBuffer, ReportInput, write_report};
use loadtest_core::resource::ResourceSampler;
use loadtest_core::scheduler::MonotonicScheduler;

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
        "run" | "calibrate" => run_dry(&parsed)?,
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
            "stage one has no real service client; run and calibrate require --dry-run".into(),
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
    allow_remote: bool,
    confirmation: Option<String>,
    dry_run: bool,
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
            allow_remote: false,
            confirmation: None,
            dry_run: false,
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
                "--allow-remote" => cli.allow_remote = true,
                "--confirm" => {
                    cli.confirmation = Some(
                        values
                            .next()
                            .ok_or("--confirm requires the environment name")?,
                    )
                }
                "--dry-run" => cli.dry_run = true,
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
    "usage: loadtest <validate|calibrate|run> --config <file> [--private-config <file>] [--allow-remote --confirm <environment>] [--dry-run] [--deadline-unix-ms N] [--max-virtual-players N --max-login-qps N --max-duration-secs N]\n       loadtest report --report-dir <reports/run-id>".into()
}
