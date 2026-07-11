use clap::Parser;
use serde::Serialize;

use crate::config::{AgentConfig, AgentLimits};
use crate::error::{AgentError, ErrorCode};
use crate::preflight::{Capabilities, ForgeRootSummary, PreflightReport};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunIntent {
    Check,
    Connect,
}

#[derive(Debug, Parser)]
#[command(
    name = "myforge-agent",
    version,
    about = "MyServer myforge execution agent"
)]
pub struct Cli {
    #[arg(
        long,
        help = "Run local configuration and capability checks without connecting"
    )]
    check: bool,
}

impl Cli {
    pub const fn intent(&self) -> RunIntent {
        if self.check {
            RunIntent::Check
        } else {
            RunIntent::Connect
        }
    }
}

pub trait Connector {
    fn connect(&self, config: &AgentConfig, preflight: &PreflightReport) -> Result<(), AgentError>;
}

pub struct PendingWebSocketConnector;

impl Connector for PendingWebSocketConnector {
    fn connect(
        &self,
        _config: &AgentConfig,
        _preflight: &PreflightReport,
    ) -> Result<(), AgentError> {
        Err(AgentError::new(
            ErrorCode::ConnectNotImplemented,
            "WebSocket transport is scheduled for the next implementation stage",
        ))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupSummary<'a> {
    pub agent_id: &'a str,
    pub project_id: &'a str,
    pub ws_endpoint: String,
    pub platform: &'a str,
    pub hostname: &'a str,
    pub agent_version: &'a str,
    pub forge_root: &'a ForgeRootSummary,
    pub capabilities: &'a Capabilities,
    pub limits: AgentLimits,
    pub ws_write_timeout_ms: u64,
    pub audit_timeout_ms: u64,
    pub legacy_shell_configured: bool,
    pub log_console: bool,
    pub log_file: bool,
}

pub fn startup_summary<'a>(
    config: &'a AgentConfig,
    preflight: &'a PreflightReport,
) -> StartupSummary<'a> {
    StartupSummary {
        agent_id: config.agent_id(),
        project_id: config.project_id(),
        ws_endpoint: config.safe_ws_endpoint(),
        platform: preflight.platform(),
        hostname: preflight.hostname(),
        agent_version: preflight.agent_version(),
        forge_root: preflight.forge_root_summary(),
        capabilities: preflight.capabilities(),
        limits: preflight.limits(),
        ws_write_timeout_ms: config.ws_write_timeout_ms(),
        audit_timeout_ms: config.audit().timeout_ms(),
        legacy_shell_configured: config.legacy_shell_configured(),
        log_console: config.logging().enable_console(),
        log_file: config.logging().enable_file(),
    }
}

pub fn dispatch(
    intent: RunIntent,
    config: &AgentConfig,
    preflight: &PreflightReport,
    connector: &impl Connector,
) -> Result<(), AgentError> {
    let summary = startup_summary(config, preflight);
    let summary_json = serde_json::to_string(&summary)
        .map_err(|_| AgentError::config("startup summary", "serialization failed"))?;
    tracing::info!(summary = %summary_json, "myforge-agent preflight completed");
    if config.legacy_shell_configured() {
        tracing::warn!(
            event = "MYFORGE_SHELL_IGNORED",
            legacy_shell_configured = true,
            "legacy shell configuration is ignored"
        );
    }

    match intent {
        RunIntent::Check => {
            tracing::info!(connect = false, "local preflight check passed");
            Ok(())
        }
        RunIntent::Connect => connector.connect(config, preflight),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use clap::Parser;
    use ed25519_dalek::SigningKey;
    use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
    use pkcs8::LineEnding;
    use tempfile::tempdir;

    use super::*;
    use crate::config::Environment;
    use crate::preflight::{CapabilityProbe, run_preflight};

    struct MapEnvironment(HashMap<String, String>);

    impl Environment for MapEnvironment {
        fn get(&self, name: &str) -> Result<Option<String>, AgentError> {
            Ok(self.0.get(name).cloned())
        }
    }

    struct FakeProbe;

    impl CapabilityProbe for FakeProbe {
        fn hostname(&self) -> Result<String, AgentError> {
            Ok("safe-host".to_string())
        }

        fn codex_available(
            &self,
            _executable: &OsStr,
            _working_directory: &std::path::Path,
        ) -> bool {
            true
        }
    }

    struct CountingConnector(AtomicUsize);

    impl Connector for CountingConnector {
        fn connect(
            &self,
            _config: &AgentConfig,
            _preflight: &PreflightReport,
        ) -> Result<(), AgentError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    fn config_fixture() -> (tempfile::TempDir, AgentConfig) {
        let directory = tempdir().unwrap();
        let root = directory.path().join("secret-external-root");
        fs::create_dir(&root).unwrap();
        let signing = SigningKey::from_bytes(&[31; 32]);
        let private_path = directory.path().join("secret-private.pem");
        let public_path = directory.path().join("secret-public.pem");
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
            (
                "MYFORGE_CODEX_BIN".to_string(),
                "C:\\sensitive-tools\\codex.exe".to_string(),
            ),
            (
                "MYFORGE_SHELL".to_string(),
                "private-shell-value".to_string(),
            ),
            ("LOG_ENABLE_FILE".to_string(), "false".to_string()),
        ]));
        let config = AgentConfig::from_environment(&environment).unwrap();
        (directory, config)
    }

    #[test]
    fn cli_distinguishes_check_and_connect_intents() {
        assert_eq!(
            Cli::try_parse_from(["myforge-agent", "--check"])
                .unwrap()
                .intent(),
            RunIntent::Check
        );
        assert_eq!(
            Cli::try_parse_from(["myforge-agent"]).unwrap().intent(),
            RunIntent::Connect
        );
    }

    #[test]
    fn check_mode_never_calls_connector() {
        let (_directory, config) = config_fixture();
        let report = run_preflight(&config, &FakeProbe).unwrap();
        let connector = CountingConnector(AtomicUsize::new(0));

        dispatch(RunIntent::Check, &config, &report, &connector).unwrap();
        assert_eq!(connector.0.load(Ordering::SeqCst), 0);

        dispatch(RunIntent::Connect, &config, &report, &connector).unwrap();
        assert_eq!(connector.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn startup_summary_contains_only_safe_configuration_projection() {
        let (_directory, config) = config_fixture();
        let report = run_preflight(&config, &FakeProbe).unwrap();
        let json = serde_json::to_string(&startup_summary(&config, &report)).unwrap();

        assert!(json.contains("secret-external-root"));
        assert!(json.contains("legacyShellConfigured\":true"));
        assert!(!json.contains(config.root().to_string_lossy().as_ref()));
        assert!(!json.contains("secret-private.pem"));
        assert!(!json.contains("secret-public.pem"));
        assert!(!json.contains("private-shell-value"));
        assert!(!json.contains("sensitive-tools"));

        let debug = format!("{config:?}");
        assert!(!debug.contains(config.root().to_string_lossy().as_ref()));
        assert!(!debug.contains("secret-private.pem"));
        assert!(!debug.contains("sensitive-tools"));
        assert!(!debug.contains("private-shell-value"));
    }
}
