use crate::abort::AbortController;
use crate::config::LoadTestConfig;
use std::collections::BTreeSet;
use std::net::IpAddr;

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
        Scenario,
    };
    use crate::{LoadTestConfig, SCHEMA_VERSION};
    use std::cell::Cell;
    use std::collections::BTreeSet;

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
