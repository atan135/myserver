use interprocess::local_socket::tokio::Listener;
use interprocess::local_socket::traits::tokio::Listener as _;
use std::sync::atomic::Ordering;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use crate::core::context::ServiceContext;
use crate::core::runtime::room_manager::{
    ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT, RolloutDrainNotice,
};
use crate::core::service::room_service;
use crate::pb::{
    ConfirmRoomOwnershipReq, ConfirmRoomOwnershipRes, CreateMatchedRoomReq, ErrorRes,
    ExportRoomTransferReq, ExportRoomTransferRes, FreezeRoomForTransferReq,
    FreezeRoomForTransferRes, GetRolloutDrainStatusReq, GetRolloutDrainStatusRes,
    ImportRoomTransferReq, ImportRoomTransferRes, RequestServerShutdownReq,
    RequestServerShutdownRes, RetireTransferredRoomReq, RetireTransferredRoomRes,
    ServerRedirectPush, TriggerRolloutDrainNoticeReq, TriggerRolloutDrainNoticeRes,
    TriggerServerRedirectReq, TriggerServerRedirectRes,
};
use crate::protocol::{HEADER_LEN, MessageType, Packet, encode_body, encode_packet, parse_header};

const INTERNAL_MAX_BODY_LEN: usize = 64 * 1024;

pub async fn run_listener(
    listener: Listener,
    services: ServiceContext,
    internal_token: String,
) -> Result<(), std::io::Error> {
    let mut next_connection_id = 2_000_000u64;

    loop {
        let socket = listener.accept().await?;
        let services = services.clone();
        let internal_token = internal_token.clone();
        let connection_id = next_connection_id;
        next_connection_id = next_connection_id.saturating_add(1);

        tokio::spawn(async move {
            if let Err(error) = handle_internal_connection(socket, services, internal_token).await {
                warn!(
                    connection_id = connection_id,
                    error = %error,
                    "internal matched-room connection failed"
                );
            }
        });
    }
}

async fn handle_internal_connection<S>(
    socket: S,
    services: ServiceContext,
    internal_token: String,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(socket);

    let Some(auth_packet) = read_packet(&mut reader).await? else {
        return Ok(());
    };

    if !authenticate_internal_packet(&auth_packet, &internal_token) {
        write_error(
            &mut writer,
            auth_packet.header.seq,
            "UNAUTHORIZED_INTERNAL_CHANNEL",
            "invalid internal channel token",
        )
        .await?;
        return Ok(());
    }

    loop {
        let Some(packet) = read_packet(&mut reader).await? else {
            break;
        };

        match packet.message_type() {
            Some(MessageType::CreateMatchedRoomReq) => {
                let request = packet
                    .decode_body::<CreateMatchedRoomReq>("INVALID_CREATE_MATCHED_ROOM_BODY")
                    .map_err(std::io::Error::other)?;

                let response =
                    room_service::handle_create_matched_room_internal(&services, request).await;

                write_message(
                    &mut writer,
                    MessageType::CreateMatchedRoomRes,
                    packet.header.seq,
                    &response,
                )
                .await?;
            }
            Some(MessageType::FreezeRoomForTransferReq) => {
                let request = packet
                    .decode_body::<FreezeRoomForTransferReq>("INVALID_FREEZE_ROOM_TRANSFER_BODY")
                    .map_err(std::io::Error::other)?;
                let target = InternalAuditTarget::for_room_transfer(
                    &request.room_id,
                    &request.rollout_epoch,
                );

                let result = services
                    .room_manager
                    .freeze_room_for_transfer(&request.rollout_epoch, &request.room_id)
                    .await;

                let (ok, error_code, migration_state, room_version) = match result {
                    Ok((migration_state, room_version)) => {
                        (true, String::new(), migration_state as i32, room_version)
                    }
                    Err(error_code) => (false, error_code.to_string(), 0, 0),
                };
                audit_internal_control_action(
                    packet.header.seq,
                    packet.header.msg_type,
                    "freeze_room_for_transfer",
                    ok,
                    &error_code,
                    &target,
                );

                write_message(
                    &mut writer,
                    MessageType::FreezeRoomForTransferRes,
                    packet.header.seq,
                    &FreezeRoomForTransferRes {
                        ok,
                        room_id: request.room_id,
                        error_code,
                        migration_state,
                        room_version,
                    },
                )
                .await?;
            }
            Some(MessageType::ExportRoomTransferReq) => {
                let request = packet
                    .decode_body::<ExportRoomTransferReq>("INVALID_EXPORT_ROOM_TRANSFER_BODY")
                    .map_err(std::io::Error::other)?;
                let mut target = InternalAuditTarget::for_room_transfer(
                    &request.room_id,
                    &request.rollout_epoch,
                );

                let result = services
                    .room_manager
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
                audit_internal_control_action(
                    packet.header.seq,
                    packet.header.msg_type,
                    "export_room_transfer",
                    response.ok,
                    &response.error_code,
                    &target,
                );

                write_message(
                    &mut writer,
                    MessageType::ExportRoomTransferRes,
                    packet.header.seq,
                    &response,
                )
                .await?;
            }
            Some(MessageType::ImportRoomTransferReq) => {
                let request = packet
                    .decode_body::<ImportRoomTransferReq>("INVALID_IMPORT_ROOM_TRANSFER_BODY")
                    .map_err(std::io::Error::other)?;
                let mut target = InternalAuditTarget::for_import_transfer(&request);

                let result = match request.payload {
                    Some(payload) => {
                        let room_id = payload.room_id.clone();
                        services
                            .room_manager
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
                audit_internal_control_action(
                    packet.header.seq,
                    packet.header.msg_type,
                    "import_room_transfer",
                    response.ok,
                    &response.error_code,
                    &target,
                );

                write_message(
                    &mut writer,
                    MessageType::ImportRoomTransferRes,
                    packet.header.seq,
                    &response,
                )
                .await?;
            }
            Some(MessageType::ConfirmRoomOwnershipReq) => {
                let request = packet
                    .decode_body::<ConfirmRoomOwnershipReq>("INVALID_CONFIRM_ROOM_OWNERSHIP_BODY")
                    .map_err(std::io::Error::other)?;
                let target = InternalAuditTarget {
                    room_id: request.room_id.clone(),
                    rollout_epoch: request.rollout_epoch.clone(),
                    checksum: request.checksum.clone(),
                    target_server_id: String::new(),
                };

                let result = services
                    .room_manager
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
                audit_internal_control_action(
                    packet.header.seq,
                    packet.header.msg_type,
                    "confirm_room_ownership",
                    response.ok,
                    &response.error_code,
                    &target,
                );

                write_message(
                    &mut writer,
                    MessageType::ConfirmRoomOwnershipRes,
                    packet.header.seq,
                    &response,
                )
                .await?;
            }
            Some(MessageType::RetireTransferredRoomReq) => {
                let request = packet
                    .decode_body::<RetireTransferredRoomReq>("INVALID_RETIRE_ROOM_TRANSFER_BODY")
                    .map_err(std::io::Error::other)?;
                let target = InternalAuditTarget {
                    room_id: request.room_id.clone(),
                    rollout_epoch: request.rollout_epoch.clone(),
                    checksum: request.checksum.clone(),
                    target_server_id: String::new(),
                };

                let result = services
                    .room_manager
                    .retire_transferred_room(
                        &request.rollout_epoch,
                        &request.room_id,
                        &request.checksum,
                    )
                    .await;
                let ok = result.is_ok();
                let error_code = result.err().unwrap_or_default().to_string();
                audit_internal_control_action(
                    packet.header.seq,
                    packet.header.msg_type,
                    "retire_transferred_room",
                    ok,
                    &error_code,
                    &target,
                );

                write_message(
                    &mut writer,
                    MessageType::RetireTransferredRoomRes,
                    packet.header.seq,
                    &RetireTransferredRoomRes {
                        ok,
                        room_id: request.room_id,
                        error_code,
                    },
                )
                .await?;
            }
            Some(MessageType::GetRolloutDrainStatusReq) => {
                packet
                    .decode_body::<GetRolloutDrainStatusReq>(
                        "INVALID_GET_ROLLOUT_DRAIN_STATUS_BODY",
                    )
                    .map_err(std::io::Error::other)?;

                let snapshot = services
                    .room_manager
                    .rollout_drain_snapshot(
                        &services.config.service_instance_id,
                        ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT,
                    )
                    .await;
                let runtime = services.runtime_config.read().await.clone();
                let connection_count = services.connection_count.load(Ordering::Relaxed);

                if connection_count == 0
                    && snapshot.owned_room_count == 0
                    && snapshot.migrating_room_count == 0
                {
                    info!(
                        channel = "internal_socket",
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

                write_message(
                    &mut writer,
                    MessageType::GetRolloutDrainStatusRes,
                    packet.header.seq,
                    &GetRolloutDrainStatusRes {
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
                    },
                )
                .await?;
            }
            Some(MessageType::TriggerServerRedirectReq) => {
                let request = match packet
                    .decode_body::<TriggerServerRedirectReq>("INVALID_TRIGGER_REDIRECT_BODY")
                {
                    Ok(request) => request,
                    Err(error_code) => {
                        let target = InternalAuditTarget::default();
                        audit_internal_control_action(
                            packet.header.seq,
                            packet.header.msg_type,
                            "trigger_server_redirect",
                            false,
                            error_code,
                            &target,
                        );
                        write_error(
                            &mut writer,
                            packet.header.seq,
                            error_code,
                            "invalid trigger redirect request",
                        )
                        .await?;
                        continue;
                    }
                };
                let target = InternalAuditTarget::for_redirect(&request);

                let room_id = request.room_id.clone();
                let result = services
                    .room_manager
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
                audit_internal_control_action(
                    packet.header.seq,
                    packet.header.msg_type,
                    "trigger_server_redirect",
                    response.ok,
                    &response.error_code,
                    &target,
                );

                write_message(
                    &mut writer,
                    MessageType::TriggerServerRedirectRes,
                    packet.header.seq,
                    &response,
                )
                .await?;
            }
            Some(MessageType::TriggerRolloutDrainNoticeReq) => {
                let request = match packet.decode_body::<TriggerRolloutDrainNoticeReq>(
                    "INVALID_ROLLOUT_DRAIN_NOTICE_BODY",
                ) {
                    Ok(request) => request,
                    Err(error_code) => {
                        let target = InternalAuditTarget::default();
                        audit_internal_control_action(
                            packet.header.seq,
                            packet.header.msg_type,
                            "trigger_rollout_drain_notice",
                            false,
                            error_code,
                            &target,
                        );
                        write_error(
                            &mut writer,
                            packet.header.seq,
                            error_code,
                            "invalid rollout drain notice request",
                        )
                        .await?;
                        continue;
                    }
                };
                let target = InternalAuditTarget::for_rollout_drain_notice(&request);
                let room_id = request.room_id.clone();
                let result = services
                    .room_manager
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
                info!(
                    channel = "internal_socket",
                    action = "trigger_rollout_drain_notice",
                    room_id = %response.room_id,
                    rollout_epoch = %target.rollout_epoch,
                    delivered_count = response.delivered_count,
                    failed_count = response.failed_count,
                    online_member_count = response.online_member_count,
                    ok = response.ok,
                    error_code = %response.error_code,
                    "rollout drain notice internal trigger result"
                );
                audit_internal_control_action(
                    packet.header.seq,
                    packet.header.msg_type,
                    "trigger_rollout_drain_notice",
                    response.ok,
                    &response.error_code,
                    &target,
                );

                write_message(
                    &mut writer,
                    MessageType::TriggerRolloutDrainNoticeRes,
                    packet.header.seq,
                    &response,
                )
                .await?;
            }
            Some(MessageType::RequestServerShutdownReq) => {
                let request = match packet
                    .decode_body::<RequestServerShutdownReq>("INVALID_SERVER_SHUTDOWN_BODY")
                {
                    Ok(request) => request,
                    Err(error_code) => {
                        let target = InternalAuditTarget::default();
                        audit_internal_control_action(
                            packet.header.seq,
                            packet.header.msg_type,
                            "request_server_shutdown",
                            false,
                            error_code,
                            &target,
                        );
                        write_error(
                            &mut writer,
                            packet.header.seq,
                            error_code,
                            "invalid server shutdown request",
                        )
                        .await?;
                        continue;
                    }
                };
                let target = InternalAuditTarget::for_shutdown();
                let response = build_server_shutdown_response(&services).await;
                let ok = response.ok;
                let error_code = response.error_code.clone();
                audit_internal_control_action(
                    packet.header.seq,
                    packet.header.msg_type,
                    "request_server_shutdown",
                    ok,
                    &error_code,
                    &target,
                );
                write_message(
                    &mut writer,
                    MessageType::RequestServerShutdownRes,
                    packet.header.seq,
                    &response,
                )
                .await?;

                if ok {
                    info!(
                        channel = "internal_socket",
                        reason = %request.reason,
                        connection_count = response.connection_count,
                        owned_room_count = response.owned_room_count,
                        migrating_room_count = response.migrating_room_count,
                        retired_room_count = response.retired_room_count,
                        "requesting game-server graceful shutdown"
                    );
                    services.shutdown_signal.notify_one();
                }
            }
            Some(_) => {
                write_error(
                    &mut writer,
                    packet.header.seq,
                    "MESSAGE_NOT_SUPPORTED",
                    "message not supported on internal channel",
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

fn authenticate_internal_packet(packet: &Packet, internal_token: &str) -> bool {
    if packet.message_type() != Some(MessageType::InternalAuthReq) {
        return false;
    }

    std::str::from_utf8(&packet.body)
        .map(|token| token == internal_token)
        .unwrap_or(false)
}

#[derive(Default)]
struct InternalAuditTarget {
    room_id: String,
    rollout_epoch: String,
    checksum: String,
    target_server_id: String,
}

impl InternalAuditTarget {
    fn for_room_transfer(room_id: &str, rollout_epoch: &str) -> Self {
        Self {
            room_id: room_id.to_string(),
            rollout_epoch: rollout_epoch.to_string(),
            ..Default::default()
        }
    }

    fn for_import_transfer(request: &ImportRoomTransferReq) -> Self {
        let Some(payload) = request.payload.as_ref() else {
            return Self::default();
        };

        Self {
            room_id: payload.room_id.clone(),
            rollout_epoch: payload.rollout_epoch.clone(),
            checksum: payload.checksum.clone(),
            target_server_id: String::new(),
        }
    }

    fn for_redirect(request: &TriggerServerRedirectReq) -> Self {
        Self {
            room_id: request.room_id.clone(),
            rollout_epoch: request.rollout_epoch.clone(),
            checksum: String::new(),
            target_server_id: request.target_server_id.clone(),
        }
    }

    fn for_rollout_drain_notice(request: &TriggerRolloutDrainNoticeReq) -> Self {
        Self {
            room_id: request.room_id.clone(),
            rollout_epoch: request.rollout_epoch.clone(),
            checksum: String::new(),
            target_server_id: String::new(),
        }
    }

    fn for_shutdown() -> Self {
        Self::default()
    }
}

pub async fn build_server_shutdown_response(services: &ServiceContext) -> RequestServerShutdownRes {
    let snapshot = services
        .room_manager
        .rollout_drain_snapshot(
            &services.config.service_instance_id,
            ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT,
        )
        .await;
    let runtime = services.runtime_config.read().await.clone();
    let connection_count = services.connection_count.load(Ordering::Relaxed);

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

fn audit_internal_control_action(
    seq: u32,
    message_type: u16,
    action: &'static str,
    ok: bool,
    error_code: &str,
    target: &InternalAuditTarget,
) {
    if ok {
        info!(
            channel = "internal_socket",
            actor = "internal_service",
            action,
            ok,
            error_code,
            room_id = %target.room_id,
            rollout_epoch = %target.rollout_epoch,
            checksum = %target.checksum,
            target_server_id = %target.target_server_id,
            seq,
            message_type,
            "game-server internal control action"
        );
    } else {
        warn!(
            channel = "internal_socket",
            actor = "internal_service",
            action,
            ok,
            error_code,
            room_id = %target.room_id,
            rollout_epoch = %target.rollout_epoch,
            checksum = %target.checksum,
            target_server_id = %target.target_server_id,
            seq,
            message_type,
            "game-server internal control action failed"
        );
    }
}

async fn read_packet<R>(reader: &mut R) -> Result<Option<Packet>, Box<dyn std::error::Error>>
where
    R: AsyncRead + Unpin,
{
    let read_header = timeout(Duration::from_secs(10), read_header_bytes(reader)).await;
    let header_buf = match read_header {
        Ok(Ok(Some(header_buf))) => header_buf,
        Ok(Ok(None)) => return Ok(None),
        Ok(Err(error)) => return Err(Box::new(error)),
        Err(_) => {
            return Err(Box::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "internal read timeout",
            )));
        }
    };

    let header = parse_header(header_buf).map_err(std::io::Error::other)?;
    if header.body_len as usize > INTERNAL_MAX_BODY_LEN {
        return Err(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "body too large",
        )));
    }

    let mut body = vec![0u8; header.body_len as usize];
    reader.read_exact(&mut body).await?;
    Ok(Some(Packet::new(header, body)))
}

async fn read_header_bytes<R>(reader: &mut R) -> Result<Option<[u8; HEADER_LEN]>, std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut header_buf = [0u8; HEADER_LEN];
    match reader.read_exact(&mut header_buf).await {
        Ok(_) => Ok(Some(header_buf)),
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error),
    }
}

async fn write_message<W, M>(
    writer: &mut W,
    message_type: MessageType,
    seq: u32,
    message: &M,
) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
    M: prost::Message,
{
    let body = encode_body(message);
    let packet = encode_packet(message_type, seq, &body);
    writer.write_all(&packet).await
}

async fn write_error<W>(
    writer: &mut W,
    seq: u32,
    error_code: &str,
    message: &str,
) -> Result<(), std::io::Error>
where
    W: AsyncWrite + Unpin,
{
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
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    use prost::Message;
    use tokio::io::duplex;
    use tokio::sync::{Mutex, Notify, RwLock};

    use super::*;
    use crate::config::{
        Config, DEFAULT_ADMIN_TOKEN, DEFAULT_INTERNAL_TOKEN, DEFAULT_OUTBOUND_QUEUE_CAPACITY,
        DEFAULT_TICKET_SECRET,
    };
    use crate::core::config_table::ConfigTableRuntime;
    use crate::core::context::PlayerRegistry;
    use crate::core::logic::{RoomLogic, RoomLogicFactory, RoomLogicTransfer};
    use crate::core::player::{PgPlayerStore, PlayerManager};
    use crate::core::room::MemberRole;
    use crate::core::runtime::RoomManager;
    use crate::db_store::PgAuditStore;
    use crate::protocol::{encode_packet, parse_header};
    use crate::server::{
        DEFAULT_DRAIN_MODE_REASON, DEFAULT_DRAIN_MODE_SOURCE, PlayerInputAnomalyTracker,
        PlayerMessageRateLimiter, RuntimeConfig,
    };

    struct NoopRoomLogic;

    impl RoomLogic for NoopRoomLogic {}

    impl RoomLogicTransfer for NoopRoomLogic {}

    struct NoopRoomLogicFactory;

    impl RoomLogicFactory for NoopRoomLogicFactory {
        fn create(&self, _policy_id: &str) -> Box<dyn RoomLogic> {
            Box::new(NoopRoomLogic)
        }
    }

    fn test_config() -> Config {
        Config {
            host: "127.0.0.1".to_string(),
            port: 7000,
            csv_dir: "csv".to_string(),
            csv_reload_enabled: false,
            csv_reload_interval_secs: 3,
            room_cleanup_interval_secs: 3600,
            admin_host: "127.0.0.1".to_string(),
            admin_port: 7500,
            admin_token: DEFAULT_ADMIN_TOKEN.to_string(),
            admin_audit_enabled: false,
            admin_audit_path: "logs/game-server/admin-audit.jsonl".to_string(),
            admin_audit_require_actor: false,
            internal_token: DEFAULT_INTERNAL_TOKEN.to_string(),
            local_socket_name: "test-game-server.sock".to_string(),
            internal_socket_name: "test-game-server-internal.sock".to_string(),
            log_level: "info".to_string(),
            log_enable_console: false,
            log_enable_file: false,
            log_dir: "logs/game-server".to_string(),
            redis_url: "redis://127.0.0.1:6379".to_string(),
            redis_key_prefix: String::new(),
            nats_url: "nats://127.0.0.1:4222".to_string(),
            db_enabled: false,
            database_url: "postgres://postgres:password@127.0.0.1:5432/myserver_game".to_string(),
            db_pool_size: 1,
            ticket_secret: DEFAULT_TICKET_SECRET.to_string(),
            heartbeat_timeout_secs: 30,
            max_body_len: 4096,
            outbound_queue_capacity: DEFAULT_OUTBOUND_QUEUE_CAPACITY,
            msg_rate_window_ms: 1000,
            msg_rate_max: 0,
            player_msg_rate_window_ms: 1000,
            player_msg_rate_max: 0,
            input_timestamp_required: false,
            input_timestamp_max_skew_ms: 5000,
            input_anomaly_window_ms: 10_000,
            input_anomaly_max: 0,
            registry_enabled: false,
            registry_url: "redis://127.0.0.1:6379".to_string(),
            registry_heartbeat_interval_secs: 10,
            service_name: "game-server".to_string(),
            service_instance_id: "game-server-test".to_string(),
        }
    }

    fn runtime_config() -> RuntimeConfig {
        RuntimeConfig {
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
            drain_mode_reason: DEFAULT_DRAIN_MODE_REASON.to_string(),
            drain_mode_source: DEFAULT_DRAIN_MODE_SOURCE.to_string(),
        }
    }

    async fn service_context_fixture() -> ServiceContext {
        let config = test_config();
        let config_tables = ConfigTableRuntime::load(std::path::Path::new(&config.csv_dir))
            .expect("test config tables should load");
        let room_manager = Arc::new(RoomManager::with_policy_registry_and_cleanup_interval(
            crate::match_client::create_match_client_shared(),
            Arc::new(NoopRoomLogicFactory),
            config_tables.room_policy_registry(),
            3600,
        ));

        ServiceContext {
            config: config.clone(),
            db_store: PgAuditStore::new(&config)
                .await
                .expect("disabled PostgreSQL audit store"),
            room_manager,
            runtime_config: Arc::new(RwLock::new(runtime_config())),
            connection_count: Arc::new(AtomicU64::new(0)),
            config_tables,
            player_manager: PlayerManager::new(PgPlayerStore::new_disabled()),
            online_player_count: Arc::new(AtomicU64::new(0)),
            player_registry: PlayerRegistry::default(),
            player_msg_rate_limiter: Arc::new(Mutex::new(PlayerMessageRateLimiter::new())),
            player_input_anomaly_tracker: Arc::new(Mutex::new(PlayerInputAnomalyTracker::new())),
            shutdown_signal: Arc::new(Notify::new()),
        }
    }

    async fn read_test_packet<R>(reader: &mut R) -> Packet
    where
        R: AsyncRead + Unpin,
    {
        let mut header_buf = [0u8; HEADER_LEN];
        reader.read_exact(&mut header_buf).await.unwrap();
        let header = parse_header(header_buf).unwrap();
        let mut body = vec![0u8; header.body_len as usize];
        reader.read_exact(&mut body).await.unwrap();
        Packet::new(header, body)
    }

    #[test]
    fn internal_redirect_audit_target_includes_room_epoch_and_target_server() {
        let target = InternalAuditTarget::for_redirect(&TriggerServerRedirectReq {
            room_id: "room-1".to_string(),
            rollout_epoch: "epoch-7".to_string(),
            reason: "rollout".to_string(),
            retry_after_ms: 250,
            target_host: "127.0.0.1".to_string(),
            target_port: 4000,
            target_server_id: "game-server-new".to_string(),
            transport: "kcp".to_string(),
        });

        assert_eq!(target.room_id, "room-1");
        assert_eq!(target.rollout_epoch, "epoch-7");
        assert_eq!(target.target_server_id, "game-server-new");
        assert_eq!(target.checksum, "");
    }

    #[test]
    fn internal_rollout_drain_notice_audit_target_includes_room_and_epoch() {
        let target = InternalAuditTarget::for_rollout_drain_notice(&TriggerRolloutDrainNoticeReq {
            room_id: "room-1".to_string(),
            rollout_epoch: "epoch-7".to_string(),
            reason: "rollout".to_string(),
            message: "Please leave after this round".to_string(),
            retry_after_ms: 500,
            deadline_ms: 123_456,
        });

        assert_eq!(target.room_id, "room-1");
        assert_eq!(target.rollout_epoch, "epoch-7");
        assert_eq!(target.target_server_id, "");
        assert_eq!(target.checksum, "");
    }

    #[tokio::test]
    async fn invalid_internal_redirect_body_returns_error_response_after_audit() {
        let services = service_context_fixture().await;
        let (server_io, mut client_io) = duplex(4096);
        let handler = tokio::spawn(async move {
            handle_internal_connection(server_io, services, DEFAULT_INTERNAL_TOKEN.to_string())
                .await
                .map_err(|error| error.to_string())
        });

        client_io
            .write_all(&encode_packet(
                MessageType::InternalAuthReq,
                1,
                DEFAULT_INTERNAL_TOKEN.as_bytes(),
            ))
            .await
            .unwrap();
        client_io
            .write_all(&encode_packet(
                MessageType::TriggerServerRedirectReq,
                2,
                b"not-protobuf",
            ))
            .await
            .unwrap();

        let packet = read_test_packet(&mut client_io).await;
        assert_eq!(packet.header.msg_type, MessageType::ErrorRes as u16);
        assert_eq!(packet.header.seq, 2);
        let response = ErrorRes::decode(packet.body.as_slice()).unwrap();
        assert_eq!(response.error_code, "INVALID_TRIGGER_REDIRECT_BODY");
        assert_eq!(response.message, "invalid trigger redirect request");

        drop(client_io);
        handler.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn invalid_internal_rollout_drain_notice_body_returns_error_response_after_audit() {
        let services = service_context_fixture().await;
        let (server_io, mut client_io) = duplex(4096);
        let handler = tokio::spawn(async move {
            handle_internal_connection(server_io, services, DEFAULT_INTERNAL_TOKEN.to_string())
                .await
                .map_err(|error| error.to_string())
        });

        client_io
            .write_all(&encode_packet(
                MessageType::InternalAuthReq,
                1,
                DEFAULT_INTERNAL_TOKEN.as_bytes(),
            ))
            .await
            .unwrap();
        client_io
            .write_all(&encode_packet(
                MessageType::TriggerRolloutDrainNoticeReq,
                2,
                b"not-protobuf",
            ))
            .await
            .unwrap();

        let packet = read_test_packet(&mut client_io).await;
        assert_eq!(packet.header.msg_type, MessageType::ErrorRes as u16);
        assert_eq!(packet.header.seq, 2);
        let response = ErrorRes::decode(packet.body.as_slice()).unwrap();
        assert_eq!(response.error_code, "INVALID_ROLLOUT_DRAIN_NOTICE_BODY");
        assert_eq!(response.message, "invalid rollout drain notice request");

        drop(client_io);
        handler.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn shutdown_request_rejects_when_drain_mode_is_disabled() {
        let services = service_context_fixture().await;

        let response = build_server_shutdown_response(&services).await;

        assert!(!response.ok);
        assert_eq!(response.error_code, "SHUTDOWN_DRAIN_MODE_REQUIRED");
        assert!(!response.drain_mode_enabled);
        assert_eq!(response.connection_count, 0);
        assert_eq!(response.owned_room_count, 0);
        assert_eq!(response.migrating_room_count, 0);
    }

    #[tokio::test]
    async fn shutdown_request_rejects_when_connections_remain() {
        let services = service_context_fixture().await;
        services.runtime_config.write().await.drain_mode_enabled = true;
        services.connection_count.store(1, Ordering::Relaxed);

        let response = build_server_shutdown_response(&services).await;

        assert!(!response.ok);
        assert_eq!(response.error_code, "SHUTDOWN_CONNECTIONS_REMAIN");
        assert_eq!(response.connection_count, 1);
    }

    #[tokio::test]
    async fn shutdown_request_rejects_when_owned_room_remains() {
        let services = service_context_fixture().await;
        services.runtime_config.write().await.drain_mode_enabled = true;
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        services
            .room_manager
            .join_room(
                "room-owned",
                "player-a",
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();

        let response = build_server_shutdown_response(&services).await;

        assert!(!response.ok);
        assert_eq!(response.error_code, "SHUTDOWN_OWNED_ROOMS_REMAIN");
        assert_eq!(response.owned_room_count, 1);
    }

    #[tokio::test]
    async fn shutdown_request_rejects_when_migrating_room_remains() {
        let services = service_context_fixture().await;
        services.runtime_config.write().await.drain_mode_enabled = true;
        let (tx, _rx) = tokio::sync::mpsc::channel(8);
        services
            .room_manager
            .join_room(
                "room-migrating",
                "player-a",
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();
        services
            .room_manager
            .disconnect_room_member("room-migrating", "player-a")
            .await;
        services
            .room_manager
            .freeze_room_for_transfer("epoch-1", "room-migrating")
            .await
            .unwrap();

        let response = build_server_shutdown_response(&services).await;

        assert!(!response.ok);
        assert_eq!(response.error_code, "SHUTDOWN_MIGRATING_ROOMS_REMAIN");
        assert_eq!(response.migrating_room_count, 1);
    }

    #[tokio::test]
    async fn internal_shutdown_request_writes_success_then_triggers_signal() {
        let services = service_context_fixture().await;
        services.runtime_config.write().await.drain_mode_enabled = true;
        let shutdown_signal = services.shutdown_signal.clone();
        let (server_io, mut client_io) = duplex(4096);
        let handler = tokio::spawn(async move {
            handle_internal_connection(server_io, services, DEFAULT_INTERNAL_TOKEN.to_string())
                .await
                .map_err(|error| error.to_string())
        });

        client_io
            .write_all(&encode_packet(
                MessageType::InternalAuthReq,
                1,
                DEFAULT_INTERNAL_TOKEN.as_bytes(),
            ))
            .await
            .unwrap();
        client_io
            .write_all(&encode_packet(
                MessageType::RequestServerShutdownReq,
                2,
                &encode_body(&RequestServerShutdownReq {
                    reason: "unit-test".to_string(),
                }),
            ))
            .await
            .unwrap();

        let packet = read_test_packet(&mut client_io).await;
        assert_eq!(
            packet.header.msg_type,
            MessageType::RequestServerShutdownRes as u16
        );
        assert_eq!(packet.header.seq, 2);
        let response = RequestServerShutdownRes::decode(packet.body.as_slice()).unwrap();
        assert!(response.ok);
        assert!(response.error_code.is_empty());
        tokio::time::timeout(Duration::from_millis(50), shutdown_signal.notified())
            .await
            .expect("shutdown signal should be triggered after success response");

        drop(client_io);
        handler.await.unwrap().unwrap();
    }
}
