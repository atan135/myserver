use interprocess::local_socket::tokio::Listener;
use interprocess::local_socket::traits::tokio::Listener as _;
use std::sync::atomic::Ordering;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use crate::core::context::ServiceContext;
use crate::core::runtime::room_manager::ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT;
use crate::core::service::room_service;
use crate::pb::{
    ConfirmRoomOwnershipReq, ConfirmRoomOwnershipRes, CreateMatchedRoomReq, ErrorRes,
    ExportRoomTransferReq, ExportRoomTransferRes, FreezeRoomForTransferReq,
    FreezeRoomForTransferRes, GetRolloutDrainStatusReq, GetRolloutDrainStatusRes,
    ImportRoomTransferReq, ImportRoomTransferRes, RetireTransferredRoomReq,
    RetireTransferredRoomRes, ServerRedirectPush, TriggerServerRedirectReq,
    TriggerServerRedirectRes,
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
                let runtime = *services.runtime_config.read().await;

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
                        connection_count: services.connection_count.load(Ordering::Relaxed),
                        routes: snapshot.routes,
                        drain_mode_enabled: runtime.drain_mode_enabled,
                        drain_mode_entered_at_ms: runtime.drain_mode_entered_at_ms.unwrap_or(0),
                    },
                )
                .await?;
            }
            Some(MessageType::TriggerServerRedirectReq) => {
                let request = packet
                    .decode_body::<TriggerServerRedirectReq>("INVALID_TRIGGER_REDIRECT_BODY")
                    .map_err(std::io::Error::other)?;
                let target = InternalAuditTarget {
                    room_id: request.room_id.clone(),
                    rollout_epoch: request.rollout_epoch.clone(),
                    checksum: String::new(),
                    target_server_id: request.target_server_id.clone(),
                };

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
