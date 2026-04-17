use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant as TokioInstant, sleep_until};
use tracing::info;

use crate::core::logic::{RoomLogicBroadcast, SharedRoomLogicFactory};
use crate::core::room::{
    MemberRole, OutboundMessage, PendingInputUpsert, PlayerInputRecord, Room, RoomMemberState,
    RoomPhase,
};
use crate::core::runtime::room_policy::{
    InputWaitStrategy, MissingInputStrategy, RoomPolicyRegistry, RoomRuntimePolicy,
};
use crate::match_client::SharedMatchClient;
use crate::metrics::METRICS;
use crate::pb::{
    FrameBundlePush, FrameInput, MovementCorrectionReason, MovementRecoveryState as PbMovementRecoveryState,
    RoomSnapshot, RoomStatePush,
};
use crate::protocol::{MessageType, encode_body};

const MAX_MISSING_INPUT_STREAK_BEFORE_OFFLINE: u32 = 3;
const DEFAULT_ROOM_CLEANUP_INTERVAL_SECS: u64 = 10;

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

#[derive(Clone)]
pub struct RoomManager {
    rooms: std::sync::Arc<Mutex<HashMap<String, Room>>>,
    runtimes: std::sync::Arc<Mutex<HashMap<String, RoomRuntime>>>,
    policies: RoomPolicyRegistry,
    logic_factory: SharedRoomLogicFactory,
    match_client: SharedMatchClient,
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
        let this = Self {
            rooms: std::sync::Arc::new(Mutex::new(HashMap::new())),
            runtimes: std::sync::Arc::new(Mutex::new(HashMap::new())),
            policies: RoomPolicyRegistry::default(),
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

                        let policy = policies.resolve(&room.policy_id);
                        let expired_players = room.collect_expired_offline_players(policy.offline_ttl_secs);
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
                        if !policy.destroy_enabled || !policy.destroy_when_empty || !should_cleanup_as_empty {
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
                        if let Err(error) = client.match_end(&match_id, &room_id, "offline_ttl_expired").await {
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

    pub async fn create_matched_room(
        &self,
        match_id: &str,
        room_id: &str,
        player_ids: &[String],
        mode: &str,
    ) -> Result<RoomSnapshot, &'static str> {
        let mut rooms = self.rooms.lock().await;
        let mut runtimes = self.runtimes.lock().await;
        let default_policy = self.policies.default_policy().clone();
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

            let _ = room;
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

        self.notify_room_created(match_id, room_id, player_ids, mode).await;

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
            match client.create_room_and_join(match_id, room_id, player_ids, mode).await {
                Ok(()) => {
                    info!(match_id = match_id, room_id = room_id, "Notified MatchService: room created");
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
                    info!(match_id = match_id, player_id = player_id, room_id = room_id, "Notified MatchService: player joined");
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
                    info!(match_id = match_id, player_id = player_id, reason = reason, should_abort = should_abort, "Notified MatchService: player left");
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
                    info!(match_id = match_id, room_id = room_id, reason = reason, "Notified MatchService: match ended");
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
        sender: mpsc::UnboundedSender<OutboundMessage>,
        role: MemberRole,
        requested_policy_id: Option<&str>,
    ) -> Result<RoomSnapshot, &'static str> {
        let mut rooms = self.rooms.lock().await;
        let mut runtimes = self.runtimes.lock().await;
        let requested_policy_id = requested_policy_id
            .filter(|value| !value.is_empty())
            .unwrap_or(self.policies.default_policy().policy_id.as_str());
        let selected_policy = self.policies.resolve(requested_policy_id);
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

            let policy = self.policies.resolve(&room.policy_id);
            if room.phase == RoomPhase::InGame && !room.members.contains_key(player_id) {
                return Err("ROOM_ALREADY_IN_GAME");
            }

            if room.members.len() >= policy.max_members && !room.members.contains_key(player_id) {
                return Err("ROOM_FULL");
            }

            let is_new_member = !room.members.contains_key(player_id);
            room.members.insert(
                player_id.to_string(),
                RoomMemberState {
                    player_id: player_id.to_string(),
                    ready: false,
                    sender,
                    offline: false,
                    offline_since: None,
                    role,
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

        Ok(snapshot)
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

        if let Some(member) = room.members.get_mut(player_id) {
            member.offline = true;
            member.offline_since = Some(Instant::now());
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
            if let Some(next_owner) = room.members.values().find(|m| !m.offline).map(|m| m.player_id.clone()) {
                room.owner_player_id = next_owner;
            }
        }

        if !room.has_online_members() {
            room.mark_empty();
        }

        let _ = policy;
        room.reset_to_waiting();

        let snapshot = room.snapshot();
        let match_id = room.match_id.clone();
        drop(rooms);
        drop(runtimes);

        if let Some(ref mid) = match_id {
            let should_abort = self.notify_player_left(mid, player_id, "normal").await;
            if should_abort {
                info!(room_id = room_id, match_id = mid, "MatchService requested abort due to player leaving");
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

        if let Some(member) = room.members.get_mut(player_id) {
            member.offline = true;
            member.offline_since = Some(Instant::now());
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
            if let Some(next_owner) = room.members.values().find(|m| !m.offline).map(|m| m.player_id.clone()) {
                room.owner_player_id = next_owner;
            }
        }

        if !room.has_online_members() {
            room.mark_empty();
            room.wait_started_at = None;
        }

        let snapshot = room.snapshot();
        let match_id = room.match_id.clone();
        drop(rooms);

        if let Some(ref mid) = match_id {
            let should_abort = self.notify_player_left(mid, player_id, "disconnect").await;
            if should_abort {
                info!(room_id = room_id, match_id = mid, "MatchService requested abort due to player disconnect");
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
        sender: mpsc::UnboundedSender<OutboundMessage>,
    ) -> Result<RoomRecoveryState, &'static str> {
        let mut rooms = self.rooms.lock().await;
        let room = rooms.get_mut(room_id).ok_or("ROOM_NOT_FOUND")?;

        if let Some(member) = room.members.get_mut(player_id) {
            if !member.offline {
                return Err("PLAYER_ALREADY_ONLINE");
            }

            member.offline = false;
            member.offline_since = None;
            member.sender = sender;
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
        sender: mpsc::UnboundedSender<OutboundMessage>,
    ) -> Result<RoomRecoveryState, &'static str> {
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
                    sender,
                    offline: false,
                    offline_since: None,
                    role: MemberRole::Observer,
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
            let movement_recovery = room.logic.movement_recovery_state(
                None,
                MovementCorrectionReason::ObserverRecovery,
            );

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

        info!(
            room_id = room_id,
            player_id = player_id,
            current_frame_id = recovery.current_frame_id,
            "observer joined"
        );

        Ok(recovery)
    }

    pub async fn cleanup_expired_offline_players(&self) {
        let mut rooms = self.rooms.lock().await;

        for (room_id, room) in rooms.iter_mut() {
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

        room.can_send_input(player_id)?;
        room.update_activity();
        room.logic.on_player_input(player_id, action, payload_json);
        if frame_id <= room.current_frame {
            return Err("INPUT_FRAME_EXPIRED");
        }

        let max_future_frame = room.current_frame.saturating_add(policy.input_delay_frames.max(1));
        if frame_id > max_future_frame {
            return Err("INPUT_FRAME_TOO_FAR");
        }

        let input_record = PlayerInputRecord {
            frame_id,
            player_id: player_id.to_string(),
            action: action.to_string(),
            payload_json: payload_json.to_string(),
            received_at: Instant::now(),
        };
        let outcome = room.upsert_pending_input(input_record);
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

    async fn update_room_fps(&self, room_id: &str) {
        let target_fps = {
            let rooms = self.rooms.lock().await;
            let Some(room) = rooms.get(room_id) else {
                return;
            };
            self.compute_room_fps(room)
        };

        let mut runtimes = self.runtimes.lock().await;
        if let Some(runtime) = runtimes.get_mut(room_id) {
            let previous_fps = runtime.current_fps;
            runtime.current_fps = target_fps;
            if previous_fps != target_fps {
                info!(
                    room_id = room_id,
                    previous_fps = previous_fps,
                    current_fps = target_fps,
                    "room fps updated"
                );
            }
        }
    }

    fn compute_room_fps(&self, room: &Room) -> u16 {
        let policy = self.policies.resolve(&room.policy_id);
        let online_count = room.online_members().len();

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
            .filter(|input| participants.iter().any(|player_id| player_id == &input.player_id))
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

        let tick_inputs =
            resolve_tick_inputs(room, &participants, waiting_frame_id, &policy);
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
                info!(
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

            for RoomLogicBroadcast {
                message_type,
                body,
                target_player_ids,
            } in pending_broadcasts
            {
                let _ = self
                    .broadcast_message(&room_id, &target_player_ids, message_type, body)
                    .await;
            }

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

            let online = room.online_members();
            info!(
                room_id = room_id,
                message_type = ?message_type,
                online_count = online.len(),
                "broadcast_to_room"
            );

            online
                .iter()
                .map(|member| member.sender.clone())
                .collect::<Vec<_>>()
        };

        for sender in senders {
            let _ = sender.send(OutboundMessage {
                message_type,
                seq: 0,
                body: body.clone(),
            });
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
                .filter(|member| !member.offline)
                .map(|member| member.sender.clone())
                .collect::<Vec<_>>();

            info!(
                room_id = room_id,
                message_type = ?message_type,
                target_count = targets.len(),
                "broadcast_to_players"
            );

            targets
        };

        for sender in senders {
            let _ = sender.send(OutboundMessage {
                message_type,
                seq: 0,
                body: body.clone(),
            });
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

    pub async fn send_to_player(
        &self,
        player_id: &str,
        message_type: MessageType,
        body: Vec<u8>,
    ) -> Result<(), std::io::Error> {
        let sender = {
            let rooms = self.rooms.lock().await;
            rooms
                .values()
                .find_map(|room| {
                    room.members.get(player_id).and_then(|member| {
                        if member.offline {
                            None
                        } else {
                            Some(member.sender.clone())
                        }
                    })
                })
        };

        if let Some(sender) = sender {
            let _ = sender.send(OutboundMessage {
                message_type,
                seq: 0,
                body,
            });
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

fn synthetic_empty_input(frame_id: u32, player_id: &str) -> PlayerInputRecord {
    PlayerInputRecord {
        frame_id,
        player_id: player_id.to_string(),
        action: String::new(),
        payload_json: String::new(),
        received_at: Instant::now(),
    }
}

fn clone_input_for_frame(frame_id: u32, input: &PlayerInputRecord) -> PlayerInputRecord {
    PlayerInputRecord {
        frame_id,
        player_id: input.player_id.clone(),
        action: input.action.clone(),
        payload_json: input.payload_json.clone(),
        received_at: Instant::now(),
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

    use crate::core::logic::{RoomLogic, RoomLogicFactory};
    use crate::core::room::PlayerInputRecord;

    use super::*;

    #[derive(Clone, Default)]
    struct RecordingRoomLogicFactory {
        ticks: Arc<StdMutex<Vec<(u32, Vec<PlayerInputRecord>)>>>,
    }

    impl RecordingRoomLogicFactory {
        fn recorded_ticks(&self) -> Vec<(u32, Vec<PlayerInputRecord>)> {
            self.ticks.lock().unwrap().clone()
        }
    }

    struct RecordingRoomLogic {
        ticks: Arc<StdMutex<Vec<(u32, Vec<PlayerInputRecord>)>>>,
    }

    impl RoomLogic for RecordingRoomLogic {
        fn on_tick(&mut self, frame_id: u32, _fps: u16, inputs: &[PlayerInputRecord]) {
            self.ticks
                .lock()
                .unwrap()
                .push((frame_id, inputs.to_vec()));
        }
    }

    impl RoomLogicFactory for RecordingRoomLogicFactory {
        fn create(&self, _policy_id: &str) -> Box<dyn RoomLogic> {
            Box::new(RecordingRoomLogic {
                ticks: Arc::clone(&self.ticks),
            })
        }
    }

    async fn setup_started_room(
        policy_id: &str,
        players: &[&str],
    ) -> (RoomManager, RecordingRoomLogicFactory, Vec<mpsc::UnboundedReceiver<OutboundMessage>>) {
        let factory = RecordingRoomLogicFactory::default();
        let manager = RoomManager::with_match_client(
            crate::match_client::create_match_client_shared(),
            Arc::new(factory.clone()),
        );

        let mut receivers = Vec::new();
        for player_id in players {
            let (tx, rx) = mpsc::unbounded_channel();
            receivers.push(rx);
            manager
                .join_room("room-test", player_id, tx, MemberRole::Player, Some(policy_id))
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
            .accept_player_input("room-test", "player-a", 1, "move_dir", "{\"dirX\":1,\"dirY\":0}")
            .await
            .unwrap();

        let progressed = manager.process_room_tick("room-test", 20).await;
        assert!(progressed.is_some());

        let recorded = factory.recorded_ticks();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, 1);
        assert_eq!(recorded[0].1.len(), 2);
        assert!(recorded[0].1.iter().any(|input| input.player_id == "player-b" && input.action.is_empty()));
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
    async fn same_frame_input_replaces_previous_one() {
        let (manager, factory, _receivers) =
            setup_started_room("movement_demo", &["player-a"]).await;

        manager
            .accept_player_input("room-test", "player-a", 1, "move_dir", "{\"dirX\":1,\"dirY\":0}")
            .await
            .unwrap();
        manager
            .accept_player_input("room-test", "player-a", 1, "face_to", "{\"dirX\":0,\"dirY\":1}")
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

        let (owner_tx, _owner_rx) = mpsc::unbounded_channel();
        let (other_tx, _other_rx) = mpsc::unbounded_channel();
        manager
            .join_room("room-test", "player-a", owner_tx, MemberRole::Player, Some("default_match"))
            .await
            .unwrap();
        manager
            .join_room("room-test", "player-b", other_tx, MemberRole::Player, Some("default_match"))
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

        let (reconnect_tx, _reconnect_rx) = mpsc::unbounded_channel();
        let recovery = manager
            .reconnect_room("room-test", "player-a", reconnect_tx)
            .await
            .unwrap();
        assert_eq!(recovery.waiting_frame_id, 1);
        assert_eq!(recovery.input_delay_frames, 2);
        assert_eq!(recovery.waiting_inputs.len(), 1);
        assert_eq!(recovery.waiting_inputs[0].frame_id, 1);

        let (observer_tx, _observer_rx) = mpsc::unbounded_channel();
        let observer = manager
            .join_room_as_observer("room-test", "observer-1", observer_tx)
            .await
            .unwrap();
        assert_eq!(observer.waiting_frame_id, 1);
        assert_eq!(observer.waiting_inputs.len(), 1);
        assert_eq!(observer.waiting_inputs[0].frame_id, 1);
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

        let (reconnect_tx, _reconnect_rx) = mpsc::unbounded_channel();
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

        let (reconnect_a_tx, _reconnect_a_rx) = mpsc::unbounded_channel();
        let reconnect_a = manager
            .reconnect_room("room-test", "player-a", reconnect_a_tx)
            .await
            .unwrap();
        assert_eq!(reconnect_a.snapshot.state, "in_game");

        let (reconnect_b_tx, _reconnect_b_rx) = mpsc::unbounded_channel();
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

        let (reconnect_a_tx, _reconnect_a_rx) = mpsc::unbounded_channel();
        manager
            .reconnect_room("room-test", "player-a", reconnect_a_tx)
            .await
            .unwrap();
        let (reconnect_b_tx, _reconnect_b_rx) = mpsc::unbounded_channel();
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
        let (sender, _receiver) = mpsc::unbounded_channel();
        let ticks = Arc::new(StdMutex::new(Vec::new()));
        let mut room = Room::new(
            "room-test".to_string(),
            "player-a".to_string(),
            "default_match".to_string(),
            Box::new(RecordingRoomLogic { ticks }),
        );
        room.members.insert(
            "player-a".to_string(),
            RoomMemberState {
                player_id: "player-a".to_string(),
                ready: true,
                sender,
                offline: false,
                offline_since: None,
                role: MemberRole::Player,
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
