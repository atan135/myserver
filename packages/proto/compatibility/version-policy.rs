// Shared by game-proxy and game-server through include!. Keep this policy in lockstep with
// version-policy.json; the Node checker verifies the public constants and protobuf fields.

#[allow(dead_code)]
pub const PACKET_HEADER_VERSION: u8 = 1;
pub const CURRENT_CLIENT_PROTOCOL_VERSION: u32 = 1;
pub const MINIMUM_CLIENT_PROTOCOL_VERSION: u32 = 1;
pub const LEGACY_IMPLICIT_PROTOCOL_VERSION: u32 = 1;

pub const CLIENT_PROTOCOL_VERSION_TOO_OLD: &str = "CLIENT_PROTOCOL_VERSION_TOO_OLD";
pub const CLIENT_PROTOCOL_VERSION_TOO_NEW: &str = "CLIENT_PROTOCOL_VERSION_TOO_NEW";
pub const DEFAULT_UPGRADE_MESSAGE: &str =
    "A newer game client is required. Please update and try again.";
pub const DEFAULT_RETRY_MESSAGE: &str =
    "This game service is not ready for this client version. Please retry shortly.";
// A release owner must replace this blank value with an absolute HTTPS URL only after that URL is
// publicly available. Clients must not manufacture a store URL from this field.
pub const DEFAULT_UPGRADE_URL: &str = "";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientProtocolVersionSource {
    LegacyImplicit,
    Explicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientProtocolVersionMetric {
    AcceptedLegacy,
    AcceptedCurrent,
    AcceptedSupportedOlder,
    RejectedTooOld,
    RejectedTooNew,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientProtocolVersionDecision {
    Accepted {
        effective_version: u32,
        source: ClientProtocolVersionSource,
    },
    RejectedTooOld {
        effective_version: u32,
        source: ClientProtocolVersionSource,
    },
    RejectedTooNew {
        effective_version: u32,
        source: ClientProtocolVersionSource,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientProtocolVersionRejection {
    pub error_code: &'static str,
    pub upgrade_message: &'static str,
    pub upgrade_url: &'static str,
}

impl ClientProtocolVersionDecision {
    pub fn metric(self) -> ClientProtocolVersionMetric {
        self.metric_for_current_version(CURRENT_CLIENT_PROTOCOL_VERSION)
    }

    pub fn metric_for_current_version(self, current_version: u32) -> ClientProtocolVersionMetric {
        match self {
            Self::Accepted {
                source: ClientProtocolVersionSource::LegacyImplicit,
                ..
            } => ClientProtocolVersionMetric::AcceptedLegacy,
            Self::Accepted {
                effective_version,
                source: ClientProtocolVersionSource::Explicit,
            } if effective_version < current_version => {
                ClientProtocolVersionMetric::AcceptedSupportedOlder
            }
            Self::Accepted { .. } => ClientProtocolVersionMetric::AcceptedCurrent,
            Self::RejectedTooOld { .. } => ClientProtocolVersionMetric::RejectedTooOld,
            Self::RejectedTooNew { .. } => ClientProtocolVersionMetric::RejectedTooNew,
        }
    }

    pub fn rejection(self) -> Option<ClientProtocolVersionRejection> {
        match self {
            Self::RejectedTooOld { .. } => Some(ClientProtocolVersionRejection {
                error_code: CLIENT_PROTOCOL_VERSION_TOO_OLD,
                upgrade_message: DEFAULT_UPGRADE_MESSAGE,
                upgrade_url: DEFAULT_UPGRADE_URL,
            }),
            Self::RejectedTooNew { .. } => Some(ClientProtocolVersionRejection {
                error_code: CLIENT_PROTOCOL_VERSION_TOO_NEW,
                upgrade_message: DEFAULT_RETRY_MESSAGE,
                upgrade_url: "",
            }),
            Self::Accepted { .. } => None,
        }
    }

    pub fn effective_version(self) -> u32 {
        match self {
            Self::Accepted {
                effective_version, ..
            }
            | Self::RejectedTooOld {
                effective_version, ..
            }
            | Self::RejectedTooNew {
                effective_version, ..
            } => effective_version,
        }
    }

    pub fn source(self) -> ClientProtocolVersionSource {
        match self {
            Self::Accepted { source, .. }
            | Self::RejectedTooOld { source, .. }
            | Self::RejectedTooNew { source, .. } => source,
        }
    }
}

pub fn negotiate_client_protocol_version(declared_version: u32) -> ClientProtocolVersionDecision {
    negotiate_client_protocol_version_with_policy(
        declared_version,
        MINIMUM_CLIENT_PROTOCOL_VERSION,
        CURRENT_CLIENT_PROTOCOL_VERSION,
        LEGACY_IMPLICIT_PROTOCOL_VERSION,
    )
}

pub fn negotiate_client_protocol_version_with_policy(
    declared_version: u32,
    minimum_version: u32,
    current_version: u32,
    legacy_implicit_version: u32,
) -> ClientProtocolVersionDecision {
    let source = if declared_version == 0 {
        ClientProtocolVersionSource::LegacyImplicit
    } else {
        ClientProtocolVersionSource::Explicit
    };
    let effective_version = if declared_version == 0 {
        legacy_implicit_version
    } else {
        declared_version
    };

    if effective_version < minimum_version {
        return ClientProtocolVersionDecision::RejectedTooOld {
            effective_version,
            source,
        };
    }
    if effective_version > current_version {
        return ClientProtocolVersionDecision::RejectedTooNew {
            effective_version,
            source,
        };
    }

    ClientProtocolVersionDecision::Accepted {
        effective_version,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_auth_field_is_the_accepted_legacy_v1_client() {
        assert_eq!(
            negotiate_client_protocol_version(0),
            ClientProtocolVersionDecision::Accepted {
                effective_version: 1,
                source: ClientProtocolVersionSource::LegacyImplicit,
            }
        );
    }

    #[test]
    fn current_explicit_client_is_accepted() {
        assert_eq!(
            negotiate_client_protocol_version(CURRENT_CLIENT_PROTOCOL_VERSION),
            ClientProtocolVersionDecision::Accepted {
                effective_version: CURRENT_CLIENT_PROTOCOL_VERSION,
                source: ClientProtocolVersionSource::Explicit,
            }
        );
    }

    #[test]
    fn policy_rejects_versions_outside_its_supported_range() {
        let too_old = negotiate_client_protocol_version_with_policy(0, 2, 2, 1);
        let too_new = negotiate_client_protocol_version_with_policy(3, 1, 2, 1);

        assert!(matches!(
            too_old,
            ClientProtocolVersionDecision::RejectedTooOld { .. }
        ));
        assert!(matches!(
            too_new,
            ClientProtocolVersionDecision::RejectedTooNew { .. }
        ));
        assert_eq!(
            too_old.rejection().unwrap().error_code,
            CLIENT_PROTOCOL_VERSION_TOO_OLD
        );
        assert_eq!(
            too_new.rejection().unwrap().error_code,
            CLIENT_PROTOCOL_VERSION_TOO_NEW
        );
    }

    #[test]
    fn explicit_supported_older_version_has_a_bounded_observability_bucket() {
        let decision = negotiate_client_protocol_version_with_policy(1, 1, 2, 1);
        assert_eq!(
            decision.metric_for_current_version(2),
            ClientProtocolVersionMetric::AcceptedSupportedOlder
        );
    }
}
