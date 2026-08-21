use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivityScope {
    Character,
    Account,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ActivityType(String);

impl ActivityType {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, ActivityDomainError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ActivityDomainError::invalid_config(
                "activity type is empty",
            ));
        }
        Ok(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_registered(
        value: impl Into<String>,
        registered: &[&str],
    ) -> Result<Self, ActivityDomainError> {
        let activity_type = Self::new(value)?;
        if registered
            .iter()
            .any(|candidate| *candidate == activity_type.as_str())
        {
            Ok(activity_type)
        } else {
            Err(ActivityDomainError::new(
                ActivityErrorCode::UnknownType,
                format!(
                    "activity type '{}' is not registered",
                    activity_type.as_str()
                ),
            ))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivityStatus {
    Draft,
    Published,
    Running,
    Ended,
    Offline,
    Archived,
}

impl ActivityStatus {
    pub(crate) fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Draft, Self::Published)
                | (Self::Published, Self::Running)
                | (Self::Published, Self::Offline)
                | (Self::Running, Self::Ended)
                | (Self::Running, Self::Offline)
                | (Self::Ended, Self::Archived)
                | (Self::Offline, Self::Archived)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Activity {
    pub(crate) id: String,
    pub(crate) key: String,
    pub(crate) activity_type: ActivityType,
    pub(crate) scope: ActivityScope,
    pub(crate) status: ActivityStatus,
    pub(crate) start_at: DateTime<Utc>,
    pub(crate) end_at: DateTime<Utc>,
    pub(crate) claim_deadline: DateTime<Utc>,
    pub(crate) timezone: String,
    pub(crate) current_version: Option<i32>,
}

impl Activity {
    pub(crate) fn new(
        id: impl Into<String>,
        key: impl Into<String>,
        activity_type: ActivityType,
        scope: ActivityScope,
        start_at: DateTime<Utc>,
        end_at: DateTime<Utc>,
        claim_deadline: DateTime<Utc>,
        timezone: impl Into<String>,
    ) -> Result<Self, ActivityDomainError> {
        let id = id.into();
        let key = key.into();
        let timezone = timezone.into();
        if id.trim().is_empty() || key.trim().is_empty() || timezone.trim().is_empty() {
            return Err(ActivityDomainError::invalid_config(
                "activity id, key and timezone are required",
            ));
        }
        if start_at >= end_at {
            return Err(ActivityDomainError::invalid_config(
                "activity start_at must be before end_at",
            ));
        }
        if claim_deadline < end_at {
            return Err(ActivityDomainError::invalid_config(
                "claim_deadline must be at or after end_at",
            ));
        }
        Ok(Self {
            id,
            key,
            activity_type,
            scope,
            status: ActivityStatus::Draft,
            start_at,
            end_at,
            claim_deadline,
            timezone,
            current_version: None,
        })
    }

    pub(crate) fn is_in_window(&self, now: DateTime<Utc>) -> bool {
        self.start_at <= now && now < self.end_at
    }

    pub(crate) fn effective_status(&self, now: DateTime<Utc>) -> ActivityStatus {
        match self.status {
            ActivityStatus::Published if now < self.start_at => ActivityStatus::Published,
            ActivityStatus::Running if now < self.start_at => ActivityStatus::Published,
            ActivityStatus::Published | ActivityStatus::Running if now < self.end_at => {
                ActivityStatus::Running
            }
            ActivityStatus::Published | ActivityStatus::Running => ActivityStatus::Ended,
            status => status,
        }
    }

    pub(crate) fn can_claim(
        &self,
        now: DateTime<Utc>,
        earned_at: DateTime<Utc>,
    ) -> Result<(), ActivityDomainError> {
        if earned_at < self.start_at || earned_at >= self.end_at {
            return Err(ActivityDomainError::new(
                ActivityErrorCode::OutsideTimeWindow,
                "qualification was not earned inside the activity window",
            ));
        }
        match self.effective_status(now) {
            ActivityStatus::Running if now < self.end_at => Ok(()),
            ActivityStatus::Ended if now < self.claim_deadline => Ok(()),
            ActivityStatus::Ended => Err(ActivityDomainError::new(
                ActivityErrorCode::ClaimExpired,
                "claim deadline has passed",
            )),
            ActivityStatus::Offline | ActivityStatus::Archived => Err(ActivityDomainError::new(
                ActivityErrorCode::InvalidState,
                "offline or archived activity cannot accept claims",
            )),
            _ => Err(ActivityDomainError::new(
                ActivityErrorCode::InvalidState,
                "activity is not claimable",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActivityVersion {
    pub(crate) activity_id: String,
    pub(crate) version_no: i32,
    pub(crate) public_config: serde_json::Value,
    pub(crate) type_config: serde_json::Value,
    pub(crate) config_digest: String,
    pub(crate) start_at: DateTime<Utc>,
    pub(crate) end_at: DateTime<Utc>,
    pub(crate) claim_deadline: DateTime<Utc>,
    pub(crate) timezone: String,
    pub(crate) published_at: Option<DateTime<Utc>>,
}

impl ActivityVersion {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draft(
        activity_id: impl Into<String>,
        version_no: i32,
        public_config: serde_json::Value,
        type_config: serde_json::Value,
        start_at: DateTime<Utc>,
        end_at: DateTime<Utc>,
        claim_deadline: DateTime<Utc>,
        timezone: impl Into<String>,
    ) -> Result<Self, ActivityDomainError> {
        let activity_id = activity_id.into();
        let timezone = timezone.into();
        if activity_id.trim().is_empty() || version_no <= 0 {
            return Err(ActivityDomainError::invalid_config(
                "activity version identity is invalid",
            ));
        }
        if !public_config.is_object() || !type_config.is_object() {
            return Err(ActivityDomainError::invalid_config(
                "activity version configuration must be JSON objects",
            ));
        }
        if start_at >= end_at || claim_deadline < end_at || timezone.trim().is_empty() {
            return Err(ActivityDomainError::invalid_config(
                "activity version time configuration is invalid",
            ));
        }
        let config_digest = Self::digest(&public_config, &type_config);
        Ok(Self {
            activity_id,
            version_no,
            public_config,
            type_config,
            config_digest,
            start_at,
            end_at,
            claim_deadline,
            timezone,
            published_at: None,
        })
    }

    pub(crate) fn digest(
        public_config: &serde_json::Value,
        type_config: &serde_json::Value,
    ) -> String {
        let payload = serde_json::json!({
            "public_config": public_config,
            "type_config": type_config,
        });
        let bytes = serde_json::to_vec(&payload).expect("JSON values are serializable");
        format!("sha256:{:x}", Sha256::digest(bytes))
    }

    pub(crate) fn validate_digest(value: &str) -> bool {
        value.strip_prefix("sha256:").is_some_and(|hex| {
            hex.len() == 64
                && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
                && hex.chars().all(|ch| !ch.is_ascii_uppercase())
        })
    }

    pub(crate) fn has_valid_digest(&self) -> bool {
        Self::validate_digest(&self.config_digest)
            && self.config_digest == Self::digest(&self.public_config, &self.type_config)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActivityStage {
    pub(crate) activity_id: String,
    pub(crate) version_no: i32,
    pub(crate) stage_id: String,
    pub(crate) stage_no: i32,
    pub(crate) period_strategy: String,
    pub(crate) reward_group_key: String,
    pub(crate) max_claims: i32,
    pub(crate) qualification: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RewardGroup {
    pub(crate) activity_id: String,
    pub(crate) version_no: i32,
    pub(crate) key: String,
    pub(crate) selection_mode: String,
    pub(crate) config: serde_json::Value,
    pub(crate) items: Vec<RewardItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RewardItem {
    pub(crate) reward_type: String,
    pub(crate) asset_key: String,
    pub(crate) quantity: i64,
    pub(crate) weight: Option<i64>,
    pub(crate) payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PlayerActivityState {
    pub(crate) character_id: String,
    pub(crate) activity_id: String,
    pub(crate) version_no: i32,
    pub(crate) current_stage_id: Option<String>,
    pub(crate) progress: serde_json::Value,
    pub(crate) type_state: serde_json::Value,
    pub(crate) state_revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ClaimRecord {
    pub(crate) character_id: String,
    pub(crate) activity_id: String,
    pub(crate) version_no: i32,
    pub(crate) action_type: String,
    pub(crate) stage_id: Option<String>,
    pub(crate) period_key: String,
    pub(crate) semantic_claim_key: String,
    pub(crate) status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivityErrorCode {
    InvalidState,
    VersionConflict,
    OutsideTimeWindow,
    UnknownType,
    InvalidConfig,
    ClaimExpired,
    NotFound,
    CacheUnavailable,
}

impl ActivityErrorCode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidState => "ACTIVITY_INVALID_STATE",
            Self::VersionConflict => "ACTIVITY_VERSION_CONFLICT",
            Self::OutsideTimeWindow => "ACTIVITY_OUTSIDE_TIME_WINDOW",
            Self::UnknownType => "ACTIVITY_UNKNOWN_TYPE",
            Self::InvalidConfig => "ACTIVITY_INVALID_CONFIG",
            Self::ClaimExpired => "ACTIVITY_CLAIM_EXPIRED",
            Self::NotFound => "ACTIVITY_NOT_FOUND",
            Self::CacheUnavailable => "ACTIVITY_CACHE_UNAVAILABLE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityDomainError {
    code: ActivityErrorCode,
    message: String,
}

impl ActivityDomainError {
    pub(crate) fn new(code: ActivityErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub(crate) fn invalid_config(message: impl Into<String>) -> Self {
        Self::new(ActivityErrorCode::InvalidConfig, message)
    }

    pub(crate) fn code(&self) -> ActivityErrorCode {
        self.code
    }
}

impl fmt::Display for ActivityDomainError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ActivityDomainError {}
