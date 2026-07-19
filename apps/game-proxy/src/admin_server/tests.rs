use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use super::{
    AdminAuditConfig, AdminAuditLogger, AdminAuthConfig, AdminPermission, AdminRequestContext,
    AdminAssertionVerifier,
    admin_request_context, admin_route_requirement, audit_ok, audit_write_failed, authorize,
    authorize_method, authorize_route, handle_character_route_upsert, handle_connection,
    handle_rollout_complete_if_drained, handle_rollout_start, handle_rollout_state,
    handle_room_route_upsert, handle_switch, is_authorized, split_path_and_query,
};
use crate::config::{AdminPermissionScope, AdminScopedTokenConfig};
use crate::rollout_drain_status::{OldServerDrainStatusCheckSummary, OldServerDrainStatusChecker};
use crate::route_store::{
    ProxyRouteStore, RolloutDrainStatus, RolloutSessionState, RoomMigrationState, RoomRouteRecord,
    UpstreamHealthState, UpstreamOperationState, UpstreamRoute,
};

const TOKEN: &str = "dev-only-change-this-proxy-admin-token";
const READ_TOKEN: &str = "dev-only-change-this-proxy-admin-read-token";

async fn route_store() -> ProxyRouteStore {
    let store = ProxyRouteStore::default();
    store
        .set_static_routes(vec![
            UpstreamRoute {
                server_id: "game-server-1".to_string(),
                local_socket_name: "server-1.sock".to_string(),
                operation_state: UpstreamOperationState::Active,
                health_state: UpstreamHealthState::Healthy,
            },
            UpstreamRoute {
                server_id: "game-server-2".to_string(),
                local_socket_name: "server-2.sock".to_string(),
                operation_state: UpstreamOperationState::Draining,
                health_state: UpstreamHealthState::Healthy,
            },
        ])
        .await;
    store
}

fn query(path: &str) -> HashMap<String, String> {
    split_path_and_query(path).1
}

fn status_code(response: &str) -> u16 {
    response
        .split_whitespace()
        .nth(1)
        .expect("HTTP response should include status code")
        .parse()
        .unwrap()
}

fn response_json(response: &str) -> serde_json::Value {
    let body = response
        .split("\r\n\r\n")
        .nth(1)
        .expect("HTTP response should include body");
    serde_json::from_str(body).unwrap()
}

fn auth_config() -> AdminAuthConfig {
    AdminAuthConfig::new(TOKEN.to_string(), Some(READ_TOKEN.to_string()))
}

fn scoped_auth_config(token: &str, permissions: Vec<AdminPermissionScope>) -> AdminAuthConfig {
    AdminAuthConfig::with_scoped_tokens(
        TOKEN.to_string(),
        Some(READ_TOKEN.to_string()),
        vec![AdminScopedTokenConfig {
            token: token.to_string(),
            permissions,
        }],
    )
}

fn assertion_verifier() -> Arc<AdminAssertionVerifier> {
    Arc::new(AdminAssertionVerifier::new(
        "admin-api".to_string(),
        &HashMap::new(),
        60_000,
    ))
}

#[derive(Default)]
struct MockOldServerDrainStatusChecker {
    results: Mutex<Vec<OldServerDrainStatusCheckSummary>>,
}

impl MockOldServerDrainStatusChecker {
    fn with_result(result: OldServerDrainStatusCheckSummary) -> Self {
        Self {
            results: Mutex::new(vec![result]),
        }
    }
}

impl OldServerDrainStatusChecker for MockOldServerDrainStatusChecker {
    fn check<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = OldServerDrainStatusCheckSummary> + Send + 'a>> {
        Box::pin(async move {
            self.results
                .lock()
                .await
                .pop()
                .expect("mock drain status result should be configured")
        })
    }
}

fn test_audit_logger() -> AdminAuditLogger {
    AdminAuditLogger::new(AdminAuditConfig::new(false, "", false))
}

fn test_context(path: &str) -> AdminRequestContext {
    AdminRequestContext {
        actor: "ops@example.com".to_string(),
        actor_missing: false,
        method: "POST".to_string(),
        path: path.to_string(),
    }
}

mod auth;

#[tokio::test]
async fn rollout_start_rejects_unknown_or_same_upstream() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let context = test_context("/rollout/start");

    let same = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=game-server-1",
            ),
            &audit_logger,
            &context,
        )
        .await;
    assert_eq!(status_code(&same), 400);
    assert!(store.get_rollout_session().await.is_none());

    let unknown = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=missing",
            ),
            &audit_logger,
            &context,
        )
        .await;
    assert_eq!(status_code(&unknown), 400);
    assert!(store.get_rollout_session().await.is_none());
}

#[tokio::test]
async fn rollout_start_and_state_accept_valid_query() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let start_context = test_context("/rollout/start");
    let state_context = test_context("/rollout/state");

    let start = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=game-server-2",
            ),
            &audit_logger,
            &start_context,
        )
        .await;
    assert_eq!(status_code(&start), 200);

    let state = handle_rollout_state(
        &store,
        &query("/rollout/state?state=Ending"),
        &audit_logger,
        &state_context,
    )
    .await;
    assert_eq!(status_code(&state), 200);
    assert_eq!(
        store.get_rollout_session().await.unwrap().state,
        RolloutSessionState::Ending
    );
}

#[tokio::test]
async fn rollout_complete_if_drained_reports_blockers_without_ending() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let start_context = test_context("/rollout/start");
    let complete_context = test_context("/rollout/complete-if-drained");

    let start = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=game-server-2",
            ),
            &audit_logger,
            &start_context,
        )
        .await;
    assert_eq!(status_code(&start), 200);
    store
        .upsert_room_route(
            RoomRouteRecord {
                room_id: "room-1".to_string(),
                owner_server_id: "game-server-1".to_string(),
                migration_state: RoomMigrationState::OwnedByOld,
                member_count: 0,
                online_member_count: 0,
                empty_since_ms: Some(123),
                room_version: 1,
                rollout_epoch: "rollout-1".to_string(),
                last_transfer_checksum: String::new(),
                updated_at_ms: 0,
            },
            Some(0),
            None,
        )
        .await
        .unwrap();

    let response =
        handle_rollout_complete_if_drained(&store, &audit_logger, &complete_context, None).await;
    let body = response_json(&response);

    assert_eq!(status_code(&response), 409);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], "ROLLOUT_NOT_DRAINED");
    assert_eq!(body["drain_evaluation"]["blocked_room_count"], 1);
    assert_eq!(
        body["drain_evaluation"]["blocked_room_samples"][0],
        "room-1"
    );
    assert!(store.get_rollout_session().await.is_some());
}

#[tokio::test]
async fn rollout_end_reports_conflict_when_routes_are_not_drained() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let route_store = route_store().await;
    let store_for_assert = route_store.clone();
    let connection_count = Arc::new(AtomicU64::new(0));
    let maintenance = Arc::new(tokio::sync::RwLock::new(false));
    let auth_config = Arc::new(auth_config());
    let audit_logger = Arc::new(test_audit_logger());

    route_store
        .begin_rollout(
            "rollout-1".to_string(),
            "game-server-1".to_string(),
            "game-server-2".to_string(),
        )
        .await
        .unwrap();
    route_store
        .upsert_room_route(
            RoomRouteRecord {
                room_id: "room-1".to_string(),
                owner_server_id: "game-server-1".to_string(),
                migration_state: RoomMigrationState::OwnedByOld,
                member_count: 0,
                online_member_count: 0,
                empty_since_ms: Some(123),
                room_version: 1,
                rollout_epoch: "rollout-1".to_string(),
                last_transfer_checksum: String::new(),
                updated_at_ms: 0,
            },
            Some(0),
            None,
        )
        .await
        .unwrap();

    let server = async {
        let (socket, _) = listener.accept().await.unwrap();
        handle_connection(
            socket,
            route_store,
            connection_count,
            maintenance,
            auth_config,
            assertion_verifier(),
            "game-proxy-test".to_string(),
            audit_logger,
            None,
        )
        .await
        .unwrap();
    };
    let client = async {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
                .write_all(
                    format!(
                        "POST /rollout/end HTTP/1.1\r\nx-admin-token: {TOKEN}\r\nX-Admin-Actor: ops@example.com\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    };

    let (_, response) = tokio::join!(server, client);

    assert_eq!(status_code(&response), 401);
    assert!(response.ends_with("ADMIN_ASSERTION_UNAUTHENTICATED"));
    assert!(store_for_assert.get_rollout_session().await.is_some());
}

#[tokio::test]
async fn rollout_complete_if_drained_ends_when_routes_are_drained() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let start_context = test_context("/rollout/start");
    let complete_context = test_context("/rollout/complete-if-drained");

    let start = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=game-server-2",
            ),
            &audit_logger,
            &start_context,
        )
        .await;
    assert_eq!(status_code(&start), 200);
    store
        .upsert_room_route(
            RoomRouteRecord {
                room_id: "room-1".to_string(),
                owner_server_id: "game-server-2".to_string(),
                migration_state: RoomMigrationState::OwnedByNew,
                member_count: 0,
                online_member_count: 0,
                empty_since_ms: Some(123),
                room_version: 1,
                rollout_epoch: "rollout-1".to_string(),
                last_transfer_checksum: "checksum-1".to_string(),
                updated_at_ms: 0,
            },
            Some(0),
            None,
        )
        .await
        .unwrap();

    let response =
        handle_rollout_complete_if_drained(&store, &audit_logger, &complete_context, None).await;
    let body = response_json(&response);

    assert_eq!(status_code(&response), 200);
    assert_eq!(body["ok"], true);
    assert_eq!(body["drain_evaluation"]["status"], "Drained");
    assert_eq!(body["end_summary"]["rollout_epoch"], "rollout-1");
    assert_eq!(body["end_summary"]["removed_room_route_count"], 1);
    assert!(store.get_rollout_session().await.is_none());
    assert!(store.list_room_routes().await.is_empty());
}

#[tokio::test]
async fn rollout_complete_if_drained_with_old_server_check_allows_drained_status() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let start_context = test_context("/rollout/start");
    let complete_context = test_context("/rollout/complete-if-drained");
    let checker =
        MockOldServerDrainStatusChecker::with_result(OldServerDrainStatusCheckSummary::passed());

    let start = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=game-server-2",
            ),
            &audit_logger,
            &start_context,
        )
        .await;
    assert_eq!(status_code(&start), 200);
    store
        .upsert_room_route(
            RoomRouteRecord {
                room_id: "room-1".to_string(),
                owner_server_id: "game-server-2".to_string(),
                migration_state: RoomMigrationState::OwnedByNew,
                member_count: 0,
                online_member_count: 0,
                empty_since_ms: Some(123),
                room_version: 1,
                rollout_epoch: "rollout-1".to_string(),
                last_transfer_checksum: "checksum-1".to_string(),
                updated_at_ms: 0,
            },
            Some(0),
            None,
        )
        .await
        .unwrap();

    let response = handle_rollout_complete_if_drained(
        &store,
        &audit_logger,
        &complete_context,
        Some(&checker),
    )
    .await;
    let body = response_json(&response);

    assert_eq!(status_code(&response), 200);
    assert_eq!(body["ok"], true);
    assert_eq!(body["drain_evaluation"]["status"], "Drained");
    assert_eq!(body["old_server_drain_status"]["passed"], true);
    assert_eq!(body["old_server_drain_status"]["connection_count"], 0);
    assert!(store.get_rollout_session().await.is_none());
    assert!(store.list_room_routes().await.is_empty());
}

#[tokio::test]
async fn rollout_complete_if_drained_with_old_server_check_blocks_nonzero_connections() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let start_context = test_context("/rollout/start");
    let complete_context = test_context("/rollout/complete-if-drained");
    let checker = MockOldServerDrainStatusChecker::with_result(
        OldServerDrainStatusCheckSummary::not_drained(2),
    );

    let start = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=game-server-2",
            ),
            &audit_logger,
            &start_context,
        )
        .await;
    assert_eq!(status_code(&start), 200);

    let response = handle_rollout_complete_if_drained(
        &store,
        &audit_logger,
        &complete_context,
        Some(&checker),
    )
    .await;
    let body = response_json(&response);

    assert_eq!(status_code(&response), 409);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], "OLD_SERVER_DRAIN_STATUS_NOT_DRAINED");
    assert_eq!(body["drain_evaluation"]["status"], "Drained");
    assert_eq!(body["old_server_drain_status"]["passed"], false);
    assert_eq!(body["old_server_drain_status"]["connection_count"], 2);
    assert!(store.get_rollout_session().await.is_some());
}

#[tokio::test]
async fn rollout_complete_if_drained_with_old_server_check_blocks_request_failure() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let start_context = test_context("/rollout/start");
    let complete_context = test_context("/rollout/complete-if-drained");
    let checker = MockOldServerDrainStatusChecker::with_result(
        OldServerDrainStatusCheckSummary::request_failed("CONNECT_TIMEOUT"),
    );

    let start = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=game-server-2",
            ),
            &audit_logger,
            &start_context,
        )
        .await;
    assert_eq!(status_code(&start), 200);

    let response = handle_rollout_complete_if_drained(
        &store,
        &audit_logger,
        &complete_context,
        Some(&checker),
    )
    .await;
    let body = response_json(&response);

    assert_eq!(status_code(&response), 409);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], "OLD_SERVER_DRAIN_STATUS_CHECK_FAILED");
    assert_eq!(body["drain_evaluation"]["status"], "Drained");
    assert_eq!(body["old_server_drain_status"]["passed"], false);
    assert_eq!(body["old_server_drain_status"]["error"], "CONNECT_TIMEOUT");
    assert!(store.get_rollout_session().await.is_some());
}

#[tokio::test]
async fn rollout_complete_if_drained_rejects_without_active_rollout() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let complete_context = test_context("/rollout/complete-if-drained");

    let response =
        handle_rollout_complete_if_drained(&store, &audit_logger, &complete_context, None).await;
    let body = response_json(&response);

    assert_eq!(status_code(&response), 400);
    assert_eq!(body["ok"], false);
    assert_eq!(body["error"], "NO_ACTIVE_ROLLOUT");
    assert_eq!(
        body["drain_evaluation"]["status"],
        serde_json::json!(RolloutDrainStatus::NoActiveRollout)
    );
}

#[tokio::test]
async fn rollout_state_rejects_invalid_or_missing_session() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let start_context = test_context("/rollout/start");
    let state_context = test_context("/rollout/state");

    let no_session = handle_rollout_state(
        &store,
        &query("/rollout/state?state=Ending"),
        &audit_logger,
        &state_context,
    )
    .await;
    assert_eq!(status_code(&no_session), 400);

    let start = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=game-server-2",
            ),
            &audit_logger,
            &start_context,
        )
        .await;
    assert_eq!(status_code(&start), 200);

    let invalid = handle_rollout_state(
        &store,
        &query("/rollout/state?state=Unknown"),
        &audit_logger,
        &state_context,
    )
    .await;
    assert_eq!(status_code(&invalid), 400);
    assert_eq!(
        store.get_rollout_session().await.unwrap().state,
        RolloutSessionState::Active
    );
}

#[tokio::test]
async fn room_route_upsert_rejects_invalid_query_without_write() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let context = test_context("/room-route/upsert");

    let invalid_number = handle_room_route_upsert(
        &store,
        &query("/room-route/upsert?room_id=room-1&owner_server_id=game-server-1&member_count=abc"),
        &audit_logger,
        &context,
    )
    .await;
    assert_eq!(status_code(&invalid_number), 400);

    let invalid_state = handle_room_route_upsert(
        &store,
        &query(
            "/room-route/upsert?room_id=room-1&owner_server_id=game-server-1&migration_state=Bad",
        ),
        &audit_logger,
        &context,
    )
    .await;
    assert_eq!(status_code(&invalid_state), 400);

    let invalid_count = handle_room_route_upsert(
            &store,
            &query(
                "/room-route/upsert?room_id=room-1&owner_server_id=game-server-1&member_count=1&online_member_count=2",
            ),
            &audit_logger,
            &context,
        )
        .await;
    assert_eq!(status_code(&invalid_count), 400);

    let unknown_owner = handle_room_route_upsert(
        &store,
        &query("/room-route/upsert?room_id=room-1&owner_server_id=missing"),
        &audit_logger,
        &context,
    )
    .await;
    assert_eq!(status_code(&unknown_owner), 400);

    assert!(store.list_room_routes().await.is_empty());
}

#[tokio::test]
async fn room_route_upsert_accepts_valid_query() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let context = test_context("/room-route/upsert");

    let response = handle_room_route_upsert(
            &store,
            &query(
                "/room-route/upsert?room_id=room-1&owner_server_id=game-server-1&migration_state=OwnedByOld&member_count=2&online_member_count=1&room_version=1&expected_room_version=0",
            ),
            &audit_logger,
            &context,
        )
        .await;

    assert_eq!(status_code(&response), 200);
    let routes = store.list_room_routes().await;
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].room_id, "room-1");
    assert_eq!(routes[0].owner_server_id, "game-server-1");
    assert_eq!(routes[0].member_count, 2);
    assert_eq!(routes[0].online_member_count, 1);
}

#[tokio::test]
async fn character_route_upsert_rejects_invalid_query_without_write() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let context = test_context("/character-route/upsert");

    let bad_character = handle_character_route_upsert(
        &store,
        &query("/character-route/upsert?character_id=bad/player&preferred_server_id=game-server-1"),
        &audit_logger,
        &context,
    )
    .await;
    assert_eq!(status_code(&bad_character), 400);

    let unknown_server = handle_character_route_upsert(
        &store,
        &query(
            "/character-route/upsert?character_id=chr_0000000000001&preferred_server_id=missing",
        ),
        &audit_logger,
        &context,
    )
    .await;
    assert_eq!(status_code(&unknown_server), 400);

    assert!(store.list_character_routes().await.is_empty());
}

#[tokio::test]
async fn character_route_upsert_accepts_valid_query() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let context = test_context("/character-route/upsert");

    let response = handle_character_route_upsert(
            &store,
            &query(
                "/character-route/upsert?character_id=chr_0000000000001&current_room_id=room-1&preferred_server_id=game-server-1",
            ),
            &audit_logger,
            &context,
        )
        .await;

    assert_eq!(status_code(&response), 200);
    let routes = store.list_character_routes().await;
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].character_id, "chr_0000000000001");
    assert_eq!(routes[0].current_room_id.as_deref(), Some("room-1"));
    assert_eq!(
        routes[0].preferred_server_id.as_deref(),
        Some("game-server-1")
    );
}

#[test]
fn legacy_player_route_admin_paths_are_removed() {
    assert!(admin_route_requirement("GET", "/player-routes").is_none());
    assert!(admin_route_requirement("POST", "/player-route/upsert").is_none());
    assert!(admin_route_requirement("GET", "/character-routes").is_some());
    assert!(admin_route_requirement("POST", "/character-route/upsert").is_some());
}

#[tokio::test]
async fn switch_rejects_unknown_server_and_accepts_existing() {
    let store = route_store().await;
    let audit_logger = test_audit_logger();
    let context = test_context("/switch/game-server-2");

    let unknown = handle_switch(&store, "missing", &audit_logger, &context).await;
    assert_eq!(status_code(&unknown), 400);

    let ok = handle_switch(&store, "game-server-2", &audit_logger, &context).await;
    assert_eq!(status_code(&ok), 200);

    let routes = store.list_routes().await;
    assert_eq!(
        routes
            .iter()
            .find(|route| route.server_id == "game-server-2")
            .unwrap()
            .operation_state,
        UpstreamOperationState::Active
    );
    assert_eq!(
        routes
            .iter()
            .find(|route| route.server_id == "game-server-1")
            .unwrap()
            .operation_state,
        UpstreamOperationState::Draining
    );
}

#[test]
fn parses_admin_actor_header_and_defaults_missing_actor() {
    let request = format!(
        "POST /maintenance/on HTTP/1.1\r\nx-admin-token: {TOKEN}\r\nX-Admin-Actor: ops@example.com\r\n\r\n"
    );
    let context = admin_request_context(&request, "POST", "/maintenance/on");

    assert_eq!(context.actor, "ops@example.com");
    assert!(!context.actor_missing);

    let missing = admin_request_context(
        "POST /maintenance/on HTTP/1.1\r\nx-admin-token: token\r\n\r\n",
        "POST",
        "/maintenance/on",
    );
    assert_eq!(missing.actor, "unknown");
    assert!(missing.actor_missing);

    let invalid = admin_request_context(
        "POST /maintenance/on HTTP/1.1\r\nX-Admin-Actor: bad actor\r\n\r\n",
        "POST",
        "/maintenance/on",
    );
    assert_eq!(invalid.actor, "unknown");
    assert!(invalid.actor_missing);
}

#[tokio::test]
async fn writes_admin_audit_jsonl_fields() {
    let path = std::env::temp_dir().join(format!(
        "game-proxy-admin-audit-{}-{}.jsonl",
        std::process::id(),
        "ok"
    ));
    let _ = tokio::fs::remove_file(&path).await;
    let audit_logger = AdminAuditLogger::new(AdminAuditConfig::new(true, &path, false));
    let context = AdminRequestContext {
        actor: "ops@example.com".to_string(),
        actor_missing: false,
        method: "POST".to_string(),
        path: "/room-route/upsert".to_string(),
    };

    audit_ok(
        &audit_logger,
        &context,
        "room_route_upsert",
        Some("game-server-1"),
        Some("room-1"),
        None,
        Some("rollout-1"),
    )
    .await
    .unwrap();

    let content = tokio::fs::read_to_string(&path).await.unwrap();
    let event: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
    assert_eq!(event["actor"], "ops@example.com");
    assert_eq!(event["actor_missing"], false);
    assert_eq!(event["method"], "POST");
    assert_eq!(event["path"], "/room-route/upsert");
    assert_eq!(event["action"], "room_route_upsert");
    assert_eq!(event["result"], "ok");
    assert_eq!(event["server_id"], "game-server-1");
    assert_eq!(event["room_id"], "room-1");
    assert_eq!(event["rollout_epoch"], "rollout-1");

    let _ = tokio::fs::remove_file(&path).await;
}

#[tokio::test]
async fn legacy_token_only_write_is_rejected_before_state_change() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let route_store = route_store().await;
    let connection_count = Arc::new(AtomicU64::new(0));
    let maintenance = Arc::new(tokio::sync::RwLock::new(false));
    let audit_path = std::env::temp_dir().join(format!(
        "game-proxy-admin-audit-{}-require-actor.jsonl",
        std::process::id()
    ));
    let _ = tokio::fs::remove_file(&audit_path).await;
    let auth_config = Arc::new(auth_config());
    let audit_logger = Arc::new(AdminAuditLogger::new(AdminAuditConfig::new(
        true,
        &audit_path,
        true,
    )));

    let server = async {
        let (socket, _) = listener.accept().await.unwrap();
        handle_connection(
            socket,
            route_store,
            connection_count,
            maintenance.clone(),
            auth_config,
            assertion_verifier(),
            "game-proxy-test".to_string(),
            audit_logger,
            None,
        )
        .await
        .unwrap();
        assert!(!*maintenance.read().await);
    };
    let client = async {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(
                format!("POST /maintenance/on HTTP/1.1\r\nx-admin-token: {TOKEN}\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    };

    let (_, response) = tokio::join!(server, client);

    assert_eq!(status_code(&response), 401);
    assert!(response.ends_with("ADMIN_ASSERTION_UNAUTHENTICATED"));

    let _ = tokio::fs::remove_file(&audit_path).await;
}

#[tokio::test]
async fn scoped_token_cannot_authorize_proxy_write() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let route_store = route_store().await;
    let connection_count = Arc::new(AtomicU64::new(0));
    let maintenance = Arc::new(tokio::sync::RwLock::new(false));
    let audit_path = std::env::temp_dir().join(format!(
        "game-proxy-admin-audit-{}-permission-denied.jsonl",
        std::process::id()
    ));
    let _ = tokio::fs::remove_file(&audit_path).await;
    let scoped_token = "maintenance-token";
    let auth_config = Arc::new(scoped_auth_config(
        scoped_token,
        vec![AdminPermissionScope::MaintenanceWrite],
    ));
    let audit_logger = Arc::new(AdminAuditLogger::new(AdminAuditConfig::new(
        true,
        &audit_path,
        false,
    )));

    let server = async {
        let (socket, _) = listener.accept().await.unwrap();
        handle_connection(
            socket,
            route_store,
            connection_count,
            maintenance,
            auth_config,
            assertion_verifier(),
            "game-proxy-test".to_string(),
            audit_logger,
            None,
        )
        .await
        .unwrap();
    };
    let client = async {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream
                .write_all(
                    format!(
                        "POST /rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=game-server-2 HTTP/1.1\r\nauthorization: Bearer {scoped_token}\r\nX-Admin-Actor: ops@example.com\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response
    };

    let (_, response) = tokio::join!(server, client);

    assert_eq!(status_code(&response), 401);
    assert!(response.ends_with("ADMIN_ASSERTION_UNAUTHENTICATED"));

    let _ = tokio::fs::remove_file(&audit_path).await;
}

#[test]
fn audit_write_failure_response_is_500() {
    let audit_logger = AdminAuditLogger::new(AdminAuditConfig::new(
        true,
        "logs/game-proxy/audit.jsonl",
        false,
    ));
    let error = super::AdminAuditError::Io(std::io::Error::other("disk full"));

    let response = audit_write_failed(&audit_logger, "maintenance_on", &error);

    assert_eq!(status_code(&response), 500);
    assert!(response.ends_with("admin audit write failed"));
}
