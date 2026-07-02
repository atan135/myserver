use std::collections::HashMap;

use serde::Serialize;

pub(super) fn split_path_and_query(path: &str) -> (&str, HashMap<String, String>) {
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

pub(super) fn write_json<T: Serialize>(payload: T) -> String {
    write_json_status(200, payload)
}

pub(super) fn write_json_status<T: Serialize>(status: u16, payload: T) -> String {
    http_response(
        status,
        "application/json",
        serde_json::to_string(&payload).unwrap(),
    )
}

pub(super) fn write_plain(body: &str) -> String {
    http_response(200, "text/plain; charset=utf-8", body.to_string())
}

pub(super) fn bad_request(body: &str) -> String {
    http_response(400, "text/plain; charset=utf-8", body.to_string())
}

pub(super) fn forbidden() -> String {
    http_response(
        403,
        "text/plain; charset=utf-8",
        "insufficient admin permission".to_string(),
    )
}

pub(super) fn http_response(status: u16, content_type: &str, body: String) -> String {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        409 => "Conflict",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
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
