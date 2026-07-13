use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use prost::Message;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream as TokioTcpStream;
use tokio::sync::Notify;
use tokio::sync::RwLock;
use tokio::sync::mpsc;

use super::audit::{AdminAuditTarget, AdminWritePreflightError, audit_admin_write_result};
use super::auth::{AdminAuthContext, authenticate_admin_packet};
use super::gm::{
    GmBanPlayerCommand, GmKickPlayerCommand, KickOnlineOutcome, decode_gm_ban_player_request,
    decode_gm_broadcast_request, decode_gm_kick_player_request, decode_grant_items_request,
    gm_disconnect_reason, grant_item_to_inventory_item, kick_online_player,
};
use super::rollout_status::build_rollout_drain_status_response;
use super::runtime_config::apply_runtime_config;
use super::*;
use crate::admin_pb::{GrantItem, GrantItemsReq, UpdateConfigReq};
use crate::core::config_table::CsvTableLoader;
use crate::core::context::{
    OnlinePlayerRegistry, PlayerConnectionHandle, PlayerRegistry, SharedRuntimeConfig,
};
use crate::core::global_id::ItemUidGenerator;
use crate::core::inventory::item::ItemElementValues;
use crate::core::logic::{RoomLogic, RoomLogicFactory, RoomLogicTransfer};
use crate::core::player::{PgPlayerStore, PlayerManager};
use crate::core::room::{ConnectionCloseState, MemberRole, OutboundChannel, OutboundMessage};
use crate::core::runtime::RoomManager;
use crate::csv_code::itemtable::ItemTable;
use crate::gm_broadcast::{
    GM_BROADCAST_TITLE_MAX_LEN, GmBroadcastCommand, broadcast_gm_message_to_online_players,
};
use crate::pb::{
    GameMessagePush, RequestServerShutdownReq, RequestServerShutdownRes,
    TriggerRolloutDrainNoticeReq, TriggerServerRedirectReq,
};
use crate::protocol::{
    HEADER_LEN, MessageType, Packet, PacketHeader, encode_body, encode_packet, parse_header,
};
use crate::server::{DEFAULT_DRAIN_MODE_REASON, DEFAULT_DRAIN_MODE_SOURCE, RuntimeConfig};

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
        drain_mode_reason: DEFAULT_DRAIN_MODE_REASON.to_string(),
        drain_mode_source: DEFAULT_DRAIN_MODE_SOURCE.to_string(),
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
    let mut registry_state = OnlinePlayerRegistry::default();
    registry_state.insert_by_account(PlayerConnectionHandle {
        account_player_id: player_id.to_string(),
        character_id: "chr_0000000000001".to_string(),
        kick_notify: notify.clone(),
        session_id: 42,
        outbound: OutboundChannel::new(tx, ConnectionCloseState::new()),
        kick_reason: kick_reason.clone(),
    });
    let registry = Arc::new(RwLock::new(registry_state));

    (registry, notify, kick_reason, rx)
}

async fn read_tcp_test_packet(stream: &mut TokioTcpStream) -> Packet {
    let mut header_buf = [0u8; HEADER_LEN];
    stream.read_exact(&mut header_buf).await.unwrap();
    let header = parse_header(header_buf).unwrap();
    let mut body = vec![0u8; header.body_len as usize];
    stream.read_exact(&mut body).await.unwrap();
    Packet::new(header, body)
}

#[test]
fn admin_auth_accepts_legacy_token_without_actor() {
    let packet = bytes_packet(MessageType::AdminAuthReq, 0, b"secret-admin-token".to_vec());

    let context = authenticate_admin_packet(&packet, "secret-admin-token").unwrap();

    assert_eq!(context.actor, "unknown");
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

    assert_eq!(context.actor, "unknown");
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
        ensure_admin_write_allowed(&audit_logger, &context, &packet, "admin_update_config").await;

    assert_eq!(result, Err(AdminWritePreflightError::AuditUnavailable));
    assert_eq!(runtime_config.read().await.max_body_len, 4096);
}

#[tokio::test]
async fn admin_audit_require_actor_rejects_write_before_state_change() {
    let audit_path = temp_audit_path("require-actor");
    let audit_logger = AdminAuditLogger::new(AdminAuditConfig::new(true, audit_path.clone(), true));
    let context = AdminAuthContext {
        actor: "unknown".to_string(),
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
        ensure_admin_write_allowed(&audit_logger, &context, &packet, "admin_update_config").await;

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

#[tokio::test]
async fn gm_send_item_audit_event_targets_character() {
    let audit_path = temp_audit_path("send-item-character-audit");
    let audit_logger =
        AdminAuditLogger::new(AdminAuditConfig::new(true, audit_path.clone(), false));
    let context = AdminAuthContext {
        actor: "ops@example.com".to_string(),
        actor_missing: false,
    };
    let request = GrantItemsReq {
        request_id: "gm-request-1".to_string(),
        character_id: "chr_0000000000001".to_string(),
        items: vec![GrantItem {
            item_id: 1001,
            count: 2,
            binded: false,
        }],
        source: "gm".to_string(),
        reason: "unit-test".to_string(),
        mail_id: String::new(),
        request_fingerprint: String::new(),
        trace_id: String::new(),
    };
    let packet = bytes_packet(MessageType::GmSendItemReq, 11, encode_body(&request));
    let target = AdminAuditTarget {
        character_id: request.character_id.clone(),
        ..Default::default()
    };

    audit_admin_write_result(
        &audit_logger,
        &context,
        &packet,
        "gm_send_item",
        true,
        "",
        &target,
    )
    .await
    .unwrap();

    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains("\"action\":\"gm_send_item\""));
    assert!(audit.contains("\"character_id\":\"chr_0000000000001\""));
    assert!(audit.contains("\"player_id\":\"\""));
    assert!(!audit.contains("gm-request-1"));
    assert!(!audit.contains("unit-test"));
    let _ = fs::remove_file(audit_path);
}

#[tokio::test]
async fn admin_redirect_audit_event_includes_actor_action_result_and_target() {
    let audit_path = temp_audit_path("redirect-audit");
    let audit_logger =
        AdminAuditLogger::new(AdminAuditConfig::new(true, audit_path.clone(), false));
    let context = AdminAuthContext {
        actor: "ops@example.com".to_string(),
        actor_missing: false,
    };
    let request = TriggerServerRedirectReq {
        room_id: "room-1".to_string(),
        rollout_epoch: "epoch-7".to_string(),
        reason: "rollout".to_string(),
        retry_after_ms: 250,
        target_host: "127.0.0.1".to_string(),
        target_port: 4000,
        target_server_id: "game-server-new".to_string(),
        transport: "kcp".to_string(),
    };
    let packet = bytes_packet(
        MessageType::TriggerServerRedirectReq,
        17,
        encode_body(&request),
    );
    let target = redirect_target(&request);

    audit_admin_write_result(
        &audit_logger,
        &context,
        &packet,
        "trigger_server_redirect",
        false,
        "ROOM_NOT_FOUND",
        &target,
    )
    .await
    .unwrap();

    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains("\"channel\":\"admin_tcp\""));
    assert!(audit.contains("\"actor\":\"ops@example.com\""));
    assert!(audit.contains("\"action\":\"trigger_server_redirect\""));
    assert!(audit.contains("\"ok\":false"));
    assert!(audit.contains("\"error_code\":\"ROOM_NOT_FOUND\""));
    assert!(audit.contains("\"room_id\":\"room-1\""));
    assert!(audit.contains("\"rollout_epoch\":\"epoch-7\""));
    assert!(audit.contains("\"target_server_id\":\"game-server-new\""));
    assert!(!audit.contains("127.0.0.1"));
    let _ = fs::remove_file(audit_path);
}

#[tokio::test]
async fn admin_rollout_drain_notice_audit_event_includes_room_epoch_and_result() {
    let audit_path = temp_audit_path("drain-notice-audit");
    let audit_logger =
        AdminAuditLogger::new(AdminAuditConfig::new(true, audit_path.clone(), false));
    let context = AdminAuthContext {
        actor: "ops@example.com".to_string(),
        actor_missing: false,
    };
    let request = TriggerRolloutDrainNoticeReq {
        room_id: "room-1".to_string(),
        rollout_epoch: "epoch-7".to_string(),
        reason: "rollout".to_string(),
        message: "Please leave after this round".to_string(),
        retry_after_ms: 500,
        deadline_ms: 123_456,
    };
    let packet = bytes_packet(
        MessageType::TriggerRolloutDrainNoticeReq,
        18,
        encode_body(&request),
    );
    let target = rollout_drain_notice_target(&request);

    audit_admin_write_result(
        &audit_logger,
        &context,
        &packet,
        "trigger_rollout_drain_notice",
        false,
        "ROOM_NOT_FOUND",
        &target,
    )
    .await
    .unwrap();

    let audit = fs::read_to_string(&audit_path).unwrap();
    assert!(audit.contains("\"channel\":\"admin_tcp\""));
    assert!(audit.contains("\"actor\":\"ops@example.com\""));
    assert!(audit.contains("\"action\":\"trigger_rollout_drain_notice\""));
    assert!(audit.contains("\"ok\":false"));
    assert!(audit.contains("\"error_code\":\"ROOM_NOT_FOUND\""));
    assert!(audit.contains("\"room_id\":\"room-1\""));
    assert!(audit.contains("\"rollout_epoch\":\"epoch-7\""));
    assert!(!audit.contains("Please leave after this round"));
    let _ = fs::remove_file(audit_path);
}

#[test]
fn admin_rollout_drain_notice_is_write_action() {
    assert_eq!(
        admin_write_action(MessageType::TriggerRolloutDrainNoticeReq),
        Some("trigger_rollout_drain_notice")
    );
}

#[test]
fn admin_shutdown_request_is_write_action() {
    assert_eq!(
        admin_write_action(MessageType::RequestServerShutdownReq),
        Some("request_server_shutdown")
    );
}

#[tokio::test]
async fn admin_shutdown_request_writes_success_then_triggers_signal() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let room_manager = Arc::new(RoomManager::with_match_client(
        crate::match_client::create_match_client_shared(),
        Arc::new(NoopRoomLogicFactory),
    ));
    let runtime_config = runtime_config_fixture();
    runtime_config.write().await.drain_mode_enabled = true;
    let connection_count = Arc::new(AtomicU64::new(0));
    let config_tables = ConfigTableRuntime::load(std::path::Path::new("csv"))
        .expect("test config tables should load");
    let shutdown_signal = Arc::new(Notify::new());
    let handler_shutdown_signal = shutdown_signal.clone();
    let handler = tokio::spawn({
        let room_manager = room_manager.clone();
        let runtime_config = runtime_config.clone();
        let connection_count = connection_count.clone();
        async move {
            let (socket, _) = listener.accept().await.unwrap();
            handle_admin_connection(
                socket,
                room_manager,
                runtime_config,
                connection_count,
                PlayerRegistry::default(),
                PlayerManager::new(PgPlayerStore::new_disabled()),
                config_tables,
                ItemUidGenerator::new_for_test(1),
                "game-server-test".to_string(),
                "secret-admin-token".to_string(),
                AdminAuditLogger::new(AdminAuditConfig::new(false, "", false)),
                handler_shutdown_signal,
            )
            .await
            .map_err(|error| error.to_string())
        }
    });

    let mut client = TokioTcpStream::connect(addr).await.unwrap();
    client
        .write_all(&encode_packet(
            MessageType::AdminAuthReq,
            1,
            b"secret-admin-token",
        ))
        .await
        .unwrap();
    client
        .write_all(&encode_packet(
            MessageType::RequestServerShutdownReq,
            2,
            &encode_body(&RequestServerShutdownReq {
                reason: "unit-test".to_string(),
            }),
        ))
        .await
        .unwrap();

    let packet = read_tcp_test_packet(&mut client).await;
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

    drop(client);
    handler.await.unwrap().unwrap();
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
fn decode_gm_send_item_uses_character_id() {
    let packet = json_packet(
        MessageType::GmSendItemReq,
        r#"{"characterId":" chr_0000000000001 ","itemId":"1001","itemCount":2,"reason":"gift"}"#,
    );

    let request = decode_grant_items_request(&packet).unwrap();

    assert_eq!(request.character_id, "chr_0000000000001");
    assert_eq!(request.items.len(), 1);
    assert_eq!(request.items[0].item_id, 1001);
    assert_eq!(request.items[0].count, 2);
    assert!(
        request
            .request_id
            .starts_with("gm-send-item:chr_0000000000001:")
    );
}

#[test]
fn decode_gm_send_item_rejects_legacy_player_id_target() {
    let packet = json_packet(
        MessageType::GmSendItemReq,
        r#"{"playerId":"plr_1","itemId":"1001","itemCount":1}"#,
    );

    let error = match decode_grant_items_request(&packet) {
        Ok(_) => panic!("legacy playerId target should be rejected"),
        Err(error) => error,
    };

    assert_eq!(error.to_string(), "INVALID_CHARACTER_ID");
}

#[test]
fn decode_gm_send_item_rejects_item_id_outside_i32_range() {
    let packet = json_packet(
        MessageType::GmSendItemReq,
        r#"{"characterId":"chr_1","itemId":4294967297,"itemCount":1}"#,
    );

    let error = decode_grant_items_request(&packet).unwrap_err();
    assert_eq!(error.to_string(), "INVALID_ITEM_ID");
}

#[test]
fn grant_item_to_inventory_item_uses_config_snapshot_and_bound_character() {
    let table = ItemTable::load_from_csv(std::path::Path::new("csv/ItemTable.csv")).unwrap();
    let generator = ItemUidGenerator::new_for_test(10);
    let request_item = GrantItem {
        item_id: 1002,
        count: 1,
        binded: true,
    };

    let item = grant_item_to_inventory_item(&request_item, "chr_0000000000001", &generator, &table)
        .unwrap();

    assert_eq!(item.uid, 10);
    assert_eq!(item.item_id, 1002);
    assert_eq!(item.count, 1);
    assert!(item.binded);
    assert_eq!(
        item.bound_character_id.as_deref(),
        Some("chr_0000000000001")
    );
    assert_eq!(item.template_elements, ItemElementValues::new(0, 80, 0, 0));
    assert!(item.growth_rules.growth_enabled);
    assert_eq!(item.growth_rules.growth_source.as_deref(), Some("Enhance"));
    assert_eq!(item.growth_rules.trade_rule, "NoTradeAfterGrowth");
    assert_eq!(item.growth_rules.decompose_rule, "ReturnMaterials");
    assert_eq!(item.growth_rules.inherit_rule, "InheritGrowth");
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
            character_id: "chr_0000000000001".to_string(),
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

    let enabled = runtime_config.read().await.clone();
    assert!(enabled.drain_mode_enabled);
    assert!(enabled.drain_mode_entered_at_ms.is_some());

    apply_runtime_config(&runtime_config, "drain_mode_enabled", "off")
        .await
        .unwrap();

    let disabled = runtime_config.read().await.clone();
    assert!(!disabled.drain_mode_enabled);
    assert_eq!(disabled.drain_mode_entered_at_ms, None);
}

#[tokio::test]
async fn apply_runtime_config_updates_drain_mode_reason_and_source() {
    let runtime_config = runtime_config_fixture();

    apply_runtime_config(&runtime_config, "drain_mode_reason", "  hotfix rollout  ")
        .await
        .unwrap();
    apply_runtime_config(&runtime_config, "drain_mode_source", "  ops-admin  ")
        .await
        .unwrap();
    apply_runtime_config(&runtime_config, "drain_mode", "on")
        .await
        .unwrap();

    let enabled = runtime_config.read().await.clone();
    assert!(enabled.drain_mode_enabled);
    assert_eq!(enabled.drain_mode_reason, "hotfix rollout");
    assert_eq!(enabled.drain_mode_source, "ops-admin");

    apply_runtime_config(&runtime_config, "drain_mode", "off")
        .await
        .unwrap();

    let disabled = runtime_config.read().await.clone();
    assert!(!disabled.drain_mode_enabled);
    assert_eq!(disabled.drain_mode_reason, DEFAULT_DRAIN_MODE_REASON);
    assert_eq!(disabled.drain_mode_source, DEFAULT_DRAIN_MODE_SOURCE);
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
    assert_eq!(response.transferable_empty_room_count, 0);
    assert!(response.transferable_empty_room_samples.is_empty());
    assert_eq!(response.retired_room_count, 0);
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

    let runtime = runtime_config.read().await.clone();
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

    let runtime = runtime_config.read().await.clone();
    assert!(runtime.input_timestamp_required);
    assert_eq!(runtime.input_timestamp_max_skew_ms, 300_000);

    apply_runtime_config(&runtime_config, "input_timestamp_required", "off")
        .await
        .unwrap();
    apply_runtime_config(&runtime_config, "input_timestamp_max_skew_ms", "0")
        .await
        .unwrap();

    let runtime = runtime_config.read().await.clone();
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

    let runtime = runtime_config.read().await.clone();
    assert_eq!(runtime.input_anomaly_window_ms, 60_000);
    assert_eq!(runtime.input_anomaly_max, 5);

    apply_runtime_config(&runtime_config, "input_anomaly_max", "0")
        .await
        .unwrap();

    let runtime = runtime_config.read().await.clone();
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
