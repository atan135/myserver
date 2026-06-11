use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::fs::{self, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

use crate::config::{AdminPermissionScope, AdminScopedTokenConfig};
use crate::rollout_drain_status::{OldServerDrainStatusCheckSummary, OldServerDrainStatusChecker};
use crate::route_store::{
    PlayerRouteRecord, ProxyRouteStore, RolloutCompleteIfDrainedResult, RolloutDrainEvaluation,
    RolloutDrainStatus, RolloutEndSummary, RolloutSessionState, RoomMigrationState,
    RoomRouteRecord, RouteStoreUpdateError, UpstreamOperationState,
};

const MAX_ID_LEN: usize = 128;
const MAX_CHECKSUM_LEN: usize = 256;
const MAX_ROOM_MEMBER_COUNT: u32 = 1_000_000;
const MAX_ACTOR_LEN: usize = 128;
const DEFAULT_ADMIN_ACTOR: &str = "unknown";

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
    drain_evaluation: RolloutDrainEvaluation,
}

#[derive(Serialize)]
struct RolloutCompleteIfDrainedResponse {
    ok: bool,
    error: Option<&'static str>,
    drain_evaluation: RolloutDrainEvaluation,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_server_drain_status: Option<OldServerDrainStatusCheckSummary>,
    end_summary: Option<RolloutEndSummary>,
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

#[derive(Clone)]
pub struct AdminAuthConfig {
    write_token: String,
    read_token: Option<String>,
    scoped_tokens: Vec<AdminScopedToken>,
}

#[derive(Clone)]
struct AdminScopedToken {
    token: String,
    permissions: Vec<AdminPermissionScope>,
}

impl AdminAuthConfig {
    #[cfg(test)]
    pub fn new(write_token: String, read_token: Option<String>) -> Self {
        Self::with_scoped_tokens(write_token, read_token, Vec::new())
    }

    pub fn with_scoped_tokens(
        write_token: String,
        read_token: Option<String>,
        scoped_tokens: Vec<AdminScopedTokenConfig>,
    ) -> Self {
        Self {
            write_token,
            read_token: read_token.filter(|token| !token.trim().is_empty()),
            scoped_tokens: scoped_tokens
                .into_iter()
                .filter(|entry| !entry.token.trim().is_empty())
                .map(|entry| AdminScopedToken {
                    token: entry.token,
                    permissions: entry.permissions,
                })
                .collect(),
        }
    }
}

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

    async fn ensure_ready(&self) -> Result<(), AdminAuditError> {
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
enum AdminAuditError {
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
struct AdminRequestContext {
    actor: String,
    actor_missing: bool,
    method: String,
    path: String,
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
    player_id: &'a str,
    rollout_epoch: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AdminPermission {
    Read,
    Write,
    Scoped(Vec<AdminPermissionScope>),
}

impl AdminPermission {
    fn allows(&self, required: AdminPermissionScope) -> bool {
        match self {
            Self::Write => true,
            Self::Read => required == AdminPermissionScope::Read,
            Self::Scoped(permissions) => permissions
                .iter()
                .any(|permission| permission_grants(*permission, required)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AdminRouteRequirement {
    permission: AdminPermissionScope,
    action: &'static str,
    is_write: bool,
}

pub async fn run(
    bind_addr: &str,
    route_store: ProxyRouteStore,
    connection_count: Arc<AtomicU64>,
    maintenance: Arc<tokio::sync::RwLock<bool>>,
    auth_config: AdminAuthConfig,
    audit_logger: AdminAuditLogger,
    old_server_drain_status_checker: Option<Arc<dyn OldServerDrainStatusChecker>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(bind_addr).await?;
    let auth_config = Arc::new(auth_config);
    let audit_logger = Arc::new(audit_logger);
    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let route_store = route_store.clone();
        let connection_count = connection_count.clone();
        let maintenance = maintenance.clone();
        let auth_config = auth_config.clone();
        let audit_logger = audit_logger.clone();
        let old_server_drain_status_checker = old_server_drain_status_checker.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(
                socket,
                route_store,
                connection_count,
                maintenance,
                auth_config,
                audit_logger,
                old_server_drain_status_checker,
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
    auth_config: Arc<AdminAuthConfig>,
    audit_logger: Arc<AdminAuditLogger>,
    old_server_drain_status_checker: Option<Arc<dyn OldServerDrainStatusChecker>>,
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

    let context = admin_request_context(&request, method, route_path);
    let route_requirement = admin_route_requirement(method, route_path)
        .unwrap_or_else(|| fallback_route_requirement(method));
    if let Err((status, body)) = authorize_route(&request, route_requirement, auth_config.as_ref())
    {
        let response = if status == 403 && route_requirement.is_write {
            audited_forbidden(
                &audit_logger,
                &context,
                route_requirement.action,
                "insufficient_permission",
            )
            .await
        } else {
            if status == 403 {
                warn!(
                    method,
                    path = route_path,
                    action = route_requirement.action,
                    result = "error",
                    error = "insufficient_permission",
                    "proxy admin operation rejected"
                );
            }
            http_response(status, "text/plain; charset=utf-8", body.to_string())
        };
        socket.write_all(response.as_bytes()).await?;
        return Ok(());
    }

    if method != "GET" {
        if let Err(error) = audit_logger.ensure_ready().await {
            warn!(
                method,
                path = route_path,
                error = %error,
                audit_path = %audit_logger.config.path.display(),
                "proxy admin audit log is not writable"
            );
            let response = http_response(
                500,
                "text/plain; charset=utf-8",
                "admin audit unavailable".to_string(),
            );
            socket.write_all(response.as_bytes()).await?;
            return Ok(());
        }
        if audit_logger.config.require_actor && context.actor_missing {
            let response = audited_bad_request(
                &audit_logger,
                &context,
                "admin_actor_required",
                "missing X-Admin-Actor",
                None,
                None,
                None,
                None,
            )
            .await;
            socket.write_all(response.as_bytes()).await?;
            return Ok(());
        }
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
            drain_evaluation: route_store.evaluate_rollout_drain().await,
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
            match audit_ok(
                &audit_logger,
                &context,
                "maintenance_on",
                None,
                None,
                None,
                None,
            )
            .await
            {
                Ok(()) => {
                    *maintenance.write().await = true;
                    write_plain("ok")
                }
                Err(error) => audit_write_failed(&audit_logger, "maintenance_on", &error),
            }
        }
        ("POST", "/maintenance/off") => {
            match audit_ok(
                &audit_logger,
                &context,
                "maintenance_off",
                None,
                None,
                None,
                None,
            )
            .await
            {
                Ok(()) => {
                    *maintenance.write().await = false;
                    write_plain("ok")
                }
                Err(error) => audit_write_failed(&audit_logger, "maintenance_off", &error),
            }
        }
        ("POST", "/rollout/start") => {
            handle_rollout_start(&route_store, &query, &audit_logger, &context).await
        }
        ("POST", "/rollout/end") => {
            let rollout_epoch = route_store
                .get_rollout_session()
                .await
                .map(|session| session.rollout_epoch);
            match route_store.end_rollout().await {
                Ok(()) => {
                    match audit_ok(
                        &audit_logger,
                        &context,
                        "rollout_end",
                        None,
                        None,
                        None,
                        rollout_epoch.as_deref(),
                    )
                    .await
                    {
                        Ok(()) => write_plain("ok"),
                        Err(error) => audit_write_failed(&audit_logger, "rollout_end", &error),
                    }
                }
                Err(error) => {
                    audited_update_error(
                        &audit_logger,
                        &context,
                        "rollout_end",
                        &error,
                        None,
                        None,
                        None,
                        rollout_epoch.as_deref(),
                    )
                    .await
                }
            }
        }
        ("POST", "/rollout/state") => {
            handle_rollout_state(&route_store, &query, &audit_logger, &context).await
        }
        ("POST", "/rollout/complete-if-drained") => {
            handle_rollout_complete_if_drained(
                &route_store,
                &audit_logger,
                &context,
                old_server_drain_status_checker.as_deref(),
            )
            .await
        }
        ("POST", "/room-route/upsert") => {
            handle_room_route_upsert(&route_store, &query, &audit_logger, &context).await
        }
        ("POST", "/player-route/upsert") => {
            handle_player_route_upsert(&route_store, &query, &audit_logger, &context).await
        }
        ("POST", route_path) => {
            if let Some(server_id) = route_path.strip_prefix("/switch/") {
                handle_switch(&route_store, server_id, &audit_logger, &context).await
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
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
) -> String {
    let action = "rollout_start";
    let rollout_epoch = match required_identifier(query, "rollout_epoch") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                None,
                None,
                None,
                None,
            )
            .await;
        }
    };
    let old_server_id = match required_identifier(query, "old_server_id") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                None,
                None,
                None,
                Some(rollout_epoch),
            )
            .await;
        }
    };
    let new_server_id = match required_identifier(query, "new_server_id") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                Some(old_server_id),
                None,
                None,
                Some(rollout_epoch),
            )
            .await;
        }
    };

    if old_server_id == new_server_id {
        return audited_bad_request(
            audit_logger,
            context,
            action,
            "old_server_id and new_server_id must differ",
            Some(old_server_id),
            None,
            None,
            Some(rollout_epoch),
        )
        .await;
    }

    for server_id in [old_server_id, new_server_id] {
        if !upstream_exists(route_store, server_id).await {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                "unknown upstream server_id",
                Some(server_id),
                None,
                None,
                Some(rollout_epoch),
            )
            .await;
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
            match audit_ok(
                audit_logger,
                context,
                action,
                Some(new_server_id),
                None,
                None,
                Some(rollout_epoch),
            )
            .await
            {
                Ok(()) => write_plain("ok"),
                Err(error) => audit_write_failed(audit_logger, action, &error),
            }
        }
        Err(error) => {
            audited_update_error(
                audit_logger,
                context,
                action,
                &error,
                Some(new_server_id),
                None,
                None,
                Some(rollout_epoch),
            )
            .await
        }
    }
}

async fn handle_rollout_state(
    route_store: &ProxyRouteStore,
    query: &HashMap<String, String>,
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
) -> String {
    let action = "rollout_state";
    let Some(state) = required(query, "state") else {
        return audited_bad_request(
            audit_logger,
            context,
            action,
            "missing state",
            None,
            None,
            None,
            None,
        )
        .await;
    };

    let rollout_state = match state {
        "Active" => RolloutSessionState::Active,
        "Ending" => RolloutSessionState::Ending,
        "Interrupted" => RolloutSessionState::Interrupted,
        _ => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                "invalid state",
                None,
                None,
                None,
                None,
            )
            .await;
        }
    };
    let Some(session) = route_store.get_rollout_session().await else {
        return audited_bad_request(
            audit_logger,
            context,
            action,
            "no active rollout",
            None,
            None,
            None,
            None,
        )
        .await;
    };
    match route_store.mark_rollout_state(rollout_state).await {
        Ok(()) => {
            match audit_ok(
                audit_logger,
                context,
                action,
                None,
                None,
                None,
                Some(session.rollout_epoch.as_str()),
            )
            .await
            {
                Ok(()) => write_plain("ok"),
                Err(error) => audit_write_failed(audit_logger, action, &error),
            }
        }
        Err(error) => {
            audited_update_error(
                audit_logger,
                context,
                action,
                &error,
                None,
                None,
                None,
                Some(session.rollout_epoch.as_str()),
            )
            .await
        }
    }
}

async fn handle_rollout_complete_if_drained(
    route_store: &ProxyRouteStore,
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
    old_server_drain_status_checker: Option<&dyn OldServerDrainStatusChecker>,
) -> String {
    let action = "rollout_complete_if_drained";
    if let Some(old_server_drain_status_checker) = old_server_drain_status_checker {
        let evaluation = route_store.evaluate_rollout_drain().await;
        match evaluation.status {
            RolloutDrainStatus::NoActiveRollout => {
                return audited_rollout_complete_if_drained_rejected(
                    audit_logger,
                    context,
                    "NO_ACTIVE_ROLLOUT",
                    400,
                    evaluation,
                    None,
                )
                .await;
            }
            RolloutDrainStatus::Blocked => {
                return audited_rollout_complete_if_drained_rejected(
                    audit_logger,
                    context,
                    "ROLLOUT_NOT_DRAINED",
                    409,
                    evaluation,
                    None,
                )
                .await;
            }
            RolloutDrainStatus::Drained => {
                let old_server_drain_status = old_server_drain_status_checker.check().await;
                if !old_server_drain_status.passed {
                    warn!(
                        rollout_epoch = evaluation.rollout_epoch.as_deref().unwrap_or_default(),
                        old_server_id = evaluation.old_server_id.as_deref().unwrap_or_default(),
                        status_code = ?old_server_drain_status.status_code,
                        ok = ?old_server_drain_status.ok,
                        owned_room_count = ?old_server_drain_status.owned_room_count,
                        migrating_room_count = ?old_server_drain_status.migrating_room_count,
                        connection_count = ?old_server_drain_status.connection_count,
                        error = old_server_drain_status.error.as_deref().unwrap_or_default(),
                        "old server drain status check blocked proxy rollout completion"
                    );
                    return audited_rollout_complete_if_drained_rejected(
                        audit_logger,
                        context,
                        old_server_drain_status.response_error_code(),
                        409,
                        evaluation,
                        Some(old_server_drain_status),
                    )
                    .await;
                }

                match route_store.complete_rollout_if_drained().await {
                    Ok(RolloutCompleteIfDrainedResult::Completed {
                        evaluation,
                        end_summary,
                    }) => {
                        match audit_ok(
                            audit_logger,
                            context,
                            action,
                            end_summary.new_server_id.as_deref(),
                            None,
                            None,
                            end_summary.rollout_epoch.as_deref(),
                        )
                        .await
                        {
                            Ok(()) => {
                                return write_json(RolloutCompleteIfDrainedResponse {
                                    ok: true,
                                    error: None,
                                    drain_evaluation: evaluation,
                                    old_server_drain_status: Some(old_server_drain_status),
                                    end_summary: Some(end_summary),
                                });
                            }
                            Err(error) => return audit_write_failed(audit_logger, action, &error),
                        }
                    }
                    Ok(RolloutCompleteIfDrainedResult::Blocked { evaluation }) => {
                        return audited_rollout_complete_if_drained_rejected(
                            audit_logger,
                            context,
                            "ROLLOUT_NOT_DRAINED",
                            409,
                            evaluation,
                            Some(old_server_drain_status),
                        )
                        .await;
                    }
                    Ok(RolloutCompleteIfDrainedResult::NoActiveRollout { evaluation }) => {
                        return audited_rollout_complete_if_drained_rejected(
                            audit_logger,
                            context,
                            "NO_ACTIVE_ROLLOUT",
                            400,
                            evaluation,
                            Some(old_server_drain_status),
                        )
                        .await;
                    }
                    Err(error) => {
                        let rollout_epoch = route_store
                            .get_rollout_session()
                            .await
                            .map(|session| session.rollout_epoch);
                        return audited_update_error(
                            audit_logger,
                            context,
                            action,
                            &error,
                            None,
                            None,
                            None,
                            rollout_epoch.as_deref(),
                        )
                        .await;
                    }
                }
            }
        }
    }

    match route_store.complete_rollout_if_drained().await {
        Ok(RolloutCompleteIfDrainedResult::Completed {
            evaluation,
            end_summary,
        }) => {
            match audit_ok(
                audit_logger,
                context,
                action,
                end_summary.new_server_id.as_deref(),
                None,
                None,
                end_summary.rollout_epoch.as_deref(),
            )
            .await
            {
                Ok(()) => write_json(RolloutCompleteIfDrainedResponse {
                    ok: true,
                    error: None,
                    drain_evaluation: evaluation,
                    old_server_drain_status: None,
                    end_summary: Some(end_summary),
                }),
                Err(error) => audit_write_failed(audit_logger, action, &error),
            }
        }
        Ok(RolloutCompleteIfDrainedResult::Blocked { evaluation }) => {
            audited_rollout_complete_if_drained_rejected(
                audit_logger,
                context,
                "ROLLOUT_NOT_DRAINED",
                409,
                evaluation,
                None,
            )
            .await
        }
        Ok(RolloutCompleteIfDrainedResult::NoActiveRollout { evaluation }) => {
            audited_rollout_complete_if_drained_rejected(
                audit_logger,
                context,
                "NO_ACTIVE_ROLLOUT",
                400,
                evaluation,
                None,
            )
            .await
        }
        Err(error) => {
            let rollout_epoch = route_store
                .get_rollout_session()
                .await
                .map(|session| session.rollout_epoch);
            audited_update_error(
                audit_logger,
                context,
                action,
                &error,
                None,
                None,
                None,
                rollout_epoch.as_deref(),
            )
            .await
        }
    }
}

async fn handle_switch(
    route_store: &ProxyRouteStore,
    server_id: &str,
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
) -> String {
    let action = "switch";
    let server_id = match validate_identifier("server_id", server_id) {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                None,
                None,
                None,
                None,
            )
            .await;
        }
    };
    let routes = route_store.list_routes().await;
    if !routes.iter().any(|route| route.server_id == server_id) {
        return audited_bad_request(
            audit_logger,
            context,
            action,
            "unknown upstream server_id",
            Some(server_id),
            None,
            None,
            None,
        )
        .await;
    }

    if let Err(error) = audit_ok(
        audit_logger,
        context,
        action,
        Some(server_id),
        None,
        None,
        None,
    )
    .await
    {
        return audit_write_failed(audit_logger, action, &error);
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
    write_plain("ok")
}

async fn handle_room_route_upsert(
    route_store: &ProxyRouteStore,
    query: &HashMap<String, String>,
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
) -> String {
    let action = "room_route_upsert";
    let room_id = match required_identifier(query, "room_id") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                None,
                None,
                None,
                None,
            )
            .await;
        }
    };
    let owner_server_id = match required_identifier(query, "owner_server_id") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                None,
                Some(room_id),
                None,
                None,
            )
            .await;
        }
    };
    if !upstream_exists(route_store, owner_server_id).await {
        return audited_bad_request(
            audit_logger,
            context,
            action,
            "unknown upstream owner_server_id",
            Some(owner_server_id),
            Some(room_id),
            None,
            None,
        )
        .await;
    }

    let migration_state = match optional_migration_state(query, "migration_state") {
        Ok(value) => value.unwrap_or(RoomMigrationState::OwnedByNew),
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            )
            .await;
        }
    };
    let member_count = match optional_u32(query, "member_count") {
        Ok(value) => value.unwrap_or(0),
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            )
            .await;
        }
    };
    let online_member_count = match optional_u32(query, "online_member_count") {
        Ok(value) => value.unwrap_or(0),
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            )
            .await;
        }
    };
    if member_count > MAX_ROOM_MEMBER_COUNT {
        return audited_bad_request(
            audit_logger,
            context,
            action,
            "member_count out of range",
            Some(owner_server_id),
            Some(room_id),
            None,
            None,
        )
        .await;
    }
    if online_member_count > member_count {
        return audited_bad_request(
            audit_logger,
            context,
            action,
            "online_member_count cannot exceed member_count",
            Some(owner_server_id),
            Some(room_id),
            None,
            None,
        )
        .await;
    }
    let empty_since_ms = match optional_u64(query, "empty_since_ms") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            )
            .await;
        }
    };
    let room_version = match optional_u64(query, "room_version") {
        Ok(value) => value.unwrap_or(1),
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            )
            .await;
        }
    };
    if room_version == 0 {
        return audited_bad_request(
            audit_logger,
            context,
            action,
            "room_version must be greater than 0",
            Some(owner_server_id),
            Some(room_id),
            None,
            None,
        )
        .await;
    }
    let expected_room_version = match optional_u64(query, "expected_room_version") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            )
            .await;
        }
    };
    let rollout_epoch = match optional_identifier(query, "rollout_epoch") {
        Ok(value) => value.unwrap_or_default(),
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                Some(owner_server_id),
                Some(room_id),
                None,
                None,
            )
            .await;
        }
    };
    let last_transfer_checksum =
        match optional_bounded_text(query, "last_transfer_checksum", MAX_CHECKSUM_LEN) {
            Ok(value) => value.unwrap_or_default(),
            Err(error) => {
                return audited_bad_request(
                    audit_logger,
                    context,
                    action,
                    error,
                    Some(owner_server_id),
                    Some(room_id),
                    None,
                    Some(rollout_epoch.as_str()),
                )
                .await;
            }
        };
    let expected_last_transfer_checksum =
        match optional_bounded_text(query, "expected_last_transfer_checksum", MAX_CHECKSUM_LEN) {
            Ok(value) => value,
            Err(error) => {
                return audited_bad_request(
                    audit_logger,
                    context,
                    action,
                    error,
                    Some(owner_server_id),
                    Some(room_id),
                    None,
                    Some(rollout_epoch.as_str()),
                )
                .await;
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
            match audit_ok(
                audit_logger,
                context,
                action,
                Some(owner_server_id),
                Some(room_id),
                None,
                Some(rollout_epoch.as_str()),
            )
            .await
            {
                Ok(()) => write_plain("ok"),
                Err(error) => audit_write_failed(audit_logger, action, &error),
            }
        }
        Err(error) => {
            audited_update_error(
                audit_logger,
                context,
                action,
                &error,
                Some(owner_server_id),
                Some(room_id),
                None,
                Some(rollout_epoch.as_str()),
            )
            .await
        }
    }
}

async fn handle_player_route_upsert(
    route_store: &ProxyRouteStore,
    query: &HashMap<String, String>,
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
) -> String {
    let action = "player_route_upsert";
    let player_id = match required_identifier(query, "player_id") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                None,
                None,
                None,
                None,
            )
            .await;
        }
    };
    let current_room_id = match optional_identifier(query, "current_room_id") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                None,
                None,
                Some(player_id),
                None,
            )
            .await;
        }
    };
    let preferred_server_id = match optional_identifier(query, "preferred_server_id") {
        Ok(value) => value,
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                None,
                None,
                Some(player_id),
                None,
            )
            .await;
        }
    };
    if let Some(server_id) = preferred_server_id.as_deref() {
        if !upstream_exists(route_store, server_id).await {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                "unknown upstream preferred_server_id",
                Some(server_id),
                current_room_id.as_deref(),
                Some(player_id),
                None,
            )
            .await;
        }
    }
    let rollout_epoch = match optional_identifier(query, "rollout_epoch") {
        Ok(value) => value.unwrap_or_default(),
        Err(error) => {
            return audited_bad_request(
                audit_logger,
                context,
                action,
                error,
                preferred_server_id.as_deref(),
                current_room_id.as_deref(),
                Some(player_id),
                None,
            )
            .await;
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
            match audit_ok(
                audit_logger,
                context,
                action,
                preferred_server_id.as_deref(),
                current_room_id.as_deref(),
                Some(player_id),
                Some(rollout_epoch.as_str()),
            )
            .await
            {
                Ok(()) => write_plain("ok"),
                Err(error) => audit_write_failed(audit_logger, action, &error),
            }
        }
        Err(error) => {
            audited_update_error(
                audit_logger,
                context,
                action,
                &error,
                preferred_server_id.as_deref(),
                current_room_id.as_deref(),
                Some(player_id),
                Some(rollout_epoch.as_str()),
            )
            .await
        }
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

#[derive(Clone, Copy, Default)]
struct AuditTarget<'a> {
    server_id: Option<&'a str>,
    room_id: Option<&'a str>,
    player_id: Option<&'a str>,
    rollout_epoch: Option<&'a str>,
}

async fn audit_ok(
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
    action: &'static str,
    server_id: Option<&str>,
    room_id: Option<&str>,
    player_id: Option<&str>,
    rollout_epoch: Option<&str>,
) -> Result<(), AdminAuditError> {
    info!(
        action,
        actor = %context.actor,
        actor_missing = context.actor_missing,
        server_id = %server_id.unwrap_or_default(),
        room_id = %room_id.unwrap_or_default(),
        player_id = %player_id.unwrap_or_default(),
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
            player_id: player_id.unwrap_or_default(),
            rollout_epoch: rollout_epoch.unwrap_or_default(),
        })
        .await
}

async fn audit_error(
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
    action: &'static str,
    error: &str,
    server_id: Option<&str>,
    room_id: Option<&str>,
    player_id: Option<&str>,
    rollout_epoch: Option<&str>,
) -> Result<(), AdminAuditError> {
    warn!(
        action,
        actor = %context.actor,
        actor_missing = context.actor_missing,
        server_id = %server_id.unwrap_or_default(),
        room_id = %room_id.unwrap_or_default(),
        player_id = %player_id.unwrap_or_default(),
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
            player_id: player_id.unwrap_or_default(),
            rollout_epoch: rollout_epoch.unwrap_or_default(),
        })
        .await
}

async fn audited_bad_request(
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
    action: &'static str,
    error: &'static str,
    server_id: Option<&str>,
    room_id: Option<&str>,
    player_id: Option<&str>,
    rollout_epoch: Option<&str>,
) -> String {
    let target = AuditTarget {
        server_id,
        room_id,
        player_id,
        rollout_epoch,
    };
    match audit_error(
        audit_logger,
        context,
        action,
        error,
        target.server_id,
        target.room_id,
        target.player_id,
        target.rollout_epoch,
    )
    .await
    {
        Ok(()) => bad_request(error),
        Err(audit_error) => audit_write_failed(audit_logger, action, &audit_error),
    }
}

async fn audited_update_error(
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
    action: &'static str,
    error: &RouteStoreUpdateError,
    server_id: Option<&str>,
    room_id: Option<&str>,
    player_id: Option<&str>,
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
        player_id,
        rollout_epoch,
    )
    .await
    {
        Ok(()) => bad_request(error_code),
        Err(audit_error) => audit_write_failed(audit_logger, action, &audit_error),
    }
}

async fn audited_rollout_complete_if_drained_rejected(
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
    error: &'static str,
    status: u16,
    evaluation: RolloutDrainEvaluation,
    old_server_drain_status: Option<OldServerDrainStatusCheckSummary>,
) -> String {
    match audit_error(
        audit_logger,
        context,
        "rollout_complete_if_drained",
        error,
        evaluation.new_server_id.as_deref(),
        None,
        None,
        evaluation.rollout_epoch.as_deref(),
    )
    .await
    {
        Ok(()) => write_json_status(
            status,
            RolloutCompleteIfDrainedResponse {
                ok: false,
                error: Some(error),
                drain_evaluation: evaluation,
                old_server_drain_status,
                end_summary: None,
            },
        ),
        Err(audit_error) => {
            audit_write_failed(audit_logger, "rollout_complete_if_drained", &audit_error)
        }
    }
}

async fn audited_forbidden(
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
                audit_path = %audit_logger.config.path.display(),
                "proxy admin permission denial audit write failed"
            );
            forbidden()
        }
    }
}

fn audit_write_failed(
    audit_logger: &AdminAuditLogger,
    action: &'static str,
    error: &AdminAuditError,
) -> String {
    warn!(
        action,
        error = %error,
        audit_path = %audit_logger.config.path.display(),
        "proxy admin audit write failed"
    );
    http_response(
        500,
        "text/plain; charset=utf-8",
        "admin audit write failed".to_string(),
    )
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

fn admin_request_context(request: &str, method: &str, path: &str) -> AdminRequestContext {
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

fn authorize(request: &str, auth_config: &AdminAuthConfig) -> Option<AdminPermission> {
    let write_token = auth_config.write_token.trim();
    if write_token.is_empty() {
        return None;
    }

    if request_contains_query_token(request) {
        return None;
    }

    let matches_write = request
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .any(|line| header_matches_token(line, write_token));
    if matches_write {
        return Some(AdminPermission::Write);
    }

    if let Some(read_token) = auth_config.read_token.as_deref().map(str::trim) {
        if !read_token.is_empty()
            && request
                .lines()
                .skip(1)
                .take_while(|line| !line.is_empty())
                .any(|line| header_matches_token(line, read_token))
        {
            return Some(AdminPermission::Read);
        }
    }

    auth_config
        .scoped_tokens
        .iter()
        .find(|entry| {
            let token = entry.token.trim();
            !token.is_empty()
                && request
                    .lines()
                    .skip(1)
                    .take_while(|line| !line.is_empty())
                    .any(|line| header_matches_token(line, token))
        })
        .map(|entry| AdminPermission::Scoped(entry.permissions.clone()))
}

fn authorize_route<'a>(
    request: &str,
    route_requirement: AdminRouteRequirement,
    auth_config: &AdminAuthConfig,
) -> Result<AdminPermission, (u16, &'a str)> {
    let Some(permission) = authorize(request, auth_config) else {
        return Err((401, "missing or invalid admin token"));
    };

    if !permission.allows(route_requirement.permission) {
        return Err((403, "insufficient admin permission"));
    }

    Ok(permission)
}

fn admin_route_requirement(method: &str, route_path: &str) -> Option<AdminRouteRequirement> {
    match (method, route_path) {
        ("GET", "/status")
        | ("GET", "/instances")
        | ("GET", "/rollout")
        | ("GET", "/room-routes")
        | ("GET", "/player-routes") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::Read,
            action: "admin_read",
            is_write: false,
        }),
        ("POST", "/maintenance/on") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::MaintenanceWrite,
            action: "maintenance_on",
            is_write: true,
        }),
        ("POST", "/maintenance/off") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::MaintenanceWrite,
            action: "maintenance_off",
            is_write: true,
        }),
        ("POST", "/rollout/start") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::RolloutWrite,
            action: "rollout_start",
            is_write: true,
        }),
        ("POST", "/rollout/end") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::RolloutWrite,
            action: "rollout_end",
            is_write: true,
        }),
        ("POST", "/rollout/state") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::RolloutWrite,
            action: "rollout_state",
            is_write: true,
        }),
        ("POST", "/rollout/complete-if-drained") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::RolloutWrite,
            action: "rollout_complete_if_drained",
            is_write: true,
        }),
        ("POST", "/room-route/upsert") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::RouteWrite,
            action: "room_route_upsert",
            is_write: true,
        }),
        ("POST", "/player-route/upsert") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::RouteWrite,
            action: "player_route_upsert",
            is_write: true,
        }),
        ("POST", route_path) if route_path.strip_prefix("/switch/").is_some() => {
            Some(AdminRouteRequirement {
                permission: AdminPermissionScope::RouteWrite,
                action: "switch",
                is_write: true,
            })
        }
        _ => None,
    }
}

fn fallback_route_requirement(method: &str) -> AdminRouteRequirement {
    if method == "GET" {
        AdminRouteRequirement {
            permission: AdminPermissionScope::Read,
            action: "admin_read",
            is_write: false,
        }
    } else {
        AdminRouteRequirement {
            permission: AdminPermissionScope::Write,
            action: "admin_write",
            is_write: true,
        }
    }
}

fn permission_grants(granted: AdminPermissionScope, required: AdminPermissionScope) -> bool {
    match granted {
        AdminPermissionScope::All => true,
        AdminPermissionScope::Write => required != AdminPermissionScope::Read,
        _ => granted == required,
    }
}

#[allow(dead_code)]
fn authorize_method<'a>(
    request: &str,
    method: &str,
    auth_config: &AdminAuthConfig,
) -> Result<AdminPermission, (u16, &'a str)> {
    authorize_route(request, fallback_route_requirement(method), auth_config)
}

#[cfg(test)]
fn is_authorized(request: &str, admin_token: &str) -> bool {
    authorize(
        request,
        &AdminAuthConfig::new(admin_token.to_string(), None),
    )
    .is_some()
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
    write_json_status(200, payload)
}

fn write_json_status<T: Serialize>(status: u16, payload: T) -> String {
    http_response(
        status,
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

fn forbidden() -> String {
    http_response(
        403,
        "text/plain; charset=utf-8",
        "insufficient admin permission".to_string(),
    )
}

fn http_response(status: u16, content_type: &str, body: String) -> String {
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

#[cfg(test)]
mod tests {
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
        admin_request_context, admin_route_requirement, audit_ok, audit_write_failed, authorize,
        authorize_method, authorize_route, handle_connection, handle_player_route_upsert,
        handle_rollout_complete_if_drained, handle_rollout_start, handle_rollout_state,
        handle_room_route_upsert, handle_switch, is_authorized, split_path_and_query,
    };
    use crate::config::{AdminPermissionScope, AdminScopedTokenConfig};
    use crate::rollout_drain_status::{
        OldServerDrainStatusCheckSummary, OldServerDrainStatusChecker,
    };
    use crate::route_store::{
        ProxyRouteStore, RolloutDrainStatus, RolloutSessionState, RoomMigrationState,
        RoomRouteRecord, UpstreamHealthState, UpstreamOperationState, UpstreamRoute,
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
    fn write_token_allows_get_and_post_admin_requests() {
        let get = format!("GET /status HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n");
        let post =
            format!("POST /maintenance/on HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n");

        assert_eq!(
            authorize_method(&get, "GET", &auth_config()),
            Ok(AdminPermission::Write)
        );
        assert_eq!(
            authorize_method(&post, "POST", &auth_config()),
            Ok(AdminPermission::Write)
        );
    }

    #[test]
    fn read_token_allows_get_admin_requests() {
        let request = format!("GET /status HTTP/1.1\r\nauthorization: Bearer {READ_TOKEN}\r\n\r\n");

        assert_eq!(
            authorize_method(&request, "GET", &auth_config()),
            Ok(AdminPermission::Read)
        );
    }

    #[test]
    fn read_token_rejects_post_admin_requests_with_forbidden() {
        let request =
            format!("POST /maintenance/on HTTP/1.1\r\nauthorization: Bearer {READ_TOKEN}\r\n\r\n");

        assert_eq!(
            authorize_method(&request, "POST", &auth_config()),
            Err((403, "insufficient admin permission"))
        );
    }

    #[test]
    fn read_token_rejects_non_get_admin_requests_with_forbidden() {
        let request =
            format!("DELETE /rollout HTTP/1.1\r\nauthorization: Bearer {READ_TOKEN}\r\n\r\n");

        assert_eq!(
            authorize_method(&request, "DELETE", &auth_config()),
            Err((403, "insufficient admin permission"))
        );
    }

    #[test]
    fn scoped_maintenance_token_allows_only_maintenance_writes() {
        let token = "maintenance-token";
        let config = scoped_auth_config(token, vec![AdminPermissionScope::MaintenanceWrite]);
        let request =
            format!("POST /maintenance/on HTTP/1.1\r\nauthorization: Bearer {token}\r\n\r\n");
        let maintenance = admin_route_requirement("POST", "/maintenance/on").unwrap();
        let rollout = admin_route_requirement("POST", "/rollout/start").unwrap();
        let route = admin_route_requirement("POST", "/room-route/upsert").unwrap();
        let switch = admin_route_requirement("POST", "/switch/game-server-2").unwrap();

        assert_eq!(
            authorize_route(&request, maintenance, &config),
            Ok(AdminPermission::Scoped(vec![
                AdminPermissionScope::MaintenanceWrite
            ]))
        );
        assert_eq!(
            authorize_route(&request, rollout, &config),
            Err((403, "insufficient admin permission"))
        );
        assert_eq!(
            authorize_route(&request, route, &config),
            Err((403, "insufficient admin permission"))
        );
        assert_eq!(
            authorize_route(&request, switch, &config),
            Err((403, "insufficient admin permission"))
        );
    }

    #[test]
    fn scoped_rollout_token_allows_rollout_but_not_route_writes() {
        let token = "rollout-token";
        let config = scoped_auth_config(token, vec![AdminPermissionScope::RolloutWrite]);
        let request = format!("POST /rollout/start HTTP/1.1\r\nx-admin-token: {token}\r\n\r\n");
        let rollout_start = admin_route_requirement("POST", "/rollout/start").unwrap();
        let rollout_end = admin_route_requirement("POST", "/rollout/end").unwrap();
        let rollout_complete =
            admin_route_requirement("POST", "/rollout/complete-if-drained").unwrap();
        let route = admin_route_requirement("POST", "/player-route/upsert").unwrap();

        assert!(authorize_route(&request, rollout_start, &config).is_ok());
        assert!(authorize_route(&request, rollout_end, &config).is_ok());
        assert!(authorize_route(&request, rollout_complete, &config).is_ok());
        assert_eq!(
            authorize_route(&request, route, &config),
            Err((403, "insufficient admin permission"))
        );
    }

    #[test]
    fn scoped_route_token_allows_route_writes_and_switch_only() {
        let token = "route-token";
        let config = scoped_auth_config(token, vec![AdminPermissionScope::RouteWrite]);
        let request =
            format!("POST /room-route/upsert HTTP/1.1\r\nauthorization: Bearer {token}\r\n\r\n");
        let room_route = admin_route_requirement("POST", "/room-route/upsert").unwrap();
        let player_route = admin_route_requirement("POST", "/player-route/upsert").unwrap();
        let switch = admin_route_requirement("POST", "/switch/game-server-2").unwrap();
        let maintenance = admin_route_requirement("POST", "/maintenance/off").unwrap();

        assert!(authorize_route(&request, room_route, &config).is_ok());
        assert!(authorize_route(&request, player_route, &config).is_ok());
        assert!(authorize_route(&request, switch, &config).is_ok());
        assert_eq!(
            authorize_route(&request, maintenance, &config),
            Err((403, "insufficient admin permission"))
        );
    }

    #[test]
    fn scoped_read_and_wildcard_permissions_work() {
        let read_token = "scoped-read-token";
        let read_config = scoped_auth_config(read_token, vec![AdminPermissionScope::Read]);
        let read_request =
            format!("GET /status HTTP/1.1\r\nauthorization: Bearer {read_token}\r\n\r\n");
        let read = admin_route_requirement("GET", "/status").unwrap();
        let maintenance = admin_route_requirement("POST", "/maintenance/on").unwrap();

        assert!(authorize_route(&read_request, read, &read_config).is_ok());
        assert_eq!(
            authorize_route(&read_request, maintenance, &read_config),
            Err((403, "insufficient admin permission"))
        );

        let all_token = "all-token";
        let all_config = scoped_auth_config(all_token, vec![AdminPermissionScope::All]);
        let all_request =
            format!("POST /maintenance/on HTTP/1.1\r\nx-admin-token: {all_token}\r\n\r\n");

        assert!(authorize_route(&all_request, read, &all_config).is_ok());
        assert!(authorize_route(&all_request, maintenance, &all_config).is_ok());
    }

    #[test]
    fn rejects_admin_token_from_query_string() {
        let request =
            format!("GET /status?admin_token={TOKEN} HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n");

        assert!(!is_authorized(&request, TOKEN));
        assert_eq!(authorize(&request, &auth_config()), None);
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
            handle_rollout_complete_if_drained(&store, &audit_logger, &complete_context, None)
                .await;
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
            handle_rollout_complete_if_drained(&store, &audit_logger, &complete_context, None)
                .await;
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
        let checker = MockOldServerDrainStatusChecker::with_result(
            OldServerDrainStatusCheckSummary::passed(),
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
            handle_rollout_complete_if_drained(&store, &audit_logger, &complete_context, None)
                .await;
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
            &query(
                "/room-route/upsert?room_id=room-1&owner_server_id=game-server-1&member_count=abc",
            ),
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
    async fn player_route_upsert_rejects_invalid_query_without_write() {
        let store = route_store().await;
        let audit_logger = test_audit_logger();
        let context = test_context("/player-route/upsert");

        let bad_player = handle_player_route_upsert(
            &store,
            &query("/player-route/upsert?player_id=bad/player&preferred_server_id=game-server-1"),
            &audit_logger,
            &context,
        )
        .await;
        assert_eq!(status_code(&bad_player), 400);

        let unknown_server = handle_player_route_upsert(
            &store,
            &query("/player-route/upsert?player_id=player-1&preferred_server_id=missing"),
            &audit_logger,
            &context,
        )
        .await;
        assert_eq!(status_code(&unknown_server), 400);

        assert!(store.list_player_routes().await.is_empty());
    }

    #[tokio::test]
    async fn player_route_upsert_accepts_valid_query() {
        let store = route_store().await;
        let audit_logger = test_audit_logger();
        let context = test_context("/player-route/upsert");

        let response = handle_player_route_upsert(
            &store,
            &query(
                "/player-route/upsert?player_id=player-1&current_room_id=room-1&preferred_server_id=game-server-1",
            ),
            &audit_logger,
            &context,
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
    async fn required_actor_rejects_write_before_state_change() {
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

        assert_eq!(status_code(&response), 400);
        assert!(response.ends_with("missing X-Admin-Actor"));

        let content = tokio::fs::read_to_string(&audit_path).await.unwrap();
        let event: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(event["actor"], "unknown");
        assert_eq!(event["actor_missing"], true);
        assert_eq!(event["path"], "/maintenance/on");
        assert_eq!(event["action"], "admin_actor_required");
        assert_eq!(event["result"], "error");
        assert_eq!(event["error"], "missing X-Admin-Actor");

        let _ = tokio::fs::remove_file(&audit_path).await;
    }

    #[tokio::test]
    async fn scoped_permission_denial_is_audited_without_token() {
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

        assert_eq!(status_code(&response), 403);
        assert!(response.ends_with("insufficient admin permission"));

        let content = tokio::fs::read_to_string(&audit_path).await.unwrap();
        assert!(!content.contains(scoped_token));
        let event: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(event["actor"], "ops@example.com");
        assert_eq!(event["method"], "POST");
        assert_eq!(event["path"], "/rollout/start");
        assert_eq!(event["action"], "rollout_start");
        assert_eq!(event["result"], "error");
        assert_eq!(event["error"], "insufficient_permission");

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
}
