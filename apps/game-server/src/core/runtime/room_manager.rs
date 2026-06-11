use std::collections::HashMap;
use std::time::{Duration, Instant};

use prost::Message;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant as TokioInstant, sleep_until};
use tracing::{debug, info, warn};

use crate::core::logic::{
    ROOM_TRANSFER_SCHEMA_VERSION, RoomLogicBroadcast, RoomLogicTransferState,
    SharedRoomLogicFactory, UNSUPPORTED_ROOM_TRANSFER,
};
use crate::core::room::{
    ConnectionCloseState, MemberRole, OutboundChannel, OutboundMessage, OutboundQueueLogContext,
    PendingInputUpsert, PlayerInputRecord, Room, RoomMemberState, RoomPhase, RoomTransferStatus,
    try_send_outbound,
};
use crate::core::runtime::room_policy::{
    InputWaitStrategy, MissingInputStrategy, RoomRuntimePolicy, SharedRoomPolicyRegistry,
};
use crate::match_client::SharedMatchClient;
use crate::metrics::METRICS;
use crate::pb::{
    FrameBundlePush, FrameInput, MovementCorrectionReason,
    MovementRecoveryState as PbMovementRecoveryState, RoomFrameRatePush, RoomMigrationState,
    RoomRouteStatus, RoomSnapshot, RoomStatePush, RoomTransferPayload, ServerRedirectPush,
};
use crate::protocol::{MessageType, encode_body};

const MAX_MISSING_INPUT_STREAK_BEFORE_OFFLINE: u32 = 3;
const DEFAULT_ROOM_CLEANUP_INTERVAL_SECS: u64 = 10;
pub const SERVER_REDIRECT_CLOSE_REASON: &str = "server_redirect_reconnect_required";
pub const ROLLOUT_DRAIN_STATUS_ROUTE_SAMPLE_LIMIT: usize = 50;

fn transfer_status_label(status: RoomTransferStatus) -> &'static str {
    match status {
        RoomTransferStatus::Owned => "Owned",
        RoomTransferStatus::Frozen => "Frozen",
        RoomTransferStatus::Exported => "Exported",
        RoomTransferStatus::Importing => "Importing",
        RoomTransferStatus::OwnedByNew => "OwnedByNew",
        RoomTransferStatus::Retired => "Retired",
    }
}

fn detach_member_outbound(member: &mut RoomMemberState) {
    let (placeholder_sender, _placeholder_receiver) = mpsc::channel(1);
    member.sender = placeholder_sender;
    member.close_state = ConnectionCloseState::new();
}

#[derive(Debug)]
pub struct RoomRuntime {
    pub current_fps: u16,
    pub tick_running: bool,
    pub tick_handle: Option<JoinHandle<()>>,
}

impl Default for RoomRuntime {
    fn default() -> Self {
        Self {
            current_fps: 1,
            tick_running: false,
            tick_handle: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RoomLeaveResult {
    pub snapshot: Option<RoomSnapshot>,
    pub room_removed: bool,
}

#[derive(Debug, Clone)]
pub struct RoomRecoveryState {
    pub snapshot: RoomSnapshot,
    pub current_frame_id: u32,
    pub recent_inputs: Vec<FrameInput>,
    pub waiting_frame_id: u32,
    pub waiting_inputs: Vec<FrameInput>,
    pub input_delay_frames: u32,
    pub movement_recovery: Option<PbMovementRecoveryState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRedirectDelivery {
    pub delivered_count: u64,
    pub failed_count: u64,
    pub online_member_count: u64,
}

#[derive(Clone)]
pub struct RolloutDrainSnapshot {
    pub rollout_epoch: String,
    pub owner_server_id: String,
    pub owned_room_count: u64,
    pub migrating_room_count: u64,
    pub routes: Vec<RoomRouteStatus>,
    pub transferable_empty_room_count: u64,
    pub transferable_empty_room_samples: Vec<RoomRouteStatus>,
    pub retired_room_count: u64,
}

#[derive(Clone)]
pub struct RoomManager {
    rooms: std::sync::Arc<Mutex<HashMap<String, Room>>>,
    runtimes: std::sync::Arc<Mutex<HashMap<String, RoomRuntime>>>,
    policies: SharedRoomPolicyRegistry,
    logic_factory: SharedRoomLogicFactory,
    match_client: SharedMatchClient,
}

fn room_rollout_route_status(room: &Room, owner_server_id: &str) -> RoomRouteStatus {
    RoomRouteStatus {
        room_id: room.room_id.clone(),
        owner_server_id: owner_server_id.to_string(),
        migration_state: room.transfer_state.status.migration_state() as i32,
        member_count: room.members.len() as u32,
        online_member_count: room
            .members
            .values()
            .filter(|member| !member.offline)
            .count() as u32,
        empty_since_ms: room
            .empty_since
            .map(|empty_since| empty_since.elapsed().as_millis() as u64)
            .unwrap_or_default(),
        room_version: room.transfer_state.room_version,
    }
}

fn log_room_entered_transferable_empty_candidate(
    room: &Room,
    trigger_player_id: &str,
    trigger_action: &'static str,
) {
    let online_member_count = room
        .members
        .values()
        .filter(|member| !member.offline)
        .count();

    info!(
        room_id = %room.room_id,
        rollout_epoch = %room.transfer_state.rollout_epoch.as_deref().unwrap_or_default(),
        migration_state = ?room.transfer_state.status.migration_state(),
        current_status = transfer_status_label(room.transfer_state.status),
        member_count = room.members.len(),
        online_member_count = online_member_count,
        empty_since_ms = room
            .empty_since
            .map(|empty_since| empty_since.elapsed().as_millis() as u64)
            .unwrap_or_default(),
        room_version = room.transfer_state.room_version,
        trigger_player_id = %trigger_player_id,
        trigger_action = trigger_action,
        "room entered empty transferable candidate state"
    );
}

impl RoomManager {
    pub fn new(logic_factory: SharedRoomLogicFactory) -> Self {
        Self::with_match_client_and_cleanup_interval(
            crate::match_client::create_match_client_shared(),
            logic_factory,
            DEFAULT_ROOM_CLEANUP_INTERVAL_SECS,
        )
    }

    pub fn with_match_client(
        match_client: SharedMatchClient,
        logic_factory: SharedRoomLogicFactory,
    ) -> Self {
        Self::with_match_client_and_cleanup_interval(
            match_client,
            logic_factory,
            DEFAULT_ROOM_CLEANUP_INTERVAL_SECS,
        )
    }

    pub fn with_match_client_and_cleanup_interval(
        match_client: SharedMatchClient,
        logic_factory: SharedRoomLogicFactory,
        cleanup_interval_secs: u64,
    ) -> Self {
        Self::with_policy_registry_and_cleanup_interval(
            match_client,
            logic_factory,
            SharedRoomPolicyRegistry::default(),
            cleanup_interval_secs,
        )
    }

    pub fn with_policy_registry_and_cleanup_interval(
        match_client: SharedMatchClient,
        logic_factory: SharedRoomLogicFactory,
        policies: SharedRoomPolicyRegistry,
        cleanup_interval_secs: u64,
    ) -> Self {
        let this = Self {
            rooms: std::sync::Arc::new(Mutex::new(HashMap::new())),
            runtimes: std::sync::Arc::new(Mutex::new(HashMap::new())),
            policies,
            logic_factory,
            match_client,
        };
        this.spawn_cleanup_task(cleanup_interval_secs);
        this
    }

    fn spawn_cleanup_task(&self, cleanup_interval_secs: u64) {
        let rooms = std::sync::Arc::clone(&self.rooms);
        let runtimes = std::sync::Arc::clone(&self.runtimes);
        let policies = self.policies.clone();
        let match_client = std::sync::Arc::clone(&self.match_client);
        let cleanup_interval_secs = cleanup_interval_secs.max(1);

        tokio::spawn(async move {
            info!(
                cleanup_interval_secs = cleanup_interval_secs,
                "room cleanup task started"
            );

            let mut interval = tokio::time::interval(Duration::from_secs(cleanup_interval_secs));
            loop {
                interval.tick().await;

                let mut to_destroy = Vec::new();
                let mut matches_to_abort = Vec::new();

                {
                    let mut rooms_guard = rooms.lock().await;

                    for (room_id, room) in rooms_guard.iter_mut() {
                        if room.marked_for_destruction {
                            continue;
                        }
                        if room.transfer_state.status.rejects_room_mutation() {
                            continue;
                        }

                        let policy = policies.resolve(&room.policy_id);
                        let expired_players =
                            room.collect_expired_offline_players(policy.offline_ttl_secs);
                        if !expired_players.is_empty() {
                            info!(
                                room_id = room_id,
                                expired_players = ?expired_players,
                                ttl_secs = policy.offline_ttl_secs,
                                "removing expired offline players from cleanup task"
                            );

                            for player_id in &expired_players {
                                room.logic.on_player_leave(player_id);
                            }

                            room.remove_members(&expired_players);

                            if !room.has_online_members() {
                                room.mark_empty();
                            } else {
                                room.clear_empty();
                            }
                        }

                        let should_cleanup_as_empty = match room.phase {
                            RoomPhase::InGame => room.members.is_empty(),
                            RoomPhase::Waiting => !room.has_online_members(),
                        };
                        if !policy.destroy_enabled
                            || !policy.destroy_when_empty
                            || !should_cleanup_as_empty
                        {
                            continue;
                        }

                        if !policy.retain_state_when_empty {
                            info!(
                                room_id = room_id,
                                policy_id = %policy.policy_id,
                                "room marked for destruction (no retain)"
                            );
                            room.mark_for_destruction();
                            to_destroy.push(room_id.clone());
                            continue;
                        }

                        if let Some(empty_since) = room.empty_since {
                            let elapsed = empty_since.elapsed().as_secs();
                            if elapsed >= policy.empty_ttl_secs {
                                info!(
                                    room_id = room_id,
                                    policy_id = %policy.policy_id,
                                    elapsed_secs = elapsed,
                                    "room TTL expired, marked for destruction"
                                );
                                room.mark_for_destruction();
                                to_destroy.push(room_id.clone());
                            }
                        }

                        if room.marked_for_destruction {
                            if let Some(match_id) = room.match_id.clone() {
                                matches_to_abort.push((match_id, room_id.clone()));
                            }
                        }
                    }
                }

                for room_id in to_destroy {
                    if let Some(runtime) = runtimes.lock().await.get(&room_id) {
                        if let Some(handle) = &runtime.tick_handle {
                            handle.abort();
                        }
                    }
                    rooms.lock().await.remove(&room_id);
                    let room_count = rooms.lock().await.len() as u64;
                    METRICS.set_room_count(room_count);
                    info!(room_id = room_id, "room destroyed by cleanup task");
                }

                for (match_id, room_id) in matches_to_abort {
                    let mut guard = match_client.lock().await;
                    if let Some(ref mut client) = *guard {
                        if let Err(error) = client
                            .match_end(&match_id, &room_id, "offline_ttl_expired")
                            .await
                        {
                            tracing::error!(
                                match_id = %match_id,
                                room_id = %room_id,
                                error = %error,
                                "failed to notify MatchService after offline TTL expiration"
                            );
                        }
                    }
                }
            }
        });
    }

    pub async fn room_count(&self) -> usize {
        self.rooms.lock().await.len()
    }

    pub async fn rollout_drain_snapshot(
        &self,
        owner_server_id: &str,
        route_limit: usize,
    ) -> RolloutDrainSnapshot {
        let rooms = self.rooms.lock().await;
        let mut room_ids = rooms.keys().cloned().collect::<Vec<_>>();
        room_ids.sort();

        let mut owned_room_count = 0_u64;
        let mut migrating_room_count = 0_u64;
        let mut routes = Vec::with_capacity(route_limit.min(room_ids.len()));
        let mut transferable_empty_room_count = 0_u64;
        let mut transferable_empty_room_samples =
            Vec::with_capacity(route_limit.min(room_ids.len()));
        let mut retired_room_count = 0_u64;
        let mut rollout_epoch: Option<&str> = None;
        let mut mixed_rollout_epoch = false;

        for room_id in room_ids {
            let Some(room) = rooms.get(&room_id) else {
                continue;
            };

            match room.transfer_state.status {
                RoomTransferStatus::Owned => {
                    owned_room_count = owned_room_count.saturating_add(1);
                    if !room.has_online_members() {
                        transferable_empty_room_count =
                            transferable_empty_room_count.saturating_add(1);
                        if transferable_empty_room_samples.len() < route_limit {
                            transferable_empty_room_samples
                                .push(room_rollout_route_status(room, owner_server_id));
                        }
                    }
                }
                RoomTransferStatus::Frozen
                | RoomTransferStatus::Exported
                | RoomTransferStatus::Importing => {
                    migrating_room_count = migrating_room_count.saturating_add(1);
                }
                RoomTransferStatus::Retired => {
                    retired_room_count = retired_room_count.saturating_add(1);
                }
                RoomTransferStatus::OwnedByNew => {}
            }

            if let Some(epoch) = room.transfer_state.rollout_epoch.as_deref() {
                if !epoch.is_empty() {
                    match rollout_epoch {
                        None => rollout_epoch = Some(epoch),
                        Some(existing) if existing == epoch => {}
                        Some(_) => mixed_rollout_epoch = true,
                    }
                }
            }

            if routes.len() < route_limit {
                routes.push(room_rollout_route_status(room, owner_server_id));
            }
        }

        RolloutDrainSnapshot {
            rollout_epoch: if mixed_rollout_epoch {
                String::new()
            } else {
                rollout_epoch.unwrap_or_default().to_string()
            },
            owner_server_id: owner_server_id.to_string(),
            owned_room_count,
            migrating_room_count,
            routes,
            transferable_empty_room_count,
            transferable_empty_room_samples,
            retired_room_count,
        }
    }

    pub async fn room_exists(&self, room_id: &str) -> bool {
        self.rooms.lock().await.contains_key(room_id)
    }

    pub async fn find_room_by_offline_player(&self, player_id: &str) -> Option<String> {
        let rooms = self.rooms.lock().await;
        rooms.iter().find_map(|(room_id, room)| {
            room.members
                .get(player_id)
                .filter(|member| member.offline)
                .map(|_| room_id.clone())
        })
    }

    pub async fn create_matched_room(
        &self,
        match_id: &str,
        room_id: &str,
        player_ids: &[String],
        mode: &str,
    ) -> Result<RoomSnapshot, &'static str> {
        let mut rooms = self.rooms.lock().await;
        let mut runtimes = self.runtimes.lock().await;
        let default_policy = self.policies.default_policy();
        {
            let room = rooms.entry(room_id.to_string()).or_insert_with(|| {
                let mut logic = self.logic_factory.create(&default_policy.policy_id);
                logic.on_room_created(room_id);
                info!(
                    room_id = room_id,
                    match_id = match_id,
                    mode = mode,
                    "matched room created"
                );
                let mut room = Room::new(
                    room_id.to_string(),
                    player_ids.first().cloned().unwrap_or_default(),
                    default_policy.policy_id.clone(),
                    logic,
                );
                room.set_match_id(match_id.to_string());
                room
            });

            if room.transfer_state.status.rejects_room_mutation() {
                return Err(room.transfer_state.mutation_error_code());
            }
            runtimes
                .entry(room_id.to_string())
                .or_insert_with(RoomRuntime::default);
        }
        let room_count = rooms.len() as u64;
        METRICS.set_room_count(room_count);

        let room = rooms.get(room_id).ok_or("ROOM_NOT_FOUND")?;
        let snapshot = room.snapshot();
        drop(rooms);
        drop(runtimes);

        self.notify_room_created(match_id, room_id, player_ids, mode)
            .await;

        Ok(snapshot)
    }

    async fn notify_room_created(
        &self,
        match_id: &str,
        room_id: &str,
        player_ids: &[String],
        mode: &str,
    ) {
        let mut guard = self.match_client.lock().await;
        if let Some(ref mut client) = *guard {
            match client
                .create_room_and_join(match_id, room_id, player_ids, mode)
                .await
            {
                Ok(()) => {
                    info!(
                        match_id = match_id,
                        room_id = room_id,
                        "Notified MatchService: room created"
                    );
                }
                Err(e) => {
                    tracing::error!(match_id = match_id, error = %e, "Failed to notify MatchService: room created");
                }
            }
        }
    }

    async fn notify_player_joined(&self, match_id: &str, player_id: &str, room_id: &str) {
        let mut guard = self.match_client.lock().await;
        if let Some(ref mut client) = *guard {
            match client.player_joined(match_id, player_id, room_id).await {
                Ok(()) => {
                    info!(
                        match_id = match_id,
                        player_id = player_id,
                        room_id = room_id,
                        "Notified MatchService: player joined"
                    );
                }
                Err(e) => {
                    tracing::error!(match_id = match_id, player_id = player_id, error = %e, "Failed to notify MatchService: player joined");
                }
            }
        }
    }

    async fn notify_player_left(&self, match_id: &str, player_id: &str, reason: &str) -> bool {
        let mut guard = self.match_client.lock().await;
        if let Some(ref mut client) = *guard {
            match client.player_left(match_id, player_id, reason).await {
                Ok(should_abort) => {
                    info!(
                        match_id = match_id,
                        player_id = player_id,
                        reason = reason,
                        should_abort = should_abort,
                        "Notified MatchService: player left"
                    );
                    return should_abort;
                }
                Err(e) => {
                    tracing::error!(match_id = match_id, player_id = player_id, error = %e, "Failed to notify MatchService: player left");
                }
            }
        }
        false
    }

    async fn notify_match_end(&self, match_id: &str, room_id: &str, reason: &str) {
        let mut guard = self.match_client.lock().await;
        if let Some(ref mut client) = *guard {
            match client.match_end(match_id, room_id, reason).await {
                Ok(()) => {
                    info!(
                        match_id = match_id,
                        room_id = room_id,
                        reason = reason,
                        "Notified MatchService: match ended"
                    );
                }
                Err(e) => {
                    tracing::error!(match_id = match_id, room_id = room_id, error = %e, "Failed to notify MatchService: match ended");
                }
            }
        }
    }

    pub async fn join_room(
        &self,
        room_id: &str,
        player_id: &str,
        outbound: impl Into<OutboundChannel>,
        role: MemberRole,
        requested_policy_id: Option<&str>,
    ) -> Result<RoomSnapshot, &'static str> {
        let outbound = outbound.into();
        let mut rooms = self.rooms.lock().await;
        let mut runtimes = self.runtimes.lock().await;
        let requested_policy_id = requested_policy_id
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.policies.default_policy().policy_id);
        let selected_policy = self.policies.resolve(&requested_policy_id);
        let (snapshot, match_id) = {
            let room = rooms.entry(room_id.to_string()).or_insert_with(|| {
                let mut logic = self.logic_factory.create(&selected_policy.policy_id);
                logic.on_room_created(room_id);
                info!(
                    room_id = room_id,
                    owner_player_id = player_id,
                    policy_id = %selected_policy.policy_id,
                    "room created"
                );
                Room::new(
                    room_id.to_string(),
                    player_id.to_string(),
                    selected_policy.policy_id.clone(),
                    logic,
                )
            });

            if room.transfer_state.status.rejects_room_mutation() {
                return Err(room.transfer_state.mutation_error_code());
            }

            let policy = self.policies.resolve(&room.policy_id);
            if room.phase == RoomPhase::InGame
                && !policy.allow_join_in_game
                && !room.members.contains_key(player_id)
            {
                return Err("ROOM_ALREADY_IN_GAME");
            }

            if room.members.len() >= policy.max_members && !room.members.contains_key(player_id) {
                return Err("ROOM_FULL");
            }

            let is_new_member = !room.members.contains_key(player_id);
            let sync_before_broadcast =
                is_new_member && room.phase == RoomPhase::InGame && policy.allow_join_in_game;
            room.members.insert(
                player_id.to_string(),
                RoomMemberState {
                    player_id: player_id.to_string(),
                    ready: false,
                    sender: outbound.sender,
                    close_state: outbound.close_state,
                    offline: false,
                    offline_since: None,
                    role,
                    syncing: sync_before_broadcast,
                },
            );

            if is_new_member {
                room.update_activity();
                room.clear_empty();
                room.logic.on_player_join(player_id);
            }

            runtimes
                .entry(room_id.to_string())
                .or_insert_with(RoomRuntime::default);

            (room.snapshot(), room.match_id.clone())
        };
        let room_count = rooms.len() as u64;
        METRICS.set_room_count(room_count);
        drop(rooms);
        drop(runtimes);

        if let Some(ref mid) = match_id {
            self.notify_player_joined(mid, player_id, room_id).await;
        }
        self.update_room_fps(room_id).await;

        Ok(snapshot)
    }

    pub async fn finish_member_sync(&self, room_id: &str, player_id: &str) {
        let sync_completed = {
            let mut rooms = self.rooms.lock().await;
            let Some(room) = rooms.get_mut(room_id) else {
                return;
            };
            room.finish_member_sync(player_id)
        };

        if sync_completed {
            info!(
                room_id = room_id,
                player_id = player_id,
                "room member sync completed"
            );
            self.update_room_fps(room_id).await;
        }
    }

    pub async fn is_member_syncing(&self, room_id: &str, player_id: &str) -> bool {
        let rooms = self.rooms.lock().await;
        rooms
            .get(room_id)
            .and_then(|room| room.members.get(player_id))
            .map(|member| member.syncing)
            .unwrap_or(false)
    }

    pub async fn leave_room(&self, room_id: &str, player_id: &str) -> RoomLeaveResult {
        info!(
            room_id = room_id,
            player_id = player_id,
            "leave_room called"
        );

        let mut rooms = self.rooms.lock().await;
        let runtimes = self.runtimes.lock().await;
        let Some(room) = rooms.get_mut(room_id) else {
            info!(room_id = room_id, "leave_room: room not found");
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        };
        let previous_online_member_count = room
            .members
            .values()
            .filter(|member| !member.offline)
            .count();

        if let Some(member) = room.members.get_mut(player_id) {
            member.offline = true;
            member.offline_since = Some(Instant::now());
            detach_member_outbound(member);
            room.logic.on_player_offline(room_id, player_id);
            info!(
                room_id = room_id,
                player_id = player_id,
                "player marked offline, members count: {}",
                room.members.len()
            );
        } else {
            info!(
                room_id = room_id,
                player_id = player_id,
                "leave_room: player not found in room members, current members: {:?}",
                room.members.keys().collect::<Vec<_>>()
            );
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        }

        let policy = self.policies.resolve(&room.policy_id);

        if room.owner_player_id == player_id {
            if let Some(next_owner) = room
                .members
                .values()
                .find(|m| !m.offline)
                .map(|m| m.player_id.clone())
            {
                room.owner_player_id = next_owner;
            }
        }

        if !room.has_online_members() {
            room.mark_empty();
            if previous_online_member_count > 0 {
                log_room_entered_transferable_empty_candidate(room, player_id, "leave_room");
            }
        }

        let _ = policy;
        room.reset_to_waiting();

        let pending_broadcasts = room.logic.take_pending_broadcasts();
        let snapshot = room.snapshot();
        let match_id = room.match_id.clone();
        drop(rooms);
        drop(runtimes);

        self.broadcast_logic_broadcasts(room_id, pending_broadcasts)
            .await;
        self.update_room_fps(room_id).await;

        if let Some(ref mid) = match_id {
            let should_abort = self.notify_player_left(mid, player_id, "normal").await;
            if should_abort {
                info!(
                    room_id = room_id,
                    match_id = mid,
                    "MatchService requested abort due to player leaving"
                );
                self.notify_match_end(mid, room_id, "aborted").await;
            }
        }

        RoomLeaveResult {
            snapshot: Some(snapshot),
            room_removed: false,
        }
    }

    pub async fn disconnect_room_member(&self, room_id: &str, player_id: &str) -> RoomLeaveResult {
        info!(
            room_id = room_id,
            player_id = player_id,
            "disconnect_room_member called"
        );

        let mut rooms = self.rooms.lock().await;
        let Some(room) = rooms.get_mut(room_id) else {
            info!(room_id = room_id, "disconnect_room_member: room not found");
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        };
        let previous_online_member_count = room
            .members
            .values()
            .filter(|member| !member.offline)
            .count();

        if let Some(member) = room.members.get_mut(player_id) {
            member.offline = true;
            member.offline_since = Some(Instant::now());
            detach_member_outbound(member);
            room.logic.on_player_offline(room_id, player_id);
            info!(
                room_id = room_id,
                player_id = player_id,
                phase = ?room.phase,
                "player marked offline without resetting runtime state"
            );
        } else {
            info!(
                room_id = room_id,
                player_id = player_id,
                "disconnect_room_member: player not found in room members, current members: {:?}",
                room.members.keys().collect::<Vec<_>>()
            );
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        }

        if room.owner_player_id == player_id {
            if let Some(next_owner) = room
                .members
                .values()
                .find(|m| !m.offline)
                .map(|m| m.player_id.clone())
            {
                room.owner_player_id = next_owner;
            }
        }

        if !room.has_online_members() {
            room.mark_empty();
            room.wait_started_at = None;
            if previous_online_member_count > 0 {
                log_room_entered_transferable_empty_candidate(
                    room,
                    player_id,
                    "disconnect_room_member",
                );
            }
        }

        let pending_broadcasts = room.logic.take_pending_broadcasts();
        let snapshot = room.snapshot();
        let match_id = room.match_id.clone();
        drop(rooms);

        self.broadcast_logic_broadcasts(room_id, pending_broadcasts)
            .await;
        self.update_room_fps(room_id).await;

        if let Some(ref mid) = match_id {
            let should_abort = self.notify_player_left(mid, player_id, "disconnect").await;
            if should_abort {
                info!(
                    room_id = room_id,
                    match_id = mid,
                    "MatchService requested abort due to player disconnect"
                );
                self.notify_match_end(mid, room_id, "aborted").await;
            }
        }

        RoomLeaveResult {
            snapshot: Some(snapshot),
            room_removed: false,
        }
    }

    pub async fn reconnect_room(
        &self,
        room_id: &str,
        player_id: &str,
        outbound: impl Into<OutboundChannel>,
    ) -> Result<RoomRecoveryState, &'static str> {
        let outbound = outbound.into();
        let mut rooms = self.rooms.lock().await;
        let room = rooms.get_mut(room_id).ok_or("ROOM_NOT_FOUND")?;

        if room.transfer_state.status.rejects_room_mutation() {
            return Err(room.transfer_state.mutation_error_code());
        }

        if let Some(member) = room.members.get_mut(player_id) {
            if !member.offline {
                return Err("PLAYER_ALREADY_ONLINE");
            }

            member.offline = false;
            member.offline_since = None;
            member.sender = outbound.sender;
            member.close_state = outbound.close_state;
            member.syncing = false;
            room.logic.on_player_online(room_id, player_id);
            room.clear_empty();
            room.update_activity();

            info!(
                room_id = room_id,
                player_id = player_id,
                "player reconnected"
            );

            let snapshot = room.snapshot();
            let current_frame_id = room.current_frame;
            let recent_inputs = room_frame_inputs_from_history(room, current_frame_id);
            let waiting_frame_id = room.current_waiting_frame_id();
            let waiting_inputs = room_frame_inputs_from_pending(room, waiting_frame_id);
            let input_delay_frames = self.policies.resolve(&room.policy_id).input_delay_frames;
            let movement_recovery = room.logic.movement_recovery_state(
                Some(player_id),
                MovementCorrectionReason::ReconnectRecovery,
            );
            let match_id = room.match_id.clone();
            drop(rooms);

            if let Some(ref mid) = match_id {
                self.notify_player_joined(mid, player_id, room_id).await;
            }
            self.update_room_fps(room_id).await;

            Ok(RoomRecoveryState {
                snapshot,
                current_frame_id,
                recent_inputs,
                waiting_frame_id,
                waiting_inputs,
                input_delay_frames,
                movement_recovery,
            })
        } else {
            Err("PLAYER_NOT_IN_ROOM")
        }
    }

    pub async fn join_room_as_observer(
        &self,
        room_id: &str,
        player_id: &str,
        outbound: impl Into<OutboundChannel>,
    ) -> Result<RoomRecoveryState, &'static str> {
        let outbound = outbound.into();
        let mut rooms = self.rooms.lock().await;
        let mut runtimes = self.runtimes.lock().await;
        let default_policy = self.policies.default_policy().clone();
        let recovery = {
            let room = rooms.entry(room_id.to_string()).or_insert_with(|| {
                let mut logic = self.logic_factory.create(&default_policy.policy_id);
                logic.on_room_created(room_id);
                info!(
                    room_id = room_id,
                    owner_player_id = player_id,
                    policy_id = %default_policy.policy_id,
                    "room created for observer"
                );
                Room::new(
                    room_id.to_string(),
                    player_id.to_string(),
                    default_policy.policy_id.clone(),
                    logic,
                )
            });

            if room.transfer_state.status.rejects_room_mutation() {
                return Err(room.transfer_state.mutation_error_code());
            }

            let policy = self.policies.resolve(&room.policy_id);
            if room.members.len() >= policy.max_members && !room.members.contains_key(player_id) {
                return Err("ROOM_FULL");
            }

            let is_new_member = !room.members.contains_key(player_id);
            room.members.insert(
                player_id.to_string(),
                RoomMemberState {
                    player_id: player_id.to_string(),
                    ready: false,
                    sender: outbound.sender,
                    close_state: outbound.close_state,
                    offline: false,
                    offline_since: None,
                    role: MemberRole::Observer,
                    syncing: false,
                },
            );

            if is_new_member {
                room.update_activity();
                room.clear_empty();
                room.logic.on_player_join(player_id);
            }

            runtimes
                .entry(room_id.to_string())
                .or_insert_with(RoomRuntime::default);

            let snapshot = room.snapshot();
            let current_frame_id = room.current_frame;
            let recent_inputs = room_frame_inputs_from_history(room, current_frame_id);
            let waiting_frame_id = room.current_waiting_frame_id();
            let waiting_inputs = room_frame_inputs_from_pending(room, waiting_frame_id);
            let input_delay_frames = self.policies.resolve(&room.policy_id).input_delay_frames;
            let movement_recovery = room
                .logic
                .movement_recovery_state(None, MovementCorrectionReason::ObserverRecovery);

            RoomRecoveryState {
                snapshot,
                current_frame_id,
                recent_inputs,
                waiting_frame_id,
                waiting_inputs,
                input_delay_frames,
                movement_recovery,
            }
        };
        let room_count = rooms.len() as u64;
        METRICS.set_room_count(room_count);
        drop(rooms);
        drop(runtimes);

        info!(
            room_id = room_id,
            player_id = player_id,
            current_frame_id = recovery.current_frame_id,
            "observer joined"
        );

        self.update_room_fps(room_id).await;

        Ok(recovery)
    }

    pub async fn cleanup_expired_offline_players(&self) {
        let mut rooms = self.rooms.lock().await;

        for (room_id, room) in rooms.iter_mut() {
            if room.transfer_state.status.rejects_room_mutation() {
                continue;
            }

            let policy = self.policies.resolve(&room.policy_id);
            let expired = room.collect_expired_offline_players(policy.offline_ttl_secs);

            if !expired.is_empty() {
                info!(
                    room_id = room_id,
                    expired_players = ?expired,
                    ttl_secs = policy.offline_ttl_secs,
                    "removing expired offline players"
                );

                for player_id in &expired {
                    room.logic.on_player_leave(player_id);
                }

                room.remove_members(&expired);

                if !room.has_online_members() {
                    room.mark_empty();
                } else {
                    room.clear_empty();
                }
            }
        }
    }

    pub async fn set_ready_state(
        &self,
        room_id: &str,
        player_id: &str,
        ready: bool,
    ) -> Result<RoomSnapshot, &'static str> {
        let mut rooms = self.rooms.lock().await;
        let room = rooms.get_mut(room_id).ok_or("ROOM_NOT_FOUND")?;

        if room.transfer_state.status.rejects_room_mutation() {
            return Err(room.transfer_state.mutation_error_code());
        }
        if room.phase == RoomPhase::InGame {
            return Err("ROOM_ALREADY_IN_GAME");
        }

        let member = room
            .members
            .get_mut(player_id)
            .ok_or("ROOM_MEMBER_NOT_FOUND")?;
        member.ready = ready;
        Ok(room.snapshot())
    }

    pub async fn start_game(
        &self,
        room_id: &str,
        player_id: &str,
    ) -> Result<RoomSnapshot, &'static str> {
        let mut rooms = self.rooms.lock().await;
        let room = rooms.get_mut(room_id).ok_or("ROOM_NOT_FOUND")?;
        let policy = self.policies.resolve(&room.policy_id);

        if room.transfer_state.status.rejects_room_mutation() {
            return Err(room.transfer_state.mutation_error_code());
        }
        room.can_start_game(player_id, policy.min_start_players)?;
        room.phase = RoomPhase::InGame;
        room.clear_runtime_inputs();
        room.logic.on_game_started(room_id);
        info!(
            room_id = room_id,
            owner_player_id = player_id,
            member_count = room.members.len(),
            "room entered in_game phase"
        );
        drop(rooms);

        self.ensure_room_tick_running(room_id).await;
        self.update_room_fps(room_id).await;

        let rooms = self.rooms.lock().await;
        let room = rooms.get(room_id).ok_or("ROOM_NOT_FOUND")?;
        Ok(room.snapshot())
    }

    pub async fn accept_player_input(
        &self,
        room_id: &str,
        player_id: &str,
        frame_id: u32,
        action: &str,
        payload_json: &str,
    ) -> Result<(), &'static str> {
        let mut rooms = self.rooms.lock().await;
        let room = rooms.get_mut(room_id).ok_or("ROOM_NOT_FOUND")?;
        let policy = self.policies.resolve(&room.policy_id);

        if room.transfer_state.status.rejects_room_mutation() {
            return Err(room.transfer_state.mutation_error_code());
        }
        room.can_send_input(player_id)?;
        room.logic
            .validate_player_input(player_id, action, payload_json)?;
        if frame_id <= room.current_frame {
            return Err("INPUT_FRAME_EXPIRED");
        }

        let max_future_frame = room
            .current_frame
            .saturating_add(policy.input_delay_frames.max(1));
        if frame_id > max_future_frame {
            return Err("INPUT_FRAME_TOO_FAR");
        }

        let input_record = PlayerInputRecord {
            frame_id,
            player_id: player_id.to_string(),
            action: action.to_string(),
            payload_json: payload_json.to_string(),
            received_at: Instant::now(),
            is_synthetic: false,
        };
        let outcome = room.upsert_pending_input(input_record);
        room.update_activity();
        room.logic.on_player_input(player_id, action, payload_json);
        if matches!(outcome, PendingInputUpsert::Replaced) {
            info!(
                room_id = room_id,
                player_id = player_id,
                frame_id = frame_id,
                "pending input replaced for same frame"
            );
        }

        Ok(())
    }

    pub async fn end_game(
        &self,
        room_id: &str,
        player_id: &str,
    ) -> Result<RoomSnapshot, &'static str> {
        let mut rooms = self.rooms.lock().await;
        let room = rooms.get_mut(room_id).ok_or("ROOM_NOT_FOUND")?;

        if room.transfer_state.status.rejects_room_mutation() {
            return Err(room.transfer_state.mutation_error_code());
        }
        room.can_end_game(player_id)?;
        room.logic.on_game_ended(room_id);
        room.reset_to_waiting();
        info!(
            room_id = room_id,
            owner_player_id = player_id,
            member_count = room.members.len(),
            "room returned to waiting phase"
        );

        let match_id = room.match_id.clone();
        drop(rooms);

        self.update_room_fps(room_id).await;

        if let Some(ref mid) = match_id {
            self.notify_match_end(mid, room_id, "game_over").await;
        }

        let rooms = self.rooms.lock().await;
        let room = rooms.get(room_id).ok_or("ROOM_NOT_FOUND")?;
        Ok(room.snapshot())
    }

    pub async fn freeze_room_for_transfer(
        &self,
        rollout_epoch: &str,
        room_id: &str,
    ) -> Result<(RoomMigrationState, u64), &'static str> {
        let rollout_epoch = rollout_epoch.trim();
        if rollout_epoch.is_empty() {
            warn!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "INVALID_ROLLOUT_EPOCH",
                "room transfer freeze rejected"
            );
            return Err("INVALID_ROLLOUT_EPOCH");
        }

        let (state, version) = {
            let mut rooms = self.rooms.lock().await;
            let room = match rooms.get_mut(room_id) {
                Some(room) => room,
                None => {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_NOT_FOUND",
                        "room transfer freeze rejected"
                    );
                    return Err("ROOM_NOT_FOUND");
                }
            };

            match room.transfer_state.status {
                RoomTransferStatus::Retired => {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_TRANSFER_RETIRED",
                        current_status = transfer_status_label(room.transfer_state.status),
                        room_version = room.transfer_state.room_version,
                        "room transfer freeze rejected"
                    );
                    return Err("ROOM_TRANSFER_RETIRED");
                }
                RoomTransferStatus::Frozen | RoomTransferStatus::Exported
                    if room.transfer_state.rollout_epoch.as_deref() == Some(rollout_epoch) =>
                {
                    info!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "IDEMPOTENT_ROOM_TRANSFER_FREEZE",
                        current_status = transfer_status_label(room.transfer_state.status),
                        room_version = room.transfer_state.room_version,
                        "room transfer freeze idempotent replay"
                    );
                    return Ok((
                        room.transfer_state.status.migration_state(),
                        room.transfer_state.room_version,
                    ));
                }
                RoomTransferStatus::Frozen | RoomTransferStatus::Exported => {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_TRANSFER_EPOCH_MISMATCH",
                        current_status = transfer_status_label(room.transfer_state.status),
                        expected = ?room.transfer_state.rollout_epoch,
                        actual = rollout_epoch,
                        room_version = room.transfer_state.room_version,
                        "room transfer freeze rejected"
                    );
                    return Err("ROOM_TRANSFER_EPOCH_MISMATCH");
                }
                RoomTransferStatus::Importing => {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_TRANSFER_IMPORTING",
                        current_status = transfer_status_label(room.transfer_state.status),
                        room_version = room.transfer_state.room_version,
                        "room transfer freeze rejected"
                    );
                    return Err("ROOM_TRANSFER_IMPORTING");
                }
                RoomTransferStatus::OwnedByNew => {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_TRANSFER_OWNED_BY_NEW",
                        current_status = transfer_status_label(room.transfer_state.status),
                        room_version = room.transfer_state.room_version,
                        "room transfer freeze rejected"
                    );
                    return Err("ROOM_TRANSFER_OWNED_BY_NEW");
                }
                RoomTransferStatus::Owned => {}
            }

            if room.has_online_members() {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_TRANSFER_HAS_ONLINE_MEMBERS",
                    current_status = transfer_status_label(room.transfer_state.status),
                    room_version = room.transfer_state.room_version,
                    online_member_count = room
                        .members
                        .values()
                        .filter(|member| !member.offline)
                        .count(),
                    "room transfer freeze rejected because room has online members"
                );
                return Err("ROOM_TRANSFER_HAS_ONLINE_MEMBERS");
            }

            room.transfer_state.status = RoomTransferStatus::Frozen;
            room.transfer_state.rollout_epoch = Some(rollout_epoch.to_string());
            room.transfer_state.last_transfer_checksum = None;
            room.transfer_state.bump_version();
            room.wait_started_at = None;

            info!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "OK",
                room_version = room.transfer_state.room_version,
                "room frozen for transfer"
            );

            (
                room.transfer_state.status.migration_state(),
                room.transfer_state.room_version,
            )
        };

        self.stop_room_tick(room_id).await;
        Ok((state, version))
    }

    pub async fn export_room_transfer(
        &self,
        rollout_epoch: &str,
        room_id: &str,
    ) -> Result<RoomTransferPayload, &'static str> {
        let rollout_epoch = rollout_epoch.trim();
        if rollout_epoch.is_empty() {
            warn!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "INVALID_ROLLOUT_EPOCH",
                "room transfer export rejected"
            );
            return Err("INVALID_ROLLOUT_EPOCH");
        }

        let mut payload = {
            let mut rooms = self.rooms.lock().await;
            let room = match rooms.get_mut(room_id) {
                Some(room) => room,
                None => {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_NOT_FOUND",
                        "room transfer export rejected"
                    );
                    return Err("ROOM_NOT_FOUND");
                }
            };

            match room.transfer_state.status {
                RoomTransferStatus::Frozen | RoomTransferStatus::Exported => {}
                RoomTransferStatus::Retired => {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_TRANSFER_RETIRED",
                        current_status = transfer_status_label(room.transfer_state.status),
                        room_version = room.transfer_state.room_version,
                        "room transfer export rejected"
                    );
                    return Err("ROOM_TRANSFER_RETIRED");
                }
                RoomTransferStatus::Importing => {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_TRANSFER_IMPORTING",
                        current_status = transfer_status_label(room.transfer_state.status),
                        room_version = room.transfer_state.room_version,
                        "room transfer export rejected"
                    );
                    return Err("ROOM_TRANSFER_IMPORTING");
                }
                RoomTransferStatus::OwnedByNew => {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_TRANSFER_OWNED_BY_NEW",
                        current_status = transfer_status_label(room.transfer_state.status),
                        room_version = room.transfer_state.room_version,
                        "room transfer export rejected"
                    );
                    return Err("ROOM_TRANSFER_OWNED_BY_NEW");
                }
                _ => {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_TRANSFER_NOT_FROZEN",
                        current_status = transfer_status_label(room.transfer_state.status),
                        room_version = room.transfer_state.room_version,
                        "room transfer export rejected"
                    );
                    return Err("ROOM_TRANSFER_NOT_FROZEN");
                }
            }

            if room.transfer_state.rollout_epoch.as_deref() != Some(rollout_epoch) {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_TRANSFER_EPOCH_MISMATCH",
                    current_status = transfer_status_label(room.transfer_state.status),
                    expected = ?room.transfer_state.rollout_epoch,
                    actual = rollout_epoch,
                    room_version = room.transfer_state.room_version,
                    "room transfer export rejected"
                );
                return Err("ROOM_TRANSFER_EPOCH_MISMATCH");
            }

            let policy = self.policies.resolve(&room.policy_id);
            let current_frame_id = room.current_frame;
            let last_applied_frame_id = room
                .last_applied_inputs
                .values()
                .map(|input| input.frame_id)
                .max()
                .unwrap_or(current_frame_id);
            let transfer_state = room.logic.export_transfer_state()?;

            let room_version = if room.transfer_state.status == RoomTransferStatus::Exported {
                room.transfer_state.room_version
            } else {
                room.transfer_state.room_version.saturating_add(1)
            };

            RoomTransferPayload {
                rollout_epoch: rollout_epoch.to_string(),
                room_id: room.room_id.clone(),
                room_version,
                policy_id: room.policy_id.clone(),
                owner_player_id: room.owner_player_id.clone(),
                room_phase: room_phase_name(room.phase).to_string(),
                current_frame_id,
                last_applied_frame_id,
                snapshot: Some(room.snapshot()),
                recent_inputs: room_frame_inputs_from_history(room, current_frame_id),
                waiting_frame_id: room.current_waiting_frame_id(),
                waiting_inputs: room_frame_inputs_from_pending(
                    room,
                    room.current_waiting_frame_id(),
                ),
                movement_state_json: room_transfer_movement_state_json(&transfer_state),
                logic_state_json: room_transfer_logic_state_json(&transfer_state),
                runtime_timers_json: room_transfer_timer_state_json(
                    &transfer_state,
                    json!({
                        "hasEmptySince": room.empty_since.is_some(),
                        "hasWaitStarted": room.wait_started_at.is_some(),
                        "inputDelayFrames": policy.input_delay_frames,
                        "snapshotIntervalFrames": policy.snapshot_interval_frames
                    }),
                ),
                match_id: room.match_id.clone().unwrap_or_default(),
                checksum: String::new(),
            }
        };

        payload.checksum = room_transfer_checksum(&payload);

        let mut rooms = self.rooms.lock().await;
        let room = match rooms.get_mut(room_id) {
            Some(room) => room,
            None => {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_NOT_FOUND",
                    checksum = %payload.checksum,
                    room_version = payload.room_version,
                    "room transfer export rejected"
                );
                return Err("ROOM_NOT_FOUND");
            }
        };
        let was_exported = room.transfer_state.status == RoomTransferStatus::Exported;
        match room.transfer_state.status {
            RoomTransferStatus::Frozen => {
                room.transfer_state.status = RoomTransferStatus::Exported;
                room.transfer_state.room_version = payload.room_version;
                room.transfer_state.last_transfer_checksum = Some(payload.checksum.clone());
            }
            RoomTransferStatus::Exported => {
                if room.transfer_state.rollout_epoch.as_deref() != Some(rollout_epoch) {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_TRANSFER_EPOCH_MISMATCH",
                        current_status = transfer_status_label(room.transfer_state.status),
                        expected = ?room.transfer_state.rollout_epoch,
                        actual = rollout_epoch,
                        room_version = room.transfer_state.room_version,
                        "room transfer export rejected"
                    );
                    return Err("ROOM_TRANSFER_EPOCH_MISMATCH");
                }
                if room.transfer_state.last_transfer_checksum.as_deref()
                    != Some(payload.checksum.as_str())
                {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_TRANSFER_CHECKSUM_MISMATCH",
                        current_status = transfer_status_label(room.transfer_state.status),
                        expected = ?room.transfer_state.last_transfer_checksum,
                        actual = %payload.checksum,
                        room_version = room.transfer_state.room_version,
                        "room transfer export rejected"
                    );
                    return Err("ROOM_TRANSFER_CHECKSUM_MISMATCH");
                }
            }
            RoomTransferStatus::Retired => {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_TRANSFER_RETIRED",
                    current_status = transfer_status_label(room.transfer_state.status),
                    room_version = room.transfer_state.room_version,
                    "room transfer export rejected"
                );
                return Err("ROOM_TRANSFER_RETIRED");
            }
            RoomTransferStatus::Importing => {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_TRANSFER_IMPORTING",
                    current_status = transfer_status_label(room.transfer_state.status),
                    room_version = room.transfer_state.room_version,
                    "room transfer export rejected"
                );
                return Err("ROOM_TRANSFER_IMPORTING");
            }
            RoomTransferStatus::OwnedByNew => {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_TRANSFER_OWNED_BY_NEW",
                    current_status = transfer_status_label(room.transfer_state.status),
                    room_version = room.transfer_state.room_version,
                    "room transfer export rejected"
                );
                return Err("ROOM_TRANSFER_OWNED_BY_NEW");
            }
            RoomTransferStatus::Owned => {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_TRANSFER_NOT_FROZEN",
                    current_status = transfer_status_label(room.transfer_state.status),
                    room_version = room.transfer_state.room_version,
                    "room transfer export rejected"
                );
                return Err("ROOM_TRANSFER_NOT_FROZEN");
            }
        }

        if was_exported {
            info!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "IDEMPOTENT_ROOM_TRANSFER_EXPORT",
                checksum = %payload.checksum,
                room_version = payload.room_version,
                current_status = transfer_status_label(room.transfer_state.status),
                "room transfer export idempotent replay"
            );
        } else {
            info!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "OK",
                checksum = %payload.checksum,
                room_version = payload.room_version,
                "room transfer payload exported"
            );
        }

        Ok(payload)
    }

    pub async fn import_room_transfer(
        &self,
        payload: RoomTransferPayload,
    ) -> Result<(String, u64), &'static str> {
        let room_id = payload.room_id.clone();
        let checksum = payload.checksum.clone();
        let rollout_epoch = payload.rollout_epoch.clone();
        let source_room_version = payload.room_version;
        if let Err(error_code) = validate_room_transfer_payload(&payload) {
            warn!(
                room_id = %room_id,
                rollout_epoch = %rollout_epoch,
                error_code = error_code,
                checksum = %checksum,
                room_version = source_room_version,
                "room transfer import rejected during payload validation"
            );
            return Err(error_code);
        }
        let phase = match parse_room_phase(&payload.room_phase) {
            Ok(phase) => phase,
            Err(error_code) => {
                warn!(
                    room_id = %room_id,
                    rollout_epoch = %rollout_epoch,
                    error_code = error_code,
                    checksum = %checksum,
                    room_version = source_room_version,
                    actual = %payload.room_phase,
                    "room transfer import rejected due to invalid room phase"
                );
                return Err(error_code);
            }
        };
        let snapshot = match payload.snapshot.clone() {
            Some(snapshot) => snapshot,
            None => {
                warn!(
                    room_id = %room_id,
                    rollout_epoch = %rollout_epoch,
                    error_code = "ROOM_TRANSFER_MISSING_SNAPSHOT",
                    checksum = %checksum,
                    room_version = source_room_version,
                    "room transfer import rejected"
                );
                return Err("ROOM_TRANSFER_MISSING_SNAPSHOT");
            }
        };
        let transfer_state = match room_transfer_state_from_payload(&payload) {
            Ok(transfer_state) => transfer_state,
            Err(error_code) => {
                warn!(
                    room_id = %room_id,
                    rollout_epoch = %rollout_epoch,
                    error_code = error_code,
                    checksum = %checksum,
                    room_version = source_room_version,
                    "room transfer import rejected while decoding transfer state"
                );
                return Err(error_code);
            }
        };

        let mut rooms = self.rooms.lock().await;
        if rooms.contains_key(&room_id) {
            warn!(
                room_id = %room_id,
                rollout_epoch = %rollout_epoch,
                error_code = "ROOM_TRANSFER_ROOM_CONFLICT",
                checksum = %checksum,
                room_version = source_room_version,
                "room transfer import rejected because room already exists"
            );
            return Err("ROOM_TRANSFER_ROOM_CONFLICT");
        }

        let mut logic = self.logic_factory.create(&payload.policy_id);
        logic.on_room_created(&room_id);
        if let Err(error_code) = logic.import_transfer_state(&transfer_state) {
            warn!(
                room_id = %room_id,
                rollout_epoch = %rollout_epoch,
                error_code = error_code,
                checksum = %checksum,
                room_version = source_room_version,
                "room transfer import rejected by room logic"
            );
            return Err(error_code);
        }

        let mut room = Room::new(
            room_id.clone(),
            payload.owner_player_id.clone(),
            payload.policy_id.clone(),
            logic,
        );
        room.match_id = (!payload.match_id.is_empty()).then_some(payload.match_id.clone());
        room.phase = phase;
        room.current_frame = payload.current_frame_id;
        room.last_snapshot_frame = payload.current_frame_id;
        room.transfer_state.status = RoomTransferStatus::Importing;
        room.transfer_state.rollout_epoch = Some(rollout_epoch.clone());
        room.transfer_state.room_version = source_room_version.saturating_add(1);
        room.transfer_state.last_transfer_checksum = Some(checksum.clone());

        for member in snapshot.members {
            let (sender, _receiver) = mpsc::channel(1);
            room.members.insert(
                member.player_id.clone(),
                RoomMemberState {
                    player_id: member.player_id,
                    ready: member.ready,
                    sender,
                    close_state: ConnectionCloseState::new(),
                    offline: true,
                    offline_since: Some(Instant::now()),
                    role: if member.role == crate::pb::MemberRole::Observer as i32 {
                        MemberRole::Observer
                    } else {
                        MemberRole::Player
                    },
                    syncing: false,
                },
            );
        }

        for input in payload.recent_inputs {
            room.push_input_history(player_input_record_from_frame_input(input, true));
        }
        for input in payload.waiting_inputs {
            room.upsert_pending_input(player_input_record_from_frame_input(input, true));
        }
        if !room.has_online_members() {
            room.mark_empty();
        }
        room.transfer_state.status = RoomTransferStatus::OwnedByNew;

        rooms.insert(room_id.clone(), room);
        let room_count = rooms.len() as u64;
        METRICS.set_room_count(room_count);
        drop(rooms);

        self.runtimes
            .lock()
            .await
            .entry(room_id.clone())
            .or_insert_with(RoomRuntime::default);

        info!(
            room_id = %room_id,
            rollout_epoch = %rollout_epoch,
            error_code = "OK",
            checksum = %checksum,
            room_version = source_room_version.saturating_add(1),
            source_room_version = source_room_version,
            "room transfer payload imported"
        );

        Ok((checksum, source_room_version.saturating_add(1)))
    }

    pub async fn confirm_room_ownership(
        &self,
        rollout_epoch: &str,
        room_id: &str,
        checksum: &str,
        room_version: u64,
    ) -> Result<(String, u64), &'static str> {
        let rollout_epoch = rollout_epoch.trim();
        if rollout_epoch.is_empty() {
            warn!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "INVALID_ROLLOUT_EPOCH",
                checksum = checksum,
                room_version = room_version,
                "room ownership confirm rejected"
            );
            return Err("INVALID_ROLLOUT_EPOCH");
        }
        let checksum = checksum.trim();
        if checksum.is_empty() {
            warn!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "ROOM_TRANSFER_CHECKSUM_MISMATCH",
                room_version = room_version,
                "room ownership confirm rejected"
            );
            return Err("ROOM_TRANSFER_CHECKSUM_MISMATCH");
        }

        let rooms = self.rooms.lock().await;
        let room = match rooms.get(room_id) {
            Some(room) => room,
            None => {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_NOT_FOUND",
                    checksum = checksum,
                    room_version = room_version,
                    "room ownership confirm rejected"
                );
                return Err("ROOM_NOT_FOUND");
            }
        };

        if room.transfer_state.status != RoomTransferStatus::OwnedByNew {
            warn!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "ROOM_TRANSFER_NOT_OWNED_BY_NEW",
                current_status = transfer_status_label(room.transfer_state.status),
                room_version = room.transfer_state.room_version,
                "room ownership confirm rejected"
            );
            return Err("ROOM_TRANSFER_NOT_OWNED_BY_NEW");
        }
        if room.transfer_state.rollout_epoch.as_deref() != Some(rollout_epoch) {
            warn!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "ROOM_TRANSFER_EPOCH_MISMATCH",
                current_status = transfer_status_label(room.transfer_state.status),
                expected = ?room.transfer_state.rollout_epoch,
                actual = rollout_epoch,
                room_version = room.transfer_state.room_version,
                "room ownership confirm rejected"
            );
            return Err("ROOM_TRANSFER_EPOCH_MISMATCH");
        }
        if room.transfer_state.last_transfer_checksum.as_deref() != Some(checksum) {
            warn!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "ROOM_TRANSFER_CHECKSUM_MISMATCH",
                current_status = transfer_status_label(room.transfer_state.status),
                expected = ?room.transfer_state.last_transfer_checksum,
                actual = checksum,
                room_version = room.transfer_state.room_version,
                "room ownership confirm rejected due to checksum mismatch"
            );
            return Err("ROOM_TRANSFER_CHECKSUM_MISMATCH");
        }
        if room.transfer_state.room_version != room_version {
            warn!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "ROOM_TRANSFER_VERSION_MISMATCH",
                current_status = transfer_status_label(room.transfer_state.status),
                expected = room.transfer_state.room_version,
                actual = room_version,
                "room ownership confirm rejected due to room version mismatch"
            );
            return Err("ROOM_TRANSFER_VERSION_MISMATCH");
        }

        info!(
            room_id = room_id,
            rollout_epoch = rollout_epoch,
            error_code = "OK",
            checksum = checksum,
            room_version = room.transfer_state.room_version,
            current_status = transfer_status_label(room.transfer_state.status),
            "room ownership confirmed on new owner"
        );

        Ok((checksum.to_string(), room.transfer_state.room_version))
    }

    pub async fn retire_transferred_room(
        &self,
        rollout_epoch: &str,
        room_id: &str,
        checksum: &str,
    ) -> Result<(), &'static str> {
        let rollout_epoch = rollout_epoch.trim();
        if rollout_epoch.is_empty() {
            warn!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "INVALID_ROLLOUT_EPOCH",
                checksum = checksum,
                "room transfer retire rejected"
            );
            return Err("INVALID_ROLLOUT_EPOCH");
        }
        if checksum.trim().is_empty() {
            warn!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "ROOM_TRANSFER_CHECKSUM_MISMATCH",
                "room transfer retire rejected"
            );
            return Err("ROOM_TRANSFER_CHECKSUM_MISMATCH");
        }

        {
            let mut rooms = self.rooms.lock().await;
            let room = match rooms.get_mut(room_id) {
                Some(room) => room,
                None => {
                    warn!(
                        room_id = room_id,
                        rollout_epoch = rollout_epoch,
                        error_code = "ROOM_NOT_FOUND",
                        checksum = checksum,
                        "room transfer retire rejected"
                    );
                    return Err("ROOM_NOT_FOUND");
                }
            };

            if room.transfer_state.status == RoomTransferStatus::Retired {
                info!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "IDEMPOTENT_ROOM_TRANSFER_RETIRE",
                    current_status = transfer_status_label(room.transfer_state.status),
                    checksum = checksum,
                    room_version = room.transfer_state.room_version,
                    "room transfer retire idempotent replay"
                );
                return Ok(());
            }
            if !matches!(
                room.transfer_state.status,
                RoomTransferStatus::Frozen | RoomTransferStatus::Exported
            ) {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_TRANSFER_NOT_EXPORTED",
                    current_status = transfer_status_label(room.transfer_state.status),
                    checksum = checksum,
                    room_version = room.transfer_state.room_version,
                    "room transfer retire rejected"
                );
                return Err("ROOM_TRANSFER_NOT_EXPORTED");
            }
            if room.transfer_state.rollout_epoch.as_deref() != Some(rollout_epoch) {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_TRANSFER_EPOCH_MISMATCH",
                    current_status = transfer_status_label(room.transfer_state.status),
                    expected = ?room.transfer_state.rollout_epoch,
                    actual = rollout_epoch,
                    checksum = checksum,
                    room_version = room.transfer_state.room_version,
                    "room transfer retire rejected"
                );
                return Err("ROOM_TRANSFER_EPOCH_MISMATCH");
            }
            if room.transfer_state.last_transfer_checksum.as_deref() != Some(checksum) {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_TRANSFER_CHECKSUM_MISMATCH",
                    current_status = transfer_status_label(room.transfer_state.status),
                    expected = ?room.transfer_state.last_transfer_checksum,
                    actual = checksum,
                    room_version = room.transfer_state.room_version,
                    "room transfer retire rejected due to checksum mismatch"
                );
                return Err("ROOM_TRANSFER_CHECKSUM_MISMATCH");
            }

            room.members.clear();
            room.pending_inputs.clear();
            room.wait_started_at = None;
            room.transfer_state.status = RoomTransferStatus::Retired;
            room.transfer_state.bump_version();

            info!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "OK",
                checksum = checksum,
                room_version = room.transfer_state.room_version,
                current_status = transfer_status_label(room.transfer_state.status),
                "room retired after transfer"
            );
        }

        self.stop_room_tick(room_id).await;
        Ok(())
    }

    pub async fn trigger_server_redirect(
        &self,
        room_id: &str,
        push: ServerRedirectPush,
    ) -> Result<ServerRedirectDelivery, &'static str> {
        if room_id.trim().is_empty() {
            return Err("INVALID_ROOM_ID");
        }
        if push.rollout_epoch.trim().is_empty() {
            return Err("INVALID_ROLLOUT_EPOCH");
        }
        if push.target_host.trim().is_empty() || push.target_port == 0 {
            return Err("INVALID_REDIRECT_TARGET");
        }

        let body = encode_body(&push);
        let targets = {
            let rooms = self.rooms.lock().await;
            let room = rooms.get(room_id).ok_or("ROOM_NOT_FOUND")?;
            room.members
                .values()
                .filter(|member| !member.offline && !member.syncing)
                .map(|member| {
                    (
                        member.player_id.clone(),
                        member.sender.clone(),
                        member.close_state.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };

        let mut delivered_count = 0u64;
        let mut failed_count = 0u64;
        for (player_id, sender, close_state) in &targets {
            match try_send_outbound(
                sender,
                close_state,
                OutboundMessage {
                    message_type: MessageType::ServerRedirectPush,
                    seq: 0,
                    body: body.clone(),
                },
                OutboundQueueLogContext {
                    player_id: Some(player_id),
                    room_id: Some(room_id),
                    operation: "server_redirect_push",
                    ..OutboundQueueLogContext::default()
                },
            ) {
                Ok(()) => {
                    delivered_count = delivered_count.saturating_add(1);
                    let close_requested = close_state.request_close(SERVER_REDIRECT_CLOSE_REASON);
                    info!(
                        room_id = room_id,
                        player_id = %player_id,
                        rollout_epoch = %push.rollout_epoch,
                        target_host = %push.target_host,
                        target_port = push.target_port,
                        target_server_id = %push.target_server_id,
                        close_reason = SERVER_REDIRECT_CLOSE_REASON,
                        close_requested = close_requested,
                        "server redirect push queued and connection close requested"
                    );
                }
                Err(error) => {
                    failed_count = failed_count.saturating_add(1);
                    warn!(
                        room_id = room_id,
                        player_id = %player_id,
                        rollout_epoch = %push.rollout_epoch,
                        target_host = %push.target_host,
                        target_port = push.target_port,
                        target_server_id = %push.target_server_id,
                        error = %error,
                        "failed to queue server redirect push"
                    );
                }
            }
        }

        let online_member_count = targets.len() as u64;
        info!(
            room_id = room_id,
            rollout_epoch = %push.rollout_epoch,
            target_host = %push.target_host,
            target_port = push.target_port,
            target_server_id = %push.target_server_id,
            delivered_count = delivered_count,
            failed_count = failed_count,
            online_member_count = online_member_count,
            "server redirect trigger completed"
        );

        Ok(ServerRedirectDelivery {
            delivered_count,
            failed_count,
            online_member_count,
        })
    }

    pub async fn broadcast_snapshot(
        &self,
        room_id: &str,
        event: &str,
        snapshot: RoomSnapshot,
    ) -> Result<(), std::io::Error> {
        let body = encode_body(&RoomStatePush {
            event: event.to_string(),
            snapshot: Some(snapshot),
        });
        self.broadcast_to_room(room_id, MessageType::RoomStatePush, body)
            .await
    }

    async fn ensure_room_tick_running(&self, room_id: &str) {
        let should_spawn = {
            let mut runtimes = self.runtimes.lock().await;
            let runtime = runtimes
                .entry(room_id.to_string())
                .or_insert_with(RoomRuntime::default);
            if runtime.tick_running {
                false
            } else {
                runtime.tick_running = true;
                true
            }
        };

        if !should_spawn {
            return;
        }

        info!(room_id = room_id, "room tick started");

        let manager = self.clone();
        let room_id_owned = room_id.to_string();
        let handle = tokio::spawn(async move {
            manager.run_room_tick_loop(room_id_owned).await;
        });

        let mut runtimes = self.runtimes.lock().await;
        if let Some(runtime) = runtimes.get_mut(room_id) {
            runtime.tick_handle = Some(handle);
        }
    }

    async fn stop_room_tick(&self, room_id: &str) {
        let mut runtimes = self.runtimes.lock().await;
        if let Some(runtime) = runtimes.get_mut(room_id) {
            if let Some(handle) = runtime.tick_handle.take() {
                handle.abort();
            }
            runtime.tick_running = false;
        }
    }

    async fn update_room_fps(&self, room_id: &str) {
        let target_fps = {
            let rooms = self.rooms.lock().await;
            let Some(room) = rooms.get(room_id) else {
                return;
            };
            self.compute_room_fps(room)
        };

        let changed = {
            let mut runtimes = self.runtimes.lock().await;
            let Some(runtime) = runtimes.get_mut(room_id) else {
                return;
            };
            let previous_fps = runtime.current_fps;
            runtime.current_fps = target_fps;
            if previous_fps != target_fps {
                info!(
                    room_id = room_id,
                    previous_fps = previous_fps,
                    current_fps = target_fps,
                    "room fps updated"
                );
                true
            } else {
                false
            }
        };

        if changed {
            let push = RoomFrameRatePush {
                room_id: room_id.to_string(),
                fps: u32::from(target_fps),
                reason: "runtime_policy_changed".to_string(),
            };
            let body = encode_body(&push);
            let _ = self
                .broadcast_to_room(room_id, MessageType::RoomFrameRatePush, body)
                .await;
        }
    }

    fn compute_room_fps(&self, room: &Room) -> u16 {
        let policy = self.policies.resolve(&room.policy_id);
        let online_count = room.broadcast_members().len();

        if online_count == 0 {
            return policy.silent_room_fps.max(1);
        }

        match room.phase {
            RoomPhase::Waiting => policy.idle_room_fps.max(1),
            RoomPhase::InGame => {
                if online_count >= policy.busy_room_player_threshold {
                    policy.busy_room_fps.max(1)
                } else {
                    policy.active_room_fps.max(1)
                }
            }
        }
    }

    async fn process_room_tick(
        &self,
        room_id: &str,
        fps: u16,
    ) -> Option<(FrameBundlePush, Vec<RoomLogicBroadcast>)> {
        let mut rooms = self.rooms.lock().await;
        let room = rooms.get_mut(room_id)?;

        room.update_activity();

        if room.transfer_state.status.rejects_room_mutation() {
            return None;
        }

        if room.phase != RoomPhase::InGame {
            return None;
        }

        if room.player_input_participants().is_empty() {
            room.wait_started_at = None;
            return None;
        }

        let policy = self.policies.resolve(&room.policy_id);
        let snapshot_interval = policy.snapshot_interval_frames;
        room.ensure_wait_started();

        let waiting_frame_id = room.current_waiting_frame_id();
        let participants = room.player_input_participants();
        let ready_count = room
            .pending_inputs_for_frame(waiting_frame_id)
            .into_iter()
            .filter(|input| {
                participants
                    .iter()
                    .any(|player_id| player_id == &input.player_id)
            })
            .count();
        let all_inputs_arrived = ready_count == participants.len();
        let wait_timed_out = room
            .wait_started_at
            .map(|started_at| started_at.elapsed().as_millis() as u64 >= policy.wait_timeout_ms)
            .unwrap_or(false);

        let should_advance = match policy.wait_strategy {
            InputWaitStrategy::Strict => all_inputs_arrived || wait_timed_out,
            InputWaitStrategy::Optimistic => {
                all_inputs_arrived || ready_count > 0 || wait_timed_out
            }
        };

        if !should_advance {
            return None;
        }

        let tick_inputs = resolve_tick_inputs(room, &participants, waiting_frame_id, &policy);
        let inputs = tick_inputs
            .iter()
            .map(frame_input_from_record)
            .collect::<Vec<_>>();

        room.current_frame = waiting_frame_id;
        room.reset_wait_started();

        room.logic.on_tick(waiting_frame_id, fps, &tick_inputs);
        let pending_broadcasts = room.logic.take_pending_broadcasts();

        for input in &tick_inputs {
            room.push_input_history(input.clone());
        }

        let snapshot = if waiting_frame_id % snapshot_interval == 0 {
            room.last_snapshot_frame = waiting_frame_id;
            info!(
                room_id = %room_id,
                frame_id = waiting_frame_id,
                snapshot_interval = snapshot_interval,
                ">>> SNAPSHOT GENERATED at frame {} <<<",
                waiting_frame_id
            );
            Some(room.snapshot())
        } else {
            None
        };

        let is_silent_frame = tick_inputs.iter().all(|input| input.action.is_empty());
        if !tick_inputs.is_empty() {
            info!(
                room_id = %room_id,
                frame_id = waiting_frame_id,
                fps = fps,
                input_count = tick_inputs.len(),
                has_snapshot = snapshot.is_some(),
                wait_timed_out = wait_timed_out,
                "FRAME Bundle: inputs={} frame={} fps={}",
                tick_inputs.len(),
                waiting_frame_id,
                fps
            );
            for input in &tick_inputs {
                debug!(
                    room_id = %room_id,
                    frame_id = waiting_frame_id,
                    player_id = %input.player_id,
                    action = %input.action,
                    "  └─ [{}] {}: {}",
                    input.player_id,
                    input.action,
                    input.payload_json
                );
            }
        } else {
            info!(
                room_id = %room_id,
                frame_id = waiting_frame_id,
                fps = fps,
                has_snapshot = snapshot.is_some(),
                wait_timed_out = wait_timed_out,
                "FRAME Bundle: SILENT frame={} fps={}",
                waiting_frame_id,
                fps
            );
        }

        Some((
            FrameBundlePush {
                room_id: room.room_id.clone(),
                frame_id: waiting_frame_id,
                fps: u32::from(fps),
                is_silent_frame,
                inputs,
                snapshot,
            },
            pending_broadcasts,
        ))
    }

    async fn run_room_tick_loop(self, room_id: String) {
        loop {
            let fps = {
                let runtimes = self.runtimes.lock().await;
                let Some(runtime) = runtimes.get(&room_id) else {
                    break;
                };
                runtime.current_fps.max(1)
            };

            let interval = Duration::from_millis((1000 / u64::from(fps.max(1))).max(1));
            let deadline = TokioInstant::now() + interval;
            sleep_until(deadline).await;

            let Some((frame_bundle, pending_broadcasts)) =
                self.process_room_tick(&room_id, fps).await
            else {
                continue;
            };

            self.broadcast_logic_broadcasts(&room_id, pending_broadcasts)
                .await;

            let body = encode_body(&frame_bundle);
            let _ = self
                .broadcast_to_room(&room_id, MessageType::FrameBundlePush, body)
                .await;
        }

        let mut runtimes = self.runtimes.lock().await;
        if let Some(runtime) = runtimes.get_mut(&room_id) {
            runtime.tick_running = false;
            runtime.tick_handle = None;
        }
        info!(room_id = room_id, "room tick stopped");
    }

    async fn broadcast_to_room(
        &self,
        room_id: &str,
        message_type: MessageType,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        let senders = {
            let rooms = self.rooms.lock().await;
            let Some(room) = rooms.get(room_id) else {
                info!(room_id = room_id, "broadcast_to_room: room not found");
                return Ok(());
            };

            let online = room.broadcast_members();
            info!(
                room_id = room_id,
                message_type = ?message_type,
                online_count = online.len(),
                "broadcast_to_room"
            );

            online
                .iter()
                .map(|member| {
                    (
                        member.player_id.clone(),
                        member.sender.clone(),
                        member.close_state.clone(),
                    )
                })
                .collect::<Vec<_>>()
        };

        for (player_id, sender, close_state) in senders {
            if let Err(error) = try_send_outbound(
                &sender,
                &close_state,
                OutboundMessage {
                    message_type,
                    seq: 0,
                    body: body.clone(),
                },
                OutboundQueueLogContext {
                    player_id: Some(&player_id),
                    room_id: Some(room_id),
                    operation: "room_broadcast",
                    ..OutboundQueueLogContext::default()
                },
            ) {
                warn!(
                    room_id = room_id,
                    player_id = %player_id,
                    message_type = ?message_type,
                    error = %error,
                    "failed to queue room broadcast"
                );
            }
        }

        Ok(())
    }

    async fn broadcast_to_players(
        &self,
        room_id: &str,
        target_player_ids: &[String],
        message_type: MessageType,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        let senders = {
            let rooms = self.rooms.lock().await;
            let Some(room) = rooms.get(room_id) else {
                info!(room_id = room_id, "broadcast_to_players: room not found");
                return Ok(());
            };

            let targets = target_player_ids
                .iter()
                .filter_map(|player_id| room.members.get(player_id))
                .filter(|member| !member.offline && !member.syncing)
                .map(|member| {
                    (
                        member.player_id.clone(),
                        member.sender.clone(),
                        member.close_state.clone(),
                    )
                })
                .collect::<Vec<_>>();

            info!(
                room_id = room_id,
                message_type = ?message_type,
                target_count = targets.len(),
                "broadcast_to_players"
            );

            targets
        };

        for (player_id, sender, close_state) in senders {
            if let Err(error) = try_send_outbound(
                &sender,
                &close_state,
                OutboundMessage {
                    message_type,
                    seq: 0,
                    body: body.clone(),
                },
                OutboundQueueLogContext {
                    player_id: Some(&player_id),
                    room_id: Some(room_id),
                    operation: "targeted_room_broadcast",
                    ..OutboundQueueLogContext::default()
                },
            ) {
                warn!(
                    room_id = room_id,
                    player_id = %player_id,
                    message_type = ?message_type,
                    error = %error,
                    "failed to queue targeted room broadcast"
                );
            }
        }

        Ok(())
    }

    async fn broadcast_message(
        &self,
        room_id: &str,
        target_player_ids: &[String],
        message_type: MessageType,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        if target_player_ids.is_empty() {
            self.broadcast_to_room(room_id, message_type, body).await
        } else {
            self.broadcast_to_players(room_id, target_player_ids, message_type, body)
                .await
        }
    }

    async fn broadcast_logic_broadcasts(&self, room_id: &str, broadcasts: Vec<RoomLogicBroadcast>) {
        for RoomLogicBroadcast {
            message_type,
            body,
            target_player_ids,
        } in broadcasts
        {
            let _ = self
                .broadcast_message(room_id, &target_player_ids, message_type, body)
                .await;
        }
    }

    pub async fn send_to_player(
        &self,
        player_id: &str,
        message_type: MessageType,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        let outbound = {
            let rooms = self.rooms.lock().await;
            rooms.values().find_map(|room| {
                room.members.get(player_id).and_then(|member| {
                    if member.offline {
                        None
                    } else {
                        Some((member.sender.clone(), member.close_state.clone()))
                    }
                })
            })
        };

        if let Some((sender, close_state)) = outbound {
            if let Err(error) = try_send_outbound(
                &sender,
                &close_state,
                OutboundMessage {
                    message_type,
                    seq: 0,
                    body,
                },
                OutboundQueueLogContext {
                    player_id: Some(player_id),
                    operation: "send_to_player",
                    ..OutboundQueueLogContext::default()
                },
            ) {
                warn!(
                    player_id = player_id,
                    message_type = ?message_type,
                    error = %error,
                    "failed to queue player message"
                );
            }
        }

        Ok(())
    }
}

fn frame_input_from_record(input: &PlayerInputRecord) -> FrameInput {
    FrameInput {
        player_id: input.player_id.clone(),
        action: input.action.clone(),
        payload_json: input.payload_json.clone(),
        frame_id: input.frame_id,
    }
}

fn room_frame_inputs_from_history(room: &Room, current_frame_id: u32) -> Vec<FrameInput> {
    room.get_inputs_in_range(current_frame_id.saturating_sub(300), current_frame_id)
        .into_iter()
        .map(frame_input_from_record)
        .collect()
}

fn room_frame_inputs_from_pending(room: &Room, frame_id: u32) -> Vec<FrameInput> {
    room.pending_inputs_for_frame(frame_id)
        .into_iter()
        .map(frame_input_from_record)
        .collect()
}

fn player_input_record_from_frame_input(
    input: FrameInput,
    is_synthetic: bool,
) -> PlayerInputRecord {
    PlayerInputRecord {
        frame_id: input.frame_id,
        player_id: input.player_id,
        action: input.action,
        payload_json: input.payload_json,
        received_at: Instant::now(),
        is_synthetic,
    }
}

fn room_phase_name(phase: RoomPhase) -> &'static str {
    match phase {
        RoomPhase::Waiting => "waiting",
        RoomPhase::InGame => "in_game",
    }
}

fn parse_room_phase(value: &str) -> Result<RoomPhase, &'static str> {
    match value {
        "waiting" | "empty" | "ready" => Ok(RoomPhase::Waiting),
        "in_game" => Ok(RoomPhase::InGame),
        _ => Err("ROOM_TRANSFER_INVALID_PHASE"),
    }
}

fn room_transfer_checksum(payload: &RoomTransferPayload) -> String {
    let mut canonical = payload.clone();
    canonical.checksum.clear();
    let mut encoded = Vec::new();
    canonical
        .encode(&mut encoded)
        .expect("room transfer payload encode failed");
    format!("{:x}", Sha256::digest(&encoded))
}

fn room_transfer_logic_state_json(state: &RoomLogicTransferState) -> String {
    json!({
        "schema": "room-transfer.logic.v1",
        "schemaVersion": state.schema_version,
        "logicStateJson": state.logic_state_json,
        "combatStateJson": state.combat_state_json,
        "npcStateJson": state.npc_state_json,
    })
    .to_string()
}

fn room_transfer_movement_state_json(state: &RoomLogicTransferState) -> String {
    json!({
        "schema": "room-transfer.movement.v1",
        "schemaVersion": state.schema_version,
        "movementStateJson": state.movement_state_json,
    })
    .to_string()
}

fn room_transfer_timer_state_json(
    state: &RoomLogicTransferState,
    runtime_summary: serde_json::Value,
) -> String {
    json!({
        "schema": "room-transfer.runtime-timers.v1",
        "schemaVersion": state.schema_version,
        "timerStateJson": state.timer_state_json,
        "runtimeSummary": runtime_summary,
    })
    .to_string()
}

fn room_transfer_state_from_payload(
    payload: &RoomTransferPayload,
) -> Result<RoomLogicTransferState, &'static str> {
    let logic =
        serde_json::from_str::<serde_json::Value>(&payload.logic_state_json).map_err(|_| {
            if payload.logic_state_json.trim().is_empty() {
                UNSUPPORTED_ROOM_TRANSFER
            } else {
                "ROOM_TRANSFER_INVALID_LOGIC_STATE"
            }
        })?;
    let movement = serde_json::from_str::<serde_json::Value>(&payload.movement_state_json)
        .map_err(|_| "ROOM_TRANSFER_INVALID_MOVEMENT_STATE")?;
    let timers = serde_json::from_str::<serde_json::Value>(&payload.runtime_timers_json)
        .map_err(|_| "ROOM_TRANSFER_INVALID_TIMER_STATE")?;

    if logic.get("schema").and_then(|value| value.as_str()) != Some("room-transfer.logic.v1") {
        return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
    }
    if movement.get("schema").and_then(|value| value.as_str()) != Some("room-transfer.movement.v1")
    {
        return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
    }
    if timers.get("schema").and_then(|value| value.as_str())
        != Some("room-transfer.runtime-timers.v1")
    {
        return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
    }

    let schema_version = logic
        .get("schemaVersion")
        .and_then(|value| value.as_u64())
        .ok_or("ROOM_TRANSFER_UNSUPPORTED_SCHEMA")?;
    if schema_version != ROOM_TRANSFER_SCHEMA_VERSION as u64
        || movement
            .get("schemaVersion")
            .and_then(|value| value.as_u64())
            != Some(schema_version)
        || timers.get("schemaVersion").and_then(|value| value.as_u64()) != Some(schema_version)
    {
        return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
    }

    Ok(RoomLogicTransferState {
        schema_version: schema_version as u32,
        logic_state_json: logic
            .get("logicStateJson")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        movement_state_json: movement
            .get("movementStateJson")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        combat_state_json: logic
            .get("combatStateJson")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        npc_state_json: logic
            .get("npcStateJson")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        timer_state_json: timers
            .get("timerStateJson")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

fn validate_room_transfer_payload(payload: &RoomTransferPayload) -> Result<(), &'static str> {
    if payload.rollout_epoch.trim().is_empty() {
        return Err("INVALID_ROLLOUT_EPOCH");
    }
    if payload.room_id.trim().is_empty() {
        return Err("ROOM_TRANSFER_INVALID_ROOM_ID");
    }
    if payload.policy_id.trim().is_empty() {
        return Err("ROOM_TRANSFER_INVALID_POLICY");
    }
    if payload.owner_player_id.trim().is_empty() {
        return Err("ROOM_TRANSFER_INVALID_OWNER");
    }
    if payload.snapshot.is_none() {
        return Err("ROOM_TRANSFER_MISSING_SNAPSHOT");
    }
    if payload.checksum.trim().is_empty() {
        return Err("ROOM_TRANSFER_CHECKSUM_MISMATCH");
    }
    let expected = room_transfer_checksum(payload);
    if expected != payload.checksum {
        return Err("ROOM_TRANSFER_CHECKSUM_MISMATCH");
    }
    parse_room_phase(&payload.room_phase)?;
    Ok(())
}

fn synthetic_empty_input(frame_id: u32, player_id: &str) -> PlayerInputRecord {
    PlayerInputRecord {
        frame_id,
        player_id: player_id.to_string(),
        action: String::new(),
        payload_json: String::new(),
        received_at: Instant::now(),
        is_synthetic: true,
    }
}

fn clone_input_for_frame(frame_id: u32, input: &PlayerInputRecord) -> PlayerInputRecord {
    PlayerInputRecord {
        frame_id,
        player_id: input.player_id.clone(),
        action: input.action.clone(),
        payload_json: input.payload_json.clone(),
        received_at: Instant::now(),
        is_synthetic: true,
    }
}

fn resolve_tick_inputs(
    room: &mut Room,
    participants: &[String],
    frame_id: u32,
    policy: &RoomRuntimePolicy,
) -> Vec<PlayerInputRecord> {
    let mut frame_inputs = room.take_pending_inputs_for_frame(frame_id);
    let mut resolved_inputs = Vec::with_capacity(participants.len());

    for player_id in participants {
        if let Some(input) = frame_inputs.remove(player_id) {
            room.reset_missing_input_streak(player_id);
            room.set_last_applied_input(player_id, input.clone());
            resolved_inputs.push(input);
            continue;
        }

        let resolved = match policy.missing_input_strategy {
            MissingInputStrategy::Empty => synthetic_empty_input(frame_id, player_id),
            MissingInputStrategy::RepeatLast => room
                .last_applied_input_for_player(player_id)
                .map(|input| clone_input_for_frame(frame_id, input))
                .unwrap_or_else(|| synthetic_empty_input(frame_id, player_id)),
            MissingInputStrategy::DropAfterMisses => {
                let streak = room.increment_missing_input_streak(player_id);
                if streak >= MAX_MISSING_INPUT_STREAK_BEFORE_OFFLINE {
                    let should_mark_offline = room
                        .members
                        .get(player_id)
                        .map(|member| !member.offline)
                        .unwrap_or(false);
                    if should_mark_offline {
                        if let Some(member) = room.members.get_mut(player_id) {
                            member.offline = true;
                            member.offline_since = Some(Instant::now());
                        }
                        room.logic.on_player_offline(&room.room_id, player_id);
                    }
                }
                synthetic_empty_input(frame_id, player_id)
            }
        };

        room.set_last_applied_input(player_id, resolved.clone());
        resolved_inputs.push(resolved);
    }

    resolved_inputs
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex as StdMutex};

    use tokio::sync::mpsc;

    use crate::core::logic::{
        ROOM_TRANSFER_SCHEMA_VERSION, RoomLogic, RoomLogicFactory, RoomLogicTransfer,
        RoomLogicTransferState,
    };
    use crate::core::room::PlayerInputRecord;

    use super::*;

    #[derive(Clone, Default)]
    struct RecordingRoomLogicFactory {
        ticks: Arc<StdMutex<Vec<(u32, Vec<PlayerInputRecord>)>>>,
        inputs: Arc<StdMutex<Vec<(String, String, String)>>>,
        imported_transfer_states: Arc<StdMutex<Vec<RoomLogicTransferState>>>,
    }

    impl RecordingRoomLogicFactory {
        fn recorded_ticks(&self) -> Vec<(u32, Vec<PlayerInputRecord>)> {
            self.ticks.lock().unwrap().clone()
        }

        fn recorded_inputs(&self) -> Vec<(String, String, String)> {
            self.inputs.lock().unwrap().clone()
        }

        fn imported_transfer_states(&self) -> Vec<RoomLogicTransferState> {
            self.imported_transfer_states.lock().unwrap().clone()
        }
    }

    struct RecordingRoomLogic {
        ticks: Arc<StdMutex<Vec<(u32, Vec<PlayerInputRecord>)>>>,
        inputs: Arc<StdMutex<Vec<(String, String, String)>>>,
        imported_transfer_states: Arc<StdMutex<Vec<RoomLogicTransferState>>>,
        state: String,
    }

    impl RoomLogic for RecordingRoomLogic {
        fn on_player_input(&mut self, player_id: &str, action: &str, payload_json: &str) {
            self.inputs.lock().unwrap().push((
                player_id.to_string(),
                action.to_string(),
                payload_json.to_string(),
            ));
        }

        fn on_tick(&mut self, frame_id: u32, _fps: u16, inputs: &[PlayerInputRecord]) {
            self.ticks.lock().unwrap().push((frame_id, inputs.to_vec()));
        }

        fn get_serialized_state(&self) -> String {
            self.state.clone()
        }
    }

    impl RoomLogicTransfer for RecordingRoomLogic {
        fn export_transfer_state(&self) -> Result<RoomLogicTransferState, &'static str> {
            Ok(RoomLogicTransferState {
                schema_version: ROOM_TRANSFER_SCHEMA_VERSION,
                logic_state_json: self.state.clone(),
                movement_state_json: r#"{"movement":"recording-v1"}"#.to_string(),
                combat_state_json: r#"{"combat":"recording-v1"}"#.to_string(),
                npc_state_json: r#"{"npc":"recording-v1"}"#.to_string(),
                timer_state_json: r#"{"timer":"recording-v1"}"#.to_string(),
            })
        }

        fn import_transfer_state(
            &mut self,
            state: &RoomLogicTransferState,
        ) -> Result<(), &'static str> {
            if state.schema_version != ROOM_TRANSFER_SCHEMA_VERSION {
                return Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA");
            }

            self.state = state.logic_state_json.clone();
            self.imported_transfer_states
                .lock()
                .unwrap()
                .push(state.clone());
            Ok(())
        }
    }

    impl RoomLogicFactory for RecordingRoomLogicFactory {
        fn create(&self, _policy_id: &str) -> Box<dyn RoomLogic> {
            Box::new(RecordingRoomLogic {
                ticks: Arc::clone(&self.ticks),
                inputs: Arc::clone(&self.inputs),
                imported_transfer_states: Arc::clone(&self.imported_transfer_states),
                state: "recording-state-v1".to_string(),
            })
        }
    }

    struct UnsupportedTransferRoomLogic;

    impl RoomLogicTransfer for UnsupportedTransferRoomLogic {}

    impl RoomLogic for UnsupportedTransferRoomLogic {}

    struct UnsupportedTransferRoomLogicFactory;

    impl RoomLogicFactory for UnsupportedTransferRoomLogicFactory {
        fn create(&self, _policy_id: &str) -> Box<dyn RoomLogic> {
            Box::new(UnsupportedTransferRoomLogic)
        }
    }

    async fn setup_started_room(
        policy_id: &str,
        players: &[&str],
    ) -> (
        RoomManager,
        RecordingRoomLogicFactory,
        Vec<mpsc::Receiver<OutboundMessage>>,
    ) {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory.clone()),
        );

        let mut receivers = Vec::new();
        for player_id in players {
            let (tx, rx) = mpsc::channel(1024);
            receivers.push(rx);
            manager
                .join_room(
                    "room-test",
                    player_id,
                    tx,
                    MemberRole::Player,
                    Some(policy_id),
                )
                .await
                .unwrap();
            manager
                .set_ready_state("room-test", player_id, true)
                .await
                .unwrap();
        }
        manager.start_game("room-test", players[0]).await.unwrap();
        {
            let mut runtimes = manager.runtimes.lock().await;
            if let Some(runtime) = runtimes.get_mut("room-test") {
                if let Some(handle) = runtime.tick_handle.take() {
                    handle.abort();
                }
                runtime.tick_running = false;
            }
        }

        (manager, factory, receivers)
    }

    fn drain_messages_of_type(
        receiver: &mut mpsc::Receiver<OutboundMessage>,
        message_type: MessageType,
    ) -> Vec<OutboundMessage> {
        let mut messages = Vec::new();
        while let Ok(message) = receiver.try_recv() {
            if message.message_type == message_type {
                messages.push(message);
            }
        }
        messages
    }

    #[tokio::test]
    async fn room_exists_reflects_room_creation() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory),
        );

        assert!(!manager.room_exists("room-test").await);

        let (tx, _rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-test",
                "player-a",
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();

        assert!(manager.room_exists("room-test").await);
    }

    #[tokio::test]
    async fn freeze_empty_or_offline_room_for_transfer_succeeds() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        manager
            .disconnect_room_member("room-test", "player-b")
            .await;

        let result = manager
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();

        assert_eq!(result.0, RoomMigrationState::FrozenForTransfer);
        assert!(result.1 > 1);
        assert_eq!(
            manager
                .accept_player_input("room-test", "player-a", 1, "move", "{}")
                .await,
            Err("ROOM_TRANSFER_FROZEN")
        );
    }

    #[tokio::test]
    async fn freeze_online_room_for_transfer_is_rejected() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory),
        );
        let (tx, _rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-test",
                "player-a",
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();

        let result = manager
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await;

        assert_eq!(result, Err("ROOM_TRANSFER_HAS_ONLINE_MEMBERS"));
    }

    #[tokio::test]
    async fn freeze_room_for_transfer_rejects_invalid_epoch_or_missing_room() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory),
        );

        assert_eq!(
            manager.freeze_room_for_transfer("", "room-test").await,
            Err("INVALID_ROLLOUT_EPOCH")
        );
        assert_eq!(
            manager
                .freeze_room_for_transfer("epoch-1", "room-missing")
                .await,
            Err("ROOM_NOT_FOUND")
        );
    }

    #[tokio::test]
    async fn freeze_room_for_transfer_rejects_mismatched_epoch_after_freeze() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        manager
            .disconnect_room_member("room-test", "player-b")
            .await;
        manager
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();

        let result = manager
            .freeze_room_for_transfer("epoch-2", "room-test")
            .await;

        assert_eq!(result, Err("ROOM_TRANSFER_EPOCH_MISMATCH"));
    }

    #[tokio::test]
    async fn rollout_drain_snapshot_empty_manager_returns_zero_counts() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory),
        );

        let snapshot = manager.rollout_drain_snapshot("game-server-old", 50).await;

        assert_eq!(snapshot.owner_server_id, "game-server-old");
        assert_eq!(snapshot.owned_room_count, 0);
        assert_eq!(snapshot.migrating_room_count, 0);
        assert!(snapshot.rollout_epoch.is_empty());
        assert!(snapshot.routes.is_empty());
        assert_eq!(snapshot.transferable_empty_room_count, 0);
        assert!(snapshot.transferable_empty_room_samples.is_empty());
        assert_eq!(snapshot.retired_room_count, 0);
    }

    #[tokio::test]
    async fn rollout_drain_snapshot_counts_owned_room() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory),
        );
        let (tx, _rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-test",
                "player-a",
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();

        let snapshot = manager.rollout_drain_snapshot("game-server-old", 50).await;

        assert_eq!(snapshot.owned_room_count, 1);
        assert_eq!(snapshot.migrating_room_count, 0);
        assert_eq!(snapshot.transferable_empty_room_count, 0);
        assert!(snapshot.transferable_empty_room_samples.is_empty());
        assert_eq!(snapshot.retired_room_count, 0);
        assert_eq!(snapshot.routes.len(), 1);
        let route = &snapshot.routes[0];
        assert_eq!(route.room_id, "room-test");
        assert_eq!(route.owner_server_id, "game-server-old");
        assert_eq!(route.migration_state, RoomMigrationState::OwnedByOld as i32);
        assert_eq!(route.member_count, 1);
        assert_eq!(route.online_member_count, 1);
        assert_eq!(route.room_version, 1);
    }

    #[tokio::test]
    async fn rollout_drain_snapshot_counts_empty_owned_rooms_as_transferable() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory.clone()),
        );

        let empty_room = Room::new(
            "room-empty".to_string(),
            "owner".to_string(),
            "default_match".to_string(),
            factory.create("default_match"),
        );
        manager
            .rooms
            .lock()
            .await
            .insert("room-empty".to_string(), empty_room);

        let (offline_tx, _offline_rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-offline",
                "player-offline",
                offline_tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();
        manager
            .disconnect_room_member("room-offline", "player-offline")
            .await;

        let (online_tx, _online_rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-online",
                "player-online",
                online_tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();

        let snapshot = manager.rollout_drain_snapshot("game-server-old", 50).await;

        assert_eq!(snapshot.owned_room_count, 3);
        assert_eq!(snapshot.migrating_room_count, 0);
        assert_eq!(snapshot.transferable_empty_room_count, 2);
        assert_eq!(snapshot.retired_room_count, 0);
        assert_eq!(
            snapshot
                .transferable_empty_room_samples
                .iter()
                .map(|route| route.room_id.as_str())
                .collect::<Vec<_>>(),
            vec!["room-empty", "room-offline"]
        );
        assert!(
            snapshot
                .transferable_empty_room_samples
                .iter()
                .all(|route| route.migration_state == RoomMigrationState::OwnedByOld as i32)
        );
        assert!(
            snapshot
                .transferable_empty_room_samples
                .iter()
                .all(|route| route.online_member_count == 0)
        );
    }

    #[tokio::test]
    async fn rollout_drain_snapshot_counts_transfer_states_as_migrating() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory.clone()),
        );

        for room_id in ["room-frozen", "room-exported", "room-importing"] {
            let mut room = Room::new(
                room_id.to_string(),
                "owner".to_string(),
                "default_match".to_string(),
                factory.create("default_match"),
            );
            room.mark_empty();
            room.transfer_state.rollout_epoch = Some("epoch-1".to_string());
            room.transfer_state.status = match room_id {
                "room-frozen" => RoomTransferStatus::Frozen,
                "room-exported" => RoomTransferStatus::Exported,
                "room-importing" => RoomTransferStatus::Importing,
                _ => unreachable!(),
            };
            manager.rooms.lock().await.insert(room_id.to_string(), room);
        }

        let snapshot = manager.rollout_drain_snapshot("game-server-old", 50).await;

        assert_eq!(snapshot.rollout_epoch, "epoch-1");
        assert_eq!(snapshot.owned_room_count, 0);
        assert_eq!(snapshot.migrating_room_count, 3);
        assert_eq!(snapshot.transferable_empty_room_count, 0);
        assert!(snapshot.transferable_empty_room_samples.is_empty());
        assert_eq!(snapshot.retired_room_count, 0);
        assert_eq!(snapshot.routes.len(), 3);
        assert_eq!(
            snapshot
                .routes
                .iter()
                .map(|route| route.migration_state)
                .collect::<Vec<_>>(),
            vec![
                RoomMigrationState::FrozenForTransfer as i32,
                RoomMigrationState::FrozenForTransfer as i32,
                RoomMigrationState::ImportingToNew as i32,
            ]
        );
        assert!(snapshot.routes.iter().all(|route| route.member_count == 0));
        assert!(
            snapshot
                .routes
                .iter()
                .all(|route| route.online_member_count == 0)
        );
    }

    #[tokio::test]
    async fn rollout_drain_snapshot_excludes_transferred_rooms_from_blockers_and_counts_retired() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory.clone()),
        );

        for (room_id, status) in [
            ("room-new-owner", RoomTransferStatus::OwnedByNew),
            ("room-retired", RoomTransferStatus::Retired),
        ] {
            let mut room = Room::new(
                room_id.to_string(),
                "owner".to_string(),
                "default_match".to_string(),
                factory.create("default_match"),
            );
            room.transfer_state.rollout_epoch = Some("epoch-1".to_string());
            room.transfer_state.status = status;
            manager.rooms.lock().await.insert(room_id.to_string(), room);
        }

        let snapshot = manager.rollout_drain_snapshot("game-server-old", 50).await;

        assert_eq!(snapshot.owned_room_count, 0);
        assert_eq!(snapshot.migrating_room_count, 0);
        assert_eq!(snapshot.transferable_empty_room_count, 0);
        assert!(snapshot.transferable_empty_room_samples.is_empty());
        assert_eq!(snapshot.retired_room_count, 1);
        assert_eq!(snapshot.routes.len(), 2);
        assert_eq!(
            snapshot
                .routes
                .iter()
                .map(|route| route.migration_state)
                .collect::<Vec<_>>(),
            vec![
                RoomMigrationState::OwnedByNew as i32,
                RoomMigrationState::RetiredOnOld as i32,
            ]
        );
    }

    #[tokio::test]
    async fn trigger_server_redirect_only_pushes_online_members_in_target_room() {
        let (manager, _factory, mut receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        manager
            .disconnect_room_member("room-test", "player-b")
            .await;

        {
            let rooms = manager.rooms.lock().await;
            let room = rooms.get("room-test").unwrap();
            assert_eq!(room.members["player-a"].close_state.reason(), None);
            assert_eq!(room.members["player-b"].close_state.reason(), None);
        }

        let delivery = manager
            .trigger_server_redirect(
                "room-test",
                ServerRedirectPush {
                    reason: "rollout".to_string(),
                    room_id: "room-test".to_string(),
                    rollout_epoch: "epoch-1".to_string(),
                    reconnect_required: true,
                    retry_after_ms: 250,
                    target_host: "127.0.0.1".to_string(),
                    target_port: 4000,
                    target_server_id: "game-server-new".to_string(),
                    transport: "kcp".to_string(),
                },
            )
            .await
            .unwrap();

        assert_eq!(
            delivery,
            ServerRedirectDelivery {
                delivered_count: 1,
                failed_count: 0,
                online_member_count: 1,
            }
        );

        let pushed = drain_messages_of_type(&mut receivers[0], MessageType::ServerRedirectPush)
            .pop()
            .expect("online member push");
        assert_eq!(pushed.message_type, MessageType::ServerRedirectPush);
        let push = ServerRedirectPush::decode(pushed.body.as_slice()).unwrap();
        assert_eq!(push.room_id, "room-test");
        assert_eq!(push.rollout_epoch, "epoch-1");
        assert_eq!(push.target_host, "127.0.0.1");
        assert_eq!(push.target_port, 4000);
        assert!(push.reconnect_required);
        assert!(
            drain_messages_of_type(&mut receivers[1], MessageType::ServerRedirectPush).is_empty()
        );

        {
            let rooms = manager.rooms.lock().await;
            let room = rooms.get("room-test").unwrap();
            assert_eq!(
                room.members["player-a"].close_state.reason().as_deref(),
                Some(SERVER_REDIRECT_CLOSE_REASON)
            );
            assert_eq!(room.members["player-b"].close_state.reason(), None);
        }
    }

    #[tokio::test]
    async fn trigger_server_redirect_queue_failure_does_not_overwrite_close_reason() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        let (full_tx, _full_rx) = mpsc::channel(1);
        full_tx
            .try_send(OutboundMessage {
                message_type: MessageType::RoomStatePush,
                seq: 0,
                body: Vec::new(),
            })
            .unwrap();
        let close_state = ConnectionCloseState::new();
        assert!(close_state.request_close("existing_reason"));

        {
            let mut rooms = manager.rooms.lock().await;
            let room = rooms.get_mut("room-test").unwrap();
            let member = room.members.get_mut("player-a").unwrap();
            member.sender = full_tx;
            member.close_state = close_state;
        }

        let delivery = manager
            .trigger_server_redirect(
                "room-test",
                ServerRedirectPush {
                    reason: "rollout".to_string(),
                    room_id: "room-test".to_string(),
                    rollout_epoch: "epoch-1".to_string(),
                    reconnect_required: true,
                    retry_after_ms: 250,
                    target_host: "127.0.0.1".to_string(),
                    target_port: 4000,
                    target_server_id: "game-server-new".to_string(),
                    transport: "kcp".to_string(),
                },
            )
            .await
            .unwrap();

        assert_eq!(
            delivery,
            ServerRedirectDelivery {
                delivered_count: 1,
                failed_count: 1,
                online_member_count: 2,
            }
        );

        let rooms = manager.rooms.lock().await;
        let room = rooms.get("room-test").unwrap();
        assert_eq!(
            room.members["player-a"].close_state.reason().as_deref(),
            Some("existing_reason")
        );
        assert_eq!(
            room.members["player-b"].close_state.reason().as_deref(),
            Some(SERVER_REDIRECT_CLOSE_REASON)
        );
    }

    #[tokio::test]
    async fn fps_change_pushes_room_frame_rate_update_to_online_members() {
        let (manager, _factory, mut receivers) =
            setup_started_room("disposable_match", &["player-a", "player-b"]).await;
        for receiver in &mut receivers {
            drain_messages_of_type(receiver, MessageType::RoomFrameRatePush);
        }

        manager
            .disconnect_room_member("room-test", "player-b")
            .await;

        let pushes = drain_messages_of_type(&mut receivers[0], MessageType::RoomFrameRatePush);
        assert_eq!(pushes.len(), 1);
        let push = RoomFrameRatePush::decode(pushes[0].body.as_slice()).unwrap();
        assert_eq!(push.room_id, "room-test");
        assert_eq!(push.fps, 15);
        assert_eq!(push.reason, "runtime_policy_changed");
        assert!(
            drain_messages_of_type(&mut receivers[1], MessageType::RoomFrameRatePush).is_empty()
        );
    }

    #[tokio::test]
    async fn join_room_pushes_initial_room_frame_rate_update() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory),
        );

        let (join_tx, mut join_rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-test",
                "player-a",
                join_tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();

        let join_pushes = drain_messages_of_type(&mut join_rx, MessageType::RoomFrameRatePush);
        assert_eq!(join_pushes.len(), 1);
        let push = RoomFrameRatePush::decode(join_pushes[0].body.as_slice()).unwrap();
        assert_eq!(push.room_id, "room-test");
        assert_eq!(push.fps, 2);
        assert_eq!(push.reason, "runtime_policy_changed");
    }

    #[tokio::test]
    async fn unchanged_fps_does_not_push_duplicate_room_frame_rate_update() {
        let (manager, _factory, mut receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        for receiver in &mut receivers {
            drain_messages_of_type(receiver, MessageType::RoomFrameRatePush);
        }

        manager
            .disconnect_room_member("room-test", "player-b")
            .await;

        assert!(
            drain_messages_of_type(&mut receivers[0], MessageType::RoomFrameRatePush).is_empty()
        );
        assert!(
            drain_messages_of_type(&mut receivers[1], MessageType::RoomFrameRatePush).is_empty()
        );
    }

    #[tokio::test]
    async fn export_room_transfer_rejects_logic_without_transfer_contract() {
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(UnsupportedTransferRoomLogicFactory),
        );
        let (tx, _rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-test",
                "player-a",
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();
        manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        manager
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();

        let result = manager.export_room_transfer("epoch-1", "room-test").await;

        assert_eq!(result, Err("UNSUPPORTED_ROOM_TRANSFER"));
        let rooms = manager.rooms.lock().await;
        let room = rooms.get("room-test").expect("room should remain");
        assert_eq!(room.transfer_state.status, RoomTransferStatus::Frozen);
        assert!(room.transfer_state.last_transfer_checksum.is_none());
    }

    #[tokio::test]
    async fn export_room_transfer_rejects_invalid_epoch_or_missing_room() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory),
        );

        assert_eq!(
            manager.export_room_transfer("", "room-test").await,
            Err("INVALID_ROLLOUT_EPOCH")
        );
        assert_eq!(
            manager
                .export_room_transfer("epoch-1", "room-missing")
                .await,
            Err("ROOM_NOT_FOUND")
        );
    }

    #[tokio::test]
    async fn export_room_transfer_rejects_room_that_was_not_frozen() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory),
        );
        let (tx, _rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-test",
                "player-a",
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();

        let result = manager.export_room_transfer("epoch-1", "room-test").await;

        assert_eq!(result, Err("ROOM_TRANSFER_NOT_FROZEN"));
    }

    #[tokio::test]
    async fn export_room_transfer_rejects_mismatched_epoch() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        manager
            .disconnect_room_member("room-test", "player-b")
            .await;
        manager
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();

        let result = manager.export_room_transfer("epoch-2", "room-test").await;

        assert_eq!(result, Err("ROOM_TRANSFER_EPOCH_MISMATCH"));
    }

    #[tokio::test]
    async fn export_room_transfer_checksum_is_deterministic() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        manager
            .disconnect_room_member("room-test", "player-b")
            .await;
        manager
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();

        let payload = manager
            .export_room_transfer("epoch-1", "room-test")
            .await
            .unwrap();

        assert!(!payload.checksum.is_empty());
        assert_eq!(payload.checksum, room_transfer_checksum(&payload));
        let transfer_state = room_transfer_state_from_payload(&payload).unwrap();
        assert_eq!(transfer_state.schema_version, ROOM_TRANSFER_SCHEMA_VERSION);
        assert_eq!(transfer_state.logic_state_json, "recording-state-v1");
        assert_eq!(
            transfer_state.movement_state_json,
            r#"{"movement":"recording-v1"}"#
        );
        assert_eq!(
            transfer_state.combat_state_json,
            r#"{"combat":"recording-v1"}"#
        );
        assert_eq!(transfer_state.npc_state_json, r#"{"npc":"recording-v1"}"#);
        assert_eq!(
            transfer_state.timer_state_json,
            r#"{"timer":"recording-v1"}"#
        );
        assert_eq!(payload.snapshot.as_ref().unwrap().room_id, "room-test");
    }

    #[tokio::test]
    async fn repeated_export_room_transfer_is_idempotent() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        manager
            .disconnect_room_member("room-test", "player-b")
            .await;
        manager
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();

        let first = manager
            .export_room_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        let second = manager
            .export_room_transfer("epoch-1", "room-test")
            .await
            .unwrap();

        assert_eq!(second.room_version, first.room_version);
        assert_eq!(second.checksum, first.checksum);
        assert_eq!(second.checksum, room_transfer_checksum(&second));
    }

    #[tokio::test]
    async fn import_room_transfer_rejects_bad_checksum() {
        let (source, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        source.disconnect_room_member("room-test", "player-a").await;
        source.disconnect_room_member("room-test", "player-b").await;
        source
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        let mut payload = source
            .export_room_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        payload.checksum = "bad-checksum".to_string();

        let target_factory = RecordingRoomLogicFactory::default();
        let target = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(target_factory),
        );

        let result = target.import_room_transfer(payload).await;

        assert_eq!(result, Err("ROOM_TRANSFER_CHECKSUM_MISMATCH"));
        assert!(!target.room_exists("room-test").await);
    }

    #[tokio::test]
    async fn import_room_transfer_rejects_logic_without_transfer_contract() {
        let (source, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        source.disconnect_room_member("room-test", "player-a").await;
        source.disconnect_room_member("room-test", "player-b").await;
        source
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        let payload = source
            .export_room_transfer("epoch-1", "room-test")
            .await
            .unwrap();

        let target = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(UnsupportedTransferRoomLogicFactory),
        );

        let result = target.import_room_transfer(payload).await;

        assert_eq!(result, Err("UNSUPPORTED_ROOM_TRANSFER"));
        assert!(!target.room_exists("room-test").await);
    }

    #[tokio::test]
    async fn import_room_transfer_rejects_unsupported_schema_without_creating_room() {
        let (source, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        source.disconnect_room_member("room-test", "player-a").await;
        source.disconnect_room_member("room-test", "player-b").await;
        source
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        let mut payload = source
            .export_room_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        let mut logic_state =
            serde_json::from_str::<serde_json::Value>(&payload.logic_state_json).unwrap();
        logic_state["schemaVersion"] = serde_json::json!(ROOM_TRANSFER_SCHEMA_VERSION + 1);
        payload.logic_state_json = logic_state.to_string();
        payload.checksum = room_transfer_checksum(&payload);

        let target_factory = RecordingRoomLogicFactory::default();
        let target = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(target_factory),
        );

        let result = target.import_room_transfer(payload).await;

        assert_eq!(result, Err("ROOM_TRANSFER_UNSUPPORTED_SCHEMA"));
        assert!(!target.room_exists("room-test").await);
    }

    #[tokio::test]
    async fn import_room_transfer_restores_basic_room_state() {
        let (source, _source_factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        source.disconnect_room_member("room-test", "player-a").await;
        source.disconnect_room_member("room-test", "player-b").await;
        source
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        let payload = source
            .export_room_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        let checksum = payload.checksum.clone();

        let target_factory = RecordingRoomLogicFactory::default();
        let target = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(target_factory.clone()),
        );

        let imported = target.import_room_transfer(payload).await.unwrap();

        assert_eq!(imported.0, checksum);
        assert!(target.room_exists("room-test").await);
        assert_eq!(
            target_factory.imported_transfer_states(),
            vec![RoomLogicTransferState {
                schema_version: ROOM_TRANSFER_SCHEMA_VERSION,
                logic_state_json: "recording-state-v1".to_string(),
                movement_state_json: r#"{"movement":"recording-v1"}"#.to_string(),
                combat_state_json: r#"{"combat":"recording-v1"}"#.to_string(),
                npc_state_json: r#"{"npc":"recording-v1"}"#.to_string(),
                timer_state_json: r#"{"timer":"recording-v1"}"#.to_string(),
            }]
        );

        let (tx, _rx) = mpsc::channel(1024);
        let snapshot = target
            .reconnect_room("room-test", "player-a", tx)
            .await
            .unwrap()
            .snapshot;
        assert_eq!(snapshot.room_id, "room-test");
    }

    async fn setup_imported_room_for_confirm()
    -> (RoomManager, RecordingRoomLogicFactory, String, u64) {
        let (source, _source_factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        source.disconnect_room_member("room-test", "player-a").await;
        source.disconnect_room_member("room-test", "player-b").await;
        source
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        let payload = source
            .export_room_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        let checksum = payload.checksum.clone();

        let target_factory = RecordingRoomLogicFactory::default();
        let target = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(target_factory.clone()),
        );
        let (_imported_checksum, room_version) =
            target.import_room_transfer(payload).await.unwrap();

        (target, target_factory, checksum, room_version)
    }

    #[tokio::test]
    async fn confirm_room_ownership_succeeds_for_imported_room() {
        let (target, _target_factory, checksum, room_version) =
            setup_imported_room_for_confirm().await;

        let confirmed = target
            .confirm_room_ownership("epoch-1", "room-test", &checksum, room_version)
            .await
            .unwrap();

        assert_eq!(confirmed.0, checksum);
        assert_eq!(confirmed.1, room_version);
    }

    #[tokio::test]
    async fn confirm_room_ownership_rejects_mismatched_epoch_checksum_or_version() {
        let (target, _target_factory, checksum, room_version) =
            setup_imported_room_for_confirm().await;

        assert_eq!(
            target
                .confirm_room_ownership("epoch-2", "room-test", &checksum, room_version)
                .await,
            Err("ROOM_TRANSFER_EPOCH_MISMATCH")
        );
        assert_eq!(
            target
                .confirm_room_ownership("epoch-1", "room-test", "wrong", room_version)
                .await,
            Err("ROOM_TRANSFER_CHECKSUM_MISMATCH")
        );
        assert_eq!(
            target
                .confirm_room_ownership(
                    "epoch-1",
                    "room-test",
                    &checksum,
                    room_version.saturating_add(1)
                )
                .await,
            Err("ROOM_TRANSFER_VERSION_MISMATCH")
        );
        assert_eq!(
            target
                .confirm_room_ownership("", "room-test", &checksum, room_version)
                .await,
            Err("INVALID_ROLLOUT_EPOCH")
        );
    }

    #[tokio::test]
    async fn imported_room_is_treated_as_taken_over_room_for_join_and_reconnect() {
        let (target, target_factory, checksum, room_version) =
            setup_imported_room_for_confirm().await;
        target
            .confirm_room_ownership("epoch-1", "room-test", &checksum, room_version)
            .await
            .unwrap();

        {
            let rooms = target.rooms.lock().await;
            let room = rooms.get("room-test").expect("imported room should exist");
            assert_eq!(room.transfer_state.status, RoomTransferStatus::OwnedByNew);
            assert_eq!(
                room.transfer_state.rollout_epoch.as_deref(),
                Some("epoch-1")
            );
            assert_eq!(room.transfer_state.room_version, room_version);
            assert_eq!(
                room.transfer_state.last_transfer_checksum.as_deref(),
                Some(checksum.as_str())
            );
            assert!(room.members.contains_key("player-a"));
            assert!(room.members.contains_key("player-b"));
        }

        let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
        let reconnect = target
            .reconnect_room("room-test", "player-a", reconnect_tx)
            .await
            .unwrap();
        assert_eq!(reconnect.snapshot.room_id, "room-test");

        let (join_tx, _join_rx) = mpsc::channel(1024);
        let join_snapshot = target
            .join_room(
                "room-test",
                "player-b",
                join_tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();
        assert_eq!(join_snapshot.room_id, "room-test");
        assert!(
            join_snapshot
                .members
                .iter()
                .any(|member| member.player_id == "player-b")
        );

        let rooms = target.rooms.lock().await;
        let room = rooms.get("room-test").expect("room should remain");
        assert_eq!(rooms.len(), 1);
        assert_eq!(room.transfer_state.status, RoomTransferStatus::OwnedByNew);
        assert_eq!(room.transfer_state.room_version, room_version);
        assert_eq!(target_factory.imported_transfer_states().len(), 1);
    }

    #[tokio::test]
    async fn confirm_room_ownership_rejects_room_not_owned_by_new() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;

        let result = manager
            .confirm_room_ownership("epoch-1", "room-test", "checksum", 1)
            .await;

        assert_eq!(result, Err("ROOM_TRANSFER_NOT_OWNED_BY_NEW"));
    }

    #[tokio::test]
    async fn retire_transfer_rejects_checksum_mismatch() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        manager
            .disconnect_room_member("room-test", "player-b")
            .await;
        manager
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        manager
            .export_room_transfer("epoch-1", "room-test")
            .await
            .unwrap();

        let result = manager
            .retire_transferred_room("epoch-1", "room-test", "wrong")
            .await;

        assert_eq!(result, Err("ROOM_TRANSFER_CHECKSUM_MISMATCH"));
        assert!(manager.room_exists("room-test").await);
    }

    #[tokio::test]
    async fn retired_room_rejects_later_mutations() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;
        manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        manager
            .disconnect_room_member("room-test", "player-b")
            .await;
        manager
            .freeze_room_for_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        let payload = manager
            .export_room_transfer("epoch-1", "room-test")
            .await
            .unwrap();
        manager
            .retire_transferred_room("epoch-1", "room-test", &payload.checksum)
            .await
            .unwrap();

        let (tx, _rx) = mpsc::channel(1024);
        let join_result = manager
            .join_room(
                "room-test",
                "player-b",
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await;
        assert_eq!(join_result.unwrap_err(), "ROOM_TRANSFER_RETIRED");

        let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
        assert_eq!(
            manager
                .reconnect_room("room-test", "player-a", reconnect_tx)
                .await
                .unwrap_err(),
            "ROOM_TRANSFER_RETIRED"
        );

        assert_eq!(
            manager
                .accept_player_input("room-test", "player-a", 1, "move", "{}")
                .await,
            Err("ROOM_TRANSFER_RETIRED")
        );
    }

    #[tokio::test]
    async fn strict_wait_strategy_blocks_until_all_inputs_arrive() {
        let (manager, factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;

        manager
            .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
            .await
            .unwrap();

        let progressed = manager.process_room_tick("room-test", 10).await;
        assert!(progressed.is_none());
        assert!(factory.recorded_ticks().is_empty());

        manager
            .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":2}")
            .await
            .unwrap();

        let progressed = manager.process_room_tick("room-test", 10).await;
        assert!(progressed.is_some());
        let recorded = factory.recorded_ticks();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, 1);
        assert_eq!(recorded[0].1.len(), 2);
    }

    #[tokio::test]
    async fn optimistic_strategy_advances_with_partial_inputs() {
        let (manager, factory, _receivers) =
            setup_started_room("movement_demo", &["player-a", "player-b"]).await;

        manager
            .accept_player_input(
                "room-test",
                "player-a",
                1,
                "move_dir",
                "{\"dirX\":1,\"dirY\":0}",
            )
            .await
            .unwrap();

        let progressed = manager.process_room_tick("room-test", 20).await;
        assert!(progressed.is_some());

        let recorded = factory.recorded_ticks();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, 1);
        assert_eq!(recorded[0].1.len(), 2);
        assert!(
            recorded[0]
                .1
                .iter()
                .any(|input| input.player_id == "player-b" && input.action.is_empty())
        );
    }

    #[tokio::test]
    async fn future_inputs_are_buffered_until_their_frame_is_ready() {
        let (manager, factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;

        manager
            .accept_player_input("room-test", "player-a", 2, "move", "{\"x\":20}")
            .await
            .unwrap();
        manager
            .accept_player_input("room-test", "player-b", 2, "move", "{\"x\":21}")
            .await
            .unwrap();
        manager
            .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":10}")
            .await
            .unwrap();
        manager
            .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":11}")
            .await
            .unwrap();

        let first = manager.process_room_tick("room-test", 10).await.unwrap();
        assert_eq!(first.0.frame_id, 1);

        let second = manager.process_room_tick("room-test", 10).await.unwrap();
        assert_eq!(second.0.frame_id, 2);

        let recorded = factory.recorded_ticks();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].0, 1);
        assert_eq!(recorded[1].0, 2);
    }

    #[tokio::test]
    async fn expired_input_frame_is_rejected() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;

        manager
            .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
            .await
            .unwrap();
        manager
            .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":2}")
            .await
            .unwrap();
        let _ = manager.process_room_tick("room-test", 10).await;

        let result = manager
            .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":3}")
            .await;
        assert_eq!(result, Err("INPUT_FRAME_EXPIRED"));
    }

    #[tokio::test]
    async fn input_too_far_is_rejected() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;

        let result = manager
            .accept_player_input("room-test", "player-a", 5, "move", "{\"x\":1}")
            .await;
        assert_eq!(result, Err("INPUT_FRAME_TOO_FAR"));
    }

    #[tokio::test]
    async fn rejected_input_does_not_trigger_player_input_hook() {
        let (manager, factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;

        let too_far = manager
            .accept_player_input("room-test", "player-a", 5, "move", "{\"x\":1}")
            .await;
        assert_eq!(too_far, Err("INPUT_FRAME_TOO_FAR"));
        assert!(factory.recorded_inputs().is_empty());

        manager
            .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
            .await
            .unwrap();
        manager
            .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":2}")
            .await
            .unwrap();
        let _ = manager.process_room_tick("room-test", 10).await;

        let expired = manager
            .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":3}")
            .await;
        assert_eq!(expired, Err("INPUT_FRAME_EXPIRED"));

        let recorded = factory.recorded_inputs();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].0, "player-a");
        assert_eq!(recorded[1].0, "player-b");
    }

    #[tokio::test]
    async fn same_frame_input_replaces_previous_one() {
        let (manager, factory, _receivers) =
            setup_started_room("movement_demo", &["player-a"]).await;

        manager
            .accept_player_input(
                "room-test",
                "player-a",
                1,
                "move_dir",
                "{\"dirX\":1,\"dirY\":0}",
            )
            .await
            .unwrap();
        manager
            .accept_player_input(
                "room-test",
                "player-a",
                1,
                "face_to",
                "{\"dirX\":0,\"dirY\":1}",
            )
            .await
            .unwrap();

        let _ = manager.process_room_tick("room-test", 20).await;
        let recorded = factory.recorded_ticks();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].1.len(), 1);
        assert_eq!(recorded[0].1[0].action, "face_to");
    }

    #[tokio::test]
    async fn reconnect_and_observer_receive_waiting_inputs_with_frame_ids() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory),
        );

        let (owner_tx, _owner_rx) = mpsc::channel(1024);
        let (other_tx, _other_rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-test",
                "player-a",
                owner_tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();
        manager
            .join_room(
                "room-test",
                "player-b",
                other_tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();
        manager
            .set_ready_state("room-test", "player-a", true)
            .await
            .unwrap();
        manager
            .set_ready_state("room-test", "player-b", true)
            .await
            .unwrap();
        manager.start_game("room-test", "player-a").await.unwrap();
        {
            let mut runtimes = manager.runtimes.lock().await;
            if let Some(runtime) = runtimes.get_mut("room-test") {
                if let Some(handle) = runtime.tick_handle.take() {
                    handle.abort();
                }
                runtime.tick_running = false;
            }
        }

        manager
            .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
            .await
            .unwrap();

        {
            let mut rooms = manager.rooms.lock().await;
            let room = rooms.get_mut("room-test").unwrap();
            let member = room.members.get_mut("player-a").unwrap();
            member.offline = true;
            member.offline_since = Some(Instant::now());
        }

        let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
        let recovery = manager
            .reconnect_room("room-test", "player-a", reconnect_tx)
            .await
            .unwrap();
        assert_eq!(recovery.waiting_frame_id, 1);
        assert_eq!(recovery.input_delay_frames, 2);
        assert_eq!(recovery.waiting_inputs.len(), 1);
        assert_eq!(recovery.waiting_inputs[0].frame_id, 1);

        let (observer_tx, _observer_rx) = mpsc::channel(1024);
        let observer = manager
            .join_room_as_observer("room-test", "observer-1", observer_tx)
            .await
            .unwrap();
        assert_eq!(observer.waiting_frame_id, 1);
        assert_eq!(observer.waiting_inputs.len(), 1);
        assert_eq!(observer.waiting_inputs[0].frame_id, 1);
    }

    #[tokio::test]
    async fn existing_room_runtime_paths_continue_for_drain_mode_contract() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory.clone()),
        );

        for player_id in ["player-a", "player-b"] {
            let (tx, _rx) = mpsc::channel(1024);
            manager
                .join_room(
                    "room-test",
                    player_id,
                    tx,
                    MemberRole::Player,
                    Some("default_match"),
                )
                .await
                .unwrap();
        }

        manager
            .set_ready_state("room-test", "player-a", false)
            .await
            .unwrap();
        manager
            .set_ready_state("room-test", "player-a", true)
            .await
            .unwrap();
        manager
            .set_ready_state("room-test", "player-b", true)
            .await
            .unwrap();
        manager.start_game("room-test", "player-a").await.unwrap();
        {
            let mut runtimes = manager.runtimes.lock().await;
            if let Some(runtime) = runtimes.get_mut("room-test") {
                if let Some(handle) = runtime.tick_handle.take() {
                    handle.abort();
                }
                runtime.tick_running = false;
            }
        }

        manager
            .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
            .await
            .unwrap();
        manager
            .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":2}")
            .await
            .unwrap();
        let progressed = manager.process_room_tick("room-test", 10).await;
        assert!(progressed.is_some());
        assert_eq!(factory.recorded_ticks().len(), 1);

        manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
        let recovery = manager
            .reconnect_room("room-test", "player-a", reconnect_tx)
            .await
            .unwrap();
        assert_eq!(recovery.snapshot.state, "in_game");

        manager.cleanup_expired_offline_players().await;
        assert!(manager.room_exists("room-test").await);

        let (waiting_tx, _waiting_rx) = mpsc::channel(1024);
        manager
            .join_room(
                "room-observer",
                "player-host",
                waiting_tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();
        let (observer_tx, _observer_rx) = mpsc::channel(1024);
        let observer = manager
            .join_room_as_observer("room-observer", "observer-1", observer_tx)
            .await
            .unwrap();
        assert_eq!(observer.snapshot.room_id, "room-observer");
    }

    #[tokio::test]
    async fn strict_wait_timeout_repeats_last_input() {
        let (manager, factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;

        manager
            .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
            .await
            .unwrap();
        manager
            .accept_player_input("room-test", "player-b", 1, "move", "{\"x\":2}")
            .await
            .unwrap();
        let _ = manager.process_room_tick("room-test", 10).await;

        manager
            .accept_player_input("room-test", "player-a", 2, "move", "{\"x\":3}")
            .await
            .unwrap();
        {
            let mut rooms = manager.rooms.lock().await;
            let room = rooms.get_mut("room-test").unwrap();
            room.wait_started_at = Some(Instant::now() - Duration::from_millis(500));
        }

        let _ = manager.process_room_tick("room-test", 10).await;
        let recorded = factory.recorded_ticks();
        assert_eq!(recorded.len(), 2);
        let second_tick = &recorded[1];
        let repeated = second_tick
            .1
            .iter()
            .find(|input| input.player_id == "player-b")
            .unwrap();
        assert_eq!(repeated.frame_id, 2);
        assert_eq!(repeated.action, "move");
        assert_eq!(repeated.payload_json, "{\"x\":2}");
    }

    #[tokio::test]
    async fn disconnect_path_preserves_in_game_waiting_state_for_reconnect() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;

        manager
            .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
            .await
            .unwrap();

        let disconnected = manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        let snapshot = disconnected.snapshot.expect("disconnect snapshot");
        assert_eq!(snapshot.state, "in_game");
        assert_eq!(snapshot.current_frame_id, 0);

        let (reconnect_tx, _reconnect_rx) = mpsc::channel(1024);
        let recovery = manager
            .reconnect_room("room-test", "player-a", reconnect_tx)
            .await
            .unwrap();

        assert_eq!(recovery.waiting_frame_id, 1);
        assert_eq!(recovery.waiting_inputs.len(), 1);
        assert_eq!(recovery.waiting_inputs[0].frame_id, 1);
        assert_eq!(recovery.snapshot.state, "in_game");
    }

    #[tokio::test]
    async fn disconnect_path_releases_previous_outbound_sender() {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory),
        );
        let (tx, mut rx) = mpsc::channel(1024);

        manager
            .join_room(
                "room-test",
                "player-a",
                tx,
                MemberRole::Player,
                Some("default_match"),
            )
            .await
            .unwrap();
        while rx.try_recv().is_ok() {}

        manager
            .disconnect_room_member("room-test", "player-a")
            .await;

        let closed = tokio::time::timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("previous outbound receiver should close after disconnect");
        assert!(closed.is_none());
    }

    #[tokio::test]
    async fn all_players_disconnected_can_reconnect_before_offline_ttl_expires() {
        let (manager, _factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;

        manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        let disconnected = manager
            .disconnect_room_member("room-test", "player-b")
            .await;
        assert_eq!(
            disconnected.snapshot.expect("disconnect snapshot").state,
            "in_game"
        );

        manager.cleanup_expired_offline_players().await;

        let (reconnect_a_tx, _reconnect_a_rx) = mpsc::channel(1024);
        let reconnect_a = manager
            .reconnect_room("room-test", "player-a", reconnect_a_tx)
            .await
            .unwrap();
        assert_eq!(reconnect_a.snapshot.state, "in_game");

        let (reconnect_b_tx, _reconnect_b_rx) = mpsc::channel(1024);
        let reconnect_b = manager
            .reconnect_room("room-test", "player-b", reconnect_b_tx)
            .await
            .unwrap();
        assert_eq!(reconnect_b.snapshot.state, "in_game");
    }

    #[tokio::test]
    async fn room_tick_pauses_when_all_players_are_offline() {
        let (manager, factory, _receivers) =
            setup_started_room("movement_demo", &["player-a"]).await;

        let disconnected = manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        assert_eq!(
            disconnected.snapshot.expect("disconnect snapshot").state,
            "in_game"
        );

        let progressed = manager.process_room_tick("room-test", 10).await;
        assert!(progressed.is_none());
        assert!(factory.recorded_ticks().is_empty());

        let rooms = manager.rooms.lock().await;
        let room = rooms.get("room-test").expect("room should exist");
        assert_eq!(room.current_frame, 0);
    }

    #[tokio::test]
    async fn reconnect_after_global_disconnect_restarts_wait_timeout_window() {
        let (manager, factory, _receivers) =
            setup_started_room("default_match", &["player-a", "player-b"]).await;

        manager
            .accept_player_input("room-test", "player-a", 1, "move", "{\"x\":1}")
            .await
            .unwrap();
        let progressed = manager.process_room_tick("room-test", 10).await;
        assert!(progressed.is_none());

        {
            let mut rooms = manager.rooms.lock().await;
            let room = rooms.get_mut("room-test").unwrap();
            room.wait_started_at = Some(Instant::now() - Duration::from_millis(500));
        }

        manager
            .disconnect_room_member("room-test", "player-a")
            .await;
        manager
            .disconnect_room_member("room-test", "player-b")
            .await;

        let offline_tick = manager.process_room_tick("room-test", 10).await;
        assert!(offline_tick.is_none());

        let (reconnect_a_tx, _reconnect_a_rx) = mpsc::channel(1024);
        manager
            .reconnect_room("room-test", "player-a", reconnect_a_tx)
            .await
            .unwrap();
        let (reconnect_b_tx, _reconnect_b_rx) = mpsc::channel(1024);
        manager
            .reconnect_room("room-test", "player-b", reconnect_b_tx)
            .await
            .unwrap();

        let progressed_after_reconnect = manager.process_room_tick("room-test", 10).await;
        assert!(progressed_after_reconnect.is_none());
        assert!(factory.recorded_ticks().is_empty());
    }

    #[tokio::test]
    async fn drop_after_misses_marks_player_offline_after_threshold() {
        let (sender, _receiver) = mpsc::channel(1024);
        let ticks = Arc::new(StdMutex::new(Vec::new()));
        let inputs = Arc::new(StdMutex::new(Vec::new()));
        let mut room = Room::new(
            "room-test".to_string(),
            "player-a".to_string(),
            "default_match".to_string(),
            Box::new(RecordingRoomLogic {
                ticks,
                inputs,
                imported_transfer_states: Arc::new(StdMutex::new(Vec::new())),
                state: "recording-state-v1".to_string(),
            }),
        );
        room.members.insert(
            "player-a".to_string(),
            RoomMemberState {
                player_id: "player-a".to_string(),
                ready: true,
                sender,
                close_state: ConnectionCloseState::new(),
                offline: false,
                offline_since: None,
                role: MemberRole::Player,
                syncing: false,
            },
        );

        let participants = vec!["player-a".to_string()];
        let policy = RoomRuntimePolicy {
            missing_input_strategy: MissingInputStrategy::DropAfterMisses,
            ..RoomRuntimePolicy::default_match()
        };

        for frame_id in 1..=MAX_MISSING_INPUT_STREAK_BEFORE_OFFLINE {
            let resolved = resolve_tick_inputs(&mut room, &participants, frame_id, &policy);
            assert_eq!(resolved.len(), 1);
            assert_eq!(resolved[0].frame_id, frame_id);
        }

        let member = room.members.get("player-a").expect("player should exist");
        assert!(member.offline);
        assert_eq!(
            room.missing_input_streaks.get("player-a").copied(),
            Some(MAX_MISSING_INPUT_STREAK_BEFORE_OFFLINE)
        );
    }
}
