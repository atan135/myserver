use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tracing::{info, warn};

const MAX_REQUEST_BYTES: usize = 2048;
const READY_RESPONSE: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 12\r\nconnection: close\r\n\r\n{\"ok\":true}\n";
const NOT_FOUND_RESPONSE: &[u8] =
    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

/// Starts an internal readiness endpoint when MYSERVER_READINESS_BIND_ADDR is set.
/// The caller must invoke this only after its critical startup work has succeeded.
pub async fn spawn_from_env(service: &str) -> io::Result<Option<JoinHandle<()>>> {
    let bind_addr = match std::env::var("MYSERVER_READINESS_BIND_ADDR") {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => return Ok(None),
    };
    let listener = TcpListener::bind(&bind_addr).await?;
    let service = service.to_string();

    info!(service = %service, addr = %bind_addr, "internal readiness endpoint listening");
    Ok(Some(tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    tokio::spawn(async move {
                        if let Err(error) = handle_connection(stream).await {
                            warn!(peer = %peer, error = %error, "readiness request failed");
                        }
                    });
                }
                Err(error) => {
                    warn!(service = %service, error = %error, "readiness listener accept failed")
                }
            }
        }
    })))
}

async fn handle_connection(mut stream: tokio::net::TcpStream) -> io::Result<()> {
    let mut request = [0u8; MAX_REQUEST_BYTES];
    let received = stream.read(&mut request).await?;
    let response = if request_targets_readyz(&request[..received]) {
        READY_RESPONSE
    } else {
        NOT_FOUND_RESPONSE
    };
    stream.write_all(response).await?;
    stream.shutdown().await
}

fn request_targets_readyz(request: &[u8]) -> bool {
    let Ok(request) = std::str::from_utf8(request) else {
        return false;
    };
    matches!(
        request.lines().next(),
        Some("GET /readyz HTTP/1.1") | Some("GET /readyz HTTP/1.0")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_accepts_get_readyz_requests() {
        assert!(request_targets_readyz(
            b"GET /readyz HTTP/1.1\r\nHost: localhost\r\n\r\n"
        ));
        assert!(!request_targets_readyz(b"POST /readyz HTTP/1.1\r\n\r\n"));
        assert!(!request_targets_readyz(b"GET /healthz HTTP/1.1\r\n\r\n"));
    }
}
