use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;
use url::{Host, Url};

use crate::error::AgentError;
use crate::keys::KeyMaterial;

const RESULT_FIXED_RESERVE_BYTES: u64 = 262_144;
const MIN_OUTPUT_BYTES: u64 = 4_096;

pub trait Environment {
    fn get(&self, name: &str) -> Result<Option<String>, AgentError>;
}

pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn get(&self, name: &str) -> Result<Option<String>, AgentError> {
        match env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(env::VarError::NotPresent) => Ok(None),
            Err(env::VarError::NotUnicode(_)) => {
                Err(AgentError::config(name, "value must be valid UTF-8"))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLimits {
    pub auth_ttl_ms: u64,
    pub command_ttl_ms: u64,
    pub clock_skew_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub max_command_timeout_ms: u64,
    pub cancel_timeout_ms: u64,
    pub max_output_bytes: u64,
    pub ws_max_message_bytes: u64,
}

pub struct LoggingConfig {
    level: String,
    enable_console: bool,
    enable_file: bool,
    directory: PathBuf,
}

impl LoggingConfig {
    pub fn level(&self) -> &str {
        &self.level
    }

    pub const fn enable_console(&self) -> bool {
        self.enable_console
    }

    pub const fn enable_file(&self) -> bool {
        self.enable_file
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
}

pub struct AuditConfig {
    enabled: bool,
    program: Option<String>,
    timeout_ms: u64,
}

impl AuditConfig {
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn program(&self) -> Option<&str> {
        self.program.as_deref()
    }

    pub const fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }
}

pub struct AgentConfig {
    admin_api_ws_url: Url,
    agent_id: String,
    project_id: String,
    root: PathBuf,
    codex_bin: OsString,
    dry_run: bool,
    danger_full_access: bool,
    legacy_shell_configured: bool,
    limits: AgentLimits,
    ws_write_timeout_ms: u64,
    audit: AuditConfig,
    logging: LoggingConfig,
    keys: KeyMaterial,
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentConfig")
            .field("ws_endpoint", &self.safe_ws_endpoint())
            .field("agent_id", &self.agent_id)
            .field("project_id", &self.project_id)
            .field("root", &"[REDACTED]")
            .field("codex_bin", &"[REDACTED]")
            .field("dry_run", &self.dry_run)
            .field("danger_full_access", &self.danger_full_access)
            .field("legacy_shell_configured", &self.legacy_shell_configured)
            .field("limits", &self.limits)
            .field("ws_write_timeout_ms", &self.ws_write_timeout_ms)
            .field("keys", &self.keys)
            .finish_non_exhaustive()
    }
}

impl AgentConfig {
    pub fn from_process() -> Result<Self, AgentError> {
        Self::from_environment(&ProcessEnvironment)
    }

    pub fn from_environment(environment: &impl Environment) -> Result<Self, AgentError> {
        let admin_api_ws_url = parse_ws_url(&required(environment, "ADMIN_API_WS_URL")?)?;
        let agent_id = parse_identifier(
            "MYFORGE_AGENT_ID",
            required(environment, "MYFORGE_AGENT_ID")?,
        )?;
        let project_id = parse_identifier(
            "MYFORGE_PROJECT_ID",
            required(environment, "MYFORGE_PROJECT_ID")?,
        )?;

        let private_key_path = required_path(environment, "MYFORGE_AGENT_PRIVATE_KEY_PATH")?;
        let agent_public_key_path = required_path(environment, "MYFORGE_AGENT_PUBLIC_KEY_PATH")?;
        let server_public_key_path = required_path(environment, "MYFORGE_SERVER_PUBLIC_KEY_PATH")?;
        let root = required_path(environment, "MYFORGE_ROOT")?;
        let keys = KeyMaterial::load(
            &private_key_path,
            &agent_public_key_path,
            &server_public_key_path,
        )?;

        let codex_bin = parse_bounded_text(
            "MYFORGE_CODEX_BIN",
            environment
                .get("MYFORGE_CODEX_BIN")?
                .unwrap_or_else(|| "codex".to_string()),
            1_024,
        )?;
        let dry_run = strict_boolean(environment, "MYFORGE_DRY_RUN", false)?;
        let danger_full_access =
            strict_boolean(environment, "MYFORGE_CODEX_DANGEROUS_FULL_ACCESS", false)?;
        let audit_enabled = strict_boolean(environment, "MYFORGE_AUDIT_ENABLED", false)?;
        let audit_program = optional_bounded_text(environment, "MYFORGE_AUDIT_PROGRAM", 512)?;
        let legacy_shell_configured = parse_legacy_shell(environment)?;
        let parsed_limits = parse_limits(environment)?;
        let logging = parse_logging(environment)?;

        Ok(Self {
            admin_api_ws_url,
            agent_id,
            project_id,
            root,
            codex_bin: OsString::from(codex_bin),
            dry_run,
            danger_full_access,
            legacy_shell_configured,
            limits: parsed_limits.protocol,
            ws_write_timeout_ms: parsed_limits.ws_write_timeout_ms,
            audit: AuditConfig {
                enabled: audit_enabled,
                program: audit_program,
                timeout_ms: parsed_limits.audit_timeout_ms,
            },
            logging,
            keys,
        })
    }

    pub fn admin_api_ws_url(&self) -> &Url {
        &self.admin_api_ws_url
    }

    pub fn safe_ws_endpoint(&self) -> String {
        sanitize_ws_url(&self.admin_api_ws_url)
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn codex_bin(&self) -> &OsString {
        &self.codex_bin
    }

    pub const fn dry_run(&self) -> bool {
        self.dry_run
    }

    pub const fn danger_full_access(&self) -> bool {
        self.danger_full_access
    }

    pub const fn legacy_shell_configured(&self) -> bool {
        self.legacy_shell_configured
    }

    pub const fn limits(&self) -> AgentLimits {
        self.limits
    }

    pub const fn ws_write_timeout_ms(&self) -> u64 {
        self.ws_write_timeout_ms
    }

    pub fn audit(&self) -> &AuditConfig {
        &self.audit
    }

    pub fn logging(&self) -> &LoggingConfig {
        &self.logging
    }

    pub fn keys(&self) -> &KeyMaterial {
        &self.keys
    }
}

fn required(environment: &impl Environment, name: &str) -> Result<String, AgentError> {
    environment
        .get(name)?
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AgentError::config(name, "required value is missing"))
}

fn required_path(environment: &impl Environment, name: &str) -> Result<PathBuf, AgentError> {
    let value = required(environment, name)?;
    if value.chars().any(char::is_control) {
        return Err(AgentError::config(name, "path contains control characters"));
    }
    Ok(PathBuf::from(value))
}

fn parse_identifier(name: &str, value: String) -> Result<String, AgentError> {
    let bytes = value.as_bytes();
    let valid = (1..=128).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .skip(1)
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid {
        return Err(AgentError::config(name, "invalid identifier"));
    }
    Ok(value)
}

fn parse_ws_url(value: &str) -> Result<Url, AgentError> {
    let url = Url::parse(value)
        .map_err(|_| AgentError::config("ADMIN_API_WS_URL", "invalid WebSocket URL"))?;
    if !matches!(url.scheme(), "ws" | "wss") || url.host().is_none() {
        return Err(AgentError::config(
            "ADMIN_API_WS_URL",
            "URL must use ws or wss and include a host",
        ));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AgentError::config(
            "ADMIN_API_WS_URL",
            "userinfo, query, and fragment are not allowed",
        ));
    }
    if url.scheme() == "ws" && !is_loopback_host(url.host()) {
        return Err(AgentError::config(
            "ADMIN_API_WS_URL",
            "non-local endpoints must use wss",
        ));
    }
    Ok(url)
}

fn is_loopback_host(host: Option<Host<&str>>) -> bool {
    match host {
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

pub fn sanitize_ws_url(url: &Url) -> String {
    let mut safe = url.clone();
    let _ = safe.set_username("");
    let _ = safe.set_password(None);
    safe.set_query(None);
    safe.set_fragment(None);
    safe.to_string()
}

fn strict_boolean(
    environment: &impl Environment,
    name: &str,
    default: bool,
) -> Result<bool, AgentError> {
    let Some(raw) = environment.get(name)? else {
        return Ok(default);
    };
    match raw.trim_matches(|character: char| character.is_ascii_whitespace()) {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(AgentError::config(name, "invalid boolean")),
    }
}

fn parse_decimal(
    environment: &impl Environment,
    name: &str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> Result<u64, AgentError> {
    let Some(raw) = environment.get(name)? else {
        return Ok(default);
    };
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AgentError::config(name, "expected decimal integer"));
    }
    let value = raw
        .parse::<u64>()
        .map_err(|_| AgentError::config(name, "decimal integer is out of range"))?;
    if !(minimum..=maximum).contains(&value) {
        return Err(AgentError::config(
            name,
            "value is outside the allowed range",
        ));
    }
    Ok(value)
}

struct ParsedLimits {
    protocol: AgentLimits,
    ws_write_timeout_ms: u64,
    audit_timeout_ms: u64,
}

fn parse_limits(environment: &impl Environment) -> Result<ParsedLimits, AgentError> {
    let limits = AgentLimits {
        auth_ttl_ms: parse_decimal(environment, "MYFORGE_AUTH_TTL_MS", 60_000, 5_000, 300_000)?,
        command_ttl_ms: parse_decimal(
            environment,
            "MYFORGE_COMMAND_TTL_MS",
            60_000,
            5_000,
            300_000,
        )?,
        clock_skew_ms: parse_decimal(environment, "MYFORGE_CLOCK_SKEW_MS", 5_000, 0, 30_000)?,
        heartbeat_interval_ms: parse_decimal(
            environment,
            "MYFORGE_HEARTBEAT_INTERVAL_MS",
            15_000,
            1_000,
            60_000,
        )?,
        max_command_timeout_ms: parse_decimal(
            environment,
            "MYFORGE_MAX_COMMAND_TIMEOUT_MS",
            600_000,
            1_000,
            1_800_000,
        )?,
        cancel_timeout_ms: parse_decimal(
            environment,
            "MYFORGE_CANCEL_TIMEOUT_MS",
            10_000,
            1_000,
            30_000,
        )?,
        max_output_bytes: parse_decimal(
            environment,
            "MYFORGE_MAX_OUTPUT_BYTES",
            1_048_576,
            4_096,
            4_194_304,
        )?,
        ws_max_message_bytes: parse_decimal(
            environment,
            "MYFORGE_WS_MAX_MESSAGE_BYTES",
            16_777_216,
            524_288,
            33_554_432,
        )?,
    };
    let ws_write_timeout_ms = parse_decimal(
        environment,
        "MYFORGE_WS_WRITE_TIMEOUT_MS",
        5_000,
        1_000,
        30_000,
    )?;
    let audit_timeout_ms = parse_decimal(
        environment,
        "MYFORGE_AUDIT_TIMEOUT_MS",
        30_000,
        1_000,
        120_000,
    )?;

    let double_skew = limits.clock_skew_ms.saturating_mul(2);
    if double_skew >= limits.auth_ttl_ms {
        return Err(AgentError::config(
            "MYFORGE_AUTH_TTL_MS",
            "must be greater than twice MYFORGE_CLOCK_SKEW_MS",
        ));
    }
    if double_skew >= limits.command_ttl_ms {
        return Err(AgentError::config(
            "MYFORGE_COMMAND_TTL_MS",
            "must be greater than twice MYFORGE_CLOCK_SKEW_MS",
        ));
    }
    if limits.cancel_timeout_ms > limits.max_command_timeout_ms {
        return Err(AgentError::config(
            "MYFORGE_CANCEL_TIMEOUT_MS",
            "must not exceed MYFORGE_MAX_COMMAND_TIMEOUT_MS",
        ));
    }
    if ws_write_timeout_ms >= limits.auth_ttl_ms || ws_write_timeout_ms >= limits.command_ttl_ms {
        return Err(AgentError::config(
            "MYFORGE_WS_WRITE_TIMEOUT_MS",
            "must be less than auth and command TTL",
        ));
    }
    if limits.ws_max_message_bytes < RESULT_FIXED_RESERVE_BYTES + 12 * MIN_OUTPUT_BYTES {
        return Err(AgentError::config(
            "MYFORGE_WS_MAX_MESSAGE_BYTES",
            "message budget cannot preserve minimum output",
        ));
    }

    Ok(ParsedLimits {
        protocol: limits,
        ws_write_timeout_ms,
        audit_timeout_ms,
    })
}

fn parse_legacy_shell(environment: &impl Environment) -> Result<bool, AgentError> {
    let Some(raw) = environment.get("MYFORGE_SHELL")? else {
        return Ok(false);
    };
    let value = raw.trim();
    if value.is_empty() || value.len() > 64 || value.chars().any(char::is_control) {
        return Err(AgentError::config(
            "MYFORGE_SHELL",
            "legacy value must be 1 to 64 UTF-8 bytes without control characters",
        ));
    }
    Ok(true)
}

fn parse_bounded_text(name: &str, raw: String, maximum_bytes: usize) -> Result<String, AgentError> {
    let value = raw.trim().to_string();
    if value.is_empty() || value.len() > maximum_bytes || value.chars().any(char::is_control) {
        return Err(AgentError::config(
            name,
            "value is empty, too long, or contains control characters",
        ));
    }
    Ok(value)
}

fn optional_bounded_text(
    environment: &impl Environment,
    name: &str,
    maximum_bytes: usize,
) -> Result<Option<String>, AgentError> {
    environment
        .get(name)?
        .map(|raw| parse_bounded_text(name, raw, maximum_bytes))
        .transpose()
}

fn parse_logging(environment: &impl Environment) -> Result<LoggingConfig, AgentError> {
    let level = parse_bounded_text(
        "LOG_LEVEL",
        environment
            .get("LOG_LEVEL")?
            .unwrap_or_else(|| "info".to_string()),
        128,
    )?;
    tracing_subscriber::EnvFilter::try_new(&level)
        .map_err(|_| AgentError::config("LOG_LEVEL", "invalid tracing filter"))?;
    let enable_console = strict_boolean(environment, "LOG_ENABLE_CONSOLE", true)?;
    let enable_file = strict_boolean(environment, "LOG_ENABLE_FILE", true)?;
    let directory = PathBuf::from(
        environment
            .get("LOG_DIR")?
            .unwrap_or_else(|| "logs/myforge-agent".to_string()),
    );
    if directory.as_os_str().is_empty() {
        return Err(AgentError::config("LOG_DIR", "path is empty"));
    }

    Ok(LoggingConfig {
        level,
        enable_console,
        enable_file,
        directory,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use ed25519_dalek::SigningKey;
    use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
    use pkcs8::LineEnding;
    use tempfile::tempdir;

    use super::*;
    use crate::ErrorCode;

    #[derive(Default)]
    struct MapEnvironment(HashMap<String, String>);

    impl Environment for MapEnvironment {
        fn get(&self, name: &str) -> Result<Option<String>, AgentError> {
            Ok(self.0.get(name).cloned())
        }
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        environment: MapEnvironment,
    }

    impl Fixture {
        fn valid() -> Self {
            let directory = tempdir().unwrap();
            let root = directory.path().join("external-myforge");
            fs::create_dir(&root).unwrap();
            let signing = SigningKey::from_bytes(&[42; 32]);
            let private_path = directory.path().join("agent-private.pem");
            let public_path = directory.path().join("agent-public.pem");
            fs::write(
                &private_path,
                signing.to_pkcs8_pem(LineEnding::LF).unwrap().as_bytes(),
            )
            .unwrap();
            fs::write(
                &public_path,
                signing
                    .verifying_key()
                    .to_public_key_pem(LineEnding::LF)
                    .unwrap(),
            )
            .unwrap();
            let environment = MapEnvironment(HashMap::from([
                (
                    "ADMIN_API_WS_URL".to_string(),
                    "wss://admin.example.test/api/v1/myforge/ws".to_string(),
                ),
                ("MYFORGE_AGENT_ID".to_string(), "dev-pc-001".to_string()),
                (
                    "MYFORGE_PROJECT_ID".to_string(),
                    "myforge-local".to_string(),
                ),
                (
                    "MYFORGE_AGENT_PRIVATE_KEY_PATH".to_string(),
                    private_path.to_string_lossy().into_owned(),
                ),
                (
                    "MYFORGE_AGENT_PUBLIC_KEY_PATH".to_string(),
                    public_path.to_string_lossy().into_owned(),
                ),
                (
                    "MYFORGE_SERVER_PUBLIC_KEY_PATH".to_string(),
                    public_path.to_string_lossy().into_owned(),
                ),
                (
                    "MYFORGE_ROOT".to_string(),
                    root.to_string_lossy().into_owned(),
                ),
            ]));
            Self {
                _directory: directory,
                environment,
            }
        }

        fn set(&mut self, name: &str, value: &str) {
            self.environment
                .0
                .insert(name.to_string(), value.to_string());
        }
    }

    #[test]
    fn loads_required_configuration_and_documented_defaults() {
        let fixture = Fixture::valid();
        let config = AgentConfig::from_environment(&fixture.environment).unwrap();

        assert_eq!(config.agent_id(), "dev-pc-001");
        assert_eq!(config.project_id(), "myforge-local");
        assert_eq!(config.codex_bin(), "codex");
        assert!(!config.dry_run());
        assert!(!config.danger_full_access());
        assert!(!config.audit().enabled());
        assert!(!config.legacy_shell_configured());
        assert_eq!(
            config.limits(),
            AgentLimits {
                auth_ttl_ms: 60_000,
                command_ttl_ms: 60_000,
                clock_skew_ms: 5_000,
                heartbeat_interval_ms: 15_000,
                max_command_timeout_ms: 600_000,
                cancel_timeout_ms: 10_000,
                max_output_bytes: 1_048_576,
                ws_max_message_bytes: 16_777_216,
            }
        );
        assert_eq!(config.ws_write_timeout_ms(), 5_000);
        assert_eq!(config.audit().timeout_ms(), 30_000);
    }

    #[test]
    fn rejects_each_missing_required_value() {
        for name in [
            "ADMIN_API_WS_URL",
            "MYFORGE_AGENT_ID",
            "MYFORGE_PROJECT_ID",
            "MYFORGE_AGENT_PRIVATE_KEY_PATH",
            "MYFORGE_AGENT_PUBLIC_KEY_PATH",
            "MYFORGE_SERVER_PUBLIC_KEY_PATH",
            "MYFORGE_ROOT",
        ] {
            let mut fixture = Fixture::valid();
            fixture.environment.0.remove(name);
            let error = AgentConfig::from_environment(&fixture.environment).unwrap_err();
            assert_eq!(error.code(), ErrorCode::ConfigInvalid, "{name}");
            assert!(error.message().contains(name), "{name}");
        }
    }

    #[test]
    fn validates_identifier_and_websocket_url_contract() {
        for (name, value) in [
            ("MYFORGE_AGENT_ID", "-invalid"),
            ("MYFORGE_PROJECT_ID", "contains space"),
            ("MYFORGE_AGENT_ID", "é"),
        ] {
            let mut fixture = Fixture::valid();
            fixture.set(name, value);
            assert!(AgentConfig::from_environment(&fixture.environment).is_err());
        }

        for value in [
            "https://admin.example.test/api/v1/myforge/ws",
            "ws://admin.example.test/api/v1/myforge/ws",
            "wss://user:password@admin.example.test/api/v1/myforge/ws",
            "wss://admin.example.test/api/v1/myforge/ws?token=secret",
        ] {
            let mut fixture = Fixture::valid();
            fixture.set("ADMIN_API_WS_URL", value);
            let error = AgentConfig::from_environment(&fixture.environment).unwrap_err();
            assert_eq!(error.code(), ErrorCode::ConfigInvalid, "{value}");
            assert!(!error.to_string().contains(value));
        }

        let mut local = Fixture::valid();
        local.set("ADMIN_API_WS_URL", "ws://127.0.0.1:3001/api/v1/myforge/ws");
        assert!(AgentConfig::from_environment(&local.environment).is_ok());
    }

    #[test]
    fn strict_booleans_accept_only_the_four_documented_values() {
        for (raw, expected) in [("true", true), ("1", true), ("false", false), ("0", false)] {
            let mut fixture = Fixture::valid();
            fixture.set("MYFORGE_DRY_RUN", raw);
            fixture.set("MYFORGE_CODEX_DANGEROUS_FULL_ACCESS", raw);
            fixture.set("MYFORGE_AUDIT_ENABLED", "false");
            fixture.set("LOG_ENABLE_CONSOLE", raw);
            let config = AgentConfig::from_environment(&fixture.environment).unwrap();
            assert_eq!(config.dry_run(), expected, "{raw}");
            assert_eq!(config.danger_full_access(), expected, "{raw}");
            assert_eq!(config.logging().enable_console(), expected, "{raw}");
        }

        for raw in ["", "TRUE", "False", "yes", "on", "tru"] {
            let mut fixture = Fixture::valid();
            fixture.set("MYFORGE_DRY_RUN", raw);
            let error = AgentConfig::from_environment(&fixture.environment).unwrap_err();
            assert_eq!(error.code(), ErrorCode::ConfigInvalid, "{raw}");
            assert!(error.message().contains("invalid boolean"), "{raw}");
            assert!(!error.to_string().contains("tru:"));

            let mut fixture = Fixture::valid();
            fixture.set("MYFORGE_CODEX_DANGEROUS_FULL_ACCESS", raw);
            let error = AgentConfig::from_environment(&fixture.environment).unwrap_err();
            assert_eq!(error.code(), ErrorCode::ConfigInvalid, "{raw}");
            assert!(error.message().contains("invalid boolean"), "{raw}");
        }
    }

    #[test]
    fn decimal_values_reject_non_decimal_and_out_of_range_input() {
        for raw in [" 5000", "+5000", "5_000", "5.0", "5e3", ""] {
            let mut fixture = Fixture::valid();
            fixture.set("MYFORGE_AUTH_TTL_MS", raw);
            assert!(
                AgentConfig::from_environment(&fixture.environment).is_err(),
                "{raw}"
            );
        }

        let mut fixture = Fixture::valid();
        fixture.set("MYFORGE_MAX_OUTPUT_BYTES", "4095");
        assert!(AgentConfig::from_environment(&fixture.environment).is_err());
        fixture.set("MYFORGE_MAX_OUTPUT_BYTES", "4194305");
        assert!(AgentConfig::from_environment(&fixture.environment).is_err());
    }

    #[test]
    fn every_numeric_setting_enforces_its_documented_closed_range() {
        let cases = [
            ("MYFORGE_AUTH_TTL_MS", Some("4999"), "300001"),
            ("MYFORGE_COMMAND_TTL_MS", Some("4999"), "300001"),
            ("MYFORGE_CLOCK_SKEW_MS", None, "30001"),
            ("MYFORGE_HEARTBEAT_INTERVAL_MS", Some("999"), "60001"),
            ("MYFORGE_MAX_COMMAND_TIMEOUT_MS", Some("999"), "1800001"),
            ("MYFORGE_CANCEL_TIMEOUT_MS", Some("999"), "30001"),
            ("MYFORGE_MAX_OUTPUT_BYTES", Some("4095"), "4194305"),
            ("MYFORGE_WS_MAX_MESSAGE_BYTES", Some("524287"), "33554433"),
            ("MYFORGE_WS_WRITE_TIMEOUT_MS", Some("999"), "30001"),
            ("MYFORGE_AUDIT_TIMEOUT_MS", Some("999"), "120001"),
        ];

        for (name, below, above) in cases {
            for raw in below.into_iter().chain([above]) {
                let mut fixture = Fixture::valid();
                fixture.set(name, raw);
                let error = AgentConfig::from_environment(&fixture.environment).unwrap_err();
                assert!(error.message().contains(name), "{name}={raw}: {error}");
                assert!(error.message().contains("allowed range"));
            }
        }
    }

    #[test]
    fn numeric_closed_range_boundaries_are_accepted_together() {
        let minimums = [
            ("MYFORGE_AUTH_TTL_MS", "5000"),
            ("MYFORGE_COMMAND_TTL_MS", "5000"),
            ("MYFORGE_CLOCK_SKEW_MS", "0"),
            ("MYFORGE_HEARTBEAT_INTERVAL_MS", "1000"),
            ("MYFORGE_MAX_COMMAND_TIMEOUT_MS", "1000"),
            ("MYFORGE_CANCEL_TIMEOUT_MS", "1000"),
            ("MYFORGE_MAX_OUTPUT_BYTES", "4096"),
            ("MYFORGE_WS_MAX_MESSAGE_BYTES", "524288"),
            ("MYFORGE_WS_WRITE_TIMEOUT_MS", "1000"),
            ("MYFORGE_AUDIT_TIMEOUT_MS", "1000"),
        ];
        let maximums = [
            ("MYFORGE_AUTH_TTL_MS", "300000"),
            ("MYFORGE_COMMAND_TTL_MS", "300000"),
            ("MYFORGE_CLOCK_SKEW_MS", "30000"),
            ("MYFORGE_HEARTBEAT_INTERVAL_MS", "60000"),
            ("MYFORGE_MAX_COMMAND_TIMEOUT_MS", "1800000"),
            ("MYFORGE_CANCEL_TIMEOUT_MS", "30000"),
            ("MYFORGE_MAX_OUTPUT_BYTES", "4194304"),
            ("MYFORGE_WS_MAX_MESSAGE_BYTES", "33554432"),
            ("MYFORGE_WS_WRITE_TIMEOUT_MS", "30000"),
            ("MYFORGE_AUDIT_TIMEOUT_MS", "120000"),
        ];

        for values in [minimums, maximums] {
            let mut fixture = Fixture::valid();
            for (name, value) in values {
                fixture.set(name, value);
            }
            AgentConfig::from_environment(&fixture.environment).unwrap();
        }
    }

    #[test]
    fn protocol_limit_projection_has_exact_register_fields() {
        let fixture = Fixture::valid();
        let config = AgentConfig::from_environment(&fixture.environment).unwrap();
        let value = serde_json::to_value(config.limits()).unwrap();
        let mut fields: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        fields.sort();

        assert_eq!(
            fields,
            [
                "authTtlMs",
                "cancelTimeoutMs",
                "clockSkewMs",
                "commandTtlMs",
                "heartbeatIntervalMs",
                "maxCommandTimeoutMs",
                "maxOutputBytes",
                "wsMaxMessageBytes",
            ]
        );
    }

    #[test]
    fn validates_agent_limit_invariants() {
        let cases = [
            (
                vec![
                    ("MYFORGE_CLOCK_SKEW_MS", "2500"),
                    ("MYFORGE_AUTH_TTL_MS", "5000"),
                ],
                "MYFORGE_AUTH_TTL_MS",
            ),
            (
                vec![
                    ("MYFORGE_CLOCK_SKEW_MS", "2500"),
                    ("MYFORGE_COMMAND_TTL_MS", "5000"),
                ],
                "MYFORGE_COMMAND_TTL_MS",
            ),
            (
                vec![
                    ("MYFORGE_MAX_COMMAND_TIMEOUT_MS", "1000"),
                    ("MYFORGE_CANCEL_TIMEOUT_MS", "1001"),
                ],
                "MYFORGE_CANCEL_TIMEOUT_MS",
            ),
            (
                vec![
                    ("MYFORGE_CLOCK_SKEW_MS", "0"),
                    ("MYFORGE_AUTH_TTL_MS", "5000"),
                    ("MYFORGE_WS_WRITE_TIMEOUT_MS", "5000"),
                ],
                "MYFORGE_WS_WRITE_TIMEOUT_MS",
            ),
        ];

        for (values, expected_name) in cases {
            let mut fixture = Fixture::valid();
            for (name, value) in values {
                fixture.set(name, value);
            }
            let error = AgentConfig::from_environment(&fixture.environment).unwrap_err();
            assert!(error.message().contains(expected_name), "{error}");
        }
    }

    #[test]
    fn legacy_shell_is_presence_only_and_strictly_validated() {
        let mut fixture = Fixture::valid();
        fixture.set("MYFORGE_SHELL", "  powershell  ");
        let config = AgentConfig::from_environment(&fixture.environment).unwrap();
        assert!(config.legacy_shell_configured());

        for raw in ["", "  ", "bad\nshell"] {
            let mut fixture = Fixture::valid();
            fixture.set("MYFORGE_SHELL", raw);
            assert!(AgentConfig::from_environment(&fixture.environment).is_err());
        }

        let mut fixture = Fixture::valid();
        fixture.set("MYFORGE_SHELL", &"界".repeat(22));
        assert!(AgentConfig::from_environment(&fixture.environment).is_err());
    }

    #[test]
    fn url_sanitizer_never_exposes_userinfo_query_or_fragment() {
        let url = Url::parse(
            "wss://user:password@admin.example.test/api/v1/myforge/ws?token=secret#fragment",
        )
        .unwrap();
        let summary = sanitize_ws_url(&url);
        assert_eq!(summary, "wss://admin.example.test/api/v1/myforge/ws");
        assert!(!summary.contains("password"));
        assert!(!summary.contains("secret"));
    }
}
