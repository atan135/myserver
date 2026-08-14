use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use loadtest_core::SCHEMA_VERSION;
use loadtest_core::abort::{AbortController, AbortReason, install_ctrl_c_flag};
use loadtest_core::accounts::{
    AccountManifest, AccountPreparationState, CharacterReadiness, EnvironmentSecretProvider,
    SecretProvider, auth_character_name, auth_login_name, read_manifest, write_manifest,
};
use loadtest_core::auth_budget::{PrepareCommand, estimate_prepare, validate_prepare_budget};
use loadtest_core::auth_http::{
    AuthAdmissionError, AuthDispatchAdmission, AuthHttpRequest, AuthHttpResponse,
    AuthHttpTransport, AuthResponseBody, ReqwestAuthHttpTransport,
};
use loadtest_core::config::{RunAccess, load_config, load_private_config};
use loadtest_core::metrics::HistogramSnapshot;
use loadtest_core::protection::{DryRunProtection, revalidate_or_abort};
use serde::Serialize;

fn main() -> ExitCode {
    match execute(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("account-prepare: {error}");
            ExitCode::from(2)
        }
    }
}

fn execute(arguments: Vec<String>) -> Result<(), String> {
    let cli = Cli::parse(arguments)?;
    let config = load_config(&cli.config, None).map_err(|error| error.to_string())?;
    let manifest_path = cli
        .manifest
        .clone()
        .unwrap_or_else(|| default_manifest_path(&config));

    match cli.command.as_str() {
        "plan" => write_plan(&config, &manifest_path),
        "apply" | "verify" => {
            let command = if cli.command == "apply" {
                PrepareCommand::Apply
            } else {
                PrepareCommand::Verify
            };
            let account_count = existing_or_planned_account_count(&config, &manifest_path)?;
            cli.require_live_gate(&config, command, account_count)?;
            let deadline_unix_ms = prepare_deadline_unix_ms(&config, unix_ms())?;
            let deadline =
                Instant::now() + Duration::from_millis(deadline_unix_ms.saturating_sub(unix_ms()));
            let private = load_private_config(
                cli.private_config
                    .as_deref()
                    .expect("live gate requires private config"),
            )
            .map_err(|error| error.to_string())?;
            let manifest = read_or_plan(&config, &manifest_path)?;
            let secret_provider = EnvironmentSecretProvider::new(&private);
            let ctrl_c = install_ctrl_c_flag()
                .map_err(|error| format!("failed to install Ctrl+C handler: {error}"))?;
            let protection = DryRunProtection::new(&config);
            let mut abort = AbortController::default();
            let mut admission = AuthDispatchAdmission::new(&config.budget)?;
            // Every actual attempt receives its remaining deadline via
            // `set_attempt_timeout`; this fallback is never used after admission.
            let mut transport =
                ReqwestAuthHttpTransport::new(&config.targets.auth_http, Duration::from_millis(1))?;
            let result = if cli.command == "apply" {
                apply_manifest(
                    &mut transport,
                    &secret_provider,
                    manifest,
                    &config.account_prepare.character_name_prefix,
                    &manifest_path,
                    &mut admission,
                    deadline,
                    &mut abort,
                    &ctrl_c,
                    &protection,
                    config.stop_file.as_deref().map(Path::new),
                )?
            } else {
                verify_manifest(
                    &mut transport,
                    &secret_provider,
                    manifest,
                    &manifest_path,
                    &mut admission,
                    deadline,
                    &mut abort,
                    &ctrl_c,
                    &protection,
                    config.stop_file.as_deref().map(Path::new),
                )?
            };
            write_prepare_result(&config, &cli.command, &manifest_path, &result.metrics)?;
            println!(
                "account-prepare {} completed: verified={}, failed={}, manifest={}",
                cli.command,
                result.metrics.verified,
                result.metrics.failed,
                manifest_path.display()
            );
            if result.metrics.failed > 0 {
                return Err("one or more accounts failed preparation; persisted manifest supports a later resume".into());
            }
            Ok(())
        }
        "export" => export_manifest(&config, &manifest_path, cli.export_path.as_deref()),
        _ => Err(usage()),
    }
}

fn existing_or_planned_account_count(
    config: &loadtest_core::LoadTestConfig,
    manifest_path: &Path,
) -> Result<u64, String> {
    if manifest_path.is_file() {
        return Ok(read_manifest(manifest_path)?.accounts.len() as u64);
    }
    Ok(u64::from(
        config
            .account_prepare
            .account_count
            .unwrap_or(config.budget.max_virtual_players),
    ))
}

fn default_manifest_path(config: &loadtest_core::LoadTestConfig) -> PathBuf {
    Path::new(&config.prepare_reports_root)
        .join("account-manifests")
        .join(&config.environment.name)
        .join(&config.account_prepare.batch)
        .join("manifest.json")
}

fn read_or_plan(
    config: &loadtest_core::LoadTestConfig,
    manifest_path: &Path,
) -> Result<AccountManifest, String> {
    if manifest_path.is_file() {
        return read_manifest(manifest_path);
    }
    let count = config
        .account_prepare
        .account_count
        .unwrap_or(config.budget.max_virtual_players);
    let manifest = AccountManifest::planned(
        &config.environment.name,
        &config.account_prepare,
        count,
        unix_ms(),
    );
    for account in &manifest.accounts {
        auth_login_name(&account.logical_account_id)?;
    }
    write_manifest(manifest_path, &manifest)?;
    Ok(manifest)
}

fn write_plan(config: &loadtest_core::LoadTestConfig, manifest_path: &Path) -> Result<(), String> {
    let manifest = read_or_plan(config, manifest_path)?;
    let account_count = manifest.accounts.len() as u64;
    let apply_estimate = estimate_prepare(PrepareCommand::Apply, account_count)?;
    let verify_estimate = estimate_prepare(PrepareCommand::Verify, account_count)?;
    let plan = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "environment": config.environment.name,
        "account_batch": config.account_prepare.batch,
        "account_count": account_count,
        "account_manifest": manifest_path,
        "prepare_result_root": config.prepare_reports_root,
        "prepare_estimates": {
            "apply": &apply_estimate,
            "verify": &verify_estimate,
        },
        "hard_budget": {
            "max_total_operations": config.budget.max_total_operations,
            "max_data_writes": config.budget.max_data_writes,
            "apply_within_operation_budget": apply_estimate.http_operations <= config.budget.max_total_operations,
            "apply_within_data_write_budget": apply_estimate.potential_data_writes <= config.budget.max_data_writes,
            "apply_within_budget": validate_prepare_budget(&apply_estimate, &config.budget).is_ok(),
            "verify_within_operation_budget": verify_estimate.http_operations <= config.budget.max_total_operations,
            "verify_within_data_write_budget": verify_estimate.potential_data_writes <= config.budget.max_data_writes,
            "verify_within_budget": validate_prepare_budget(&verify_estimate, &config.budget).is_ok(),
        },
        "network_calls": false,
        "status": "planned_requires_explicit_live_gate",
    });
    let plan_path = manifest_path
        .parent()
        .ok_or("account manifest path must have a parent")?
        .join("plan.json");
    std::fs::create_dir_all(
        plan_path
            .parent()
            .ok_or("plan path must have a parent directory")?,
    )
    .map_err(|error| error.to_string())?;
    std::fs::write(&plan_path, serde_json::to_vec_pretty(&plan).unwrap())
        .map_err(|error| error.to_string())?;
    println!(
        "account plan contains no credentials and performs no business writes: {}",
        plan_path.display()
    );
    Ok(())
}

fn export_manifest(
    config: &loadtest_core::LoadTestConfig,
    manifest_path: &Path,
    requested_output: Option<&Path>,
) -> Result<(), String> {
    let manifest = read_manifest(manifest_path)?;
    if manifest.environment != config.environment.name
        || manifest.batch != config.account_prepare.batch
    {
        return Err(
            "manifest environment or batch does not match the selected configuration".into(),
        );
    }
    let output = requested_output.map(Path::to_path_buf).unwrap_or_else(|| {
        manifest_path
            .parent()
            .expect("manifest path has a parent")
            .join("manifest-export.json")
    });
    write_manifest(&output, &manifest)?;
    println!(
        "credential-free account manifest exported: {}",
        output.display()
    );
    Ok(())
}

#[derive(Default, Serialize)]
struct PrepareMetrics {
    operation_attempts: BTreeMap<String, u64>,
    operation_successes: BTreeMap<String, u64>,
    operation_latencies_ms: BTreeMap<String, HistogramSnapshot>,
    registrations_created: u64,
    registrations_resumed: u64,
    characters_created: u64,
    verified: u64,
    failed: u64,
}

impl PrepareMetrics {
    fn record(&mut self, operation: &'static str, started: Instant, success: bool) {
        *self.operation_attempts.entry(operation.into()).or_default() += 1;
        if success {
            *self
                .operation_successes
                .entry(operation.into())
                .or_default() += 1;
        }
        self.operation_latencies_ms
            .entry(operation.into())
            .or_default()
            .record(started.elapsed().as_millis() as u64);
    }
}

struct PrepareResult {
    metrics: PrepareMetrics,
}

fn apply_manifest<T: AuthHttpTransport, S: SecretProvider>(
    transport: &mut T,
    secret_provider: &S,
    mut manifest: AccountManifest,
    character_prefix: &str,
    manifest_path: &Path,
    admission: &mut AuthDispatchAdmission,
    deadline: Instant,
    abort: &mut AbortController,
    ctrl_c: &Arc<AtomicBool>,
    protection: &DryRunProtection<'_>,
    stop_file: Option<&Path>,
) -> Result<PrepareResult, String> {
    let mut metrics = PrepareMetrics::default();
    for index in 0..manifest.accounts.len() {
        if abort.should_stop_new_sessions() {
            break;
        }
        if manifest.accounts[index].preparation_state == AccountPreparationState::Verified
            && manifest.accounts[index].character_readiness == CharacterReadiness::Ready
        {
            metrics.verified += 1;
            continue;
        }
        let logical_account_id = manifest.accounts[index].logical_account_id.clone();
        let batch = manifest.accounts[index].batch.clone();
        let prepared = (|| {
            let login_name = auth_login_name(&logical_account_id)?;
            let password = secret_provider.password_for(&logical_account_id)?;
            let started = Instant::now();
            let register = dispatch_prepare_request(
                transport,
                AuthHttpRequest::Register {
                    login_name: login_name.clone(),
                    password: password.clone(),
                    display_name: None,
                },
                admission,
                deadline,
                abort,
                ctrl_c,
                protection,
                stop_file,
            )?;
            let registered = is_login_name_exists(&register) || is_success(&register);
            metrics.record("register", started, registered);
            if is_login_name_exists(&register) {
                metrics.registrations_resumed += 1;
            } else if registered {
                metrics.registrations_created += 1;
            } else {
                return Err(
                    "account registration did not receive a successful or resumable response"
                        .into(),
                );
            }
            let character_index = u32::try_from(index + 1)
                .map_err(|_| "account manifest contains too many accounts for character naming")?;
            let character_name = auth_character_name(character_prefix, &batch, character_index);
            complete_readiness(
                transport,
                &mut metrics,
                &login_name,
                &password,
                &character_name,
                true,
                admission,
                deadline,
                abort,
                ctrl_c,
                protection,
                stop_file,
            )
        })();
        apply_entry_result(&mut manifest.accounts[index], prepared, &mut metrics);
        write_manifest(manifest_path, &manifest)?;
    }
    Ok(PrepareResult { metrics })
}

fn verify_manifest<T: AuthHttpTransport, S: SecretProvider>(
    transport: &mut T,
    secret_provider: &S,
    mut manifest: AccountManifest,
    manifest_path: &Path,
    admission: &mut AuthDispatchAdmission,
    deadline: Instant,
    abort: &mut AbortController,
    ctrl_c: &Arc<AtomicBool>,
    protection: &DryRunProtection<'_>,
    stop_file: Option<&Path>,
) -> Result<PrepareResult, String> {
    let mut metrics = PrepareMetrics::default();
    for index in 0..manifest.accounts.len() {
        if abort.should_stop_new_sessions() {
            break;
        }
        let logical_account_id = manifest.accounts[index].logical_account_id.clone();
        let verified = (|| {
            let login_name = auth_login_name(&logical_account_id)?;
            let password = secret_provider.password_for(&logical_account_id)?;
            complete_readiness(
                transport,
                &mut metrics,
                &login_name,
                &password,
                "",
                false,
                admission,
                deadline,
                abort,
                ctrl_c,
                protection,
                stop_file,
            )
        })();
        apply_entry_result(&mut manifest.accounts[index], verified, &mut metrics);
        write_manifest(manifest_path, &manifest)?;
    }
    Ok(PrepareResult { metrics })
}

fn complete_readiness<T: AuthHttpTransport>(
    transport: &mut T,
    metrics: &mut PrepareMetrics,
    login_name: &str,
    password: &str,
    character_name: &str,
    create_missing_character: bool,
    admission: &mut AuthDispatchAdmission,
    deadline: Instant,
    abort: &mut AbortController,
    ctrl_c: &Arc<AtomicBool>,
    protection: &DryRunProtection<'_>,
    stop_file: Option<&Path>,
) -> Result<(), String> {
    let started = Instant::now();
    let login = dispatch_prepare_request(
        transport,
        AuthHttpRequest::Login {
            login_name: login_name.into(),
            password: password.into(),
        },
        admission,
        deadline,
        abort,
        ctrl_c,
        protection,
        stop_file,
    )?;
    metrics.record("login", started, is_success(&login));
    let access_token = success(&login, "login")?
        .access_token
        .ok_or("login response did not provide an access token")?;

    let started = Instant::now();
    let listed = dispatch_prepare_request(
        transport,
        AuthHttpRequest::ListCharacters {
            access_token: access_token.clone(),
        },
        admission,
        deadline,
        abort,
        ctrl_c,
        protection,
        stop_file,
    )?;
    metrics.record("list_characters", started, is_success(&listed));
    let mut character_id = success(&listed, "list_characters")?.character_id;
    if character_id.is_none() && create_missing_character {
        let started = Instant::now();
        let created = dispatch_prepare_request(
            transport,
            AuthHttpRequest::CreateCharacter {
                access_token: access_token.clone(),
                name: character_name.into(),
            },
            admission,
            deadline,
            abort,
            ctrl_c,
            protection,
            stop_file,
        )?;
        metrics.record("create_character", started, is_success(&created));
        character_id = success(&created, "create_character")?.character_id;
        metrics.characters_created += 1;
    }
    let character_id = character_id.ok_or("account has no loginable character")?;

    let started = Instant::now();
    let selected = dispatch_prepare_request(
        transport,
        AuthHttpRequest::SelectCharacter {
            access_token: access_token.clone(),
            character_id: character_id.clone(),
        },
        admission,
        deadline,
        abort,
        ctrl_c,
        protection,
        stop_file,
    )?;
    metrics.record("select_character", started, is_success(&selected));
    if success(&selected, "select_character")?.ticket.is_none() {
        return Err("character selection did not provide a ticket".into());
    }

    let started = Instant::now();
    let issued = dispatch_prepare_request(
        transport,
        AuthHttpRequest::IssueTicket {
            access_token,
            character_id,
        },
        admission,
        deadline,
        abort,
        ctrl_c,
        protection,
        stop_file,
    )?;
    metrics.record("issue_ticket", started, is_success(&issued));
    if success(&issued, "issue_ticket")?.ticket.is_none() {
        return Err("ticket issue did not provide a ticket".into());
    }
    Ok(())
}

fn dispatch_prepare_request<T: AuthHttpTransport>(
    transport: &mut T,
    request: AuthHttpRequest,
    admission: &mut AuthDispatchAdmission,
    deadline: Instant,
    abort: &mut AbortController,
    ctrl_c: &Arc<AtomicBool>,
    protection: &DryRunProtection<'_>,
    stop_file: Option<&Path>,
) -> Result<AuthHttpResponse, String> {
    let remaining = admission
        .admit(&request, deadline, || {
            abort.check_ctrl_c(ctrl_c);
            abort.check_stop_file(stop_file);
            if revalidate_or_abort(protection, abort).is_some() || abort.should_stop_new_sessions()
            {
                return Err("account preparation admission stopped before request dispatch".into());
            }
            Ok(())
        })
        .map_err(|error| match error {
            AuthAdmissionError::BudgetExceeded(error) => {
                abort.request(AbortReason::BudgetExceeded);
                error
            }
            AuthAdmissionError::DeadlineExceeded => {
                abort.request(AbortReason::Deadline);
                "account preparation deadline elapsed before request dispatch".into()
            }
            AuthAdmissionError::Stopped(error) => error,
        })?;
    transport.set_attempt_timeout(remaining);
    Ok(transport.send(request))
}

fn apply_entry_result(
    account: &mut loadtest_core::accounts::AccountManifestEntry,
    result: Result<(), String>,
    metrics: &mut PrepareMetrics,
) {
    match result {
        Ok(()) => {
            account.preparation_state = AccountPreparationState::Verified;
            account.character_readiness = CharacterReadiness::Ready;
            account.last_verified_unix_ms = Some(unix_ms());
            metrics.verified += 1;
        }
        Err(_) => {
            account.preparation_state = AccountPreparationState::Failed;
            account.character_readiness = CharacterReadiness::VerificationFailed;
            metrics.failed += 1;
        }
    }
}

fn is_success(response: &AuthHttpResponse) -> bool {
    matches!(&response.body, AuthResponseBody::Success(_))
        && response.status.is_some_and(|status| status < 400)
}

fn is_login_name_exists(response: &AuthHttpResponse) -> bool {
    matches!(&response.body, AuthResponseBody::BusinessError(code) if code == "LOGIN_NAME_EXISTS")
}

fn success(
    response: &AuthHttpResponse,
    operation: &str,
) -> Result<loadtest_core::auth_http::AuthSuccess, String> {
    match &response.body {
        AuthResponseBody::Success(success)
            if response.status.is_some_and(|status| status < 400) =>
        {
            Ok(success.clone())
        }
        _ => Err(format!("auth-http {operation} readiness check failed")),
    }
}

fn write_prepare_result(
    config: &loadtest_core::LoadTestConfig,
    command: &str,
    manifest_path: &Path,
    metrics: &PrepareMetrics,
) -> Result<(), String> {
    let path = manifest_path
        .parent()
        .ok_or("account manifest path must have a parent")?
        .join(format!("{command}-result.json"));
    let value = serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "kind": "account_prepare",
        "command": command,
        "environment": config.environment.name,
        "account_batch": config.account_prepare.batch,
        "prepare_result_root": config.prepare_reports_root,
        "metrics": metrics,
    });
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap())
        .map_err(|error| error.to_string())
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn prepare_deadline_unix_ms(
    config: &loadtest_core::LoadTestConfig,
    started_unix_ms: u64,
) -> Result<u64, String> {
    let budget_deadline =
        started_unix_ms.saturating_add(config.budget.max_duration_secs.saturating_mul(1_000));
    let deadline = config.deadline_unix_ms.unwrap_or(budget_deadline);
    if deadline > budget_deadline {
        return Err("deadline_unix_ms may not exceed the profile duration budget".into());
    }
    if deadline <= started_unix_ms {
        return Err("account preparation deadline has already elapsed".into());
    }
    Ok(deadline)
}

#[derive(Debug)]
struct Cli {
    command: String,
    config: PathBuf,
    private_config: Option<PathBuf>,
    manifest: Option<PathBuf>,
    export_path: Option<PathBuf>,
    execute: bool,
    confirm_write: Option<String>,
    allow_remote: bool,
    confirmation: Option<String>,
}

impl Cli {
    fn parse(arguments: Vec<String>) -> Result<Self, String> {
        let mut values = arguments.into_iter();
        let command = values.next().ok_or_else(usage)?;
        let mut cli = Self {
            command,
            config: PathBuf::new(),
            private_config: None,
            manifest: None,
            export_path: None,
            execute: false,
            confirm_write: None,
            allow_remote: false,
            confirmation: None,
        };
        while let Some(argument) = values.next() {
            match argument.as_str() {
                "--config" => cli.config = PathBuf::from(next_value(&mut values, "--config")?),
                "--private-config" => {
                    cli.private_config =
                        Some(PathBuf::from(next_value(&mut values, "--private-config")?))
                }
                "--manifest" => {
                    cli.manifest = Some(PathBuf::from(next_value(&mut values, "--manifest")?))
                }
                "--output" => {
                    cli.export_path = Some(PathBuf::from(next_value(&mut values, "--output")?))
                }
                "--execute" => cli.execute = true,
                "--confirm-write" => {
                    cli.confirm_write = Some(next_value(&mut values, "--confirm-write")?)
                }
                "--allow-remote" => cli.allow_remote = true,
                "--confirm" => cli.confirmation = Some(next_value(&mut values, "--confirm")?),
                _ => {
                    return Err(format!(
                        "unknown account-prepare argument {argument}\n{}",
                        usage()
                    ));
                }
            }
        }
        if cli.config.as_os_str().is_empty() {
            return Err("--config is required".into());
        }
        Ok(cli)
    }

    fn require_live_gate(
        &self,
        config: &loadtest_core::LoadTestConfig,
        command: PrepareCommand,
        account_count: u64,
    ) -> Result<(), String> {
        if !self.execute {
            return Err("apply and verify require --execute; no service request was made".into());
        }
        if self.confirm_write.as_deref() != Some(config.environment.name.as_str()) {
            return Err("apply and verify require --confirm-write <environment>".into());
        }
        if self.private_config.is_none() {
            return Err("apply and verify require --private-config with secret references".into());
        }
        let estimate = estimate_prepare(command, account_count)?;
        validate_prepare_budget(&estimate, &config.budget)?;
        config
            .validate_access(RunAccess {
                allow_remote: self.allow_remote,
                confirmation: self.confirmation.as_deref(),
            })
            .map_err(|error| error.to_string())
    }
}

fn next_value(values: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    values
        .next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn usage() -> String {
    "usage: account-prepare plan --config <file> [--manifest <file>]\n       account-prepare apply|verify --config <file> --private-config <file> --execute --confirm-write <environment> [--manifest <file>] [--allow-remote --confirm <environment>]\n       account-prepare export --config <file> [--manifest <file>] [--output <file>]".into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use loadtest_core::auth_http::{FakeAuthHttpService, FakeAuthOutcome};
    use loadtest_core::config::AccountPrepareConfig;

    struct TestSecretProvider;

    impl SecretProvider for TestSecretProvider {
        fn password_for(&self, _logical_account_id: &str) -> Result<String, String> {
            Ok("test-password-not-reported".into())
        }
    }

    struct CreateFailureTransport;

    impl AuthHttpTransport for CreateFailureTransport {
        fn send(&mut self, request: AuthHttpRequest) -> AuthHttpResponse {
            let success = |access_token, character_id| AuthHttpResponse {
                status: Some(200),
                retry_after_secs: None,
                body: AuthResponseBody::Success(loadtest_core::auth_http::AuthSuccess {
                    access_token,
                    ticket: None,
                    character_id,
                    services: None,
                }),
            };
            match request {
                AuthHttpRequest::Register { .. } => success(None, None),
                AuthHttpRequest::Login { .. } => success(Some("in-memory-token".into()), None),
                AuthHttpRequest::ListCharacters { .. } => success(None, None),
                AuthHttpRequest::CreateCharacter { .. } => AuthHttpResponse {
                    status: Some(400),
                    retry_after_secs: None,
                    body: AuthResponseBody::BusinessError("INVALID_CHARACTER_NAME".into()),
                },
                _ => panic!("create failure fixture must stop at character creation"),
            }
        }
    }

    fn config() -> loadtest_core::LoadTestConfig {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "environment": {"name": "local", "kind": "local"},
            "targets": {"auth_http": "http://127.0.0.1:3000", "game_proxy": "kcp://127.0.0.1:4000"},
            "budget": {"max_virtual_players": 1, "max_login_qps": 1.0, "max_new_connections_per_second": 1.0, "max_business_messages_per_second": 1.0, "max_messages_per_connection_per_second": 1.0, "max_duration_secs": 1, "max_total_operations": 1, "max_error_rate": 0.1, "max_connection_failure_rate": 0.1, "max_p99_ms": 1, "max_data_writes": 1},
            "scenario": {"name": "auth", "load": {"type": "fixed_concurrency", "virtual_players": 1, "duration_secs": 1}, "writes_data": true},
            "reports_root": "reports",
            "prepare_reports_root": "prepare"
        })).unwrap()
    }

    #[test]
    fn live_prepare_commands_reject_missing_execute_confirmation_or_secret_config() {
        let config = config();
        let base = Cli::parse(vec!["apply".into(), "--config".into(), "test.json".into()]).unwrap();
        assert!(
            base.require_live_gate(&config, PrepareCommand::Apply, 1)
                .is_err()
        );
        let missing_private = Cli::parse(vec![
            "apply".into(),
            "--config".into(),
            "test.json".into(),
            "--execute".into(),
            "--confirm-write".into(),
            "local".into(),
        ])
        .unwrap();
        assert!(
            missing_private
                .require_live_gate(&config, PrepareCommand::Apply, 1)
                .is_err()
        );
        let over_budget = Cli::parse(vec![
            "apply".into(),
            "--config".into(),
            "test.json".into(),
            "--execute".into(),
            "--confirm-write".into(),
            "local".into(),
            "--private-config".into(),
            "private.json".into(),
        ])
        .unwrap();
        let mut write_budget_config = config.clone();
        write_budget_config.budget.max_total_operations = 100;
        assert!(
            over_budget
                .require_live_gate(&write_budget_config, PrepareCommand::Apply, 1)
                .unwrap_err()
                .contains("max_data_writes")
        );
        let mut operation_budget_config = config;
        operation_budget_config.budget.max_data_writes = 100;
        assert!(
            over_budget
                .require_live_gate(&operation_budget_config, PrepareCommand::Apply, 1)
                .unwrap_err()
                .contains("max_total_operations")
        );
    }

    #[test]
    fn fake_prepare_persists_partial_failure_and_resumes_from_manifest() {
        let root = std::env::temp_dir().join(format!("loadtest-prepare-{}", std::process::id()));
        let manifest_path = root.join("manifest.json");
        let manifest = AccountManifest::planned("local", &AccountPrepareConfig::default(), 1, 1);
        write_manifest(&manifest_path, &manifest).unwrap();
        let mut live_config = config();
        live_config.budget.max_login_qps = 1_000.0;
        live_config.budget.max_new_connections_per_second = 1_000.0;
        live_config.budget.max_business_messages_per_second = 1_000.0;
        live_config.budget.max_messages_per_connection_per_second = 1_000.0;
        live_config.budget.max_total_operations = 100;
        live_config.budget.max_data_writes = 100;
        let protection = DryRunProtection::new(&live_config);
        let ctrl_c = Arc::new(AtomicBool::new(false));
        let mut abort = AbortController::default();
        let mut admission = AuthDispatchAdmission::new(&live_config.budget).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);

        let mut failing = FakeAuthHttpService::scripted([FakeAuthOutcome::Timeout]);
        let first = apply_manifest(
            &mut failing,
            &TestSecretProvider,
            read_manifest(&manifest_path).unwrap(),
            "loadtest",
            &manifest_path,
            &mut admission,
            deadline,
            &mut abort,
            &ctrl_c,
            &protection,
            None,
        )
        .unwrap();
        assert_eq!(first.metrics.failed, 1);
        assert_eq!(
            read_manifest(&manifest_path).unwrap().accounts[0].preparation_state,
            AccountPreparationState::Failed
        );

        let mut succeeding = FakeAuthHttpService::scripted([FakeAuthOutcome::Success; 5]);
        let resumed = apply_manifest(
            &mut succeeding,
            &TestSecretProvider,
            read_manifest(&manifest_path).unwrap(),
            "loadtest",
            &manifest_path,
            &mut admission,
            deadline,
            &mut abort,
            &ctrl_c,
            &protection,
            None,
        )
        .unwrap();
        assert_eq!(resumed.metrics.verified, 1);
        let persisted = read_manifest(&manifest_path).unwrap();
        assert_eq!(
            persisted.accounts[0].preparation_state,
            AccountPreparationState::Verified
        );
        assert_eq!(
            persisted.accounts[0].character_readiness,
            CharacterReadiness::Ready
        );
        let output = std::fs::read_to_string(&manifest_path).unwrap();
        assert!(!output.contains("test-password-not-reported"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_character_creation_is_not_counted_as_created() {
        let root = std::env::temp_dir().join(format!(
            "loadtest-prepare-create-failure-{}",
            std::process::id()
        ));
        let manifest_path = root.join("manifest.json");
        let manifest = AccountManifest::planned("local", &AccountPrepareConfig::default(), 1, 1);
        write_manifest(&manifest_path, &manifest).unwrap();
        let mut live_config = config();
        live_config.budget.max_login_qps = 1_000.0;
        live_config.budget.max_new_connections_per_second = 1_000.0;
        live_config.budget.max_business_messages_per_second = 1_000.0;
        live_config.budget.max_messages_per_connection_per_second = 1_000.0;
        live_config.budget.max_total_operations = 100;
        live_config.budget.max_data_writes = 100;
        let protection = DryRunProtection::new(&live_config);
        let ctrl_c = Arc::new(AtomicBool::new(false));
        let mut abort = AbortController::default();
        let mut admission = AuthDispatchAdmission::new(&live_config.budget).unwrap();
        let mut transport = CreateFailureTransport;
        let result = apply_manifest(
            &mut transport,
            &TestSecretProvider,
            read_manifest(&manifest_path).unwrap(),
            "loadtest",
            &manifest_path,
            &mut admission,
            Instant::now() + Duration::from_secs(1),
            &mut abort,
            &ctrl_c,
            &protection,
            None,
        )
        .unwrap();
        assert_eq!(result.metrics.failed, 1);
        assert_eq!(result.metrics.characters_created, 0);
        std::fs::remove_dir_all(root).unwrap();
    }
}
