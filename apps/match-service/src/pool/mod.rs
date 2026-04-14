//! 匹配池模块

pub mod candidate;
pub mod match_pool;

pub use candidate::MatchCandidate;
pub use match_pool::{new_match_pool, new_match_pool_with_modes, SharedMatchPool};
