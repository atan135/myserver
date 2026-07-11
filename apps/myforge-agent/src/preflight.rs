use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::{AgentConfig, AgentLimits};
use crate::error::{AgentError, ErrorCode};

const CODEX_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

pub trait CapabilityProbe {
    fn hostname(&self) -> Result<String, AgentError>;
    fn codex_available(&self, executable: &OsStr, working_directory: &Path) -> bool;
}

pub struct SystemCapabilityProbe;

impl CapabilityProbe for SystemCapabilityProbe {
    fn hostname(&self) -> Result<String, AgentError> {
        let hostname = hostname::get()
            .map_err(|_| AgentError::config("hostname", "platform hostname is unavailable"))?
            .into_string()
            .map_err(|_| AgentError::config("hostname", "platform hostname is not UTF-8"))?;
        validate_hostname(hostname)
    }

    fn codex_available(&self, executable: &OsStr, working_directory: &Path) -> bool {
        probe_command_version(executable, working_directory, CODEX_PROBE_TIMEOUT)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeRootSummary {
    pub name: String,
    pub configured: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditAvailability {
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub profiles: Vec<String>,
    pub codex_exec: bool,
    pub fangyuan_blueprint: bool,
    pub audit: AuditAvailability,
    pub dry_run: bool,
    pub max_concurrent_tasks: u8,
}

pub struct PreflightReport {
    root_real: PathBuf,
    auditor_real: Option<PathBuf>,
    platform: String,
    hostname: String,
    agent_version: String,
    forge_root_summary: ForgeRootSummary,
    capabilities: Capabilities,
    limits: AgentLimits,
}

impl PreflightReport {
    pub fn root_real(&self) -> &Path {
        &self.root_real
    }

    pub fn auditor_real(&self) -> Option<&Path> {
        self.auditor_real.as_deref()
    }

    pub fn platform(&self) -> &str {
        &self.platform
    }

    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    pub fn agent_version(&self) -> &str {
        &self.agent_version
    }

    pub fn forge_root_summary(&self) -> &ForgeRootSummary {
        &self.forge_root_summary
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    pub const fn limits(&self) -> AgentLimits {
        self.limits
    }
}

impl std::fmt::Debug for PreflightReport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreflightReport")
            .field("platform", &self.platform)
            .field("hostname", &self.hostname)
            .field("agent_version", &self.agent_version)
            .field("forge_root_summary", &self.forge_root_summary)
            .field("capabilities", &self.capabilities)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

pub fn run_preflight(
    config: &AgentConfig,
    probe: &impl CapabilityProbe,
) -> Result<PreflightReport, AgentError> {
    let root_real = validate_root(config.root())?;
    let forge_root_summary = root_summary(&root_real)?;
    let platform = platform_name()?;
    let hostname = validate_hostname(probe.hostname()?)?;
    let codex_exec = probe.codex_available(config.codex_bin().as_os_str(), &root_real);
    if !config.dry_run() && !codex_exec {
        return Err(AgentError::new(
            ErrorCode::CodexUnavailable,
            "configured Codex binary is unavailable",
        ));
    }

    let auditor_real = if config.audit().enabled() {
        Some(validate_auditor(
            &root_real,
            config.audit().program().ok_or_else(|| {
                AgentError::new(
                    ErrorCode::AuditorInvalid,
                    "configured auditor program is missing",
                )
            })?,
        )?)
    } else {
        None
    };
    let audit = if auditor_real.is_some() {
        AuditAvailability::Available
    } else {
        AuditAvailability::Unavailable
    };

    Ok(PreflightReport {
        root_real,
        auditor_real,
        platform,
        hostname,
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        forge_root_summary,
        capabilities: Capabilities {
            profiles: vec!["codex_exec".to_string()],
            codex_exec,
            fangyuan_blueprint: true,
            audit,
            dry_run: config.dry_run(),
            max_concurrent_tasks: 1,
        },
        limits: config.limits(),
    })
}

fn validate_root(root: &Path) -> Result<PathBuf, AgentError> {
    match root.try_exists() {
        Ok(false) => {
            return Err(AgentError::new(
                ErrorCode::RootMissing,
                "configured forge root does not exist",
            ));
        }
        Err(_) => {
            return Err(AgentError::new(
                ErrorCode::RootInvalid,
                "configured forge root is inaccessible",
            ));
        }
        Ok(true) => {}
    }

    let root_real = fs::canonicalize(root).map_err(|_| {
        AgentError::new(
            ErrorCode::RootInvalid,
            "configured forge root cannot be resolved",
        )
    })?;
    let metadata = fs::metadata(&root_real).map_err(|_| {
        AgentError::new(
            ErrorCode::RootInvalid,
            "configured forge root is inaccessible",
        )
    })?;
    if !metadata.is_dir() || fs::read_dir(&root_real).is_err() {
        return Err(AgentError::new(
            ErrorCode::RootInvalid,
            "configured forge root must be a readable directory",
        ));
    }

    let configured_agent_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let configured_repository_dir = configured_agent_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| {
            AgentError::new(
                ErrorCode::RootInvalid,
                "repository boundary cannot be resolved",
            )
        })?
        .to_path_buf();
    let agent_dir = fs::canonicalize(&configured_agent_dir).unwrap_or(configured_agent_dir);
    let repository_dir =
        fs::canonicalize(&configured_repository_dir).unwrap_or(configured_repository_dir);

    if paths_overlap(&root_real, &repository_dir) || paths_overlap(&root_real, &agent_dir) {
        return Err(AgentError::new(
            ErrorCode::RootInvalid,
            "forge root must be an external workspace isolated from MyServer",
        ));
    }
    Ok(root_real)
}

fn paths_overlap(first: &Path, second: &Path) -> bool {
    first.starts_with(second) || second.starts_with(first)
}

fn root_summary(root_real: &Path) -> Result<ForgeRootSummary, AgentError> {
    let name = root_real
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| {
            !name.is_empty()
                && name.len() <= 128
                && !name.chars().any(char::is_control)
                && !name
                    .chars()
                    .any(|character| matches!(character, '\\' | '/' | ':'))
        })
        .ok_or_else(|| {
            AgentError::new(
                ErrorCode::RootInvalid,
                "forge root directory name is invalid",
            )
        })?;
    Ok(ForgeRootSummary {
        name: name.to_string(),
        configured: true,
    })
}

fn platform_name() -> Result<String, AgentError> {
    match std::env::consts::OS {
        "windows" => Ok("windows".to_string()),
        "linux" => Ok("linux".to_string()),
        "macos" => Ok("macos".to_string()),
        _ => Err(AgentError::config(
            "platform",
            "unsupported operating system",
        )),
    }
}

fn validate_hostname(hostname: String) -> Result<String, AgentError> {
    let hostname = hostname.trim().to_string();
    if hostname.is_empty() || hostname.len() > 255 || hostname.chars().any(char::is_control) {
        return Err(AgentError::config("hostname", "invalid platform hostname"));
    }
    Ok(hostname)
}

fn validate_auditor(root_real: &Path, program: &str) -> Result<PathBuf, AgentError> {
    if !valid_auditor_path(program) {
        return Err(AgentError::new(
            ErrorCode::AuditorInvalid,
            "auditor program must be a normalized relative path below tools",
        ));
    }

    let candidate = root_real.join(program);
    let real = fs::canonicalize(candidate).map_err(|_| {
        AgentError::new(
            ErrorCode::AuditorInvalid,
            "configured auditor program is unavailable",
        )
    })?;
    let metadata = fs::metadata(&real).map_err(|_| {
        AgentError::new(
            ErrorCode::AuditorInvalid,
            "configured auditor program is unavailable",
        )
    })?;
    if !real.starts_with(root_real) || !metadata.is_file() || !is_executable(&metadata) {
        return Err(AgentError::new(
            ErrorCode::AuditorInvalid,
            "configured auditor program is not an executable file inside forge root",
        ));
    }
    Ok(real)
}

fn valid_auditor_path(program: &str) -> bool {
    if program.is_empty()
        || program.len() > 512
        || !program.starts_with("tools/")
        || program.starts_with('/')
        || program.ends_with('/')
        || program.contains("//")
        || program.contains('\\')
        || program.chars().any(|character| {
            character.is_control() || matches!(character, ':' | '"' | '<' | '>' | '|' | '?' | '*')
        })
    {
        return false;
    }
    program.split('/').all(|segment| {
        !segment.is_empty()
            && !matches!(segment, "." | "..")
            && !segment.ends_with([' ', '.'])
            && !is_windows_device_name(segment)
    })
}

fn is_windows_device_name(segment: &str) -> bool {
    let stem = segment
        .split('.')
        .next()
        .unwrap_or(segment)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }

    ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix)
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
    })
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(windows)]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    true
}

fn probe_command_version(executable: &OsStr, working_directory: &Path, timeout: Duration) -> bool {
    let mut command = version_probe_command(executable, working_directory);
    let mut child = match command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn version_probe_command(executable: &OsStr, working_directory: &Path) -> Command {
    const ALLOWED_ENVIRONMENT: &[&str] = &[
        "PATH",
        "PATHEXT",
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "HOME",
        "USERPROFILE",
        "TMP",
        "TEMP",
    ];

    let mut command = Command::new(executable);
    command
        .arg("--version")
        .current_dir(working_directory)
        .env_clear();
    for name in ALLOWED_ENVIRONMENT {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ed25519_dalek::SigningKey;
    use ed25519_dalek::pkcs8::{EncodePrivateKey, EncodePublicKey};
    use pkcs8::LineEnding;
    use tempfile::tempdir;

    use super::*;
    use crate::config::Environment;

    struct FakeProbe {
        hostname: String,
        codex_available: bool,
        codex_calls: AtomicUsize,
        codex_working_directory: Mutex<Option<PathBuf>>,
    }

    impl FakeProbe {
        fn new(codex_available: bool) -> Self {
            Self {
                hostname: "test-host".to_string(),
                codex_available,
                codex_calls: AtomicUsize::new(0),
                codex_working_directory: Mutex::new(None),
            }
        }
    }

    impl CapabilityProbe for FakeProbe {
        fn hostname(&self) -> Result<String, AgentError> {
            Ok(self.hostname.clone())
        }

        fn codex_available(&self, _executable: &OsStr, working_directory: &Path) -> bool {
            self.codex_calls.fetch_add(1, Ordering::SeqCst);
            *self.codex_working_directory.lock().unwrap() = Some(working_directory.to_path_buf());
            self.codex_available
        }
    }

    struct MapEnvironment(HashMap<String, String>);

    impl Environment for MapEnvironment {
        fn get(&self, name: &str) -> Result<Option<String>, AgentError> {
            Ok(self.0.get(name).cloned())
        }
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        environment: MapEnvironment,
        root: PathBuf,
    }

    impl Fixture {
        fn valid() -> Self {
            let directory = tempdir().unwrap();
            let root = directory.path().join("external-myforge");
            fs::create_dir(&root).unwrap();
            let signing = SigningKey::from_bytes(&[17; 32]);
            let private_path = directory.path().join("private.pem");
            let public_path = directory.path().join("public.pem");
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
                ("LOG_ENABLE_FILE".to_string(), "false".to_string()),
            ]));
            Self {
                _directory: directory,
                environment,
                root,
            }
        }

        fn config(&self) -> AgentConfig {
            AgentConfig::from_environment(&self.environment).unwrap()
        }

        fn set(&mut self, name: &str, value: impl Into<String>) {
            self.environment.0.insert(name.to_string(), value.into());
        }
    }

    #[test]
    fn reports_exact_p0_capabilities_and_safe_root_summary() {
        let fixture = Fixture::valid();
        let probe = FakeProbe::new(true);
        let report = run_preflight(&fixture.config(), &probe).unwrap();

        assert_eq!(report.platform(), std::env::consts::OS);
        assert_eq!(report.hostname(), "test-host");
        assert_eq!(report.agent_version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(report.forge_root_summary().name, "external-myforge");
        assert_eq!(report.capabilities().profiles, ["codex_exec"]);
        assert!(report.capabilities().codex_exec);
        assert!(report.capabilities().fangyuan_blueprint);
        assert_eq!(report.capabilities().audit, AuditAvailability::Unavailable);
        assert!(!report.capabilities().dry_run);
        assert_eq!(report.capabilities().max_concurrent_tasks, 1);
        assert_eq!(probe.codex_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            probe.codex_working_directory.lock().unwrap().as_deref(),
            Some(report.root_real())
        );
    }

    #[test]
    fn root_summary_rejects_names_that_server_would_treat_as_paths() {
        let error = root_summary(Path::new("invalid:name")).unwrap_err();
        assert_eq!(error.code(), ErrorCode::RootInvalid);
    }

    #[test]
    fn missing_and_non_directory_roots_use_distinct_safe_errors() {
        let mut missing = Fixture::valid();
        let missing_path = missing.root.join("missing-sensitive-name");
        missing.set("MYFORGE_ROOT", missing_path.to_string_lossy());
        let error = run_preflight(&missing.config(), &FakeProbe::new(true)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::RootMissing);
        assert!(!error.to_string().contains("missing-sensitive-name"));

        let mut invalid = Fixture::valid();
        let file = invalid.root.join("not-a-directory-secret");
        fs::write(&file, "data").unwrap();
        invalid.set("MYFORGE_ROOT", file.to_string_lossy());
        let error = run_preflight(&invalid.config(), &FakeProbe::new(true)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::RootInvalid);
        assert!(!error.to_string().contains("not-a-directory-secret"));
    }

    #[test]
    fn rejects_myserver_repository_and_agent_directories() {
        let agent_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repository_dir = agent_dir.parent().unwrap().parent().unwrap().to_path_buf();
        let repository_parent = repository_dir.parent().unwrap().to_path_buf();
        for root in [agent_dir, repository_dir, repository_parent] {
            let mut fixture = Fixture::valid();
            fixture.set("MYFORGE_ROOT", root.to_string_lossy());
            let error = run_preflight(&fixture.config(), &FakeProbe::new(true)).unwrap_err();
            assert_eq!(error.code(), ErrorCode::RootInvalid);
            assert!(error.message().contains("external workspace"));
            assert!(!error.to_string().contains(root.to_string_lossy().as_ref()));
        }
    }

    #[test]
    fn dry_run_reports_failed_codex_probe_but_non_dry_run_fails() {
        let fixture = Fixture::valid();
        let error = run_preflight(&fixture.config(), &FakeProbe::new(false)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::CodexUnavailable);

        let mut dry_run = Fixture::valid();
        dry_run.set("MYFORGE_DRY_RUN", "true");
        let probe = FakeProbe::new(false);
        let report = run_preflight(&dry_run.config(), &probe).unwrap();
        assert!(report.capabilities().dry_run);
        assert!(!report.capabilities().codex_exec);
        assert_eq!(probe.codex_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn enabled_auditor_must_be_an_executable_inside_tools() {
        let mut fixture = Fixture::valid();
        let tools = fixture.root.join("tools");
        fs::create_dir(&tools).unwrap();
        let auditor = tools.join("fangyuan-audit");
        fs::write(&auditor, "test executable").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&auditor, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fixture.set("MYFORGE_AUDIT_ENABLED", "true");
        fixture.set("MYFORGE_AUDIT_PROGRAM", "tools/fangyuan-audit");
        let report = run_preflight(&fixture.config(), &FakeProbe::new(true)).unwrap();
        assert_eq!(report.capabilities().audit, AuditAvailability::Available);
        let auditor_real = fs::canonicalize(&auditor).unwrap();
        assert_eq!(report.auditor_real(), Some(auditor_real.as_path()));

        fixture.set("MYFORGE_AUDIT_PROGRAM", "../outside");
        let error = run_preflight(&fixture.config(), &FakeProbe::new(true)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::AuditorInvalid);
    }

    #[test]
    fn enabled_auditor_requires_a_program_during_preflight() {
        let mut fixture = Fixture::valid();
        fixture.set("MYFORGE_AUDIT_ENABLED", "true");

        let error = run_preflight(&fixture.config(), &FakeProbe::new(true)).unwrap_err();
        assert_eq!(error.code(), ErrorCode::AuditorInvalid);
        assert!(error.message().contains("program is missing"));
    }

    #[test]
    fn codex_probe_command_has_fixed_argument_and_minimal_environment() {
        let directory = tempdir().unwrap();
        let root_real = fs::canonicalize(directory.path()).unwrap();
        let command = version_probe_command(OsStr::new("codex"), &root_real);
        let arguments: Vec<_> = command.get_args().collect();
        let environment: Vec<_> = command
            .get_envs()
            .filter_map(|(name, value)| value.map(|_| name.to_string_lossy().into_owned()))
            .collect();

        assert_eq!(arguments, ["--version"]);
        assert_eq!(command.get_current_dir(), Some(root_real.as_path()));
        assert!(
            environment.iter().all(|name| matches!(
                name.as_str(),
                "PATH"
                    | "PATHEXT"
                    | "SYSTEMROOT"
                    | "WINDIR"
                    | "COMSPEC"
                    | "HOME"
                    | "USERPROFILE"
                    | "TMP"
                    | "TEMP"
            )),
            "unexpected environment allowlist: {environment:?}"
        );
        assert!(!environment.iter().any(|name| name.starts_with("MYFORGE_")));
    }

    #[test]
    fn auditor_path_rejects_windows_device_names_on_every_platform() {
        for path in [
            "tools/CON",
            "tools/PRN.exe",
            "tools/AuX/auditor",
            "tools/valid/nUl.json",
            "tools/COM1.exe",
            "tools/valid/lPt9/auditor",
        ] {
            assert!(!valid_auditor_path(path), "reserved path accepted: {path}");
        }
    }

    #[test]
    fn auditor_path_allows_non_device_names_with_similar_prefixes() {
        for path in [
            "tools/COM10",
            "tools/LPT10.exe",
            "tools/console-auditor",
            "tools/auxiliary/auditor",
        ] {
            assert!(valid_auditor_path(path), "valid path rejected: {path}");
        }
    }
}
