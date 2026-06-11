use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use prost::Message;
use redis::AsyncCommands;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::chat_service::{self, ChatSessionMap};
use crate::chat_store::ChatStore;
use crate::metrics::METRICS;
use crate::online_route;
use crate::proto::chat::{ChatAuthReq, ChatAuthRes};
use crate::protocol::{HEADER_LEN, OutboundMessage, Packet, encode_packet, parse_header};
use crate::ticket::{hash_ticket, verify_ticket};

const PLAYER_CONNECTION_LIMIT_EXCEEDED: &str = "PLAYER_CONNECTION_LIMIT_EXCEEDED";
const IP_CONNECTION_LIMIT_EXCEEDED: &str = "IP_CONNECTION_LIMIT_EXCEEDED";

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

#[derive(Debug)]
pub struct ConnectionRateLimiter {
    window_started_at: Option<Instant>,
    count: u64,
}

impl ConnectionRateLimiter {
    pub fn new() -> Self {
        Self {
            window_started_at: None,
            count: 0,
        }
    }

    pub fn allow(&mut self, now: Instant, window_ms: u64, max_messages: u64) -> bool {
        if window_ms == 0 || max_messages == 0 {
            return true;
        }

        let window = Duration::from_millis(window_ms);
        if self
            .window_started_at
            .is_none_or(|started_at| now.saturating_duration_since(started_at) >= window)
        {
            self.window_started_at = Some(now);
            self.count = 0;
        }

        self.count = self.count.saturating_add(1);
        self.count <= max_messages
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchDecision {
    Dispatch,
    RateLimited,
}

pub fn dispatch_decision(
    limiter: &mut ConnectionRateLimiter,
    now: Instant,
    window_ms: u64,
    max_messages: u64,
) -> DispatchDecision {
    if limiter.allow(now, window_ms, max_messages) {
        DispatchDecision::Dispatch
    } else {
        DispatchDecision::RateLimited
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionLimitExceeded {
    Player {
        player_id: String,
        current: u64,
        limit: u64,
    },
    Ip {
        ip: IpAddr,
        current: u64,
        limit: u64,
    },
}

impl ConnectionLimitExceeded {
    fn error_code(&self) -> &'static str {
        match self {
            Self::Player { .. } => PLAYER_CONNECTION_LIMIT_EXCEEDED,
            Self::Ip { .. } => IP_CONNECTION_LIMIT_EXCEEDED,
        }
    }

    fn current(&self) -> u64 {
        match self {
            Self::Player { current, .. } | Self::Ip { current, .. } => *current,
        }
    }

    fn limit(&self) -> u64 {
        match self {
            Self::Player { limit, .. } | Self::Ip { limit, .. } => *limit,
        }
    }
}

#[derive(Debug, Default)]
struct ConnectionLimitCounts {
    by_player: HashMap<String, u64>,
    by_ip: HashMap<IpAddr, u64>,
}

#[derive(Debug)]
pub struct ConnectionLimitTracker {
    max_per_player: u64,
    max_per_ip: u64,
    counts: Mutex<ConnectionLimitCounts>,
}

impl ConnectionLimitTracker {
    pub fn new(max_per_player: u64, max_per_ip: u64) -> Self {
        Self {
            max_per_player,
            max_per_ip,
            counts: Mutex::new(ConnectionLimitCounts::default()),
        }
    }

    pub fn acquire(
        self: &Arc<Self>,
        player_id: &str,
        ip: IpAddr,
    ) -> Result<ConnectionLimitGuard, ConnectionLimitExceeded> {
        let mut counts = self.counts.lock().expect("connection limit mutex poisoned");
        let player_current = counts.by_player.get(player_id).copied().unwrap_or(0);
        if self.max_per_player > 0 && player_current >= self.max_per_player {
            return Err(ConnectionLimitExceeded::Player {
                player_id: player_id.to_string(),
                current: player_current,
                limit: self.max_per_player,
            });
        }

        let ip_current = counts.by_ip.get(&ip).copied().unwrap_or(0);
        if self.max_per_ip > 0 && ip_current >= self.max_per_ip {
            return Err(ConnectionLimitExceeded::Ip {
                ip,
                current: ip_current,
                limit: self.max_per_ip,
            });
        }

        if self.max_per_player > 0 {
            counts
                .by_player
                .insert(player_id.to_string(), player_current.saturating_add(1));
        }
        if self.max_per_ip > 0 {
            counts.by_ip.insert(ip, ip_current.saturating_add(1));
        }

        Ok(ConnectionLimitGuard {
            tracker: Arc::clone(self),
            player_id: player_id.to_string(),
            ip,
            count_player: self.max_per_player > 0,
            count_ip: self.max_per_ip > 0,
            released: false,
        })
    }

    #[cfg(test)]
    fn count_for_player(&self, player_id: &str) -> u64 {
        self.counts
            .lock()
            .expect("connection limit mutex poisoned")
            .by_player
            .get(player_id)
            .copied()
            .unwrap_or(0)
    }

    #[cfg(test)]
    fn count_for_ip(&self, ip: IpAddr) -> u64 {
        self.counts
            .lock()
            .expect("connection limit mutex poisoned")
            .by_ip
            .get(&ip)
            .copied()
            .unwrap_or(0)
    }

    fn release(&self, player_id: &str, ip: IpAddr, count_player: bool, count_ip: bool) {
        let mut counts = self.counts.lock().expect("connection limit mutex poisoned");
        if count_player {
            decrement_player_count(&mut counts.by_player, player_id);
        }
        if count_ip {
            decrement_ip_count(&mut counts.by_ip, ip);
        }
    }
}

#[derive(Debug)]
pub struct ConnectionLimitGuard {
    tracker: Arc<ConnectionLimitTracker>,
    player_id: String,
    ip: IpAddr,
    count_player: bool,
    count_ip: bool,
    released: bool,
}

impl Drop for ConnectionLimitGuard {
    fn drop(&mut self) {
        if !self.released {
            self.tracker
                .release(&self.player_id, self.ip, self.count_player, self.count_ip);
            self.released = true;
        }
    }
}

fn decrement_player_count(counts: &mut HashMap<String, u64>, player_id: &str) {
    if let Some(count) = counts.get_mut(player_id) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            counts.remove(player_id);
        }
    }
}

fn decrement_ip_count(counts: &mut HashMap<IpAddr, u64>, ip: IpAddr) {
    if let Some(count) = counts.get_mut(&ip) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            counts.remove(&ip);
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub bind_addr: String,
    pub heartbeat_timeout_secs: u64,
    pub max_body_len: usize,
    pub msg_rate_window_ms: u64,
    pub msg_rate_max: u64,
    pub max_connections_per_player: u64,
    pub max_connections_per_ip: u64,
    pub ticket_secret: String,
    pub redis_url: String,
    pub redis_key_prefix: String,
    pub service_instance_id: String,
    pub online_route_ttl_secs: u64,
    pub outbound_queue_capacity: usize,
}

pub async fn run(
    config: Config,
    chat_store: ChatStore,
    chat_sessions: ChatSessionMap,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(&config.bind_addr).await?;

    info!(
        addr = %config.bind_addr,
        max_connections_per_player = config.max_connections_per_player,
        max_connections_per_ip = config.max_connections_per_ip,
        "chat server listening"
    );

    let connection_limits = Arc::new(ConnectionLimitTracker::new(
        config.max_connections_per_player,
        config.max_connections_per_ip,
    ));

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
        let connection_limits = Arc::clone(&connection_limits);
        let peer_ip = peer_addr.ip();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(
                socket,
                peer_addr.to_string(),
                peer_ip,
                chat_store,
                chat_sessions,
                config,
                connection_limits,
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
    peer_ip: IpAddr,
    chat_store: ChatStore,
    chat_sessions: ChatSessionMap,
    config: Config,
    connection_limits: Arc<ConnectionLimitTracker>,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(socket);
    let (tx, mut rx) = mpsc::channel::<OutboundMessage>(config.outbound_queue_capacity);
    let mut rate_limiter = ConnectionRateLimiter::new();

    // === 认证阶段 ===
    let auth_started_at = Instant::now();
    let auth = match read_auth_request(&mut reader, &mut writer, &config).await {
        Ok(auth) => auth,
        Err(e) => {
            METRICS.record_request();
            METRICS.record_latency(auth_started_at.elapsed().as_millis() as u64);
            warn!(peer = %peer_addr, error = %e, "auth failed");
            return Ok(());
        }
    };
    METRICS.record_request();
    METRICS.record_latency(auth_started_at.elapsed().as_millis() as u64);

    let player_id = auth.player_id;
    let _connection_limit_guard = match connection_limits.acquire(&player_id, peer_ip) {
        Ok(guard) => guard,
        Err(error) => {
            let error_code = error.error_code();
            warn!(
                peer = %peer_addr,
                peer_ip = %peer_ip,
                player_id = %player_id,
                current = error.current(),
                limit = error.limit(),
                error_code = error_code,
                "chat connection limit exceeded"
            );
            write_auth_response(&mut writer, auth.seq, false, error_code).await?;
            return Ok(());
        }
    };

    write_auth_response(&mut writer, auth.seq, true, "").await?;
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
    let loop_error: Option<String> = loop {
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
                break None;
            }
            Ok(Err(error)) => break Some(error.to_string()),
            Err(_) => {
                warn!(peer = %peer_addr, "heartbeat timeout");
                break None;
            }
        }

        let header = match parse_header(header_buf) {
            Ok(value) => value,
            Err(e) => {
                warn!(peer = %peer_addr, error = %e, "invalid header");
                break None;
            }
        };

        if header.body_len as usize > config.max_body_len {
            warn!(peer = %peer_addr, body_len = header.body_len, "body too large");
            break None;
        }

        let mut body = vec![0u8; header.body_len as usize];
        if let Err(error) = reader.read_exact(&mut body).await {
            break Some(error.to_string());
        }
        let packet = Packet::new(header, body);

        let msg_type = match MessageType::from_u16(packet.header.msg_type) {
            Some(t) => t,
            None => {
                warn!(peer = %peer_addr, msg_type = packet.header.msg_type, "unknown message type");
                continue;
            }
        };

        let started_at = Instant::now();
        if dispatch_decision(
            &mut rate_limiter,
            started_at,
            config.msg_rate_window_ms,
            config.msg_rate_max,
        ) == DispatchDecision::RateLimited
        {
            warn!(
                peer = %peer_addr,
                player_id = %player_id,
                msg_type = ?msg_type,
                window_ms = config.msg_rate_window_ms,
                max_messages = config.msg_rate_max,
                "chat message rate exceeded"
            );
            if let Err(e) = chat_service::queue_error(
                &tx,
                packet.header.seq,
                "MSG_RATE_EXCEEDED",
                "message rate exceeded",
            ) {
                warn!(
                    peer = %peer_addr,
                    player_id = %player_id,
                    msg_type = ?msg_type,
                    error = %e,
                    "failed to queue chat message rate exceeded error"
                );
            }
            continue;
        }

        debug!(
            peer = %peer_addr,
            player_id = %player_id,
            msg_type = ?msg_type,
            "dispatching chat client message"
        );

        // 处理聊天消息
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
    };

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

    drop(tx);
    let _ = writer_task.await;

    match loop_error {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}

#[derive(Debug)]
struct AuthenticatedPlayer {
    player_id: String,
    seq: u32,
}

async fn read_auth_request<R, W>(
    reader: &mut R,
    writer: &mut W,
    config: &Config,
) -> Result<AuthenticatedPlayer, Box<dyn std::error::Error>>
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

            Ok(AuthenticatedPlayer {
                player_id,
                seq: header.seq,
            })
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
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::Arc;

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

    #[test]
    fn rate_limiter_disabled_allows_all_messages() {
        let mut limiter = ConnectionRateLimiter::new();
        let now = Instant::now();

        assert!(limiter.allow(now, 1000, 0));
        assert!(limiter.allow(now, 0, 1));
    }

    #[test]
    fn rate_limiter_rejects_after_window_limit() {
        let mut limiter = ConnectionRateLimiter::new();
        let now = Instant::now();

        assert!(limiter.allow(now, 1000, 2));
        assert!(limiter.allow(now, 1000, 2));
        assert!(!limiter.allow(now, 1000, 2));
    }

    #[test]
    fn rate_limiter_resets_after_window_rolls() {
        let mut limiter = ConnectionRateLimiter::new();
        let now = Instant::now();

        assert!(limiter.allow(now, 1000, 1));
        assert!(!limiter.allow(now + Duration::from_millis(999), 1000, 1));
        assert!(limiter.allow(now + Duration::from_millis(1000), 1000, 1));
    }

    #[test]
    fn connection_limits_disabled_do_not_track_counts() {
        let tracker = Arc::new(ConnectionLimitTracker::new(0, 0));
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));

        let guard = tracker.acquire("player-1", ip).unwrap();

        assert_eq!(tracker.count_for_player("player-1"), 0);
        assert_eq!(tracker.count_for_ip(ip), 0);

        drop(guard);
        assert_eq!(tracker.count_for_player("player-1"), 0);
        assert_eq!(tracker.count_for_ip(ip), 0);
    }

    #[test]
    fn connection_limits_reject_player_at_boundary_and_release_on_drop() {
        let tracker = Arc::new(ConnectionLimitTracker::new(1, 0));
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let first = tracker.acquire("player-1", ip).unwrap();

        assert_eq!(
            tracker.acquire("player-1", ip).unwrap_err(),
            ConnectionLimitExceeded::Player {
                player_id: "player-1".to_string(),
                current: 1,
                limit: 1,
            }
        );
        assert_eq!(tracker.count_for_player("player-1"), 1);

        drop(first);
        assert_eq!(tracker.count_for_player("player-1"), 0);
        let second = tracker.acquire("player-1", ip).unwrap();
        assert_eq!(tracker.count_for_player("player-1"), 1);
        drop(second);
    }

    #[test]
    fn connection_limits_reject_ip_without_incrementing_player_count() {
        let tracker = Arc::new(ConnectionLimitTracker::new(2, 1));
        let ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let first = tracker.acquire("player-1", ip).unwrap();

        assert_eq!(
            tracker.acquire("player-2", ip).unwrap_err(),
            ConnectionLimitExceeded::Ip {
                ip,
                current: 1,
                limit: 1,
            }
        );
        assert_eq!(tracker.count_for_player("player-2"), 0);
        assert_eq!(tracker.count_for_ip(ip), 1);

        drop(first);
        assert_eq!(tracker.count_for_ip(ip), 0);
    }

    #[test]
    fn dispatch_decision_blocks_over_limit_before_business_dispatch() {
        let mut limiter = ConnectionRateLimiter::new();
        let now = Instant::now();

        assert_eq!(
            dispatch_decision(&mut limiter, now, 1000, 1),
            DispatchDecision::Dispatch
        );
        assert_eq!(
            dispatch_decision(&mut limiter, now, 1000, 1),
            DispatchDecision::RateLimited
        );
    }
}
