use std::time::Instant;

use prost::Message;
use redis::AsyncCommands;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{info, warn};

use crate::chat_service::{self, ChatSessionMap};
use crate::chat_store::ChatStore;
use crate::metrics::METRICS;
use crate::online_route;
use crate::proto::chat::{ChatAuthReq, ChatAuthRes};
use crate::protocol::{HEADER_LEN, OutboundMessage, Packet, encode_packet, parse_header};
use crate::ticket::{hash_ticket, verify_ticket};

#[derive(Debug, Clone, Copy)]
pub enum MessageType {
    ChatAuthReq = 20001,
    ChatAuthRes = 20002,
    ChatPrivateReq = 20101,
    ChatPrivateRes = 20102,
    ChatGroupReq = 20103,
    ChatGroupRes = 20104,
    ChatPush = 20105,
    GroupCreateReq = 20201,
    GroupCreateRes = 20202,
    GroupJoinReq = 20203,
    GroupJoinRes = 20204,
    GroupLeaveReq = 20205,
    GroupLeaveRes = 20206,
    GroupDismissReq = 20207,
    GroupDismissRes = 20208,
    GroupListReq = 20209,
    GroupListRes = 20210,
    ChatHistoryReq = 20211,
    ChatHistoryRes = 20212,
    MailNotifyPush = 20301,
    ErrorRes = 9000,
}

impl MessageType {
    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            20001 => Some(Self::ChatAuthReq),
            20002 => Some(Self::ChatAuthRes),
            20101 => Some(Self::ChatPrivateReq),
            20102 => Some(Self::ChatPrivateRes),
            20103 => Some(Self::ChatGroupReq),
            20104 => Some(Self::ChatGroupRes),
            20105 => Some(Self::ChatPush),
            20201 => Some(Self::GroupCreateReq),
            20202 => Some(Self::GroupCreateRes),
            20203 => Some(Self::GroupJoinReq),
            20204 => Some(Self::GroupJoinRes),
            20205 => Some(Self::GroupLeaveReq),
            20206 => Some(Self::GroupLeaveRes),
            20207 => Some(Self::GroupDismissReq),
            20208 => Some(Self::GroupDismissRes),
            20209 => Some(Self::GroupListReq),
            20210 => Some(Self::GroupListRes),
            20211 => Some(Self::ChatHistoryReq),
            20212 => Some(Self::ChatHistoryRes),
            20301 => Some(Self::MailNotifyPush),
            9000 => Some(Self::ErrorRes),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub bind_addr: String,
    pub heartbeat_timeout_secs: u64,
    pub max_body_len: usize,
    pub ticket_secret: String,
    pub redis_url: String,
    pub redis_key_prefix: String,
    pub service_instance_id: String,
    pub online_route_ttl_secs: u64,
}

pub async fn run(
    config: Config,
    chat_store: ChatStore,
    chat_sessions: ChatSessionMap,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(&config.bind_addr).await?;

    info!(
        addr = %config.bind_addr,
        "chat server listening"
    );

    loop {
        let accept_result = tokio::select! {
            result = listener.accept() => Some(result),
            _ = tokio::signal::ctrl_c() => None,
        };

        let Some((socket, peer_addr)) = accept_result.transpose()? else {
            info!("shutdown signal received, stopping chat server");
            break;
        };

        let chat_store = chat_store.clone();
        let chat_sessions = chat_sessions.clone();
        let config = config.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(
                socket,
                peer_addr.to_string(),
                chat_store,
                chat_sessions,
                config,
            )
            .await
            {
                warn!(peer = %peer_addr, error = %e, "connection handler error");
            }
        });
    }

    Ok(())
}

pub fn ticket_key(prefix: &str, ticket: &str) -> String {
    format!("{}ticket:{}", prefix, hash_ticket(ticket))
}

pub fn ticket_version_key(prefix: &str, player_id: &str) -> String {
    format!("{}player-ticket-version:{}", prefix, player_id)
}

pub fn validate_ticket_owner(
    stored_owner: Option<&str>,
    player_id: &str,
) -> Result<(), &'static str> {
    if stored_owner == Some(player_id) {
        Ok(())
    } else {
        Err("TICKET_REVOKED")
    }
}

pub fn validate_ticket_version(
    ticket_version: Option<u64>,
    current_ticket_version: Option<u64>,
) -> Result<(), &'static str> {
    if ticket_version.unwrap_or(1) == current_ticket_version.unwrap_or(1) {
        Ok(())
    } else {
        Err("TICKET_REVOKED")
    }
}

async fn write_auth_response<W>(
    writer: &mut W,
    seq: u32,
    ok: bool,
    error_code: &str,
) -> Result<(), Box<dyn std::error::Error>>
where
    W: AsyncWrite + Unpin,
{
    let res = ChatAuthRes {
        ok,
        error_code: error_code.to_string(),
    };
    let mut buf = Vec::new();
    res.encode(&mut buf)?;
    let packet = encode_packet(MessageType::ChatAuthRes as u16, seq, &buf);
    writer.write_all(&packet).await?;
    Ok(())
}

async fn handle_connection<S>(
    socket: S,
    peer_addr: String,
    chat_store: ChatStore,
    chat_sessions: ChatSessionMap,
    config: Config,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(socket);
    let (tx, mut rx) = mpsc::unbounded_channel::<OutboundMessage>();

    // === 认证阶段 ===
    let auth_started_at = Instant::now();
    let player_id = match read_auth_request(&mut reader, &mut writer, &config).await {
        Ok(id) => id,
        Err(e) => {
            METRICS.record_request();
            METRICS.record_latency(auth_started_at.elapsed().as_millis() as u64);
            warn!(peer = %peer_addr, error = %e, "auth failed");
            return Ok(());
        }
    };
    METRICS.record_request();
    METRICS.record_latency(auth_started_at.elapsed().as_millis() as u64);

    info!(peer = %peer_addr, player_id = %player_id, "player authenticated");

    // 注册聊天会话
    chat_service::register_session(&chat_sessions, player_id.clone(), tx.clone()).await;
    if let Err(e) = online_route::set_online_route(
        &config.redis_url,
        &config.redis_key_prefix,
        &player_id,
        &config.service_instance_id,
        config.online_route_ttl_secs,
    )
    .await
    {
        warn!(
            player_id = %player_id,
            instance_id = %config.service_instance_id,
            error = %e,
            "failed to set chat online route"
        );
    }

    // 写线程：处理所有出站消息
    let writer_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let packet = encode_packet(message.message_type, message.seq, &message.body);
            if let Err(error) = writer.write_all(&packet).await {
                return Err(error);
            }
        }
        Ok::<(), std::io::Error>(())
    });

    // === 主消息循环 ===
    loop {
        let mut header_buf = [0u8; HEADER_LEN];
        let read_header = timeout(
            std::time::Duration::from_secs(config.heartbeat_timeout_secs),
            reader.read_exact(&mut header_buf),
        )
        .await;

        match read_header {
            Ok(Ok(_)) => {}
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                info!(peer = %peer_addr, "peer closed connection");
                break;
            }
            Ok(Err(error)) => return Err(Box::new(error)),
            Err(_) => {
                warn!(peer = %peer_addr, "heartbeat timeout");
                break;
            }
        }

        let header = match parse_header(header_buf) {
            Ok(value) => value,
            Err(e) => {
                warn!(peer = %peer_addr, error = %e, "invalid header");
                break;
            }
        };

        if header.body_len as usize > config.max_body_len {
            warn!(peer = %peer_addr, body_len = header.body_len, "body too large");
            break;
        }

        let mut body = vec![0u8; header.body_len as usize];
        reader.read_exact(&mut body).await?;
        let packet = Packet::new(header, body);

        let msg_type = match MessageType::from_u16(packet.header.msg_type) {
            Some(t) => t,
            None => {
                warn!(peer = %peer_addr, msg_type = packet.header.msg_type, "unknown message type");
                continue;
            }
        };

        // 处理聊天消息
        let started_at = Instant::now();
        match msg_type {
            MessageType::ChatPrivateReq => {
                if let Err(e) = chat_service::handle_chat_private(
                    &chat_store,
                    &chat_sessions,
                    &player_id,
                    &packet,
                    &tx,
                )
                .await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_chat_private failed");
                }
            }
            MessageType::ChatGroupReq => {
                if let Err(e) = chat_service::handle_chat_group(
                    &chat_store,
                    &chat_sessions,
                    &player_id,
                    &packet,
                    &tx,
                )
                .await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_chat_group failed");
                }
            }
            MessageType::GroupCreateReq => {
                if let Err(e) =
                    chat_service::handle_group_create(&chat_store, &player_id, &packet, &tx).await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_group_create failed");
                }
            }
            MessageType::GroupJoinReq => {
                if let Err(e) =
                    chat_service::handle_group_join(&chat_store, &player_id, &packet, &tx).await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_group_join failed");
                }
            }
            MessageType::GroupLeaveReq => {
                if let Err(e) =
                    chat_service::handle_group_leave(&chat_store, &player_id, &packet, &tx).await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_group_leave failed");
                }
            }
            MessageType::GroupDismissReq => {
                if let Err(e) =
                    chat_service::handle_group_dismiss(&chat_store, &player_id, &packet, &tx).await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_group_dismiss failed");
                }
            }
            MessageType::GroupListReq => {
                if let Err(e) =
                    chat_service::handle_group_list(&chat_store, &player_id, &packet, &tx).await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_group_list failed");
                }
            }
            MessageType::ChatHistoryReq => {
                if let Err(e) =
                    chat_service::handle_chat_history(&chat_store, &player_id, &packet, &tx).await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_chat_history failed");
                }
            }
            _ => {
                warn!(peer = %peer_addr, msg_type = ?msg_type, "unsupported message type");
            }
        }
        METRICS.record_request();
        METRICS.record_latency(started_at.elapsed().as_millis() as u64);
    }

    // 注销聊天会话
    chat_service::unregister_session(&chat_sessions, &player_id).await;
    if let Err(e) = online_route::clear_online_route(
        &config.redis_url,
        &config.redis_key_prefix,
        &player_id,
        &config.service_instance_id,
    )
    .await
    {
        warn!(
            player_id = %player_id,
            instance_id = %config.service_instance_id,
            error = %e,
            "failed to clear chat online route"
        );
    }

    let _ = writer_task.await;

    Ok(())
}

async fn read_auth_request<R, W>(
    reader: &mut R,
    writer: &mut W,
    config: &Config,
) -> Result<String, Box<dyn std::error::Error>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // 读取认证请求头
    let mut header_buf = [0u8; HEADER_LEN];
    timeout(
        std::time::Duration::from_secs(config.heartbeat_timeout_secs),
        reader.read_exact(&mut header_buf),
    )
    .await??;

    let header = parse_header(header_buf)?;
    match MessageType::from_u16(header.msg_type) {
        Some(MessageType::ChatAuthReq) => {}
        Some(other) => {
            return Err(format!("expected chat auth request, got {:?}", other).into());
        }
        None => {
            return Err(format!("unknown auth msg_type: {}", header.msg_type).into());
        }
    }

    if header.body_len as usize > config.max_body_len {
        return Err("body too large".into());
    }

    let mut body = vec![0u8; header.body_len as usize];
    reader.read_exact(&mut body).await?;

    // 解析认证请求
    let auth_req = ChatAuthReq::decode(&*body).map_err(|e| format!("decode error: {}", e))?;

    // 使用与 game-server 相同的票据验证逻辑
    match verify_ticket(&config.ticket_secret, &auth_req.token) {
        Ok(ticket_payload) => {
            let player_id = ticket_payload.player_id;
            let redis_client = redis::Client::open(config.redis_url.as_str())?;
            let mut redis = redis_client.get_multiplexed_async_connection().await?;
            let ticket_key = ticket_key(&config.redis_key_prefix, &auth_req.token);
            let ticket_version_key = ticket_version_key(&config.redis_key_prefix, &player_id);
            let ticket_owner: Option<String> = redis.get(ticket_key).await?;
            if let Err(error_code) = validate_ticket_owner(ticket_owner.as_deref(), &player_id) {
                write_auth_response(writer, header.seq, false, error_code).await?;
                return Err(format!("auth failed: {}", error_code).into());
            }

            let current_ticket_version: Option<u64> = redis.get(ticket_version_key).await?;
            if let Err(error_code) =
                validate_ticket_version(ticket_payload.ver, current_ticket_version)
            {
                write_auth_response(writer, header.seq, false, error_code).await?;
                return Err(format!("auth failed: {}", error_code).into());
            }

            write_auth_response(writer, header.seq, true, "").await?;
            Ok(player_id)
        }
        Err(e) => {
            write_auth_response(writer, header.seq, false, e).await?;
            Err(format!("auth failed: {}", e).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ticket::hash_ticket;

    #[test]
    fn ticket_key_uses_prefix_and_sha256_hash() {
        let ticket = "payload.signature";
        assert_eq!(
            ticket_key("dev:", ticket),
            format!("dev:ticket:{}", hash_ticket(ticket))
        );
    }

    #[test]
    fn ticket_version_key_uses_prefix_and_player_id() {
        assert_eq!(
            ticket_version_key("dev:", "player-1"),
            "dev:player-ticket-version:player-1"
        );
    }

    #[test]
    fn validate_ticket_owner_accepts_matching_owner() {
        assert_eq!(validate_ticket_owner(Some("player-1"), "player-1"), Ok(()));
    }

    #[test]
    fn validate_ticket_owner_rejects_missing_owner_as_revoked() {
        assert_eq!(
            validate_ticket_owner(None, "player-1"),
            Err("TICKET_REVOKED")
        );
    }

    #[test]
    fn validate_ticket_owner_rejects_mismatch_as_revoked() {
        assert_eq!(
            validate_ticket_owner(Some("player-2"), "player-1"),
            Err("TICKET_REVOKED")
        );
    }

    #[test]
    fn validate_ticket_version_accepts_matching_or_missing_versions() {
        assert_eq!(validate_ticket_version(Some(2), Some(2)), Ok(()));
        assert_eq!(validate_ticket_version(None, None), Ok(()));
    }

    #[test]
    fn validate_ticket_version_rejects_mismatch_as_revoked() {
        assert_eq!(
            validate_ticket_version(Some(2), Some(3)),
            Err("TICKET_REVOKED")
        );
    }
}
