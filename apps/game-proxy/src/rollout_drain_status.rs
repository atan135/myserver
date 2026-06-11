use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time;

pub const DEFAULT_ROLLOUT_DRAIN_STATUS_URL: &str =
    "http://127.0.0.1:3000/api/v1/internal/game-server/rollout-drain-status";
pub const DEFAULT_ROLLOUT_DRAIN_STATUS_TIMEOUT_MS: u64 = 3000;
pub const DEFAULT_ROLLOUT_DRAIN_STATUS_MAX_BODY_BYTES: usize = 1024 * 1024;

const MAX_HEADER_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolloutDrainStatusCheckConfig {
    pub enabled: bool,
    pub url: String,
    pub token: Option<String>,
    pub connect_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub overall_timeout_ms: u64,
    pub max_body_bytes: usize,
}

impl Default for RolloutDrainStatusCheckConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: DEFAULT_ROLLOUT_DRAIN_STATUS_URL.to_string(),
            token: None,
            connect_timeout_ms: DEFAULT_ROLLOUT_DRAIN_STATUS_TIMEOUT_MS,
            read_timeout_ms: DEFAULT_ROLLOUT_DRAIN_STATUS_TIMEOUT_MS,
            overall_timeout_ms: DEFAULT_ROLLOUT_DRAIN_STATUS_TIMEOUT_MS,
            max_body_bytes: DEFAULT_ROLLOUT_DRAIN_STATUS_MAX_BODY_BYTES,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OldServerDrainStatusCheckSummary {
    pub checked: bool,
    pub passed: bool,
    pub status_code: Option<u16>,
    pub ok: Option<bool>,
    pub rollout_epoch: Option<String>,
    pub owner_server_id: Option<String>,
    pub owned_room_count: Option<u64>,
    pub migrating_room_count: Option<u64>,
    pub connection_count: Option<u64>,
    pub drain_mode_enabled: Option<bool>,
    pub drain_mode_entered_at_ms: Option<u64>,
    pub drain_mode_reason: Option<String>,
    pub drain_mode_source: Option<String>,
    pub error: Option<String>,
}

impl OldServerDrainStatusCheckSummary {
    #[cfg(test)]
    pub fn passed() -> Self {
        Self {
            checked: true,
            passed: true,
            status_code: Some(200),
            ok: Some(true),
            rollout_epoch: None,
            owner_server_id: None,
            owned_room_count: Some(0),
            migrating_room_count: Some(0),
            connection_count: Some(0),
            drain_mode_enabled: Some(false),
            drain_mode_entered_at_ms: Some(0),
            drain_mode_reason: Some("rollout".to_string()),
            drain_mode_source: Some("admin".to_string()),
            error: None,
        }
    }

    #[cfg(test)]
    pub fn not_drained(connection_count: u64) -> Self {
        Self {
            checked: true,
            passed: false,
            status_code: Some(200),
            ok: Some(true),
            rollout_epoch: None,
            owner_server_id: None,
            owned_room_count: Some(0),
            migrating_room_count: Some(0),
            connection_count: Some(connection_count),
            drain_mode_enabled: Some(true),
            drain_mode_entered_at_ms: Some(1000),
            drain_mode_reason: Some("rollout".to_string()),
            drain_mode_source: Some("admin".to_string()),
            error: Some("OLD_SERVER_NOT_DRAINED".to_string()),
        }
    }

    #[cfg(test)]
    pub fn request_failed(error: impl Into<String>) -> Self {
        Self::failed(error)
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            checked: true,
            passed: false,
            status_code: None,
            ok: None,
            rollout_epoch: None,
            owner_server_id: None,
            owned_room_count: None,
            migrating_room_count: None,
            connection_count: None,
            drain_mode_enabled: None,
            drain_mode_entered_at_ms: None,
            drain_mode_reason: None,
            drain_mode_source: None,
            error: Some(error.into()),
        }
    }

    pub fn response_error_code(&self) -> &'static str {
        if matches!(self.status_code, Some(200..=299)) && self.ok == Some(true) {
            "OLD_SERVER_DRAIN_STATUS_NOT_DRAINED"
        } else {
            "OLD_SERVER_DRAIN_STATUS_CHECK_FAILED"
        }
    }
}

pub trait OldServerDrainStatusChecker: Send + Sync {
    fn check<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = OldServerDrainStatusCheckSummary> + Send + 'a>>;
}

#[derive(Clone, Debug)]
pub struct HttpOldServerDrainStatusChecker {
    config: RolloutDrainStatusCheckConfig,
}

impl HttpOldServerDrainStatusChecker {
    pub fn new(config: RolloutDrainStatusCheckConfig) -> Self {
        Self { config }
    }

    async fn check_once(&self) -> Result<OldServerDrainStatusCheckSummary, String> {
        let request = ParsedHttpUrl::parse(&self.config.url)?;
        let response = time::timeout(
            duration_ms(self.config.overall_timeout_ms),
            fetch_http_get(
                &request,
                self.config.token.as_deref(),
                duration_ms(self.config.connect_timeout_ms),
                duration_ms(self.config.read_timeout_ms),
                self.config.max_body_bytes,
            ),
        )
        .await
        .map_err(|_| "OVERALL_TIMEOUT".to_string())??;

        Ok(summary_from_http_response(response))
    }
}

impl OldServerDrainStatusChecker for HttpOldServerDrainStatusChecker {
    fn check<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = OldServerDrainStatusCheckSummary> + Send + 'a>> {
        Box::pin(async move {
            match self.check_once().await {
                Ok(summary) => summary,
                Err(error) => OldServerDrainStatusCheckSummary::failed(error),
            }
        })
    }
}

#[derive(Debug)]
struct ParsedHttpUrl {
    host: String,
    port: u16,
    path: String,
}

impl ParsedHttpUrl {
    fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        let Some(rest) = raw.strip_prefix("http://") else {
            return Err("ROLLOUT_DRAIN_STATUS_URL_SCHEME_UNSUPPORTED".to_string());
        };
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, "/".to_string()),
        };
        if authority.is_empty() {
            return Err("ROLLOUT_DRAIN_STATUS_URL_HOST_REQUIRED".to_string());
        }
        if has_invalid_request_component(authority) || has_invalid_request_component(&path) {
            return Err("ROLLOUT_DRAIN_STATUS_URL_INVALID".to_string());
        }

        let (host, port) = parse_authority(authority)?;
        Ok(Self { host, port, path })
    }
}

fn parse_authority(authority: &str) -> Result<(String, u16), String> {
    if authority.starts_with('[') {
        let Some(end) = authority.find(']') else {
            return Err("ROLLOUT_DRAIN_STATUS_URL_INVALID_IPV6".to_string());
        };
        let host = authority[..=end].to_string();
        let port = match authority[end + 1..].strip_prefix(':') {
            Some(port) if !port.is_empty() => parse_port(port)?,
            Some(_) => return Err("ROLLOUT_DRAIN_STATUS_URL_INVALID_PORT".to_string()),
            None if authority.len() == end + 1 => 80,
            None => return Err("ROLLOUT_DRAIN_STATUS_URL_INVALID".to_string()),
        };
        return Ok((host, port));
    }

    if let Some((host, port)) = authority.rsplit_once(':') {
        if host.is_empty() || port.is_empty() {
            return Err("ROLLOUT_DRAIN_STATUS_URL_INVALID".to_string());
        }
        return Ok((host.to_string(), parse_port(port)?));
    }

    Ok((authority.to_string(), 80))
}

fn parse_port(port: &str) -> Result<u16, String> {
    port.parse::<u16>()
        .map_err(|_| "ROLLOUT_DRAIN_STATUS_URL_INVALID_PORT".to_string())
}

fn has_invalid_request_component(value: &str) -> bool {
    value.bytes().any(|byte| byte <= 0x20 || byte == 0x7f)
}

fn duration_ms(value: u64) -> Duration {
    Duration::from_millis(value.max(1))
}

#[derive(Debug)]
struct HttpResponse {
    status_code: u16,
    body: Vec<u8>,
}

async fn fetch_http_get(
    request: &ParsedHttpUrl,
    token: Option<&str>,
    connect_timeout: Duration,
    read_timeout: Duration,
    max_body_bytes: usize,
) -> Result<HttpResponse, String> {
    if let Some(token) = token {
        if token.bytes().any(|byte| byte == b'\r' || byte == b'\n') {
            return Err("ROLLOUT_DRAIN_STATUS_TOKEN_INVALID".to_string());
        }
    }

    let mut stream = time::timeout(
        connect_timeout,
        TcpStream::connect((request.host.as_str(), request.port)),
    )
    .await
    .map_err(|_| "CONNECT_TIMEOUT".to_string())?
    .map_err(|error| format!("CONNECT_FAILED: {error}"))?;

    let mut request_head = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\n",
        request.path, request.host
    );
    if let Some(token) = token.filter(|value| !value.is_empty()) {
        request_head.push_str("X-Service-Token: ");
        request_head.push_str(token);
        request_head.push_str("\r\n");
    }
    request_head.push_str("\r\n");

    stream
        .write_all(request_head.as_bytes())
        .await
        .map_err(|error| format!("WRITE_FAILED: {error}"))?;

    read_http_response(stream, read_timeout, max_body_bytes.max(1)).await
}

async fn read_http_response(
    mut stream: TcpStream,
    read_timeout: Duration,
    max_body_bytes: usize,
) -> Result<HttpResponse, String> {
    let mut buffer = Vec::new();
    let header_end = loop {
        if let Some(header_end) = find_header_end(&buffer) {
            break header_end;
        }
        if buffer.len() > MAX_HEADER_BYTES {
            return Err("RESPONSE_HEADERS_TOO_LARGE".to_string());
        }
        let read = read_chunk(&mut stream, read_timeout).await?;
        if read.is_empty() {
            return Err("RESPONSE_HEADERS_INCOMPLETE".to_string());
        }
        buffer.extend_from_slice(&read);
    };

    let (status_code, headers) = parse_response_headers(&buffer[..header_end])?;
    let mut body = buffer[header_end..].to_vec();
    if body.len() > max_body_bytes {
        return Err("RESPONSE_BODY_TOO_LARGE".to_string());
    }

    if is_chunked(&headers) {
        read_until_eof(
            &mut stream,
            read_timeout,
            &mut body,
            max_body_bytes + MAX_HEADER_BYTES,
        )
        .await?;
        body = decode_chunked_body(&body, max_body_bytes)?;
    } else if let Some(content_length) = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        if content_length > max_body_bytes {
            return Err("RESPONSE_BODY_TOO_LARGE".to_string());
        }
        while body.len() < content_length {
            let read = read_chunk(&mut stream, read_timeout).await?;
            if read.is_empty() {
                return Err("RESPONSE_BODY_INCOMPLETE".to_string());
            }
            body.extend_from_slice(&read);
            if body.len() > max_body_bytes {
                return Err("RESPONSE_BODY_TOO_LARGE".to_string());
            }
        }
        body.truncate(content_length);
    } else {
        read_until_eof(&mut stream, read_timeout, &mut body, max_body_bytes).await?;
    }

    Ok(HttpResponse { status_code, body })
}

async fn read_chunk(stream: &mut TcpStream, read_timeout: Duration) -> Result<Vec<u8>, String> {
    let mut chunk = [0u8; 4096];
    let read = time::timeout(read_timeout, stream.read(&mut chunk))
        .await
        .map_err(|_| "READ_TIMEOUT".to_string())?
        .map_err(|error| format!("READ_FAILED: {error}"))?;
    Ok(chunk[..read].to_vec())
}

async fn read_until_eof(
    stream: &mut TcpStream,
    read_timeout: Duration,
    body: &mut Vec<u8>,
    max_bytes: usize,
) -> Result<(), String> {
    loop {
        let read = read_chunk(stream, read_timeout).await?;
        if read.is_empty() {
            return Ok(());
        }
        body.extend_from_slice(&read);
        if body.len() > max_bytes {
            return Err("RESPONSE_BODY_TOO_LARGE".to_string());
        }
    }
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn parse_response_headers(buffer: &[u8]) -> Result<(u16, HashMap<String, String>), String> {
    let text =
        std::str::from_utf8(buffer).map_err(|_| "RESPONSE_HEADERS_INVALID_UTF8".to_string())?;
    let mut lines = text.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| "RESPONSE_STATUS_MISSING".to_string())?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "RESPONSE_STATUS_INVALID".to_string())?
        .parse::<u16>()
        .map_err(|_| "RESPONSE_STATUS_INVALID".to_string())?;

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    Ok((status_code, headers))
}

fn is_chunked(headers: &HashMap<String, String>) -> bool {
    headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
}

fn decode_chunked_body(raw: &[u8], max_body_bytes: usize) -> Result<Vec<u8>, String> {
    let mut cursor = 0usize;
    let mut decoded = Vec::new();
    loop {
        let Some(line_end) = find_crlf(&raw[cursor..]) else {
            return Err("CHUNKED_BODY_INVALID".to_string());
        };
        let size_line = std::str::from_utf8(&raw[cursor..cursor + line_end])
            .map_err(|_| "CHUNKED_BODY_INVALID".to_string())?;
        let size_hex = size_line.split(';').next().unwrap_or_default().trim();
        let size =
            usize::from_str_radix(size_hex, 16).map_err(|_| "CHUNKED_BODY_INVALID".to_string())?;
        cursor += line_end + 2;
        if size == 0 {
            return Ok(decoded);
        }
        if raw.len().saturating_sub(cursor) < size + 2 {
            return Err("CHUNKED_BODY_INVALID".to_string());
        }
        decoded.extend_from_slice(&raw[cursor..cursor + size]);
        if decoded.len() > max_body_bytes {
            return Err("RESPONSE_BODY_TOO_LARGE".to_string());
        }
        cursor += size;
        if raw.get(cursor..cursor + 2) != Some(b"\r\n") {
            return Err("CHUNKED_BODY_INVALID".to_string());
        }
        cursor += 2;
    }
}

fn find_crlf(buffer: &[u8]) -> Option<usize> {
    buffer.windows(2).position(|window| window == b"\r\n")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthHttpDrainStatusResponse {
    ok: bool,
    error_code: Option<String>,
    rollout_epoch: Option<String>,
    owner_server_id: Option<String>,
    owned_room_count: Option<u64>,
    migrating_room_count: Option<u64>,
    connection_count: Option<u64>,
    drain_mode_enabled: Option<bool>,
    drain_mode_entered_at_ms: Option<u64>,
    drain_mode_reason: Option<String>,
    drain_mode_source: Option<String>,
}

fn summary_from_http_response(response: HttpResponse) -> OldServerDrainStatusCheckSummary {
    if !(200..=299).contains(&response.status_code) {
        return OldServerDrainStatusCheckSummary {
            checked: true,
            passed: false,
            status_code: Some(response.status_code),
            ok: None,
            rollout_epoch: None,
            owner_server_id: None,
            owned_room_count: None,
            migrating_room_count: None,
            connection_count: None,
            drain_mode_enabled: None,
            drain_mode_entered_at_ms: None,
            drain_mode_reason: None,
            drain_mode_source: None,
            error: Some(format!("HTTP_STATUS_{}", response.status_code)),
        };
    }

    let payload = match serde_json::from_slice::<AuthHttpDrainStatusResponse>(&response.body) {
        Ok(payload) => payload,
        Err(error) => {
            return OldServerDrainStatusCheckSummary {
                checked: true,
                passed: false,
                status_code: Some(response.status_code),
                ok: None,
                rollout_epoch: None,
                owner_server_id: None,
                owned_room_count: None,
                migrating_room_count: None,
                connection_count: None,
                drain_mode_enabled: None,
                drain_mode_entered_at_ms: None,
                drain_mode_reason: None,
                drain_mode_source: None,
                error: Some(format!("INVALID_JSON: {error}")),
            };
        }
    };

    let passed = payload.ok
        && payload.owned_room_count == Some(0)
        && payload.migrating_room_count == Some(0)
        && payload.connection_count == Some(0);
    let error = if passed {
        None
    } else if !payload.ok {
        Some(
            payload
                .error_code
                .clone()
                .unwrap_or_else(|| "OLD_SERVER_STATUS_NOT_OK".to_string()),
        )
    } else if payload.owned_room_count.is_none()
        || payload.migrating_room_count.is_none()
        || payload.connection_count.is_none()
    {
        Some("MISSING_DRAIN_STATUS_FIELDS".to_string())
    } else {
        Some("OLD_SERVER_NOT_DRAINED".to_string())
    };

    OldServerDrainStatusCheckSummary {
        checked: true,
        passed,
        status_code: Some(response.status_code),
        ok: Some(payload.ok),
        rollout_epoch: payload.rollout_epoch,
        owner_server_id: payload.owner_server_id,
        owned_room_count: payload.owned_room_count,
        migrating_room_count: payload.migrating_room_count,
        connection_count: payload.connection_count,
        drain_mode_enabled: payload.drain_mode_enabled,
        drain_mode_entered_at_ms: payload.drain_mode_entered_at_ms,
        drain_mode_reason: payload.drain_mode_reason,
        drain_mode_source: payload.drain_mode_source,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HttpResponse, OldServerDrainStatusCheckSummary, ParsedHttpUrl, summary_from_http_response,
    };

    #[test]
    fn parses_default_http_url() {
        let parsed =
            ParsedHttpUrl::parse("http://127.0.0.1:3000/api/v1/internal/game-server/status")
                .unwrap();

        assert_eq!(parsed.host, "127.0.0.1");
        assert_eq!(parsed.port, 3000);
        assert_eq!(parsed.path, "/api/v1/internal/game-server/status");
    }

    #[test]
    fn response_summary_passes_only_when_real_counts_are_zero() {
        let summary = summary_from_http_response(HttpResponse {
            status_code: 200,
            body: br#"{"ok":true,"ownedRoomCount":0,"migratingRoomCount":0,"connectionCount":0,"drainModeEnabled":false,"drainModeEnteredAtMs":0,"drainModeReason":"rollout","drainModeSource":"admin"}"#.to_vec(),
        });

        assert_eq!(summary, OldServerDrainStatusCheckSummary::passed());
    }

    #[test]
    fn response_summary_blocks_nonzero_connections() {
        let summary = summary_from_http_response(HttpResponse {
            status_code: 200,
            body: br#"{"ok":true,"ownedRoomCount":0,"migratingRoomCount":0,"connectionCount":1,"drainModeEnabled":true,"drainModeEnteredAtMs":123,"drainModeReason":"rollout","drainModeSource":"admin"}"#.to_vec(),
        });

        assert!(!summary.passed);
        assert_eq!(summary.connection_count, Some(1));
        assert_eq!(summary.error.as_deref(), Some("OLD_SERVER_NOT_DRAINED"));
    }

    #[test]
    fn response_error_code_treats_not_ok_as_check_failure() {
        let summary = summary_from_http_response(HttpResponse {
            status_code: 200,
            body: br#"{"ok":false,"errorCode":"GAME_SERVER_UNAVAILABLE"}"#.to_vec(),
        });

        assert!(!summary.passed);
        assert_eq!(summary.ok, Some(false));
        assert_eq!(
            summary.response_error_code(),
            "OLD_SERVER_DRAIN_STATUS_CHECK_FAILED"
        );
    }
}
