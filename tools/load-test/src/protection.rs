use crate::abort::AbortController;
use crate::config::LoadTestConfig;

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
    use crate::config::{EnvironmentKind, EnvironmentProfile, HardBudget, PlayerTargets, Scenario};
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
            },
            reports_root: "reports".into(),
            prepare_reports_root: "prepare".into(),
            stop_file: None,
            deadline_unix_ms: None,
            graceful_shutdown_ms: 1,
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
}
