use std::collections::HashMap;
use std::sync::Arc;
use std::sync::OnceLock;

use global_id::{GlobalIdError, GlobalIdGenerator};
use tokio::sync::{RwLock, mpsc, watch};

use crate::chat_server::MessageType;
use crate::chat_store::{ChatGroup, ChatMessage};
use crate::metrics::METRICS;
use crate::proto::chat::{
    ChatGroupReq, ChatGroupRes, ChatHistoryReq, ChatHistoryRes, ChatPrivateReq, ChatPrivateRes,
    ChatPush, ErrorRes, GroupCreateReq, GroupCreateRes, GroupDismissReq, GroupDismissRes,
    GroupInfo, GroupJoinReq, GroupJoinRes, GroupLeaveReq, GroupListRes,
};
use crate::protocol::{OutboundMessage, Packet, encode_body};

pub type ChatOutboundSender = mpsc::Sender<OutboundMessage>;
pub type ChatSessionMap = Arc<RwLock<HashMap<String, ChatSession>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionCloseReason {
    Replaced,
    OutboundQueueFull,
}

impl SessionCloseReason {
    pub fn error_code(self) -> &'static str {
        match self {
            Self::Replaced => "SESSION_REPLACED",
            Self::OutboundQueueFull => "OUTBOUND_QUEUE_FULL",
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            Self::Replaced => "session_replaced",
            Self::OutboundQueueFull => "outbound_queue_full",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboundQueueError {
    Full,
    Closed,
}

impl OutboundQueueError {
    pub fn category(self) -> &'static str {
        match self {
            Self::Full => "outbound_queue_full",
            Self::Closed => "outbound_queue_closed",
        }
    }
}

impl std::fmt::Display for OutboundQueueError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.category())
    }
}

impl std::error::Error for OutboundQueueError {}

#[derive(Clone)]
pub struct ChatSession {
    sender: ChatOutboundSender,
    shutdown: watch::Sender<Option<SessionCloseReason>>,
}

impl ChatSession {
    pub fn new(
        sender: ChatOutboundSender,
        shutdown: watch::Sender<Option<SessionCloseReason>>,
    ) -> Self {
        Self { sender, shutdown }
    }

    pub fn try_send(&self, message: OutboundMessage) -> Result<(), OutboundQueueError> {
        match self.sender.try_send(message) {
            Ok(()) => Ok(()),
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.request_shutdown(SessionCloseReason::OutboundQueueFull);
                Err(OutboundQueueError::Full)
            }
            Err(mpsc::error::TrySendError::Closed(_)) => Err(OutboundQueueError::Closed),
        }
    }

    fn request_shutdown(&self, reason: SessionCloseReason) {
        let _ = self.shutdown.send(Some(reason));
    }

    fn same_channel(&self, sender: &ChatOutboundSender) -> bool {
        self.sender.same_channel(sender)
    }
}

pub fn new_chat_session_map() -> ChatSessionMap {
    Arc::new(RwLock::new(HashMap::new()))
}

static CHAT_ID_GENERATOR: OnceLock<GlobalIdGenerator> = OnceLock::new();

pub fn initialize_global_id_generator(generator: GlobalIdGenerator) -> Result<(), GlobalIdError> {
    CHAT_ID_GENERATOR.set(generator).map_err(|_| {
        GlobalIdError::InvalidInput("chat global id generator already initialized".to_string())
    })
}

fn chat_id_generator() -> Result<&'static GlobalIdGenerator, Box<dyn std::error::Error>> {
    if let Some(generator) = CHAT_ID_GENERATOR.get() {
        return Ok(generator);
    }

    let generator = GlobalIdGenerator::from_env()?;
    let _ = CHAT_ID_GENERATOR.set(generator);
    Ok(CHAT_ID_GENERATOR
        .get()
        .expect("chat global id generator should be initialized"))
}

fn generate_msg_id() -> Result<String, Box<dyn std::error::Error>> {
    Ok(chat_id_generator()?.generate_string("msg")?)
}

fn generate_group_id() -> Result<String, Box<dyn std::error::Error>> {
    Ok(chat_id_generator()?.generate_string("grp")?)
}

fn current_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default()
}

fn build_chat_push(msg: &ChatMessage, sender_name: &str) -> OutboundMessage {
    let push = ChatPush {
        msg_id: msg.msg_id.clone(),
        chat_type: msg.chat_type,
        sender_id: msg.sender_id.clone(),
        sender_name: sender_name.to_string(),
        content: msg.content.clone(),
        timestamp: msg.created_at,
        target_id: msg.target_id.clone(),
        group_id: msg.group_id.clone(),
    };
    let body = encode_body(&push);
    OutboundMessage {
        message_type: MessageType::ChatPush as u16,
        seq: 0,
        body,
    }
}

// ============================================================
// 处理私聊
// ============================================================

pub async fn handle_chat_private(
    chat_store: &crate::chat_store::ChatStore,
    sessions: &ChatSessionMap,
    player_id: &str,
    packet: &Packet,
    tx: &ChatOutboundSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = match packet.decode_body::<ChatPrivateReq>() {
        Ok(value) => value,
        Err(e) => {
            queue_error(tx, packet.header.seq, &e, "invalid chat private body")?;
            return Ok(());
        }
    };

    if request.target_id.is_empty() {
        queue_error(
            tx,
            packet.header.seq,
            "INVALID_TARGET",
            "target_id is empty",
        )?;
        return Ok(());
    }

    if request.target_id == player_id {
        queue_error(
            tx,
            packet.header.seq,
            "CANNOT_CHAT_SELF",
            "cannot chat with yourself",
        )?;
        return Ok(());
    }

    if request.content.is_empty() {
        queue_error(tx, packet.header.seq, "EMPTY_CONTENT", "content is empty")?;
        return Ok(());
    }

    let msg_id = generate_msg_id()?;
    let timestamp = current_unix_ms();

    let msg = ChatMessage {
        msg_id: msg_id.clone(),
        chat_type: 1,
        sender_id: player_id.to_string(),
        content: request.content.clone(),
        created_at: timestamp,
        target_id: request.target_id.clone(),
        group_id: String::new(),
    };

    if chat_store.save_private_message(&msg).await.is_err() {
        tracing::warn!(
            message_type = MessageType::ChatPrivateReq as u16,
            error_category = "chat_store_save_failed",
            "failed to save private message"
        );
    }

    // 发送响应给发送者
    let res = ChatPrivateRes {
        ok: true,
        error_code: String::new(),
        msg_id: msg_id.clone(),
    };
    queue_message(
        tx,
        MessageType::ChatPrivateRes as u16,
        packet.header.seq,
        &res,
    )?;

    // 如果目标玩家在线，推送消息
    if let Some(sender) = sessions.read().await.get(&request.target_id) {
        let push = build_chat_push(&msg, player_id);
        if let Err(error) = sender.try_send(push) {
            tracing::warn!(
                message_type = MessageType::ChatPush as u16,
                error_category = error.category(),
                "failed to queue private chat push"
            );
        }
    }

    Ok(())
}

// ============================================================
// 处理群聊
// ============================================================

pub async fn handle_chat_group(
    chat_store: &crate::chat_store::ChatStore,
    sessions: &ChatSessionMap,
    player_id: &str,
    packet: &Packet,
    tx: &ChatOutboundSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = match packet.decode_body::<ChatGroupReq>() {
        Ok(value) => value,
        Err(e) => {
            queue_error(tx, packet.header.seq, &e, "invalid chat group body")?;
            return Ok(());
        }
    };

    if request.group_id.is_empty() {
        queue_error(tx, packet.header.seq, "INVALID_GROUP", "group_id is empty")?;
        return Ok(());
    }

    if !chat_store
        .is_group_member(&request.group_id, player_id)
        .await?
    {
        queue_error(
            tx,
            packet.header.seq,
            "NOT_GROUP_MEMBER",
            "you are not a member of this group",
        )?;
        return Ok(());
    }

    if request.content.is_empty() {
        queue_error(tx, packet.header.seq, "EMPTY_CONTENT", "content is empty")?;
        return Ok(());
    }

    let msg_id = generate_msg_id()?;
    let timestamp = current_unix_ms();

    let msg = ChatMessage {
        msg_id: msg_id.clone(),
        chat_type: 2,
        sender_id: player_id.to_string(),
        content: request.content.clone(),
        created_at: timestamp,
        target_id: String::new(),
        group_id: request.group_id.clone(),
    };

    if chat_store.save_group_message(&msg).await.is_err() {
        tracing::warn!(
            message_type = MessageType::ChatGroupReq as u16,
            error_category = "chat_store_save_failed",
            "failed to save group message"
        );
    }

    // 发送响应给发送者
    let res = ChatGroupRes {
        ok: true,
        error_code: String::new(),
        msg_id: msg_id.clone(),
    };
    queue_message(
        tx,
        MessageType::ChatGroupRes as u16,
        packet.header.seq,
        &res,
    )?;

    // 推送给所有在线群成员
    let members = chat_store.get_group_members(&request.group_id).await?;

    for member_id in members {
        if member_id != player_id {
            if let Some(sender) = sessions.read().await.get(&member_id) {
                let push = build_chat_push(&msg, player_id);
                if let Err(error) = sender.try_send(push) {
                    tracing::warn!(
                        message_type = MessageType::ChatPush as u16,
                        error_category = error.category(),
                        "failed to queue group chat push"
                    );
                }
            }
        }
    }

    Ok(())
}

// ============================================================
// 创建群组
// ============================================================

pub async fn handle_group_create(
    chat_store: &crate::chat_store::ChatStore,
    player_id: &str,
    packet: &Packet,
    tx: &ChatOutboundSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = match packet.decode_body::<GroupCreateReq>() {
        Ok(value) => value,
        Err(e) => {
            queue_error(tx, packet.header.seq, &e, "invalid group create body")?;
            return Ok(());
        }
    };

    if request.name.is_empty() {
        queue_error(tx, packet.header.seq, "EMPTY_NAME", "group name is empty")?;
        return Ok(());
    }

    let group_id = generate_group_id()?;
    let timestamp = current_unix_ms();

    let group = ChatGroup {
        group_id: group_id.clone(),
        name: request.name.clone(),
        owner_id: player_id.to_string(),
        created_at: timestamp,
    };

    if chat_store.create_group(&group).await.is_err() {
        tracing::warn!(
            message_type = MessageType::GroupCreateReq as u16,
            error_category = "chat_store_write_failed",
            "failed to create group"
        );
        let res = GroupCreateRes {
            ok: false,
            group_id: String::new(),
            error_code: "CREATE_FAILED".to_string(),
        };
        queue_message(
            tx,
            MessageType::GroupCreateRes as u16,
            packet.header.seq,
            &res,
        )?;
        return Ok(());
    }

    let res = GroupCreateRes {
        ok: true,
        group_id,
        error_code: String::new(),
    };
    queue_message(
        tx,
        MessageType::GroupCreateRes as u16,
        packet.header.seq,
        &res,
    )?;

    Ok(())
}

// ============================================================
// 加入群组
// ============================================================

pub async fn handle_group_join(
    chat_store: &crate::chat_store::ChatStore,
    player_id: &str,
    packet: &Packet,
    tx: &ChatOutboundSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = match packet.decode_body::<GroupJoinReq>() {
        Ok(value) => value,
        Err(e) => {
            queue_error(tx, packet.header.seq, &e, "invalid group join body")?;
            return Ok(());
        }
    };

    if request.group_id.is_empty() {
        queue_error(tx, packet.header.seq, "INVALID_GROUP", "group_id is empty")?;
        return Ok(());
    }

    if chat_store.get_group(&request.group_id).await?.is_none() {
        let res = GroupJoinRes {
            ok: false,
            error_code: "GROUP_NOT_FOUND".to_string(),
        };
        queue_message(
            tx,
            MessageType::GroupJoinRes as u16,
            packet.header.seq,
            &res,
        )?;
        return Ok(());
    }

    if chat_store
        .is_group_member(&request.group_id, player_id)
        .await?
    {
        let res = GroupJoinRes {
            ok: false,
            error_code: "ALREADY_MEMBER".to_string(),
        };
        queue_message(
            tx,
            MessageType::GroupJoinRes as u16,
            packet.header.seq,
            &res,
        )?;
        return Ok(());
    }

    let timestamp = current_unix_ms();
    if chat_store
        .add_group_member(&request.group_id, player_id, timestamp)
        .await
        .is_err()
    {
        tracing::warn!(
            message_type = MessageType::GroupJoinReq as u16,
            error_category = "chat_store_write_failed",
            "failed to join group"
        );
        let res = GroupJoinRes {
            ok: false,
            error_code: "JOIN_FAILED".to_string(),
        };
        queue_message(
            tx,
            MessageType::GroupJoinRes as u16,
            packet.header.seq,
            &res,
        )?;
        return Ok(());
    }

    let res = GroupJoinRes {
        ok: true,
        error_code: String::new(),
    };
    queue_message(
        tx,
        MessageType::GroupJoinRes as u16,
        packet.header.seq,
        &res,
    )?;

    Ok(())
}

// ============================================================
// 离开群组
// ============================================================

pub async fn handle_group_leave(
    chat_store: &crate::chat_store::ChatStore,
    player_id: &str,
    packet: &Packet,
    tx: &ChatOutboundSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = match packet.decode_body::<GroupLeaveReq>() {
        Ok(value) => value,
        Err(e) => {
            queue_error(tx, packet.header.seq, &e, "invalid group leave body")?;
            return Ok(());
        }
    };

    if request.group_id.is_empty() {
        queue_error(tx, packet.header.seq, "INVALID_GROUP", "group_id is empty")?;
        return Ok(());
    }

    if chat_store
        .is_group_owner(&request.group_id, player_id)
        .await?
    {
        let res = crate::proto::chat::GroupLeaveRes {
            ok: false,
            error_code: "OWNER_CANNOT_LEAVE".to_string(),
        };
        queue_message(
            tx,
            MessageType::GroupLeaveRes as u16,
            packet.header.seq,
            &res,
        )?;
        return Ok(());
    }

    if chat_store
        .remove_group_member(&request.group_id, player_id)
        .await
        .is_err()
    {
        tracing::warn!(
            message_type = MessageType::GroupLeaveReq as u16,
            error_category = "chat_store_write_failed",
            "failed to leave group"
        );
        let res = crate::proto::chat::GroupLeaveRes {
            ok: false,
            error_code: "LEAVE_FAILED".to_string(),
        };
        queue_message(
            tx,
            MessageType::GroupLeaveRes as u16,
            packet.header.seq,
            &res,
        )?;
        return Ok(());
    }

    let res = crate::proto::chat::GroupLeaveRes {
        ok: true,
        error_code: String::new(),
    };
    queue_message(
        tx,
        MessageType::GroupLeaveRes as u16,
        packet.header.seq,
        &res,
    )?;

    Ok(())
}

// ============================================================
// 解散群组
// ============================================================

pub async fn handle_group_dismiss(
    chat_store: &crate::chat_store::ChatStore,
    player_id: &str,
    packet: &Packet,
    tx: &ChatOutboundSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = match packet.decode_body::<GroupDismissReq>() {
        Ok(value) => value,
        Err(e) => {
            queue_error(tx, packet.header.seq, &e, "invalid group dismiss body")?;
            return Ok(());
        }
    };

    if request.group_id.is_empty() {
        queue_error(tx, packet.header.seq, "INVALID_GROUP", "group_id is empty")?;
        return Ok(());
    }

    if !chat_store
        .is_group_owner(&request.group_id, player_id)
        .await?
    {
        let res = GroupDismissRes {
            ok: false,
            error_code: "NOT_OWNER".to_string(),
        };
        queue_message(
            tx,
            MessageType::GroupDismissRes as u16,
            packet.header.seq,
            &res,
        )?;
        return Ok(());
    }

    if chat_store.delete_group(&request.group_id).await.is_err() {
        tracing::warn!(
            message_type = MessageType::GroupDismissReq as u16,
            error_category = "chat_store_write_failed",
            "failed to dismiss group"
        );
        let res = GroupDismissRes {
            ok: false,
            error_code: "DISMISS_FAILED".to_string(),
        };
        queue_message(
            tx,
            MessageType::GroupDismissRes as u16,
            packet.header.seq,
            &res,
        )?;
        return Ok(());
    }

    let res = GroupDismissRes {
        ok: true,
        error_code: String::new(),
    };
    queue_message(
        tx,
        MessageType::GroupDismissRes as u16,
        packet.header.seq,
        &res,
    )?;

    Ok(())
}

// ============================================================
// 获取群组列表
// ============================================================

pub async fn handle_group_list(
    chat_store: &crate::chat_store::ChatStore,
    player_id: &str,
    packet: &Packet,
    tx: &ChatOutboundSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let groups = chat_store.get_player_groups(player_id).await?;

    let mut group_infos = Vec::new();
    for group in groups {
        let member_count = chat_store.get_group_member_count(&group.group_id).await?;
        group_infos.push(GroupInfo {
            group_id: group.group_id,
            name: group.name,
            member_count,
        });
    }

    let res = GroupListRes {
        groups: group_infos,
    };
    queue_message(
        tx,
        MessageType::GroupListRes as u16,
        packet.header.seq,
        &res,
    )?;

    Ok(())
}

// ============================================================
// 获取聊天历史
// ============================================================

pub async fn handle_chat_history(
    chat_store: &crate::chat_store::ChatStore,
    player_id: &str,
    packet: &Packet,
    tx: &ChatOutboundSender,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = match packet.decode_body::<ChatHistoryReq>() {
        Ok(value) => value,
        Err(e) => {
            queue_error(tx, packet.header.seq, &e, "invalid chat history body")?;
            return Ok(());
        }
    };

    let limit = if request.limit <= 0 {
        20
    } else {
        request.limit.min(100)
    };
    let before_time = if request.before_time <= 0 {
        current_unix_ms()
    } else {
        request.before_time
    };

    let messages = match request.chat_type {
        1 => {
            if request.target_id.is_empty() {
                queue_error(
                    tx,
                    packet.header.seq,
                    "INVALID_TARGET",
                    "target_id is empty",
                )?;
                return Ok(());
            }
            chat_store
                .get_private_history(player_id, &request.target_id, before_time, limit)
                .await?
        }
        2 => {
            if request.target_id.is_empty() {
                queue_error(tx, packet.header.seq, "INVALID_GROUP", "group_id is empty")?;
                return Ok(());
            }
            if !chat_store
                .is_group_member(&request.target_id, player_id)
                .await?
            {
                queue_error(
                    tx,
                    packet.header.seq,
                    "NOT_GROUP_MEMBER",
                    "you are not a member of this group",
                )?;
                return Ok(());
            }
            chat_store
                .get_group_history(&request.target_id, before_time, limit)
                .await?
        }
        _ => {
            queue_error(
                tx,
                packet.header.seq,
                "INVALID_CHAT_TYPE",
                "chat_type must be 1 or 2",
            )?;
            return Ok(());
        }
    };

    let pushes: Vec<ChatPush> = messages
        .into_iter()
        .map(|msg| ChatPush {
            msg_id: msg.msg_id,
            chat_type: msg.chat_type,
            sender_id: msg.sender_id.clone(),
            sender_name: msg.sender_id,
            content: msg.content,
            timestamp: msg.created_at,
            target_id: msg.target_id,
            group_id: msg.group_id,
        })
        .collect();

    let res = ChatHistoryRes { messages: pushes };
    queue_message(
        tx,
        MessageType::ChatHistoryRes as u16,
        packet.header.seq,
        &res,
    )?;

    Ok(())
}

// ============================================================
// 会话管理
// ============================================================

pub async fn register_session(
    sessions: &ChatSessionMap,
    player_id: String,
    sender: ChatOutboundSender,
    shutdown: watch::Sender<Option<SessionCloseReason>>,
) {
    let (online_players, replaced) = {
        let mut guard = sessions.write().await;
        let replaced = guard.insert(player_id, ChatSession::new(sender, shutdown));
        (guard.len() as u64, replaced)
    };
    if let Some(replaced) = replaced {
        replaced.request_shutdown(SessionCloseReason::Replaced);
    }
    METRICS.set_online_players(online_players);
}

pub async fn unregister_session(
    sessions: &ChatSessionMap,
    player_id: &str,
    sender: &ChatOutboundSender,
) -> bool {
    let (online_players, removed) = {
        let mut guard = sessions.write().await;
        let removed = if guard
            .get(player_id)
            .is_some_and(|current| current.same_channel(sender))
        {
            guard.remove(player_id);
            true
        } else {
            false
        };
        (guard.len() as u64, removed)
    };
    METRICS.set_online_players(online_players);
    removed
}

// ============================================================
// 辅助函数
// ============================================================

pub(crate) fn queue_error(
    tx: &ChatOutboundSender,
    seq: u32,
    error_code: &str,
    message: &str,
) -> Result<(), OutboundQueueError> {
    let res = ErrorRes {
        error_code: error_code.to_string(),
        message: message.to_string(),
    };
    queue_message(tx, MessageType::ErrorRes as u16, seq, &res)
}

fn queue_message<M: prost::Message>(
    tx: &ChatOutboundSender,
    message_type: u16,
    seq: u32,
    message: &M,
) -> Result<(), OutboundQueueError> {
    let body = encode_body(message);
    match tx.try_send(OutboundMessage {
        message_type,
        seq,
        body,
    }) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => Err(OutboundQueueError::Full),
        Err(mpsc::error::TrySendError::Closed(_)) => Err(OutboundQueueError::Closed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_message_returns_error_when_bounded_channel_is_full() {
        let (tx, _rx) = mpsc::channel(1);
        let message = ErrorRes {
            error_code: "TEST".to_string(),
            message: "test".to_string(),
        };

        assert!(queue_message(&tx, MessageType::ErrorRes as u16, 1, &message).is_ok());
        assert!(queue_message(&tx, MessageType::ErrorRes as u16, 2, &message).is_err());
    }

    #[tokio::test]
    async fn stale_session_cannot_unregister_the_current_session() {
        let sessions = new_chat_session_map();
        let (old_sender, _old_receiver) = mpsc::channel(1);
        let (current_sender, _current_receiver) = mpsc::channel(1);
        let (old_shutdown, mut old_shutdown_rx) = watch::channel(None);
        let (current_shutdown, current_shutdown_rx) = watch::channel(None);

        register_session(
            &sessions,
            "player_001".to_string(),
            old_sender.clone(),
            old_shutdown,
        )
        .await;
        register_session(
            &sessions,
            "player_001".to_string(),
            current_sender.clone(),
            current_shutdown,
        )
        .await;
        old_shutdown_rx.changed().await.unwrap();
        assert_eq!(
            *old_shutdown_rx.borrow(),
            Some(SessionCloseReason::Replaced)
        );
        assert!(!unregister_session(&sessions, "player_001", &old_sender).await);

        assert!(
            sessions
                .read()
                .await
                .get("player_001")
                .is_some_and(|sender| sender.same_channel(&current_sender))
        );

        assert!(unregister_session(&sessions, "player_001", &current_sender).await);
        assert!(!sessions.read().await.contains_key("player_001"));
        assert_eq!(*current_shutdown_rx.borrow(), None);
    }

    #[tokio::test]
    async fn full_session_queue_requests_connection_shutdown() {
        let (sender, _receiver) = mpsc::channel(1);
        let (shutdown, mut shutdown_rx) = watch::channel(None);
        let session = ChatSession::new(sender, shutdown);
        let message = OutboundMessage {
            message_type: MessageType::ChatPush as u16,
            seq: 0,
            body: vec![],
        };

        session.try_send(message.clone()).unwrap();
        assert_eq!(session.try_send(message), Err(OutboundQueueError::Full));
        shutdown_rx.changed().await.unwrap();
        assert_eq!(
            *shutdown_rx.borrow(),
            Some(SessionCloseReason::OutboundQueueFull)
        );
    }
}
