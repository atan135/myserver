use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

use crate::admin_pb::{
    GrantItem, GrantItemsReq, GrantItemsRes, ServerStatusReq, ServerStatusRes, UpdateConfigReq,
    UpdateConfigRes,
};
use crate::core::config_table::ConfigTableRuntime;
use crate::core::context::{
    PlayerRegistry, SharedRoomManager, SharedRuntimeConfig, ShutdownSignal,
};
use crate::core::global_id::ItemUidGenerator;
use crate::core::inventory::Item;
use crate::core::player::PlayerManager;
use crate::core::runtime::room_manager::{
    ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT, RolloutDrainNotice,
};
use crate::csv_code::itemtable::ItemTable;
use crate::gm_broadcast::{
    GM_BROADCAST_CONTENT_MAX_LEN, GM_BROADCAST_TITLE_MAX_LEN, GM_SENDER_MAX_LEN,
    GmBroadcastCommand, broadcast_gm_message_to_online_players, normalize_optional_string,
    normalize_required_string,
};
use crate::pb::{
    ConfirmRoomOwnershipReq, ConfirmRoomOwnershipRes, ExportRoomTransferReq, ExportRoomTransferRes,
    FreezeRoomForTransferReq, FreezeRoomForTransferRes, GetRolloutDrainStatusReq,
    GetRolloutDrainStatusRes, ImportRoomTransferReq, ImportRoomTransferRes,
    RequestServerShutdownReq, RequestServerShutdownRes, RetireTransferredRoomReq,
    RetireTransferredRoomRes, ServerRedirectPush, TriggerRolloutDrainNoticeReq,
    TriggerRolloutDrainNoticeRes, TriggerServerRedirectReq, TriggerServerRedirectRes,
};
use crate::pb::{InventoryUpdatePush, Item as PbItem, ItemObtainPush};
use crate::protocol::{MessageType, Packet, encode_body};
use crate::server::{DEFAULT_DRAIN_MODE_REASON, DEFAULT_DRAIN_MODE_SOURCE, RuntimeConfig};

const GM_REASON_MAX_LEN: usize = 512;
const GM_PLAYER_ID_MAX_LEN: usize = 128;
const GM_CHARACTER_ID_MAX_LEN: usize = 128;
const GM_BAN_DURATION_MAX_SECONDS: u64 = 31_536_000;

mod audit;
mod auth;
mod protocol_io;

pub use audit::{AdminAuditConfig, AdminAuditLogger};

use audit::{
    AdminAuditTarget, audit_then_write_error, audit_then_write_message, ensure_admin_write_allowed,
};
use auth::authenticate_admin_packet;
use protocol_io::{read_packet, write_error, write_message};

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
    character_id: String,
    session_id: u64,
}

pub async fn run_listener(
    listener: TcpListener,
    room_manager: SharedRoomManager,
    runtime_config: SharedRuntimeConfig,
    connection_count: Arc<AtomicU64>,
    player_registry: PlayerRegistry,
    player_manager: PlayerManager,
    config_tables: ConfigTableRuntime,
    item_uid_generator: ItemUidGenerator,
    owner_server_id: String,
    admin_token: String,
    audit_logger: AdminAuditLogger,
    shutdown_signal: ShutdownSignal,
) -> Result<(), std::io::Error> {
    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let room_manager = room_manager.clone();
        let runtime_config = runtime_config.clone();
        let connection_count = connection_count.clone();
        let player_registry = player_registry.clone();
        let player_manager = player_manager.clone();
        let config_tables = config_tables.clone();
        let item_uid_generator = item_uid_generator.clone();
        let owner_server_id = owner_server_id.clone();
        let admin_token = admin_token.clone();
        let audit_logger = audit_logger.clone();
        let shutdown_signal = shutdown_signal.clone();

        tokio::spawn(async move {
            if let Err(error) = handle_admin_connection(
                socket,
                room_manager,
                runtime_config,
                connection_count,
                player_registry,
                player_manager,
                config_tables,
                item_uid_generator,
                owner_server_id,
                admin_token,
                audit_logger,
                shutdown_signal,
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
    item_uid_generator: ItemUidGenerator,
    owner_server_id: String,
    admin_token: String,
    audit_logger: AdminAuditLogger,
    shutdown_signal: ShutdownSignal,
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
                let runtime = runtime_config.read().await.clone();
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
                let target = character_target(request.character_id.clone());
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
                let tables = config_tables.tables_snapshot().await;
                let items = match request
                    .items
                    .iter()
                    .map(|item| {
                        grant_item_to_inventory_item(
                            item,
                            &request.character_id,
                            &item_uid_generator,
                            &tables.item_table,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()
                {
                    Ok(items) => items,
                    Err(error) => {
                        warn!(error = %error, "failed to generate global item uid for GM grant");
                        audit_then_write_error(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            "GLOBAL_ID_GENERATE_FAILED",
                            "failed to generate item uid",
                            &target,
                        )
                        .await?;
                        continue;
                    }
                };

                let result = player_manager
                    .grant_items_with_request(
                        &request.character_id,
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
                                &request.character_id,
                                &items,
                                &request.source,
                            )
                            .await;
                            let _ = push_inventory_update_if_online(
                                &room_manager,
                                &request.character_id,
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
                                    account_player_id = %outcome.player_id,
                                    player_id = %outcome.player_id,
                                    character_id = %outcome.character_id,
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
                                    account_player_id = %outcome.player_id,
                                    player_id = %outcome.player_id,
                                    character_id = %outcome.character_id,
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
            Some(MessageType::ConfirmRoomOwnershipReq) => {
                let action = "confirm_room_ownership";
                let request = match packet
                    .decode_body::<ConfirmRoomOwnershipReq>("INVALID_CONFIRM_ROOM_OWNERSHIP_BODY")
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
                            "invalid confirm room ownership request",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                        continue;
                    }
                };
                let target = confirm_room_ownership_target(&request);

                let result = room_manager
                    .confirm_room_ownership(
                        &request.rollout_epoch,
                        &request.room_id,
                        &request.checksum,
                        request.room_version,
                    )
                    .await;

                let response = match result {
                    Ok((checksum, room_version)) => ConfirmRoomOwnershipRes {
                        ok: true,
                        room_id: request.room_id,
                        error_code: String::new(),
                        checksum,
                        room_version,
                    },
                    Err(error_code) => ConfirmRoomOwnershipRes {
                        ok: false,
                        room_id: request.room_id,
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
                    MessageType::ConfirmRoomOwnershipRes,
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
            Some(MessageType::TriggerRolloutDrainNoticeReq) => {
                let action = "trigger_rollout_drain_notice";
                let request = match packet.decode_body::<TriggerRolloutDrainNoticeReq>(
                    "INVALID_ROLLOUT_DRAIN_NOTICE_BODY",
                ) {
                    Ok(request) => request,
                    Err(error_code) => {
                        audit_then_write_error(
                            &mut writer,
                            &audit_logger,
                            &auth_context,
                            &packet,
                            action,
                            error_code,
                            "invalid rollout drain notice request",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                        continue;
                    }
                };
                let target = rollout_drain_notice_target(&request);
                let room_id = request.room_id.clone();
                let result = room_manager
                    .trigger_rollout_drain_notice(RolloutDrainNotice {
                        room_id: request.room_id,
                        rollout_epoch: request.rollout_epoch,
                        reason: request.reason,
                        message: request.message,
                        retry_after_ms: request.retry_after_ms,
                        deadline_ms: request.deadline_ms,
                    })
                    .await;

                let response = match result {
                    Ok(delivery) => TriggerRolloutDrainNoticeRes {
                        ok: true,
                        room_id,
                        error_code: String::new(),
                        delivered_count: delivery.delivered_count,
                        failed_count: delivery.failed_count,
                        online_member_count: delivery.online_member_count,
                    },
                    Err(error_code) => TriggerRolloutDrainNoticeRes {
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
                info!(
                    action,
                    room_id = %response.room_id,
                    rollout_epoch = %target.rollout_epoch,
                    delivered_count = response.delivered_count,
                    failed_count = response.failed_count,
                    online_member_count = response.online_member_count,
                    ok = response.ok,
                    error_code = %response.error_code,
                    "rollout drain notice admin trigger result"
                );
                audit_then_write_message(
                    &mut writer,
                    &audit_logger,
                    &auth_context,
                    &packet,
                    action,
                    MessageType::TriggerRolloutDrainNoticeRes,
                    &response,
                    ok,
                    &error_code,
                    &target,
                )
                .await?;
            }
            Some(MessageType::RequestServerShutdownReq) => {
                let action = "request_server_shutdown";
                let request = match packet
                    .decode_body::<RequestServerShutdownReq>("INVALID_SERVER_SHUTDOWN_BODY")
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
                            "invalid server shutdown request",
                            &AdminAuditTarget::default(),
                        )
                        .await?;
                        continue;
                    }
                };
                let target = server_shutdown_target();
                let response = build_server_shutdown_response(
                    &room_manager,
                    &runtime_config,
                    &owner_server_id,
                    &connection_count,
                )
                .await;
                let ok = response.ok;
                let error_code = response.error_code.clone();
                audit_then_write_message(
                    &mut writer,
                    &audit_logger,
                    &auth_context,
                    &packet,
                    action,
                    MessageType::RequestServerShutdownRes,
                    &response,
                    ok,
                    &error_code,
                    &target,
                )
                .await?;

                if ok {
                    info!(
                        channel = "admin_tcp",
                        actor = %auth_context.actor,
                        reason = %request.reason,
                        connection_count = response.connection_count,
                        owned_room_count = response.owned_room_count,
                        migrating_room_count = response.migrating_room_count,
                        retired_room_count = response.retired_room_count,
                        "requesting game-server graceful shutdown"
                    );
                    shutdown_signal.notify_one();
                }
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
        MessageType::ConfirmRoomOwnershipReq => Some("confirm_room_ownership"),
        MessageType::RetireTransferredRoomReq => Some("retire_transferred_room"),
        MessageType::TriggerServerRedirectReq => Some("trigger_server_redirect"),
        MessageType::TriggerRolloutDrainNoticeReq => Some("trigger_rollout_drain_notice"),
        MessageType::RequestServerShutdownReq => Some("request_server_shutdown"),
        _ => None,
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

fn character_target(character_id: impl Into<String>) -> AdminAuditTarget {
    AdminAuditTarget {
        character_id: character_id.into(),
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

fn confirm_room_ownership_target(request: &ConfirmRoomOwnershipReq) -> AdminAuditTarget {
    AdminAuditTarget {
        room_id: request.room_id.clone(),
        rollout_epoch: request.rollout_epoch.clone(),
        checksum: request.checksum.clone(),
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

fn rollout_drain_notice_target(request: &TriggerRolloutDrainNoticeReq) -> AdminAuditTarget {
    AdminAuditTarget {
        room_id: request.room_id.clone(),
        rollout_epoch: request.rollout_epoch.clone(),
        ..Default::default()
    }
}

fn server_shutdown_target() -> AdminAuditTarget {
    AdminAuditTarget::default()
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
    let runtime = runtime_config.read().await.clone();
    let connection_count = connection_count.load(Ordering::Relaxed);

    if connection_count == 0 && snapshot.owned_room_count == 0 && snapshot.migrating_room_count == 0
    {
        info!(
            channel = "admin_tcp",
            drain_mode_enabled = runtime.drain_mode_enabled,
            drain_mode_reason = %runtime.drain_mode_reason,
            drain_mode_source = %runtime.drain_mode_source,
            connection_count = connection_count,
            owned_room_count = snapshot.owned_room_count,
            migrating_room_count = snapshot.migrating_room_count,
            transferable_empty_room_count = snapshot.transferable_empty_room_count,
            retired_room_count = snapshot.retired_room_count,
            rollout_epoch = %snapshot.rollout_epoch,
            owner_server_id = %snapshot.owner_server_id,
            "game-server rollout drain completed"
        );
    }

    GetRolloutDrainStatusRes {
        ok: true,
        error_code: String::new(),
        rollout_epoch: snapshot.rollout_epoch,
        owner_server_id: snapshot.owner_server_id,
        owned_room_count: snapshot.owned_room_count,
        migrating_room_count: snapshot.migrating_room_count,
        connection_count,
        routes: snapshot.routes,
        drain_mode_enabled: runtime.drain_mode_enabled,
        drain_mode_entered_at_ms: runtime.drain_mode_entered_at_ms.unwrap_or(0),
        transferable_empty_room_count: snapshot.transferable_empty_room_count,
        transferable_empty_room_samples: snapshot.transferable_empty_room_samples,
        drain_mode_reason: runtime.drain_mode_reason,
        drain_mode_source: runtime.drain_mode_source,
        retired_room_count: snapshot.retired_room_count,
    }
}

async fn build_server_shutdown_response(
    room_manager: &SharedRoomManager,
    runtime_config: &SharedRuntimeConfig,
    owner_server_id: &str,
    connection_count: &Arc<AtomicU64>,
) -> RequestServerShutdownRes {
    let snapshot = room_manager
        .rollout_drain_snapshot(owner_server_id, ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT)
        .await;
    let runtime = runtime_config.read().await.clone();
    let connection_count = connection_count.load(Ordering::Relaxed);

    let error_code = if !runtime.drain_mode_enabled {
        "SHUTDOWN_DRAIN_MODE_REQUIRED"
    } else if connection_count != 0 {
        "SHUTDOWN_CONNECTIONS_REMAIN"
    } else if snapshot.owned_room_count != 0 {
        "SHUTDOWN_OWNED_ROOMS_REMAIN"
    } else if snapshot.migrating_room_count != 0 {
        "SHUTDOWN_MIGRATING_ROOMS_REMAIN"
    } else {
        ""
    };

    RequestServerShutdownRes {
        ok: error_code.is_empty(),
        error_code: error_code.to_string(),
        connection_count,
        owned_room_count: snapshot.owned_room_count,
        migrating_room_count: snapshot.migrating_room_count,
        drain_mode_enabled: runtime.drain_mode_enabled,
        retired_room_count: snapshot.retired_room_count,
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
    // GM kick keeps the legacy player_id request field as an account id in P0.
    let handle = {
        let registry = player_registry.read().await;
        registry.get_by_account(player_id).cloned()
    }
    .ok_or("PLAYER_OFFLINE")?;

    if handle.outbound.is_closed() {
        return Err("PLAYER_CONNECTION_UNAVAILABLE");
    }

    *handle.kick_reason.write().await = kick_reason.to_string();
    handle.kick_notify.notify_one();

    Ok(KickOnlineOutcome {
        player_id: handle.account_player_id,
        character_id: handle.character_id,
        session_id: handle.session_id,
    })
}

async fn validate_grant_items_request(
    request: &GrantItemsReq,
    config_tables: &ConfigTableRuntime,
) -> Result<(), Box<dyn std::error::Error>> {
    if request.character_id.trim().is_empty() {
        return Err(Box::new(std::io::Error::other("INVALID_CHARACTER_ID")));
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
        if !request.character_id.is_empty() {
            return Ok(request);
        }
    }

    #[derive(serde::Deserialize)]
    struct GmSendItem {
        #[serde(rename = "itemId", alias = "id")]
        item_id: serde_json::Value,
        count: u32,
        #[serde(default)]
        binded: bool,
    }

    #[derive(serde::Deserialize)]
    struct GmSendItemRequest {
        #[serde(rename = "requestId")]
        request_id: Option<String>,
        #[serde(rename = "characterId")]
        character_id: Option<String>,
        #[serde(rename = "itemId")]
        item_id: Option<serde_json::Value>,
        #[serde(rename = "itemCount")]
        item_count: Option<u32>,
        items: Option<Vec<GmSendItem>>,
        reason: Option<String>,
        source: Option<String>,
    }

    let request: GmSendItemRequest = serde_json::from_slice(&packet.body)?;
    let character_id = normalize_required_string(
        request.character_id,
        "INVALID_CHARACTER_ID",
        GM_CHARACTER_ID_MAX_LEN,
        "CHARACTER_ID_TOO_LONG",
    )
    .map_err(std::io::Error::other)?;
    let items = if let Some(items) = request.items {
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
            request
                .item_id
                .ok_or_else(|| std::io::Error::other("INVALID_ITEM_ID"))?,
        )?;
        vec![GrantItem {
            item_id,
            count: request.item_count.unwrap_or(0),
            binded: false,
        }]
    };

    let request_id = request
        .request_id
        .unwrap_or_else(|| format!("gm-send-item:{}:{}", character_id, current_unix_ms()));

    Ok(GrantItemsReq {
        request_id,
        character_id,
        items,
        source: request.source.unwrap_or_else(|| "gm".to_string()),
        reason: request.reason.unwrap_or_default(),
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

fn grant_item_to_inventory_item(
    item: &GrantItem,
    character_id: &str,
    item_uid_generator: &ItemUidGenerator,
    item_table: &ItemTable,
) -> std::io::Result<Item> {
    let row = item_table
        .get(item.item_id)
        .ok_or_else(|| std::io::Error::other("ITEM_NOT_FOUND"))?;
    Ok(Item::from_config(
        item_uid_generator.next()?,
        item.item_id,
        item.count,
        item.binded,
        Some(character_id),
        row,
        item_table,
    ))
}

fn current_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default()
}

async fn push_item_obtain_if_online(
    room_manager: &SharedRoomManager,
    character_id: &str,
    items: &[Item],
    source: &str,
) -> Result<(), std::io::Error> {
    room_manager
        .send_to_character(
            character_id,
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
    character_id: &str,
    player_data: &crate::core::inventory::PlayerData,
) -> Result<(), std::io::Error> {
    room_manager
        .send_to_character(
            character_id,
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
            runtime.drain_mode_reason = if parsed {
                normalized_drain_metadata(
                    &runtime.drain_mode_reason,
                    DEFAULT_DRAIN_MODE_REASON,
                    "INVALID_DRAIN_MODE_REASON",
                )?
            } else {
                DEFAULT_DRAIN_MODE_REASON.to_string()
            };
            runtime.drain_mode_source = if parsed {
                normalized_drain_metadata(
                    &runtime.drain_mode_source,
                    DEFAULT_DRAIN_MODE_SOURCE,
                    "INVALID_DRAIN_MODE_SOURCE",
                )?
            } else {
                DEFAULT_DRAIN_MODE_SOURCE.to_string()
            };

            if previous != parsed {
                info!(
                    drain_mode_enabled = parsed,
                    drain_mode_entered_at_ms = ?runtime.drain_mode_entered_at_ms,
                    drain_mode_reason = %runtime.drain_mode_reason,
                    drain_mode_source = %runtime.drain_mode_source,
                    "game-server drain mode updated"
                );
            }
            Ok(())
        }
        "drain_mode_reason" => {
            runtime.drain_mode_reason = normalized_drain_metadata(
                value,
                DEFAULT_DRAIN_MODE_REASON,
                "INVALID_DRAIN_MODE_REASON",
            )?;
            Ok(())
        }
        "drain_mode_source" => {
            runtime.drain_mode_source = normalized_drain_metadata(
                value,
                DEFAULT_DRAIN_MODE_SOURCE,
                "INVALID_DRAIN_MODE_SOURCE",
            )?;
            Ok(())
        }
        _ => Err("UNSUPPORTED_CONFIG_KEY"),
    }
}

fn normalized_drain_metadata(
    value: &str,
    default_value: &str,
    too_long_error: &'static str,
) -> Result<String, &'static str> {
    let normalized = value.trim();
    if normalized.len() > 128 {
        return Err(too_long_error);
    }
    if normalized.is_empty() {
        Ok(default_value.to_string())
    } else {
        Ok(normalized.to_string())
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

#[cfg(test)]
mod tests;
