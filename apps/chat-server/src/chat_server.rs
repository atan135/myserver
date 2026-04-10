use prost::Message;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{info, warn};

use crate::chat_service::{self, ChatSessionMap, new_chat_session_map};
use crate::chat_store::ChatStore;
use crate::protocol::{encode_packet, parse_header, OutboundMessage, Packet, HEADER_LEN};
use crate::proto::chat::{ChatAuthReq, ChatAuthRes};
use crate::ticket::verify_ticket;

#[derive(Debug, Clone, Copy)]
pub enum MessageType {
    ChatPrivateReq = 1401,
    ChatPrivateRes = 1402,
    ChatGroupReq = 1403,
    ChatGroupRes = 1404,
    ChatPush = 1405,
    GroupCreateReq = 1411,
    GroupCreateRes = 1412,
    GroupJoinReq = 1413,
    GroupJoinRes = 1414,
    GroupLeaveReq = 1415,
    GroupLeaveRes = 1416,
    GroupDismissReq = 1417,
    GroupDismissRes = 1418,
    GroupListReq = 1419,
    GroupListRes = 1420,
    ChatHistoryReq = 1421,
    ChatHistoryRes = 1422,
    ErrorRes = 9000,
}

impl MessageType {
    pub fn from_u16(value: u16) -> Option<Self> {
        match value {
            1401 => Some(Self::ChatPrivateReq),
            1402 => Some(Self::ChatPrivateRes),
            1403 => Some(Self::ChatGroupReq),
            1404 => Some(Self::ChatGroupRes),
            1405 => Some(Self::ChatPush),
            1411 => Some(Self::GroupCreateReq),
            1412 => Some(Self::GroupCreateRes),
            1413 => Some(Self::GroupJoinReq),
            1414 => Some(Self::GroupJoinRes),
            1415 => Some(Self::GroupLeaveReq),
            1416 => Some(Self::GroupLeaveRes),
            1417 => Some(Self::GroupDismissReq),
            1418 => Some(Self::GroupDismissRes),
            1419 => Some(Self::GroupListReq),
            1420 => Some(Self::GroupListRes),
            1421 => Some(Self::ChatHistoryReq),
            1422 => Some(Self::ChatHistoryRes),
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
}

pub async fn run(
    config: Config,
    chat_store: ChatStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(&config.bind_addr).await?;
    let chat_sessions: ChatSessionMap = new_chat_session_map();

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
            if let Err(e) = handle_connection(socket, peer_addr.to_string(), chat_store, chat_sessions, config).await {
                warn!(peer = %peer_addr, error = %e, "connection handler error");
            }
        });
    }

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
    let player_id = match read_auth_request(&mut reader, &mut writer, &config).await {
        Ok(id) => id,
        Err(e) => {
            warn!(peer = %peer_addr, error = %e, "auth failed");
            return Ok(());
        }
    };

    info!(peer = %peer_addr, player_id = %player_id, "player authenticated");

    // 注册聊天会话
    chat_service::register_session(&chat_sessions, player_id.clone(), tx.clone()).await;

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
        match msg_type {
            MessageType::ChatPrivateReq => {
                if let Err(e) = chat_service::handle_chat_private(
                    &chat_store, &chat_sessions, &player_id, &packet, &tx,
                )
                .await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_chat_private failed");
                }
            }
            MessageType::ChatGroupReq => {
                if let Err(e) = chat_service::handle_chat_group(
                    &chat_store, &chat_sessions, &player_id, &packet, &tx,
                )
                .await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_chat_group failed");
                }
            }
            MessageType::GroupCreateReq => {
                if let Err(e) = chat_service::handle_group_create(
                    &chat_store, &player_id, &packet, &tx,
                )
                .await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_group_create failed");
                }
            }
            MessageType::GroupJoinReq => {
                if let Err(e) = chat_service::handle_group_join(
                    &chat_store, &player_id, &packet, &tx,
                )
                .await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_group_join failed");
                }
            }
            MessageType::GroupLeaveReq => {
                if let Err(e) = chat_service::handle_group_leave(
                    &chat_store, &player_id, &packet, &tx,
                )
                .await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_group_leave failed");
                }
            }
            MessageType::GroupDismissReq => {
                if let Err(e) = chat_service::handle_group_dismiss(
                    &chat_store, &player_id, &packet, &tx,
                )
                .await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_group_dismiss failed");
                }
            }
            MessageType::GroupListReq => {
                if let Err(e) = chat_service::handle_group_list(
                    &chat_store, &player_id, &packet, &tx,
                )
                .await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_group_list failed");
                }
            }
            MessageType::ChatHistoryReq => {
                if let Err(e) = chat_service::handle_chat_history(
                    &chat_store, &player_id, &packet, &tx,
                )
                .await
                {
                    warn!(peer = %peer_addr, error = %e, "handle_chat_history failed");
                }
            }
            _ => {
                warn!(peer = %peer_addr, msg_type = ?msg_type, "unsupported message type");
            }
        }
    }

    // 注销聊天会话
    chat_service::unregister_session(&chat_sessions, &player_id).await;

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

    if header.body_len as usize > config.max_body_len {
        return Err("body too large".into());
    }

    let mut body = vec![0u8; header.body_len as usize];
    reader.read_exact(&mut body).await?;

    // 解析认证请求
    let auth_req = ChatAuthReq::decode(&*body).map_err(|e| format!("decode error: {}", e))?;

    // 使用与 game-server 相同的票据验证逻辑
    match verify_ticket(&config.ticket_secret, &auth_req.token) {
        Ok(player_id) => {
            let res = ChatAuthRes { ok: true, error_code: String::new() };
            let mut buf = Vec::new();
            res.encode(&mut buf)?;
            let packet = encode_packet(1402, header.seq, &buf);
            writer.write_all(&packet).await?;
            Ok(player_id)
        }
        Err(e) => {
            let res = ChatAuthRes { ok: false, error_code: e.to_string() };
            let mut buf = Vec::new();
            res.encode(&mut buf)?;
            let packet = encode_packet(1402, header.seq, &buf);
            writer.write_all(&packet).await?;
            Err(format!("auth failed: {}", e).into())
        }
    }
}
