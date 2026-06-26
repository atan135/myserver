//! 角色匹配状态机

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};

use crate::proto::myserver::matchservice::MatchEvent;
use crate::runtime_store::{
    MatchRuntimeSnapshot, SharedMatchRuntimeStore, StoredCharacterMatchContext,
    StoredCharacterMatchStatus, StoredMatchEvent,
};

/// 角色匹配状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharacterMatchStatus {
    /// 未匹配
    Idle,
    /// 匹配中
    Matching,
    /// 匹配成功，等待进入房间
    Matched,
    /// 已进入房间
    InRoom,
}

impl Default for CharacterMatchStatus {
    fn default() -> Self {
        CharacterMatchStatus::Idle
    }
}

/// 角色匹配上下文
#[derive(Clone)]
pub struct CharacterMatchContext {
    pub match_id: String,
    pub mode: String,
    pub room_id: Option<String>,
    pub token: Option<String>,
}

/// 角色状态管理
pub struct CharacterStateStore {
    /// character_id -> 匹配状态
    status: RwLock<HashMap<String, CharacterMatchStatus>>,
    /// character_id -> 匹配上下文
    context: RwLock<HashMap<String, CharacterMatchContext>>,
    /// character_id -> 事件推送通道
    streams: RwLock<HashMap<String, mpsc::Sender<MatchEvent>>>,
    /// character_id -> 最近一次匹配事件
    latest_events: RwLock<HashMap<String, StoredMatchEvent>>,
    /// 可选运行时持久化存储
    runtime_store: Option<SharedMatchRuntimeStore>,
}

impl Default for CharacterStateStore {
    fn default() -> Self {
        Self::new()
    }
}

impl CharacterStateStore {
    pub fn new() -> Self {
        Self {
            status: RwLock::new(HashMap::new()),
            context: RwLock::new(HashMap::new()),
            streams: RwLock::new(HashMap::new()),
            latest_events: RwLock::new(HashMap::new()),
            runtime_store: None,
        }
    }

    pub fn with_runtime_store(runtime_store: SharedMatchRuntimeStore) -> Self {
        Self {
            status: RwLock::new(HashMap::new()),
            context: RwLock::new(HashMap::new()),
            streams: RwLock::new(HashMap::new()),
            latest_events: RwLock::new(HashMap::new()),
            runtime_store: Some(runtime_store),
        }
    }

    pub async fn apply_snapshot(&self, snapshot: &MatchRuntimeSnapshot) {
        {
            let mut status = self.status.write().await;
            status.clear();
            status.extend(
                snapshot
                    .character_status
                    .iter()
                    .map(|(character_id, value)| (character_id.clone(), value.clone().into())),
            );
        }

        {
            let mut context = self.context.write().await;
            context.clear();
            context.extend(
                snapshot
                    .character_context
                    .iter()
                    .map(|(character_id, value)| (character_id.clone(), value.clone().into())),
            );
        }

        {
            let mut latest_events = self.latest_events.write().await;
            latest_events.clear();
            latest_events.extend(snapshot.latest_events.clone());
        }
    }

    /// 获取角色当前状态
    pub async fn get_status(&self, character_id: &str) -> CharacterMatchStatus {
        self.status
            .read()
            .await
            .get(character_id)
            .copied()
            .unwrap_or(CharacterMatchStatus::Idle)
    }

    /// 设置角色状态
    pub async fn set_status(&self, character_id: &str, status: CharacterMatchStatus) {
        self.status
            .write()
            .await
            .insert(character_id.to_string(), status);
        if let Some(runtime_store) = &self.runtime_store {
            if let Err(error) = runtime_store
                .set_character_status(character_id, StoredCharacterMatchStatus::from(status))
                .await
            {
                tracing::warn!(
                    character_id,
                    error = %error,
                    "failed to persist character match status"
                );
            }
        }
    }

    /// 获取角色上下文
    pub async fn get_context(&self, character_id: &str) -> Option<CharacterMatchContext> {
        self.context.read().await.get(character_id).cloned()
    }

    /// 设置角色上下文
    pub async fn set_context(&self, character_id: &str, ctx: CharacterMatchContext) {
        self.context
            .write()
            .await
            .insert(character_id.to_string(), ctx.clone());
        if let Some(runtime_store) = &self.runtime_store {
            if let Err(error) = runtime_store
                .set_character_context(character_id, StoredCharacterMatchContext::from(ctx))
                .await
            {
                tracing::warn!(
                    character_id,
                    error = %error,
                    "failed to persist character match context"
                );
            }
        }
    }

    /// 清除角色上下文
    pub async fn clear_context(&self, character_id: &str) {
        self.context.write().await.remove(character_id);
        if let Some(runtime_store) = &self.runtime_store {
            if let Err(error) = runtime_store.clear_character_context(character_id).await {
                tracing::warn!(
                    character_id,
                    error = %error,
                    "failed to clear persisted character match context"
                );
            }
        }
    }

    /// 注册推送通道
    pub async fn register_stream(&self, character_id: &str, sender: mpsc::Sender<MatchEvent>) {
        self.streams
            .write()
            .await
            .insert(character_id.to_string(), sender);
    }

    /// 注销推送通道
    pub async fn unregister_stream(&self, character_id: &str) {
        self.streams.write().await.remove(character_id);
    }

    /// 获取推送通道
    pub async fn get_stream(&self, character_id: &str) -> Option<mpsc::Sender<MatchEvent>> {
        self.streams.read().await.get(character_id).cloned()
    }

    pub async fn latest_event(&self, character_id: &str) -> Option<MatchEvent> {
        self.latest_events
            .read()
            .await
            .get(character_id)
            .cloned()
            .map(StoredMatchEvent::into_event)
    }

    /// 发送事件给角色
    pub async fn send_event(&self, character_id: &str, event: MatchEvent) -> bool {
        let stored_event = StoredMatchEvent::new(event.clone());
        self.latest_events
            .write()
            .await
            .insert(character_id.to_string(), stored_event.clone());

        if let Some(runtime_store) = &self.runtime_store {
            if let Err(error) = runtime_store
                .save_latest_event(character_id, stored_event)
                .await
            {
                tracing::warn!(
                    character_id,
                    error = %error,
                    "failed to persist latest match event"
                );
            }
        }

        if let Some(sender) = self.streams.read().await.get(character_id) {
            sender.send(event).await.is_ok()
        } else {
            false
        }
    }
}

pub type SharedCharacterState = Arc<CharacterStateStore>;

pub fn new_character_state_store() -> SharedCharacterState {
    Arc::new(CharacterStateStore::new())
}

pub fn new_character_state_store_with_runtime_store(
    runtime_store: SharedMatchRuntimeStore,
) -> SharedCharacterState {
    Arc::new(CharacterStateStore::with_runtime_store(runtime_store))
}
