use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, timeout};
use tracing::warn;

use crate::admin_pb::{
    GrantItem, GrantItemsReq, GrantItemsRes, ServerStatusReq, ServerStatusRes, UpdateConfigReq,
    UpdateConfigRes,
};
use crate::core::config_table::ConfigTableRuntime;
use crate::core::context::{SharedRoomManager, SharedRuntimeConfig};
use crate::core::inventory::Item;
use crate::core::player::PlayerManager;
use crate::pb::{ErrorRes, InventoryUpdatePush, Item as PbItem, ItemObtainPush};
use crate::protocol::{HEADER_LEN, MessageType, Packet, encode_body, encode_packet, parse_header};
use crate::server::RuntimeConfig;

const ADMIN_MAX_BODY_LEN: usize = 64 * 1024;
static ITEM_UID_COUNTER: AtomicU64 = AtomicU64::new(0);

pub async fn run_listener(
    listener: TcpListener,
    room_manager: SharedRoomManager,
    runtime_config: SharedRuntimeConfig,
    connection_count: Arc<AtomicU64>,
    player_manager: PlayerManager,
    config_tables: ConfigTableRuntime,
) -> Result<(), std::io::Error> {
    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let room_manager = room_manager.clone();
        let runtime_config = runtime_config.clone();
        let connection_count = connection_count.clone();
        let player_manager = player_manager.clone();
        let config_tables = config_tables.clone();

        tokio::spawn(async move {
            if let Err(error) =
                handle_admin_connection(
                    socket,
                    room_manager,
                    runtime_config,
                    connection_count,
                    player_manager,
                    config_tables,
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
    player_manager: PlayerManager,
    config_tables: ConfigTableRuntime,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut reader, mut writer) = socket.into_split();

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
                let RuntimeConfig {
                    heartbeat_timeout_secs,
                    max_body_len,
                } = *runtime_config.read().await;

                write_message(
                    &mut writer,
                    MessageType::AdminServerStatusRes,
                    packet.header.seq,
                    &ServerStatusRes {
                        connection_count: connection_count.load(Ordering::Relaxed),
                        room_count,
                        status: "ok".to_string(),
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
                let result = apply_runtime_config(&runtime_config, &request.key, &request.value).await;

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
            Some(_) => {
                write_error(&mut writer, packet.header.seq, "MESSAGE_NOT_SUPPORTED", "message not supported on admin channel").await?;
            }
            None => {
                write_error(&mut writer, packet.header.seq, "UNKNOWN_MESSAGE_TYPE", "unknown message type").await?;
            }
        }
    }

    Ok(())
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

    let tables = config_tables.snapshot().await;
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

fn decode_grant_items_request(packet: &Packet) -> Result<GrantItemsReq, Box<dyn std::error::Error>> {
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
        items.into_iter()
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
            .ok_or_else(|| std::io::Error::other("INVALID_ITEM_ID"))? as i32),
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
        _ => Err("UNSUPPORTED_CONFIG_KEY"),
    }
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
