use std::sync::Arc;
use std::time::{Duration, Instant};

use interprocess::local_socket::traits::tokio::Stream as _;
use interprocess::local_socket::{GenericFilePath, ToFsName, tokio::Stream};
use prost::Message;
use service_registry::{RegistryClient, ServiceInstance};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::config::Config;
use crate::error::MatchError;
use crate::proto::myserver::game::{CreateMatchedRoomReq, CreateMatchedRoomRes, ErrorRes};

const HEADER_LEN: usize = 14;
const MAGIC: u16 = 0xCAFE;
const VERSION: u8 = 1;
const CREATE_MATCHED_ROOM_REQ: u16 = 1119;
const CREATE_MATCHED_ROOM_RES: u16 = 1120;
const INTERNAL_AUTH_REQ: u16 = 2199;
const ERROR_RES: u16 = 9000;

#[derive(Clone)]
pub struct GameServerClient {
    config: Config,
    discovery: Arc<GameServerDiscovery>,
}

impl GameServerClient {
    pub fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            discovery: Arc::new(GameServerDiscovery::new(config)),
        }
    }

    pub async fn create_matched_room(
        &self,
        match_id: &str,
        room_id: &str,
        player_ids: &[String],
        mode: &str,
    ) -> Result<String, MatchError> {
        let socket_name = self.resolve_internal_socket_name(match_id, mode).await?;
        let mut stream = connect_local_socket(&socket_name).await.map_err(|error| {
            MatchError::RoomCreateFailed(format!(
                "connect internal socket {socket_name} failed: {error}"
            ))
        })?;

        let auth_packet = encode_packet(
            INTERNAL_AUTH_REQ,
            0,
            self.config.game_internal_token.as_bytes(),
        );
        stream.write_all(&auth_packet).await.map_err(|error| {
            MatchError::RoomCreateFailed(format!("write InternalAuthReq failed: {error}"))
        })?;

        let request = CreateMatchedRoomReq {
            match_id: match_id.to_string(),
            room_id: room_id.to_string(),
            player_ids: player_ids.to_vec(),
            mode: mode.to_string(),
        };
        let body = encode_body(&request);
        let packet = encode_packet(CREATE_MATCHED_ROOM_REQ, 1, &body);

        stream.write_all(&packet).await.map_err(|error| {
            MatchError::RoomCreateFailed(format!("write CreateMatchedRoomReq failed: {error}"))
        })?;

        let response_packet = read_packet(&mut stream).await?;
        match response_packet.msg_type {
            CREATE_MATCHED_ROOM_RES => {
                let response = CreateMatchedRoomRes::decode(response_packet.body.as_slice())
                    .map_err(|error| {
                        MatchError::RoomCreateFailed(format!(
                            "decode CreateMatchedRoomRes failed: {error}"
                        ))
                    })?;

                if response.ok {
                    Ok(response.room_id)
                } else {
                    Err(MatchError::RoomCreateFailed(response.error_code))
                }
            }
            ERROR_RES => {
                let response =
                    ErrorRes::decode(response_packet.body.as_slice()).map_err(|error| {
                        MatchError::RoomCreateFailed(format!("decode ErrorRes failed: {error}"))
                    })?;
                Err(MatchError::RoomCreateFailed(response.error_code))
            }
            other => Err(MatchError::RoomCreateFailed(format!(
                "unexpected response message type: {other}"
            ))),
        }
    }

    async fn resolve_internal_socket_name(
        &self,
        match_id: &str,
        mode: &str,
    ) -> Result<String, MatchError> {
        let discovery_required = discovery_required();

        if !self.config.registry_enabled {
            if discovery_required {
                return Err(MatchError::RoomCreateFailed(
                    "required registry discovery failed: REGISTRY_ENABLED=false for game-server.internal"
                        .to_string(),
                ));
            }
            tracing::warn!(
                source = "fallback",
                socket = %self.config.game_server_internal_socket_name,
                "service registry disabled, using local game-server internal socket fallback"
            );
            return Ok(self.config.game_server_internal_socket_name.clone());
        }

        match self
            .discovery
            .resolve_socket(match_id, mode, discovery_required)
            .await
        {
            Ok(socket) => {
                tracing::info!(
                    source = "registry",
                    service = %self.config.game_server_service_name,
                    endpoint = "internal",
                    socket = %socket,
                    match_id = %match_id,
                    mode = %mode,
                    "game-server internal socket resolved"
                );
                Ok(socket)
            }
            Err(error) => {
                if discovery_required {
                    return Err(error);
                }
                tracing::warn!(
                    source = "fallback",
                    error = %error,
                    socket = %self.config.game_server_internal_socket_name,
                    "failed to discover game-server internal endpoint, using fallback"
                );
                Ok(self.config.game_server_internal_socket_name.clone())
            }
        }
    }
}

struct GameServerDiscovery {
    registry_url: String,
    registry_service_name: String,
    registry_instance_id: String,
    game_server_service_name: String,
    fallback_socket_name: String,
    cache_ttl: Duration,
    target_zone: String,
    registry_client: Mutex<Option<Arc<RegistryClient>>>,
    cache: Mutex<DiscoveryCache>,
}

impl GameServerDiscovery {
    fn new(config: &Config) -> Self {
        Self {
            registry_url: config.registry_url.clone(),
            registry_service_name: config.service_name.clone(),
            registry_instance_id: config.service_instance_id.clone(),
            game_server_service_name: config.game_server_service_name.clone(),
            fallback_socket_name: config.game_server_internal_socket_name.clone(),
            cache_ttl: Duration::from_secs(config.game_server_discovery_cache_ttl_secs.max(1)),
            target_zone: config.game_server_target_zone.clone(),
            registry_client: Mutex::new(None),
            cache: Mutex::new(DiscoveryCache::default()),
        }
    }

    async fn resolve_socket(
        &self,
        match_id: &str,
        mode: &str,
        discovery_required: bool,
    ) -> Result<String, MatchError> {
        let now = Instant::now();
        if let Some(candidates) = self.cache.lock().await.get(now) {
            return select_socket(&candidates, match_id, mode, &self.target_zone);
        }

        let client = self.registry_client().await?;
        let instances = client
            .discover(&self.game_server_service_name)
            .await
            .map_err(|error| {
                MatchError::RoomCreateFailed(format!(
                    "required registry discovery failed for {}.internal: {}",
                    self.game_server_service_name, error
                ))
            })?;
        let candidates = internal_socket_candidates(&instances);

        if candidates.is_empty() {
            return Err(MatchError::RoomCreateFailed(format!(
                "required registry discovery failed: {}.internal local_socket endpoint not found",
                self.game_server_service_name
            )));
        }

        self.cache
            .lock()
            .await
            .store(candidates.clone(), now, self.cache_ttl);

        match select_socket(&candidates, match_id, mode, &self.target_zone) {
            Ok(socket) => Ok(socket),
            Err(error) if !discovery_required => {
                tracing::warn!(
                    source = "fallback",
                    error = %error,
                    socket = %self.fallback_socket_name,
                    "game-server discovery selection failed, using local fallback"
                );
                Ok(self.fallback_socket_name.clone())
            }
            Err(error) => Err(error),
        }
    }

    async fn registry_client(&self) -> Result<Arc<RegistryClient>, MatchError> {
        let mut guard = self.registry_client.lock().await;
        if let Some(client) = guard.as_ref() {
            return Ok(client.clone());
        }

        let client = RegistryClient::new(
            &self.registry_url,
            &self.registry_service_name,
            &self.registry_instance_id,
        )
        .await
        .map_err(|error| {
            MatchError::RoomCreateFailed(format!(
                "required registry discovery failed: registry client unavailable for game-server.internal: {error}"
            ))
        })?;
        let client = Arc::new(client);
        *guard = Some(client.clone());
        Ok(client)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct GameServerEndpointCandidate {
    instance_id: String,
    socket: String,
    modes: Vec<String>,
    zone: Option<String>,
    weight: u32,
}

#[derive(Default)]
struct DiscoveryCache {
    candidates: Vec<GameServerEndpointCandidate>,
    expires_at: Option<Instant>,
}

impl DiscoveryCache {
    fn get(&self, now: Instant) -> Option<Vec<GameServerEndpointCandidate>> {
        self.expires_at
            .filter(|expires_at| *expires_at > now)
            .map(|_| self.candidates.clone())
            .filter(|candidates| !candidates.is_empty())
    }

    fn store(
        &mut self,
        mut candidates: Vec<GameServerEndpointCandidate>,
        now: Instant,
        ttl: Duration,
    ) {
        sort_candidates(&mut candidates);
        self.candidates = candidates;
        self.expires_at = Some(now + ttl);
    }
}

fn internal_socket_candidates(instances: &[ServiceInstance]) -> Vec<GameServerEndpointCandidate> {
    let mut candidates = instances
        .iter()
        .filter(|instance| instance.healthy && instance.weight > 0)
        .flat_map(|instance| {
            instance
                .endpoints
                .iter()
                .filter(|endpoint| {
                    endpoint.name == "internal"
                        && endpoint.healthy
                        && endpoint.protocol == "local_socket"
                        && !endpoint.socket.trim().is_empty()
                })
                .map(|endpoint| GameServerEndpointCandidate {
                    instance_id: instance.id.clone(),
                    socket: endpoint.socket.trim().to_string(),
                    modes: metadata_string_list(&endpoint.metadata, "modes")
                        .or_else(|| metadata_string_list(&endpoint.metadata, "match_modes"))
                        .or_else(|| metadata_string_list(&instance.metadata, "modes"))
                        .or_else(|| metadata_string_list(&instance.metadata, "match_modes"))
                        .unwrap_or_default(),
                    zone: metadata_string(&endpoint.metadata, "zone")
                        .or_else(|| metadata_string(&instance.metadata, "zone")),
                    weight: instance.weight,
                })
        })
        .collect::<Vec<_>>();
    sort_candidates(&mut candidates);
    candidates
}

fn select_socket(
    candidates: &[GameServerEndpointCandidate],
    match_id: &str,
    mode: &str,
    target_zone: &str,
) -> Result<String, MatchError> {
    let eligible = eligible_candidates(candidates, mode, target_zone);
    if eligible.is_empty() {
        return Err(MatchError::RoomCreateFailed(format!(
            "required registry discovery failed: no eligible game-server internal endpoint for mode={mode}"
        )));
    }

    let key = format!("{match_id}:{mode}");
    let index = stable_hash(&key) as usize % eligible.len();
    Ok(eligible[index].socket.clone())
}

fn eligible_candidates<'a>(
    candidates: &'a [GameServerEndpointCandidate],
    mode: &str,
    target_zone: &str,
) -> Vec<&'a GameServerEndpointCandidate> {
    let mut mode_filtered = if candidates.iter().any(|candidate| {
        candidate
            .modes
            .iter()
            .any(|candidate_mode| candidate_mode == mode)
    }) {
        candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .modes
                    .iter()
                    .any(|candidate_mode| candidate_mode == mode)
            })
            .collect::<Vec<_>>()
    } else {
        candidates.iter().collect::<Vec<_>>()
    };
    sort_candidate_refs(&mut mode_filtered);

    let target_zone = target_zone.trim();
    if target_zone.is_empty()
        || !mode_filtered
            .iter()
            .any(|candidate| candidate.zone.as_deref() == Some(target_zone))
    {
        return mode_filtered;
    }

    let mut zone_filtered = mode_filtered
        .into_iter()
        .filter(|candidate| candidate.zone.as_deref() == Some(target_zone))
        .collect::<Vec<_>>();
    sort_candidate_refs(&mut zone_filtered);
    zone_filtered
}

fn sort_candidates(candidates: &mut [GameServerEndpointCandidate]) {
    candidates.sort_by(|a, b| {
        a.instance_id
            .cmp(&b.instance_id)
            .then_with(|| a.socket.cmp(&b.socket))
            .then_with(|| b.weight.cmp(&a.weight))
    });
}

fn sort_candidate_refs(candidates: &mut [&GameServerEndpointCandidate]) {
    candidates.sort_by(|a, b| {
        a.instance_id
            .cmp(&b.instance_id)
            .then_with(|| a.socket.cmp(&b.socket))
            .then_with(|| b.weight.cmp(&a.weight))
    });
}

fn metadata_string(metadata: &serde_json::Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn metadata_string_list(metadata: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    let value = metadata.get(key)?;
    let mut values = match value {
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str())
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>(),
        serde_json::Value::String(raw) => raw
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    values.sort();
    values.dedup();
    (!values.is_empty()).then_some(values)
}

fn discovery_required() -> bool {
    env_flag("DISCOVERY_REQUIRED")
        || env_name_is("NODE_ENV", "production")
        || env_name_is("APP_ENV", "production")
        || env_name_is("NODE_ENV", "test")
        || env_name_is("APP_ENV", "test")
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "True"))
        .unwrap_or(false)
}

fn env_name_is(name: &str, expected: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
}

fn stable_hash(value: &str) -> u32 {
    let mut hash = 2_166_136_261_u32;
    for byte in value.as_bytes() {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(16_777_619);
    }
    hash
}

struct PacketFrame {
    msg_type: u16,
    body: Vec<u8>,
}

async fn connect_local_socket(name: &str) -> std::io::Result<Stream> {
    Stream::connect(to_name(name)?).await
}

fn to_name(name: &str) -> std::io::Result<interprocess::local_socket::Name<'_>> {
    normalize_name(name).to_fs_name::<GenericFilePath>()
}

fn normalize_name(name: &str) -> String {
    #[cfg(windows)]
    {
        if name.starts_with("\\\\.\\pipe\\") {
            return name.to_string();
        }

        return format!("\\\\.\\pipe\\{}", name.replace('/', "_").replace('\\', "_"));
    }

    #[cfg(not(windows))]
    {
        if name.starts_with('/') {
            return name.to_string();
        }

        format!("/tmp/{name}")
    }
}

fn encode_body<M: Message>(message: &M) -> Vec<u8> {
    let mut body = Vec::new();
    message.encode(&mut body).expect("protobuf encode failed");
    body
}

fn encode_packet(msg_type: u16, seq: u32, body: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(HEADER_LEN + body.len());

    packet.extend_from_slice(&MAGIC.to_be_bytes());
    packet.push(VERSION);
    packet.push(0);
    packet.extend_from_slice(&msg_type.to_be_bytes());
    packet.extend_from_slice(&seq.to_be_bytes());
    packet.extend_from_slice(&(body.len() as u32).to_be_bytes());
    packet.extend_from_slice(body);

    packet
}

async fn read_packet<R>(reader: &mut R) -> Result<PacketFrame, MatchError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header).await.map_err(|error| {
        MatchError::RoomCreateFailed(format!("read response header failed: {error}"))
    })?;

    let (msg_type, body_len) = parse_header(header)?;
    let mut body = vec![0u8; body_len];
    reader.read_exact(&mut body).await.map_err(|error| {
        MatchError::RoomCreateFailed(format!("read response body failed: {error}"))
    })?;

    Ok(PacketFrame { msg_type, body })
}

fn parse_header(header: [u8; HEADER_LEN]) -> Result<(u16, usize), MatchError> {
    let magic = u16::from_be_bytes([header[0], header[1]]);
    if magic != MAGIC {
        return Err(MatchError::RoomCreateFailed(
            "invalid response magic".to_string(),
        ));
    }

    if header[2] != VERSION {
        return Err(MatchError::RoomCreateFailed(
            "invalid response version".to_string(),
        ));
    }

    if header[3] != 0 {
        return Err(MatchError::RoomCreateFailed(
            "unsupported response flags".to_string(),
        ));
    }

    let msg_type = u16::from_be_bytes([header[4], header[5]]);
    let body_len = u32::from_be_bytes([header[10], header[11], header[12], header[13]]) as usize;
    Ok((msg_type, body_len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::{Mutex as StdMutex, OnceLock};

    fn candidate(
        instance_id: &str,
        socket: &str,
        modes: &[&str],
        zone: Option<&str>,
    ) -> GameServerEndpointCandidate {
        GameServerEndpointCandidate {
            instance_id: instance_id.to_string(),
            socket: socket.to_string(),
            modes: modes.iter().map(|mode| (*mode).to_string()).collect(),
            zone: zone.map(ToOwned::to_owned),
            weight: 100,
        }
    }

    fn env_lock() -> &'static StdMutex<()> {
        static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| StdMutex::new(()))
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

    fn test_config() -> Config {
        let mut config = Config::from_env();
        config.registry_enabled = false;
        config.registry_url = "redis://127.0.0.1:1".to_string();
        config.game_server_internal_socket_name = "fallback.sock".to_string();
        config.game_server_discovery_cache_ttl_secs = 1;
        config.game_server_target_zone = String::new();
        config
    }

    #[test]
    fn discovery_cache_returns_hit_until_ttl_expires() {
        let now = Instant::now();
        let mut cache = DiscoveryCache::default();
        let candidates = vec![candidate("game-a", "a.sock", &[], None)];

        cache.store(candidates.clone(), now, Duration::from_secs(5));

        assert_eq!(cache.get(now + Duration::from_secs(4)), Some(candidates));
        assert!(cache.get(now + Duration::from_secs(5)).is_none());
    }

    #[test]
    fn stable_selection_is_deterministic_for_same_candidate_set() {
        let candidates = vec![
            candidate("game-c", "c.sock", &[], None),
            candidate("game-a", "a.sock", &[], None),
            candidate("game-b", "b.sock", &[], None),
        ];
        let reversed = candidates.iter().cloned().rev().collect::<Vec<_>>();

        let first = select_socket(&candidates, "match-123", "5v5", "").unwrap();
        let second = select_socket(&reversed, "match-123", "5v5", "").unwrap();

        assert_eq!(first, second);
    }

    #[test]
    fn selection_filters_by_mode_and_zone_when_metadata_is_available() {
        let candidates = vec![
            candidate("game-a", "a.sock", &["1v1"], Some("zone-a")),
            candidate("game-b", "b.sock", &["5v5"], Some("zone-a")),
            candidate("game-c", "c.sock", &["5v5"], Some("zone-b")),
        ];

        let socket = select_socket(&candidates, "match-123", "5v5", "zone-b").unwrap();

        assert_eq!(socket, "c.sock");
    }

    #[tokio::test]
    async fn strict_discovery_disables_registry_disabled_fallback() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&["DISCOVERY_REQUIRED", "NODE_ENV", "APP_ENV"]);
        unsafe {
            env::set_var("DISCOVERY_REQUIRED", "true");
            env::remove_var("NODE_ENV");
            env::remove_var("APP_ENV");
        }
        let client = GameServerClient::new(&test_config());

        let error = client
            .resolve_internal_socket_name("match-1", "1v1")
            .await
            .expect_err("strict discovery should reject local fallback")
            .to_string();

        assert!(error.contains("REGISTRY_ENABLED=false"));
    }

    #[tokio::test]
    async fn registry_disabled_local_mode_uses_fallback_socket() {
        let _guard = env_lock().lock().unwrap();
        let _env = EnvGuard::capture(&["DISCOVERY_REQUIRED", "NODE_ENV", "APP_ENV"]);
        unsafe {
            env::remove_var("DISCOVERY_REQUIRED");
            env::remove_var("NODE_ENV");
            env::remove_var("APP_ENV");
        }
        let client = GameServerClient::new(&test_config());

        let socket = client
            .resolve_internal_socket_name("match-1", "1v1")
            .await
            .unwrap();

        assert_eq!(socket, "fallback.sock");
    }
}
