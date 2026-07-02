use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tokio::net::tcp::OwnedWriteHalf;
use tracing::{info, warn};

use super::audit::{
    AdminAuditLogger, AdminAuditTarget, audit_then_write_error, audit_then_write_message,
};
use super::auth::AdminAuthContext;
use crate::admin_pb::{GrantItem, GrantItemsReq, GrantItemsRes};
use crate::core::config_table::ConfigTableRuntime;
use crate::core::context::{PlayerRegistry, SharedRoomManager};
use crate::core::global_id::ItemUidGenerator;
use crate::core::inventory::Item;
use crate::core::player::PlayerManager;
use crate::csv_code::itemtable::ItemTable;
use crate::gm_broadcast::{
    GM_BROADCAST_CONTENT_MAX_LEN, GM_BROADCAST_TITLE_MAX_LEN, GM_SENDER_MAX_LEN,
    GmBroadcastCommand, broadcast_gm_message_to_online_players, normalize_optional_string,
    normalize_required_string,
};
use crate::pb::{InventoryUpdatePush, Item as PbItem, ItemObtainPush};
use crate::protocol::{MessageType, Packet, encode_body};

const GM_REASON_MAX_LEN: usize = 512;
const GM_PLAYER_ID_MAX_LEN: usize = 128;
const GM_CHARACTER_ID_MAX_LEN: usize = 128;
const GM_BAN_DURATION_MAX_SECONDS: u64 = 31_536_000;

#[derive(Clone, PartialEq, prost::Message)]
struct GmCommandRes {
    #[prost(bool, tag = "1")]
    ok: bool,
    #[prost(string, tag = "2")]
    error_code: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GmKickPlayerCommand {
    pub(super) player_id: String,
    pub(super) reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GmBanPlayerCommand {
    pub(super) player_id: String,
    pub(super) duration_seconds: u64,
    pub(super) reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct KickOnlineOutcome {
    pub(super) player_id: String,
    pub(super) character_id: String,
    pub(super) session_id: u64,
}

pub(super) async fn handle_gm_send_item(
    writer: &mut OwnedWriteHalf,
    audit_logger: &AdminAuditLogger,
    auth_context: &AdminAuthContext,
    packet: &Packet,
    room_manager: &SharedRoomManager,
    player_manager: &PlayerManager,
    config_tables: &ConfigTableRuntime,
    item_uid_generator: &ItemUidGenerator,
) -> Result<(), std::io::Error> {
    let action = "gm_send_item";
    let request = match decode_grant_items_request(packet).map_err(|error| error.to_string()) {
        Ok(request) => request,
        Err(error_code) => {
            audit_then_write_error(
                writer,
                audit_logger,
                auth_context,
                packet,
                action,
                &error_code,
                "invalid grant items request",
                &AdminAuditTarget::default(),
            )
            .await?;
            return Ok(());
        }
    };
    let target = AdminAuditTarget {
        character_id: request.character_id.clone(),
        ..Default::default()
    };
    if let Err(error_code) = validate_grant_items_request(&request, config_tables)
        .await
        .map_err(|error| error.to_string())
    {
        audit_then_write_error(
            writer,
            audit_logger,
            auth_context,
            packet,
            action,
            &error_code,
            "invalid grant items request",
            &target,
        )
        .await?;
        return Ok(());
    }
    let tables = config_tables.tables_snapshot().await;
    let items = match request
        .items
        .iter()
        .map(|item| {
            grant_item_to_inventory_item(
                item,
                &request.character_id,
                item_uid_generator,
                &tables.item_table,
            )
        })
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(items) => items,
        Err(error) => {
            warn!(error = %error, "failed to generate global item uid for GM grant");
            audit_then_write_error(
                writer,
                audit_logger,
                auth_context,
                packet,
                action,
                "GLOBAL_ID_GENERATE_FAILED",
                "failed to generate item uid",
                &target,
            )
            .await?;
            return Ok(());
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
                    room_manager,
                    &request.character_id,
                    &items,
                    &request.source,
                )
                .await;
                let _ = push_inventory_update_if_online(
                    room_manager,
                    &request.character_id,
                    &outcome.player_data,
                )
                .await;
            }

            audit_then_write_message(
                writer,
                audit_logger,
                auth_context,
                packet,
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
            .await
        }
        Err(error_code) => {
            audit_then_write_error(
                writer,
                audit_logger,
                auth_context,
                packet,
                action,
                &error_code,
                "failed to grant items",
                &target,
            )
            .await
        }
    }
}

pub(super) async fn handle_gm_broadcast(
    writer: &mut OwnedWriteHalf,
    audit_logger: &AdminAuditLogger,
    auth_context: &AdminAuthContext,
    packet: &Packet,
    player_registry: &PlayerRegistry,
) -> Result<(), std::io::Error> {
    let action = "gm_broadcast";
    match decode_gm_broadcast_request(packet) {
        Ok(request) => {
            let delivered = broadcast_gm_message_to_online_players(player_registry, &request).await;
            info!(
                delivered = delivered,
                sender = %request.sender,
                title = %request.title,
                "gm broadcast delivered"
            );
            audit_then_write_message(
                writer,
                audit_logger,
                auth_context,
                packet,
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
            .await
        }
        Err(error_code) => {
            audit_then_write_error(
                writer,
                audit_logger,
                auth_context,
                packet,
                action,
                error_code,
                "invalid gm broadcast request",
                &AdminAuditTarget::default(),
            )
            .await
        }
    }
}

pub(super) async fn handle_gm_kick_player(
    writer: &mut OwnedWriteHalf,
    audit_logger: &AdminAuditLogger,
    auth_context: &AdminAuthContext,
    packet: &Packet,
    player_registry: &PlayerRegistry,
) -> Result<(), std::io::Error> {
    let action = "gm_kick_player";
    match decode_gm_kick_player_request(packet) {
        Ok(request) => {
            let target = AdminAuditTarget {
                player_id: request.player_id.clone(),
                ..Default::default()
            };
            let kick_reason = gm_disconnect_reason("gm_kick", &request.reason);
            match kick_online_player(player_registry, &request.player_id, &kick_reason).await {
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
                        writer,
                        audit_logger,
                        auth_context,
                        packet,
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
                    .await
                }
                Err(error_code) => {
                    audit_then_write_error(
                        writer,
                        audit_logger,
                        auth_context,
                        packet,
                        action,
                        error_code,
                        "failed to kick player",
                        &target,
                    )
                    .await
                }
            }
        }
        Err(error_code) => {
            audit_then_write_error(
                writer,
                audit_logger,
                auth_context,
                packet,
                action,
                error_code,
                "invalid gm kick player request",
                &AdminAuditTarget::default(),
            )
            .await
        }
    }
}

pub(super) async fn handle_gm_ban_player(
    writer: &mut OwnedWriteHalf,
    audit_logger: &AdminAuditLogger,
    auth_context: &AdminAuthContext,
    packet: &Packet,
    player_registry: &PlayerRegistry,
) -> Result<(), std::io::Error> {
    let action = "gm_ban_player";
    match decode_gm_ban_player_request(packet) {
        Ok(request) => {
            let target = AdminAuditTarget {
                player_id: request.player_id.clone(),
                ..Default::default()
            };
            let kick_reason = gm_disconnect_reason("gm_ban", &request.reason);
            match kick_online_player(player_registry, &request.player_id, &kick_reason).await {
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
                        writer,
                        audit_logger,
                        auth_context,
                        packet,
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
                    .await
                }
                Err(error_code) => {
                    audit_then_write_error(
                        writer,
                        audit_logger,
                        auth_context,
                        packet,
                        action,
                        error_code,
                        "failed to ban player on this game-server",
                        &target,
                    )
                    .await
                }
            }
        }
        Err(error_code) => {
            audit_then_write_error(
                writer,
                audit_logger,
                auth_context,
                packet,
                action,
                error_code,
                "invalid gm ban player request",
                &AdminAuditTarget::default(),
            )
            .await
        }
    }
}

pub(super) fn decode_gm_broadcast_request(
    packet: &Packet,
) -> Result<GmBroadcastCommand, &'static str> {
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

pub(super) fn decode_gm_kick_player_request(
    packet: &Packet,
) -> Result<GmKickPlayerCommand, &'static str> {
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

pub(super) fn decode_gm_ban_player_request(
    packet: &Packet,
) -> Result<GmBanPlayerCommand, &'static str> {
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

pub(super) fn gm_disconnect_reason(default_reason: &str, request_reason: &str) -> String {
    if request_reason.is_empty() {
        default_reason.to_string()
    } else {
        request_reason.to_string()
    }
}

pub(super) async fn kick_online_player(
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

pub(super) fn decode_grant_items_request(
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

pub(super) fn grant_item_to_inventory_item(
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
