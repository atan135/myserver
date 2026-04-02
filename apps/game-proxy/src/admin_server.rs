use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::warn;

use crate::route_store::{ProxyRouteStore, UpstreamState};

#[derive(Serialize)]
struct StatusResponse {
    ok: bool,
    connection_count: u64,
    maintenance: bool,
    active_upstream: Option<String>,
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
            if let Err(error) = handle_connection(socket, route_store, connection_count, maintenance).await {
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

    let response = if first_line.starts_with("GET /status") {
        let active = route_store
            .list_routes()
            .await
            .into_iter()
            .find(|route| route.state == UpstreamState::Active)
            .map(|route| route.server_id);
        write_json(StatusResponse {
            ok: true,
            connection_count: connection_count.load(Ordering::Relaxed),
            maintenance: *maintenance.read().await,
            active_upstream: active,
        })
    } else if first_line.starts_with("POST /maintenance/on") {
        *maintenance.write().await = true;
        write_plain("ok")
    } else if first_line.starts_with("POST /maintenance/off") {
        *maintenance.write().await = false;
        write_plain("ok")
    } else if let Some(server_id) = parse_switch_target(first_line) {
        let routes = route_store.list_routes().await;
        for route in &routes {
            let next_state = if route.server_id == server_id {
                UpstreamState::Active
            } else {
                UpstreamState::Draining
            };
            route_store.update_state(&route.server_id, next_state).await;
        }
        write_plain("ok")
    } else {
        http_response(404, "text/plain; charset=utf-8", "not found".to_string())
    };

    socket.write_all(response.as_bytes()).await?;
    Ok(())
}

fn parse_switch_target(first_line: &str) -> Option<&str> {
    if !first_line.starts_with("POST /switch/") {
        return None;
    }
    let path = first_line.split_whitespace().nth(1)?;
    path.strip_prefix("/switch/")
}

fn write_json<T: Serialize>(payload: T) -> String {
    http_response(200, "application/json", serde_json::to_string(&payload).unwrap())
}

fn write_plain(body: &str) -> String {
    http_response(200, "text/plain; charset=utf-8", body.to_string())
}

fn http_response(status: u16, content_type: &str, body: String) -> String {
    let reason = match status {
        200 => "OK",
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
