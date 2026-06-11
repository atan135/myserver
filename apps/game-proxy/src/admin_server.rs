use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

use crate::route_store::{
    PlayerRouteRecord, ProxyRouteStore, RolloutSessionState, RoomMigrationState, RoomRouteRecord,
    RouteStoreUpdateError, UpstreamOperationState,
};

const MAX_ID_LEN: usize = 128;
const MAX_CHECKSUM_LEN: usize = 256;
const MAX_ROOM_MEMBER_COUNT: u32 = 1_000_000;

#[derive(Serialize)]
struct StatusResponse {
    ok: bool,
    // Active frontend sessions, including pre-auth connections.
    connection_count: u64,
    maintenance: bool,
    active_upstream: Option<String>,
    rollout_session: Option<crate::route_store::RolloutSession>,
    room_route_count: usize,
    player_route_count: usize,
}

#[derive(Serialize)]
struct InstancesResponse {
    ok: bool,
    instances: Vec<crate::route_store::UpstreamRoute>,
}

#[derive(Serialize)]
struct RolloutResponse {
    ok: bool,
    rollout_session: Option<crate::route_store::RolloutSession>,
}

#[derive(Serialize)]
struct RoomRoutesResponse {
    ok: bool,
    routes: Vec<RoomRouteRecord>,
}

#[derive(Serialize)]
struct PlayerRoutesResponse {
    ok: bool,
    routes: Vec<PlayerRouteRecord>,
}

pub async fn run(
    bind_addr: &str,
    route_store: ProxyRouteStore,
    connection_count: Arc<AtomicU64>,
    maintenance: Arc<tokio::sync::RwLock<bool>>,
    admin_token: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(bind_addr).await?;
    let admin_token = Arc::new(admin_token);
    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let route_store = route_store.clone();
        let connection_count = connection_count.clone();
        let maintenance = maintenance.clone();
        let admin_token = admin_token.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(
                socket,
                route_store,
                connection_count,
                maintenance,
                admin_token,
            )
            .await
            {
                warn!(peer = %peer_addr, error = %error, "proxy admin connection failed");
            }
        });
    }
}

async fn handle_connection(
    mut socket: TcpStream,
    route_store: ProxyRouteStore,
    connection_count: Arc<AtomicU64>,
    maintenance: Arc<tokio::sync::RwLock<bool>>,
    admin_token: Arc<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut buffer = [0u8; 4096];
    let read = socket.read(&mut buffer).await?;
    if read == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buffer[..read]);
    let first_line = request.lines().next().unwrap_or_default();
    let method = first_line.split_whitespace().next().unwrap_or_default();
    let path = first_line.split_whitespace().nth(1).unwrap_or_default();
    let (route_path, query) = split_path_and_query(path);

    if !is_authorized(&request, admin_token.as_str()) {
        let response = http_response(
            401,
            "text/plain; charset=utf-8",
            "missing or invalid admin token".to_string(),
        );
        socket.write_all(response.as_bytes()).await?;
        return Ok(());
    }

    let response = match (method, route_path) {
        ("GET", "/status") => {
            let counts = route_store.route_counts().await;
            write_json(StatusResponse {
                ok: true,
                connection_count: connection_count.load(Ordering::Relaxed),
                maintenance: *maintenance.read().await,
                active_upstream: route_store.active_upstream_server_id().await,
                rollout_session: route_store.get_rollout_session().await,
                room_route_count: counts.room_routes,
                player_route_count: counts.player_routes,
            })
        }
        ("GET", "/instances") => write_json(InstancesResponse {
            ok: true,
            instances: route_store.list_routes().await,
        }),
        ("GET", "/rollout") => write_json(RolloutResponse {
            ok: true,
            rollout_session: route_store.get_rollout_session().await,
        }),
        ("GET", "/room-routes") => write_json(RoomRoutesResponse {
            ok: true,
            routes: route_store.list_room_routes().await,
        }),
        ("GET", "/player-routes") => write_json(PlayerRoutesResponse {
            ok: true,
            routes: route_store.list_player_routes().await,
        }),
        ("POST", "/maintenance/on") => {
            *maintenance.write().await = true;
            audit_ok("maintenance_on", None, None, None, None);
            write_plain("ok")
        }
        ("POST", "/maintenance/off") => {
            *maintenance.write().await = false;
            audit_ok("maintenance_off", None, None, None, None);
            write_plain("ok")
        }
        ("POST", "/rollout/start") => handle_rollout_start(&route_store, &query).await,
        ("POST", "/rollout/end") => {
            let rollout_epoch = route_store
                .get_rollout_session()
                .await
                .map(|session| session.rollout_epoch);
            match route_store.end_rollout().await {
                Ok(()) => {
                    audit_ok("rollout_end", None, None, None, rollout_epoch.as_deref());
                    write_plain("ok")
                }
                Err(error) => audited_update_error(
                    "rollout_end",
                    &error,
                    None,
                    None,
                    None,
                    rollout_epoch.as_deref(),
                ),
            }
        }
        ("POST", "/rollout/state") => handle_rollout_state(&route_store, &query).await,
        ("POST", "/room-route/upsert") => handle_room_route_upsert(&route_store, &query).await,
        ("POST", "/player-route/upsert") => handle_player_route_upsert(&route_store, &query).await,
        ("POST", route_path) => {
            if let Some(server_id) = route_path.strip_prefix("/switch/") {
                handle_switch(&route_store, server_id).await
            } else {
                http_response(404, "text/plain; charset=utf-8", "not found".to_string())
            }
        }
        _ => http_response(404, "text/plain; charset=utf-8", "not found".to_string()),
    };

    socket.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn handle_rollout_start(
    route_store: &ProxyRouteStore,
    query: &HashMap<String, String>,
) -> String {
    let action = "rollout_start";
    let rollout_epoch = match required_identifier(query, "rollout_epoch") {
        Ok(value) => value,
        Err(error) => return audited_bad_request(action, error, None, None, None, None),
    };
    let old_server_id = match required_identifier(query, "old_server_id") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(action, error, None, None, None, Some(rollout_epoch));
        }
    };
    let new_server_id = match required_identifier(query, "new_server_id") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                action,
                error,
                Some(old_server_id),
                None,
                None,
                Some(rollout_epoch),
            );
        }
    };

    if old_server_id == new_server_id {
        return audited_bad_request(
            action,
            "old_server_id and new_server_id must differ",
            Some(old_server_id),
            None,
            None,
            Some(rollout_epoch),
        );
    }

    for server_id in [old_server_id, new_server_id] {
        if !upstream_exists(route_store, server_id).await {
            return audited_bad_request(
                action,
                "unknown upstream server_id",
                Some(server_id),
                None,
                None,
                Some(rollout_epoch),
            );
        }
    }

    match route_store
        .begin_rollout(
            rollout_epoch.to_string(),
            old_server_id.to_string(),
            new_server_id.to_string(),
        )
        .await
    {
        Ok(()) => {
            audit_ok(action, Some(new_server_id), None, None, Some(rollout_epoch));
            write_plain("ok")
        }
        Err(error) => audited_update_error(
            action,
            &error,
            Some(new_server_id),
            None,
            None,
            Some(rollout_epoch),
        ),
    }
}

async fn handle_rollout_state(
    route_store: &ProxyRouteStore,
    query: &HashMap<String, String>,
) -> String {
    let action = "rollout_state";
    let Some(state) = required(query, "state") else {
        return audited_bad_request(action, "missing state", None, None, None, None);
    };

    let rollout_state = match state {
        "Active" => RolloutSessionState::Active,
        "Ending" => RolloutSessionState::Ending,
        "Interrupted" => RolloutSessionState::Interrupted,
        _ => return audited_bad_request(action, "invalid state", None, None, None, None),
    };
    let Some(session) = route_store.get_rollout_session().await else {
        return audited_bad_request(action, "no active rollout", None, None, None, None);
    };
    match route_store.mark_rollout_state(rollout_state).await {
        Ok(()) => {
            audit_ok(
                action,
                None,
                None,
                None,
                Some(session.rollout_epoch.as_str()),
            );
            write_plain("ok")
        }
        Err(error) => audited_update_error(
            action,
            &error,
            None,
            None,
            None,
            Some(session.rollout_epoch.as_str()),
        ),
    }
}

async fn handle_switch(route_store: &ProxyRouteStore, server_id: &str) -> String {
    let action = "switch";
    let server_id = match validate_identifier("server_id", server_id) {
        Ok(value) => value,
        Err(error) => return audited_bad_request(action, error, None, None, None, None),
    };
    let routes = route_store.list_routes().await;
    if !routes.iter().any(|route| route.server_id == server_id) {
        return audited_bad_request(
            action,
            "unknown upstream server_id",
            Some(server_id),
            None,
            None,
            None,
        );
    }

    for route in &routes {
        let next_state = if route.server_id == server_id {
            UpstreamOperationState::Active
        } else {
            UpstreamOperationState::Draining
        };
        route_store
            .update_operation_state(&route.server_id, next_state)
            .await;
    }
    audit_ok(action, Some(server_id), None, None, None);
    write_plain("ok")
}

async fn handle_room_route_upsert(
    route_store: &ProxyRouteStore,
    query: &HashMap<String, String>,
) -> String {
    let action = "room_route_upsert";
    let room_id = match required_identifier(query, "room_id") {
        Ok(value) => value,
        Err(error) => return audited_bad_request(action, error, None, None, None, None),
    };
    let owner_server_id = match required_identifier(query, "owner_server_id") {
        Ok(value) => value,
        Err(error) => return audited_bad_request(action, error, None, Some(room_id), None, None),
    };
    if !upstream_exists(route_store, owner_server_id).await {
        return audited_bad_request(
            action,
            "unknown upstream owner_server_id",
            Some(owner_server_id),
            Some(room_id),
            None,
            None,
        );
    }

    let migration_state = match optional_migration_state(query, "migration_state") {
        Ok(value) => value.unwrap_or(RoomMigrationState::OwnedByNew),
        Err(error) => {
            return audited_bad_request(
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            );
        }
    };
    let member_count = match optional_u32(query, "member_count") {
        Ok(value) => value.unwrap_or(0),
        Err(error) => {
            return audited_bad_request(
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            );
        }
    };
    let online_member_count = match optional_u32(query, "online_member_count") {
        Ok(value) => value.unwrap_or(0),
        Err(error) => {
            return audited_bad_request(
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            );
        }
    };
    if member_count > MAX_ROOM_MEMBER_COUNT {
        return audited_bad_request(
            action,
            "member_count out of range",
            Some(owner_server_id),
            Some(room_id),
            None,
            None,
        );
    }
    if online_member_count > member_count {
        return audited_bad_request(
            action,
            "online_member_count cannot exceed member_count",
            Some(owner_server_id),
            Some(room_id),
            None,
            None,
        );
    }
    let empty_since_ms = match optional_u64(query, "empty_since_ms") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            );
        }
    };
    let room_version = match optional_u64(query, "room_version") {
        Ok(value) => value.unwrap_or(1),
        Err(error) => {
            return audited_bad_request(
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            );
        }
    };
    if room_version == 0 {
        return audited_bad_request(
            action,
            "room_version must be greater than 0",
            Some(owner_server_id),
            Some(room_id),
            None,
            None,
        );
    }
    let expected_room_version = match optional_u64(query, "expected_room_version") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            );
        }
    };
    let rollout_epoch = match optional_identifier(query, "rollout_epoch") {
        Ok(value) => value.unwrap_or_default(),
        Err(error) => {
            return audited_bad_request(
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            );
        }
    };
    let last_transfer_checksum =
        match optional_bounded_text(query, "last_transfer_checksum", MAX_CHECKSUM_LEN) {
            Ok(value) => value.unwrap_or_default(),
            Err(error) => {
                return audited_bad_request(
                    action,
                    error,
                    Some(owner_server_id),
                    Some(room_id),
                    None,
                    Some(rollout_epoch.as_str()),
                );
            }
        };
    let expected_last_transfer_checksum =
        match optional_bounded_text(query, "expected_last_transfer_checksum", MAX_CHECKSUM_LEN) {
            Ok(value) => value,
            Err(error) => {
                return audited_bad_request(
                    action,
                    error,
                    Some(owner_server_id),
                    Some(room_id),
                    None,
                    Some(rollout_epoch.as_str()),
                );
            }
        };

    let result = route_store
        .upsert_room_route(
            RoomRouteRecord {
                room_id: room_id.to_string(),
                owner_server_id: owner_server_id.to_string(),
                migration_state,
                member_count,
                online_member_count,
                empty_since_ms,
                room_version,
                rollout_epoch: rollout_epoch.clone(),
                last_transfer_checksum,
                updated_at_ms: 0,
            },
            expected_room_version,
            expected_last_transfer_checksum,
        )
        .await;

    match result {
        Ok(()) => {
            audit_ok(
                action,
                Some(owner_server_id),
                Some(room_id),
                None,
                Some(rollout_epoch.as_str()),
            );
            write_plain("ok")
        }
        Err(error) => audited_update_error(
            action,
            &error,
            Some(owner_server_id),
            Some(room_id),
            None,
            Some(rollout_epoch.as_str()),
        ),
    }
}

async fn handle_player_route_upsert(
    route_store: &ProxyRouteStore,
    query: &HashMap<String, String>,
) -> String {
    let action = "player_route_upsert";
    let player_id = match required_identifier(query, "player_id") {
        Ok(value) => value,
        Err(error) => return audited_bad_request(action, error, None, None, None, None),
    };
    let current_room_id = match optional_identifier(query, "current_room_id") {
        Ok(value) => value,
        Err(error) => return audited_bad_request(action, error, None, None, Some(player_id), None),
    };
    let preferred_server_id = match optional_identifier(query, "preferred_server_id") {
        Ok(value) => value,
        Err(error) => return audited_bad_request(action, error, None, None, Some(player_id), None),
    };
    if let Some(server_id) = preferred_server_id.as_deref() {
        if !upstream_exists(route_store, server_id).await {
            return audited_bad_request(
                action,
                "unknown upstream preferred_server_id",
                Some(server_id),
                current_room_id.as_deref(),
                Some(player_id),
                None,
            );
        }
    }
    let rollout_epoch = match optional_identifier(query, "rollout_epoch") {
        Ok(value) => value.unwrap_or_default(),
        Err(error) => {
            return audited_bad_request(
                action,
                error,
                preferred_server_id.as_deref(),
                current_room_id.as_deref(),
                Some(player_id),
                None,
            );
        }
    };

    let result = route_store
        .upsert_player_route(PlayerRouteRecord {
            player_id: player_id.to_string(),
            current_room_id: current_room_id.clone(),
            preferred_server_id: preferred_server_id.clone(),
            rollout_epoch: rollout_epoch.clone(),
            updated_at_ms: 0,
        })
        .await;

    match result {
        Ok(()) => {
            audit_ok(
                action,
                preferred_server_id.as_deref(),
                current_room_id.as_deref(),
                Some(player_id),
                Some(rollout_epoch.as_str()),
            );
            write_plain("ok")
        }
        Err(error) => audited_update_error(
            action,
            &error,
            preferred_server_id.as_deref(),
            current_room_id.as_deref(),
            Some(player_id),
            Some(rollout_epoch.as_str()),
        ),
    }
}

fn required<'a>(query: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    query
        .get(key)
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn required_identifier<'a>(
    query: &'a HashMap<String, String>,
    key: &'static str,
) -> Result<&'a str, &'static str> {
    let Some(value) = required(query, key) else {
        return Err(missing_field_error(key));
    };
    validate_identifier(key, value)
}

fn optional_identifier(
    query: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<String>, &'static str> {
    let Some(value) = query.get(key).map(String::as_str).map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    validate_identifier(key, value).map(|value| Some(value.to_string()))
}

fn validate_identifier<'a>(key: &'static str, value: &'a str) -> Result<&'a str, &'static str> {
    if value.is_empty() {
        return Err(missing_field_error(key));
    }
    if value.len() > MAX_ID_LEN {
        return Err(field_too_long_error(key));
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'@')
    }) {
        return Err(invalid_identifier_error(key));
    }
    Ok(value)
}

fn optional_bounded_text(
    query: &HashMap<String, String>,
    key: &'static str,
    max_len: usize,
) -> Result<Option<String>, &'static str> {
    let Some(value) = query.get(key).map(String::as_str).map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > max_len {
        return Err(field_too_long_error(key));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'&' | b'?' | b'#'))
    {
        return Err(invalid_identifier_error(key));
    }
    Ok(Some(value.to_string()))
}

fn optional_u32(
    query: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<u32>, &'static str> {
    let Some(value) = query.get(key).map(String::as_str).map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<u32>()
        .map(Some)
        .map_err(|_| invalid_number_error(key))
}

fn optional_u64(
    query: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<u64>, &'static str> {
    let Some(value) = query.get(key).map(String::as_str).map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| invalid_number_error(key))
}

fn optional_migration_state(
    query: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<RoomMigrationState>, &'static str> {
    let Some(value) = query.get(key).map(String::as_str).map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    RoomMigrationState::parse(value)
        .map(Some)
        .ok_or("invalid migration_state")
}

fn missing_field_error(key: &str) -> &'static str {
    match key {
        "rollout_epoch" => "missing rollout_epoch",
        "old_server_id" => "missing old_server_id",
        "new_server_id" => "missing new_server_id",
        "server_id" => "missing server_id",
        "room_id" => "missing room_id",
        "owner_server_id" => "missing owner_server_id",
        "player_id" => "missing player_id",
        _ => "missing required field",
    }
}

fn field_too_long_error(key: &str) -> &'static str {
    match key {
        "rollout_epoch" => "rollout_epoch too long",
        "old_server_id" => "old_server_id too long",
        "new_server_id" => "new_server_id too long",
        "server_id" => "server_id too long",
        "room_id" => "room_id too long",
        "owner_server_id" => "owner_server_id too long",
        "player_id" => "player_id too long",
        "current_room_id" => "current_room_id too long",
        "preferred_server_id" => "preferred_server_id too long",
        "last_transfer_checksum" => "last_transfer_checksum too long",
        "expected_last_transfer_checksum" => "expected_last_transfer_checksum too long",
        _ => "field too long",
    }
}

fn invalid_identifier_error(key: &str) -> &'static str {
    match key {
        "rollout_epoch" => "invalid rollout_epoch",
        "old_server_id" => "invalid old_server_id",
        "new_server_id" => "invalid new_server_id",
        "server_id" => "invalid server_id",
        "room_id" => "invalid room_id",
        "owner_server_id" => "invalid owner_server_id",
        "player_id" => "invalid player_id",
        "current_room_id" => "invalid current_room_id",
        "preferred_server_id" => "invalid preferred_server_id",
        "last_transfer_checksum" => "invalid last_transfer_checksum",
        "expected_last_transfer_checksum" => "invalid expected_last_transfer_checksum",
        _ => "invalid identifier",
    }
}

fn invalid_number_error(key: &str) -> &'static str {
    match key {
        "member_count" => "invalid member_count",
        "online_member_count" => "invalid online_member_count",
        "empty_since_ms" => "invalid empty_since_ms",
        "room_version" => "invalid room_version",
        "expected_room_version" => "invalid expected_room_version",
        _ => "invalid number",
    }
}

async fn upstream_exists(route_store: &ProxyRouteStore, server_id: &str) -> bool {
    route_store
        .list_routes()
        .await
        .iter()
        .any(|route| route.server_id == server_id)
}

fn audit_ok(
    action: &'static str,
    server_id: Option<&str>,
    room_id: Option<&str>,
    player_id: Option<&str>,
    rollout_epoch: Option<&str>,
) {
    info!(
        action,
        server_id = %server_id.unwrap_or_default(),
        room_id = %room_id.unwrap_or_default(),
        player_id = %player_id.unwrap_or_default(),
        rollout_epoch = %rollout_epoch.unwrap_or_default(),
        result = "ok",
        "proxy admin write operation"
    );
}

fn audit_error(
    action: &'static str,
    error: &str,
    server_id: Option<&str>,
    room_id: Option<&str>,
    player_id: Option<&str>,
    rollout_epoch: Option<&str>,
) {
    warn!(
        action,
        server_id = %server_id.unwrap_or_default(),
        room_id = %room_id.unwrap_or_default(),
        player_id = %player_id.unwrap_or_default(),
        rollout_epoch = %rollout_epoch.unwrap_or_default(),
        result = "error",
        error,
        "proxy admin write operation failed"
    );
}

fn audited_bad_request(
    action: &'static str,
    error: &'static str,
    server_id: Option<&str>,
    room_id: Option<&str>,
    player_id: Option<&str>,
    rollout_epoch: Option<&str>,
) -> String {
    audit_error(action, error, server_id, room_id, player_id, rollout_epoch);
    bad_request(error)
}

fn audited_update_error(
    action: &'static str,
    error: &RouteStoreUpdateError,
    server_id: Option<&str>,
    room_id: Option<&str>,
    player_id: Option<&str>,
    rollout_epoch: Option<&str>,
) -> String {
    let error_code = error.code();
    audit_error(
        action,
        error_code,
        server_id,
        room_id,
        player_id,
        rollout_epoch,
    );
    bad_request(error_code)
}

fn split_path_and_query(path: &str) -> (&str, HashMap<String, String>) {
    let Some((route_path, query_string)) = path.split_once('?') else {
        return (path, HashMap::new());
    };

    let mut query = HashMap::new();
    for pair in query_string.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((key, value)) => (key, value),
            None => (pair, ""),
        };
        query.insert(key.to_string(), value.to_string());
    }
    (route_path, query)
}

fn is_authorized(request: &str, admin_token: &str) -> bool {
    if admin_token.trim().is_empty() {
        return false;
    }

    if request_contains_query_token(request) {
        return false;
    }

    request
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .any(|line| header_matches_token(line, admin_token))
}

fn request_contains_query_token(request: &str) -> bool {
    let request_target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default();
    let Some((_, query_string)) = request_target.split_once('?') else {
        return false;
    };

    query_string.split('&').any(|pair| {
        let key = pair.split_once('=').map(|(key, _)| key).unwrap_or(pair);
        key.eq_ignore_ascii_case("admin_token") || key.eq_ignore_ascii_case("proxy_admin_token")
    })
}

fn header_matches_token(line: &str, admin_token: &str) -> bool {
    let Some((name, value)) = line.split_once(':') else {
        return false;
    };
    let name = name.trim();
    let value = value.trim();

    if name.eq_ignore_ascii_case("authorization") {
        let Some(token) = value.strip_prefix("Bearer ") else {
            return false;
        };
        return token.trim() == admin_token;
    }

    name.eq_ignore_ascii_case("x-admin-token") && value == admin_token
}

fn write_json<T: Serialize>(payload: T) -> String {
    http_response(
        200,
        "application/json",
        serde_json::to_string(&payload).unwrap(),
    )
}

fn write_plain(body: &str) -> String {
    http_response(200, "text/plain; charset=utf-8", body.to_string())
}

fn bad_request(body: &str) -> String {
    http_response(400, "text/plain; charset=utf-8", body.to_string())
}

fn http_response(status: u16, content_type: &str, body: String) -> String {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "OK",
    };
    format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        status,
        reason,
        content_type,
        body.len(),
        body
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        handle_player_route_upsert, handle_rollout_start, handle_rollout_state,
        handle_room_route_upsert, handle_switch, is_authorized, split_path_and_query,
    };
    use crate::route_store::{
        ProxyRouteStore, RolloutSessionState, UpstreamHealthState, UpstreamOperationState,
        UpstreamRoute,
    };

    const TOKEN: &str = "dev-only-change-this-proxy-admin-token";

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

    #[test]
    fn rejects_missing_admin_token() {
        let request = "GET /status HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n";

        assert!(!is_authorized(request, TOKEN));
    }

    #[test]
    fn accepts_bearer_admin_token() {
        let request = format!("GET /status HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n");

        assert!(is_authorized(&request, TOKEN));
    }

    #[test]
    fn accepts_x_admin_token() {
        let request = format!("GET /status HTTP/1.1\r\nx-admin-token: {TOKEN}\r\n\r\n");

        assert!(is_authorized(&request, TOKEN));
    }

    #[test]
    fn rejects_admin_token_from_query_string() {
        let request =
            format!("GET /status?admin_token={TOKEN} HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n");

        assert!(!is_authorized(&request, TOKEN));
    }

    #[test]
    fn rejects_query_admin_token_even_with_valid_header() {
        let request = format!(
            "GET /status?proxy_admin_token=ignored HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n"
        );

        assert!(!is_authorized(&request, TOKEN));
    }

    #[test]
    fn rejects_empty_configured_admin_token() {
        let request = "GET /status HTTP/1.1\r\nauthorization: Bearer \r\n\r\n";

        assert!(!is_authorized(request, ""));
    }

    #[tokio::test]
    async fn rollout_start_rejects_unknown_or_same_upstream() {
        let store = route_store().await;

        let same = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=game-server-1",
            ),
        )
        .await;
        assert_eq!(status_code(&same), 400);
        assert!(store.get_rollout_session().await.is_none());

        let unknown = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=missing",
            ),
        )
        .await;
        assert_eq!(status_code(&unknown), 400);
        assert!(store.get_rollout_session().await.is_none());
    }

    #[tokio::test]
    async fn rollout_start_and_state_accept_valid_query() {
        let store = route_store().await;

        let start = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=game-server-2",
            ),
        )
        .await;
        assert_eq!(status_code(&start), 200);

        let state = handle_rollout_state(&store, &query("/rollout/state?state=Ending")).await;
        assert_eq!(status_code(&state), 200);
        assert_eq!(
            store.get_rollout_session().await.unwrap().state,
            RolloutSessionState::Ending
        );
    }

    #[tokio::test]
    async fn rollout_state_rejects_invalid_or_missing_session() {
        let store = route_store().await;

        let no_session = handle_rollout_state(&store, &query("/rollout/state?state=Ending")).await;
        assert_eq!(status_code(&no_session), 400);

        let start = handle_rollout_start(
            &store,
            &query(
                "/rollout/start?rollout_epoch=rollout-1&old_server_id=game-server-1&new_server_id=game-server-2",
            ),
        )
        .await;
        assert_eq!(status_code(&start), 200);

        let invalid = handle_rollout_state(&store, &query("/rollout/state?state=Unknown")).await;
        assert_eq!(status_code(&invalid), 400);
        assert_eq!(
            store.get_rollout_session().await.unwrap().state,
            RolloutSessionState::Active
        );
    }

    #[tokio::test]
    async fn room_route_upsert_rejects_invalid_query_without_write() {
        let store = route_store().await;

        let invalid_number = handle_room_route_upsert(
            &store,
            &query(
                "/room-route/upsert?room_id=room-1&owner_server_id=game-server-1&member_count=abc",
            ),
        )
        .await;
        assert_eq!(status_code(&invalid_number), 400);

        let invalid_state = handle_room_route_upsert(
            &store,
            &query(
                "/room-route/upsert?room_id=room-1&owner_server_id=game-server-1&migration_state=Bad",
            ),
        )
        .await;
        assert_eq!(status_code(&invalid_state), 400);

        let invalid_count = handle_room_route_upsert(
            &store,
            &query(
                "/room-route/upsert?room_id=room-1&owner_server_id=game-server-1&member_count=1&online_member_count=2",
            ),
        )
        .await;
        assert_eq!(status_code(&invalid_count), 400);

        let unknown_owner = handle_room_route_upsert(
            &store,
            &query("/room-route/upsert?room_id=room-1&owner_server_id=missing"),
        )
        .await;
        assert_eq!(status_code(&unknown_owner), 400);

        assert!(store.list_room_routes().await.is_empty());
    }

    #[tokio::test]
    async fn room_route_upsert_accepts_valid_query() {
        let store = route_store().await;

        let response = handle_room_route_upsert(
            &store,
            &query(
                "/room-route/upsert?room_id=room-1&owner_server_id=game-server-1&migration_state=OwnedByOld&member_count=2&online_member_count=1&room_version=1&expected_room_version=0",
            ),
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
    async fn player_route_upsert_rejects_invalid_query_without_write() {
        let store = route_store().await;

        let bad_player = handle_player_route_upsert(
            &store,
            &query("/player-route/upsert?player_id=bad/player&preferred_server_id=game-server-1"),
        )
        .await;
        assert_eq!(status_code(&bad_player), 400);

        let unknown_server = handle_player_route_upsert(
            &store,
            &query("/player-route/upsert?player_id=player-1&preferred_server_id=missing"),
        )
        .await;
        assert_eq!(status_code(&unknown_server), 400);

        assert!(store.list_player_routes().await.is_empty());
    }

    #[tokio::test]
    async fn player_route_upsert_accepts_valid_query() {
        let store = route_store().await;

        let response = handle_player_route_upsert(
            &store,
            &query(
                "/player-route/upsert?player_id=player-1&current_room_id=room-1&preferred_server_id=game-server-1",
            ),
        )
        .await;

        assert_eq!(status_code(&response), 200);
        let routes = store.list_player_routes().await;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].player_id, "player-1");
        assert_eq!(routes[0].current_room_id.as_deref(), Some("room-1"));
        assert_eq!(
            routes[0].preferred_server_id.as_deref(),
            Some("game-server-1")
        );
    }

    #[tokio::test]
    async fn switch_rejects_unknown_server_and_accepts_existing() {
        let store = route_store().await;

        let unknown = handle_switch(&store, "missing").await;
        assert_eq!(status_code(&unknown), 400);

        let ok = handle_switch(&store, "game-server-2").await;
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
}
