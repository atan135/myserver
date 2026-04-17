use interprocess::local_socket::{
    GenericFilePath, ToFsName,
    tokio::Stream,
};
use interprocess::local_socket::traits::tokio::Stream as _;
use prost::Message;
use service_registry::RegistryClient;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::config::Config;
use crate::error::MatchError;
use crate::proto::myserver::game::{CreateMatchedRoomReq, CreateMatchedRoomRes, ErrorRes};

const HEADER_LEN: usize = 14;
const MAGIC: u16 = 0xCAFE;
const VERSION: u8 = 1;
const CREATE_MATCHED_ROOM_REQ: u16 = 1119;
const CREATE_MATCHED_ROOM_RES: u16 = 1120;
const ERROR_RES: u16 = 9000;

#[derive(Clone)]
pub struct GameServerClient {
    config: Config,
}

impl GameServerClient {
    pub fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
        }
    }

    pub async fn create_matched_room(
        &self,
        match_id: &str,
        room_id: &str,
        player_ids: &[String],
        mode: &str,
    ) -> Result<String, MatchError> {
        let socket_name = self.resolve_internal_socket_name().await?;
        let mut stream = connect_local_socket(&socket_name)
            .await
            .map_err(|error| {
                MatchError::RoomCreateFailed(format!(
                    "connect internal socket {socket_name} failed: {error}"
                ))
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
                let response = ErrorRes::decode(response_packet.body.as_slice()).map_err(|error| {
                    MatchError::RoomCreateFailed(format!("decode ErrorRes failed: {error}"))
                })?;
                Err(MatchError::RoomCreateFailed(response.error_code))
            }
            other => Err(MatchError::RoomCreateFailed(format!(
                "unexpected response message type: {other}"
            ))),
        }
    }

    async fn resolve_internal_socket_name(&self) -> Result<String, MatchError> {
        if !self.config.registry_enabled {
            return Ok(self.config.game_server_internal_socket_name.clone());
        }

        let client = RegistryClient::new(
            &self.config.registry_url,
            &self.config.service_name,
            &self.config.service_instance_id,
        )
        .await;

        let Ok(client) = client else {
            return Ok(self.config.game_server_internal_socket_name.clone());
        };

        match client.discover_one(&self.config.game_server_service_name).await {
            Ok(Some(instance)) => {
                if let Some(internal_socket) = instance
                    .metadata
                    .get("internal_socket")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                {
                    return Ok(internal_socket.to_string());
                }

                if !instance.local_socket.is_empty() {
                    return Ok(derive_internal_socket_name(&instance.local_socket));
                }

                Ok(self.config.game_server_internal_socket_name.clone())
            }
            Ok(None) => Ok(self.config.game_server_internal_socket_name.clone()),
            Err(_) => Ok(self.config.game_server_internal_socket_name.clone()),
        }
    }
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
        return Err(MatchError::RoomCreateFailed("invalid response magic".to_string()));
    }

    if header[2] != VERSION {
        return Err(MatchError::RoomCreateFailed("invalid response version".to_string()));
    }

    if header[3] != 0 {
        return Err(MatchError::RoomCreateFailed("unsupported response flags".to_string()));
    }

    let msg_type = u16::from_be_bytes([header[4], header[5]]);
    let body_len = u32::from_be_bytes([header[10], header[11], header[12], header[13]]) as usize;
    Ok((msg_type, body_len))
}

fn derive_internal_socket_name(local_socket_name: &str) -> String {
    if let Some(prefix) = local_socket_name.strip_suffix(".sock") {
        return format!("{prefix}-internal.sock");
    }

    format!("{local_socket_name}-internal")
}
