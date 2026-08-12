use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::auth_http::AuthRunMetrics;
use crate::calibration::CalibrationReport;
use crate::config::{HardBudget, LoadTestConfig};
use crate::metrics::MetricsSnapshot;
use crate::resource::GeneratorResources;

pub const MAX_ERROR_SAMPLES: usize = 100;
pub const MAX_ERROR_SAMPLE_BYTES: usize = 2 * 1024;

const PHASE_LATENCY_KEYS: [(&str, &str); 8] = [
    ("login_ms", "Login"),
    ("ticket_ms", "Ticket"),
    ("connect_ms", "Connect"),
    ("auth_ms", "Proxy auth"),
    ("room_join_ms", "Room join"),
    ("room_first_frame_ms", "First frame"),
    ("room_recovery_ms", "Reconnect"),
    ("gameplay_step_ms", "Gameplay operation"),
];

const FLOW_COUNTER_KEYS: [(&str, &str); 13] = [
    ("connections_opened", "Connections opened"),
    ("connections_active", "Connections active"),
    ("frame_inputs_sent", "Frame inputs sent"),
    ("frame_inputs_received", "Frame inputs received"),
    ("frame_bundles_received", "Frame bundles received"),
    ("gameplay_bytes_sent", "Gameplay bytes sent"),
    ("gameplay_bytes_received", "Gameplay bytes received"),
    ("gameplay_messages_sent", "Gameplay messages sent"),
    ("frame_timeouts", "Frame timeouts"),
    ("frame_late_response", "Frame late responses"),
    ("frame_out_of_order", "Frame out of order"),
    ("gameplay_business_errors", "Gameplay business errors"),
    ("metrics_dropped", "Metrics dropped"),
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorSample {
    pub category: String,
    pub message: String,
    pub context: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default)]
pub struct ErrorBuffer {
    samples: Vec<ErrorSample>,
}
impl ErrorBuffer {
    pub fn push(
        &mut self,
        category: impl Into<String>,
        message: impl AsRef<str>,
        context: BTreeMap<String, String>,
    ) {
        if self.samples.len() >= MAX_ERROR_SAMPLES {
            return;
        }
        let mut sample = ErrorSample {
            category: redact_text(&category.into()),
            message: truncate(redact_text(message.as_ref()), MAX_ERROR_SAMPLE_BYTES),
            context: BTreeMap::new(),
        };
        for (key, value) in context {
            let redacted_key = redact_text(&key);
            let redacted_value = if is_sensitive_key(&key) {
                "[REDACTED]".to_string()
            } else {
                redact_text(&value)
            };
            sample.context.insert(
                redacted_key,
                truncate(redacted_value, MAX_ERROR_SAMPLE_BYTES),
            );
        }
        self.samples.push(sample);
    }
    pub fn samples(&self) -> &[ErrorSample] {
        &self.samples
    }
}

#[derive(Debug, Clone)]
pub struct ReportInput<'a> {
    pub run_id: &'a str,
    pub config: &'a LoadTestConfig,
    pub effective_budget: &'a HardBudget,
    pub status: &'a str,
    pub abort_reason: Option<&'a str>,
    pub shutdown_phase: Option<&'a str>,
    pub deadline_unix_ms: u64,
    pub graceful_shutdown_ms: u64,
    pub started_unix_ms: u64,
    pub ended_unix_ms: u64,
    pub metrics: MetricsSnapshot,
    pub resources: GeneratorResources,
    pub errors: &'a ErrorBuffer,
    pub auth_metrics: Option<&'a AuthRunMetrics>,
    pub calibration: Option<&'a CalibrationReport>,
}

#[derive(Debug, Serialize)]
struct RunJson<'a> {
    schema_version: u32,
    run_id: &'a str,
    status: &'a str,
    abort_reason: Option<&'a str>,
    shutdown_phase: Option<&'a str>,
    environment: &'a str,
    targets: Vec<String>,
    scenario_hash: String,
    tool_git_commit: &'static str,
    account_batch: &'a str,
    auth_metrics_available: bool,
    started_unix_ms: u64,
    ended_unix_ms: u64,
    deadline_unix_ms: u64,
    graceful_shutdown_ms: u64,
    budget: &'a HardBudget,
    generator_resources: GeneratorResources,
    calibration: Option<&'a CalibrationReport>,
}

pub fn write_report(root: &Path, input: ReportInput<'_>) -> std::io::Result<PathBuf> {
    let report_dir = root.join(input.run_id);
    fs::create_dir_all(&report_dir)?;
    let targets = input
        .config
        .parsed_targets()
        .map_err(std::io::Error::other)?
        .iter()
        .map(|target| target.safe_summary())
        .collect();
    let scenario_json = serde_json::to_vec(&input.config.scenario).expect("scenario serializes");
    let run = RunJson {
        schema_version: crate::SCHEMA_VERSION,
        run_id: input.run_id,
        status: input.status,
        abort_reason: input.abort_reason,
        shutdown_phase: input.shutdown_phase,
        environment: &input.config.environment.name,
        targets,
        scenario_hash: format!("{:x}", Sha256::digest(scenario_json)),
        tool_git_commit: tool_git_commit(),
        account_batch: &input.config.account_prepare.batch,
        auth_metrics_available: input.auth_metrics.is_some(),
        started_unix_ms: input.started_unix_ms,
        ended_unix_ms: input.ended_unix_ms,
        deadline_unix_ms: input.deadline_unix_ms,
        graceful_shutdown_ms: input.graceful_shutdown_ms,
        budget: input.effective_budget,
        generator_resources: input.resources,
        calibration: input.calibration,
    };
    write_json(report_dir.join("run.json"), &run)?;
    write_json(report_dir.join("metrics.json"), &input.metrics)?;
    if let Some(auth_metrics) = input.auth_metrics {
        write_json(report_dir.join("auth-metrics.json"), auth_metrics)?;
    }
    write_timeseries(report_dir.join("timeseries.csv"), &input.metrics)?;
    write_error_samples(report_dir.join("errors.jsonl"), input.errors.samples())?;
    write_summary(
        report_dir.join("summary.md"),
        &run,
        &input.metrics,
        input.auth_metrics,
        input.calibration,
    )?;
    Ok(report_dir)
}

pub fn tool_git_commit() -> &'static str {
    option_env!("MYSERVER_GIT_COMMIT").unwrap_or("build_metadata_missing:compile_env_not_set")
}

pub fn redact_json(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| {
                    let sensitive = is_sensitive_key(&key);
                    (
                        key,
                        if sensitive {
                            Value::String("[REDACTED]".into())
                        } else {
                            redact_json(value)
                        },
                    )
                })
                .collect::<Map<_, _>>(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(redact_json).collect()),
        Value::String(value) => Value::String(redact_text(&value)),
        value => value,
    }
}

pub fn redact_text(input: &str) -> String {
    let mut output = input.to_string();
    for key in [
        "password",
        "access_token",
        "token",
        "ticket",
        "authorization",
        "admin_token",
        "secret",
        "account",
        "email",
        "player_id",
        "character_id",
    ] {
        output = redact_assignment(&output, key);
    }
    redact_email_tokens(&output)
}

fn redact_email_tokens(input: &str) -> String {
    input
        .split_inclusive(|character: char| {
            character.is_whitespace() || character == ',' || character == '&'
        })
        .map(|token| {
            let trimmed = token.trim_end_matches(|character: char| {
                character.is_whitespace() || character == ',' || character == '&'
            });
            if trimmed.contains('@') {
                token.replacen(trimmed, "[REDACTED]", 1)
            } else {
                token.to_string()
            }
        })
        .collect()
}

fn redact_assignment(input: &str, key: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut cursor = 0;
    let mut output = String::new();
    while let Some(relative) = lower[cursor..].find(key) {
        let start = cursor + relative;
        output.push_str(&input[cursor..start]);
        let after_key = start + key.len();
        output.push_str(&input[start..after_key]);
        let rest = &input[after_key..];
        if let Some(delimiter) = rest
            .chars()
            .next()
            .filter(|delimiter| matches!(delimiter, '=' | ':' | ' '))
        {
            output.push(delimiter);
            output.push_str("[REDACTED]");
            let consumed = delimiter.len_utf8();
            let tail = &rest[consumed..];
            let end = tail
                .find(|character: char| {
                    character.is_whitespace() || character == ',' || character == '&'
                })
                .unwrap_or(tail.len());
            cursor = after_key + consumed + end;
        } else {
            cursor = after_key;
        }
    }
    output.push_str(&input[cursor..]);
    output
}

fn is_sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    if matches!(
        key.as_str(),
        "ticket_attempts" | "ticket_successes" | "ticket_success_rate" | "ticket_issued"
    ) {
        return false;
    }
    [
        "password",
        "token",
        "ticket",
        "authorization",
        "secret",
        "email",
        "identity",
    ]
    .iter()
    .any(|fragment| key.contains(fragment))
        || matches!(key.as_str(), "account" | "player" | "character")
        || matches!(key.as_str(), "account_id" | "player_id" | "character_id")
}

fn truncate(mut value: String, max_bytes: usize) -> String {
    if value.len() > max_bytes {
        value.truncate(max_bytes);
        value.push_str("...[TRUNCATED]");
    }
    value
}

fn write_json(path: PathBuf, value: &impl Serialize) -> std::io::Result<()> {
    fs::write(
        path,
        serde_json::to_vec_pretty(&redact_json(
            serde_json::to_value(value).expect("report serializes"),
        ))
        .expect("redacted report serializes"),
    )
}

fn write_timeseries(path: PathBuf, metrics: &MetricsSnapshot) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    writeln!(
        file,
        "monotonic_ms,virtual_players,scheduler_lag_ms,queue_depth,metrics_dropped"
    )?;
    writeln!(
        file,
        "0,{},{},{},{}",
        metrics.counters.get("virtual_players").unwrap_or(&0),
        metrics.counters.get("scheduler_lag_ms").unwrap_or(&0),
        metrics.counters.get("scheduler_queue_depth").unwrap_or(&0),
        metrics.counters.get("metrics_dropped").unwrap_or(&0)
    )
}

fn write_error_samples(path: PathBuf, samples: &[ErrorSample]) -> std::io::Result<()> {
    let mut file = File::create(path)?;
    for sample in samples {
        writeln!(
            file,
            "{}",
            serde_json::to_string(&redact_json(
                serde_json::to_value(sample).expect("error sample serializes")
            ))
            .expect("redacted error sample serializes")
        )?;
    }
    Ok(())
}

fn write_summary(
    path: PathBuf,
    run: &RunJson<'_>,
    metrics: &MetricsSnapshot,
    auth_metrics: Option<&AuthRunMetrics>,
    calibration: Option<&CalibrationReport>,
) -> std::io::Result<()> {
    let operation = metrics.histograms.get("operation_ms");
    let auth = auth_metrics.map_or_else(String::new, |auth| {
        format!(
            "\nAuth login attempt QPS: {:.3}\n\nAuth login success rate: {:.3}\n\nAuth P50/P95/P99 latency (ms): {}/{}/{}\n\nAuth ticket success rate: {:.3}\n\nAuth rate-limit rate: {:.3}\n\nAuth connection-failure rate: {:.3}\n\nAuth HTTP status categories: {}\n\nAuth business code categories: {}\n\nAuth virtual player states: {}\n",
            auth.login_qps(),
            auth.login_success_rate(),
            auth.p50_ms(),
            auth.p95_ms(),
            auth.p99_ms(),
            auth.ticket_success_rate(),
            auth.rate_limit_rate(),
            auth.connection_failure_rate(),
            serde_json::to_string(&auth.http_statuses).expect("auth status metrics serialize"),
            serde_json::to_string(&auth.business_codes).expect("auth business metrics serialize"),
            serde_json::to_string(&auth.virtual_player_states).expect("auth state metrics serialize"),
        )
    });
    let calibration = calibration.map_or_else(String::new, |calibration| {
        format!(
            "\nCalibration generator capacity: {}\n\nCalibration service stable capacity: {}\n\nCalibration system burst capacity: {}\n",
            capacity_summary(&calibration.generator_capacity),
            capacity_summary(&calibration.service_stable_capacity),
            capacity_summary(&calibration.system_burst_capacity),
        )
    });
    let phases = phase_latency_summary(metrics);
    let flow = flow_counter_summary(metrics);
    fs::write(
        path,
        format!(
            "# Load Test Summary\n\nStatus: {}\n\nEnvironment: {}\n\nP50/P90/P95/P99/max operation latency (ms): {}/{}/{}/{}/{}\n\nLatency by phase:\n{}\n\nConnections, frames, throughput, and errors:\n{}\n\nAbort reason: {}\n{}{}",
            run.status,
            run.environment,
            operation.map_or(0, |histogram| histogram.percentile(0.50)),
            operation.map_or(0, |histogram| histogram.percentile(0.90)),
            operation.map_or(0, |histogram| histogram.percentile(0.95)),
            operation.map_or(0, |histogram| histogram.percentile(0.99)),
            operation.map_or(0, |histogram| histogram.max()),
            phases,
            flow,
            run.abort_reason.unwrap_or("none"),
            auth,
            calibration,
        ),
    )
}

fn phase_latency_summary(metrics: &MetricsSnapshot) -> String {
    PHASE_LATENCY_KEYS
        .iter()
        .map(|(key, label)| {
            let histogram = metrics.histograms.get(*key);
            format!(
                "- {label}: P50/P95/P99={}/{}/{} ms",
                histogram.map_or(0, |value| value.percentile(0.50)),
                histogram.map_or(0, |value| value.percentile(0.95)),
                histogram.map_or(0, |value| value.percentile(0.99)),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn flow_counter_summary(metrics: &MetricsSnapshot) -> String {
    FLOW_COUNTER_KEYS
        .iter()
        .map(|(key, label)| format!("- {label}: {}", metrics.counters.get(*key).unwrap_or(&0)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn capacity_summary(capacity: &crate::calibration::CapacityConclusion) -> String {
    match capacity {
        crate::calibration::CapacityConclusion::Available { virtual_players } => {
            format!("available ({virtual_players} virtual players)")
        }
        crate::calibration::CapacityConclusion::Unavailable { reason } => {
            format!("unavailable ({reason})")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_http::{FakeAuthHttpService, FakeAuthOutcome, execute_auth_operations};
    use crate::config::*;
    use crate::resource::ResourceSampler;

    fn config() -> LoadTestConfig {
        serde_json::from_value(serde_json::json!({"schema_version":1,"environment":{"name":"local","kind":"local"},"targets":{"auth_http":"http://127.0.0.1:3000","game_proxy":"kcp://127.0.0.1:4000"},"budget":{"max_virtual_players":1,"max_login_qps":1.0,"max_new_connections_per_second":1.0,"max_business_messages_per_second":1.0,"max_messages_per_connection_per_second":1.0,"max_duration_secs":1,"max_total_operations":1,"max_error_rate":0.1,"max_connection_failure_rate":0.1,"max_p99_ms":100,"max_data_writes":0},"scenario":{"name":"safe","load":{"type":"fixed_concurrency","virtual_players":1,"duration_secs":1}},"reports_root":"reports","prepare_reports_root":"prepare"})).unwrap()
    }
    #[test]
    fn report_redacts_secrets_and_bounds_errors() {
        let mut errors = ErrorBuffer::default();
        for _ in 0..(MAX_ERROR_SAMPLES + 2) {
            errors.push(
                "request",
                "ticket=super-secret-token password:abc",
                BTreeMap::from([("email".into(), "me@example.com".into())]),
            );
        }
        assert_eq!(errors.samples().len(), MAX_ERROR_SAMPLES);
        let text = serde_json::to_string(errors.samples()).unwrap();
        assert!(!text.contains("super-secret-token"));
        assert!(!text.contains("me@example.com"));
        let root = std::env::temp_dir().join(format!("loadtest-report-{}", std::process::id()));
        let value = config();
        let report = write_report(
            &root,
            ReportInput {
                run_id: "safe-run",
                config: &value,
                effective_budget: &value.budget,
                status: "completed",
                abort_reason: None,
                shutdown_phase: None,
                deadline_unix_ms: 3,
                graceful_shutdown_ms: 1,
                started_unix_ms: 1,
                ended_unix_ms: 2,
                metrics: MetricsSnapshot::default(),
                resources: ResourceSampler.sample(0, 0, 0),
                errors: &errors,
                auth_metrics: None,
                calibration: None,
            },
        )
        .unwrap();
        assert!(report.join("run.json").is_file());
        assert!(report.join("summary.md").is_file());
        let run = fs::read_to_string(report.join("run.json")).unwrap();
        assert!(!run.contains("127.0.0.1"));
        assert!(run.contains("\"account_batch\": \"default\""));
        assert!(run.contains("\"max_virtual_players\": 1"));
        let commit = tool_git_commit();
        assert!(
            (commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit()))
                || commit.starts_with("build_metadata_missing:")
        );
        assert!(run.contains(commit));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ticket_metric_names_remain_visible_while_ticket_values_are_redacted() {
        let rendered = serde_json::to_string(&redact_json(serde_json::json!({
            "ticket_attempts": 3,
            "ticket_successes": 2,
            "ticket": "opaque-secret-value"
        })))
        .unwrap();
        assert!(rendered.contains("ticket_attempts"));
        assert!(rendered.contains("ticket_successes"));
        assert!(!rendered.contains("opaque-secret-value"));
    }

    #[test]
    fn summary_separates_fixed_latency_phases_and_flow_counters() {
        let mut metrics = MetricsSnapshot::default();
        for key in [
            "login_ms",
            "ticket_ms",
            "connect_ms",
            "auth_ms",
            "room_join_ms",
            "room_first_frame_ms",
            "room_recovery_ms",
            "gameplay_step_ms",
        ] {
            metrics.histograms.entry(key.into()).or_default().record(10);
        }
        metrics.counters.insert("connections_opened".into(), 2);
        metrics.counters.insert("frame_bundles_received".into(), 3);
        metrics.counters.insert("gameplay_bytes_received".into(), 4);
        metrics
            .counters
            .insert("gameplay_business_errors".into(), 5);
        let summary = format!(
            "{}\n{}",
            phase_latency_summary(&metrics),
            flow_counter_summary(&metrics)
        );
        for expected in [
            "Login: P50/P95/P99=10/10/10 ms",
            "Ticket: P50/P95/P99=10/10/10 ms",
            "Connect: P50/P95/P99=10/10/10 ms",
            "Proxy auth: P50/P95/P99=10/10/10 ms",
            "Room join: P50/P95/P99=10/10/10 ms",
            "First frame: P50/P95/P99=10/10/10 ms",
            "Reconnect: P50/P95/P99=10/10/10 ms",
            "Gameplay operation: P50/P95/P99=10/10/10 ms",
            "Connections opened: 2",
            "Frame bundles received: 3",
            "Gameplay bytes received: 4",
            "Gameplay business errors: 5",
        ] {
            assert!(summary.contains(expected), "missing {expected}");
        }
    }

    #[test]
    fn merged_staged_flow_metrics_keep_http_business_timeout_and_ticket_categories() {
        let mut auth_metrics = AuthRunMetrics::default();
        let mut success = FakeAuthHttpService::scripted([FakeAuthOutcome::Success; 4]);
        let success = execute_auth_operations(
            &mut success,
            &[
                AuthOperation::Login,
                AuthOperation::ListCharacters,
                AuthOperation::SelectCharacter,
                AuthOperation::IssueTicket,
            ],
            "loadtest_000001",
            "loadtest_local_default_000001",
            "in-memory-only",
            |_, _| Ok(std::time::Duration::MAX),
        );
        assert!(success.error.is_none());
        auth_metrics.merge(&success.metrics);

        let mut rejected = FakeAuthHttpService::scripted([FakeAuthOutcome::BusinessError]);
        let rejected = execute_auth_operations(
            &mut rejected,
            &[AuthOperation::FailedLogin],
            "loadtest_000002",
            "loadtest_local_default_000002",
            "in-memory-only",
            |_, _| Ok(std::time::Duration::MAX),
        );
        assert!(rejected.error.is_none());
        auth_metrics.merge(&rejected.metrics);

        let mut timed_out = FakeAuthHttpService::scripted([FakeAuthOutcome::Timeout]);
        let timed_out = execute_auth_operations(
            &mut timed_out,
            &[AuthOperation::Login],
            "loadtest_000003",
            "loadtest_local_default_000003",
            "in-memory-only",
            |_, _| Ok(std::time::Duration::MAX),
        );
        assert!(timed_out.error.is_some());
        auth_metrics.merge(&timed_out.metrics);
        auth_metrics.set_wall_clock_window_ms(1_000);

        let root = std::env::temp_dir().join(format!(
            "loadtest-staged-auth-report-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        ));
        let value = config();
        let report = write_report(
            &root,
            ReportInput {
                run_id: "staged-fake-run",
                config: &value,
                effective_budget: &value.budget,
                status: "completed",
                abort_reason: None,
                shutdown_phase: None,
                deadline_unix_ms: 3,
                graceful_shutdown_ms: 1,
                started_unix_ms: 1,
                ended_unix_ms: 2,
                metrics: MetricsSnapshot::default(),
                resources: ResourceSampler.sample(0, 0, 0),
                errors: &ErrorBuffer::default(),
                auth_metrics: Some(&auth_metrics),
                calibration: None,
            },
        )
        .unwrap();
        let serialized = fs::read_to_string(report.join("auth-metrics.json")).unwrap();
        assert!(serialized.contains("http401"));
        assert!(serialized.contains("invalid_login_credentials"));
        assert!(serialized.contains("no_response"));
        assert!(serialized.contains("timeout"));
        assert!(serialized.contains("\"ticket_successes\": 1"));
        let summary = fs::read_to_string(report.join("summary.md")).unwrap();
        assert!(summary.contains("Auth connection-failure rate: 0.167"));
        fs::remove_dir_all(root).unwrap();
    }
}
