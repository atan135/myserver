use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::warn;

use crate::rollout_drain_status::{OldServerDrainStatusCheckSummary, OldServerDrainStatusChecker};
use crate::route_store::{
    CharacterRouteRecord, ProxyRouteStore, RolloutCompleteIfDrainedResult, RolloutDrainEvaluation,
    RolloutDrainStatus, RolloutEndSummary, RolloutSessionState, RoomRouteRecord,
};

mod audit;
mod assertion;
mod auth;
mod http;
mod query;
mod route_handlers;

pub use audit::{AdminAuditConfig, AdminAuditLogger};
pub(crate) use assertion::AdminAssertionVerifier;
pub use auth::AdminAuthConfig;

#[cfg(test)]
use audit::AdminAuditError;
use audit::{
    AdminRequestContext, admin_request_context, audit_error, audit_ok, audit_write_failed,
    audited_bad_request, audited_forbidden, audited_update_error,
};
#[cfg(test)]
use auth::{AdminPermission, authorize, authorize_method, is_authorized};
use auth::{admin_route_requirement, assertion_route_requirement, authorize_route, fallback_route_requirement};
use http::{http_response, split_path_and_query, write_json, write_json_status, write_plain};
use query::{required, required_identifier};
use route_handlers::{handle_character_route_upsert, handle_room_route_upsert, handle_switch};

#[derive(Serialize)]
struct StatusResponse {
    ok: bool,
    // Active frontend sessions, including pre-auth connections.
    connection_count: u64,
    maintenance: bool,
    active_upstream: Option<String>,
    rollout_session: Option<crate::route_store::RolloutSession>,
    room_route_count: usize,
    character_route_count: usize,
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
struct CharacterRoutesResponse {
    ok: bool,
    routes: Vec<CharacterRouteRecord>,
}

pub async fn run(
    bind_addr: &str,
    route_store: ProxyRouteStore,
    connection_count: Arc<AtomicU64>,
    maintenance: Arc<tokio::sync::RwLock<bool>>,
    auth_config: AdminAuthConfig,
    assertion_verifier: AdminAssertionVerifier,
    service_instance_id: String,
    audit_logger: AdminAuditLogger,
    old_server_drain_status_checker: Option<Arc<dyn OldServerDrainStatusChecker>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind(bind_addr).await?;
    let auth_config = Arc::new(auth_config);
    let assertion_verifier = Arc::new(assertion_verifier);
    let audit_logger = Arc::new(audit_logger);
    loop {
        let (socket, peer_addr) = listener.accept().await?;
        let route_store = route_store.clone();
        let connection_count = connection_count.clone();
        let maintenance = maintenance.clone();
        let auth_config = auth_config.clone();
        let assertion_verifier = assertion_verifier.clone();
        let service_instance_id = service_instance_id.clone();
        let audit_logger = audit_logger.clone();
        let old_server_drain_status_checker = old_server_drain_status_checker.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(
                socket,
                route_store,
                connection_count,
                maintenance,
                auth_config,
                assertion_verifier,
                service_instance_id,
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
    assertion_verifier: Arc<AdminAssertionVerifier>,
    service_instance_id: String,
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

    let mut context = admin_request_context(&request, method, route_path);
    let route_requirement = admin_route_requirement(method, route_path)
        .unwrap_or_else(|| fallback_route_requirement(method));
    if route_requirement.is_write {
        let (permission, target_type) = assertion_route_requirement(route_requirement);
        match assertion_verifier.verify_http_request(
            &request,
            method,
            path,
            permission,
            target_type,
            "game-proxy",
            &service_instance_id,
        ) {
            Ok(assertion_context) => context = assertion_context,
            Err(error) => {
                let response = if error.status() == 403 {
                    audited_forbidden(
                        &audit_logger,
                        &context,
                        route_requirement.action,
                        error.error_code(),
                    )
                    .await
                } else {
                    http_response(
                        error.status(),
                        "text/plain; charset=utf-8",
                        error.error_code().to_string(),
                    )
                };
                socket.write_all(response.as_bytes()).await?;
                return Ok(());
            }
        }
    } else if let Err((status, body)) = authorize_route(&request, route_requirement, auth_config.as_ref()) {
        let response = http_response(status, "text/plain; charset=utf-8", body.to_string());
        socket.write_all(response.as_bytes()).await?;
        return Ok(());
    }

    if method != "GET" {
        if let Err(error) = audit_logger.ensure_ready().await {
            warn!(
                method,
                path = route_path,
                error = %error,
                audit_path = %audit_logger.path().display(),
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
        if audit_logger.require_actor() && context.actor_missing {
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
                character_route_count: counts.character_routes,
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
        ("GET", "/character-routes") => write_json(CharacterRoutesResponse {
            ok: true,
            routes: route_store.list_character_routes().await,
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
                    if error.code() == "ROLLOUT_NOT_DRAINED" {
                        match audit_error(
                            &audit_logger,
                            &context,
                            "rollout_end",
                            error.code(),
                            None,
                            None,
                            None,
                            rollout_epoch.as_deref(),
                        )
                        .await
                        {
                            Ok(()) => http_response(
                                409,
                                "text/plain; charset=utf-8",
                                error.code().to_string(),
                            ),
                            Err(audit_error) => {
                                audit_write_failed(&audit_logger, "rollout_end", &audit_error)
                            }
                        }
                    } else {
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
        ("POST", "/character-route/upsert") => {
            handle_character_route_upsert(&route_store, &query, &audit_logger, &context).await
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
                log_rollout_complete_if_drained_rejected(
                    "no_active",
                    "NO_ACTIVE_ROLLOUT",
                    &evaluation,
                );
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
                log_rollout_complete_if_drained_rejected(
                    "blocked",
                    "ROLLOUT_NOT_DRAINED",
                    &evaluation,
                );
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
                    log_old_server_drain_status_blocked_completion(
                        &evaluation,
                        &old_server_drain_status,
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

async fn upstream_exists(route_store: &ProxyRouteStore, server_id: &str) -> bool {
    route_store
        .list_routes()
        .await
        .iter()
        .any(|route| route.server_id == server_id)
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

fn log_old_server_drain_status_blocked_completion(
    evaluation: &RolloutDrainEvaluation,
    old_server_drain_status: &OldServerDrainStatusCheckSummary,
) {
    warn!(
        update_source = "complete_rollout_if_drained",
        event = "blocked_by_old_server_drain_status",
        rollout_epoch = evaluation.rollout_epoch.as_deref().unwrap_or_default(),
        old_server_id = evaluation.old_server_id.as_deref().unwrap_or_default(),
        new_server_id = evaluation.new_server_id.as_deref().unwrap_or_default(),
        drain_status = evaluation.status.as_str(),
        blocked_room_count = evaluation.blocked_room_count,
        blocked_character_count = evaluation.blocked_character_count,
        stale_room_route_count = evaluation.stale_room_route_count,
        stale_character_route_count = evaluation.stale_character_route_count,
        removed_room_route_count = 0,
        removed_character_route_count = 0,
        remaining_room_route_count = 0,
        remaining_character_route_count = 0,
        status_code = ?old_server_drain_status.status_code,
        ok = ?old_server_drain_status.ok,
        owned_room_count = ?old_server_drain_status.owned_room_count,
        migrating_room_count = ?old_server_drain_status.migrating_room_count,
        retired_room_count = ?old_server_drain_status.retired_room_count,
        connection_count = ?old_server_drain_status.connection_count,
        error = old_server_drain_status.error.as_deref().unwrap_or_default(),
        "old server drain status check blocked proxy rollout completion"
    );
}

fn log_rollout_complete_if_drained_rejected(
    event: &'static str,
    error_code: &'static str,
    evaluation: &RolloutDrainEvaluation,
) {
    warn!(
        update_source = "complete_rollout_if_drained",
        event,
        error_code,
        rollout_epoch = evaluation.rollout_epoch.as_deref().unwrap_or_default(),
        old_server_id = evaluation.old_server_id.as_deref().unwrap_or_default(),
        new_server_id = evaluation.new_server_id.as_deref().unwrap_or_default(),
        drain_status = evaluation.status.as_str(),
        blocked_room_count = evaluation.blocked_room_count,
        blocked_character_count = evaluation.blocked_character_count,
        stale_room_route_count = evaluation.stale_room_route_count,
        stale_character_route_count = evaluation.stale_character_route_count,
        removed_room_route_count = 0,
        removed_character_route_count = 0,
        remaining_room_route_count = 0,
        remaining_character_route_count = 0,
        "proxy rollout complete-if-drained rejected"
    );
}

#[cfg(test)]
mod tests;
