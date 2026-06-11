use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::json;
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
use crate::core::room::OutboundMessage;
use crate::pb::{ErrorRes, GameMessagePush, InventoryUpdatePush, Item as PbItem, ItemObtainPush};
use crate::pb::{
    ExportRoomTransferReq, ExportRoomTransferRes, FreezeRoomForTransferReq,
    FreezeRoomForTransferRes, ImportRoomTransferReq, ImportRoomTransferRes,
    RetireTransferredRoomReq, RetireTransferredRoomRes, ServerRedirectPush,
    TriggerServerRedirectReq, TriggerServerRedirectRes,
};
use crate::protocol::{HEADER_LEN, MessageType, Packet, encode_body, encode_packet, parse_header};
use crate::server::RuntimeConfig;

const ADMIN_MAX_BODY_LEN: usize = 64 * 1024;
const GM_BROADCAST_TITLE_MAX_LEN: usize = 128;
const GM_BROADCAST_CONTENT_MAX_LEN: usize = 4096;
const GM_SENDER_MAX_LEN: usize = 64;
const GM_REASON_MAX_LEN: usize = 512;
const GM_PLAYER_ID_MAX_LEN: usize = 128;
const GM_BAN_DURATION_MAX_SECONDS: u64 = 31_536_000;
static ITEM_UID_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, PartialEq, prost::Message)]
struct GmCommandRes {
    #[prost(bool, tag = "1")]
    ok: bool,
    #[prost(string, tag = "2")]
    error_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GmBroadcastCommand {
    title: String,
    content: String,
    sender: String,
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

pub async fn run_listener(
    listener: TcpListener,
    room_manager: SharedRoomManager,
    runtime_config: SharedRuntimeConfig,
    connection_count: Arc<AtomicU64>,
    player_registry: PlayerRegistry,
    player_manager: PlayerManager,
    config_tables: ConfigTableRuntime,
    admin_token: String,
) -> Result<(), std::io::Error> {
    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let room_manager = room_manager.clone();
        let runtime_config = runtime_config.clone();
        let connection_count = connection_count.clone();
        let player_registry = player_registry.clone();
        let player_manager = player_manager.clone();
        let config_tables = config_tables.clone();
        let admin_token = admin_token.clone();

        tokio::spawn(async move {
            if let Err(error) = handle_admin_connection(
                socket,
                room_manager,
                runtime_config,
                connection_count,
                player_registry,
                player_manager,
                config_tables,
                admin_token,
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
    admin_token: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut reader, mut writer) = socket.into_split();

    let Some(auth_packet) = read_packet(&mut reader).await? else {
        return Ok(());
    };
    if !authenticate_admin_packet(&auth_packet, &admin_token) {
        write_error(
            &mut writer,
            auth_packet.header.seq,
            "UNAUTHORIZED_ADMIN",
            "invalid admin token",
        )
        .await?;
        return Ok(());
    }

    loop {
        let Some(packet) = read_packet(&mut reader).await? else {
            break;
        };

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
                let request = packet
                    .decode_body::<UpdateConfigReq>("INVALID_ADMIN_UPDATE_CONFIG_BODY")
                    .map_err(std::io::Error::other)?;
                let result =
                    apply_runtime_config(&runtime_config, &request.key, &request.value).await;

                write_message(
                    &mut writer,
                    MessageType::AdminUpdateConfigRes,
                    packet.header.seq,
                    &UpdateConfigRes {
                        ok: result.is_ok(),
                        error_code: result.err().unwrap_or_default().to_string(),
                    },
                )
                .await?;
            }
            Some(MessageType::GmSendItemReq) => {
                let request = decode_grant_items_request(&packet)?;
                validate_grant_items_request(&request, &config_tables).await?;
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

                        write_message(
                            &mut writer,
                            MessageType::GmSendItemRes,
                            packet.header.seq,
                            &GrantItemsRes {
                                ok: true,
                                error_code: String::new(),
                                applied: outcome.applied,
                            },
                        )
                        .await?;
                    }
                    Err(error_code) => {
                        write_error(
                            &mut writer,
                            packet.header.seq,
                            &error_code,
                            "failed to grant items",
                        )
                        .await?;
                    }
                }
            }
            Some(MessageType::GmBroadcastReq) => match decode_gm_broadcast_request(&packet) {
                Ok(request) => {
                    let delivered =
                        broadcast_gm_message_to_online_players(&player_registry, &request).await;
                    info!(
                        delivered = delivered,
                        sender = %request.sender,
                        title = %request.title,
                        "gm broadcast delivered"
                    );
                    write_gm_response(&mut writer, MessageType::GmBroadcastRes, packet.header.seq)
                        .await?;
                }
                Err(error_code) => {
                    write_error(
                        &mut writer,
                        packet.header.seq,
                        error_code,
                        "invalid gm broadcast request",
                    )
                    .await?;
                }
            },
            Some(MessageType::GmKickPlayerReq) => match decode_gm_kick_player_request(&packet) {
                Ok(request) => {
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
                            write_gm_response(
                                &mut writer,
                                MessageType::GmKickPlayerRes,
                                packet.header.seq,
                            )
                            .await?;
                        }
                        Err(error_code) => {
                            write_error(
                                &mut writer,
                                packet.header.seq,
                                error_code,
                                "failed to kick player",
                            )
                            .await?;
                        }
                    }
                }
                Err(error_code) => {
                    write_error(
                        &mut writer,
                        packet.header.seq,
                        error_code,
                        "invalid gm kick player request",
                    )
                    .await?;
                }
            },
            Some(MessageType::GmBanPlayerReq) => match decode_gm_ban_player_request(&packet) {
                Ok(request) => {
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
                            write_gm_response(
                                &mut writer,
                                MessageType::GmBanPlayerRes,
                                packet.header.seq,
                            )
                            .await?;
                        }
                        Err(error_code) => {
                            write_error(
                                &mut writer,
                                packet.header.seq,
                                error_code,
                                "failed to ban player on this game-server",
                            )
                            .await?;
                        }
                    }
                }
                Err(error_code) => {
                    write_error(
                        &mut writer,
                        packet.header.seq,
                        error_code,
                        "invalid gm ban player request",
                    )
                    .await?;
                }
            },
            Some(MessageType::FreezeRoomForTransferReq) => {
                let request = packet
                    .decode_body::<FreezeRoomForTransferReq>("INVALID_FREEZE_ROOM_TRANSFER_BODY")
                    .map_err(std::io::Error::other)?;

                let result = room_manager
                    .freeze_room_for_transfer(&request.rollout_epoch, &request.room_id)
                    .await;

                let (ok, error_code, migration_state, room_version) = match result {
                    Ok((migration_state, room_version)) => {
                        (true, String::new(), migration_state as i32, room_version)
                    }
                    Err(error_code) => (false, error_code.to_string(), 0, 0),
                };

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

                let result = room_manager
                    .export_room_transfer(&request.rollout_epoch, &request.room_id)
                    .await;

                let response = match result {
                    Ok(payload) => ExportRoomTransferRes {
                        ok: true,
                        room_id: request.room_id,
                        error_code: String::new(),
                        checksum: payload.checksum.clone(),
                        payload: Some(payload),
                    },
                    Err(error_code) => ExportRoomTransferRes {
                        ok: false,
                        room_id: request.room_id,
                        error_code: error_code.to_string(),
                        checksum: String::new(),
                        payload: None,
                    },
                };

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
                    Ok((room_id, checksum, room_version)) => ImportRoomTransferRes {
                        ok: true,
                        room_id,
                        error_code: String::new(),
                        checksum,
                        room_version,
                    },
                    Err(error_code) => ImportRoomTransferRes {
                        ok: false,
                        room_id: String::new(),
                        error_code: error_code.to_string(),
                        checksum: String::new(),
                        room_version: 0,
                    },
                };

                write_message(
                    &mut writer,
                    MessageType::ImportRoomTransferRes,
                    packet.header.seq,
                    &response,
                )
                .await?;
            }
            Some(MessageType::RetireTransferredRoomReq) => {
                let request = packet
                    .decode_body::<RetireTransferredRoomReq>("INVALID_RETIRE_ROOM_TRANSFER_BODY")
                    .map_err(std::io::Error::other)?;

                let result = room_manager
                    .retire_transferred_room(
                        &request.rollout_epoch,
                        &request.room_id,
                        &request.checksum,
                    )
                    .await;

                write_message(
                    &mut writer,
                    MessageType::RetireTransferredRoomRes,
                    packet.header.seq,
                    &RetireTransferredRoomRes {
                        ok: result.is_ok(),
                        room_id: request.room_id,
                        error_code: result.err().unwrap_or_default().to_string(),
                    },
                )
                .await?;
            }
            Some(MessageType::TriggerServerRedirectReq) => {
                let request = packet
                    .decode_body::<TriggerServerRedirectReq>("INVALID_TRIGGER_REDIRECT_BODY")
                    .map_err(std::io::Error::other)?;

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

fn authenticate_admin_packet(packet: &Packet, admin_token: &str) -> bool {
    if packet.message_type() != Some(MessageType::AdminAuthReq) {
        return false;
    }

    std::str::from_utf8(&packet.body)
        .map(|token| token == admin_token)
        .unwrap_or(false)
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

fn normalize_required_string(
    value: Option<String>,
    invalid_code: &'static str,
    max_chars: usize,
    too_long_code: &'static str,
) -> Result<String, &'static str> {
    let value = value.ok_or(invalid_code)?;
    let value = value.trim();
    if value.is_empty() {
        return Err(invalid_code);
    }
    if value.chars().count() > max_chars {
        return Err(too_long_code);
    }
    Ok(value.to_string())
}

fn normalize_optional_string(
    value: Option<String>,
    default_value: &str,
    max_chars: usize,
    too_long_code: &'static str,
) -> Result<String, &'static str> {
    let value = value.unwrap_or_else(|| default_value.to_string());
    let value = value.trim();
    if value.chars().count() > max_chars {
        return Err(too_long_code);
    }
    if value.is_empty() {
        Ok(default_value.to_string())
    } else {
        Ok(value.to_string())
    }
}

fn gm_disconnect_reason(default_reason: &str, request_reason: &str) -> String {
    if request_reason.is_empty() {
        default_reason.to_string()
    } else {
        request_reason.to_string()
    }
}

async fn broadcast_gm_message_to_online_players(
    player_registry: &PlayerRegistry,
    request: &GmBroadcastCommand,
) -> usize {
    let handles = {
        let registry = player_registry.read().await;
        registry
            .iter()
            .map(|(player_id, handle)| (player_id.clone(), handle.outbound.clone()))
            .collect::<Vec<_>>()
    };

    let body = encode_body(&GameMessagePush {
        event: "gm_broadcast".to_string(),
        room_id: String::new(),
        player_id: String::new(),
        action: "broadcast".to_string(),
        payload_json: json!({
            "title": request.title,
            "content": request.content,
            "sender": request.sender,
            "timestamp": current_unix_ms()
        })
        .to_string(),
    });

    let mut delivered = 0;
    for (player_id, outbound) in handles {
        match outbound.try_send(OutboundMessage {
            message_type: MessageType::GameMessagePush,
            seq: 0,
            body: body.clone(),
        }) {
            Ok(()) => delivered += 1,
            Err(error) => {
                warn!(
                    player_id = %player_id,
                    error = %error,
                    "failed to queue gm broadcast"
                );
            }
        }
    }

    delivered
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

async fn write_gm_response(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    message_type: MessageType,
    seq: u32,
) -> Result<(), std::io::Error> {
    write_message(
        writer,
        message_type,
        seq,
        &GmCommandRes {
            ok: true,
            error_code: String::new(),
        },
    )
    .await
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
    use std::sync::Arc;

    use tokio::sync::Notify;
    use tokio::sync::RwLock;
    use tokio::sync::mpsc;

    use super::*;
    use crate::core::context::PlayerConnectionHandle;
    use crate::protocol::PacketHeader;

    fn runtime_config_fixture() -> SharedRuntimeConfig {
        Arc::new(RwLock::new(RuntimeConfig {
            heartbeat_timeout_secs: 30,
            max_body_len: 4096,
            msg_rate_window_ms: 1000,
            msg_rate_max: 0,
            player_msg_rate_window_ms: 1000,
            player_msg_rate_max: 0,
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
                outbound: tx,
                kick_reason: kick_reason.clone(),
            },
        )])));

        (registry, notify, kick_reason, rx)
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
