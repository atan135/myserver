use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::health::{HealthConfig, HealthSnapshot, HealthState};

const MAX_REQUEST_BYTES: usize = 2048;

/// Compatibility wrapper for services not migrated to the dynamic model yet.
pub async fn spawn_from_env(service: &str) -> io::Result<Option<JoinHandle<()>>> {
    let instance_id = std::env::var("SERVICE_INSTANCE_ID").unwrap_or_else(|_| service.to_string());
    let state = HealthState::new(
        service,
        instance_id,
        HealthConfig::for_tests(1, 0, u64::MAX),
        [],
    );
    spawn_health_from_env(state).await
}

/// Starts `/livez` and `/readyz` on the configured internal health listener.
pub async fn spawn_health_from_env(state: HealthState) -> io::Result<Option<JoinHandle<()>>> {
    let bind_addr = health_bind_addr();
    let Some(bind_addr) = bind_addr else {
        return Ok(None);
    };
    let listener = TcpListener::bind(&bind_addr).await?;
    let identity = state.snapshot();

    info!(
        service = %identity.service,
        instance_id = %identity.instance_id,
        addr = %bind_addr,
        "internal health endpoint listening"
    );
    Ok(Some(tokio::spawn(async move {
        let mut health_tick = tokio::time::interval(std::time::Duration::from_secs(1));
        health_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = health_tick.tick() => {
                    state.snapshot();
                }
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, peer)) => {
                            let state = state.clone();
                            tokio::spawn(async move {
                                if let Err(error) = handle_connection(stream, state).await {
                                    warn!(peer = %peer, error = %error, "health request failed");
                                }
                            });
                        }
                        Err(error) => {
                            let identity = state.snapshot();
                            warn!(
                                service = %identity.service,
                                instance_id = %identity.instance_id,
                                error = %error,
                                "health listener accept failed"
                            );
                        }
                    }
                }
            }
        }
    })))
}

fn health_bind_addr() -> Option<String> {
    ["MYSERVER_HEALTH_BIND_ADDR", "MYSERVER_READINESS_BIND_ADDR"]
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
}

async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    state: HealthState,
) -> io::Result<()> {
    let mut request = [0u8; MAX_REQUEST_BYTES];
    let received = stream.read(&mut request).await?;
    let response = render_response(&request[..received], &state.snapshot());
    stream.write_all(&response).await?;
    stream.shutdown().await
}

fn render_response(request: &[u8], snapshot: &HealthSnapshot) -> Vec<u8> {
    match request_path(request) {
        Some("/livez") => json_response(
            if snapshot.live { 200 } else { 503 },
            &LiveResponse {
                service: &snapshot.service,
                instance_id: &snapshot.instance_id,
                state: snapshot.state,
                live: snapshot.live,
            },
        ),
        Some("/readyz") => json_response(if snapshot.ready { 200 } else { 503 }, snapshot),
        _ => b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_vec(),
    }
}

#[derive(serde::Serialize)]
struct LiveResponse<'a> {
    service: &'a str,
    instance_id: &'a str,
    state: crate::StartupState,
    live: bool,
}

fn json_response(status: u16, value: &impl serde::Serialize) -> Vec<u8> {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{\"ready\":false}".to_vec());
    let reason = match status {
        200 => "OK",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    };
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(&body);
    response
}

fn request_path(request: &[u8]) -> Option<&str> {
    let request = std::str::from_utf8(request).ok()?;
    let first = request.lines().next()?;
    let mut parts = first.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let path = parts.next()?;
    matches!(path, "/livez" | "/readyz").then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DependencySpec, StartupErrorCode};

    fn response_status(response: &[u8]) -> &str {
        std::str::from_utf8(response)
            .unwrap()
            .lines()
            .next()
            .unwrap()
    }

    #[test]
    fn livez_is_independent_from_required_dependency_readiness() {
        let state = HealthState::new(
            "game-server",
            "game-1",
            HealthConfig::for_tests(100, 0, 100),
            [DependencySpec::required("match-service", "grpc")],
        );
        let snapshot = state.snapshot();

        let live = render_response(b"GET /livez HTTP/1.1\r\n\r\n", &snapshot);
        let ready = render_response(b"GET /readyz HTTP/1.1\r\n\r\n", &snapshot);
        assert_eq!(response_status(&live), "HTTP/1.1 200 OK");
        assert_eq!(response_status(&ready), "HTTP/1.1 503 Service Unavailable");
    }

    #[test]
    fn readyz_serializes_only_structured_safe_errors() {
        let state = HealthState::new(
            "game-proxy",
            "proxy-1",
            HealthConfig::for_tests(100, 0, 100),
            [DependencySpec::required("game-server", "proxy-local")],
        );
        state.mark_pending(
            "game-server",
            "proxy-local",
            StartupErrorCode::RegistryUnavailable,
        );
        let response = render_response(b"GET /readyz HTTP/1.1\r\n\r\n", &state.snapshot());
        let text = std::str::from_utf8(&response).unwrap();
        assert!(text.contains("REGISTRY_UNAVAILABLE"));
        assert!(!text.contains("redis://"));
        assert!(!text.contains("password"));
    }

    #[test]
    fn unknown_and_non_get_paths_are_not_found() {
        let state = HealthState::new(
            "service",
            "instance",
            HealthConfig::for_tests(100, 0, 100),
            [],
        );
        for request in [
            b"GET /healthz HTTP/1.1\r\n\r\n".as_slice(),
            b"POST /readyz HTTP/1.1\r\n\r\n",
        ] {
            assert_eq!(
                response_status(&render_response(request, &state.snapshot())),
                "HTTP/1.1 404 Not Found"
            );
        }
    }
}
