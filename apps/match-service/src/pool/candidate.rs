//! 匹配候选人结构

use std::time::Instant;

/// 匹配候选人
#[derive(Clone)]
pub struct MatchCandidate {
    /// 玩家ID
    pub player_id: String,
    /// 匹配ID
    pub match_id: String,
    /// 模式
    pub mode: String,
    /// 进入匹配池的时间
    pub created_at: Instant,
    /// 超时时间
    pub timeout_at: Instant,
}

impl MatchCandidate {
    pub fn new(player_id: String, match_id: String, mode: String, timeout_at: Instant) -> Self {
        Self {
            player_id,
            match_id,
            mode,
            created_at: Instant::now(),
            timeout_at,
        }
    }

    /// 检查是否超时
    pub fn is_timeout(&self) -> bool {
        Instant::now() >= self.timeout_at
    }
}
