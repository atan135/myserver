use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use tokio::net::tcp::OwnedWriteHalf;
use tracing::{info, warn};

use super::audit::{
    AdminAuditLogger, AdminAuditTarget, audit_then_write_error, audit_then_write_message,
};
use super::auth::AdminAuthContext;
use super::protocol_io::write_message;
use crate::admin_pb::{
    GrantItem, GrantItemsReq, GrantItemsRes, GrantItemsResultQueryReq, GrantItemsResultQueryRes,
    GrantItemsResultSummary,
};
use crate::core::config_table::ConfigTableRuntime;
use crate::core::context::{PlayerRegistry, SharedRoomManager};
use crate::core::global_id::ItemUidGenerator;
use crate::core::inventory::Item;
use crate::core::player::PlayerManager;
use crate::core::player::db_player_store::{GrantRecord, GrantRecordLookup};
use crate::core::player::grant_contract::{
    GrantItemIntent, GrantResultSummary, compute_grant_fingerprint, normalize_grant_items,
};
use crate::core::player::player_manager::GrantItemsError;
use crate::csv_code::itemtable::ItemTable;
use crate::gm_broadcast::{
    GM_BROADCAST_CONTENT_MAX_LEN, GM_BROADCAST_TITLE_MAX_LEN, GM_SENDER_MAX_LEN,
    GmBroadcastCommand, broadcast_gm_message_to_online_players, normalize_optional_string,
    normalize_required_string,
};
use crate::metrics::METRICS;
use crate::pb::{InventoryUpdatePush, Item as PbItem, ItemObtainPush};
use crate::protocol::{MessageType, Packet, encode_body};

const GM_REASON_MAX_LEN: usize = 512;
const GM_PLAYER_ID_MAX_LEN: usize = 128;
const GM_CHARACTER_ID_MAX_LEN: usize = 128;
const GRANT_REQUEST_ID_MAX_LEN: usize = 128;
const GRANT_MAIL_ID_MAX_LEN: usize = 64;
const GRANT_SOURCE_MAX_LEN: usize = 64;
const GRANT_TRACE_ID_LEN: usize = 32;
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
        Err(_) => {
            let failure = invalid_grant_request("INVALID_GRANT_ITEMS_BODY");
            audit_then_write_message(
                writer,
                audit_logger,
                auth_context,
                packet,
                action,
                MessageType::GmSendItemRes,
                &grant_failure_response("", "", "", &failure),
                false,
                failure.error_code,
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
    let validated = match validate_grant_items_request(&request, config_tables).await {
        Ok(validated) => validated,
        Err(failure) => {
            audit_then_write_message(
                writer,
                audit_logger,
                auth_context,
                packet,
                action,
                MessageType::GmSendItemRes,
                &grant_failure_response(
                    &request.request_id,
                    &request.request_fingerprint,
                    &request.trace_id,
                    &failure,
                ),
                false,
                failure.error_code,
                &target,
            )
            .await?;
            return Ok(());
        }
    };
    let tables = config_tables.tables_snapshot().await;
    let result_summary = GrantResultSummary {
        character_id: request.character_id.clone(),
        source: request.source.clone(),
        items: validated.items.clone(),
    };

    let result = player_manager
        .grant_items_with_request(
            &request.character_id,
            &request.request_id,
            &validated.request_fingerprint,
            &request.source,
            &request.reason,
            result_summary,
            || {
                validated
                    .items
                    .iter()
                    .map(|item| {
                        grant_item_to_inventory_item(
                            &GrantItem {
                                item_id: item.item_id,
                                count: item.count,
                                binded: item.binded,
                            },
                            &request.character_id,
                            item_uid_generator,
                            &tables.item_table,
                        )
                        .map_err(|error| {
                            warn!(error = %error, "failed to build inventory item for grant");
                            if error.to_string() == "ITEM_NOT_FOUND" {
                                permanent_grant_failure("ITEM_NOT_FOUND")
                            } else {
                                retryable_grant_failure("GLOBAL_ID_GENERATE_FAILED")
                            }
                        })
                    })
                    .collect()
            },
        )
        .await;

    match result {
        Ok(outcome) => {
            if request.source == "mail-claim" {
                if outcome.applied {
                    METRICS.record_inventory_grant_first_success();
                } else {
                    METRICS.record_inventory_grant_idempotent_hit();
                }
            }
            let mut push_failed = false;
            if outcome.applied {
                let obtain_result = push_item_obtain_if_online(
                    room_manager,
                    &request.character_id,
                    &outcome.granted_items,
                    &request.source,
                )
                .await;
                let inventory_result = if let Some(player_data) = &outcome.player_data {
                    push_inventory_update_if_online(
                        room_manager,
                        &request.character_id,
                        player_data,
                    )
                    .await
                } else {
                    Ok(())
                };
                if obtain_result.is_err() || inventory_result.is_err() {
                    push_failed = true;
                    METRICS.record_inventory_grant_push_failure();
                    warn!(
                        request_id = %request.request_id,
                        character_id = %request.character_id,
                        "inventory grant committed but online push failed"
                    );
                }
            }

            let audit_result = if push_failed {
                "ONLINE_PUSH_FAILED_AFTER_COMMIT"
            } else if outcome.applied {
                ""
            } else {
                "IDEMPOTENT_REPLAY"
            };
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
                    request_id: outcome.record.request_id.clone(),
                    request_fingerprint: outcome.record.request_fingerprint.clone(),
                    error_category: String::new(),
                    result_state: "applied".to_string(),
                    retryable: false,
                    result_summary: Some(grant_result_summary_to_proto(
                        &outcome.record.result_summary,
                    )),
                    trace_id: request.trace_id.clone(),
                },
                true,
                audit_result,
                &target,
            )
            .await
        }
        Err(failure) => {
            if request.source == "mail-claim" {
                if failure.error_code == "REQUEST_FINGERPRINT_CONFLICT" {
                    METRICS.record_inventory_grant_fingerprint_conflict();
                } else if matches!(
                    failure.error_code,
                    "INVENTORY_TRANSACTION_FAILED" | "INVENTORY_COMMIT_RESULT_UNKNOWN"
                ) {
                    METRICS.record_inventory_grant_transaction_failure();
                }
            }
            audit_then_write_message(
                writer,
                audit_logger,
                auth_context,
                packet,
                action,
                MessageType::GmSendItemRes,
                &grant_failure_response(
                    &request.request_id,
                    &validated.request_fingerprint,
                    &request.trace_id,
                    &failure,
                ),
                false,
                failure.error_code,
                &target,
            )
            .await
        }
    }
}

pub(super) async fn handle_grant_items_result_query(
    writer: &mut OwnedWriteHalf,
    packet: &Packet,
    player_manager: &PlayerManager,
) -> Result<(), std::io::Error> {
    let request = match decode_grant_items_result_query(packet).map_err(|error| error.to_string()) {
        Ok(request) => request,
        Err(_) => {
            return write_message(
                writer,
                MessageType::GrantItemsResultQueryRes,
                packet.header.seq,
                &grant_query_failure_response(
                    "",
                    "",
                    "",
                    "INVALID_REQUEST_ID",
                    "INVALID_REQUEST",
                    "not_applied",
                    false,
                ),
            )
            .await;
        }
    };

    if !is_trimmed_nonempty_with_max_bytes(&request.request_id, GRANT_REQUEST_ID_MAX_LEN)
        || (!request.request_fingerprint.is_empty()
            && !is_lower_sha256(&request.request_fingerprint))
        || (!request.trace_id.is_empty() && !is_lower_hex(&request.trace_id, GRANT_TRACE_ID_LEN))
    {
        return write_message(
            writer,
            MessageType::GrantItemsResultQueryRes,
            packet.header.seq,
            &grant_query_failure_response(
                &request.request_id,
                &request.request_fingerprint,
                &request.trace_id,
                "INVALID_GRANT_RESULT_QUERY",
                "INVALID_REQUEST",
                "not_applied",
                false,
            ),
        )
        .await;
    }

    let response = match player_manager.find_grant_record(&request.request_id).await {
        Ok(GrantRecordLookup::NotFound) => GrantItemsResultQueryRes {
            ok: true,
            query_status: "not_seen".to_string(),
            request_id: request.request_id.clone(),
            request_fingerprint: request.request_fingerprint.clone(),
            error_code: String::new(),
            error_category: String::new(),
            result_state: "not_applied".to_string(),
            retryable: false,
            result_summary: None,
            trace_id: request.trace_id.clone(),
            created_at_ms: 0,
        },
        Ok(GrantRecordLookup::Succeeded(record))
            if !request.request_fingerprint.is_empty()
                && request.request_fingerprint != record.request_fingerprint =>
        {
            grant_query_failure_response(
                &request.request_id,
                &record.request_fingerprint,
                &request.trace_id,
                "REQUEST_FINGERPRINT_CONFLICT",
                "PERMANENT_FAILURE",
                "not_applied",
                false,
            )
        }
        Ok(GrantRecordLookup::Succeeded(record)) => {
            grant_query_success_response(&record, &request.trace_id)
        }
        Ok(GrantRecordLookup::ResultUnavailable) => grant_query_failure_response(
            &request.request_id,
            &request.request_fingerprint,
            &request.trace_id,
            "GRANT_RESULT_UNAVAILABLE",
            "RESULT_UNKNOWN",
            "unknown",
            false,
        ),
        Err(error) => {
            warn!(
                request_id = %request.request_id,
                error = %error,
                "failed to query inventory grant result"
            );
            grant_query_failure_response(
                &request.request_id,
                &request.request_fingerprint,
                &request.trace_id,
                "GRANT_RESULT_QUERY_FAILED",
                "RESULT_UNKNOWN",
                "unknown",
                true,
            )
        }
    };

    write_message(
        writer,
        MessageType::GrantItemsResultQueryRes,
        packet.header.seq,
        &response,
    )
    .await
}

fn decode_grant_items_result_query(
    packet: &Packet,
) -> Result<GrantItemsResultQueryReq, Box<dyn std::error::Error>> {
    if let Ok(request) =
        packet.decode_body::<GrantItemsResultQueryReq>("INVALID_GRANT_RESULT_QUERY_BODY")
    {
        if !request.request_id.is_empty() {
            return Ok(request);
        }
    }

    #[derive(Deserialize)]
    struct QueryJson {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "requestFingerprint", default)]
        request_fingerprint: String,
        #[serde(rename = "traceId", default)]
        trace_id: String,
    }

    let request: QueryJson = serde_json::from_slice(&packet.body)?;
    Ok(GrantItemsResultQueryReq {
        request_id: request.request_id,
        request_fingerprint: request.request_fingerprint,
        trace_id: request.trace_id,
    })
}

fn grant_failure_response(
    request_id: &str,
    request_fingerprint: &str,
    trace_id: &str,
    failure: &GrantItemsError,
) -> GrantItemsRes {
    GrantItemsRes {
        ok: false,
        error_code: failure.error_code.to_string(),
        applied: false,
        request_id: request_id.to_string(),
        request_fingerprint: request_fingerprint.to_string(),
        error_category: failure.error_category.to_string(),
        result_state: failure.result_state.to_string(),
        retryable: failure.retryable,
        result_summary: None,
        trace_id: trace_id.to_string(),
    }
}

fn grant_query_success_response(record: &GrantRecord, trace_id: &str) -> GrantItemsResultQueryRes {
    GrantItemsResultQueryRes {
        ok: true,
        query_status: "succeeded".to_string(),
        request_id: record.request_id.clone(),
        request_fingerprint: record.request_fingerprint.clone(),
        error_code: String::new(),
        error_category: String::new(),
        result_state: "applied".to_string(),
        retryable: false,
        result_summary: Some(grant_result_summary_to_proto(&record.result_summary)),
        trace_id: trace_id.to_string(),
        created_at_ms: record.created_at_ms,
    }
}

#[allow(clippy::too_many_arguments)]
fn grant_query_failure_response(
    request_id: &str,
    request_fingerprint: &str,
    trace_id: &str,
    error_code: &str,
    error_category: &str,
    result_state: &str,
    retryable: bool,
) -> GrantItemsResultQueryRes {
    GrantItemsResultQueryRes {
        ok: false,
        query_status: if error_code == "REQUEST_FINGERPRINT_CONFLICT" {
            "conflict".to_string()
        } else {
            "result_unavailable".to_string()
        },
        request_id: request_id.to_string(),
        request_fingerprint: request_fingerprint.to_string(),
        error_code: error_code.to_string(),
        error_category: error_category.to_string(),
        result_state: result_state.to_string(),
        retryable,
        result_summary: None,
        trace_id: trace_id.to_string(),
        created_at_ms: 0,
    }
}

fn grant_result_summary_to_proto(summary: &GrantResultSummary) -> GrantItemsResultSummary {
    GrantItemsResultSummary {
        character_id: summary.character_id.clone(),
        source: summary.source.clone(),
        items: summary
            .items
            .iter()
            .map(|item| GrantItem {
                item_id: item.item_id,
                count: item.count,
                binded: item.binded,
            })
            .collect(),
    }
}

fn invalid_grant_request(error_code: &'static str) -> GrantItemsError {
    GrantItemsError {
        error_code,
        error_category: "INVALID_REQUEST",
        result_state: "not_applied",
        retryable: false,
    }
}

fn permanent_grant_failure(error_code: &'static str) -> GrantItemsError {
    GrantItemsError {
        error_code,
        error_category: "PERMANENT_FAILURE",
        result_state: "not_applied",
        retryable: false,
    }
}

fn retryable_grant_failure(error_code: &'static str) -> GrantItemsError {
    GrantItemsError {
        error_code,
        error_category: "RETRYABLE_FAILURE",
        result_state: "not_applied",
        retryable: true,
    }
}

fn is_trimmed_nonempty_with_max_bytes(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn is_lower_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_lower_sha256(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|digest| is_lower_hex(digest, 64))
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

#[derive(Debug, Clone)]
struct ValidatedGrantItemsRequest {
    items: Vec<GrantItemIntent>,
    request_fingerprint: String,
}

async fn validate_grant_items_request(
    request: &GrantItemsReq,
    config_tables: &ConfigTableRuntime,
) -> Result<ValidatedGrantItemsRequest, GrantItemsError> {
    if !is_trimmed_nonempty_with_max_bytes(&request.character_id, GM_CHARACTER_ID_MAX_LEN) {
        return Err(invalid_grant_request("INVALID_CHARACTER_ID"));
    }
    if !is_trimmed_nonempty_with_max_bytes(&request.request_id, GRANT_REQUEST_ID_MAX_LEN) {
        return Err(invalid_grant_request("INVALID_REQUEST_ID"));
    }
    if !is_trimmed_nonempty_with_max_bytes(&request.source, GRANT_SOURCE_MAX_LEN) {
        return Err(invalid_grant_request("INVALID_SOURCE"));
    }
    if request.reason.len() > GM_REASON_MAX_LEN {
        return Err(invalid_grant_request("REASON_TOO_LONG"));
    }
    if !request.trace_id.is_empty() && !is_lower_hex(&request.trace_id, GRANT_TRACE_ID_LEN) {
        return Err(invalid_grant_request("INVALID_TRACE_ID"));
    }

    let intents = request
        .items
        .iter()
        .map(|item| GrantItemIntent {
            item_id: item.item_id,
            count: item.count,
            binded: item.binded,
        })
        .collect::<Vec<_>>();
    let items = normalize_grant_items(&intents).map_err(invalid_grant_request)?;
    let is_mail_claim = request.source == "mail-claim"
        || request.request_id.starts_with("mail_claim:")
        || !request.mail_id.is_empty();
    let mail_id = if is_mail_claim {
        if request.source != "mail-claim" {
            return Err(invalid_grant_request("INVALID_SOURCE"));
        }
        if !is_trimmed_nonempty_with_max_bytes(&request.mail_id, GRANT_MAIL_ID_MAX_LEN) {
            return Err(invalid_grant_request("INVALID_MAIL_ID"));
        }
        if request.request_id.strip_prefix("mail_claim:") != Some(request.mail_id.as_str()) {
            return Err(invalid_grant_request("INVALID_REQUEST_ID"));
        }
        if !is_lower_sha256(&request.request_fingerprint) {
            return Err(invalid_grant_request("INVALID_FINGERPRINT"));
        }
        if !is_lower_hex(&request.trace_id, GRANT_TRACE_ID_LEN) {
            return Err(invalid_grant_request("INVALID_TRACE_ID"));
        }
        request.mail_id.as_str()
    } else {
        ""
    };

    let request_fingerprint =
        compute_grant_fingerprint(mail_id, &request.character_id, &request.source, &items)
            .map_err(|_| retryable_grant_failure("FINGERPRINT_COMPUTE_FAILED"))?;
    if !request.request_fingerprint.is_empty() && request.request_fingerprint != request_fingerprint
    {
        return Err(invalid_grant_request("INVALID_FINGERPRINT"));
    }

    let tables = config_tables.tables_snapshot().await;
    for item in &items {
        if tables.item_table.get(item.item_id).is_none() {
            return Err(permanent_grant_failure("ITEM_NOT_FOUND"));
        }
    }

    Ok(ValidatedGrantItemsRequest {
        items,
        request_fingerprint,
    })
}

pub(super) fn decode_grant_items_request(
    packet: &Packet,
) -> Result<GrantItemsReq, Box<dyn std::error::Error>> {
    if let Ok(mut request) = packet.decode_body::<GrantItemsReq>("INVALID_GRANT_ITEMS_BODY") {
        if !request.character_id.is_empty() {
            normalize_generic_grant_defaults(&mut request);
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
        #[serde(rename = "mailId")]
        mail_id: Option<String>,
        #[serde(rename = "requestFingerprint")]
        request_fingerprint: Option<String>,
        #[serde(rename = "traceId")]
        trace_id: Option<String>,
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
        mail_id: request.mail_id.unwrap_or_default(),
        request_fingerprint: request.request_fingerprint.unwrap_or_default(),
        trace_id: request.trace_id.unwrap_or_default(),
    })
}

fn normalize_generic_grant_defaults(request: &mut GrantItemsReq) {
    if request.request_id.trim().is_empty() && request.source != "mail-claim" {
        request.request_id = format!(
            "gm-send-item:{}:{}",
            request.character_id.trim(),
            current_unix_ms()
        );
    }
    if request.source.trim().is_empty() {
        request.source = "gm".to_string();
    }
}

fn parse_item_id_value(value: serde_json::Value) -> Result<i32, Box<dyn std::error::Error>> {
    match value {
        serde_json::Value::Number(number) => {
            let value = number
                .as_i64()
                .ok_or_else(|| std::io::Error::other("INVALID_ITEM_ID"))?;
            i32::try_from(value).map_err(|_| {
                Box::new(std::io::Error::other("INVALID_ITEM_ID")) as Box<dyn std::error::Error>
            })
        }
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
