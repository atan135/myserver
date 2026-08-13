use serde::Serialize;

use crate::auth_budget::{AuthRunBudgetEstimate, estimate_auth_run};
use crate::config::{EnvironmentKind, HardBudget, LoadModel, LoadTestConfig, RunAccess};
use crate::side_services::ServiceDescriptor;

const SUPPORTED_LOAD_MODELS: [&str; 4] = ["fixed_concurrency", "arrival_rate", "staged", "burst"];
const PROTECTION_CONTRACT: &str = "fail_closed: revalidate DNS, certificate, descriptor, and environment identity before ramp and every controller tick";

/// A safe-to-display execution contract emitted before any load scheduling,
/// transport setup, or report directory write occurs.
#[derive(Debug, Serialize)]
pub struct PreflightSummary<'a> {
    pub schema_version: u32,
    pub command: &'a str,
    pub environment: &'a str,
    pub environment_kind: EnvironmentKind,
    pub targets: Vec<String>,
    pub account_batch: &'a str,
    pub planned_account_count: u32,
    pub account_manifest_supplied: bool,
    pub auth_budget_estimate: Option<AuthRunBudgetEstimate>,
    pub auth_http_admission_mapping: Option<&'static str>,
    pub selected_load_model: &'a LoadModel,
    pub supported_load_models: [&'static str; 4],
    pub effective_budget: &'a HardBudget,
    pub effective_duration_secs: u64,
    pub deadline_unix_ms: u64,
    pub writes_data: bool,
    pub max_data_writes: u64,
    pub dry_run: bool,
    pub remote_gate: &'static str,
    pub protection_revalidation: &'static str,
    pub side_service_steps: usize,
    pub side_service_descriptors: Vec<String>,
}

pub fn summarize_run<'a>(
    command: &'a str,
    config: &'a LoadTestConfig,
    budget: &'a HardBudget,
    access: RunAccess<'_>,
    deadline_unix_ms: u64,
    dry_run: bool,
    account_manifest_supplied: bool,
) -> Result<PreflightSummary<'a>, String> {
    let targets = config
        .parsed_targets()
        .map_err(|error| error.to_string())?
        .iter()
        .map(|target| target.safe_summary())
        .collect();
    let remote_gate = if config.environment.kind.is_remote() {
        if access.allow_remote
            && access.confirmation == Some(config.environment.name.as_str())
            && config
                .environment
                .approval_reference
                .as_deref()
                .is_some_and(|reference| !reference.trim().is_empty())
            && !config.environment.allowed_hosts.is_empty()
            && !config.environment.allowed_ips.is_empty()
        {
            "remote_allowlist_approval_confirmation_verified"
        } else {
            "remote_gate_not_verified"
        }
    } else {
        "local_loopback_only"
    };
    let (side_service_steps, side_service_descriptors) = config
        .scenario
        .side_services
        .as_ref()
        .map(|side| {
            let steps = side
                .executable_plan(budget)
                .map(|plan| plan.steps.len())
                .unwrap_or(0);
            let descriptors = [
                side.chat.as_ref().and_then(|c| c.descriptor.as_ref()),
                side.mail.as_ref().and_then(|c| c.descriptor.as_ref()),
                side.announce.as_ref().and_then(|c| c.descriptor.as_ref()),
                side.r#match.as_ref().and_then(|c| c.descriptor.as_ref()),
            ]
            .into_iter()
            .flatten()
            .map(ServiceDescriptor::safe_summary)
            .collect();
            (steps, descriptors)
        })
        .unwrap_or_default();
    Ok(PreflightSummary {
        schema_version: crate::SCHEMA_VERSION,
        command,
        environment: &config.environment.name,
        environment_kind: config.environment.kind,
        targets,
        account_batch: &config.account_prepare.batch,
        planned_account_count: config
            .account_prepare
            .account_count
            .unwrap_or(budget.max_virtual_players),
        account_manifest_supplied,
        auth_budget_estimate: config
            .scenario
            .auth
            .as_ref()
            .map(|_| estimate_auth_run(&config.scenario, budget))
            .transpose()?,
        auth_http_admission_mapping: config.scenario.auth.as_ref().map(|_| {
            "each outbound attempt is admitted as one HTTP operation, one new HTTP/1.1 connection, one business message, and one message on a worst-case connection; Connection: close disables reuse"
        }),
        selected_load_model: &config.scenario.load,
        supported_load_models: SUPPORTED_LOAD_MODELS,
        effective_budget: budget,
        effective_duration_secs: budget.max_duration_secs,
        deadline_unix_ms,
        writes_data: config.scenario.writes_data,
        max_data_writes: budget.max_data_writes,
        dry_run,
        remote_gate,
        protection_revalidation: PROTECTION_CONTRACT,
        side_service_steps,
        side_service_descriptors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BudgetOverride;

    fn config() -> LoadTestConfig {
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "environment": { "name": "local", "kind": "local" },
            "targets": {
                "auth_http": "http://127.0.0.1:3000",
                "game_proxy": "kcp://127.0.0.1:4000"
            },
            "budget": {
                "max_virtual_players": 2,
                "max_login_qps": 2.0,
                "max_new_connections_per_second": 2.0,
                "max_business_messages_per_second": 4.0,
                "max_messages_per_connection_per_second": 2.0,
                "max_duration_secs": 10,
                "max_total_operations": 20,
                "max_error_rate": 0.1,
                "max_connection_failure_rate": 0.1,
                "max_p99_ms": 1000,
                "max_data_writes": 0
            },
            "scenario": {
                "name": "local-dry-run",
                "load": {
                    "type": "fixed_concurrency",
                    "virtual_players": 2,
                    "duration_secs": 1
                },
                "writes_data": false
            },
            "reports_root": "reports",
            "prepare_reports_root": "prepare"
        }))
        .unwrap()
    }

    #[test]
    fn preflight_is_structured_and_does_not_expose_raw_targets_or_credentials() {
        let config = config();
        let budget = config.effective_budget(&BudgetOverride::default()).unwrap();
        let summary = summarize_run(
            "run",
            &config,
            &budget,
            RunAccess::default(),
            100,
            true,
            false,
        )
        .unwrap();
        let output = serde_json::to_string(&summary).unwrap();
        assert!(output.contains("local_loopback_only"));
        assert!(output.contains("fixed_concurrency"));
        assert!(output.contains("arrival_rate"));
        assert!(output.contains("default"));
        assert!(output.contains("fail_closed"));
        assert!(!output.contains("127.0.0.1"));
        assert!(!output.contains("password"));
        assert!(!output.contains("token"));
        assert!(!output.contains("ticket"));
    }
}
