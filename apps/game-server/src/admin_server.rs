use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use crate::admin_pb::{
    GrantItem, GrantItemsReq, GrantItemsRes, ServerStatusReq, ServerStatusRes, UpdateConfigReq,
    UpdateConfigRes,
};
use crate::core::config_table::ConfigTableRuntime;
use crate::core::context::{PlayerRegistry, SharedRoomManager, SharedRuntimeConfig};
use crate::core::inventory::Item;
use crate::core::player::PlayerManager;
use crate::core::runtime::room_manager::ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT;
use crate::gm_broadcast::{
    GM_BROADCAST_CONTENT_MAX_LEN, GM_BROADCAST_TITLE_MAX_LEN, GM_SENDER_MAX_LEN,
    GmBroadcastCommand, broadcast_gm_message_to_online_players, normalize_optional_string,
    normalize_required_string,
};
use crate::pb::{ErrorRes, InventoryUpdatePush, Item as PbItem, ItemObtainPush};
use crate::pb::{
    ExportRoomTransferReq, ExportRoomTransferRes, FreezeRoomForTransferReq,
    FreezeRoomForTransferRes, GetRolloutDrainStatusReq, GetRolloutDrainStatusRes,
    ImportRoomTransferReq, ImportRoomTransferRes, RetireTransferredRoomReq,
    RetireTransferredRoomRes, ServerRedirectPush, TriggerServerRedirectReq,
    TriggerServerRedirectRes,
};
use crate::protocol::{HEADER_LEN, MessageType, Packet, encode_body, encode_packet, parse_header};
use crate::server::RuntimeConfig;

const ADMIN_MAX_BODY_LEN: usize = 64 * 1024;
const GM_REASON_MAX_LEN: usize = 512;
const GM_PLAYER_ID_MAX_LEN: usize = 128;
const GM_BAN_DURATION_MAX_SECONDS: u64 = 31_536_000;
const MAX_ADMIN_ACTOR_LEN: usize = 128;
const DEFAULT_ADMIN_ACTOR: &str = "unknown";
static ITEM_UID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, PartialEq, prost::Message)]
struct GmCommandRes {
    #[prost(bool, tag = "1")]
    ok: bool,
    #[prost(string, tag = "2")]
    error_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GmKickPlayerCommand {
    player_id: String,
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GmBanPlayerCommand {
    player_id: String,
    duration_seconds: u64,
    reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KickOnlineOutcome {
    player_id: String,
    session_id: u64,
}

#[derive(Clone)]
pub struct AdminAuditConfig {
    enabled: bool,
    path: PathBuf,
    require_actor: bool,
}

impl AdminAuditConfig {
    pub fn new(enabled: bool, path: impl Into<PathBuf>, require_actor: bool) -> Self {
        Self {
            enabled,
            path: path.into(),
            require_actor,
        }
    }
}

#[derive(Clone)]
pub struct AdminAuditLogger {
    config: AdminAuditConfig,
}

impl AdminAuditLogger {
    pub fn new(config: AdminAuditConfig) -> Self {
        Self { config }
    }

    async fn ensure_ready(&self) -> Result<(), AdminAuditError> {
        if !self.config.enabled {
            return Ok(());
        }

        ensure_parent_dir(&self.config.path).await?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.path)
            .await
            .map_err(AdminAuditError::Io)?;
        Ok(())
    }

    async fn append(&self, event: &AdminAuditEvent<'_>) -> Result<(), AdminAuditError> {
        if !self.config.enabled {
            return Ok(());
        }

        ensure_parent_dir(&self.config.path).await?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.path)
            .await
            .map_err(AdminAuditError::Io)?;
        let mut line = serde_json::to_string(event).map_err(AdminAuditError::Serialize)?;
        line.push('\n');
        file.write_all(line.as_bytes())
            .await
            .map_err(AdminAuditError::Io)
    }
}

#[derive(Debug)]
enum AdminAuditError {
    Io(std::io::Error),
    Serialize(serde_json::Error),
}

impl std::fmt::Display for AdminAuditError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{}", error),
            Self::Serialize(error) => write!(formatter, "{}", error),
        }
    }
}

impl std::error::Error for AdminAuditError {}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AdminAuthContext {
    actor: String,
    actor_missing: bool,
}

#[derive(Deserialize)]
struct AdminAuthEnvelope {
    token: String,
    actor: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AdminAuditTarget {
    room_id: String,
    player_id: String,
    rollout_epoch: String,
    checksum: String,
    target_server_id: String,
    config_key: String,
}

#[derive(Serialize)]
struct AdminAuditEvent<'a> {
    timestamp_ms: u64,
    channel: &'static str,
    action: &'a str,
    actor: &'a str,
    actor_missing: bool,
    ok: bool,
    error_code: &'a str,
    room_id: &'a str,
    player_id: &'a str,
    rollout_epoch: &'a str,
    checksum: &'a str,
    target_server_id: &'a str,
    config_key: &'a str,
    seq: u32,
    message_type: u16,
}

#[derive(Debug, PartialEq, Eq)]
enum AdminWritePreflightError {
    ActorRequired,
    AuditUnavailable,
}

impl AdminWritePreflightError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::ActorRequired => "ADMIN_ACTOR_REQUIRED",
            Self::AuditUnavailable => "ADMIN_AUDIT_WRITE_FAILED",
        }
    }

    fn message(&self) -> &'static str {
        match self {
            Self::ActorRequired => "admin actor is required for write operations",
            Self::AuditUnavailable => "admin audit log is not writable",
        }
    }
}

pub async fn run_listener(
    listener: TcpListener,
    room_manager: SharedRoomManager,
    runtime_config: SharedRuntimeConfig,
    connection_count: Arc<AtomicU64>,
    player_registry: PlayerRegistry,
    player_manager: PlayerManager,
    config_tables: ConfigTableRuntime,
    owner_server_id: String,
    admin_token: String,
    audit_logger: AdminAuditLogger,
) -> Result<(), std::io::Error> {
    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let room_manager = room_manager.clone();
        let runtime_config = runtime_config.clone();
        let connection_count = connection_count.clone();
        let player_registry = player_registry.clone();
        let player_manager = player_manager.clone();
        let config_tables = config_tables.clone();
        let owner_server_id = owner_server_id.clone();
        let admin_token = admin_token.clone();
        let audit_logger = audit_logger.clone();

        tokio::spawn(async move {
            if let Err(error) = handle_admin_connection(
                socket,
                room_manager,
                runtime_config,
                connection_count,
                player_registry,
                player_manager,
                config_tables,
                owner_server_id,
                admin_token,
                audit_logger,
            )
            .await
            {
                warn!(peer = %peer_addr, error = %error, "admin connection failed");
            }
        });
    }
}

async fn handle_admin_connection(
    socket: TcpStream,
    room_manager: SharedRoomManager,
    runtime_config: SharedRuntimeConfig,
    connection_count: Arc<AtomicU64>,
    player_registry: PlayerRegistry,
    player_manager: PlayerManager,
    config_tables: ConfigTableRuntime,
    owner_server_id: String,
    admin_token: String,
    audit_logger: AdminAuditLogger,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut reader, mut writer) = socket.into_split();

    let Some(auth_packet) = read_packet(&mut reader).await? else {
        return Ok(());
    };
    let Some(auth_context) = authenticate_admin_packet(&auth_packet, &admin_token) else {
        write_error(
            &mut writer,
            auth_packet.header.seq,
            "UNAUTHORIZED_ADMIN",
            "invalid admin token",
        )
        .await?;
        return Ok(());
    };

    loop {
        let Some(packet) = read_packet(&mut reader).await? else {
            break;
        };

        if let Some(action) = packet.message_type().and_then(admin_write_action) {
            if let Err(error) =
                ensure_admin_write_allowed(&audit_logger, &auth_context, &packet, action).await
            {
                write_error(
                    &mut writer,
                    packet.header.seq,
                    error.error_code(),
                    error.message(),
                )
                .await?;
                continue;
            }
        }

        match packet.message_type() {
            Some(MessageType::AdminServerStatusReq) => {
                packet
                    .decode_body::<ServerStatusReq>("INVALID_ADMIN_STATUS_BODY")
                    .map_err(std::io::Error::other)?;

                let room_count = room_manager.room_count().await as u64;
                let runtime = *runtime_config.read().await;
                let RuntimeConfig {
                    heartbeat_timeout_secs,
                    max_body_len,
                    ..
                } = runtime;

                write_message(
                    &mut writer,
                    MessageType::AdminServerStatusRes,
                    packet.header.seq,
                    &ServerStatusRes {
                        connection_count: connection_count.load(Ordering::Relaxed),
                        room_count,
                        status: runtime.status_label().to_string(),
                        max_body_len: max_body_len as u64,
                        heartbeat_timeout_secs,
                    },
                )
                .await?;
            }
            Some(MessageType::AdminUpdateConfigReq) => {
                let action = "admin_update_config";
                let request = match packet
                    .decode_body::<UpdateConfigReq>("INVALID_ADMIN_UPDATE_CONFIG_BODY")
                {
                    Ok(request) => request,
                    Err(error_code) => {
                        audit_then_write_error(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            error_code,
                            "invalid admin update config request",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                        continue;
                    }
                };
                let target = update_config_target(&request);
                let result =
                    apply_runtime_config(&runtime_config, &request.key, &request.value).await;
                let ok = result.is_ok();
                let error_code = result.err().unwrap_or_default().to_string();

                audit_then_write_message(
                    &mut writer,
                    &audit_logger,
                    &auth_context,
                    &packet,
                    action,
                    MessageType::AdminUpdateConfigRes,
                    &UpdateConfigRes {
                        ok,
                        error_code: error_code.clone(),
                    },
                    ok,
                    &error_code,
                    &target,
                )
                .await?;
            }
            Some(MessageType::GmSendItemReq) => {
                let action = "gm_send_item";
                let request =
                    match decode_grant_items_request(&packet).map_err(|error| error.to_string()) {
                        Ok(request) => request,
                        Err(error_code) => {
                            audit_then_write_error(
                                &mut writer,
                                &audit_logger,
                                &auth_context,
                                &packet,
                                action,
                                &error_code,
                                "invalid grant items request",
                                &AdminAuditTarget::default(),
                            )
                            .await?;
                            continue;
                        }
                    };
                let target = player_target(request.player_id.clone());
                if let Err(error_code) = validate_grant_items_request(&request, &config_tables)
                    .await
                    .map_err(|error| error.to_string())
                {
                    audit_then_write_error(
                        &mut writer,
                        &audit_logger,
                        &auth_context,
                        &packet,
                        action,
                        &error_code,
                        "invalid grant items request",
                        &target,
                    )
                    .await?;
                    continue;
                }
                let items = request
                    .items
                    .iter()
                    .map(grant_item_to_inventory_item)
                    .collect::<Vec<_>>();

                let result = player_manager
                    .grant_items_with_request(
                        &request.player_id,
                        &items,
                        &request.request_id,
                        &request.source,
                        &request.reason,
                    )
                    .await;

                match result {
                    Ok(outcome) => {
                        if outcome.applied {
                            let _ = push_item_obtain_if_online(
                                &room_manager,
                                &request.player_id,
                                &items,
                                &request.source,
                            )
                            .await;
                            let _ = push_inventory_update_if_online(
                                &room_manager,
                                &request.player_id,
                                &outcome.player_data,
                            )
                            .await;
                        }

                        audit_then_write_message(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            MessageType::GmSendItemRes,
                            &GrantItemsRes {
                                ok: true,
                                error_code: String::new(),
                                applied: outcome.applied,
                            },
                            true,
                            "",
                            &target,
                        )
                        .await?;
                    }
                    Err(error_code) => {
                        audit_then_write_error(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            &error_code,
                            "failed to grant items",
                            &target,
                        )
                        .await?;
                    }
                }
            }
            Some(MessageType::GmBroadcastReq) => {
                let action = "gm_broadcast";
                match decode_gm_broadcast_request(&packet) {
                    Ok(request) => {
                        let delivered =
                            broadcast_gm_message_to_online_players(&player_registry, &request)
                                .await;
                        info!(
                            delivered = delivered,
                            sender = %request.sender,
                            title = %request.title,
                            "gm broadcast delivered"
                        );
                        audit_then_write_message(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            MessageType::GmBroadcastRes,
                            &GmCommandRes {
                                ok: true,
                                error_code: String::new(),
                            },
                            true,
                            "",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                    }
                    Err(error_code) => {
                        audit_then_write_error(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            error_code,
                            "invalid gm broadcast request",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                    }
                }
            }
            Some(MessageType::GmKickPlayerReq) => {
                let action = "gm_kick_player";
                match decode_gm_kick_player_request(&packet) {
                    Ok(request) => {
                        let target = player_target(request.player_id.clone());
                        let kick_reason = gm_disconnect_reason("gm_kick", &request.reason);
                        match kick_online_player(&player_registry, &request.player_id, &kick_reason)
                            .await
                        {
                            Ok(outcome) => {
                                info!(
                                    player_id = %outcome.player_id,
                                    session_id = outcome.session_id,
                                    reason = %kick_reason,
                                    "gm kick player delivered"
                                );
                                audit_then_write_message(
                                    &mut writer,
                                    &audit_logger,
                                    &auth_context,
                                    &packet,
                                    action,
                                    MessageType::GmKickPlayerRes,
                                    &GmCommandRes {
                                        ok: true,
                                        error_code: String::new(),
                                    },
                                    true,
                                    "",
                                    &target,
                                )
                                .await?;
                            }
                            Err(error_code) => {
                                audit_then_write_error(
                                    &mut writer,
                                    &audit_logger,
                                    &auth_context,
                                    &packet,
                                    action,
                                    error_code,
                                    "failed to kick player",
                                    &target,
                                )
                                .await?;
                            }
                        }
                    }
                    Err(error_code) => {
                        audit_then_write_error(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            error_code,
                            "invalid gm kick player request",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                    }
                }
            }
            Some(MessageType::GmBanPlayerReq) => {
                let action = "gm_ban_player";
                match decode_gm_ban_player_request(&packet) {
                    Ok(request) => {
                        let target = player_target(request.player_id.clone());
                        let kick_reason = gm_disconnect_reason("gm_ban", &request.reason);
                        match kick_online_player(&player_registry, &request.player_id, &kick_reason)
                            .await
                        {
                            Ok(outcome) => {
                                info!(
                                    player_id = %outcome.player_id,
                                    session_id = outcome.session_id,
                                    duration_seconds = request.duration_seconds,
                                    reason = %kick_reason,
                                    "gm ban online player handled"
                                );
                                audit_then_write_message(
                                    &mut writer,
                                    &audit_logger,
                                    &auth_context,
                                    &packet,
                                    action,
                                    MessageType::GmBanPlayerRes,
                                    &GmCommandRes {
                                        ok: true,
                                        error_code: String::new(),
                                    },
                                    true,
                                    "",
                                    &target,
                                )
                                .await?;
                            }
                            Err(error_code) => {
                                audit_then_write_error(
                                    &mut writer,
                                    &audit_logger,
                                    &auth_context,
                                    &packet,
                                    action,
                                    error_code,
                                    "failed to ban player on this game-server",
                                    &target,
                                )
                                .await?;
                            }
                        }
                    }
                    Err(error_code) => {
                        audit_then_write_error(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            error_code,
                            "invalid gm ban player request",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                    }
                }
            }
            Some(MessageType::FreezeRoomForTransferReq) => {
                let action = "freeze_room_for_transfer";
                let request = match packet
                    .decode_body::<FreezeRoomForTransferReq>("INVALID_FREEZE_ROOM_TRANSFER_BODY")
                {
                    Ok(request) => request,
                    Err(error_code) => {
                        audit_then_write_error(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            error_code,
                            "invalid freeze room transfer request",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                        continue;
                    }
                };
                let target =
                    room_transfer_target(request.room_id.clone(), request.rollout_epoch.clone());

                let result = room_manager
                    .freeze_room_for_transfer(&request.rollout_epoch, &request.room_id)
                    .await;

                let (ok, error_code, migration_state, room_version) = match result {
                    Ok((migration_state, room_version)) => {
                        (true, String::new(), migration_state as i32, room_version)
                    }
                    Err(error_code) => (false, error_code.to_string(), 0, 0),
                };

                audit_then_write_message(
                    &mut writer,
                    &audit_logger,
                    &auth_context,
                    &packet,
                    action,
                    MessageType::FreezeRoomForTransferRes,
                    &FreezeRoomForTransferRes {
                        ok,
                        room_id: request.room_id,
                        error_code: error_code.clone(),
                        migration_state,
                        room_version,
                    },
                    ok,
                    &error_code,
                    &target,
                )
                .await?;
            }
            Some(MessageType::ExportRoomTransferReq) => {
                let action = "export_room_transfer";
                let request = match packet
                    .decode_body::<ExportRoomTransferReq>("INVALID_EXPORT_ROOM_TRANSFER_BODY")
                {
                    Ok(request) => request,
                    Err(error_code) => {
                        audit_then_write_error(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            error_code,
                            "invalid export room transfer request",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                        continue;
                    }
                };
                let mut target =
                    room_transfer_target(request.room_id.clone(), request.rollout_epoch.clone());

                let result = room_manager
                    .export_room_transfer(&request.rollout_epoch, &request.room_id)
                    .await;

                let response = match result {
                    Ok(payload) => {
                        target.checksum = payload.checksum.clone();
                        ExportRoomTransferRes {
                            ok: true,
                            room_id: request.room_id,
                            error_code: String::new(),
                            checksum: payload.checksum.clone(),
                            payload: Some(payload),
                        }
                    }
                    Err(error_code) => ExportRoomTransferRes {
                        ok: false,
                        room_id: request.room_id,
                        error_code: error_code.to_string(),
                        checksum: String::new(),
                        payload: None,
                    },
                };

                let ok = response.ok;
                let error_code = response.error_code.clone();
                audit_then_write_message(
                    &mut writer,
                    &audit_logger,
                    &auth_context,
                    &packet,
                    action,
                    MessageType::ExportRoomTransferRes,
                    &response,
                    ok,
                    &error_code,
                    &target,
                )
                .await?;
            }
            Some(MessageType::ImportRoomTransferReq) => {
                let action = "import_room_transfer";
                let request = match packet
                    .decode_body::<ImportRoomTransferReq>("INVALID_IMPORT_ROOM_TRANSFER_BODY")
                {
                    Ok(request) => request,
                    Err(error_code) => {
                        audit_then_write_error(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            error_code,
                            "invalid import room transfer request",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                        continue;
                    }
                };
                let mut target = import_room_transfer_target(&request);

                let result = match request.payload {
                    Some(payload) => {
                        let room_id = payload.room_id.clone();
                        room_manager
                            .import_room_transfer(payload)
                            .await
                            .map(|(checksum, room_version)| (room_id, checksum, room_version))
                    }
                    None => Err("ROOM_TRANSFER_MISSING_PAYLOAD"),
                };

                let response = match result {
                    Ok((room_id, checksum, room_version)) => {
                        target.room_id = room_id.clone();
                        target.checksum = checksum.clone();
                        ImportRoomTransferRes {
                            ok: true,
                            room_id,
                            error_code: String::new(),
                            checksum,
                            room_version,
                        }
                    }
                    Err(error_code) => ImportRoomTransferRes {
                        ok: false,
                        room_id: String::new(),
                        error_code: error_code.to_string(),
                        checksum: String::new(),
                        room_version: 0,
                    },
                };

                let ok = response.ok;
                let error_code = response.error_code.clone();
                audit_then_write_message(
                    &mut writer,
                    &audit_logger,
                    &auth_context,
                    &packet,
                    action,
                    MessageType::ImportRoomTransferRes,
                    &response,
                    ok,
                    &error_code,
                    &target,
                )
                .await?;
            }
            Some(MessageType::RetireTransferredRoomReq) => {
                let action = "retire_transferred_room";
                let request = match packet
                    .decode_body::<RetireTransferredRoomReq>("INVALID_RETIRE_ROOM_TRANSFER_BODY")
                {
                    Ok(request) => request,
                    Err(error_code) => {
                        audit_then_write_error(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            error_code,
                            "invalid retire room transfer request",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                        continue;
                    }
                };
                let target = retire_room_transfer_target(&request);

                let result = room_manager
                    .retire_transferred_room(
                        &request.rollout_epoch,
                        &request.room_id,
                        &request.checksum,
                    )
                    .await;
                let ok = result.is_ok();
                let error_code = result.err().unwrap_or_default().to_string();

                audit_then_write_message(
                    &mut writer,
                    &audit_logger,
                    &auth_context,
                    &packet,
                    action,
                    MessageType::RetireTransferredRoomRes,
                    &RetireTransferredRoomRes {
                        ok,
                        room_id: request.room_id,
                        error_code: error_code.clone(),
                    },
                    ok,
                    &error_code,
                    &target,
                )
                .await?;
            }
            Some(MessageType::GetRolloutDrainStatusReq) => {
                packet
                    .decode_body::<GetRolloutDrainStatusReq>(
                        "INVALID_GET_ROLLOUT_DRAIN_STATUS_BODY",
                    )
                    .map_err(std::io::Error::other)?;

                write_message(
                    &mut writer,
                    MessageType::GetRolloutDrainStatusRes,
                    packet.header.seq,
                    &build_rollout_drain_status_response(
                        &room_manager,
                        &runtime_config,
                        &owner_server_id,
                        &connection_count,
                    )
                    .await,
                )
                .await?;
            }
            Some(MessageType::TriggerServerRedirectReq) => {
                let action = "trigger_server_redirect";
                let request = match packet
                    .decode_body::<TriggerServerRedirectReq>("INVALID_TRIGGER_REDIRECT_BODY")
                {
                    Ok(request) => request,
                    Err(error_code) => {
                        audit_then_write_error(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            error_code,
                            "invalid trigger redirect request",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                        continue;
                    }
                };
                let target = redirect_target(&request);

                let room_id = request.room_id.clone();
                let result = room_manager
                    .trigger_server_redirect(
                        &room_id,
                        ServerRedirectPush {
                            reason: request.reason,
                            room_id: room_id.clone(),
                            rollout_epoch: request.rollout_epoch,
                            reconnect_required: true,
                            retry_after_ms: request.retry_after_ms,
                            target_host: request.target_host,
                            target_port: request.target_port,
                            target_server_id: request.target_server_id,
                            transport: if request.transport.trim().is_empty() {
                                "kcp".to_string()
                            } else {
                                request.transport
                            },
                        },
                    )
                    .await;

                let response = match result {
                    Ok(delivery) => TriggerServerRedirectRes {
                        ok: true,
                        room_id,
                        error_code: String::new(),
                        delivered_count: delivery.delivered_count,
                        failed_count: delivery.failed_count,
                        online_member_count: delivery.online_member_count,
                    },
                    Err(error_code) => TriggerServerRedirectRes {
                        ok: false,
                        room_id,
                        error_code: error_code.to_string(),
                        delivered_count: 0,
                        failed_count: 0,
                        online_member_count: 0,
                    },
                };

                let ok = response.ok;
                let error_code = response.error_code.clone();
                audit_then_write_message(
                    &mut writer,
                    &audit_logger,
                    &auth_context,
                    &packet,
                    action,
                    MessageType::TriggerServerRedirectRes,
                    &response,
                    ok,
                    &error_code,
                    &target,
                )
                .await?;
            }
            Some(_) => {
                write_error(
                    &mut writer,
                    packet.header.seq,
                    "MESSAGE_NOT_SUPPORTED",
                    "message not supported on admin channel",
                )
                .await?;
            }
            None => {
                write_error(
                    &mut writer,
                    packet.header.seq,
                    "UNKNOWN_MESSAGE_TYPE",
                    "unknown message type",
                )
                .await?;
            }
        }
    }

    Ok(())
}

fn authenticate_admin_packet(packet: &Packet, admin_token: &str) -> Option<AdminAuthContext> {
    if packet.message_type() != Some(MessageType::AdminAuthReq) {
        return None;
    }

    let body = std::str::from_utf8(&packet.body).ok()?;
    if body == admin_token {
        return Some(AdminAuthContext {
            actor: DEFAULT_ADMIN_ACTOR.to_string(),
            actor_missing: true,
        });
    }

    let envelope: AdminAuthEnvelope = serde_json::from_str(body).ok()?;
    if envelope.token != admin_token {
        return None;
    }

    Some(normalize_admin_auth_context(envelope.actor))
}

fn normalize_admin_auth_context(actor: Option<String>) -> AdminAuthContext {
    let Some(actor) = actor
        .as_deref()
        .map(str::trim)
        .and_then(normalize_admin_actor)
    else {
        return AdminAuthContext {
            actor: DEFAULT_ADMIN_ACTOR.to_string(),
            actor_missing: true,
        };
    };

    AdminAuthContext {
        actor,
        actor_missing: false,
    }
}

fn normalize_admin_actor(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > MAX_ADMIN_ACTOR_LEN {
        return None;
    }

    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'))
        .then(|| value.to_string())
}

async fn ensure_admin_write_allowed(
    audit_logger: &AdminAuditLogger,
    context: &AdminAuthContext,
    packet: &Packet,
    action: &'static str,
) -> Result<(), AdminWritePreflightError> {
    if let Err(error) = audit_logger.ensure_ready().await {
        warn!(
            action,
            seq = packet.header.seq,
            message_type = packet.header.msg_type,
            error = %error,
            audit_path = %audit_logger.config.path.display(),
            "game-server admin audit log is not writable"
        );
        return Err(AdminWritePreflightError::AuditUnavailable);
    }

    if audit_logger.config.require_actor && context.actor_missing {
        match audit_admin_write_result(
            audit_logger,
            context,
            packet,
            action,
            false,
            "ADMIN_ACTOR_REQUIRED",
            &AdminAuditTarget::default(),
        )
        .await
        {
            Ok(()) => Err(AdminWritePreflightError::ActorRequired),
            Err(error) => {
                warn!(
                    action,
                    seq = packet.header.seq,
                    message_type = packet.header.msg_type,
                    error = %error,
                    audit_path = %audit_logger.config.path.display(),
                    "game-server admin actor rejection audit write failed"
                );
                Err(AdminWritePreflightError::AuditUnavailable)
            }
        }
    } else {
        Ok(())
    }
}

fn admin_write_action(message_type: MessageType) -> Option<&'static str> {
    match message_type {
        MessageType::AdminUpdateConfigReq => Some("admin_update_config"),
        MessageType::GmSendItemReq => Some("gm_send_item"),
        MessageType::GmBroadcastReq => Some("gm_broadcast"),
        MessageType::GmKickPlayerReq => Some("gm_kick_player"),
        MessageType::GmBanPlayerReq => Some("gm_ban_player"),
        MessageType::FreezeRoomForTransferReq => Some("freeze_room_for_transfer"),
        MessageType::ExportRoomTransferReq => Some("export_room_transfer"),
        MessageType::ImportRoomTransferReq => Some("import_room_transfer"),
        MessageType::RetireTransferredRoomReq => Some("retire_transferred_room"),
        MessageType::TriggerServerRedirectReq => Some("trigger_server_redirect"),
        _ => None,
    }
}

async fn audit_admin_write_result(
    audit_logger: &AdminAuditLogger,
    context: &AdminAuthContext,
    packet: &Packet,
    action: &'static str,
    ok: bool,
    error_code: &str,
    target: &AdminAuditTarget,
) -> Result<(), AdminAuditError> {
    if ok {
        info!(
            channel = "admin_tcp",
            action,
            actor = %context.actor,
            actor_missing = context.actor_missing,
            ok,
            error_code,
            room_id = %target.room_id,
            player_id = %target.player_id,
            rollout_epoch = %target.rollout_epoch,
            checksum = %target.checksum,
            target_server_id = %target.target_server_id,
            config_key = %target.config_key,
            seq = packet.header.seq,
            message_type = packet.header.msg_type,
            "game-server admin write operation"
        );
    } else {
        warn!(
            channel = "admin_tcp",
            action,
            actor = %context.actor,
            actor_missing = context.actor_missing,
            ok,
            error_code,
            room_id = %target.room_id,
            player_id = %target.player_id,
            rollout_epoch = %target.rollout_epoch,
            checksum = %target.checksum,
            target_server_id = %target.target_server_id,
            config_key = %target.config_key,
            seq = packet.header.seq,
            message_type = packet.header.msg_type,
            "game-server admin write operation failed"
        );
    }

    audit_logger
        .append(&AdminAuditEvent {
            timestamp_ms: current_unix_ms_u64(),
            channel: "admin_tcp",
            action,
            actor: &context.actor,
            actor_missing: context.actor_missing,
            ok,
            error_code,
            room_id: &target.room_id,
            player_id: &target.player_id,
            rollout_epoch: &target.rollout_epoch,
            checksum: &target.checksum,
            target_server_id: &target.target_server_id,
            config_key: &target.config_key,
            seq: packet.header.seq,
            message_type: packet.header.msg_type,
        })
        .await
}

async fn write_admin_audit_failure(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    seq: u32,
    audit_logger: &AdminAuditLogger,
    action: &'static str,
    error: &AdminAuditError,
) -> Result<(), std::io::Error> {
    warn!(
        action,
        error = %error,
        audit_path = %audit_logger.config.path.display(),
        "game-server admin audit write failed"
    );
    write_error(
        writer,
        seq,
        "ADMIN_AUDIT_WRITE_FAILED",
        "admin audit write failed",
    )
    .await
}

async fn audit_then_write_error(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    audit_logger: &AdminAuditLogger,
    context: &AdminAuthContext,
    packet: &Packet,
    action: &'static str,
    error_code: &str,
    message: &str,
    target: &AdminAuditTarget,
) -> Result<(), std::io::Error> {
    match audit_admin_write_result(
        audit_logger,
        context,
        packet,
        action,
        false,
        error_code,
        target,
    )
    .await
    {
        Ok(()) => write_error(writer, packet.header.seq, error_code, message).await,
        Err(error) => {
            write_admin_audit_failure(writer, packet.header.seq, audit_logger, action, &error).await
        }
    }
}

async fn audit_then_write_message<M: prost::Message>(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    audit_logger: &AdminAuditLogger,
    context: &AdminAuthContext,
    packet: &Packet,
    action: &'static str,
    message_type: MessageType,
    message: &M,
    ok: bool,
    error_code: &str,
    target: &AdminAuditTarget,
) -> Result<(), std::io::Error> {
    match audit_admin_write_result(
        audit_logger,
        context,
        packet,
        action,
        ok,
        error_code,
        target,
    )
    .await
    {
        Ok(()) => write_message(writer, message_type, packet.header.seq, message).await,
        Err(error) => {
            write_admin_audit_failure(writer, packet.header.seq, audit_logger, action, &error).await
        }
    }
}

fn update_config_target(request: &UpdateConfigReq) -> AdminAuditTarget {
    AdminAuditTarget {
        config_key: request.key.clone(),
        ..Default::default()
    }
}

fn player_target(player_id: impl Into<String>) -> AdminAuditTarget {
    AdminAuditTarget {
        player_id: player_id.into(),
        ..Default::default()
    }
}

fn room_transfer_target(
    room_id: impl Into<String>,
    rollout_epoch: impl Into<String>,
) -> AdminAuditTarget {
    AdminAuditTarget {
        room_id: room_id.into(),
        rollout_epoch: rollout_epoch.into(),
        ..Default::default()
    }
}

fn retire_room_transfer_target(request: &RetireTransferredRoomReq) -> AdminAuditTarget {
    AdminAuditTarget {
        room_id: request.room_id.clone(),
        rollout_epoch: request.rollout_epoch.clone(),
        checksum: request.checksum.clone(),
        ..Default::default()
    }
}

fn import_room_transfer_target(request: &ImportRoomTransferReq) -> AdminAuditTarget {
    let Some(payload) = request.payload.as_ref() else {
        return AdminAuditTarget::default();
    };

    AdminAuditTarget {
        room_id: payload.room_id.clone(),
        rollout_epoch: payload.rollout_epoch.clone(),
        checksum: payload.checksum.clone(),
        ..Default::default()
    }
}

fn redirect_target(request: &TriggerServerRedirectReq) -> AdminAuditTarget {
    AdminAuditTarget {
        room_id: request.room_id.clone(),
        rollout_epoch: request.rollout_epoch.clone(),
        target_server_id: request.target_server_id.clone(),
        ..Default::default()
    }
}

async fn ensure_parent_dir(path: &Path) -> Result<(), AdminAuditError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .await
            .map_err(AdminAuditError::Io)?;
    }

    Ok(())
}

async fn build_rollout_drain_status_response(
    room_manager: &SharedRoomManager,
    runtime_config: &SharedRuntimeConfig,
    owner_server_id: &str,
    connection_count: &Arc<AtomicU64>,
) -> GetRolloutDrainStatusRes {
    let snapshot = room_manager
        .rollout_drain_snapshot(owner_server_id, ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT)
        .await;
    let runtime = *runtime_config.read().await;

    GetRolloutDrainStatusRes {
        ok: true,
        error_code: String::new(),
        rollout_epoch: snapshot.rollout_epoch,
        owner_server_id: snapshot.owner_server_id,
        owned_room_count: snapshot.owned_room_count,
        migrating_room_count: snapshot.migrating_room_count,
        connection_count: connection_count.load(Ordering::Relaxed),
        routes: snapshot.routes,
        drain_mode_enabled: runtime.drain_mode_enabled,
        drain_mode_entered_at_ms: runtime.drain_mode_entered_at_ms.unwrap_or(0),
    }
}

fn decode_gm_broadcast_request(packet: &Packet) -> Result<GmBroadcastCommand, &'static str> {
    #[derive(Deserialize)]
    struct GmBroadcastJson {
        title: Option<String>,
        content: Option<String>,
        sender: Option<String>,
    }

    let request: GmBroadcastJson =
        serde_json::from_slice(&packet.body).map_err(|_| "INVALID_GM_BROADCAST_BODY")?;

    let title = normalize_required_string(
        request.title,
        "INVALID_TITLE",
        GM_BROADCAST_TITLE_MAX_LEN,
        "TITLE_TOO_LONG",
    )?;
    let content = normalize_required_string(
        request.content,
        "INVALID_CONTENT",
        GM_BROADCAST_CONTENT_MAX_LEN,
        "CONTENT_TOO_LONG",
    )?;
    let sender = normalize_optional_string(
        request.sender,
        "System",
        GM_SENDER_MAX_LEN,
        "SENDER_TOO_LONG",
    )?;

    Ok(GmBroadcastCommand {
        title,
        content,
        sender,
    })
}

fn decode_gm_kick_player_request(packet: &Packet) -> Result<GmKickPlayerCommand, &'static str> {
    #[derive(Deserialize)]
    struct GmKickPlayerJson {
        #[serde(rename = "playerId")]
        player_id: Option<String>,
        reason: Option<String>,
    }

    let request: GmKickPlayerJson =
        serde_json::from_slice(&packet.body).map_err(|_| "INVALID_GM_KICK_BODY")?;

    let player_id = normalize_required_string(
        request.player_id,
        "INVALID_PLAYER_ID",
        GM_PLAYER_ID_MAX_LEN,
        "PLAYER_ID_TOO_LONG",
    )?;
    let reason =
        normalize_optional_string(request.reason, "", GM_REASON_MAX_LEN, "REASON_TOO_LONG")?;

    Ok(GmKickPlayerCommand { player_id, reason })
}

fn decode_gm_ban_player_request(packet: &Packet) -> Result<GmBanPlayerCommand, &'static str> {
    #[derive(Deserialize)]
    struct GmBanPlayerJson {
        #[serde(rename = "playerId")]
        player_id: Option<String>,
        #[serde(rename = "durationSeconds")]
        duration_seconds: Option<u64>,
        reason: Option<String>,
    }

    let request: GmBanPlayerJson =
        serde_json::from_slice(&packet.body).map_err(|_| "INVALID_GM_BAN_BODY")?;

    let player_id = normalize_required_string(
        request.player_id,
        "INVALID_PLAYER_ID",
        GM_PLAYER_ID_MAX_LEN,
        "PLAYER_ID_TOO_LONG",
    )?;
    let duration_seconds = request.duration_seconds.ok_or("INVALID_DURATION")?;
    if duration_seconds == 0 || duration_seconds > GM_BAN_DURATION_MAX_SECONDS {
        return Err("INVALID_DURATION");
    }
    let reason =
        normalize_optional_string(request.reason, "", GM_REASON_MAX_LEN, "REASON_TOO_LONG")?;

    Ok(GmBanPlayerCommand {
        player_id,
        duration_seconds,
        reason,
    })
}

fn gm_disconnect_reason(default_reason: &str, request_reason: &str) -> String {
    if request_reason.is_empty() {
        default_reason.to_string()
    } else {
        request_reason.to_string()
    }
}

async fn kick_online_player(
    player_registry: &PlayerRegistry,
    player_id: &str,
    kick_reason: &str,
) -> Result<KickOnlineOutcome, &'static str> {
    let handle = {
        let registry = player_registry.read().await;
        registry.get(player_id).cloned()
    }
    .ok_or("PLAYER_OFFLINE")?;

    if handle.outbound.is_closed() {
        return Err("PLAYER_CONNECTION_UNAVAILABLE");
    }

    *handle.kick_reason.write().await = kick_reason.to_string();
    handle.kick_notify.notify_one();

    Ok(KickOnlineOutcome {
        player_id: player_id.to_string(),
        session_id: handle.session_id,
    })
}

async fn validate_grant_items_request(
    request: &GrantItemsReq,
    config_tables: &ConfigTableRuntime,
) -> Result<(), Box<dyn std::error::Error>> {
    if request.player_id.trim().is_empty() {
        return Err(Box::new(std::io::Error::other("INVALID_PLAYER_ID")));
    }

    if request.items.is_empty() {
        return Err(Box::new(std::io::Error::other("EMPTY_ITEMS")));
    }

    let tables = config_tables.tables_snapshot().await;
    for item in &request.items {
        if item.count == 0 {
            return Err(Box::new(std::io::Error::other("INVALID_ITEM_COUNT")));
        }
        if tables.item_table.get(item.item_id).is_none() {
            return Err(Box::new(std::io::Error::other("ITEM_NOT_FOUND")));
        }
    }

    Ok(())
}

fn decode_grant_items_request(
    packet: &Packet,
) -> Result<GrantItemsReq, Box<dyn std::error::Error>> {
    if let Ok(request) = packet.decode_body::<GrantItemsReq>("INVALID_GRANT_ITEMS_BODY") {
        if !request.player_id.is_empty() {
            return Ok(request);
        }
    }

    #[derive(serde::Deserialize)]
    struct LegacySendItem {
        #[serde(rename = "itemId", alias = "id")]
        item_id: serde_json::Value,
        count: u32,
        #[serde(default)]
        binded: bool,
    }

    #[derive(serde::Deserialize)]
    struct LegacySendItemRequest {
        #[serde(rename = "requestId")]
        request_id: Option<String>,
        #[serde(rename = "playerId")]
        player_id: String,
        #[serde(rename = "itemId")]
        item_id: Option<serde_json::Value>,
        #[serde(rename = "itemCount")]
        item_count: Option<u32>,
        items: Option<Vec<LegacySendItem>>,
        reason: Option<String>,
        source: Option<String>,
    }

    let legacy: LegacySendItemRequest = serde_json::from_slice(&packet.body)?;
    let items = if let Some(items) = legacy.items {
        items
            .into_iter()
            .map(|item| {
                let item_id = parse_item_id_value(item.item_id)?;
                Ok(GrantItem {
                    item_id,
                    count: item.count,
                    binded: item.binded,
                })
            })
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?
    } else {
        let item_id = parse_item_id_value(
            legacy
                .item_id
                .ok_or_else(|| std::io::Error::other("INVALID_ITEM_ID"))?,
        )?;
        vec![GrantItem {
            item_id,
            count: legacy.item_count.unwrap_or(0),
            binded: false,
        }]
    };

    let request_id = legacy.request_id.unwrap_or_else(|| {
        format!(
            "legacy-gm-send-item:{}:{}",
            legacy.player_id,
            current_unix_ms()
        )
    });

    Ok(GrantItemsReq {
        request_id,
        player_id: legacy.player_id,
        items,
        source: legacy.source.unwrap_or_else(|| "gm".to_string()),
        reason: legacy.reason.unwrap_or_default(),
    })
}

fn parse_item_id_value(value: serde_json::Value) -> Result<i32, Box<dyn std::error::Error>> {
    match value {
        serde_json::Value::Number(number) => Ok(number
            .as_i64()
            .ok_or_else(|| std::io::Error::other("INVALID_ITEM_ID"))?
            as i32),
        serde_json::Value::String(text) => Ok(text.parse::<i32>()?),
        _ => Err(Box::new(std::io::Error::other("INVALID_ITEM_ID"))),
    }
}

fn grant_item_to_inventory_item(item: &GrantItem) -> Item {
    Item {
        uid: next_item_uid(),
        item_id: item.item_id,
        count: item.count,
        binded: item.binded,
    }
}

fn next_item_uid() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos() as u64)
        .unwrap_or(0);

    loop {
        let previous = ITEM_UID_COUNTER.load(Ordering::Relaxed);
        let next = now.max(previous.saturating_add(1));
        if ITEM_UID_COUNTER
            .compare_exchange(previous, next, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return next;
        }
    }
}

fn current_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default()
}

async fn push_item_obtain_if_online(
    room_manager: &SharedRoomManager,
    player_id: &str,
    items: &[Item],
    source: &str,
) -> Result<(), std::io::Error> {
    room_manager
        .send_to_player(
            player_id,
            MessageType::ItemObtainPush,
            encode_body(&ItemObtainPush {
                items: items.iter().map(inventory_item_to_pb_item).collect(),
                source: source.to_string(),
            }),
        )
        .await
}

async fn push_inventory_update_if_online(
    room_manager: &SharedRoomManager,
    player_id: &str,
    player_data: &crate::core::inventory::PlayerData,
) -> Result<(), std::io::Error> {
    room_manager
        .send_to_player(
            player_id,
            MessageType::InventoryUpdatePush,
            encode_body(&InventoryUpdatePush {
                inventory_items: player_data
                    .get_inventory_items()
                    .iter()
                    .map(|item| inventory_item_to_pb_item(item))
                    .collect(),
                warehouse_items: player_data
                    .get_warehouse_items()
                    .iter()
                    .map(|item| inventory_item_to_pb_item(item))
                    .collect(),
            }),
        )
        .await
}

fn inventory_item_to_pb_item(item: &Item) -> PbItem {
    PbItem {
        uid: item.uid,
        item_id: item.item_id,
        count: item.count,
        binded: item.binded,
    }
}

async fn apply_runtime_config(
    runtime_config: &SharedRuntimeConfig,
    key: &str,
    value: &str,
) -> Result<(), &'static str> {
    let mut runtime = runtime_config.write().await;

    match key {
        "max_body_len" => {
            let parsed = value.parse::<usize>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if !(1..=1024 * 1024).contains(&parsed) {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.max_body_len = parsed;
            Ok(())
        }
        "heartbeat_timeout_secs" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if !(1..=3600).contains(&parsed) {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.heartbeat_timeout_secs = parsed;
            Ok(())
        }
        "msg_rate_window_ms" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if !(1..=60_000).contains(&parsed) {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.msg_rate_window_ms = parsed;
            Ok(())
        }
        "msg_rate_max" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if parsed > 10_000 {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.msg_rate_max = parsed;
            Ok(())
        }
        "player_msg_rate_window_ms" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if !(1..=60_000).contains(&parsed) {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.player_msg_rate_window_ms = parsed;
            Ok(())
        }
        "player_msg_rate_max" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if parsed > 10_000 {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.player_msg_rate_max = parsed;
            Ok(())
        }
        "input_timestamp_required" => {
            runtime.input_timestamp_required = parse_bool_config_value(value)?;
            Ok(())
        }
        "input_timestamp_max_skew_ms" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if parsed > 300_000 {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.input_timestamp_max_skew_ms = parsed;
            Ok(())
        }
        "input_anomaly_window_ms" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if !(1..=300_000).contains(&parsed) {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.input_anomaly_window_ms = parsed;
            Ok(())
        }
        "input_anomaly_max" => {
            let parsed = value.parse::<u64>().map_err(|_| "INVALID_CONFIG_VALUE")?;
            if parsed > 10_000 {
                return Err("INVALID_CONFIG_VALUE");
            }
            runtime.input_anomaly_max = parsed;
            Ok(())
        }
        "drain_mode" | "drain_mode_enabled" => {
            let parsed = parse_bool_config_value(value)?;
            let previous = runtime.drain_mode_enabled;
            runtime.drain_mode_enabled = parsed;
            runtime.drain_mode_entered_at_ms = if parsed {
                runtime
                    .drain_mode_entered_at_ms
                    .or(Some(current_unix_ms_u64()))
            } else {
                None
            };

            if previous != parsed {
                info!(
                    drain_mode_enabled = parsed,
                    drain_mode_entered_at_ms = ?runtime.drain_mode_entered_at_ms,
                    "game-server drain mode updated"
                );
            }
            Ok(())
        }
        _ => Err("UNSUPPORTED_CONFIG_KEY"),
    }
}

fn parse_bool_config_value(value: &str) -> Result<bool, &'static str> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "on" | "enabled" => Ok(true),
        "0" | "false" | "off" | "disabled" => Ok(false),
        _ => Err("INVALID_CONFIG_VALUE"),
    }
}

fn current_unix_ms_u64() -> u64 {
    current_unix_ms().max(0) as u64
}

async fn read_packet(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
) -> Result<Option<Packet>, Box<dyn std::error::Error>> {
    let read_header = timeout(Duration::from_secs(10), read_header_bytes(reader)).await;
    let header_buf = match read_header {
        Ok(Ok(Some(header_buf))) => header_buf,
        Ok(Ok(None)) => return Ok(None),
        Ok(Err(error)) => return Err(Box::new(error)),
        Err(_) => {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "read timeout",
            )));
        }
    };

    let header = parse_header(header_buf).map_err(std::io::Error::other)?;
    if header.body_len as usize > ADMIN_MAX_BODY_LEN {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "body too large",
        )));
    }

    let mut body = vec![0u8; header.body_len as usize];
    reader.read_exact(&mut body).await?;
    Ok(Some(Packet::new(header, body)))
}

async fn read_header_bytes(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
) -> Result<Option<[u8; HEADER_LEN]>, std::io::Error> {
    let mut header_buf = [0u8; HEADER_LEN];
    match reader.read_exact(&mut header_buf).await {
        Ok(_) => Ok(Some(header_buf)),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error),
    }
}

async fn write_message<M: prost::Message>(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    message_type: MessageType,
    seq: u32,
    message: &M,
) -> Result<(), std::io::Error> {
    let body = encode_body(message);
    let packet = encode_packet(message_type, seq, &body);
    writer.write_all(&packet).await
}

async fn write_error(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    seq: u32,
    error_code: &str,
    message: &str,
) -> Result<(), std::io::Error> {
    write_message(
        writer,
        MessageType::ErrorRes,
        seq,
        &ErrorRes {
            error_code: error_code.to_string(),
            message: message.to_string(),
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use tokio::sync::Notify;
    use tokio::sync::RwLock;
    use tokio::sync::mpsc;

    use super::*;
    use crate::core::context::PlayerConnectionHandle;
    use crate::core::logic::{RoomLogic, RoomLogicFactory, RoomLogicTransfer};
    use crate::core::room::{ConnectionCloseState, MemberRole, OutboundChannel, OutboundMessage};
    use crate::core::runtime::RoomManager;
    use crate::pb::GameMessagePush;
    use crate::protocol::PacketHeader;

    struct NoopRoomLogic;

    impl RoomLogic for NoopRoomLogic {}

    impl RoomLogicTransfer for NoopRoomLogic {}

    struct NoopRoomLogicFactory;

    impl RoomLogicFactory for NoopRoomLogicFactory {
        fn create(&self, _policy_id: &str) -> Box<dyn RoomLogic> {
            Box::new(NoopRoomLogic)
        }
    }

    fn runtime_config_fixture() -> SharedRuntimeConfig {
        Arc::new(RwLock::new(RuntimeConfig {
            heartbeat_timeout_secs: 30,
            max_body_len: 4096,
            msg_rate_window_ms: 1000,
            msg_rate_max: 0,
            player_msg_rate_window_ms: 1000,
            player_msg_rate_max: 0,
            input_timestamp_required: false,
            input_timestamp_max_skew_ms: 5000,
            input_anomaly_window_ms: 10_000,
            input_anomaly_max: 0,
            drain_mode_enabled: false,
            drain_mode_entered_at_ms: None,
        }))
    }

    fn json_packet(message_type: MessageType, body: &str) -> Packet {
        Packet::new(
            PacketHeader {
                msg_type: message_type as u16,
                seq: 1,
                body_len: body.len() as u32,
            },
            body.as_bytes().to_vec(),
        )
    }

    fn bytes_packet(message_type: MessageType, seq: u32, body: Vec<u8>) -> Packet {
        Packet::new(
            PacketHeader {
                msg_type: message_type as u16,
                seq,
                body_len: body.len() as u32,
            },
            body,
        )
    }

    fn temp_audit_path(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("myserver-{name}-{unique}.jsonl"))
    }

    fn player_registry_fixture(
        player_id: &str,
    ) -> (
        PlayerRegistry,
        Arc<Notify>,
        Arc<RwLock<String>>,
        mpsc::Receiver<OutboundMessage>,
    ) {
        let (tx, rx) = mpsc::channel(8);
        let notify = Arc::new(Notify::new());
        let kick_reason = Arc::new(RwLock::new("session_kicked".to_string()));
        let registry = Arc::new(RwLock::new(std::collections::HashMap::from([(
            player_id.to_string(),
            PlayerConnectionHandle {
                kick_notify: notify.clone(),
                session_id: 42,
                outbound: OutboundChannel::new(tx, ConnectionCloseState::new()),
                kick_reason: kick_reason.clone(),
            },
        )])));

        (registry, notify, kick_reason, rx)
    }

    #[test]
    fn admin_auth_accepts_legacy_token_without_actor() {
        let packet = bytes_packet(MessageType::AdminAuthReq, 0, b"secret-admin-token".to_vec());

        let context = authenticate_admin_packet(&packet, "secret-admin-token").unwrap();

        assert_eq!(context.actor, DEFAULT_ADMIN_ACTOR);
        assert!(context.actor_missing);
    }

    #[test]
    fn admin_auth_accepts_json_envelope_actor() {
        let packet = bytes_packet(
            MessageType::AdminAuthReq,
            0,
            br#"{"token":"secret-admin-token","actor":"ops@example.com"}"#.to_vec(),
        );

        let context = authenticate_admin_packet(&packet, "secret-admin-token").unwrap();

        assert_eq!(context.actor, "ops@example.com");
        assert!(!context.actor_missing);
    }

    #[test]
    fn admin_auth_normalizes_invalid_actor_to_missing() {
        let packet = bytes_packet(
            MessageType::AdminAuthReq,
            0,
            br#"{"token":"secret-admin-token","actor":"bad actor"}"#.to_vec(),
        );

        let context = authenticate_admin_packet(&packet, "secret-admin-token").unwrap();

        assert_eq!(context.actor, DEFAULT_ADMIN_ACTOR);
        assert!(context.actor_missing);
    }

    #[test]
    fn admin_auth_rejects_wrong_json_envelope_token() {
        let packet = bytes_packet(
            MessageType::AdminAuthReq,
            0,
            br#"{"token":"wrong-token","actor":"ops@example.com"}"#.to_vec(),
        );

        assert!(authenticate_admin_packet(&packet, "secret-admin-token").is_none());
    }

    #[tokio::test]
    async fn admin_audit_unwritable_path_rejects_write_before_state_change() {
        let audit_path = std::env::temp_dir();
        let audit_logger =
            AdminAuditLogger::new(AdminAuditConfig::new(true, audit_path.clone(), false));
        let context = AdminAuthContext {
            actor: "ops@example.com".to_string(),
            actor_missing: false,
        };
        let runtime_config = runtime_config_fixture();
        let packet = bytes_packet(
            MessageType::AdminUpdateConfigReq,
            100,
            encode_body(&UpdateConfigReq {
                key: "max_body_len".to_string(),
                value: "8192".to_string(),
            }),
        );

        let result =
            ensure_admin_write_allowed(&audit_logger, &context, &packet, "admin_update_config")
                .await;

        assert_eq!(result, Err(AdminWritePreflightError::AuditUnavailable));
        assert_eq!(runtime_config.read().await.max_body_len, 4096);
    }

    #[tokio::test]
    async fn admin_audit_require_actor_rejects_write_before_state_change() {
        let audit_path = temp_audit_path("require-actor");
        let audit_logger =
            AdminAuditLogger::new(AdminAuditConfig::new(true, audit_path.clone(), true));
        let context = AdminAuthContext {
            actor: DEFAULT_ADMIN_ACTOR.to_string(),
            actor_missing: true,
        };
        let runtime_config = runtime_config_fixture();
        let packet = bytes_packet(
            MessageType::AdminUpdateConfigReq,
            99,
            encode_body(&UpdateConfigReq {
                key: "max_body_len".to_string(),
                value: "8192".to_string(),
            }),
        );

        let result =
            ensure_admin_write_allowed(&audit_logger, &context, &packet, "admin_update_config")
                .await;

        assert_eq!(result, Err(AdminWritePreflightError::ActorRequired));
        assert_eq!(runtime_config.read().await.max_body_len, 4096);
        let audit = fs::read_to_string(&audit_path).unwrap();
        assert!(audit.contains("\"action\":\"admin_update_config\""));
        assert!(audit.contains("\"actor\":\"unknown\""));
        assert!(audit.contains("\"actor_missing\":true"));
        assert!(audit.contains("\"error_code\":\"ADMIN_ACTOR_REQUIRED\""));
        let _ = fs::remove_file(audit_path);
    }

    #[tokio::test]
    async fn admin_audit_event_does_not_leak_token_or_payload() {
        let audit_path = temp_audit_path("no-secret");
        let audit_logger =
            AdminAuditLogger::new(AdminAuditConfig::new(true, audit_path.clone(), false));
        let context = AdminAuthContext {
            actor: "ops@example.com".to_string(),
            actor_missing: false,
        };
        let packet = bytes_packet(
            MessageType::AdminUpdateConfigReq,
            7,
            encode_body(&UpdateConfigReq {
                key: "max_body_len".to_string(),
                value: "8192-secret-value".to_string(),
            }),
        );
        let target = AdminAuditTarget {
            config_key: "max_body_len".to_string(),
            ..Default::default()
        };

        audit_admin_write_result(
            &audit_logger,
            &context,
            &packet,
            "admin_update_config",
            true,
            "",
            &target,
        )
        .await
        .unwrap();

        let audit = fs::read_to_string(&audit_path).unwrap();
        assert!(audit.contains("\"channel\":\"admin_tcp\""));
        assert!(audit.contains("\"actor\":\"ops@example.com\""));
        assert!(audit.contains("\"config_key\":\"max_body_len\""));
        assert!(!audit.contains("secret-admin-token"));
        assert!(!audit.contains("8192-secret-value"));
        assert!(!audit.contains("\"token\""));
        let _ = fs::remove_file(audit_path);
    }

    #[test]
    fn decode_gm_broadcast_trims_and_defaults_sender() {
        let packet = json_packet(
            MessageType::GmBroadcastReq,
            r#"{"title":"  Notice ","content":" Hello ","sender":" "}"#,
        );

        let request = decode_gm_broadcast_request(&packet).unwrap();

        assert_eq!(
            request,
            GmBroadcastCommand {
                title: "Notice".to_string(),
                content: "Hello".to_string(),
                sender: "System".to_string(),
            }
        );
    }

    #[test]
    fn decode_gm_broadcast_rejects_empty_and_too_long_values() {
        let empty_title = json_packet(
            MessageType::GmBroadcastReq,
            r#"{"title":" ","content":"Hello","sender":"System"}"#,
        );
        assert_eq!(
            decode_gm_broadcast_request(&empty_title),
            Err("INVALID_TITLE")
        );

        let title = "a".repeat(GM_BROADCAST_TITLE_MAX_LEN + 1);
        let too_long_title = json_packet(
            MessageType::GmBroadcastReq,
            &json!({"title": title, "content": "Hello", "sender": "System"}).to_string(),
        );
        assert_eq!(
            decode_gm_broadcast_request(&too_long_title),
            Err("TITLE_TOO_LONG")
        );
    }

    #[test]
    fn decode_gm_kick_validates_player_id_and_reason() {
        let packet = json_packet(
            MessageType::GmKickPlayerReq,
            r#"{"playerId":" player-a ","reason":" reconnect "}"#,
        );

        let request = decode_gm_kick_player_request(&packet).unwrap();

        assert_eq!(
            request,
            GmKickPlayerCommand {
                player_id: "player-a".to_string(),
                reason: "reconnect".to_string(),
            }
        );

        let missing_player = json_packet(MessageType::GmKickPlayerReq, r#"{"reason":"x"}"#);
        assert_eq!(
            decode_gm_kick_player_request(&missing_player),
            Err("INVALID_PLAYER_ID")
        );
    }

    #[test]
    fn decode_gm_ban_validates_duration() {
        let packet = json_packet(
            MessageType::GmBanPlayerReq,
            r#"{"playerId":"player-a","durationSeconds":3600,"reason":"cheat"}"#,
        );

        let request = decode_gm_ban_player_request(&packet).unwrap();

        assert_eq!(
            request,
            GmBanPlayerCommand {
                player_id: "player-a".to_string(),
                duration_seconds: 3600,
                reason: "cheat".to_string(),
            }
        );

        let invalid_duration = json_packet(
            MessageType::GmBanPlayerReq,
            r#"{"playerId":"player-a","durationSeconds":0}"#,
        );
        assert_eq!(
            decode_gm_ban_player_request(&invalid_duration),
            Err("INVALID_DURATION")
        );
    }

    #[test]
    fn gm_disconnect_reason_uses_request_reason_when_present() {
        assert_eq!(gm_disconnect_reason("gm_kick", ""), "gm_kick");
        assert_eq!(gm_disconnect_reason("gm_kick", "manual"), "manual");
    }

    #[tokio::test]
    async fn kick_online_player_sets_reason_and_notifies_connection() {
        let (registry, notify, kick_reason, _rx) = player_registry_fixture("player-a");
        let notified = notify.notified();

        let outcome = kick_online_player(&registry, "player-a", "gm_kick")
            .await
            .unwrap();

        assert_eq!(
            outcome,
            KickOnlineOutcome {
                player_id: "player-a".to_string(),
                session_id: 42,
            }
        );
        assert_eq!(&*kick_reason.read().await, "gm_kick");
        notified.await;
    }

    #[tokio::test]
    async fn kick_offline_player_returns_player_offline() {
        let (registry, _notify, _kick_reason, _rx) = player_registry_fixture("player-a");

        let result = kick_online_player(&registry, "player-offline", "gm_kick").await;

        assert_eq!(result, Err("PLAYER_OFFLINE"));
    }

    #[tokio::test]
    async fn broadcast_gm_message_queues_game_message_for_online_players() {
        let (registry, _notify, _kick_reason, mut rx) = player_registry_fixture("player-a");
        let request = GmBroadcastCommand {
            title: "Notice".to_string(),
            content: "Hello".to_string(),
            sender: "System".to_string(),
        };

        let delivered = broadcast_gm_message_to_online_players(&registry, &request).await;

        assert_eq!(delivered, 1);
        let message = rx.try_recv().expect("gm broadcast queued");
        assert_eq!(message.message_type, MessageType::GameMessagePush);
        let push = prost::Message::decode(message.body.as_slice()).unwrap();
        let push: GameMessagePush = push;
        assert_eq!(push.event, "gm_broadcast");
        assert_eq!(push.action, "broadcast");
        assert!(push.payload_json.contains("\"title\":\"Notice\""));
    }

    #[tokio::test]
    async fn apply_runtime_config_updates_drain_mode() {
        let runtime_config = runtime_config_fixture();

        apply_runtime_config(&runtime_config, "drain_mode", "on")
            .await
            .unwrap();

        let enabled = *runtime_config.read().await;
        assert!(enabled.drain_mode_enabled);
        assert!(enabled.drain_mode_entered_at_ms.is_some());

        apply_runtime_config(&runtime_config, "drain_mode_enabled", "off")
            .await
            .unwrap();

        let disabled = *runtime_config.read().await;
        assert!(!disabled.drain_mode_enabled);
        assert_eq!(disabled.drain_mode_entered_at_ms, None);
    }

    #[tokio::test]
    async fn rollout_drain_status_response_includes_connection_count_and_routes() {
        let room_manager = Arc::new(RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(NoopRoomLogicFactory),
        ));
        let runtime_config = runtime_config_fixture();
        apply_runtime_config(&runtime_config, "drain_mode", "on")
            .await
            .unwrap();
        let drain_mode_entered_at_ms = runtime_config
            .read()
            .await
            .drain_mode_entered_at_ms
            .unwrap();
        let connection_count = Arc::new(AtomicU64::new(7));
        let (tx, _rx) = mpsc::channel(1024);
        room_manager
            .join_room(
                "room-test",
                "player-a",
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();

        let response = build_rollout_drain_status_response(
            &room_manager,
            &runtime_config,
            "game-server-old",
            &connection_count,
        )
        .await;

        assert!(response.ok);
        assert!(response.error_code.is_empty());
        assert_eq!(response.owner_server_id, "game-server-old");
        assert_eq!(response.connection_count, 7);
        assert!(response.drain_mode_enabled);
        assert_eq!(response.drain_mode_entered_at_ms, drain_mode_entered_at_ms);
        assert_eq!(response.owned_room_count, 1);
        assert_eq!(response.migrating_room_count, 0);
        assert_eq!(response.routes.len(), 1);
        assert_eq!(response.routes[0].room_id, "room-test");
        assert_eq!(response.routes[0].owner_server_id, "game-server-old");
    }

    #[tokio::test]
    async fn apply_runtime_config_rejects_invalid_drain_mode_value() {
        let runtime_config = runtime_config_fixture();

        let result = apply_runtime_config(&runtime_config, "drain_mode", "maybe").await;

        assert_eq!(result, Err("INVALID_CONFIG_VALUE"));
    }

    #[tokio::test]
    async fn apply_runtime_config_updates_message_rate_limit() {
        let runtime_config = runtime_config_fixture();

        apply_runtime_config(&runtime_config, "msg_rate_window_ms", "500")
            .await
            .unwrap();
        apply_runtime_config(&runtime_config, "msg_rate_max", "20")
            .await
            .unwrap();
        apply_runtime_config(&runtime_config, "player_msg_rate_window_ms", "750")
            .await
            .unwrap();
        apply_runtime_config(&runtime_config, "player_msg_rate_max", "30")
            .await
            .unwrap();

        let runtime = *runtime_config.read().await;
        assert_eq!(runtime.msg_rate_window_ms, 500);
        assert_eq!(runtime.msg_rate_max, 20);
        assert_eq!(runtime.player_msg_rate_window_ms, 750);
        assert_eq!(runtime.player_msg_rate_max, 30);
    }

    #[tokio::test]
    async fn apply_runtime_config_updates_input_timestamp_window() {
        let runtime_config = runtime_config_fixture();

        apply_runtime_config(&runtime_config, "input_timestamp_required", "true")
            .await
            .unwrap();
        apply_runtime_config(&runtime_config, "input_timestamp_max_skew_ms", "300000")
            .await
            .unwrap();

        let runtime = *runtime_config.read().await;
        assert!(runtime.input_timestamp_required);
        assert_eq!(runtime.input_timestamp_max_skew_ms, 300_000);

        apply_runtime_config(&runtime_config, "input_timestamp_required", "off")
            .await
            .unwrap();
        apply_runtime_config(&runtime_config, "input_timestamp_max_skew_ms", "0")
            .await
            .unwrap();

        let runtime = *runtime_config.read().await;
        assert!(!runtime.input_timestamp_required);
        assert_eq!(runtime.input_timestamp_max_skew_ms, 0);
    }

    #[tokio::test]
    async fn apply_runtime_config_updates_input_anomaly_policy() {
        let runtime_config = runtime_config_fixture();

        apply_runtime_config(&runtime_config, "input_anomaly_window_ms", "60000")
            .await
            .unwrap();
        apply_runtime_config(&runtime_config, "input_anomaly_max", "5")
            .await
            .unwrap();

        let runtime = *runtime_config.read().await;
        assert_eq!(runtime.input_anomaly_window_ms, 60_000);
        assert_eq!(runtime.input_anomaly_max, 5);

        apply_runtime_config(&runtime_config, "input_anomaly_max", "0")
            .await
            .unwrap();

        let runtime = *runtime_config.read().await;
        assert_eq!(runtime.input_anomaly_max, 0);
    }

    #[tokio::test]
    async fn apply_runtime_config_rejects_invalid_input_timestamp_window() {
        let runtime_config = runtime_config_fixture();

        assert_eq!(
            apply_runtime_config(&runtime_config, "input_timestamp_required", "maybe").await,
            Err("INVALID_CONFIG_VALUE")
        );
        assert_eq!(
            apply_runtime_config(&runtime_config, "input_timestamp_max_skew_ms", "300001").await,
            Err("INVALID_CONFIG_VALUE")
        );
    }

    #[tokio::test]
    async fn apply_runtime_config_rejects_invalid_input_anomaly_policy() {
        let runtime_config = runtime_config_fixture();

        assert_eq!(
            apply_runtime_config(&runtime_config, "input_anomaly_window_ms", "0").await,
            Err("INVALID_CONFIG_VALUE")
        );
        assert_eq!(
            apply_runtime_config(&runtime_config, "input_anomaly_window_ms", "300001").await,
            Err("INVALID_CONFIG_VALUE")
        );
        assert_eq!(
            apply_runtime_config(&runtime_config, "input_anomaly_max", "10001").await,
            Err("INVALID_CONFIG_VALUE")
        );
    }

    #[tokio::test]
    async fn apply_runtime_config_rejects_invalid_message_rate_limit() {
        let runtime_config = runtime_config_fixture();

        assert_eq!(
            apply_runtime_config(&runtime_config, "msg_rate_window_ms", "0").await,
            Err("INVALID_CONFIG_VALUE")
        );
        assert_eq!(
            apply_runtime_config(&runtime_config, "msg_rate_max", "10001").await,
            Err("INVALID_CONFIG_VALUE")
        );
        assert_eq!(
            apply_runtime_config(&runtime_config, "player_msg_rate_window_ms", "0").await,
            Err("INVALID_CONFIG_VALUE")
        );
        assert_eq!(
            apply_runtime_config(&runtime_config, "player_msg_rate_max", "10001").await,
            Err("INVALID_CONFIG_VALUE")
        );
    }
}
