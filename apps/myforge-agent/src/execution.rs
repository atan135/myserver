use std::env;
use std::ffi::OsString;
use std::fs;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use command_group::AsyncCommandGroup;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::task::JoinHandle;

use crate::command::{
    CommandControl, CommandHandler, CommandHandlerOutcome, StartedExecution,
    StartedExecutionOutcome,
};
use crate::config::AgentConfig;
use crate::preflight::{AuditorIdentity, PreflightReport};
use crate::protocol::MAX_SAFE_INTEGER;
use crate::schemas::{
    ArtifactSummary, AuditFinding, AuditSummary, CommandExecute, CommandRejection,
    CommandResultSemantic,
};

const AUDITOR_STDOUT_LIMIT: usize = 1_048_576;
const AUDITOR_DIAGNOSTIC_LIMIT: usize = 4_096;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(5);
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const ENVIRONMENT_ALLOWLIST: &[&str] = &[
    "APPDATA",
    "CODEX_HOME",
    "COMSPEC",
    "HOME",
    "LANG",
    "LC_ALL",
    "LOCALAPPDATA",
    "PATH",
    "PATHEXT",
    "SSL_CERT_DIR",
    "SSL_CERT_FILE",
    "SystemRoot",
    "TEMP",
    "TERM",
    "TMP",
    "USERPROFILE",
    "WINDIR",
];

#[derive(Clone, Copy)]
struct CommandBudget {
    deadline: tokio::time::Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BudgetStop {
    Cancelled,
    TimedOut,
}

impl CommandBudget {
    fn new(timeout: Duration) -> Self {
        Self {
            deadline: tokio::time::Instant::now() + timeout,
        }
    }

    fn check(self, control: &CommandControl) -> Result<(), BudgetStop> {
        if control.cancellation().is_cancelled() {
            Err(BudgetStop::Cancelled)
        } else if tokio::time::Instant::now() >= self.deadline {
            Err(BudgetStop::TimedOut)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObservationFailure {
    Cancelled,
    TimedOut,
    Invalid,
}

impl From<BudgetStop> for ObservationFailure {
    fn from(stop: BudgetStop) -> Self {
        match stop {
            BudgetStop::Cancelled => Self::Cancelled,
            BudgetStop::TimedOut => Self::TimedOut,
        }
    }
}

pub struct ControlledCommandHandler {
    settings: Arc<ExecutionSettings>,
    runner: Arc<dyn ProcessRunner>,
    clock: Arc<dyn ExecutionClock>,
}

impl ControlledCommandHandler {
    pub fn new(config: &AgentConfig, preflight: &PreflightReport) -> Self {
        Self {
            settings: Arc::new(ExecutionSettings {
                codex_bin: config.codex_bin().clone(),
                root_real: preflight.root_real().to_path_buf(),
                auditor: preflight.auditor_identity().cloned(),
                dry_run: config.dry_run(),
                audit_timeout_ms: config.audit().timeout_ms(),
                clock_skew_ms: config.limits().clock_skew_ms,
                environment: minimal_environment(),
            }),
            runner: Arc::new(SystemProcessRunner),
            clock: Arc::new(SystemExecutionClock),
        }
    }

    #[cfg(test)]
    fn with_dependencies(
        settings: ExecutionSettings,
        runner: Arc<dyn ProcessRunner>,
        clock: Arc<dyn ExecutionClock>,
    ) -> Self {
        Self {
            settings: Arc::new(settings),
            runner,
            clock,
        }
    }
}

#[async_trait]
impl CommandHandler for ControlledCommandHandler {
    async fn execute(
        &self,
        command: CommandExecute,
        control: CommandControl,
    ) -> CommandHandlerOutcome {
        let mut paths = match validate_workspace_paths(&self.settings.root_real, &command) {
            Ok(paths) => paths,
            Err(rejection) => return CommandHandlerOutcome::PreStartError(rejection),
        };
        paths.dry_run = self.settings.dry_run;

        if control.cancellation().is_cancelled() {
            return CommandHandlerOutcome::CompletedBeforeStart(Box::new(cancelled_before_start(
                &command,
                &paths,
                self.clock.as_ref(),
            )));
        }
        let now_ms = self.clock.now_ms();
        if now_ms
            > command
                .expires_at_ms
                .saturating_add(self.settings.clock_skew_ms)
        {
            return CommandHandlerOutcome::PreStartError(CommandRejection::new(
                "MYFORGE_COMMAND_EXPIRED",
                "command expired before local execution",
                false,
            ));
        }

        if self.settings.dry_run {
            let started_at_ms = now_ms;
            let budget = CommandBudget::new(Duration::from_millis(command.timeout_ms));
            let clock = self.clock.clone();
            return CommandHandlerOutcome::Started(StartedExecution::new_outcome(
                started_at_ms,
                complete_dry_run(command, paths, control, started_at_ms, budget, clock),
            ));
        }

        if control.cancellation().is_cancelled() {
            return CommandHandlerOutcome::CompletedBeforeStart(Box::new(cancelled_before_start(
                &command,
                &paths,
                self.clock.as_ref(),
            )));
        }
        let started_at_ms = self.clock.now_ms();
        if started_at_ms
            > command
                .expires_at_ms
                .saturating_add(self.settings.clock_skew_ms)
        {
            return CommandHandlerOutcome::PreStartError(CommandRejection::new(
                "MYFORGE_COMMAND_EXPIRED",
                "command expired before local execution",
                false,
            ));
        }
        let budget = CommandBudget::new(Duration::from_millis(command.timeout_ms));
        let specification = codex_specification(&self.settings, &command);
        let process = match self.runner.spawn(specification) {
            Ok(process) => process,
            Err(()) if control.cancellation().is_cancelled() => {
                return CommandHandlerOutcome::CompletedBeforeStart(Box::new(
                    cancelled_before_start(&command, &paths, self.clock.as_ref()),
                ));
            }
            Err(()) => {
                return CommandHandlerOutcome::PreStartError(CommandRejection::new(
                    "MYFORGE_COMMAND_SPAWN_FAILED",
                    "controlled command could not be started",
                    true,
                ));
            }
        };
        let settings = self.settings.clone();
        let runner = self.runner.clone();
        let clock = self.clock.clone();
        CommandHandlerOutcome::Started(StartedExecution::new_outcome(
            started_at_ms,
            complete_codex_execution(
                command,
                paths,
                process,
                control,
                started_at_ms,
                budget,
                settings,
                runner,
                clock,
            ),
        ))
    }
}

async fn complete_dry_run(
    command: CommandExecute,
    paths: ValidatedPaths,
    control: CommandControl,
    started_at_ms: u64,
    budget: CommandBudget,
    clock: Arc<dyn ExecutionClock>,
) -> StartedExecutionOutcome {
    let artifact =
        match observe_artifact(&paths.artifact_path, &paths.root_real, &control, budget).await {
            Ok(artifact) => artifact,
            Err(ObservationFailure::Cancelled) => {
                return cancelled_result(
                    &command,
                    "dry_run",
                    Some(started_at_ms),
                    ArtifactSummary::missing(),
                    clock.now_ms(),
                )
                .into();
            }
            Err(ObservationFailure::TimedOut) => {
                return StartedExecutionOutcome::FailClosed {
                    reason: "dry_run_command_timeout",
                };
            }
            Err(ObservationFailure::Invalid) => ArtifactSummary::missing(),
        };
    match budget.check(&control) {
        Err(BudgetStop::Cancelled) => {
            return cancelled_result(
                &command,
                "dry_run",
                Some(started_at_ms),
                artifact,
                clock.now_ms(),
            )
            .into();
        }
        Err(BudgetStop::TimedOut) => {
            return StartedExecutionOutcome::FailClosed {
                reason: "dry_run_command_timeout",
            };
        }
        Ok(()) => {}
    }
    let stdout = format!(
        "DRY_RUN_OK requestId={} artifactFile={}",
        command.request_id, command.input.artifact_file
    );
    let captured = capture_string(&stdout, command.max_output_bytes as usize);
    CommandResultSemantic {
        execution_mode: "dry_run".to_string(),
        status: "completed".to_string(),
        exit_code: None,
        stdout_preview: captured.preview,
        stderr_preview: String::new(),
        stdout_bytes: captured.bytes,
        stderr_bytes: 0,
        stdout_truncated: captured.truncated,
        stderr_truncated: false,
        artifact_file: command.input.artifact_file,
        consumer_target_file: command.input.consumer_target_file,
        artifact,
        audit: AuditSummary::skipped("dry_run"),
        error_code: None,
        error_message: None,
        started_at_ms: Some(started_at_ms),
        completed_at_ms: clock.now_ms().max(started_at_ms),
    }
    .into()
}

#[allow(clippy::too_many_arguments)]
async fn complete_codex_execution(
    command: CommandExecute,
    paths: ValidatedPaths,
    process: Box<dyn RunningProcess>,
    control: CommandControl,
    started_at_ms: u64,
    budget: CommandBudget,
    settings: Arc<ExecutionSettings>,
    runner: Arc<dyn ProcessRunner>,
    clock: Arc<dyn ExecutionClock>,
) -> StartedExecutionOutcome {
    let output = process
        .wait(control.clone(), budget.deadline, clock.clone())
        .await;
    let artifact_observation =
        observe_artifact(&paths.artifact_path, &paths.root_real, &control, budget).await;
    let artifact = artifact_observation
        .as_ref()
        .cloned()
        .unwrap_or_else(|_| ArtifactSummary::missing());
    let completed_at_ms = clock.now_ms().max(started_at_ms);
    let mut base = result_with_output(
        &command,
        "codex_exec",
        Some(started_at_ms),
        completed_at_ms,
        artifact,
        &output,
    );

    match budget.check(&control) {
        Err(BudgetStop::Cancelled) => {
            apply_failure(
                &mut base,
                "cancelled",
                "MYFORGE_COMMAND_CANCELLED",
                "command was cancelled",
                "cancelled",
            );
            base.exit_code = output.exit_code;
            return base.into();
        }
        Err(BudgetStop::TimedOut) => {
            apply_failure(
                &mut base,
                "failed",
                "MYFORGE_COMMAND_TIMEOUT",
                "command exceeded its time limit",
                "execution_failed",
            );
            base.exit_code = None;
            return base.into();
        }
        Ok(()) => {}
    }

    match output.termination {
        ProcessTermination::Cancelled => {
            apply_failure(
                &mut base,
                "cancelled",
                "MYFORGE_COMMAND_CANCELLED",
                "command was cancelled",
                "cancelled",
            );
            base.exit_code = output.exit_code;
            return base.into();
        }
        ProcessTermination::TimedOut => {
            apply_failure(
                &mut base,
                "failed",
                "MYFORGE_COMMAND_TIMEOUT",
                "command exceeded its time limit",
                "execution_failed",
            );
            base.exit_code = None;
            return base.into();
        }
        ProcessTermination::IoFailed => {
            apply_failure(
                &mut base,
                "failed",
                "MYFORGE_COMMAND_FAILED",
                "command execution did not complete cleanly",
                "execution_failed",
            );
            base.exit_code = output.exit_code.filter(|code| *code != 0);
            return base.into();
        }
        ProcessTermination::Exited => {}
    }

    match artifact_observation {
        Err(ObservationFailure::Cancelled) => {
            apply_failure(
                &mut base,
                "cancelled",
                "MYFORGE_COMMAND_CANCELLED",
                "command was cancelled",
                "cancelled",
            );
            return base.into();
        }
        Err(ObservationFailure::TimedOut) => {
            apply_failure(
                &mut base,
                "failed",
                "MYFORGE_COMMAND_TIMEOUT",
                "command exceeded its time limit",
                "execution_failed",
            );
            base.exit_code = None;
            return base.into();
        }
        Ok(_) | Err(ObservationFailure::Invalid) => {}
    }
    if output.exit_code != Some(0) {
        apply_failure(
            &mut base,
            "failed",
            "MYFORGE_COMMAND_FAILED",
            "command exited unsuccessfully",
            "execution_failed",
        );
        return base.into();
    }
    if !base.artifact.exists {
        apply_failure(
            &mut base,
            "failed",
            "MYFORGE_TARGET_FILE_MISSING",
            "expected artifact was not created",
            "artifact_missing",
        );
        return base.into();
    }

    let audit = run_audit(
        &command,
        &paths,
        control.clone(),
        budget,
        &settings,
        runner,
        clock.clone(),
    )
    .await;
    let mut audit = match audit {
        AuditOutcome::Summary(audit) => audit,
        AuditOutcome::Cancelled => {
            apply_failure(
                &mut base,
                "cancelled",
                "MYFORGE_COMMAND_CANCELLED",
                "command was cancelled",
                "cancelled",
            );
            return base.into();
        }
        AuditOutcome::CommandTimedOut => {
            apply_failure(
                &mut base,
                "failed",
                "MYFORGE_COMMAND_TIMEOUT",
                "command exceeded its time limit",
                "execution_failed",
            );
            base.exit_code = None;
            return base.into();
        }
    };
    match budget.check(&control) {
        Err(BudgetStop::Cancelled) => {
            apply_failure(
                &mut base,
                "cancelled",
                "MYFORGE_COMMAND_CANCELLED",
                "command was cancelled",
                "cancelled",
            );
            return base.into();
        }
        Err(BudgetStop::TimedOut) => {
            apply_failure(
                &mut base,
                "failed",
                "MYFORGE_COMMAND_TIMEOUT",
                "command exceeded its time limit",
                "execution_failed",
            );
            base.exit_code = None;
            return base.into();
        }
        Ok(()) => {}
    }
    apply_artifact_freshness(
        &mut audit,
        &base.artifact,
        started_at_ms,
        settings.clock_skew_ms,
    );
    base.audit = audit;
    match base.audit.status.as_str() {
        "passed" | "unavailable" => {}
        "warning" => {
            base.status = "completed_with_errors".to_string();
            base.error_code = Some("FANGYUAN_BLUEPRINT_AUDIT_WARNING".to_string());
            base.error_message = Some("artifact audit completed with warnings".to_string());
        }
        _ => {
            base.status = "completed_with_errors".to_string();
            base.error_code = Some("FANGYUAN_BLUEPRINT_AUDIT_FAILED".to_string());
            base.error_message = Some("artifact audit failed".to_string());
        }
    }
    base.into()
}

fn result_with_output(
    command: &CommandExecute,
    execution_mode: &str,
    started_at_ms: Option<u64>,
    completed_at_ms: u64,
    artifact: ArtifactSummary,
    output: &ProcessOutput,
) -> CommandResultSemantic {
    CommandResultSemantic {
        execution_mode: execution_mode.to_string(),
        status: "completed".to_string(),
        exit_code: output.exit_code,
        stdout_preview: output.stdout.preview.clone(),
        stderr_preview: output.stderr.preview.clone(),
        stdout_bytes: output.stdout.bytes,
        stderr_bytes: output.stderr.bytes,
        stdout_truncated: output.stdout.truncated,
        stderr_truncated: output.stderr.truncated,
        artifact_file: command.input.artifact_file.clone(),
        consumer_target_file: command.input.consumer_target_file.clone(),
        artifact,
        audit: AuditSummary::unavailable(),
        error_code: None,
        error_message: None,
        started_at_ms,
        completed_at_ms,
    }
}

fn apply_failure(
    result: &mut CommandResultSemantic,
    status: &str,
    error_code: &str,
    error_message: &str,
    audit_reason: &str,
) {
    result.status = status.to_string();
    result.audit = AuditSummary::skipped(audit_reason);
    result.error_code = Some(error_code.to_string());
    result.error_message = Some(error_message.to_string());
}

fn cancelled_before_start(
    command: &CommandExecute,
    paths: &ValidatedPaths,
    clock: &dyn ExecutionClock,
) -> CommandResultSemantic {
    cancelled_result(
        command,
        if paths.dry_run {
            "dry_run"
        } else {
            "codex_exec"
        },
        None,
        ArtifactSummary::missing(),
        clock.now_ms(),
    )
}

fn cancelled_result(
    command: &CommandExecute,
    execution_mode: &str,
    started_at_ms: Option<u64>,
    artifact: ArtifactSummary,
    completed_at_ms: u64,
) -> CommandResultSemantic {
    CommandResultSemantic {
        execution_mode: execution_mode.to_string(),
        status: "cancelled".to_string(),
        exit_code: None,
        stdout_preview: String::new(),
        stderr_preview: String::new(),
        stdout_bytes: 0,
        stderr_bytes: 0,
        stdout_truncated: false,
        stderr_truncated: false,
        artifact_file: command.input.artifact_file.clone(),
        consumer_target_file: command.input.consumer_target_file.clone(),
        artifact,
        audit: AuditSummary::skipped("cancelled"),
        error_code: Some("MYFORGE_COMMAND_CANCELLED".to_string()),
        error_message: Some("command was cancelled".to_string()),
        started_at_ms,
        completed_at_ms: started_at_ms
            .map_or(completed_at_ms, |started| completed_at_ms.max(started)),
    }
}

struct ExecutionSettings {
    codex_bin: OsString,
    root_real: PathBuf,
    auditor: Option<AuditorIdentity>,
    dry_run: bool,
    audit_timeout_ms: u64,
    clock_skew_ms: u64,
    environment: Vec<(OsString, OsString)>,
}

struct ValidatedPaths {
    root_real: PathBuf,
    artifact_path: PathBuf,
    rules_path: PathBuf,
    dry_run: bool,
}

fn validate_workspace_paths(
    configured_root_real: &Path,
    command: &CommandExecute,
) -> Result<ValidatedPaths, CommandRejection> {
    validate_relative_path(&command.input.artifact_file, "artifacts/fangyuan/", ".ron")?;
    validate_relative_path(&command.input.rules_file, "rules/fangyuan/", ".md")?;
    if let Some(consumer) = &command.input.consumer_target_file {
        validate_relative_path(consumer, "project/assets/fangyuan/", ".ron")?;
    }

    let root_real = fs::canonicalize(configured_root_real).map_err(|_| {
        CommandRejection::new(
            "MYFORGE_ROOT_MISSING",
            "configured workspace root is unavailable",
            true,
        )
    })?;
    if root_real != configured_root_real
        || !fs::metadata(&root_real).is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(CommandRejection::new(
            "MYFORGE_ROOT_INVALID",
            "configured workspace root is invalid",
            false,
        ));
    }

    let rules_candidate = root_real.join(path_from_wire(&command.input.rules_file));
    let rules_path = match fs::canonicalize(&rules_candidate) {
        Ok(path) => path,
        Err(_) => {
            return Err(CommandRejection::new(
                "MYFORGE_RULES_FILE_MISSING",
                "rules file is unavailable",
                false,
            ));
        }
    };
    if !inside_root(&root_real, &rules_path)
        || !fs::metadata(&rules_path).is_ok_and(|metadata| metadata.is_file())
    {
        return Err(path_rejection());
    }

    let artifact_candidate = root_real.join(path_from_wire(&command.input.artifact_file));
    let Some(parent) = artifact_candidate.parent() else {
        return Err(path_rejection());
    };
    let parent_real = fs::canonicalize(parent).map_err(|_| path_rejection())?;
    if !inside_root(&root_real, &parent_real)
        || !fs::metadata(&parent_real).is_ok_and(|metadata| metadata.is_dir())
    {
        return Err(path_rejection());
    }
    let file_name = artifact_candidate.file_name().ok_or_else(path_rejection)?;
    let artifact_path = parent_real.join(file_name);
    match fs::symlink_metadata(&artifact_path) {
        Ok(_) => {
            let artifact_real = fs::canonicalize(&artifact_path).map_err(|_| path_rejection())?;
            if !inside_root(&root_real, &artifact_real)
                || !fs::metadata(&artifact_real).is_ok_and(|metadata| metadata.is_file())
            {
                return Err(path_rejection());
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(path_rejection()),
    }

    Ok(ValidatedPaths {
        root_real,
        artifact_path,
        rules_path,
        dry_run: false,
    })
}

fn validate_relative_path(value: &str, prefix: &str, suffix: &str) -> Result<(), CommandRejection> {
    let invalid = value.is_empty()
        || value.len() > 512
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains("//")
        || value.contains('\\')
        || value.chars().any(|character| {
            character <= '\u{001f}'
                || character == '\u{007f}'
                || matches!(character, ':' | '"' | '<' | '>' | '|' | '?' | '*')
        })
        || !value.starts_with(prefix)
        || !value.ends_with(suffix)
        || value.split('/').any(|segment| {
            segment.is_empty()
                || matches!(segment, "." | "..")
                || segment.ends_with([' ', '.'])
                || is_windows_device_name(segment)
        });
    if invalid {
        Err(path_rejection())
    } else {
        Ok(())
    }
}

fn is_windows_device_name(segment: &str) -> bool {
    let stem = segment
        .split('.')
        .next()
        .unwrap_or(segment)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|suffix| {
            (suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
                || matches!(suffix, "\u{00b9}" | "\u{00b2}" | "\u{00b3}")
        })
    })
}

fn path_from_wire(value: &str) -> PathBuf {
    value.split('/').collect()
}

fn inside_root(root: &Path, candidate: &Path) -> bool {
    candidate != root && candidate.starts_with(root)
}

fn path_rejection() -> CommandRejection {
    CommandRejection::new(
        "MYFORGE_TARGET_PATH_INVALID",
        "command path is outside the allowed workspace layout",
        false,
    )
}

fn codex_specification(
    settings: &ExecutionSettings,
    command: &CommandExecute,
) -> ProcessSpecification {
    ProcessSpecification {
        program: settings.codex_bin.clone(),
        arguments: vec![
            OsString::from("exec"),
            OsString::from("--sandbox"),
            OsString::from("workspace-write"),
            OsString::from("--ephemeral"),
            OsString::from("--color"),
            OsString::from("never"),
            OsString::from(&command.input.rendered_prompt),
        ],
        working_directory: settings.root_real.clone(),
        environment: settings.environment.clone(),
        stdout_preview_bytes: command.max_output_bytes as usize,
        stderr_preview_bytes: command.max_output_bytes as usize,
        stdout_raw_bytes: None,
    }
}

async fn observe_artifact(
    path: &Path,
    root_real: &Path,
    control: &CommandControl,
    budget: CommandBudget,
) -> Result<ArtifactSummary, ObservationFailure> {
    let metadata =
        match await_observation_io(control, budget, tokio::fs::symlink_metadata(path)).await? {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ArtifactSummary::missing());
            }
            Err(_) => return Err(ObservationFailure::Invalid),
        };
    ensure_execution_budget(control, budget)?;
    if metadata.file_type().is_symlink() {
        let real = await_observation_io(control, budget, tokio::fs::canonicalize(path))
            .await?
            .map_err(|_| ObservationFailure::Invalid)?;
        if !inside_root(root_real, &real) {
            return Err(ObservationFailure::Invalid);
        }
    }
    let real = await_observation_io(control, budget, tokio::fs::canonicalize(path))
        .await?
        .map_err(|_| ObservationFailure::Invalid)?;
    let metadata = await_observation_io(control, budget, tokio::fs::metadata(&real))
        .await?
        .map_err(|_| ObservationFailure::Invalid)?;
    if !inside_root(root_real, &real)
        || !metadata.is_file()
        || metadata.len() > MAX_SAFE_INTEGER as u64
    {
        return Err(ObservationFailure::Invalid);
    }
    let modified_at_ms = system_time_ms(
        metadata
            .modified()
            .map_err(|_| ObservationFailure::Invalid)?,
    )
    .ok_or(ObservationFailure::Invalid)?;
    let mut file = await_observation_io(control, budget, tokio::fs::File::open(&real))
        .await?
        .map_err(|_| ObservationFailure::Invalid)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = await_observation_io(control, budget, file.read(&mut buffer))
            .await?
            .map_err(|_| ObservationFailure::Invalid)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let final_metadata = await_observation_io(control, budget, file.metadata())
        .await?
        .map_err(|_| ObservationFailure::Invalid)?;
    let final_modified = final_metadata
        .modified()
        .map_err(|_| ObservationFailure::Invalid)?;
    if final_metadata.len() != metadata.len()
        || final_modified
            != metadata
                .modified()
                .map_err(|_| ObservationFailure::Invalid)?
        || await_observation_io(control, budget, tokio::fs::canonicalize(path))
            .await?
            .map_err(|_| ObservationFailure::Invalid)?
            != real
    {
        return Err(ObservationFailure::Invalid);
    }
    Ok(ArtifactSummary {
        exists: true,
        sha256: Some(lower_hex(&digest.finalize())),
        bytes: Some(metadata.len()),
        modified_at_ms: Some(modified_at_ms),
    })
}

async fn await_observation_io<T>(
    control: &CommandControl,
    budget: CommandBudget,
    operation: impl Future<Output = io::Result<T>>,
) -> Result<io::Result<T>, ObservationFailure> {
    ensure_execution_budget(control, budget)?;
    tokio::select! {
        biased;
        () = control.cancellation().cancelled() => Err(ObservationFailure::Cancelled),
        () = tokio::time::sleep_until(budget.deadline) => Err(ObservationFailure::TimedOut),
        result = operation => Ok(result),
    }
}

fn ensure_execution_budget(
    control: &CommandControl,
    budget: CommandBudget,
) -> Result<(), ObservationFailure> {
    budget.check(control).map_err(ObservationFailure::from)
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn system_time_ms(time: SystemTime) -> Option<u64> {
    let value: u64 = time
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()?;
    (value <= MAX_SAFE_INTEGER as u64).then_some(value)
}

trait ExecutionClock: Send + Sync {
    fn now_ms(&self) -> u64;
}

fn cancellation_completion_deadline(
    control: &CommandControl,
    command_deadline: tokio::time::Instant,
    clock: &dyn ExecutionClock,
) -> tokio::time::Instant {
    let now = tokio::time::Instant::now();
    let cancel_deadline = control
        .cancel_deadline_at_ms()
        .map_or(now, |deadline_at_ms| {
            now + Duration::from_millis(deadline_at_ms.saturating_sub(clock.now_ms()))
        });
    command_deadline
        .min(now + PIPE_DRAIN_TIMEOUT)
        .min(cancel_deadline)
}

struct SystemExecutionClock;

impl ExecutionClock for SystemExecutionClock {
    fn now_ms(&self) -> u64 {
        system_time_ms(SystemTime::now()).unwrap_or(MAX_SAFE_INTEGER as u64)
    }
}

fn minimal_environment() -> Vec<(OsString, OsString)> {
    ENVIRONMENT_ALLOWLIST
        .iter()
        .filter_map(|name| env::var_os(name).map(|value| (OsString::from(name), value)))
        .collect()
}

struct ProcessSpecification {
    program: OsString,
    arguments: Vec<OsString>,
    working_directory: PathBuf,
    environment: Vec<(OsString, OsString)>,
    stdout_preview_bytes: usize,
    stderr_preview_bytes: usize,
    stdout_raw_bytes: Option<usize>,
}

trait ProcessRunner: Send + Sync {
    fn spawn(&self, specification: ProcessSpecification) -> Result<Box<dyn RunningProcess>, ()>;
}

struct AbortOnDropTask<T>(JoinHandle<T>);

impl<T> AbortOnDropTask<T> {
    fn new(task: JoinHandle<T>) -> Self {
        Self(task)
    }
}

impl<T> Future for AbortOnDropTask<T> {
    type Output = Result<T, tokio::task::JoinError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(context)
    }
}

impl<T> Drop for AbortOnDropTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[async_trait]
trait RunningProcess: Send {
    async fn wait(
        self: Box<Self>,
        control: CommandControl,
        deadline: tokio::time::Instant,
        clock: Arc<dyn ExecutionClock>,
    ) -> ProcessOutput;
}

struct SystemProcessRunner;

impl ProcessRunner for SystemProcessRunner {
    fn spawn(&self, specification: ProcessSpecification) -> Result<Box<dyn RunningProcess>, ()> {
        let mut command = Command::new(&specification.program);
        command
            .args(&specification.arguments)
            .current_dir(&specification.working_directory)
            .env_clear()
            .envs(specification.environment.iter().cloned())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut group = command.group();
        group.kill_on_drop(true);
        let child = group.spawn().map_err(|_| ())?;
        Ok(Box::new(SystemRunningProcess {
            child,
            stdout_preview_bytes: specification.stdout_preview_bytes,
            stderr_preview_bytes: specification.stderr_preview_bytes,
            stdout_raw_bytes: specification.stdout_raw_bytes,
        }))
    }
}

struct SystemRunningProcess {
    child: command_group::AsyncGroupChild,
    stdout_preview_bytes: usize,
    stderr_preview_bytes: usize,
    stdout_raw_bytes: Option<usize>,
}

impl SystemRunningProcess {
    fn force_group_termination(&mut self) {
        let _ = self.child.start_kill();
    }

    async fn reap_group_until(&mut self, deadline: tokio::time::Instant) {
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) if tokio::time::Instant::now() >= deadline => return,
                Ok(None) => {
                    tokio::time::sleep_until(
                        deadline.min(tokio::time::Instant::now() + PROCESS_POLL_INTERVAL),
                    )
                    .await;
                }
            }
        }
    }
}

impl Drop for SystemRunningProcess {
    fn drop(&mut self) {
        self.force_group_termination();
    }
}

#[async_trait]
impl RunningProcess for SystemRunningProcess {
    async fn wait(
        mut self: Box<Self>,
        control: CommandControl,
        deadline: tokio::time::Instant,
        clock: Arc<dyn ExecutionClock>,
    ) -> ProcessOutput {
        let stdout = self.child.inner().stdout.take();
        let stderr = self.child.inner().stderr.take();
        let stdout_task = stdout.map(|stream| {
            AbortOnDropTask::new(tokio::spawn(capture_stream(
                stream,
                self.stdout_preview_bytes,
                self.stdout_raw_bytes,
            )))
        });
        let stderr_task = stderr.map(|stream| {
            AbortOnDropTask::new(tokio::spawn(capture_stream(
                stream,
                self.stderr_preview_bytes,
                None,
            )))
        });
        let (termination, exit_code, forced_deadline) = loop {
            if control.cancellation().is_cancelled() {
                let forced_deadline =
                    cancellation_completion_deadline(&control, deadline, clock.as_ref());
                self.force_group_termination();
                self.reap_group_until(forced_deadline).await;
                break (ProcessTermination::Cancelled, None, Some(forced_deadline));
            }
            if tokio::time::Instant::now() >= deadline {
                self.force_group_termination();
                self.reap_group_until(deadline).await;
                break (ProcessTermination::TimedOut, None, Some(deadline));
            }
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    break (ProcessTermination::Exited, status.code(), None);
                }
                Ok(None) => {}
                Err(_) => {
                    let forced_deadline =
                        deadline.min(tokio::time::Instant::now() + PIPE_DRAIN_TIMEOUT);
                    self.force_group_termination();
                    self.reap_group_until(forced_deadline).await;
                    break (ProcessTermination::IoFailed, None, Some(forced_deadline));
                }
            }
            tokio::select! {
                biased;
                () = control.cancellation().cancelled() => {}
                () = tokio::time::sleep_until(deadline) => {}
                () = tokio::time::sleep(PROCESS_POLL_INTERVAL) => {}
            }
        };
        drop(self);
        let pipe_deadline = tokio::time::Instant::now() + PIPE_DRAIN_TIMEOUT;
        let capture_deadline = forced_deadline.unwrap_or_else(|| deadline.min(pipe_deadline));
        let deadline_is_process_deadline = capture_deadline == deadline;
        let (stdout, stdout_failure) = finish_capture(stdout_task, capture_deadline).await;
        let (stderr, stderr_failure) = finish_capture(stderr_task, capture_deadline).await;
        let capture_failed =
            stdout_failure != CaptureFailure::None || stderr_failure != CaptureFailure::None;
        let capture_timed_out = stdout_failure == CaptureFailure::Deadline
            || stderr_failure == CaptureFailure::Deadline;
        ProcessOutput {
            termination: match termination {
                ProcessTermination::Exited if capture_timed_out && deadline_is_process_deadline => {
                    ProcessTermination::TimedOut
                }
                ProcessTermination::Exited if capture_failed => ProcessTermination::IoFailed,
                termination => termination,
            },
            exit_code,
            stdout,
            stderr,
        }
    }
}

async fn finish_capture(
    task: Option<AbortOnDropTask<StreamCapture>>,
    deadline: tokio::time::Instant,
) -> (StreamCapture, CaptureFailure) {
    let Some(mut task) = task else {
        return (StreamCapture::empty(), CaptureFailure::None);
    };
    match tokio::time::timeout_at(deadline, &mut task).await {
        Ok(Ok(capture)) => {
            let failure = if capture.read_failed {
                CaptureFailure::Read
            } else {
                CaptureFailure::None
            };
            (capture, failure)
        }
        Ok(Err(_)) => (StreamCapture::empty(), CaptureFailure::Read),
        Err(_) => (StreamCapture::empty(), CaptureFailure::Deadline),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureFailure {
    None,
    Read,
    Deadline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessTermination {
    Exited,
    TimedOut,
    Cancelled,
    IoFailed,
}

struct ProcessOutput {
    termination: ProcessTermination,
    exit_code: Option<i32>,
    stdout: StreamCapture,
    stderr: StreamCapture,
}

#[derive(Clone)]
struct StreamCapture {
    preview: String,
    bytes: u64,
    truncated: bool,
    raw: Option<Vec<u8>>,
    raw_truncated: bool,
    read_failed: bool,
}

impl StreamCapture {
    fn empty() -> Self {
        Self {
            preview: String::new(),
            bytes: 0,
            truncated: false,
            raw: None,
            raw_truncated: false,
            read_failed: false,
        }
    }
}

async fn capture_stream(
    mut stream: impl AsyncRead + Unpin,
    preview_limit: usize,
    raw_limit: Option<usize>,
) -> StreamCapture {
    let mut capture = Utf8Capture::new(preview_limit, raw_limit);
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => capture.push(&buffer[..read]),
            Err(_) => {
                capture.read_failed = true;
                break;
            }
        }
    }
    capture.finish()
}

struct Utf8Capture {
    preview: String,
    bytes: u64,
    preview_limit: usize,
    preview_closed: bool,
    pending: Vec<u8>,
    raw: Option<Vec<u8>>,
    raw_limit: Option<usize>,
    raw_truncated: bool,
    read_failed: bool,
}

impl Utf8Capture {
    fn new(preview_limit: usize, raw_limit: Option<usize>) -> Self {
        Self {
            preview: String::new(),
            bytes: 0,
            preview_limit,
            preview_closed: false,
            pending: Vec::new(),
            raw: raw_limit.map(|limit| Vec::with_capacity(limit.min(64 * 1024))),
            raw_limit,
            raw_truncated: false,
            read_failed: false,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if let (Some(raw), Some(limit)) = (&mut self.raw, self.raw_limit) {
            let available = limit.saturating_sub(raw.len());
            raw.extend_from_slice(&bytes[..bytes.len().min(available)]);
            self.raw_truncated |= bytes.len() > available;
        }
        self.pending.extend_from_slice(bytes);
        self.decode_pending(false);
    }

    fn decode_pending(&mut self, eof: bool) {
        let mut consumed = 0;
        while consumed < self.pending.len() {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(valid) => {
                    let valid = valid.to_owned();
                    self.record(&valid);
                    consumed = self.pending.len();
                }
                Err(error) => {
                    let valid_end = consumed + error.valid_up_to();
                    if valid_end > consumed {
                        let valid = String::from_utf8_lossy(&self.pending[consumed..valid_end])
                            .into_owned();
                        self.record(&valid);
                    }
                    match error.error_len() {
                        Some(length) => {
                            self.record("\u{fffd}");
                            consumed = valid_end + length;
                        }
                        None if eof => {
                            self.record("\u{fffd}");
                            consumed = self.pending.len();
                        }
                        None => {
                            consumed = valid_end;
                            break;
                        }
                    }
                }
            }
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
        }
    }

    fn record(&mut self, value: &str) {
        self.bytes = self
            .bytes
            .saturating_add(value.len() as u64)
            .min(MAX_SAFE_INTEGER as u64);
        if self.preview_closed {
            return;
        }
        for character in value.chars() {
            let character_bytes = character.len_utf8();
            if self.preview.len().saturating_add(character_bytes) > self.preview_limit {
                self.preview_closed = true;
                break;
            }
            self.preview.push(character);
        }
    }

    fn finish(mut self) -> StreamCapture {
        self.decode_pending(true);
        StreamCapture {
            truncated: self.preview_closed
                || self.bytes > self.preview.len() as u64
                || self.read_failed,
            preview: self.preview,
            bytes: self.bytes,
            raw: self.raw,
            raw_truncated: self.raw_truncated || self.read_failed,
            read_failed: self.read_failed,
        }
    }
}

fn capture_string(value: &str, limit: usize) -> StreamCapture {
    let mut capture = Utf8Capture::new(limit, None);
    capture.push(value.as_bytes());
    capture.finish()
}

enum AuditOutcome {
    Summary(AuditSummary),
    Cancelled,
    CommandTimedOut,
}

fn audit_budget_outcome(budget: CommandBudget, control: &CommandControl) -> Option<AuditOutcome> {
    match budget.check(control) {
        Err(BudgetStop::Cancelled) => Some(AuditOutcome::Cancelled),
        Err(BudgetStop::TimedOut) => Some(AuditOutcome::CommandTimedOut),
        Ok(()) => None,
    }
}

async fn run_audit(
    command: &CommandExecute,
    paths: &ValidatedPaths,
    control: CommandControl,
    budget: CommandBudget,
    settings: &ExecutionSettings,
    runner: Arc<dyn ProcessRunner>,
    clock: Arc<dyn ExecutionClock>,
) -> AuditOutcome {
    if let Some(outcome) = audit_budget_outcome(budget, &control) {
        return outcome;
    }
    let Some(auditor) = settings.auditor.as_ref() else {
        return AuditOutcome::Summary(AuditSummary::unavailable());
    };
    let auditor_real = match verify_audit_inputs(command, paths, auditor, &control, budget).await {
        Ok(real) => real,
        Err(ObservationFailure::Cancelled) => return AuditOutcome::Cancelled,
        Err(ObservationFailure::TimedOut) => return AuditOutcome::CommandTimedOut,
        Err(ObservationFailure::Invalid) => {
            return AuditOutcome::Summary(failed_audit(
                "auditor_spawn_failed",
                "configured auditor is unavailable",
            ));
        }
    };
    if let Some(outcome) = audit_budget_outcome(budget, &control) {
        return outcome;
    }
    let specification = ProcessSpecification {
        program: auditor_real.into_os_string(),
        arguments: vec![
            OsString::from("--format"),
            OsString::from("json"),
            OsString::from("--rules"),
            OsString::from(&command.input.rules_file),
            OsString::from("--artifact"),
            OsString::from(&command.input.artifact_file),
        ],
        working_directory: settings.root_real.clone(),
        environment: settings.environment.clone(),
        stdout_preview_bytes: AUDITOR_STDOUT_LIMIT,
        stderr_preview_bytes: AUDITOR_DIAGNOSTIC_LIMIT,
        stdout_raw_bytes: Some(AUDITOR_STDOUT_LIMIT),
    };
    let local_deadline =
        tokio::time::Instant::now() + Duration::from_millis(settings.audit_timeout_ms);
    let audit_deadline = budget.deadline.min(local_deadline);
    let command_deadline_is_limit = budget.deadline <= local_deadline;
    let process = match runner.spawn(specification) {
        Ok(process) => process,
        Err(()) => {
            return AuditOutcome::Summary(failed_audit(
                "auditor_spawn_failed",
                "auditor could not be started",
            ));
        }
    };
    if let Some(outcome) = audit_budget_outcome(budget, &control) {
        drop(process);
        return outcome;
    }
    let output = process.wait(control.clone(), audit_deadline, clock).await;
    tracing::debug!(
        audit_stderr_bytes = output.stderr.bytes,
        audit_stderr_truncated = output.stderr.truncated,
        "local auditor completed"
    );
    if let Some(outcome) = audit_budget_outcome(budget, &control) {
        return outcome;
    }
    match output.termination {
        ProcessTermination::TimedOut if command_deadline_is_limit => {
            return AuditOutcome::CommandTimedOut;
        }
        ProcessTermination::TimedOut => {
            return AuditOutcome::Summary(failed_audit(
                "auditor_timeout",
                "auditor exceeded its time limit",
            ));
        }
        ProcessTermination::Cancelled => return AuditOutcome::Cancelled,
        ProcessTermination::IoFailed => {
            return AuditOutcome::Summary(failed_audit(
                "auditor_exit_failed",
                "auditor did not complete cleanly",
            ));
        }
        ProcessTermination::Exited if output.exit_code != Some(0) => {
            return AuditOutcome::Summary(failed_audit(
                "auditor_exit_failed",
                "auditor exited unsuccessfully",
            ));
        }
        ProcessTermination::Exited => {}
    }
    if output.stdout.raw_truncated {
        return AuditOutcome::Summary(failed_audit(
            "auditor_output_invalid",
            "auditor output is invalid",
        ));
    }
    let Some(raw) = output.stdout.raw.as_deref() else {
        return AuditOutcome::Summary(failed_audit(
            "auditor_output_invalid",
            "auditor output is invalid",
        ));
    };
    let summary = parse_audit_output(raw)
        .unwrap_or_else(|| failed_audit("auditor_output_invalid", "auditor output is invalid"));
    audit_budget_outcome(budget, &control).unwrap_or(AuditOutcome::Summary(summary))
}

async fn verify_audit_inputs(
    command: &CommandExecute,
    paths: &ValidatedPaths,
    auditor: &AuditorIdentity,
    control: &CommandControl,
    budget: CommandBudget,
) -> Result<PathBuf, ObservationFailure> {
    let root_real =
        await_observation_io(control, budget, tokio::fs::canonicalize(&paths.root_real))
            .await?
            .map_err(|_| ObservationFailure::Invalid)?;
    let root_metadata = await_observation_io(control, budget, tokio::fs::metadata(&root_real))
        .await?
        .map_err(|_| ObservationFailure::Invalid)?;
    if root_real != paths.root_real || !root_metadata.is_dir() {
        return Err(ObservationFailure::Invalid);
    }

    verify_regular_path(
        &paths
            .root_real
            .join(path_from_wire(&command.input.rules_file)),
        &paths.rules_path,
        &paths.root_real,
        control,
        budget,
    )
    .await?;
    verify_regular_path(
        &paths
            .root_real
            .join(path_from_wire(&command.input.artifact_file)),
        &paths.artifact_path,
        &paths.root_real,
        control,
        budget,
    )
    .await?;

    let link_metadata =
        await_observation_io(control, budget, tokio::fs::symlink_metadata(&auditor.path))
            .await?
            .map_err(|_| ObservationFailure::Invalid)?;
    if link_metadata.file_type().is_symlink() {
        return Err(ObservationFailure::Invalid);
    }
    let real = await_observation_io(control, budget, tokio::fs::canonicalize(&auditor.path))
        .await?
        .map_err(|_| ObservationFailure::Invalid)?;
    let metadata = await_observation_io(control, budget, tokio::fs::metadata(&real))
        .await?
        .map_err(|_| ObservationFailure::Invalid)?;
    if real != auditor.path
        || !inside_root(&paths.root_real, &real)
        || !metadata.is_file()
        || !is_executable_file(&metadata)
        || metadata.len() != auditor.bytes
        || metadata
            .modified()
            .map_err(|_| ObservationFailure::Invalid)?
            != auditor.modified
    {
        return Err(ObservationFailure::Invalid);
    }
    let modified = metadata
        .modified()
        .map_err(|_| ObservationFailure::Invalid)?;

    let mut file = await_observation_io(control, budget, tokio::fs::File::open(&real))
        .await?
        .map_err(|_| ObservationFailure::Invalid)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = await_observation_io(control, budget, file.read(&mut buffer))
            .await?
            .map_err(|_| ObservationFailure::Invalid)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let final_metadata = await_observation_io(control, budget, file.metadata())
        .await?
        .map_err(|_| ObservationFailure::Invalid)?;
    let final_real = await_observation_io(control, budget, tokio::fs::canonicalize(&auditor.path))
        .await?
        .map_err(|_| ObservationFailure::Invalid)?;
    let actual_digest: [u8; 32] = digest.finalize().into();
    ensure_execution_budget(control, budget)?;
    if final_real != real
        || final_metadata.len() != metadata.len()
        || final_metadata
            .modified()
            .map_err(|_| ObservationFailure::Invalid)?
            != modified
        || actual_digest != auditor.sha256
    {
        return Err(ObservationFailure::Invalid);
    }
    Ok(real)
}

async fn verify_regular_path(
    candidate: &Path,
    expected_real: &Path,
    root_real: &Path,
    control: &CommandControl,
    budget: CommandBudget,
) -> Result<(), ObservationFailure> {
    let link_metadata =
        await_observation_io(control, budget, tokio::fs::symlink_metadata(candidate)).await?;
    let link_metadata = link_metadata.map_err(|_| ObservationFailure::Invalid)?;
    if link_metadata.file_type().is_symlink() {
        return Err(ObservationFailure::Invalid);
    }
    let real = await_observation_io(control, budget, tokio::fs::canonicalize(candidate))
        .await?
        .map_err(|_| ObservationFailure::Invalid)?;
    let metadata = await_observation_io(control, budget, tokio::fs::metadata(&real))
        .await?
        .map_err(|_| ObservationFailure::Invalid)?;
    if real != expected_real || !inside_root(root_real, &real) || !metadata.is_file() {
        return Err(ObservationFailure::Invalid);
    }
    ensure_execution_budget(control, budget)
}

#[cfg(unix)]
fn is_executable_file(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
fn is_executable_file(_metadata: &fs::Metadata) -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuditorDocument {
    status: String,
    errors: u64,
    warnings: u64,
    primitive_count: Option<u64>,
    main_code: Option<String>,
    findings: Vec<AuditorFinding>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AuditorFinding {
    severity: String,
    code: String,
    field_path: String,
    message: String,
}

fn parse_audit_output(raw: &[u8]) -> Option<AuditSummary> {
    let text = std::str::from_utf8(raw).ok()?;
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    const FIELDS: &[&str] = &[
        "status",
        "errors",
        "warnings",
        "primitiveCount",
        "mainCode",
        "findings",
    ];
    if object.len() != FIELDS.len() || FIELDS.iter().any(|field| !object.contains_key(*field)) {
        return None;
    }
    let document: AuditorDocument = serde_json::from_str(text).ok()?;
    if document.errors > MAX_SAFE_INTEGER as u64
        || document.warnings > MAX_SAFE_INTEGER as u64
        || document
            .primitive_count
            .is_some_and(|value| value > MAX_SAFE_INTEGER as u64)
        || !matches!(document.status.as_str(), "passed" | "warning" | "failed")
    {
        return None;
    }
    if (document.status == "passed" && document.main_code.is_some())
        || (document.status != "passed"
            && (document
                .main_code
                .as_deref()
                .is_none_or(|code| !valid_audit_code(code))
                || document.findings.is_empty()))
        || document.findings.iter().any(|finding| {
            !matches!(finding.severity.as_str(), "info" | "warning" | "error")
                || !valid_audit_code(&finding.code)
                || !valid_audit_text(&finding.field_path, 256)
                || !valid_audit_text(&finding.message, 512)
        })
    {
        return None;
    }
    let findings = document
        .findings
        .into_iter()
        .take(20)
        .map(|finding| AuditFinding {
            severity: finding.severity,
            code: finding.code,
            field_path: finding.field_path,
            message: finding.message,
        })
        .collect::<Vec<_>>();
    let summary = AuditSummary {
        status: document.status,
        errors: Some(document.errors),
        warnings: Some(document.warnings),
        primitive_count: document.primitive_count,
        main_code: document.main_code,
        reason_code: None,
        findings_preview: findings,
    };
    let probe = CommandResultSemantic {
        execution_mode: "codex_exec".to_string(),
        status: if summary.status == "passed" {
            "completed".to_string()
        } else {
            "completed_with_errors".to_string()
        },
        exit_code: Some(0),
        stdout_preview: String::new(),
        stderr_preview: String::new(),
        stdout_bytes: 0,
        stderr_bytes: 0,
        stdout_truncated: false,
        stderr_truncated: false,
        artifact_file: "artifacts/fangyuan/probe.ron".to_string(),
        consumer_target_file: None,
        artifact: ArtifactSummary {
            exists: true,
            sha256: Some("0".repeat(64)),
            bytes: Some(0),
            modified_at_ms: Some(0),
        },
        audit: summary.clone(),
        error_code: (summary.status != "passed").then(|| {
            if summary.status == "warning" {
                "FANGYUAN_BLUEPRINT_AUDIT_WARNING".to_string()
            } else {
                "FANGYUAN_BLUEPRINT_AUDIT_FAILED".to_string()
            }
        }),
        error_message: (summary.status != "passed").then(|| "audit result".to_string()),
        started_at_ms: Some(0),
        completed_at_ms: 0,
    };
    probe.validate(4_096).ok().map(|_| summary)
}

fn valid_audit_code(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && (value.as_bytes()[0].is_ascii_lowercase() || value.as_bytes()[0].is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.' | b'-')
        })
}

fn valid_audit_text(value: &str, maximum: usize) -> bool {
    (1..=maximum).contains(&value.len())
        && !value
            .chars()
            .any(|character| character <= '\u{001f}' || character == '\u{007f}')
}

fn failed_audit(main_code: &str, message: &str) -> AuditSummary {
    AuditSummary {
        status: "failed".to_string(),
        errors: Some(1),
        warnings: Some(0),
        primitive_count: None,
        main_code: Some(main_code.to_string()),
        reason_code: None,
        findings_preview: vec![AuditFinding {
            severity: "error".to_string(),
            code: main_code.to_string(),
            field_path: "$".to_string(),
            message: message.to_string(),
        }],
    }
}

fn apply_artifact_freshness(
    audit: &mut AuditSummary,
    artifact: &ArtifactSummary,
    started_at_ms: u64,
    clock_skew_ms: u64,
) {
    let minimum = started_at_ms.saturating_sub(clock_skew_ms);
    if artifact
        .modified_at_ms
        .is_none_or(|modified_at_ms| modified_at_ms >= minimum)
    {
        return;
    }
    let finding = AuditFinding {
        severity: "warning".to_string(),
        code: "artifact.stale".to_string(),
        field_path: "artifact.modifiedAtMs".to_string(),
        message: "artifact modification time predates this execution".to_string(),
    };
    match audit.status.as_str() {
        "passed" | "unavailable" => {
            *audit = AuditSummary {
                status: "warning".to_string(),
                errors: Some(0),
                warnings: Some(1),
                primitive_count: audit.primitive_count,
                main_code: Some("artifact_stale".to_string()),
                reason_code: None,
                findings_preview: vec![finding],
            };
        }
        "warning" | "failed" => {
            audit.warnings = audit
                .warnings
                .map(|value| value.saturating_add(1).min(MAX_SAFE_INTEGER as u64));
            if audit.findings_preview.len() < 20 {
                audit.findings_preview.push(finding);
            } else if let Some(last) = audit.findings_preview.last_mut() {
                *last = finding;
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::Write;
    use std::process::Command as StdCommand;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    use crate::command::CommandCancellation;
    use crate::schemas::{BlueprintBounds, BlueprintPrompt, CommandInput};
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::*;

    struct FixedClock(u64);

    impl ExecutionClock for FixedClock {
        fn now_ms(&self) -> u64 {
            self.0
        }
    }

    struct SharedClock(Arc<AtomicU64>);

    impl ExecutionClock for SharedClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    struct FakeRunningProcess {
        output: Option<ProcessOutput>,
    }

    #[async_trait]
    impl RunningProcess for FakeRunningProcess {
        async fn wait(
            mut self: Box<Self>,
            control: CommandControl,
            _deadline: tokio::time::Instant,
            _clock: Arc<dyn ExecutionClock>,
        ) -> ProcessOutput {
            let mut output = self.output.take().unwrap();
            if control.cancellation().is_cancelled() {
                output.termination = ProcessTermination::Cancelled;
                output.exit_code = None;
            }
            output
        }
    }

    struct RecordingRunner {
        specifications: Mutex<Vec<ProcessSpecification>>,
        outputs: Mutex<VecDeque<Result<ProcessOutput, ()>>>,
    }

    impl RecordingRunner {
        fn new(outputs: impl IntoIterator<Item = Result<ProcessOutput, ()>>) -> Self {
            Self {
                specifications: Mutex::new(Vec::new()),
                outputs: Mutex::new(outputs.into_iter().collect()),
            }
        }
    }

    impl ProcessRunner for RecordingRunner {
        fn spawn(
            &self,
            specification: ProcessSpecification,
        ) -> Result<Box<dyn RunningProcess>, ()> {
            self.specifications.lock().unwrap().push(specification);
            let output = self
                .outputs
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(()))?;
            Ok(Box::new(FakeRunningProcess {
                output: Some(output),
            }))
        }
    }

    enum TimedBehavior {
        Delay(Duration),
        UntilDeadline,
        CancelWithoutDeadline,
        Pending,
    }

    struct TimedPlan {
        behavior: TimedBehavior,
        output: ProcessOutput,
    }

    struct TimedRunner {
        specifications: Mutex<Vec<ProcessSpecification>>,
        plans: Mutex<VecDeque<TimedPlan>>,
        observations: Arc<Mutex<Vec<(tokio::time::Instant, Duration)>>>,
        entered: Arc<AtomicBool>,
        drops: Arc<AtomicUsize>,
    }

    impl TimedRunner {
        fn new(plans: impl IntoIterator<Item = TimedPlan>) -> Self {
            Self {
                specifications: Mutex::new(Vec::new()),
                plans: Mutex::new(plans.into_iter().collect()),
                observations: Arc::new(Mutex::new(Vec::new())),
                entered: Arc::new(AtomicBool::new(false)),
                drops: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl ProcessRunner for TimedRunner {
        fn spawn(
            &self,
            specification: ProcessSpecification,
        ) -> Result<Box<dyn RunningProcess>, ()> {
            self.specifications.lock().unwrap().push(specification);
            let plan = self.plans.lock().unwrap().pop_front().ok_or(())?;
            Ok(Box::new(TimedRunningProcess {
                plan: Some(plan),
                observations: self.observations.clone(),
                entered: self.entered.clone(),
                drops: self.drops.clone(),
            }))
        }
    }

    struct TimedRunningProcess {
        plan: Option<TimedPlan>,
        observations: Arc<Mutex<Vec<(tokio::time::Instant, Duration)>>>,
        entered: Arc<AtomicBool>,
        drops: Arc<AtomicUsize>,
    }

    impl Drop for TimedRunningProcess {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl RunningProcess for TimedRunningProcess {
        async fn wait(
            mut self: Box<Self>,
            control: CommandControl,
            deadline: tokio::time::Instant,
            _clock: Arc<dyn ExecutionClock>,
        ) -> ProcessOutput {
            self.entered.store(true, Ordering::SeqCst);
            self.observations.lock().unwrap().push((
                deadline,
                deadline.saturating_duration_since(tokio::time::Instant::now()),
            ));
            let mut plan = self.plan.take().unwrap();
            match plan.behavior {
                TimedBehavior::Delay(duration) => tokio::time::sleep(duration).await,
                TimedBehavior::UntilDeadline => {
                    tokio::time::sleep_until(deadline).await;
                    plan.output.termination = ProcessTermination::TimedOut;
                    plan.output.exit_code = None;
                }
                TimedBehavior::CancelWithoutDeadline => control.cancellation().cancel(),
                TimedBehavior::Pending => std::future::pending::<()>().await,
            }
            plan.output
        }
    }

    struct ClockAdvancingRunner {
        inner: RecordingRunner,
        clock: Arc<AtomicU64>,
        after_spawn_ms: u64,
    }

    impl ProcessRunner for ClockAdvancingRunner {
        fn spawn(
            &self,
            specification: ProcessSpecification,
        ) -> Result<Box<dyn RunningProcess>, ()> {
            let process = self.inner.spawn(specification)?;
            self.clock.store(self.after_spawn_ms, Ordering::SeqCst);
            Ok(process)
        }
    }

    struct Fixture {
        _directory: TempDir,
        root: PathBuf,
        rules: PathBuf,
        artifact: PathBuf,
        auditor: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path().join("forge");
            let rules_directory = root.join("rules/fangyuan");
            let artifact_directory = root.join("artifacts/fangyuan");
            let tools_directory = root.join("tools");
            fs::create_dir_all(&rules_directory).unwrap();
            fs::create_dir_all(&artifact_directory).unwrap();
            fs::create_dir_all(&tools_directory).unwrap();
            let rules = rules_directory.join("rules.md");
            let artifact = artifact_directory.join("result.ron");
            let auditor = tools_directory.join("audit-test");
            fs::write(&rules, "rules").unwrap();
            fs::write(&auditor, "fixture").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                fs::set_permissions(&auditor, fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self {
                _directory: directory,
                root: fs::canonicalize(root).unwrap(),
                rules,
                artifact,
                auditor,
            }
        }

        fn settings(&self, dry_run: bool, audit: bool) -> ExecutionSettings {
            ExecutionSettings {
                codex_bin: OsString::from("codex-fixture"),
                root_real: self.root.clone(),
                auditor: audit.then(|| {
                    let path = fs::canonicalize(&self.auditor).unwrap();
                    let content = fs::read(&path).unwrap();
                    AuditorIdentity {
                        path,
                        sha256: Sha256::digest(&content).into(),
                        bytes: content.len() as u64,
                        modified: fs::metadata(&self.auditor).unwrap().modified().unwrap(),
                    }
                }),
                dry_run,
                audit_timeout_ms: 1_000,
                clock_skew_ms: 5_000,
                environment: vec![
                    (OsString::from("PATH"), OsString::from("safe-path")),
                    (OsString::from("HOME"), OsString::from("safe-home")),
                ],
            }
        }
    }

    fn command(timestamp_ms: u64) -> CommandExecute {
        CommandExecute {
            protocol_version: 1,
            message_type: "command.execute".to_string(),
            connection_id: "67da7da9-a653-4d6e-9e81-f5f8baf874bb".to_string(),
            request_id: "2d0465b1-dc92-46d2-bc45-c90ed9724f5a".to_string(),
            task_type: "fangyuan.blueprint.generate".to_string(),
            agent_id: "dev-pc-001".to_string(),
            project_id: "myforge-local".to_string(),
            profile: "codex_exec".to_string(),
            input: CommandInput {
                artifact_file: "artifacts/fangyuan/result.ron".to_string(),
                consumer_target_file: Some("project/assets/fangyuan/result.ron".to_string()),
                rules_file: "rules/fangyuan/rules.md".to_string(),
                prompt: BlueprintPrompt {
                    theme: "test".to_string(),
                    primitive_limit: 10,
                    bounds: BlueprintBounds {
                        width: 10,
                        depth: 10,
                        height: 10,
                    },
                    requirements: vec!["safe".to_string()],
                },
                rendered_prompt: "rendered prompt with spaces; no shell parsing".to_string(),
            },
            timeout_ms: 5_000,
            max_output_bytes: 4_096,
            timestamp_ms,
            expires_at_ms: timestamp_ms + 60_000,
            nonce: "nonce".to_string(),
            signature: "signature".to_string(),
        }
    }

    fn captured(value: &[u8], limit: usize, raw: bool) -> StreamCapture {
        let mut capture = Utf8Capture::new(limit, raw.then_some(limit));
        capture.push(value);
        capture.finish()
    }

    fn process_output(
        termination: ProcessTermination,
        exit_code: Option<i32>,
        stdout: &[u8],
        stderr: &[u8],
    ) -> ProcessOutput {
        ProcessOutput {
            termination,
            exit_code,
            stdout: captured(stdout, 4_096, false),
            stderr: captured(stderr, 4_096, false),
        }
    }

    fn auditor_output(value: &[u8], exit_code: i32) -> ProcessOutput {
        ProcessOutput {
            termination: ProcessTermination::Exited,
            exit_code: Some(exit_code),
            stdout: captured(value, AUDITOR_STDOUT_LIMIT, true),
            stderr: captured(b"local diagnostic", AUDITOR_DIAGNOSTIC_LIMIT, false),
        }
    }

    async fn finish(outcome: CommandHandlerOutcome) -> (Option<u64>, CommandResultSemantic) {
        match outcome {
            CommandHandlerOutcome::Started(execution) => {
                let started = execution.started_at_ms();
                match execution.finish().await {
                    StartedExecutionOutcome::Result(result) => (Some(started), *result),
                    StartedExecutionOutcome::FailClosed { reason } => {
                        panic!("unexpected fail-closed completion: {reason}")
                    }
                }
            }
            CommandHandlerOutcome::CompletedBeforeStart(result) => (None, *result),
            _ => panic!("expected a command result"),
        }
    }

    #[test]
    fn lossy_capture_counts_replacement_bytes_and_preserves_character_boundary() {
        let mut capture = Utf8Capture::new(5, None);
        capture.push(&[b'a', 0xf0, 0x9f]);
        capture.push(&[0x92, 0xa9, 0xff, b'z']);
        let capture = capture.finish();
        assert_eq!(capture.preview, "a💩");
        assert_eq!(capture.bytes, 9);
        assert!(capture.truncated);
    }

    #[test]
    fn reserved_windows_names_are_rejected_on_every_platform() {
        for value in [
            "artifacts/fangyuan/CON.ron",
            "artifacts/fangyuan/aux/file.ron",
            "artifacts/fangyuan/LPT9.ron",
            "artifacts/fangyuan/CONOUT$.ron",
            "artifacts/fangyuan/COM\u{00b9}.ron",
        ] {
            assert!(validate_relative_path(value, "artifacts/fangyuan/", ".ron").is_err());
        }
        for value in [
            "/artifacts/fangyuan/result.ron",
            "C:/artifacts/fangyuan/result.ron",
            "artifacts\\fangyuan\\result.ron",
            "artifacts/fangyuan//result.ron",
            "artifacts/fangyuan/../result.ron",
        ] {
            assert!(
                validate_relative_path(value, "artifacts/fangyuan/", ".ron").is_err(),
                "unsafe path accepted: {value}"
            );
        }
    }

    #[test]
    fn strict_auditor_schema_accepts_only_node_compatible_results() {
        let valid = br#"{"status":"warning","errors":0,"warnings":1,"primitiveCount":3,"mainCode":"audit.warning","findings":[{"severity":"warning","code":"audit.warning","fieldPath":"root","message":"warning"}]}"#;
        assert_eq!(parse_audit_output(valid).unwrap().status, "warning");
        let unknown = br#"{"status":"passed","errors":0,"warnings":0,"primitiveCount":3,"mainCode":null,"findings":[],"extra":true}"#;
        assert!(parse_audit_output(unknown).is_none());
        let invalid_utf8 = [b'{', 0xff, b'}'];
        assert!(parse_audit_output(&invalid_utf8).is_none());

        let findings = (0..21)
            .map(|index| {
                serde_json::json!({
                    "severity": "warning",
                    "code": format!("audit.warning.{index}"),
                    "fieldPath": "root",
                    "message": "warning"
                })
            })
            .collect::<Vec<_>>();
        let capped = serde_json::to_vec(&serde_json::json!({
            "status": "warning",
            "errors": 0,
            "warnings": 21,
            "primitiveCount": null,
            "mainCode": "audit.warning",
            "findings": findings
        }))
        .unwrap();
        assert_eq!(
            parse_audit_output(&capped).unwrap().findings_preview.len(),
            20
        );
    }

    #[tokio::test]
    async fn codex_profile_uses_exact_direct_process_contract_and_allowlisted_environment() {
        let fixture = Fixture::new();
        fs::write(&fixture.artifact, "artifact").unwrap();
        let runner = Arc::new(RecordingRunner::new([Ok(process_output(
            ProcessTermination::Exited,
            Some(0),
            b"done",
            b"",
        ))]));
        let clock = Arc::new(FixedClock(system_time_ms(SystemTime::now()).unwrap()));
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(false, false),
            runner.clone(),
            clock,
        );
        let command = command(system_time_ms(SystemTime::now()).unwrap());
        let control = CommandControl::new(CancellationToken::new(), command.timestamp_ms);
        let (_, result) = finish(handler.execute(command.clone(), control).await).await;
        assert_eq!(result.status, "completed");
        assert_eq!(result.audit.status, "unavailable");
        assert_eq!(
            result.audit.reason_code.as_deref(),
            Some("auditor_not_configured")
        );

        let specifications = runner.specifications.lock().unwrap();
        assert_eq!(specifications.len(), 1);
        let specification = &specifications[0];
        assert_eq!(specification.program, OsString::from("codex-fixture"));
        assert_eq!(
            specification.arguments,
            vec![
                "exec",
                "--sandbox",
                "workspace-write",
                "--ephemeral",
                "--color",
                "never",
                command.input.rendered_prompt.as_str(),
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
        assert_eq!(specification.working_directory, fixture.root);
        assert_eq!(
            specification.environment,
            vec![
                (OsString::from("PATH"), OsString::from("safe-path")),
                (OsString::from("HOME"), OsString::from("safe-home")),
            ]
        );
        assert!(
            specification
                .environment
                .iter()
                .all(|(name, _)| !name.to_string_lossy().starts_with("MYFORGE_"))
        );
    }

    #[tokio::test]
    async fn successful_spawn_never_returns_a_prestart_expiry_error() {
        let fixture = Fixture::new();
        fs::write(&fixture.artifact, "artifact").unwrap();
        let initial_ms = system_time_ms(SystemTime::now()).unwrap();
        let command = command(initial_ms);
        let after_spawn_ms = command.expires_at_ms + 5_001;
        let shared_clock = Arc::new(AtomicU64::new(initial_ms));
        let runner = Arc::new(ClockAdvancingRunner {
            inner: RecordingRunner::new([Ok(process_output(
                ProcessTermination::Exited,
                Some(0),
                b"done",
                b"",
            ))]),
            clock: shared_clock.clone(),
            after_spawn_ms,
        });
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(false, false),
            runner.clone(),
            Arc::new(SharedClock(shared_clock)),
        );

        let outcome = handler
            .execute(
                command.clone(),
                CommandControl::new(CancellationToken::new(), initial_ms),
            )
            .await;
        let (started_at_ms, result) = finish(outcome).await;

        assert_eq!(started_at_ms, Some(initial_ms));
        assert_eq!(result.started_at_ms, Some(initial_ms));
        assert_eq!(result.completed_at_ms, after_spawn_ms);
        assert_eq!(result.status, "completed");
        assert_eq!(runner.inner.specifications.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn dry_run_is_read_only_observes_artifact_and_never_starts_a_process() {
        let fixture = Fixture::new();
        fs::write(&fixture.artifact, "existing artifact").unwrap();
        let before = fs::read(&fixture.artifact).unwrap();
        let runner = Arc::new(RecordingRunner::new([]));
        let now = system_time_ms(SystemTime::now()).unwrap();
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(true, true),
            runner.clone(),
            Arc::new(FixedClock(now)),
        );
        let command = command(now);
        let control = CommandControl::new(CancellationToken::new(), now);
        let (started, result) = finish(handler.execute(command.clone(), control).await).await;
        assert_eq!(started, Some(now));
        assert_eq!(result.execution_mode, "dry_run");
        assert_eq!(result.status, "completed");
        assert_eq!(result.audit.reason_code.as_deref(), Some("dry_run"));
        assert!(result.artifact.exists);
        assert_eq!(result.artifact.bytes, Some(before.len() as u64));
        assert!(result.stdout_preview.starts_with("DRY_RUN_OK requestId="));
        assert_eq!(fs::read(&fixture.artifact).unwrap(), before);
        assert!(runner.specifications.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn prestart_path_checks_distinguish_missing_rules_and_reject_escape() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.rules).unwrap();
        let runner = Arc::new(RecordingRunner::new([]));
        let now = system_time_ms(SystemTime::now()).unwrap();
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(false, false),
            runner.clone(),
            Arc::new(FixedClock(now)),
        );
        let outcome = handler
            .execute(
                command(now),
                CommandControl::new(CancellationToken::new(), now),
            )
            .await;
        let CommandHandlerOutcome::PreStartError(rejection) = outcome else {
            panic!("expected missing rules rejection");
        };
        assert_eq!(rejection.error_code, "MYFORGE_RULES_FILE_MISSING");

        let mut invalid = command(now);
        invalid.input.artifact_file = "artifacts/fangyuan/../escape.ron".to_string();
        let outcome = handler
            .execute(invalid, CommandControl::new(CancellationToken::new(), now))
            .await;
        let CommandHandlerOutcome::PreStartError(rejection) = outcome else {
            panic!("expected path rejection");
        };
        assert_eq!(rejection.error_code, "MYFORGE_TARGET_PATH_INVALID");
        assert!(runner.specifications.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn canonical_path_checks_reject_symlink_or_junction_escape() {
        let fixture = Fixture::new();
        let outside = fixture._directory.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let escape = fixture.root.join("artifacts/fangyuan/escape");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &escape).unwrap();
        #[cfg(windows)]
        junction::create(&outside, &escape).unwrap();

        let runner = Arc::new(RecordingRunner::new([]));
        let now = system_time_ms(SystemTime::now()).unwrap();
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(false, false),
            runner.clone(),
            Arc::new(FixedClock(now)),
        );
        let mut escaped = command(now);
        escaped.input.artifact_file = "artifacts/fangyuan/escape/result.ron".to_string();
        let outcome = handler
            .execute(escaped, CommandControl::new(CancellationToken::new(), now))
            .await;
        let CommandHandlerOutcome::PreStartError(rejection) = outcome else {
            panic!("expected canonical path rejection");
        };
        assert_eq!(rejection.error_code, "MYFORGE_TARGET_PATH_INVALID");
        assert!(runner.specifications.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn spawn_timeout_failure_and_missing_artifact_map_to_exact_codes() {
        let now = system_time_ms(SystemTime::now()).unwrap();

        let fixture = Fixture::new();
        let spawn_runner = Arc::new(RecordingRunner::new([Err(())]));
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(false, false),
            spawn_runner,
            Arc::new(FixedClock(now)),
        );
        let outcome = handler
            .execute(
                command(now),
                CommandControl::new(CancellationToken::new(), now),
            )
            .await;
        let CommandHandlerOutcome::PreStartError(rejection) = outcome else {
            panic!("expected spawn rejection");
        };
        assert_eq!(rejection.error_code, "MYFORGE_COMMAND_SPAWN_FAILED");

        let fixture = Fixture::new();
        let timeout_runner = Arc::new(RecordingRunner::new([Ok(process_output(
            ProcessTermination::TimedOut,
            None,
            b"partial",
            b"timeout",
        ))]));
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(false, false),
            timeout_runner,
            Arc::new(FixedClock(now)),
        );
        let (_, timeout) = finish(
            handler
                .execute(
                    command(now),
                    CommandControl::new(CancellationToken::new(), now),
                )
                .await,
        )
        .await;
        assert_eq!(timeout.status, "failed");
        assert_eq!(
            timeout.error_code.as_deref(),
            Some("MYFORGE_COMMAND_TIMEOUT")
        );
        assert_eq!(timeout.exit_code, None);

        let fixture = Fixture::new();
        let pipe_failure_runner = Arc::new(RecordingRunner::new([Ok(process_output(
            ProcessTermination::IoFailed,
            Some(0),
            b"partial",
            b"partial",
        ))]));
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(false, false),
            pipe_failure_runner,
            Arc::new(FixedClock(now)),
        );
        let (_, pipe_failure) = finish(
            handler
                .execute(
                    command(now),
                    CommandControl::new(CancellationToken::new(), now),
                )
                .await,
        )
        .await;
        assert_eq!(
            pipe_failure.error_code.as_deref(),
            Some("MYFORGE_COMMAND_FAILED")
        );
        assert_eq!(pipe_failure.exit_code, None);
        pipe_failure.validate(4_096).unwrap();

        let fixture = Fixture::new();
        let missing_runner = Arc::new(RecordingRunner::new([Ok(process_output(
            ProcessTermination::Exited,
            Some(0),
            b"done",
            b"",
        ))]));
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(false, false),
            missing_runner,
            Arc::new(FixedClock(now)),
        );
        let (_, missing) = finish(
            handler
                .execute(
                    command(now),
                    CommandControl::new(CancellationToken::new(), now),
                )
                .await,
        )
        .await;
        assert_eq!(
            missing.error_code.as_deref(),
            Some("MYFORGE_TARGET_FILE_MISSING")
        );
        assert_eq!(
            missing.audit.reason_code.as_deref(),
            Some("artifact_missing")
        );
    }

    #[tokio::test]
    async fn artifact_summary_hashes_content_and_audit_maps_pass_warning_and_invalid_output() {
        let now = system_time_ms(SystemTime::now()).unwrap();
        for (audit_json, expected_status, expected_error) in [
            (
                br#"{"status":"passed","errors":0,"warnings":0,"primitiveCount":3,"mainCode":null,"findings":[]}"#.as_slice(),
                "completed",
                None,
            ),
            (
                br#"{"status":"warning","errors":0,"warnings":1,"primitiveCount":3,"mainCode":"audit.warning","findings":[{"severity":"warning","code":"audit.warning","fieldPath":"root","message":"warning"}]}"#.as_slice(),
                "completed_with_errors",
                Some("FANGYUAN_BLUEPRINT_AUDIT_WARNING"),
            ),
            (b"not-json".as_slice(), "completed_with_errors", Some("FANGYUAN_BLUEPRINT_AUDIT_FAILED")),
        ] {
            let fixture = Fixture::new();
            fs::write(&fixture.artifact, "artifact-content").unwrap();
            let runner = Arc::new(RecordingRunner::new([
                Ok(process_output(
                    ProcessTermination::Exited,
                    Some(0),
                    b"done",
                    b"",
                )),
                Ok(auditor_output(audit_json, 0)),
            ]));
            let handler = ControlledCommandHandler::with_dependencies(
                fixture.settings(false, true),
                runner.clone(),
                Arc::new(FixedClock(now)),
            );
            let (_, result) = finish(
                handler
                    .execute(
                        command(now),
                        CommandControl::new(CancellationToken::new(), now),
                    )
                    .await,
            )
            .await;
            assert_eq!(result.status, expected_status);
            assert_eq!(result.error_code.as_deref(), expected_error);
            assert_eq!(
                result.artifact.sha256.as_deref(),
                Some("e7ba55ec4b1cbb1e2a7f7c8a959e545f4db0230bf1e0a84396990601a1cd63ed")
            );
            let specifications = runner.specifications.lock().unwrap();
            assert_eq!(specifications.len(), 2);
            assert_eq!(
                specifications[1].arguments,
                [
                    "--format",
                    "json",
                    "--rules",
                    "rules/fangyuan/rules.md",
                    "--artifact",
                    "artifacts/fangyuan/result.ron",
                ]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
            );
        }
    }

    #[tokio::test]
    async fn changed_auditor_identity_is_rejected_without_spawning_it() {
        let fixture = Fixture::new();
        fs::write(&fixture.artifact, "artifact").unwrap();
        let settings = fixture.settings(false, true);
        fs::write(&fixture.auditor, "tampered").unwrap();
        let runner = Arc::new(RecordingRunner::new([
            Ok(process_output(
                ProcessTermination::Exited,
                Some(0),
                b"done",
                b"",
            )),
            Ok(auditor_output(
                br#"{"status":"passed","errors":0,"warnings":0,"primitiveCount":1,"mainCode":null,"findings":[]}"#,
                0,
            )),
        ]));
        let now = system_time_ms(SystemTime::now()).unwrap();
        let handler = ControlledCommandHandler::with_dependencies(
            settings,
            runner.clone(),
            Arc::new(FixedClock(now)),
        );

        let (_, result) = finish(
            handler
                .execute(
                    command(now),
                    CommandControl::new(CancellationToken::new(), now),
                )
                .await,
        )
        .await;

        assert_eq!(result.status, "completed_with_errors");
        assert_eq!(
            result.error_code.as_deref(),
            Some("FANGYUAN_BLUEPRINT_AUDIT_FAILED")
        );
        assert_eq!(
            result.audit.main_code.as_deref(),
            Some("auditor_spawn_failed")
        );
        assert_eq!(runner.specifications.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn replaced_auditor_path_is_rejected_without_spawning_it() {
        let fixture = Fixture::new();
        fs::write(&fixture.artifact, "artifact").unwrap();
        let settings = fixture.settings(false, true);
        fs::remove_file(&fixture.auditor).unwrap();
        #[cfg(unix)]
        {
            let replacement = fixture._directory.path().join("replacement-auditor");
            fs::write(&replacement, "fixture").unwrap();
            std::os::unix::fs::symlink(replacement, &fixture.auditor).unwrap();
        }
        #[cfg(windows)]
        {
            let replacement = fixture._directory.path().join("replacement-directory");
            fs::create_dir(&replacement).unwrap();
            junction::create(&replacement, &fixture.auditor).unwrap();
        }
        let runner = Arc::new(RecordingRunner::new([
            Ok(process_output(
                ProcessTermination::Exited,
                Some(0),
                b"done",
                b"",
            )),
            Ok(auditor_output(
                br#"{"status":"passed","errors":0,"warnings":0,"primitiveCount":1,"mainCode":null,"findings":[]}"#,
                0,
            )),
        ]));
        let now = system_time_ms(SystemTime::now()).unwrap();
        let handler = ControlledCommandHandler::with_dependencies(
            settings,
            runner.clone(),
            Arc::new(FixedClock(now)),
        );

        let (_, result) = finish(
            handler
                .execute(
                    command(now),
                    CommandControl::new(CancellationToken::new(), now),
                )
                .await,
        )
        .await;

        assert_eq!(result.status, "completed_with_errors");
        assert_eq!(
            result.error_code.as_deref(),
            Some("FANGYUAN_BLUEPRINT_AUDIT_FAILED")
        );
        assert_eq!(
            result.audit.main_code.as_deref(),
            Some("auditor_spawn_failed")
        );
        assert_eq!(runner.specifications.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn cancellation_before_start_returns_nullable_started_time_without_spawning() {
        let fixture = Fixture::new();
        let runner = Arc::new(RecordingRunner::new([]));
        let now = system_time_ms(SystemTime::now()).unwrap();
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(false, false),
            runner.clone(),
            Arc::new(FixedClock(now)),
        );
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let (_, result) = finish(
            handler
                .execute(command(now), CommandControl::new(cancellation, now))
                .await,
        )
        .await;
        assert_eq!(result.status, "cancelled");
        assert_eq!(result.started_at_ms, None);
        assert_eq!(result.exit_code, None);
        assert!(runner.specifications.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn one_absolute_deadline_limits_process_artifact_and_blocking_audit() {
        let fixture = Fixture::new();
        fs::write(&fixture.artifact, "artifact").unwrap();
        let runner = Arc::new(TimedRunner::new([
            TimedPlan {
                behavior: TimedBehavior::Delay(Duration::from_millis(800)),
                output: process_output(ProcessTermination::Exited, Some(0), b"done", b""),
            },
            TimedPlan {
                behavior: TimedBehavior::UntilDeadline,
                output: auditor_output(
                    br#"{"status":"passed","errors":0,"warnings":0,"primitiveCount":1,"mainCode":null,"findings":[]}"#,
                    0,
                ),
            },
        ]));
        let now_ms = system_time_ms(SystemTime::now()).unwrap();
        let mut settings = fixture.settings(false, true);
        settings.audit_timeout_ms = 10_000;
        let handler = ControlledCommandHandler::with_dependencies(
            settings,
            runner.clone(),
            Arc::new(FixedClock(now_ms)),
        );
        let mut execute = command(now_ms);
        execute.timeout_ms = 1_000;
        let started = tokio::time::Instant::now();

        let (_, result) = finish(
            handler
                .execute(
                    execute,
                    CommandControl::new(CancellationToken::new(), now_ms),
                )
                .await,
        )
        .await;

        assert_eq!(
            tokio::time::Instant::now().duration_since(started),
            Duration::from_secs(1)
        );
        assert_eq!(result.status, "failed");
        assert_eq!(
            result.error_code.as_deref(),
            Some("MYFORGE_COMMAND_TIMEOUT")
        );
        let observations = runner.observations.lock().unwrap();
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].0, observations[1].0);
        assert_eq!(observations[0].1, Duration::from_secs(1));
        assert!(observations[1].1 <= Duration::from_millis(200));
        assert!(observations[1].1 < observations[0].1);
        drop(observations);
        assert_eq!(runner.drops.load(Ordering::SeqCst), 2);
        assert_eq!(runner.specifications.lock().unwrap().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn aborting_started_execution_drops_inner_process_and_prevents_audit() {
        let fixture = Fixture::new();
        fs::write(&fixture.artifact, "artifact").unwrap();
        let runner = Arc::new(TimedRunner::new([TimedPlan {
            behavior: TimedBehavior::Pending,
            output: process_output(ProcessTermination::Exited, Some(0), b"", b""),
        }]));
        let now_ms = system_time_ms(SystemTime::now()).unwrap();
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(false, true),
            runner.clone(),
            Arc::new(FixedClock(now_ms)),
        );
        let outcome = handler
            .execute(
                command(now_ms),
                CommandControl::new(CancellationToken::new(), now_ms),
            )
            .await;
        let CommandHandlerOutcome::Started(execution) = outcome else {
            panic!("expected started execution");
        };
        let outer = tokio::spawn(execution.finish());
        while !runner.entered.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }

        outer.abort();
        assert!(matches!(outer.await, Err(error) if error.is_cancelled()));
        assert_eq!(runner.drops.load(Ordering::SeqCst), 1);
        assert_eq!(runner.specifications.lock().unwrap().len(), 1);

        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert_eq!(runner.specifications.lock().unwrap().len(), 1);
        assert_eq!(runner.drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn disconnect_cancellation_interrupts_artifact_io_and_never_starts_audit() {
        let cancellation = CommandCancellation::new();
        let control = CommandControl::from_cancellation(cancellation.clone(), 100);
        let budget = CommandBudget::new(Duration::from_secs(60));
        let trigger = cancellation.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            trigger.cancel();
        });
        let before = tokio::time::Instant::now();
        let blocked =
            await_observation_io(&control, budget, std::future::pending::<io::Result<()>>()).await;
        assert!(matches!(blocked, Err(ObservationFailure::Cancelled)));
        assert_eq!(tokio::time::Instant::now(), before);
        assert_eq!(control.cancel_deadline_at_ms(), None);

        let fixture = Fixture::new();
        fs::write(&fixture.artifact, "artifact").unwrap();
        let runner = Arc::new(TimedRunner::new([TimedPlan {
            behavior: TimedBehavior::CancelWithoutDeadline,
            output: process_output(ProcessTermination::Exited, Some(0), b"done", b""),
        }]));
        let cancellation = CommandCancellation::new();
        let now_ms = system_time_ms(SystemTime::now()).unwrap();
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(false, true),
            runner.clone(),
            Arc::new(FixedClock(now_ms)),
        );
        let (_, result) = finish(
            handler
                .execute(
                    command(now_ms),
                    CommandControl::from_cancellation(cancellation.clone(), now_ms),
                )
                .await,
        )
        .await;
        assert!(cancellation.is_cancelled());
        assert_eq!(cancellation.deadline_at_ms(), None);
        assert_eq!(result.status, "cancelled");
        assert_eq!(
            result.error_code.as_deref(),
            Some("MYFORGE_COMMAND_CANCELLED")
        );
        assert_eq!(runner.specifications.lock().unwrap().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn dry_run_deadline_expires_fail_closed_without_an_invalid_result() {
        let fixture = Fixture::new();
        let now_ms = system_time_ms(SystemTime::now()).unwrap();
        let mut paths = validate_workspace_paths(&fixture.root, &command(now_ms)).unwrap();
        paths.dry_run = true;
        let outcome = complete_dry_run(
            command(now_ms),
            paths,
            CommandControl::new(CancellationToken::new(), now_ms),
            now_ms,
            CommandBudget {
                deadline: tokio::time::Instant::now(),
            },
            Arc::new(FixedClock(now_ms)),
        )
        .await;
        assert!(matches!(
            outcome,
            StartedExecutionOutcome::FailClosed {
                reason: "dry_run_command_timeout"
            }
        ));
    }

    #[test]
    fn artifact_checks_stop_immediately_for_disconnect_cancellation() {
        let cancellation = CommandCancellation::new();
        let control = CommandControl::from_cancellation(cancellation.clone(), 100);
        let budget = CommandBudget::new(Duration::from_secs(1));

        assert!(ensure_execution_budget(&control, budget).is_ok());
        cancellation.cancel();
        assert_eq!(
            ensure_execution_budget(&control, budget),
            Err(ObservationFailure::Cancelled)
        );
    }

    #[tokio::test]
    async fn auditor_spawn_timeout_and_exit_failures_use_stable_main_codes() {
        let now = system_time_ms(SystemTime::now()).unwrap();
        let scenarios = [
            (Err(()), "auditor_spawn_failed"),
            (
                Ok(process_output(
                    ProcessTermination::TimedOut,
                    None,
                    b"",
                    b"",
                )),
                "auditor_timeout",
            ),
            (
                Ok(auditor_output(
                    br#"{"status":"passed","errors":0,"warnings":0,"primitiveCount":null,"mainCode":null,"findings":[]}"#,
                    2,
                )),
                "auditor_exit_failed",
            ),
        ];
        for (audit_process, expected_main_code) in scenarios {
            let fixture = Fixture::new();
            fs::write(&fixture.artifact, "artifact").unwrap();
            let runner = Arc::new(RecordingRunner::new([
                Ok(process_output(
                    ProcessTermination::Exited,
                    Some(0),
                    b"done",
                    b"",
                )),
                audit_process,
            ]));
            let handler = ControlledCommandHandler::with_dependencies(
                fixture.settings(false, true),
                runner,
                Arc::new(FixedClock(now)),
            );
            let (_, result) = finish(
                handler
                    .execute(
                        command(now),
                        CommandControl::new(CancellationToken::new(), now),
                    )
                    .await,
            )
            .await;
            assert_eq!(result.status, "completed_with_errors");
            assert_eq!(result.audit.status, "failed");
            assert_eq!(result.audit.main_code.as_deref(), Some(expected_main_code));
            assert_eq!(
                result.error_code.as_deref(),
                Some("FANGYUAN_BLUEPRINT_AUDIT_FAILED")
            );
        }
    }

    #[tokio::test]
    async fn stale_artifact_becomes_a_node_compatible_audit_warning() {
        let fixture = Fixture::new();
        fs::write(&fixture.artifact, "old artifact").unwrap();
        let modified = fs::metadata(&fixture.artifact).unwrap().modified().unwrap();
        let now = system_time_ms(modified).unwrap() + 10_000;
        let runner = Arc::new(RecordingRunner::new([Ok(process_output(
            ProcessTermination::Exited,
            Some(0),
            b"done",
            b"",
        ))]));
        let handler = ControlledCommandHandler::with_dependencies(
            fixture.settings(false, false),
            runner,
            Arc::new(FixedClock(now)),
        );
        let (_, result) = finish(
            handler
                .execute(
                    command(now),
                    CommandControl::new(CancellationToken::new(), now),
                )
                .await,
        )
        .await;
        assert_eq!(result.status, "completed_with_errors");
        assert_eq!(result.audit.status, "warning");
        assert_eq!(result.audit.main_code.as_deref(), Some("artifact_stale"));
        assert_eq!(result.audit.findings_preview.len(), 1);
        result.validate(4_096).unwrap();
    }

    #[test]
    fn process_fixture_entry() {
        let mode = env::var("AGENT_PROCESS_FIXTURE_MODE").unwrap_or_default();
        match mode.as_str() {
            "dual" => {
                let stdout = std::io::stdout();
                let stderr = std::io::stderr();
                let mut stdout = stdout.lock();
                let mut stderr = stderr.lock();
                let out = vec![b'o'; 16 * 1024];
                let err = vec![b'e'; 16 * 1024];
                for _ in 0..128 {
                    stdout.write_all(&out).unwrap();
                    stderr.write_all(&err).unwrap();
                }
                stdout.write_all(&[0xff, b'z']).unwrap();
                stdout.flush().unwrap();
                stderr.flush().unwrap();
            }
            "tree" => {
                let executable = std::env::current_exe().unwrap();
                let sentinel = env::var("AGENT_PROCESS_FIXTURE_SENTINEL").unwrap();
                let mut child = StdCommand::new(executable);
                child
                    .args([
                        "--exact",
                        "execution::tests::process_fixture_entry",
                        "--nocapture",
                    ])
                    .env("AGENT_PROCESS_FIXTURE_MODE", "leaf")
                    .env("AGENT_PROCESS_FIXTURE_SENTINEL", sentinel)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
                let mut spawned = child.spawn().unwrap();
                let ready = PathBuf::from(env::var("AGENT_PROCESS_FIXTURE_SENTINEL").unwrap())
                    .with_extension("ready");
                fs::write(ready, "ready").unwrap();
                std::thread::sleep(Duration::from_secs(30));
                let _ = spawned.kill();
                let _ = spawned.wait();
            }
            "leaf" => {
                std::thread::sleep(Duration::from_millis(800));
                fs::write(
                    env::var("AGENT_PROCESS_FIXTURE_SENTINEL").unwrap(),
                    "orphaned",
                )
                .unwrap();
            }
            _ => {}
        }
    }

    fn fixture_process_specification(
        mode: &str,
        sentinel: Option<&Path>,
        preview_bytes: usize,
    ) -> ProcessSpecification {
        let mut environment = minimal_environment();
        environment.push((
            OsString::from("AGENT_PROCESS_FIXTURE_MODE"),
            OsString::from(mode),
        ));
        if let Some(sentinel) = sentinel {
            environment.push((
                OsString::from("AGENT_PROCESS_FIXTURE_SENTINEL"),
                sentinel.as_os_str().to_owned(),
            ));
        }
        ProcessSpecification {
            program: std::env::current_exe().unwrap().into_os_string(),
            arguments: [
                "--exact",
                "execution::tests::process_fixture_entry",
                "--nocapture",
            ]
            .into_iter()
            .map(OsString::from)
            .collect(),
            working_directory: std::env::current_dir().unwrap(),
            environment,
            stdout_preview_bytes: preview_bytes,
            stderr_preview_bytes: preview_bytes,
            stdout_raw_bytes: None,
        }
    }

    fn system_control(cancellation: CancellationToken) -> CommandControl {
        CommandControl::new(cancellation, system_time_ms(SystemTime::now()).unwrap())
    }

    #[tokio::test]
    async fn system_runner_drains_both_pipes_and_truncates_lossy_utf8_without_deadlock() {
        let runner = SystemProcessRunner;
        let process = runner
            .spawn(fixture_process_specification("dual", None, 4_096))
            .unwrap();
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            process.wait(
                system_control(CancellationToken::new()),
                tokio::time::Instant::now() + Duration::from_secs(8),
                Arc::new(SystemExecutionClock),
            ),
        )
        .await
        .unwrap();
        assert_eq!(output.termination, ProcessTermination::Exited);
        assert_eq!(output.exit_code, Some(0));
        assert!(output.stdout.bytes > 2 * 1024 * 1024);
        assert!(output.stderr.bytes >= 2 * 1024 * 1024);
        assert!(output.stdout.truncated);
        assert!(output.stderr.truncated);
        assert!(output.stdout.preview.len() <= 4_096);
        assert!(output.stderr.preview.len() <= 4_096);
    }

    #[tokio::test]
    async fn system_runner_timeout_terminates_the_entire_process_tree() {
        let directory = tempfile::tempdir().unwrap();
        let sentinel = directory.path().join("orphan-sentinel");
        let runner = SystemProcessRunner;
        let process = runner
            .spawn(fixture_process_specification(
                "tree",
                Some(&sentinel),
                4_096,
            ))
            .unwrap();
        wait_for_fixture_ready(&sentinel).await;
        let output = process
            .wait(
                system_control(CancellationToken::new()),
                tokio::time::Instant::now() + Duration::from_millis(150),
                Arc::new(SystemExecutionClock),
            )
            .await;
        assert_eq!(output.termination, ProcessTermination::TimedOut);
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        assert!(
            !sentinel.exists(),
            "grandchild survived process-tree timeout"
        );
    }

    #[tokio::test]
    async fn dropping_running_process_terminates_the_entire_process_group() {
        let directory = tempfile::tempdir().unwrap();
        let sentinel = directory.path().join("orphan-sentinel");
        let runner = SystemProcessRunner;
        let process = runner
            .spawn(fixture_process_specification(
                "tree",
                Some(&sentinel),
                4_096,
            ))
            .unwrap();
        wait_for_fixture_ready(&sentinel).await;

        drop(process);
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        assert!(
            !sentinel.exists(),
            "grandchild survived running-process drop"
        );
    }

    #[tokio::test]
    async fn system_runner_cancellation_terminates_the_entire_process_tree() {
        let directory = tempfile::tempdir().unwrap();
        let sentinel = directory.path().join("orphan-sentinel");
        let runner = SystemProcessRunner;
        let process = runner
            .spawn(fixture_process_specification(
                "tree",
                Some(&sentinel),
                4_096,
            ))
            .unwrap();
        wait_for_fixture_ready(&sentinel).await;
        let cancellation = CommandCancellation::new();
        let trigger = cancellation.clone();
        let now_ms = system_time_ms(SystemTime::now()).unwrap();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            trigger.cancel_at(now_ms + 1_000);
        });
        let output = process
            .wait(
                CommandControl::from_cancellation(cancellation, now_ms),
                tokio::time::Instant::now() + Duration::from_secs(5),
                Arc::new(SystemExecutionClock),
            )
            .await;
        assert_eq!(output.termination, ProcessTermination::Cancelled);
        tokio::time::sleep(Duration::from_millis(1_000)).await;
        assert!(
            !sentinel.exists(),
            "grandchild survived process-tree cancellation"
        );
    }

    async fn wait_for_fixture_ready(sentinel: &Path) {
        let ready = sentinel.with_extension("ready");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !ready.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("process-tree fixture did not start its grandchild");
    }
}
