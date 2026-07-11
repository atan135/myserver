use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCode {
    ConfigInvalid,
    RootMissing,
    RootInvalid,
    CodexUnavailable,
    AuditorInvalid,
}

impl ErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConfigInvalid => "MYFORGE_CONFIG_INVALID",
            Self::RootMissing => "MYFORGE_ROOT_MISSING",
            Self::RootInvalid => "MYFORGE_ROOT_INVALID",
            Self::CodexUnavailable => "MYFORGE_CODEX_UNAVAILABLE",
            Self::AuditorInvalid => "MYFORGE_AUDITOR_INVALID",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentError {
    code: ErrorCode,
    message: String,
}

impl AgentError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub const fn code(&self) -> ErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn config(variable: &str, reason: &str) -> Self {
        Self::new(
            ErrorCode::ConfigInvalid,
            format!("invalid configuration for {variable}: {reason}"),
        )
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for AgentError {}
