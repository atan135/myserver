use crate::abort::AbortController;
use crate::config::{EnvironmentKind, LoadTestConfig};
use crate::side_services::{AuthServicesPayload, SideTransportKind};
use reqwest::blocking::Client;
use reqwest::header::CONNECTION;
use reqwest::tls::TlsInfo;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::Mutex;
use std::time::Duration;

/// Runtime target identity checks that must succeed before a controller may
/// continue ramping load. Transport-backed implementations are responsible for
/// certificate and protocol descriptor verification once those clients exist.
pub trait RuntimeProtection {
    fn verify_dns(&self) -> Result<(), String>;
    fn verify_certificate(&self) -> Result<(), String>;
    fn verify_descriptor(&self) -> Result<(), String>;
    fn verify_environment_identity(&self) -> Result<(), String>;

    fn revalidate(&self) -> Result<(), String> {
        self.verify_dns()?;
        self.verify_certificate()?;
        self.verify_descriptor()?;
        self.verify_environment_identity()
    }
}

/// Transport adapters can feed this identity snapshot to a baseline before
/// the first request and on every controller tick. Keeping the comparison in
/// the core makes DNS rebinding, certificate rotation and descriptor drift
/// fail closed even when the concrete HTTP/KCP client is unavailable offline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetProtectionSnapshot {
    pub dns_ips: BTreeSet<IpAddr>,
    pub certificate_fingerprint: String,
    pub descriptor_digest: String,
    pub environment: String,
}

impl TargetProtectionSnapshot {
    fn validate(&self) -> Result<(), String> {
        if self.dns_ips.is_empty() {
            return Err("target protection snapshot has no resolved DNS addresses".into());
        }
        if self.certificate_fingerprint.trim().is_empty() {
            return Err("target protection snapshot is missing certificate identity".into());
        }
        if self.descriptor_digest.trim().is_empty() {
            return Err("target protection snapshot is missing descriptor identity".into());
        }
        if self.environment.trim().is_empty() {
            return Err("target protection snapshot is missing environment identity".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct TargetProtectionBaseline {
    expected: Option<TargetProtectionSnapshot>,
}

impl TargetProtectionBaseline {
    pub fn observe(&mut self, snapshot: TargetProtectionSnapshot) -> Result<(), String> {
        snapshot.validate()?;
        let Some(expected) = &self.expected else {
            self.expected = Some(snapshot);
            return Ok(());
        };
        if expected.dns_ips != snapshot.dns_ips {
            return Err("target DNS identity changed (possible rebinding)".into());
        }
        if expected.certificate_fingerprint != snapshot.certificate_fingerprint {
            return Err("target certificate identity changed".into());
        }
        if expected.descriptor_digest != snapshot.descriptor_digest {
            return Err("target protocol descriptor identity changed".into());
        }
        if expected.environment != snapshot.environment {
            return Err("target environment identity changed".into());
        }
        Ok(())
    }

    pub fn expected(&self) -> Option<&TargetProtectionSnapshot> {
        self.expected.as_ref()
    }
}

/// Called before ramping and on every controller-loop iteration. A failed
/// identity check is fail-closed: no later scheduler iteration may create more
/// sessions once the shared abort controller has been signalled.
pub fn revalidate_or_abort(
    protection: &impl RuntimeProtection,
    abort: &mut AbortController,
) -> Option<String> {
    match protection.revalidate() {
        Ok(()) => None,
        Err(error) => {
            abort.check_protection(false);
            Some(error)
        }
    }
}

/// Obtains the public auth target identity without exposing credentials. The
/// production implementation probes the public health endpoint over the same
/// HTTPS origin used by account preparation.
pub trait AuthTargetProbe {
    fn inspect(&self, config: &LoadTestConfig) -> Result<TargetProtectionSnapshot, String>;
}

/// A remote-only target protector for operations that must authenticate
/// against the public auth endpoint before any player ticket exists. It
/// establishes a baseline during the first successful health probe and keeps
/// it immutable for the remainder of the operation.
pub struct LiveAuthProtection<'a, P = ReqwestAuthTargetProbe> {
    config: &'a LoadTestConfig,
    probe: P,
    baseline: Mutex<TargetProtectionBaseline>,
    auth_services_digest: Mutex<Option<String>>,
}

impl<'a> LiveAuthProtection<'a, ReqwestAuthTargetProbe> {
    pub fn new(config: &'a LoadTestConfig, timeout: Duration) -> Result<Self, String> {
        Ok(Self::with_probe(
            config,
            ReqwestAuthTargetProbe::new(config, timeout)?,
        ))
    }
}

impl<'a, P> LiveAuthProtection<'a, P>
where
    P: AuthTargetProbe,
{
    pub fn with_probe(config: &'a LoadTestConfig, probe: P) -> Self {
        Self {
            config,
            probe,
            baseline: Mutex::new(TargetProtectionBaseline::default()),
            auth_services_digest: Mutex::new(None),
        }
    }

    /// Login is the first public auth response that can carry player service
    /// descriptors. Keep the game descriptor pinned to the configured public
    /// KCP target and reject a later descriptor change before another account
    /// can progress through preparation.
    pub fn observe_auth_services(
        &self,
        services: Option<&AuthServicesPayload>,
    ) -> Result<(), String> {
        let services =
            services.ok_or("auth login response is missing public services descriptors")?;
        let game = services
            .game
            .as_ref()
            .ok_or("auth login response is missing the public game descriptor")?;
        let configured_proxy = &self
            .config
            .parsed_targets()
            .map_err(|error| error.to_string())?[1];
        if game.protocol != SideTransportKind::Kcp
            || game.host != configured_proxy.host
            || game.port != configured_proxy.port
        {
            return Err(
                "auth public game descriptor does not match the approved KCP target".into(),
            );
        }
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(
                serde_json::to_vec(services)
                    .map_err(|_| "could not serialize public auth descriptors")?,
            )
        );
        let mut baseline = self
            .auth_services_digest
            .lock()
            .map_err(|_| "auth descriptor baseline lock is unavailable")?;
        match baseline.as_deref() {
            None => {
                *baseline = Some(digest);
                Ok(())
            }
            Some(expected) if expected == digest => Ok(()),
            Some(_) => {
                Err("auth public service descriptor changed during account preparation".into())
            }
        }
    }

    fn revalidate_live_target(&self) -> Result<(), String> {
        self.revalidate_while_waiting()?;
        let snapshot = self.probe.inspect(self.config)?;
        self.baseline
            .lock()
            .map_err(|_| "auth target protection baseline lock is unavailable")?
            .observe(snapshot)
    }

    /// This is intentionally local-only so rate-admission polling cannot emit
    /// unbudgeted DNS or HTTPS traffic. The full probe resolves and validates
    /// approved targets immediately before its bounded health request.
    pub fn revalidate_while_waiting(&self) -> Result<(), String> {
        self.config
            .validate_remote_test_window_at(current_unix_ms())
            .map_err(|error| error.to_string())
    }
}

impl<P> RuntimeProtection for LiveAuthProtection<'_, P>
where
    P: AuthTargetProbe,
{
    fn verify_dns(&self) -> Result<(), String> {
        self.revalidate_while_waiting()
    }

    fn verify_certificate(&self) -> Result<(), String> {
        Err("remote certificate identity requires a per-request health probe".into())
    }

    fn verify_descriptor(&self) -> Result<(), String> {
        Err("remote protocol descriptor requires a per-request health probe".into())
    }

    fn verify_environment_identity(&self) -> Result<(), String> {
        self.config
            .validate_remote_test_window_at(current_unix_ms())
            .map_err(|error| error.to_string())
    }

    fn revalidate(&self) -> Result<(), String> {
        self.revalidate_live_target()
    }
}

/// Blocking reqwest probe used only after the explicit live account-prepare
/// gates have passed. Construction itself does not open a connection.
pub struct ReqwestAuthTargetProbe {
    health_url: String,
    client: Client,
}

impl ReqwestAuthTargetProbe {
    pub fn new(config: &LoadTestConfig, timeout: Duration) -> Result<Self, String> {
        let [auth, _] = config.parsed_targets().map_err(|error| error.to_string())?;
        if config.environment.kind.is_remote() && auth.scheme != "https" {
            return Err("remote account preparation requires an HTTPS auth target".into());
        }
        let client = Client::builder()
            .timeout(timeout)
            .http1_only()
            .pool_max_idle_per_host(0)
            .tls_info(true)
            .build()
            .map_err(|error| format!("could not build auth target protection client: {error}"))?;
        Ok(Self {
            health_url: format!("{}://{}:{}/healthz", auth.scheme, auth.host, auth.port),
            client,
        })
    }
}

impl AuthTargetProbe for ReqwestAuthTargetProbe {
    fn inspect(&self, config: &LoadTestConfig) -> Result<TargetProtectionSnapshot, String> {
        let dns_ips = resolve_approved_target_ips(config)?;
        let response = self
            .client
            .get(&self.health_url)
            .header(CONNECTION, "close")
            .send()
            .map_err(
                |_| "public auth health probe could not establish a verified TLS connection",
            )?;
        if !response.status().is_success() {
            return Err("public auth health probe did not return a successful status".into());
        }
        let certificate_fingerprint = response
            .extensions()
            .get::<TlsInfo>()
            .and_then(TlsInfo::peer_certificate)
            .map(|certificate_der| format!("sha256:{:x}", Sha256::digest(certificate_der)))
            .ok_or("public auth health probe did not expose a peer TLS certificate")?;
        let health: Value = response
            .json()
            .map_err(|_| "public auth health probe returned an invalid descriptor")?;
        let service = health
            .get("service")
            .and_then(Value::as_str)
            .filter(|service| *service == "auth-http")
            .ok_or("public auth health probe did not identify auth-http")?;
        let environment = health
            .get("env")
            .and_then(Value::as_str)
            .filter(|environment| !environment.trim().is_empty())
            .ok_or("public auth health probe did not provide an environment identity")?;
        let expected_environment = expected_auth_environment(config.environment.kind)
            .ok_or("live auth target protection is only valid for remote profiles")?;
        if environment != expected_environment {
            return Err(
                "public auth health environment does not match the approved profile kind".into(),
            );
        }
        let storage = health.get("storage").and_then(Value::as_str).unwrap_or("");
        Ok(TargetProtectionSnapshot {
            dns_ips,
            certificate_fingerprint,
            descriptor_digest: format!(
                "sha256:{:x}",
                Sha256::digest(format!(
                    "auth-health-v1\\0{service}\\0{environment}\\0{storage}"
                ))
            ),
            environment: config.environment.name.clone(),
        })
    }
}

fn resolve_approved_target_ips(config: &LoadTestConfig) -> Result<BTreeSet<IpAddr>, String> {
    let mut ips = BTreeSet::new();
    for target in config.parsed_targets().map_err(|error| error.to_string())? {
        if !config.environment.allowed_hosts.contains(&target.host) {
            return Err("approved target host escaped the host allowlist".into());
        }
        let resolved = (target.host.as_str(), target.port)
            .to_socket_addrs()
            .map_err(|_| "approved target DNS resolution failed")?
            .map(|address| address.ip())
            .collect::<BTreeSet<_>>();
        if resolved.is_empty() {
            return Err("approved target DNS resolution returned no addresses".into());
        }
        if resolved
            .iter()
            .any(|address| !config.environment.allowed_ips.contains(address))
        {
            return Err("approved target DNS resolution escaped the IP allowlist".into());
        }
        ips.extend(resolved);
    }
    if ips.is_empty() {
        return Err("approved target DNS resolution returned no addresses".into());
    }
    Ok(ips)
}

fn expected_auth_environment(kind: EnvironmentKind) -> Option<&'static str> {
    match kind {
        EnvironmentKind::Local => None,
        EnvironmentKind::Test => Some("test"),
        EnvironmentKind::Staging => Some("staging"),
        EnvironmentKind::Production => Some("production"),
    }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Offline-only implementation used by stage one. It verifies the configured
/// local target identity but deliberately fails closed for remote profiles:
/// certificate and descriptor identity cannot be confirmed without transport.
pub struct DryRunProtection<'a> {
    config: &'a LoadTestConfig,
}

impl<'a> DryRunProtection<'a> {
    pub fn new(config: &'a LoadTestConfig) -> Self {
        Self { config }
    }
}

impl RuntimeProtection for DryRunProtection<'_> {
    fn verify_dns(&self) -> Result<(), String> {
        if self.config.environment.kind.is_remote() {
            self.config
                .revalidate_targets()
                .map_err(|error| error.to_string())
        } else if self
            .config
            .parsed_targets()
            .map_err(|error| error.to_string())?
            .iter()
            .all(|target| target.is_loopback())
        {
            Ok(())
        } else {
            Err("local dry-run target identity is not loopback".into())
        }
    }

    fn verify_certificate(&self) -> Result<(), String> {
        if self.config.environment.kind.is_remote() {
            Err("remote certificate identity cannot be confirmed by dry-run".into())
        } else {
            Ok(())
        }
    }

    fn verify_descriptor(&self) -> Result<(), String> {
        if self.config.environment.kind.is_remote() {
            Err("remote protocol descriptor cannot be confirmed by dry-run".into())
        } else {
            Ok(())
        }
    }

    fn verify_environment_identity(&self) -> Result<(), String> {
        let now_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.config
            .validate_remote_test_window_at(now_unix_ms)
            .map_err(|error| error.to_string())?;
        if self.config.environment.name.trim().is_empty() {
            Err("environment identity is empty".into())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AccountPrepareConfig, EnvironmentKind, EnvironmentProfile, HardBudget, PlayerTargets,
        RemoteTestWindow, Scenario,
    };
    use crate::side_services::ServiceDescriptor;
    use crate::{LoadTestConfig, SCHEMA_VERSION};
    use std::cell::Cell;
    use std::collections::{BTreeSet, VecDeque};

    fn local_config() -> LoadTestConfig {
        LoadTestConfig {
            schema_version: SCHEMA_VERSION,
            environment: EnvironmentProfile {
                name: "local".into(),
                kind: EnvironmentKind::Local,
                approval_reference: None,
                allowed_hosts: BTreeSet::new(),
                allowed_ips: BTreeSet::new(),
                test_window: None,
                observers: BTreeSet::new(),
                stop_responsible_party: None,
                manual_confirmation_reference: None,
            },
            targets: PlayerTargets {
                auth_http: "http://127.0.0.1:3000".into(),
                game_proxy: "kcp://127.0.0.1:4000".into(),
            },
            budget: HardBudget {
                max_virtual_players: 1,
                max_login_qps: 1.0,
                max_new_connections_per_second: 1.0,
                max_business_messages_per_second: 1.0,
                max_messages_per_connection_per_second: 1.0,
                max_duration_secs: 1,
                max_total_operations: 1,
                max_error_rate: 0.1,
                max_connection_failure_rate: 0.1,
                max_p99_ms: 1,
                max_data_writes: 0,
            },
            scenario: Scenario {
                name: "dry".into(),
                load: crate::config::LoadModel::FixedConcurrency {
                    virtual_players: 1,
                    duration_secs: 1,
                },
                steps: Vec::new(),
                writes_data: false,
                auth: None,
                reconnect_burst: None,
                live_gameplay: None,
                side_services: None,
                registry_observation: None,
            },
            reports_root: "reports".into(),
            prepare_reports_root: "prepare".into(),
            stop_file: None,
            deadline_unix_ms: None,
            graceful_shutdown_ms: 1,
            account_prepare: AccountPrepareConfig::default(),
            calibration: Default::default(),
            unsafe_operations: Vec::new(),
        }
    }

    #[test]
    fn local_dry_run_can_confirm_its_offline_target_identity() {
        assert!(DryRunProtection::new(&local_config()).revalidate().is_ok());
    }

    #[test]
    fn remote_dry_run_fails_closed_when_transport_protection_is_unavailable() {
        let mut config = local_config();
        config.environment.kind = EnvironmentKind::Test;
        config.environment.allowed_hosts.insert("127.0.0.1".into());
        config
            .environment
            .allowed_ips
            .insert("127.0.0.1".parse().unwrap());
        assert!(DryRunProtection::new(&config).revalidate().is_err());
    }

    #[test]
    fn live_auth_waiting_recheck_does_not_invoke_the_network_probe() {
        let config = remote_config();
        let protection = LiveAuthProtection::with_probe(
            &config,
            ScriptedAuthTargetProbe::new(std::iter::empty()),
        );
        protection.revalidate_while_waiting().unwrap();
    }

    fn remote_config() -> LoadTestConfig {
        let mut config = local_config();
        config.environment = EnvironmentProfile {
            name: "test".into(),
            kind: EnvironmentKind::Test,
            approval_reference: Some("approved-for-test".into()),
            allowed_hosts: ["192.0.2.10".into()].into(),
            allowed_ips: ["192.0.2.10".parse().unwrap()].into(),
            test_window: Some(RemoteTestWindow {
                starts_unix_ms: 0,
                ends_unix_ms: u64::MAX,
            }),
            observers: ["load-test-observer".into()].into(),
            stop_responsible_party: Some("load-test-owner".into()),
            manual_confirmation_reference: Some("manual-confirmation".into()),
        };
        config.targets = PlayerTargets {
            auth_http: "https://192.0.2.10:443".into(),
            game_proxy: "kcp://192.0.2.10:4000".into(),
        };
        config
    }

    struct ScriptedAuthTargetProbe {
        snapshots: Mutex<VecDeque<Result<TargetProtectionSnapshot, String>>>,
    }

    impl ScriptedAuthTargetProbe {
        fn new(
            snapshots: impl IntoIterator<Item = Result<TargetProtectionSnapshot, String>>,
        ) -> Self {
            Self {
                snapshots: Mutex::new(snapshots.into_iter().collect()),
            }
        }
    }

    impl AuthTargetProbe for ScriptedAuthTargetProbe {
        fn inspect(&self, _config: &LoadTestConfig) -> Result<TargetProtectionSnapshot, String> {
            self.snapshots
                .lock()
                .map_err(|_| "scripted auth target probe lock is unavailable".to_string())?
                .pop_front()
                .ok_or_else(|| "scripted auth target probe ran out of snapshots".to_string())?
        }
    }

    #[test]
    fn live_auth_protection_accepts_baseline_and_rejects_drift_without_replacing_it() {
        let config = remote_config();
        let expected = protection_snapshot();
        let mut changed = expected.clone();
        changed.certificate_fingerprint = "sha256:cert-b".into();
        let protection = LiveAuthProtection::with_probe(
            &config,
            ScriptedAuthTargetProbe::new([Ok(expected.clone()), Ok(changed)]),
        );

        protection.revalidate().unwrap();
        assert_eq!(
            protection.baseline.lock().unwrap().expected(),
            Some(&expected)
        );
        assert!(protection.revalidate().unwrap_err().contains("certificate"));
        assert_eq!(
            protection.baseline.lock().unwrap().expected(),
            Some(&expected)
        );
    }

    #[test]
    fn live_auth_protection_pins_the_public_game_descriptor() {
        let config = remote_config();
        let protection = LiveAuthProtection::with_probe(
            &config,
            ScriptedAuthTargetProbe::new(std::iter::empty()),
        );
        let services = AuthServicesPayload {
            game: Some(ServiceDescriptor {
                host: "192.0.2.10".into(),
                port: 4000,
                protocol: SideTransportKind::Kcp,
            }),
            chat: None,
            mail: None,
            announce: None,
        };
        protection.observe_auth_services(Some(&services)).unwrap();

        let mut changed = services.clone();
        changed.chat = Some(ServiceDescriptor {
            host: "chat.example".into(),
            port: 443,
            protocol: SideTransportKind::Wss,
        });
        assert!(
            protection
                .observe_auth_services(Some(&changed))
                .unwrap_err()
                .contains("changed")
        );

        let mut mismatched = services;
        mismatched.game.as_mut().unwrap().port = 4001;
        assert!(
            protection
                .observe_auth_services(Some(&mismatched))
                .unwrap_err()
                .contains("approved KCP target")
        );
    }

    struct BecomesUnconfirmed {
        calls: Cell<u8>,
    }

    impl RuntimeProtection for BecomesUnconfirmed {
        fn verify_dns(&self) -> Result<(), String> {
            self.calls.set(self.calls.get() + 1);
            if self.calls.get() == 1 {
                Ok(())
            } else {
                Err("DNS target changed".into())
            }
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

    #[test]
    fn failed_recheck_signals_fail_closed_protection_abort() {
        let protection = BecomesUnconfirmed {
            calls: Cell::new(0),
        };
        let mut abort = AbortController::default();
        assert!(revalidate_or_abort(&protection, &mut abort).is_none());
        assert_eq!(abort.reason(), None);
        assert_eq!(
            revalidate_or_abort(&protection, &mut abort),
            Some("DNS target changed".into())
        );
        assert_eq!(
            abort.reason(),
            Some(&crate::abort::AbortReason::ProtectionUnknown)
        );
    }

    fn protection_snapshot() -> TargetProtectionSnapshot {
        TargetProtectionSnapshot {
            dns_ips: ["192.0.2.10".parse().unwrap()].into(),
            certificate_fingerprint: "sha256:cert-a".into(),
            descriptor_digest: "sha256:descriptor-a".into(),
            environment: "staging".into(),
        }
    }

    #[test]
    fn protection_baseline_rejects_dns_rebinding_descriptor_and_certificate_changes() {
        let mut baseline = TargetProtectionBaseline::default();
        baseline.observe(protection_snapshot()).unwrap();
        assert!(baseline.expected().is_some());

        let mut changed = protection_snapshot();
        changed.dns_ips = ["192.0.2.11".parse().unwrap()].into();
        assert!(baseline.observe(changed).unwrap_err().contains("DNS"));

        let mut changed = protection_snapshot();
        changed.certificate_fingerprint = "sha256:cert-b".into();
        assert!(
            baseline
                .observe(changed)
                .unwrap_err()
                .contains("certificate")
        );

        let mut changed = protection_snapshot();
        changed.descriptor_digest = "sha256:descriptor-b".into();
        assert!(
            baseline
                .observe(changed)
                .unwrap_err()
                .contains("descriptor")
        );
    }

    #[test]
    fn protection_baseline_rejects_empty_identity_and_environment_changes() {
        let mut baseline = TargetProtectionBaseline::default();
        let mut invalid = protection_snapshot();
        invalid.dns_ips.clear();
        assert!(baseline.observe(invalid).is_err());

        baseline.observe(protection_snapshot()).unwrap();
        let mut changed = protection_snapshot();
        changed.environment = "production".into();
        assert!(
            baseline
                .observe(changed)
                .unwrap_err()
                .contains("environment")
        );
    }
}
