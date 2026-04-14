//! 玩家匹配状态机

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

use crate::proto::myserver::matchservice::MatchEvent;

/// 玩家匹配状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerMatchStatus {
    /// 未匹配
    Idle,
    /// 匹配中
    Matching,
    /// 匹配成功，等待进入房间
    Matched,
    /// 已进入房间
    InRoom,
}

impl Default for PlayerMatchStatus {
    fn default() -> Self {
        PlayerMatchStatus::Idle
    }
}

/// 玩家匹配上下文
#[derive(Clone)]
pub struct PlayerMatchContext {
    pub match_id: String,
    pub mode: String,
    pub room_id: Option<String>,
    pub token: Option<String>,
}

/// 玩家状态管理
pub struct PlayerStateStore {
    /// player_id -> 匹配状态
    status: RwLock<HashMap<String, PlayerMatchStatus>>,
    /// player_id -> 匹配上下文
    context: RwLock<HashMap<String, PlayerMatchContext>>,
    /// player_id -> 事件推送通道
    streams: RwLock<HashMap<String, mpsc::Sender<MatchEvent>>>,
}

impl Default for PlayerStateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerStateStore {
    pub fn new() -> Self {
        Self {
            status: RwLock::new(HashMap::new()),
            context: RwLock::new(HashMap::new()),
            streams: RwLock::new(HashMap::new()),
        }
    }

    /// 获取玩家当前状态
    pub async fn get_status(&self, player_id: &str) -> PlayerMatchStatus {
        self.status
            .read()
            .await
            .get(player_id)
            .copied()
            .unwrap_or(PlayerMatchStatus::Idle)
    }

    /// 设置玩家状态
    pub async fn set_status(&self, player_id: &str, status: PlayerMatchStatus) {
        self.status.write().await.insert(player_id.to_string(), status);
    }

    /// 获取玩家上下文
    pub async fn get_context(&self, player_id: &str) -> Option<PlayerMatchContext> {
        self.context.read().await.get(player_id).cloned()
    }

    /// 设置玩家上下文
    pub async fn set_context(&self, player_id: &str, ctx: PlayerMatchContext) {
        self.context.write().await.insert(player_id.to_string(), ctx);
    }

    /// 清除玩家上下文
    pub async fn clear_context(&self, player_id: &str) {
        self.context.write().await.remove(player_id);
    }

    /// 注册推送通道
    pub async fn register_stream(
        &self,
        player_id: &str,
        sender: mpsc::Sender<MatchEvent>,
    ) {
        self.streams.write().await.insert(player_id.to_string(), sender);
    }

    /// 注销推送通道
    pub async fn unregister_stream(&self, player_id: &str) {
        self.streams.write().await.remove(player_id);
    }

    /// 获取推送通道
    pub async fn get_stream(
        &self,
        player_id: &str,
    ) -> Option<mpsc::Sender<MatchEvent>> {
        self.streams.read().await.get(player_id).cloned()
    }

    /// 发送事件给玩家
    pub async fn send_event(
        &self,
        player_id: &str,
        event: MatchEvent,
    ) -> bool {
        if let Some(sender) = self.streams.read().await.get(player_id) {
            sender.send(event).await.is_ok()
        } else {
            false
        }
    }
}

pub type SharedPlayerState = Arc<PlayerStateStore>;

pub fn new_player_state_store() -> SharedPlayerState {
    Arc::new(PlayerStateStore::new())
}
