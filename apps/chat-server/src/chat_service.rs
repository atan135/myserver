use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock};

use crate::chat_store::{ChatGroup, ChatMessage};
use crate::chat_server::MessageType;
use crate::protocol::{encode_body, OutboundMessage, Packet};
use crate::proto::chat::{
    ChatGroupReq, ChatGroupRes, ChatHistoryReq, ChatHistoryRes, ChatPrivateReq, ChatPrivateRes,
    ChatPush, ErrorRes, GroupCreateReq, GroupCreateRes, GroupDismissReq, GroupDismissRes, GroupInfo,
    GroupJoinReq, GroupJoinRes, GroupLeaveReq, GroupListRes,
};

pub type ChatSessionMap = Arc<RwLock<HashMap<String, mpsc::UnboundedSender<OutboundMessage>>>>;

pub fn new_chat_session_map() -> ChatSessionMap {
    Arc::new(RwLock::new(HashMap::new()))
}

fn generate_msg_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn generate_group_id() -> String {
    format!("grp_{}", uuid::Uuid::new_v4())
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
    tx: &mpsc::UnboundedSender<OutboundMessage>,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = match packet.decode_body::<ChatPrivateReq>() {
        Ok(value) => value,
        Err(e) => {
            queue_error(tx, packet.header.seq, &e, "invalid chat private body")?;
            return Ok(());
        }
    };

    if request.target_id.is_empty() {
        queue_error(tx, packet.header.seq, "INVALID_TARGET", "target_id is empty")?;
        return Ok(());
    }

    if request.target_id == player_id {
        queue_error(tx, packet.header.seq, "CANNOT_CHAT_SELF", "cannot chat with yourself")?;
        return Ok(());
    }

    if request.content.is_empty() {
        queue_error(tx, packet.header.seq, "EMPTY_CONTENT", "content is empty")?;
        return Ok(());
    }

    let msg_id = generate_msg_id();
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

    if let Err(e) = chat_store.save_private_message(&msg).await {
        tracing::warn!("failed to save private message: {}", e);
    }

    // 发送响应给发送者
    let res = ChatPrivateRes {
        ok: true,
        error_code: String::new(),
        msg_id: msg_id.clone(),
    };
    queue_message(tx, MessageType::ChatPrivateRes as u16, packet.header.seq, &res)?;

    // 如果目标玩家在线，推送消息
    if let Some(sender) = sessions.read().await.get(&request.target_id) {
        let push = build_chat_push(&msg, player_id);
        let _ = sender.send(push);
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
    tx: &mpsc::UnboundedSender<OutboundMessage>,
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

    if !chat_store.is_group_member(&request.group_id, player_id).await? {
        queue_error(tx, packet.header.seq, "NOT_GROUP_MEMBER", "you are not a member of this group")?;
        return Ok(());
    }

    if request.content.is_empty() {
        queue_error(tx, packet.header.seq, "EMPTY_CONTENT", "content is empty")?;
        return Ok(());
    }

    let msg_id = generate_msg_id();
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

    if let Err(e) = chat_store.save_group_message(&msg).await {
        tracing::warn!("failed to save group message: {}", e);
    }

    // 发送响应给发送者
    let res = ChatGroupRes {
        ok: true,
        error_code: String::new(),
        msg_id: msg_id.clone(),
    };
    queue_message(tx, MessageType::ChatGroupRes as u16, packet.header.seq, &res)?;

    // 推送给所有在线群成员
    let members = chat_store.get_group_members(&request.group_id).await?;

    for member_id in members {
        if member_id != player_id {
            if let Some(sender) = sessions.read().await.get(&member_id) {
                let push = build_chat_push(&msg, player_id);
                let _ = sender.send(push);
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
    tx: &mpsc::UnboundedSender<OutboundMessage>,
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

    let group_id = generate_group_id();
    let timestamp = current_unix_ms();

    let group = ChatGroup {
        group_id: group_id.clone(),
        name: request.name.clone(),
        owner_id: player_id.to_string(),
        created_at: timestamp,
    };

    if let Err(e) = chat_store.create_group(&group).await {
        tracing::warn!("failed to create group: {}", e);
        let res = GroupCreateRes {
            ok: false,
            group_id: String::new(),
            error_code: "CREATE_FAILED".to_string(),
        };
        queue_message(tx, MessageType::GroupCreateRes as u16, packet.header.seq, &res)?;
        return Ok(());
    }

    let res = GroupCreateRes {
        ok: true,
        group_id,
        error_code: String::new(),
    };
    queue_message(tx, MessageType::GroupCreateRes as u16, packet.header.seq, &res)?;

    Ok(())
}

// ============================================================
// 加入群组
// ============================================================

pub async fn handle_group_join(
    chat_store: &crate::chat_store::ChatStore,
    player_id: &str,
    packet: &Packet,
    tx: &mpsc::UnboundedSender<OutboundMessage>,
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
        queue_message(tx, MessageType::GroupJoinRes as u16, packet.header.seq, &res)?;
        return Ok(());
    }

    if chat_store.is_group_member(&request.group_id, player_id).await? {
        let res = GroupJoinRes {
            ok: false,
            error_code: "ALREADY_MEMBER".to_string(),
        };
        queue_message(tx, MessageType::GroupJoinRes as u16, packet.header.seq, &res)?;
        return Ok(());
    }

    let timestamp = current_unix_ms();
    if let Err(e) = chat_store.add_group_member(&request.group_id, player_id, timestamp).await {
        tracing::warn!("failed to join group: {}", e);
        let res = GroupJoinRes {
            ok: false,
            error_code: "JOIN_FAILED".to_string(),
        };
        queue_message(tx, MessageType::GroupJoinRes as u16, packet.header.seq, &res)?;
        return Ok(());
    }

    let res = GroupJoinRes {
        ok: true,
        error_code: String::new(),
    };
    queue_message(tx, MessageType::GroupJoinRes as u16, packet.header.seq, &res)?;

    Ok(())
}

// ============================================================
// 离开群组
// ============================================================

pub async fn handle_group_leave(
    chat_store: &crate::chat_store::ChatStore,
    player_id: &str,
    packet: &Packet,
    tx: &mpsc::UnboundedSender<OutboundMessage>,
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

    if chat_store.is_group_owner(&request.group_id, player_id).await? {
        let res = crate::proto::chat::GroupLeaveRes {
            ok: false,
            error_code: "OWNER_CANNOT_LEAVE".to_string(),
        };
        queue_message(tx, MessageType::GroupLeaveRes as u16, packet.header.seq, &res)?;
        return Ok(());
    }

    if let Err(e) = chat_store.remove_group_member(&request.group_id, player_id).await {
        tracing::warn!("failed to leave group: {}", e);
        let res = crate::proto::chat::GroupLeaveRes {
            ok: false,
            error_code: "LEAVE_FAILED".to_string(),
        };
        queue_message(tx, MessageType::GroupLeaveRes as u16, packet.header.seq, &res)?;
        return Ok(());
    }

    let res = crate::proto::chat::GroupLeaveRes {
        ok: true,
        error_code: String::new(),
    };
    queue_message(tx, MessageType::GroupLeaveRes as u16, packet.header.seq, &res)?;

    Ok(())
}

// ============================================================
// 解散群组
// ============================================================

pub async fn handle_group_dismiss(
    chat_store: &crate::chat_store::ChatStore,
    player_id: &str,
    packet: &Packet,
    tx: &mpsc::UnboundedSender<OutboundMessage>,
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

    if !chat_store.is_group_owner(&request.group_id, player_id).await? {
        let res = GroupDismissRes {
            ok: false,
            error_code: "NOT_OWNER".to_string(),
        };
        queue_message(tx, MessageType::GroupDismissRes as u16, packet.header.seq, &res)?;
        return Ok(());
    }

    if let Err(e) = chat_store.delete_group(&request.group_id).await {
        tracing::warn!("failed to dismiss group: {}", e);
        let res = GroupDismissRes {
            ok: false,
            error_code: "DISMISS_FAILED".to_string(),
        };
        queue_message(tx, MessageType::GroupDismissRes as u16, packet.header.seq, &res)?;
        return Ok(());
    }

    let res = GroupDismissRes {
        ok: true,
        error_code: String::new(),
    };
    queue_message(tx, MessageType::GroupDismissRes as u16, packet.header.seq, &res)?;

    Ok(())
}

// ============================================================
// 获取群组列表
// ============================================================

pub async fn handle_group_list(
    chat_store: &crate::chat_store::ChatStore,
    player_id: &str,
    packet: &Packet,
    tx: &mpsc::UnboundedSender<OutboundMessage>,
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

    let res = GroupListRes { groups: group_infos };
    queue_message(tx, MessageType::GroupListRes as u16, packet.header.seq, &res)?;

    Ok(())
}

// ============================================================
// 获取聊天历史
// ============================================================

pub async fn handle_chat_history(
    chat_store: &crate::chat_store::ChatStore,
    player_id: &str,
    packet: &Packet,
    tx: &mpsc::UnboundedSender<OutboundMessage>,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = match packet.decode_body::<ChatHistoryReq>() {
        Ok(value) => value,
        Err(e) => {
            queue_error(tx, packet.header.seq, &e, "invalid chat history body")?;
            return Ok(());
        }
    };

    let limit = if request.limit <= 0 { 20 } else { request.limit.min(100) };
    let before_time = if request.before_time <= 0 {
        current_unix_ms()
    } else {
        request.before_time
    };

    let messages = match request.chat_type {
        1 => {
            if request.target_id.is_empty() {
                queue_error(tx, packet.header.seq, "INVALID_TARGET", "target_id is empty")?;
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
            if !chat_store.is_group_member(&request.target_id, player_id).await? {
                queue_error(tx, packet.header.seq, "NOT_GROUP_MEMBER", "you are not a member of this group")?;
                return Ok(());
            }
            chat_store
                .get_group_history(&request.target_id, before_time, limit)
                .await?
        }
        _ => {
            queue_error(tx, packet.header.seq, "INVALID_CHAT_TYPE", "chat_type must be 1 or 2")?;
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
    queue_message(tx, MessageType::ChatHistoryRes as u16, packet.header.seq, &res)?;

    Ok(())
}

// ============================================================
// 会话管理
// ============================================================

pub async fn register_session(
    sessions: &ChatSessionMap,
    player_id: String,
    sender: mpsc::UnboundedSender<OutboundMessage>,
) {
    sessions.write().await.insert(player_id, sender);
}

pub async fn unregister_session(sessions: &ChatSessionMap, player_id: &str) {
    sessions.write().await.remove(player_id);
}

// ============================================================
// 辅助函数
// ============================================================

fn queue_error(
    tx: &mpsc::UnboundedSender<OutboundMessage>,
    seq: u32,
    error_code: &str,
    message: &str,
) -> Result<(), std::io::Error> {
    let res = ErrorRes {
        error_code: error_code.to_string(),
        message: message.to_string(),
    };
    queue_message(tx, MessageType::ErrorRes as u16, seq, &res)
}

fn queue_message<M: prost::Message>(
    tx: &mpsc::UnboundedSender<OutboundMessage>,
    message_type: u16,
    seq: u32,
    message: &M,
) -> Result<(), std::io::Error> {
    let body = encode_body(message);
    tx.send(OutboundMessage { message_type, seq, body })
        .map_err(|_| std::io::Error::other("failed to queue outbound"))
}
