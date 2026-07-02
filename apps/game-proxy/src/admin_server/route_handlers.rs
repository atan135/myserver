use std::collections::HashMap;

use super::audit::{
    AdminRequestContext, audit_ok, audit_write_failed, audited_bad_request, audited_update_error,
};
use super::http::write_plain;
use super::query::{
    optional_bounded_text, optional_identifier, optional_migration_state, optional_u32,
    optional_u64, required_identifier, validate_identifier,
};
use super::{AdminAuditLogger, upstream_exists};
use crate::route_store::{
    CharacterRouteRecord, ProxyRouteStore, RoomMigrationState, RoomRouteRecord,
    UpstreamOperationState,
};

const MAX_CHECKSUM_LEN: usize = 256;
const MAX_ROOM_MEMBER_COUNT: u32 = 1_000_000;

pub(super) async fn handle_switch(
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

pub(super) async fn handle_room_route_upsert(
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

pub(super) async fn handle_character_route_upsert(
    route_store: &ProxyRouteStore,
    query: &HashMap<String, String>,
    audit_logger: &AdminAuditLogger,
    context: &AdminRequestContext,
) -> String {
    let action = "character_route_upsert";
    let character_id = match required_identifier(query, "character_id") {
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
                Some(character_id),
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
                Some(character_id),
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
                Some(character_id),
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
                Some(character_id),
                None,
            )
            .await;
        }
    };

    let result = route_store
        .upsert_character_route(CharacterRouteRecord {
            character_id: character_id.to_string(),
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
                Some(character_id),
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
                Some(character_id),
                Some(rollout_epoch.as_str()),
            )
            .await
        }
    }
}
