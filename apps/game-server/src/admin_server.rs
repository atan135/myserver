use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

use crate::admin_pb::{ServerStatusReq, ServerStatusRes, UpdateConfigReq, UpdateConfigRes};
use crate::core::config_table::ConfigTableRuntime;
use crate::core::context::{
    PlayerRegistry, SharedRoomManager, SharedRuntimeConfig, ShutdownSignal,
};
use crate::core::global_id::ItemUidGenerator;
use crate::core::player::PlayerManager;
use crate::core::runtime::room_manager::RolloutDrainNotice;
use crate::pb::{
    ConfirmRoomOwnershipReq, ConfirmRoomOwnershipRes, ExportRoomTransferReq, ExportRoomTransferRes,
    FreezeRoomForTransferReq, FreezeRoomForTransferRes, GetRolloutDrainStatusReq,
    ImportRoomTransferReq, ImportRoomTransferRes, RequestServerShutdownReq,
    RetireTransferredRoomReq, RetireTransferredRoomRes, ServerRedirectPush,
    TriggerRolloutDrainNoticeReq, TriggerRolloutDrainNoticeRes, TriggerServerRedirectReq,
    TriggerServerRedirectRes,
};
use crate::protocol::MessageType;
use crate::server::RuntimeConfig;

mod audit;
mod auth;
mod gm;
mod protocol_io;
mod rollout_status;
mod runtime_config;

pub use audit::{AdminAuditConfig, AdminAuditLogger};

use audit::{
    AdminAuditTarget, audit_then_write_error, audit_then_write_message, ensure_admin_write_allowed,
};
use auth::authenticate_admin_packet;
use gm::{
    handle_gm_ban_player, handle_gm_broadcast, handle_gm_kick_player, handle_gm_send_item,
    handle_grant_items_result_query,
};
use protocol_io::{read_packet, write_error, write_message};
use rollout_status::{build_rollout_drain_status_response, build_server_shutdown_response};
use runtime_config::apply_runtime_config;

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
                handle_gm_send_item(
                    &mut writer,
                    &audit_logger,
                    &auth_context,
                    &packet,
                    &room_manager,
                    &player_manager,
                    &config_tables,
                    &item_uid_generator,
                )
                .await?;
            }
            Some(MessageType::GrantItemsResultQueryReq) => {
                handle_grant_items_result_query(&mut writer, &packet, &player_manager).await?;
            }
            Some(MessageType::GmBroadcastReq) => {
                handle_gm_broadcast(
                    &mut writer,
                    &audit_logger,
                    &auth_context,
                    &packet,
                    &player_registry,
                )
                .await?;
            }
            Some(MessageType::GmKickPlayerReq) => {
                handle_gm_kick_player(
                    &mut writer,
                    &audit_logger,
                    &auth_context,
                    &packet,
                    &player_registry,
                )
                .await?;
            }
            Some(MessageType::GmBanPlayerReq) => {
                handle_gm_ban_player(
                    &mut writer,
                    &audit_logger,
                    &auth_context,
                    &packet,
                    &player_registry,
                )
                .await?;
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

fn current_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or_default()
}

fn current_unix_ms_u64() -> u64 {
    current_unix_ms().max(0) as u64
}

#[cfg(test)]
mod tests;
