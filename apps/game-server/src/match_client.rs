//! MatchService gRPC Client
//!
//! GameServer 通过此客户端调用 MatchService 的内部接口

use service_registry::RegistryClient;
use std::sync::Arc;
use tokio::sync::Mutex;
use tonic::transport::Channel;

use crate::proto::myserver::matchservice::match_internal_client::MatchInternalClient;
use crate::proto::myserver::matchservice::{
    CreateRoomAndJoinReq, CreateRoomAndJoinRes, MatchEndReq, MatchEndRes,
    PlayerJoinedReq, PlayerJoinedRes, PlayerLeftReq, PlayerLeftRes,
};

/// MatchClient 配置
#[derive(Clone)]
pub struct MatchClientConfig {
    /// MatchService 地址
    pub addr: String,
}

impl MatchClientConfig {
    pub async fn from_env() -> Self {
        let fallback_addr = std::env::var("MATCH_SERVICE_ADDR")
            .unwrap_or_else(|_| "http://127.0.0.1:9002".to_string());
        let registry_enabled = std::env::var("REGISTRY_ENABLED")
            .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "True"))
            .unwrap_or(false);

        if !registry_enabled {
            return Self { addr: fallback_addr };
        }

        let registry_url = std::env::var("REGISTRY_URL")
            .or_else(|_| std::env::var("REDIS_URL"))
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let service_name = std::env::var("MATCH_SERVICE_NAME")
            .unwrap_or_else(|_| "match-service".to_string());

        match RegistryClient::new(&registry_url, "game-server", "match-discovery").await {
            Ok(client) => match client.discover_one(&service_name).await {
                Ok(Some(instance)) => Self {
                    addr: format!("http://{}:{}", instance.host, instance.port),
                },
                Ok(None) => Self { addr: fallback_addr },
                Err(error) => {
                    tracing::warn!(error = %error, "failed to discover match-service, using fallback");
                    Self { addr: fallback_addr }
                }
            },
            Err(error) => {
                tracing::warn!(error = %error, "failed to create registry client for match discovery, using fallback");
                Self { addr: fallback_addr }
            }
        }
    }
}

/// MatchClient
pub struct MatchClient {
    inner: MatchInternalClient<Channel>,
}

impl MatchClient {
    /// 创建 MatchClient
    pub async fn new(config: MatchClientConfig) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let addr: Box<str> = config.addr.clone().into_boxed_str();
        let channel = tonic::transport::Endpoint::from_static(Box::leak(addr))
            .connect()
            .await?;

        let inner = MatchInternalClient::new(channel);

        tracing::info!(addr = %config.addr, "connected to match-service");

        Ok(Self { inner })
    }

    /// 通知 MatchService 房间已创建
    pub async fn create_room_and_join(
        &mut self,
        match_id: &str,
        room_id: &str,
        player_ids: &[String],
        mode: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let req = CreateRoomAndJoinReq {
            match_id: match_id.to_string(),
            room_id: room_id.to_string(),
            player_ids: player_ids.to_vec(),
            mode: mode.to_string(),
        };

        let resp = self.inner.create_room_and_join(req).await?;
        let res: CreateRoomAndJoinRes = resp.into_inner();

        if res.ok {
            tracing::info!(
                match_id = %match_id,
                room_id = %room_id,
                players = ?player_ids,
                mode = %mode,
                "CreateRoomAndJoin success"
            );
            Ok(())
        } else {
            tracing::error!(
                match_id = %match_id,
                error_code = %res.error_code,
                "CreateRoomAndJoin failed"
            );
            Err(format!("CreateRoomAndJoin failed: {}", res.error_code).into())
        }
    }

    /// 通知 MatchService 玩家已进入房间
    pub async fn player_joined(
        &mut self,
        match_id: &str,
        player_id: &str,
        room_id: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let req = PlayerJoinedReq {
            match_id: match_id.to_string(),
            player_id: player_id.to_string(),
            room_id: room_id.to_string(),
        };

        let resp = self.inner.player_joined(req).await?;
        let res: PlayerJoinedRes = resp.into_inner();

        if res.ok {
            tracing::info!(
                match_id = %match_id,
                player_id = %player_id,
                room_id = %room_id,
                "PlayerJoined success"
            );
            Ok(())
        } else {
            tracing::error!(
                match_id = %match_id,
                player_id = %player_id,
                error_code = %res.error_code,
                "PlayerJoined failed"
            );
            Err(format!("PlayerJoined failed: {}", res.error_code).into())
        }
    }

    /// 通知 MatchService 玩家已离开房间
    pub async fn player_left(
        &mut self,
        match_id: &str,
        player_id: &str,
        reason: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let req = PlayerLeftReq {
            match_id: match_id.to_string(),
            player_id: player_id.to_string(),
            reason: reason.to_string(),
        };

        let resp = self.inner.player_left(req).await?;
        let res: PlayerLeftRes = resp.into_inner();

        if res.ok {
            tracing::info!(
                match_id = %match_id,
                player_id = %player_id,
                reason = %reason,
                match_should_abort = res.match_should_abort,
                "PlayerLeft success"
            );
            Ok(res.match_should_abort)
        } else {
            tracing::error!(
                match_id = %match_id,
                player_id = %player_id,
                error_code = %res.error_code,
                "PlayerLeft failed"
            );
            Err(format!("PlayerLeft failed: {}", res.error_code).into())
        }
    }

    /// 通知 MatchService 对局结束
    pub async fn match_end(
        &mut self,
        match_id: &str,
        room_id: &str,
        reason: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let req = MatchEndReq {
            match_id: match_id.to_string(),
            room_id: room_id.to_string(),
            reason: reason.to_string(),
        };

        let resp = self.inner.match_end(req).await?;
        let res: MatchEndRes = resp.into_inner();

        if res.ok {
            tracing::info!(
                match_id = %match_id,
                room_id = %room_id,
                reason = %reason,
                "MatchEnd success"
            );
            Ok(())
        } else {
            tracing::error!(
                match_id = %match_id,
                room_id = %room_id,
                error_code = %res.error_code,
                "MatchEnd failed"
            );
            Err(format!("MatchEnd failed: {}", res.error_code).into())
        }
    }
}

/// Shared MatchClient
pub type SharedMatchClient = Arc<Mutex<Option<MatchClient>>>;

/// 创建未连接的 MatchClient
pub fn create_match_client_shared() -> SharedMatchClient {
    Arc::new(Mutex::new(None))
}

/// 初始化 MatchClient 连接
pub async fn init_match_client(
    client: &SharedMatchClient,
    config: MatchClientConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let new_client = MatchClient::new(config).await?;
    let mut guard = client.lock().await;
    *guard = Some(new_client);
    Ok(())
}
