//! 错误定义

use thiserror::Error;

#[derive(Error, Debug)]
pub enum MatchError {
    #[error("invalid mode: {0}")]
    InvalidMode(String),

    #[error("already matching: player_id={0}")]
    AlreadyMatching(String),

    #[error("not matching: player_id={0}")]
    NotMatching(String),

    #[error("match not found: match_id={0}")]
    MatchNotFound(String),

    #[error("player not found: player_id={0}")]
    PlayerNotFound(String),

    #[error("match timeout: match_id={0}")]
    MatchTimeout(String),

    #[error("room create failed: {0}")]
    RoomCreateFailed(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl MatchError {
    pub fn error_code(&self) -> &'static str {
        match self {
            MatchError::InvalidMode(_) => "INVALID_MODE",
            MatchError::AlreadyMatching(_) => "ALREADY_MATCHING",
            MatchError::NotMatching(_) => "NOT_MATCHING",
            MatchError::MatchNotFound(_) => "MATCH_NOT_FOUND",
            MatchError::PlayerNotFound(_) => "PLAYER_NOT_FOUND",
            MatchError::MatchTimeout(_) => "MATCH_TIMEOUT",
            MatchError::RoomCreateFailed(_) => "ROOM_CREATE_FAILED",
            MatchError::Internal(_) => "INTERNAL_ERROR",
        }
    }
}
