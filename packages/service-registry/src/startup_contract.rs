use serde::{Deserialize, Serialize};

/// Process lifecycle shared by application services during startup convergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupState {
    Starting,
    WaitingDependencies,
    Ready,
    Degraded,
    ShuttingDown,
}

impl StartupState {
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }

        matches!(
            (self, next),
            (
                Self::Starting,
                Self::WaitingDependencies | Self::ShuttingDown
            ) | (
                Self::WaitingDependencies,
                Self::Ready | Self::Degraded | Self::ShuttingDown
            ) | (Self::Ready, Self::Degraded | Self::ShuttingDown)
                | (Self::Degraded, Self::Ready | Self::ShuttingDown)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyRequirement {
    /// Missing dependency prevents readiness and healthy endpoint publication.
    Required,
    /// Missing capability degrades only the operations that consume it.
    Optional,
}

impl DependencyRequirement {
    pub fn blocks_readiness(self) -> bool {
        self == Self::Required
    }
}

/// Stable machine-readable startup codes. Log messages remain free-form context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StartupErrorCode {
    DependencyPending,
    DependencyTimeout,
    RegistryUnavailable,
    LeaseUnavailable,
    LeaseLost,
    SocketConflict,
    StartupPhaseFailure,
}

impl StartupErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DependencyPending => "DEPENDENCY_PENDING",
            Self::DependencyTimeout => "DEPENDENCY_TIMEOUT",
            Self::RegistryUnavailable => "REGISTRY_UNAVAILABLE",
            Self::LeaseUnavailable => "LEASE_UNAVAILABLE",
            Self::LeaseLost => "LEASE_LOST",
            Self::SocketConflict => "SOCKET_CONFLICT",
            Self::StartupPhaseFailure => "STARTUP_PHASE_FAILURE",
        }
    }
}

/// Field names form the cross-service logging and readiness diagnostic contract.
pub mod observation_fields {
    pub const SERVICE: &str = "service";
    pub const INSTANCE_ID: &str = "instance_id";
    pub const LIFECYCLE_STATE: &str = "lifecycle_state";
    pub const STARTUP_PHASE: &str = "startup_phase";
    pub const DEPENDENCY: &str = "dependency";
    /// Logical endpoint name such as `grpc` or `proxy-local`, never an address or URL.
    pub const ENDPOINT: &str = "endpoint";
    pub const DEPENDENCY_REQUIREMENT: &str = "dependency_requirement";
    pub const ERROR_CODE: &str = "error_code";
    pub const RETRY_COUNT: &str = "retry_count";
    pub const ELAPSED_MS: &str = "elapsed_ms";
    pub const LAST_SUCCESS_AT_MS: &str = "last_success_at_ms";

    pub const ALL: &[&str] = &[
        SERVICE,
        INSTANCE_ID,
        LIFECYCLE_STATE,
        STARTUP_PHASE,
        DEPENDENCY,
        ENDPOINT,
        DEPENDENCY_REQUIREMENT,
        ERROR_CODE,
        RETRY_COUNT,
        ELAPSED_MS,
        LAST_SUCCESS_AT_MS,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_only_allows_the_convergence_contract() {
        assert!(StartupState::Starting.can_transition_to(StartupState::WaitingDependencies));
        assert!(StartupState::WaitingDependencies.can_transition_to(StartupState::Ready));
        assert!(StartupState::WaitingDependencies.can_transition_to(StartupState::Degraded));
        assert!(StartupState::Ready.can_transition_to(StartupState::Degraded));
        assert!(StartupState::Degraded.can_transition_to(StartupState::Ready));
        assert!(StartupState::Ready.can_transition_to(StartupState::ShuttingDown));

        assert!(!StartupState::Starting.can_transition_to(StartupState::Ready));
        assert!(!StartupState::Ready.can_transition_to(StartupState::WaitingDependencies));
        assert!(!StartupState::ShuttingDown.can_transition_to(StartupState::Starting));
    }

    #[test]
    fn only_required_dependencies_block_readiness() {
        assert!(DependencyRequirement::Required.blocks_readiness());
        assert!(!DependencyRequirement::Optional.blocks_readiness());
    }

    #[test]
    fn startup_error_codes_have_stable_wire_values() {
        let values = [
            (StartupErrorCode::DependencyPending, "DEPENDENCY_PENDING"),
            (StartupErrorCode::DependencyTimeout, "DEPENDENCY_TIMEOUT"),
            (
                StartupErrorCode::RegistryUnavailable,
                "REGISTRY_UNAVAILABLE",
            ),
            (StartupErrorCode::LeaseUnavailable, "LEASE_UNAVAILABLE"),
            (StartupErrorCode::LeaseLost, "LEASE_LOST"),
            (StartupErrorCode::SocketConflict, "SOCKET_CONFLICT"),
            (
                StartupErrorCode::StartupPhaseFailure,
                "STARTUP_PHASE_FAILURE",
            ),
        ];

        for (code, expected) in values {
            assert_eq!(code.as_str(), expected);
            assert_eq!(
                serde_json::to_string(&code).unwrap(),
                format!("\"{expected}\"")
            );
        }
    }

    #[test]
    fn observation_field_names_are_stable_and_secret_free() {
        assert_eq!(
            observation_fields::ALL,
            &[
                "service",
                "instance_id",
                "lifecycle_state",
                "startup_phase",
                "dependency",
                "endpoint",
                "dependency_requirement",
                "error_code",
                "retry_count",
                "elapsed_ms",
                "last_success_at_ms",
            ]
        );
        assert!(observation_fields::ALL.iter().all(|field| {
            !field.contains("password") && !field.contains("token") && !field.contains("url")
        }));
    }
}
