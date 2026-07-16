use std::path::{Path, PathBuf};

use serde::Serialize;
use tokio::fs::{self, OpenOptions};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use super::auth::AdminAuthContext;
use super::current_unix_ms_u64;
use super::protocol_io::{write_error, write_message};
use crate::protocol::{MessageType, Packet};

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
            .map_err(AdminAuditError::Io)?;
        // Tokio file writes can remain buffered after `write_all`; admin callers and tests may
        // immediately query this audit record, so complete the flush before acknowledging it.
        file.flush().await.map_err(AdminAuditError::Io)
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct AdminAuditTarget {
    pub(super) room_id: String,
    pub(super) player_id: String,
    pub(super) character_id: String,
    pub(super) rollout_epoch: String,
    pub(super) checksum: String,
    pub(super) target_server_id: String,
    pub(super) config_key: String,
}

#[derive(Serialize)]
struct AdminAuditEvent<'a> {
    timestamp_ms: u64,
    channel: &'static str,
    action: &'a str,
    actor: &'a str,
    actor_missing: bool,
    ok: bool,
    error_code: &'a str,
    room_id: &'a str,
    player_id: &'a str,
    character_id: &'a str,
    rollout_epoch: &'a str,
    checksum: &'a str,
    target_server_id: &'a str,
    config_key: &'a str,
    seq: u32,
    message_type: u16,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum AdminWritePreflightError {
    ActorRequired,
    AuditUnavailable,
}

impl AdminWritePreflightError {
    pub(super) fn error_code(&self) -> &'static str {
        match self {
            Self::ActorRequired => "ADMIN_ACTOR_REQUIRED",
            Self::AuditUnavailable => "ADMIN_AUDIT_WRITE_FAILED",
        }
    }

    pub(super) fn message(&self) -> &'static str {
        match self {
            Self::ActorRequired => "admin actor is required for write operations",
            Self::AuditUnavailable => "admin audit log is not writable",
        }
    }
}

pub(super) async fn ensure_admin_write_allowed(
    audit_logger: &AdminAuditLogger,
    context: &AdminAuthContext,
    packet: &Packet,
    action: &'static str,
) -> Result<(), AdminWritePreflightError> {
    if let Err(error) = audit_logger.ensure_ready().await {
        warn!(
            action,
            seq = packet.header.seq,
            message_type = packet.header.msg_type,
            error = %error,
            audit_path = %audit_logger.path().display(),
            "game-server admin audit log is not writable"
        );
        return Err(AdminWritePreflightError::AuditUnavailable);
    }

    if audit_logger.require_actor() && context.actor_missing {
        match audit_admin_write_result(
            audit_logger,
            context,
            packet,
            action,
            false,
            "ADMIN_ACTOR_REQUIRED",
            &AdminAuditTarget::default(),
        )
        .await
        {
            Ok(()) => Err(AdminWritePreflightError::ActorRequired),
            Err(error) => {
                warn!(
                    action,
                    seq = packet.header.seq,
                    message_type = packet.header.msg_type,
                    error = %error,
                    audit_path = %audit_logger.path().display(),
                    "game-server admin actor rejection audit write failed"
                );
                Err(AdminWritePreflightError::AuditUnavailable)
            }
        }
    } else {
        Ok(())
    }
}

pub(super) async fn audit_admin_write_result(
    audit_logger: &AdminAuditLogger,
    context: &AdminAuthContext,
    packet: &Packet,
    action: &'static str,
    ok: bool,
    error_code: &str,
    target: &AdminAuditTarget,
) -> Result<(), AdminAuditError> {
    if ok {
        info!(
            channel = "admin_tcp",
            action,
            actor = %context.actor,
            actor_missing = context.actor_missing,
            ok,
            error_code,
            room_id = %target.room_id,
            player_id = %target.player_id,
            character_id = %target.character_id,
            rollout_epoch = %target.rollout_epoch,
            checksum = %target.checksum,
            target_server_id = %target.target_server_id,
            config_key = %target.config_key,
            seq = packet.header.seq,
            message_type = packet.header.msg_type,
            "game-server admin write operation"
        );
    } else {
        warn!(
            channel = "admin_tcp",
            action,
            actor = %context.actor,
            actor_missing = context.actor_missing,
            ok,
            error_code,
            room_id = %target.room_id,
            player_id = %target.player_id,
            character_id = %target.character_id,
            rollout_epoch = %target.rollout_epoch,
            checksum = %target.checksum,
            target_server_id = %target.target_server_id,
            config_key = %target.config_key,
            seq = packet.header.seq,
            message_type = packet.header.msg_type,
            "game-server admin write operation failed"
        );
    }

    audit_logger
        .append(&AdminAuditEvent {
            timestamp_ms: current_unix_ms_u64(),
            channel: "admin_tcp",
            action,
            actor: &context.actor,
            actor_missing: context.actor_missing,
            ok,
            error_code,
            room_id: &target.room_id,
            player_id: &target.player_id,
            character_id: &target.character_id,
            rollout_epoch: &target.rollout_epoch,
            checksum: &target.checksum,
            target_server_id: &target.target_server_id,
            config_key: &target.config_key,
            seq: packet.header.seq,
            message_type: packet.header.msg_type,
        })
        .await
}

async fn write_admin_audit_failure(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    seq: u32,
    audit_logger: &AdminAuditLogger,
    action: &'static str,
    error: &AdminAuditError,
) -> Result<(), std::io::Error> {
    warn!(
        action,
        error = %error,
        audit_path = %audit_logger.path().display(),
        "game-server admin audit write failed"
    );
    write_error(
        writer,
        seq,
        "ADMIN_AUDIT_WRITE_FAILED",
        "admin audit write failed",
    )
    .await
}

pub(super) async fn audit_then_write_error(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    audit_logger: &AdminAuditLogger,
    context: &AdminAuthContext,
    packet: &Packet,
    action: &'static str,
    error_code: &str,
    message: &str,
    target: &AdminAuditTarget,
) -> Result<(), std::io::Error> {
    match audit_admin_write_result(
        audit_logger,
        context,
        packet,
        action,
        false,
        error_code,
        target,
    )
    .await
    {
        Ok(()) => write_error(writer, packet.header.seq, error_code, message).await,
        Err(error) => {
            write_admin_audit_failure(writer, packet.header.seq, audit_logger, action, &error).await
        }
    }
}

pub(super) async fn audit_then_write_message<M: prost::Message>(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    audit_logger: &AdminAuditLogger,
    context: &AdminAuthContext,
    packet: &Packet,
    action: &'static str,
    message_type: MessageType,
    message: &M,
    ok: bool,
    error_code: &str,
    target: &AdminAuditTarget,
) -> Result<(), std::io::Error> {
    match audit_admin_write_result(
        audit_logger,
        context,
        packet,
        action,
        ok,
        error_code,
        target,
    )
    .await
    {
        Ok(()) => write_message(writer, message_type, packet.header.seq, message).await,
        Err(error) => {
            write_admin_audit_failure(writer, packet.header.seq, audit_logger, action, &error).await
        }
    }
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
