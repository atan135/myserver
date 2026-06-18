//! MatchService gRPC Client
//!
//! GameServer 通过此客户端调用 MatchService 的内部接口

use service_registry::{record_discovery_metric, RegistryClient};
use std::error::Error;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tonic::transport::Channel;

use crate::proto::myserver::matchservice::match_internal_client::MatchInternalClient;
use crate::proto::myserver::matchservice::{
    CreateRoomAndJoinReq, CreateRoomAndJoinRes, MatchEndReq, MatchEndRes, PlayerJoinedReq,
    PlayerJoinedRes, PlayerLeftReq, PlayerLeftRes,
};

/// MatchClient 配置
pub const DEFAULT_MATCH_REDISCOVERY_INTERVAL_SECS: u64 = 30;
const DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME: &str = "DISALLOW_LEGACY_DIRECT_CONFIG";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MatchClientConfig {
    /// MatchService 地址
    pub addr: String,
    pub fallback_addr: String,
    pub local_discovery_fallback_enabled: bool,
    pub registry_enabled: bool,
    pub discovery_required: bool,
    pub registry_url: String,
    pub registry_key_prefix: String,
    pub service_name: String,
    pub rediscovery_interval_secs: u64,
}

impl MatchClientConfig {
    pub async fn from_env() -> Self {
        validate_legacy_direct_config(&["MATCH_SERVICE_ADDR"]);
        let local_discovery_fallback_enabled = is_local_discovery_fallback_env();
        let fallback_addr = if local_discovery_fallback_enabled {
            std::env::var("MATCH_SERVICE_ADDR")
                .unwrap_or_else(|_| "http://127.0.0.1:9002".to_string())
        } else {
            "http://127.0.0.1:9002".to_string()
        };
        let registry_enabled = env_flag("REGISTRY_ENABLED", false);
        let discovery_required = env_flag("DISCOVERY_REQUIRED", false) || is_strict_discovery_env();
        let registry_url = std::env::var("REGISTRY_URL")
            .or_else(|_| std::env::var("REDIS_URL"))
            .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let registry_key_prefix = std::env::var("REGISTRY_KEY_PREFIX")
            .or_else(|_| std::env::var("REDIS_KEY_PREFIX"))
            .unwrap_or_default();
        let service_name =
            std::env::var("MATCH_SERVICE_NAME").unwrap_or_else(|_| "match-service".to_string());
        let rediscovery_interval_secs = env_u64(
            "MATCH_SERVICE_REDISCOVERY_INTERVAL_SECS",
            DEFAULT_MATCH_REDISCOVERY_INTERVAL_SECS,
        );

        if !registry_enabled {
            if discovery_required {
                record_discovery_metric(&service_name, "grpc", "registry", "registry_disabled");
                panic!(
                    "required registry discovery failed: REGISTRY_ENABLED=false for match-service.grpc"
                );
            }
            record_discovery_metric(&service_name, "grpc", "fallback", "fallback_used");
            tracing::info!(
                service = %service_name,
                endpoint = "grpc",
                instance_id = "",
                source = "fallback",
                reason = "registry_disabled",
                addr = %fallback_addr,
                "match-service address resolved"
            );
            return Self {
                addr: fallback_addr.clone(),
                fallback_addr,
                local_discovery_fallback_enabled,
                registry_enabled,
                discovery_required,
                registry_url,
                registry_key_prefix,
                service_name,
                rediscovery_interval_secs,
            };
        }

        let addr = resolve_match_service_addr(
            &registry_url,
            &registry_key_prefix,
            &service_name,
            &fallback_addr,
            discovery_required,
        )
        .await
        .unwrap_or_else(|error| {
            panic!(
                "required registry discovery failed for {}.grpc: {}",
                service_name, error
            )
        })
        .addr;

        Self {
            addr,
            fallback_addr,
            local_discovery_fallback_enabled,
            registry_enabled,
            discovery_required,
            registry_url,
            registry_key_prefix,
            service_name,
            rediscovery_interval_secs,
        }
    }

    pub fn rediscovery_enabled(&self) -> bool {
        self.registry_enabled
    }
}

/// MatchClient
pub struct MatchClient {
    inner: MatchInternalClient<Channel>,
    addr: String,
    reconnect_required: bool,
}

impl MatchClient {
    /// 创建 MatchClient
    pub async fn new(
        config: MatchClientConfig,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let channel = tonic::transport::Endpoint::from_shared(config.addr.clone())?
            .connect()
            .await?;

        let inner = MatchInternalClient::new(channel);

        tracing::info!(addr = %config.addr, "connected to match-service");

        Ok(Self {
            inner,
            addr: config.addr,
            reconnect_required: false,
        })
    }

    pub fn addr(&self) -> &str {
        &self.addr
    }

    pub fn reconnect_required(&self) -> bool {
        self.reconnect_required
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

        let resp = self
            .inner
            .create_room_and_join(req)
            .await
            .map_err(|error| {
                self.reconnect_required = true;
                error
            })?;
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

        let resp = self.inner.player_joined(req).await.map_err(|error| {
            self.reconnect_required = true;
            error
        })?;
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

        let resp = self.inner.player_left(req).await.map_err(|error| {
            self.reconnect_required = true;
            error
        })?;
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

        let resp = self.inner.match_end(req).await.map_err(|error| {
            self.reconnect_required = true;
            error
        })?;
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

pub fn spawn_match_client_rediscovery(
    client: SharedMatchClient,
    config: MatchClientConfig,
) -> Option<JoinHandle<()>> {
    if !config.rediscovery_enabled() {
        tracing::info!(
            service = %config.service_name,
            endpoint = "grpc",
            instance_id = "",
            source = "fallback",
            reason = "registry_disabled",
            "match-service rediscovery disabled because service registry is disabled"
        );
        return None;
    }

    let interval = Duration::from_secs(config.rediscovery_interval_secs.max(1));
    tracing::info!(
        service = %config.service_name,
        endpoint = "grpc",
        instance_id = "",
        source = "registry",
        reason = "watch_started",
        interval_secs = config.rediscovery_interval_secs,
        "match-service rediscovery task started"
    );

    Some(tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;

            let discovered_addr = match resolve_match_service_addr(
                &config.registry_url,
                &config.registry_key_prefix,
                &config.service_name,
                &config.fallback_addr,
                config.discovery_required,
            )
            .await
            {
                Ok(resolved) => resolved.addr,
                Err(error) => {
                    tracing::warn!(
                        service = %config.service_name,
                        endpoint = "grpc",
                        instance_id = "",
                        source = "registry",
                        reason = "registry_error",
                        error = %error,
                        "match-service rediscovery failed; keeping existing client and retrying next tick"
                    );
                    continue;
                }
            };

            match rebuild_match_client_if_needed_with_connector(
                &client,
                &config,
                &discovered_addr,
                MatchClient::new,
            )
            .await
            {
                Ok(true) => {
                    tracing::info!(addr = %discovered_addr, "match-service rediscovery reconnected");
                }
                Ok(false) => {
                    tracing::debug!(
                        addr = %discovered_addr,
                        "match-service rediscovery kept existing client"
                    );
                }
                Err(error) => {
                    tracing::warn!(
                        addr = %discovered_addr,
                        error = %error,
                        "match-service rediscovery reconnect failed; keeping existing client"
                    );
                }
            }
        }
    }))
}

async fn rebuild_match_client_if_needed_with_connector<Connect, ConnectFuture>(
    client: &SharedMatchClient,
    config: &MatchClientConfig,
    discovered_addr: &str,
    connect: Connect,
) -> Result<bool, Box<dyn Error + Send + Sync>>
where
    Connect: FnOnce(MatchClientConfig) -> ConnectFuture,
    ConnectFuture: Future<Output = Result<MatchClient, Box<dyn Error + Send + Sync>>>,
{
    let current_state = current_match_client_state(client).await;
    let reconnect = should_rebuild_match_client(
        current_state.addr.as_deref(),
        current_state.reconnect_required,
        discovered_addr,
    );
    if !reconnect {
        return Ok(false);
    }

    tracing::info!(
        previous_addr = current_state.addr.as_deref().unwrap_or("<none>"),
        reconnect_required = current_state.reconnect_required,
        addr = %discovered_addr,
        "match-service rediscovery rebuilding client"
    );

    let reconnect_config = MatchClientConfig {
        addr: discovered_addr.to_string(),
        ..config.clone()
    };
    let new_client = connect(reconnect_config).await?;
    let mut guard = client.lock().await;
    *guard = Some(new_client);
    Ok(true)
}

struct MatchClientState {
    addr: Option<String>,
    reconnect_required: bool,
}

async fn current_match_client_state(client: &SharedMatchClient) -> MatchClientState {
    let guard = client.lock().await;
    MatchClientState {
        addr: guard.as_ref().map(|client| client.addr().to_string()),
        reconnect_required: guard
            .as_ref()
            .is_some_and(|client| client.reconnect_required()),
    }
}

pub fn should_rebuild_match_client(
    current_addr: Option<&str>,
    reconnect_required: bool,
    discovered_addr: &str,
) -> bool {
    if reconnect_required {
        return true;
    }

    match current_addr {
        Some(current_addr) => current_addr != discovered_addr,
        None => true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryOutcome {
    Found(String),
    NotFound,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchServiceAddrSource {
    Registry,
    Fallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMatchServiceAddr {
    pub addr: String,
    pub source: MatchServiceAddrSource,
}

pub fn resolve_discovery_outcome(
    outcome: DiscoveryOutcome,
    fallback_addr: &str,
    discovery_required: bool,
) -> Result<ResolvedMatchServiceAddr, String> {
    match outcome {
        DiscoveryOutcome::Found(addr) => Ok(ResolvedMatchServiceAddr {
            addr,
            source: MatchServiceAddrSource::Registry,
        }),
        DiscoveryOutcome::NotFound => {
            if discovery_required {
                Err("match-service grpc endpoint not found".to_string())
            } else {
                record_discovery_metric("match-service", "grpc", "fallback", "fallback_used");
                Ok(ResolvedMatchServiceAddr {
                    addr: fallback_addr.to_string(),
                    source: MatchServiceAddrSource::Fallback,
                })
            }
        }
        DiscoveryOutcome::Error(error) => {
            if discovery_required {
                Err(error)
            } else {
                record_discovery_metric("match-service", "grpc", "fallback", "fallback_used");
                Ok(ResolvedMatchServiceAddr {
                    addr: fallback_addr.to_string(),
                    source: MatchServiceAddrSource::Fallback,
                })
            }
        }
    }
}

async fn resolve_match_service_addr(
    registry_url: &str,
    registry_key_prefix: &str,
    service_name: &str,
    fallback_addr: &str,
    discovery_required: bool,
) -> Result<ResolvedMatchServiceAddr, Box<dyn Error + Send + Sync>> {
    let outcome = match RegistryClient::new(registry_url, "game-server", "match-discovery").await {
        Ok(client) => match client
            .with_key_prefix(registry_key_prefix.to_string())
            .discover_endpoint(service_name, "grpc")
            .await
        {
            Ok(Some(endpoint)) => {
                let addr = format!("http://{}:{}", endpoint.host, endpoint.port);
                let instance_id = endpoint
                    .metadata
                    .get("instance_id")
                    .or_else(|| endpoint.metadata.get("service_instance_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                tracing::info!(
                    service = %service_name,
                    endpoint = "grpc",
                    instance_id,
                    source = "registry",
                    reason = "discovered",
                    addr = %addr,
                    "match-service address resolved"
                );
                DiscoveryOutcome::Found(addr)
            }
            Ok(None) => DiscoveryOutcome::NotFound,
            Err(error) => {
                record_discovery_metric(service_name, "grpc", "registry", "registry_error");
                DiscoveryOutcome::Error(error.to_string())
            }
        },
        Err(error) => {
            record_discovery_metric(service_name, "grpc", "registry", "registry_error");
            DiscoveryOutcome::Error(format!(
                "registry client unavailable for match-service discovery: {}",
                error
            ))
        }
    };

    match resolve_discovery_outcome(outcome, fallback_addr, discovery_required) {
        Ok(resolved) => {
            if resolved.source == MatchServiceAddrSource::Fallback {
                tracing::warn!(
                    service = %service_name,
                    endpoint = "grpc",
                    instance_id = "",
                    source = "fallback",
                    reason = "fallback_used",
                    addr = %fallback_addr,
                    "failed to discover match-service grpc endpoint, using fallback"
                );
            }
            Ok(resolved)
        }
        Err(error) => Err(std::io::Error::other(error).into()),
    }
}

fn env_flag(name: &str, default: bool) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(default)
}

fn configured_legacy_direct_config_names(names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter(|name| std::env::var_os(name).is_some())
        .map(|name| (*name).to_string())
        .collect()
}

fn validate_legacy_direct_config(names: &[&str]) {
    if !env_flag(DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME, false) {
        return;
    }

    let configured = configured_legacy_direct_config_names(names);
    if !configured.is_empty() {
        panic!(
            "invalid game-server match client discovery config: {DISALLOW_LEGACY_DIRECT_CONFIG_ENV_NAME}=true forbids legacy direct config: {}; remove these variables and use service registry endpoints instead",
            configured.join(", ")
        );
    }
}

fn is_strict_discovery_env() -> bool {
    ["NODE_ENV", "APP_ENV"].iter().any(|name| {
        std::env::var(name).ok().is_some_and(|value| {
            let value = value.trim();
            value.eq_ignore_ascii_case("production") || value.eq_ignore_ascii_case("test")
        })
    })
}

fn is_local_discovery_fallback_env() -> bool {
    if is_strict_discovery_env() {
        return false;
    }

    let names = ["NODE_ENV", "APP_ENV"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    names.is_empty()
        || names
            .iter()
            .any(|value| matches!(value.as_str(), "development" | "local"))
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use service_registry::{get_discovery_metrics_snapshot, reset_discovery_metrics};
    use std::env;
    use std::sync::{Mutex as StdMutex, OnceLock};

    fn env_lock() -> &'static StdMutex<()> {
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(()))
    }

    fn test_config(addr: &str) -> MatchClientConfig {
        MatchClientConfig {
            addr: addr.to_string(),
            fallback_addr: "http://127.0.0.1:9002".to_string(),
            local_discovery_fallback_enabled: false,
            registry_enabled: true,
            discovery_required: true,
            registry_url: "redis://127.0.0.1:6379".to_string(),
            registry_key_prefix: String::new(),
            service_name: "match-service".to_string(),
            rediscovery_interval_secs: 1,
        }
    }

    fn test_match_client(addr: &str, reconnect_required: bool) -> MatchClient {
        let channel = tonic::transport::Endpoint::from_shared(addr.to_string())
            .expect("test endpoint should be valid")
            .connect_lazy();

        MatchClient {
            inner: MatchInternalClient::new(channel),
            addr: addr.to_string(),
            reconnect_required,
        }
    }

    async fn test_connect_match_client(
        config: MatchClientConfig,
    ) -> Result<MatchClient, Box<dyn Error + Send + Sync>> {
        Ok(test_match_client(&config.addr, false))
    }

    async fn set_shared_client(client: &SharedMatchClient, addr: &str, reconnect_required: bool) {
        let mut guard = client.lock().await;
        *guard = Some(test_match_client(addr, reconnect_required));
    }

    async fn shared_client_state(client: &SharedMatchClient) -> MatchClientState {
        current_match_client_state(client).await
    }

    struct EnvGuard {
        saved: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn capture(names: &[&'static str]) -> Self {
            Self {
                saved: names
                    .iter()
                    .map(|name| (*name, env::var(name).ok()))
                    .collect(),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in self.saved.drain(..) {
                unsafe {
                    match value {
                        Some(value) => env::set_var(name, value),
                        None => env::remove_var(name),
                    }
                }
            }
        }
    }

    const MATCH_CLIENT_ENV_NAMES: &[&str] = &[
        "MATCH_SERVICE_ADDR",
        "DISALLOW_LEGACY_DIRECT_CONFIG",
        "MATCH_SERVICE_NAME",
        "MATCH_SERVICE_REDISCOVERY_INTERVAL_SECS",
        "NODE_ENV",
        "APP_ENV",
        "REGISTRY_ENABLED",
        "DISCOVERY_REQUIRED",
        "REGISTRY_URL",
        "REDIS_URL",
    ];

    #[tokio::test]
    async fn config_uses_default_rediscovery_interval() {
        let _lock = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(MATCH_CLIENT_ENV_NAMES);
        for name in MATCH_CLIENT_ENV_NAMES {
            unsafe {
                env::remove_var(name);
            }
        }

        let config = MatchClientConfig::from_env().await;

        assert!(!config.registry_enabled);
        assert_eq!(
            config.rediscovery_interval_secs,
            DEFAULT_MATCH_REDISCOVERY_INTERVAL_SECS
        );
        assert_eq!(config.addr, "http://127.0.0.1:9002");
    }

    #[tokio::test]
    async fn config_reads_rediscovery_interval() {
        let _lock = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(MATCH_CLIENT_ENV_NAMES);
        for name in MATCH_CLIENT_ENV_NAMES {
            unsafe {
                env::remove_var(name);
            }
        }
        unsafe {
            env::set_var("MATCH_SERVICE_REDISCOVERY_INTERVAL_SECS", "7");
            env::remove_var("DISALLOW_LEGACY_DIRECT_CONFIG");
            env::set_var("MATCH_SERVICE_ADDR", "http://127.0.0.1:19002");
        }

        let config = MatchClientConfig::from_env().await;

        assert_eq!(config.rediscovery_interval_secs, 7);
        assert_eq!(config.addr, "http://127.0.0.1:19002");
    }

    #[tokio::test]
    async fn config_rejects_match_service_addr_when_migration_complete_switch_is_enabled() {
        let _lock = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(MATCH_CLIENT_ENV_NAMES);
        for name in MATCH_CLIENT_ENV_NAMES {
            unsafe {
                env::remove_var(name);
            }
        }
        unsafe {
            env::set_var("DISALLOW_LEGACY_DIRECT_CONFIG", "true");
            env::set_var("MATCH_SERVICE_ADDR", "http://127.0.0.1:19002");
        }

        let error = tokio::spawn(async { MatchClientConfig::from_env().await })
            .await
            .expect_err("legacy direct config should panic in migration complete mode");

        assert!(error.is_panic());
        let payload = error.into_panic();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap_or("");
        assert!(message.contains(
            "DISALLOW_LEGACY_DIRECT_CONFIG=true forbids legacy direct config: MATCH_SERVICE_ADDR"
        ));
    }

    #[test]
    fn local_fallback_is_disabled_in_test_environment() {
        let _lock = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(MATCH_CLIENT_ENV_NAMES);
        for name in MATCH_CLIENT_ENV_NAMES {
            unsafe {
                env::remove_var(name);
            }
        }
        unsafe {
            env::set_var("APP_ENV", "test");
            env::set_var("REGISTRY_ENABLED", "false");
            env::remove_var("DISALLOW_LEGACY_DIRECT_CONFIG");
            env::set_var("MATCH_SERVICE_ADDR", "http://203.0.113.40:19002");
        }

        assert!(!is_local_discovery_fallback_env());
        assert!(is_strict_discovery_env());
    }

    #[test]
    fn rebuild_decision_updates_on_missing_or_changed_client() {
        assert!(should_rebuild_match_client(
            None,
            false,
            "http://127.0.0.1:9002"
        ));
        assert!(should_rebuild_match_client(
            Some("http://127.0.0.1:9002"),
            false,
            "http://127.0.0.1:19002"
        ));
        assert!(!should_rebuild_match_client(
            Some("http://127.0.0.1:9002"),
            false,
            "http://127.0.0.1:9002"
        ));
    }

    #[test]
    fn rebuild_decision_updates_when_existing_client_is_marked_for_reconnect() {
        assert!(should_rebuild_match_client(
            Some("http://127.0.0.1:9002"),
            true,
            "http://127.0.0.1:9002"
        ));
    }

    #[tokio::test]
    async fn rediscovery_rebuilds_client_when_registry_endpoint_changes() {
        let client = create_match_client_shared();
        set_shared_client(&client, "http://127.0.0.1:9002", false).await;

        let rebuilt = rebuild_match_client_if_needed_with_connector(
            &client,
            &test_config("http://127.0.0.1:9002"),
            "http://127.0.0.1:19002",
            test_connect_match_client,
        )
        .await
        .expect("endpoint change should reconnect");

        assert!(rebuilt);
        let state = shared_client_state(&client).await;
        assert_eq!(state.addr.as_deref(), Some("http://127.0.0.1:19002"));
        assert!(!state.reconnect_required);
    }

    #[tokio::test]
    async fn rediscovery_rebuilds_client_when_same_endpoint_requires_reconnect() {
        let client = create_match_client_shared();
        set_shared_client(&client, "http://127.0.0.1:9002", true).await;

        let rebuilt = rebuild_match_client_if_needed_with_connector(
            &client,
            &test_config("http://127.0.0.1:9002"),
            "http://127.0.0.1:9002",
            test_connect_match_client,
        )
        .await
        .expect("reconnect_required should reconnect even when addr is unchanged");

        assert!(rebuilt);
        let state = shared_client_state(&client).await;
        assert_eq!(state.addr.as_deref(), Some("http://127.0.0.1:9002"));
        assert!(!state.reconnect_required);
    }

    #[tokio::test]
    async fn rediscovery_keeps_existing_client_until_registry_recovers_with_new_endpoint() {
        let client = create_match_client_shared();
        set_shared_client(&client, "http://127.0.0.1:9002", false).await;

        let missing_endpoint =
            resolve_discovery_outcome(DiscoveryOutcome::NotFound, "http://127.0.0.1:9002", true);
        assert!(missing_endpoint.is_err());
        let state = shared_client_state(&client).await;
        assert_eq!(state.addr.as_deref(), Some("http://127.0.0.1:9002"));
        assert!(!state.reconnect_required);

        let rebuilt = rebuild_match_client_if_needed_with_connector(
            &client,
            &test_config("http://127.0.0.1:9002"),
            "http://127.0.0.1:19002",
            test_connect_match_client,
        )
        .await
        .expect("client should reconnect after registry recovers");

        assert!(rebuilt);
        let state = shared_client_state(&client).await;
        assert_eq!(state.addr.as_deref(), Some("http://127.0.0.1:19002"));
    }

    #[test]
    fn rediscovery_only_enabled_with_registry() {
        let config = MatchClientConfig {
            addr: "http://127.0.0.1:9002".to_string(),
            fallback_addr: "http://127.0.0.1:9002".to_string(),
            local_discovery_fallback_enabled: true,
            registry_enabled: false,
            discovery_required: false,
            registry_url: "redis://127.0.0.1:6379".to_string(),
            registry_key_prefix: String::new(),
            service_name: "match-service".to_string(),
            rediscovery_interval_secs: DEFAULT_MATCH_REDISCOVERY_INTERVAL_SECS,
        };
        assert!(!config.rediscovery_enabled());

        let config = MatchClientConfig {
            registry_enabled: true,
            ..config
        };
        assert!(config.rediscovery_enabled());
    }

    #[test]
    fn discovery_outcome_uses_fallback_when_not_strict() {
        reset_discovery_metrics();
        let resolved = resolve_discovery_outcome(
            DiscoveryOutcome::Error("redis unavailable".to_string()),
            "http://127.0.0.1:9002",
            false,
        )
        .expect("non-strict discovery should fallback");

        assert_eq!(resolved.addr, "http://127.0.0.1:9002");
        assert_eq!(resolved.source, MatchServiceAddrSource::Fallback);
        assert!(get_discovery_metrics_snapshot().iter().any(|entry| {
            entry.kind == "fallback_used"
                && entry.service == "match-service"
                && entry.endpoint == "grpc"
                && entry.count == 1
        }));
    }

    #[test]
    fn discovery_outcome_returns_error_when_strict() {
        let error = resolve_discovery_outcome(
            DiscoveryOutcome::Error("redis unavailable".to_string()),
            "http://127.0.0.1:9002",
            true,
        )
        .expect_err("strict discovery should not fallback");

        assert_eq!(error, "redis unavailable");
    }

    #[test]
    fn discovery_outcome_returns_error_for_strict_not_found() {
        let error =
            resolve_discovery_outcome(DiscoveryOutcome::NotFound, "http://127.0.0.1:9002", true)
                .expect_err("strict discovery should not fallback");

        assert_eq!(error, "match-service grpc endpoint not found");
    }
}
