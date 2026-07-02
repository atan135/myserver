use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use super::http::{bad_request, forbidden, http_response};
use crate::route_store::RouteStoreUpdateError;

const MAX_ACTOR_LEN: usize = 128;
const DEFAULT_ADMIN_ACTOR: &str = "unknown";

#[derive(Clone)]
pub struct AdminAuditConfig {
    enabled: bool,
    path: PathBuf,
    require_actor: bool,
}

impl AdminAuditConfig {
    pub fn new(enabled: bool, path: impl Into<PathBuf>, require_actor: bool) -> Self {
        Self {
            enabled,
            path: path.into(),
            require_actor,
        }
    }
}

#[derive(Clone)]
pub struct AdminAuditLogger {
    config: AdminAuditConfig,
}

impl AdminAuditLogger {
    pub fn new(config: AdminAuditConfig) -> Self {
        Self { config }
    }

    pub(super) fn path(&self) -> &Path {
        &self.config.path
    }

    pub(super) fn require_actor(&self) -> bool {
        self.config.require_actor
    }

    pub(super) async fn ensure_ready(&self) -> Result<(), AdminAuditError> {
        if !self.config.enabled {
            return Ok(());
        }
        ensure_parent_dir(&self.config.path).await?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.path)
            .await
            .map_err(AdminAuditError::Io)?;
        Ok(())
    }

    async fn append(&self, event: &AdminAuditEvent<'_>) -> Result<(), AdminAuditError> {
        if !self.config.enabled {
            return Ok(());
        }
        ensure_parent_dir(&self.config.path).await?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.path)
            .await
            .map_err(AdminAuditError::Io)?;
        let mut line = serde_json::to_string(event).map_err(AdminAuditError::Serialize)?;
        line.push('\n');
        file.write_all(line.as_bytes())
            .await
            .map_err(AdminAuditError::Io)
    }
}

#[derive(Debug)]
pub(super) enum AdminAuditError {
    Io(std::io::Error),
    Serialize(serde_json::Error),
}

impl std::fmt::Display for AdminAuditError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{}", error),
            Self::Serialize(error) => write!(formatter, "{}", error),
        }
    }
}

impl std::error::Error for AdminAuditError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AdminRequestContext {
    pub(super) actor: String,
    pub(super) actor_missing: bool,
    pub(super) method: String,
    pub(super) path: String,
}

#[derive(Serialize)]
struct AdminAuditEvent<'a> {
    ts_ms: u64,
    actor: &'a str,
    actor_missing: bool,
    method: &'a str,
    path: &'a str,
    action: &'a str,
    result: &'a str,
    error: &'a str,
    server_id: &'a str,
    room_id: &'a str,
    character_id: &'a str,
    rollout_epoch: &'a str,
}

#[derive(Clone, Copy, Default)]
struct AuditTarget<'a> {
    server_id: Option<&'a str>,
    room_id: Option<&'a str>,
    character_id: Option<&'a str>,
    rollout_epoch: Option<&'a str>,
}

pub(super) async fn audit_ok(
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
    action: &'static str,
    server_id: Option<&str>,
    room_id: Option<&str>,
    character_id: Option<&str>,
    rollout_epoch: Option<&str>,
) -> Result<(), AdminAuditError> {
    info!(
        action,
        actor = %context.actor,
        actor_missing = context.actor_missing,
        server_id = %server_id.unwrap_or_default(),
        room_id = %room_id.unwrap_or_default(),
        character_id = %character_id.unwrap_or_default(),
        rollout_epoch = %rollout_epoch.unwrap_or_default(),
        result = "ok",
        "proxy admin write operation"
    );
    audit_logger
        .append(&AdminAuditEvent {
            ts_ms: unix_time_ms(),
            actor: &context.actor,
            actor_missing: context.actor_missing,
            method: &context.method,
            path: &context.path,
            action,
            result: "ok",
            error: "",
            server_id: server_id.unwrap_or_default(),
            room_id: room_id.unwrap_or_default(),
            character_id: character_id.unwrap_or_default(),
            rollout_epoch: rollout_epoch.unwrap_or_default(),
        })
        .await
}

pub(super) async fn audit_error(
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
    action: &'static str,
    error: &str,
    server_id: Option<&str>,
    room_id: Option<&str>,
    character_id: Option<&str>,
    rollout_epoch: Option<&str>,
) -> Result<(), AdminAuditError> {
    warn!(
        action,
        actor = %context.actor,
        actor_missing = context.actor_missing,
        server_id = %server_id.unwrap_or_default(),
        room_id = %room_id.unwrap_or_default(),
        character_id = %character_id.unwrap_or_default(),
        rollout_epoch = %rollout_epoch.unwrap_or_default(),
        result = "error",
        error,
        "proxy admin write operation failed"
    );
    audit_logger
        .append(&AdminAuditEvent {
            ts_ms: unix_time_ms(),
            actor: &context.actor,
            actor_missing: context.actor_missing,
            method: &context.method,
            path: &context.path,
            action,
            result: "error",
            error,
            server_id: server_id.unwrap_or_default(),
            room_id: room_id.unwrap_or_default(),
            character_id: character_id.unwrap_or_default(),
            rollout_epoch: rollout_epoch.unwrap_or_default(),
        })
        .await
}

pub(super) async fn audited_bad_request(
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
    action: &'static str,
    error: &'static str,
    server_id: Option<&str>,
    room_id: Option<&str>,
    character_id: Option<&str>,
    rollout_epoch: Option<&str>,
) -> String {
    let target = AuditTarget {
        server_id,
        room_id,
        character_id,
        rollout_epoch,
    };
    match audit_error(
        audit_logger,
        context,
        action,
        error,
        target.server_id,
        target.room_id,
        target.character_id,
        target.rollout_epoch,
    )
    .await
    {
        Ok(()) => bad_request(error),
        Err(audit_error) => audit_write_failed(audit_logger, action, &audit_error),
    }
}

pub(super) async fn audited_update_error(
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
    action: &'static str,
    error: &RouteStoreUpdateError,
    server_id: Option<&str>,
    room_id: Option<&str>,
    character_id: Option<&str>,
    rollout_epoch: Option<&str>,
) -> String {
    let error_code = error.code();
    match audit_error(
        audit_logger,
        context,
        action,
        error_code,
        server_id,
        room_id,
        character_id,
        rollout_epoch,
    )
    .await
    {
        Ok(()) => bad_request(error_code),
        Err(audit_error) => audit_write_failed(audit_logger, action, &audit_error),
    }
}

pub(super) async fn audited_forbidden(
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
    action: &'static str,
    error: &'static str,
) -> String {
    match audit_error(audit_logger, context, action, error, None, None, None, None).await {
        Ok(()) => forbidden(),
        Err(audit_error) => {
            warn!(
                action,
                error = %audit_error,
                audit_path = %audit_logger.path().display(),
                "proxy admin permission denial audit write failed"
            );
            forbidden()
        }
    }
}

pub(super) fn audit_write_failed(
    audit_logger: &AdminAuditLogger,
    action: &'static str,
    error: &AdminAuditError,
) -> String {
    warn!(
        action,
        error = %error,
        audit_path = %audit_logger.path().display(),
        "proxy admin audit write failed"
    );
    http_response(
        500,
        "text/plain; charset=utf-8",
        "admin audit write failed".to_string(),
    )
}

pub(super) fn admin_request_context(
    request: &str,
    method: &str,
    path: &str,
) -> AdminRequestContext {
    let actor = request_header(request, "x-admin-actor")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(normalize_actor);

    match actor {
        Some(actor) => AdminRequestContext {
            actor,
            actor_missing: false,
            method: method.to_string(),
            path: path.to_string(),
        },
        None => AdminRequestContext {
            actor: DEFAULT_ADMIN_ACTOR.to_string(),
            actor_missing: true,
            method: method.to_string(),
            path: path.to_string(),
        },
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

async fn ensure_parent_dir(path: &Path) -> Result<(), AdminAuditError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .await
            .map_err(AdminAuditError::Io)?;
    }
    Ok(())
}

fn normalize_actor(value: &str) -> Option<String> {
    if value.len() > MAX_ACTOR_LEN {
        return None;
    }
    value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@'))
        .then(|| value.to_string())
}

fn request_header<'a>(request: &'a str, header_name: &str) -> Option<&'a str> {
    request
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case(header_name)
                .then_some(value.trim())
        })
}
