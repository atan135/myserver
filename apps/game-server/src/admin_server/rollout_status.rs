use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tracing::info;

use crate::core::context::{SharedRoomManager, SharedRuntimeConfig};
use crate::core::runtime::room_manager::ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT;
use crate::pb::{GetRolloutDrainStatusRes, RequestServerShutdownRes};

pub(super) async fn build_rollout_drain_status_response(
    room_manager: &SharedRoomManager,
    runtime_config: &SharedRuntimeConfig,
    owner_server_id: &str,
    connection_count: &Arc<AtomicU64>,
) -> GetRolloutDrainStatusRes {
    let snapshot = room_manager
        .rollout_drain_snapshot(owner_server_id, ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT)
        .await;
    let runtime = runtime_config.read().await.clone();
    let connection_count = connection_count.load(Ordering::Relaxed);

    if connection_count == 0 && snapshot.owned_room_count == 0 && snapshot.migrating_room_count == 0
    {
        info!(
            channel = "admin_tcp",
            drain_mode_enabled = runtime.drain_mode_enabled,
            drain_mode_reason = %runtime.drain_mode_reason,
            drain_mode_source = %runtime.drain_mode_source,
            connection_count = connection_count,
            owned_room_count = snapshot.owned_room_count,
            migrating_room_count = snapshot.migrating_room_count,
            transferable_empty_room_count = snapshot.transferable_empty_room_count,
            retired_room_count = snapshot.retired_room_count,
            rollout_epoch = %snapshot.rollout_epoch,
            owner_server_id = %snapshot.owner_server_id,
            "game-server rollout drain completed"
        );
    }

    GetRolloutDrainStatusRes {
        ok: true,
        error_code: String::new(),
        rollout_epoch: snapshot.rollout_epoch,
        owner_server_id: snapshot.owner_server_id,
        owned_room_count: snapshot.owned_room_count,
        migrating_room_count: snapshot.migrating_room_count,
        connection_count,
        routes: snapshot.routes,
        drain_mode_enabled: runtime.drain_mode_enabled,
        drain_mode_entered_at_ms: runtime.drain_mode_entered_at_ms.unwrap_or(0),
        transferable_empty_room_count: snapshot.transferable_empty_room_count,
        transferable_empty_room_samples: snapshot.transferable_empty_room_samples,
        drain_mode_reason: runtime.drain_mode_reason,
        drain_mode_source: runtime.drain_mode_source,
        retired_room_count: snapshot.retired_room_count,
    }
}

pub(super) async fn build_server_shutdown_response(
    room_manager: &SharedRoomManager,
    runtime_config: &SharedRuntimeConfig,
    owner_server_id: &str,
    connection_count: &Arc<AtomicU64>,
) -> RequestServerShutdownRes {
    let snapshot = room_manager
        .rollout_drain_snapshot(owner_server_id, ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT)
        .await;
    let runtime = runtime_config.read().await.clone();
    let connection_count = connection_count.load(Ordering::Relaxed);

    let error_code = if !runtime.drain_mode_enabled {
        "SHUTDOWN_DRAIN_MODE_REQUIRED"
    } else if connection_count != 0 {
        "SHUTDOWN_CONNECTIONS_REMAIN"
    } else if snapshot.owned_room_count != 0 {
        "SHUTDOWN_OWNED_ROOMS_REMAIN"
    } else if snapshot.migrating_room_count != 0 {
        "SHUTDOWN_MIGRATING_ROOMS_REMAIN"
    } else {
        ""
    };

    RequestServerShutdownRes {
        ok: error_code.is_empty(),
        error_code: error_code.to_string(),
        connection_count,
        owned_room_count: snapshot.owned_room_count,
        migrating_room_count: snapshot.migrating_room_count,
        drain_mode_enabled: runtime.drain_mode_enabled,
        retired_room_count: snapshot.retired_room_count,
    }
}
