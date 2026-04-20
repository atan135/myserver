use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::warn;

use crate::route_store::{
    PlayerRouteRecord, ProxyRouteStore, RolloutSessionState, RoomMigrationState, RoomRouteRecord,
    UpstreamOperationState,
};

#[derive(Serialize)]
struct StatusResponse {
    ok: bool,
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
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(bind_addr).await?;
    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let route_store = route_store.clone();
        let connection_count = connection_count.clone();
        let maintenance = maintenance.clone();
        tokio::spawn(async move {
            if let Err(error) =
                handle_connection(socket, route_store, connection_count, maintenance).await
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
            write_plain("ok")
        }
        ("POST", "/maintenance/off") => {
            *maintenance.write().await = false;
            write_plain("ok")
        }
        ("POST", "/rollout/start") => handle_rollout_start(&route_store, &query).await,
        ("POST", "/rollout/end") => {
            route_store.end_rollout().await;
            write_plain("ok")
        }
        ("POST", "/rollout/state") => handle_rollout_state(&route_store, &query).await,
        ("POST", "/room-route/upsert") => handle_room_route_upsert(&route_store, &query).await,
        ("POST", "/player-route/upsert") => handle_player_route_upsert(&route_store, &query).await,
        _ => {
            if let Some(server_id) = route_path.strip_prefix("/switch/") {
                handle_switch(&route_store, server_id).await
            } else {
                http_response(404, "text/plain; charset=utf-8", "not found".to_string())
            }
        }
    };

    socket.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn handle_rollout_start(
    route_store: &ProxyRouteStore,
    query: &HashMap<String, String>,
) -> String {
    let Some(rollout_epoch) = required(query, "rollout_epoch") else {
        return bad_request("missing rollout_epoch");
    };
    let Some(old_server_id) = required(query, "old_server_id") else {
        return bad_request("missing old_server_id");
    };
    let Some(new_server_id) = required(query, "new_server_id") else {
        return bad_request("missing new_server_id");
    };

    route_store
        .begin_rollout(
            rollout_epoch.to_string(),
            old_server_id.to_string(),
            new_server_id.to_string(),
        )
        .await;
    write_plain("ok")
}

async fn handle_rollout_state(
    route_store: &ProxyRouteStore,
    query: &HashMap<String, String>,
) -> String {
    let Some(state) = required(query, "state") else {
        return bad_request("missing state");
    };

    let rollout_state = match state {
        "Active" => RolloutSessionState::Active,
        "Ending" => RolloutSessionState::Ending,
        "Interrupted" => RolloutSessionState::Interrupted,
        _ => return bad_request("invalid state"),
    };
    route_store.mark_rollout_state(rollout_state).await;
    write_plain("ok")
}

async fn handle_switch(route_store: &ProxyRouteStore, server_id: &str) -> String {
    let routes = route_store.list_routes().await;
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
    write_plain("ok")
}

async fn handle_room_route_upsert(
    route_store: &ProxyRouteStore,
    query: &HashMap<String, String>,
) -> String {
    let Some(room_id) = required(query, "room_id") else {
        return bad_request("missing room_id");
    };
    let Some(owner_server_id) = required(query, "owner_server_id") else {
        return bad_request("missing owner_server_id");
    };
    let migration_state = query
        .get("migration_state")
        .and_then(|value| RoomMigrationState::parse(value))
        .unwrap_or(RoomMigrationState::OwnedByNew);
    let member_count = parse_u32(query, "member_count").unwrap_or(0);
    let online_member_count = parse_u32(query, "online_member_count").unwrap_or(0);
    let empty_since_ms = parse_u64(query, "empty_since_ms");
    let room_version = parse_u64(query, "room_version").unwrap_or(1);
    let expected_room_version = parse_u64(query, "expected_room_version");
    let rollout_epoch = query.get("rollout_epoch").cloned().unwrap_or_default();
    let last_transfer_checksum = query
        .get("last_transfer_checksum")
        .cloned()
        .unwrap_or_default();
    let expected_last_transfer_checksum = query
        .get("expected_last_transfer_checksum")
        .filter(|value| !value.is_empty())
        .cloned();

    let result = route_store
        .upsert_room_route(RoomRouteRecord {
            room_id: room_id.to_string(),
            owner_server_id: owner_server_id.to_string(),
            migration_state,
            member_count,
            online_member_count,
            empty_since_ms,
            room_version,
            rollout_epoch,
            last_transfer_checksum,
            updated_at_ms: 0,
        }, expected_room_version, expected_last_transfer_checksum)
        .await;

    match result {
        Ok(()) => write_plain("ok"),
        Err(error_code) => bad_request(error_code),
    }
}

async fn handle_player_route_upsert(
    route_store: &ProxyRouteStore,
    query: &HashMap<String, String>,
) -> String {
    let Some(player_id) = required(query, "player_id") else {
        return bad_request("missing player_id");
    };
    let current_room_id = query
        .get("current_room_id")
        .filter(|value| !value.is_empty())
        .cloned();
    let preferred_server_id = query
        .get("preferred_server_id")
        .filter(|value| !value.is_empty())
        .cloned();
    let rollout_epoch = query.get("rollout_epoch").cloned().unwrap_or_default();

    let result = route_store
        .upsert_player_route(PlayerRouteRecord {
            player_id: player_id.to_string(),
            current_room_id,
            preferred_server_id,
            rollout_epoch,
            updated_at_ms: 0,
        })
        .await;

    match result {
        Ok(()) => write_plain("ok"),
        Err(error_code) => bad_request(error_code),
    }
}

fn required<'a>(query: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    query.get(key).map(String::as_str).filter(|value| !value.is_empty())
}

fn parse_u32(query: &HashMap<String, String>, key: &str) -> Option<u32> {
    query.get(key).and_then(|value| value.parse::<u32>().ok())
}

fn parse_u64(query: &HashMap<String, String>, key: &str) -> Option<u64> {
    query.get(key).and_then(|value| value.parse::<u64>().ok())
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

fn write_json<T: Serialize>(payload: T) -> String {
    http_response(200, "application/json", serde_json::to_string(&payload).unwrap())
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
