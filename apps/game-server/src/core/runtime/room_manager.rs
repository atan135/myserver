use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant as TokioInstant, sleep_until};
use tracing::info;

use crate::core::logic::{RoomLogicBroadcast, SharedRoomLogicFactory};
use crate::core::room::{MemberRole, OutboundMessage, PlayerInputRecord, Room, RoomMemberState, RoomPhase};
use crate::core::runtime::room_policy::RoomPolicyRegistry;
use crate::match_client::SharedMatchClient;
use crate::metrics::METRICS;
use crate::pb::{FrameBundlePush, FrameInput, RoomSnapshot, RoomStatePush};
use crate::protocol::{MessageType, encode_body};

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
        Self::with_match_client(
            crate::match_client::create_match_client_shared(),
            logic_factory,
        )
    }

    pub fn with_match_client(
        match_client: SharedMatchClient,
        logic_factory: SharedRoomLogicFactory,
    ) -> Self {
        let this = Self {
            rooms: std::sync::Arc::new(Mutex::new(HashMap::new())),
            runtimes: std::sync::Arc::new(Mutex::new(HashMap::new())),
            policies: RoomPolicyRegistry::default(),
            logic_factory,
            match_client,
        };
        this.spawn_cleanup_task();
        this
    }

    fn spawn_cleanup_task(&self) {
        let rooms = std::sync::Arc::clone(&self.rooms);
        let runtimes = std::sync::Arc::clone(&self.runtimes);
        let policies = self.policies.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop {
                interval.tick().await;

                let mut to_destroy = Vec::new();

                {
                    let mut rooms_guard = rooms.lock().await;

                    for (room_id, room) in rooms_guard.iter_mut() {
                        if room.marked_for_destruction {
                            continue;
                        }

                        let policy = policies.resolve(&room.policy_id);
                        if !policy.destroy_enabled || !policy.destroy_when_empty || room.has_online_members() {
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
    ) -> Result<(RoomSnapshot, u32, Vec<FrameInput>), &'static str> {
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
            let recent_inputs = room
                .get_inputs_in_range(current_frame_id.saturating_sub(300), current_frame_id)
                .iter()
                .map(|input| FrameInput {
                    player_id: input.player_id.clone(),
                    action: input.action.clone(),
                    payload_json: input.payload_json.clone(),
                })
                .collect();

            Ok((snapshot, current_frame_id, recent_inputs))
        } else {
            Err("PLAYER_NOT_IN_ROOM")
        }
    }

    pub async fn join_room_as_observer(
        &self,
        room_id: &str,
        player_id: &str,
        sender: mpsc::UnboundedSender<OutboundMessage>,
    ) -> Result<(RoomSnapshot, u32, Vec<FrameInput>), &'static str> {
        let mut rooms = self.rooms.lock().await;
        let mut runtimes = self.runtimes.lock().await;
        let default_policy = self.policies.default_policy().clone();
        let (snapshot, current_frame_id, recent_inputs) = {
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
            let recent_inputs = room
                .get_inputs_in_range(current_frame_id.saturating_sub(300), current_frame_id)
                .iter()
                .map(|input| FrameInput {
                    player_id: input.player_id.clone(),
                    action: input.action.clone(),
                    payload_json: input.payload_json.clone(),
                })
                .collect();

            (snapshot, current_frame_id, recent_inputs)
        };
        let room_count = rooms.len() as u64;
        METRICS.set_room_count(room_count);

        info!(
            room_id = room_id,
            player_id = player_id,
            current_frame_id = current_frame_id,
            "observer joined"
        );

        Ok((snapshot, current_frame_id, recent_inputs))
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

                if room.owner_player_id != *"" && !room.members.contains_key(&room.owner_player_id) {
                    if let Some(next) = room.members.keys().next() {
                        room.owner_player_id = next.clone();
                    }
                }

                if !room.has_online_members() {
                    room.mark_empty();
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
        room.current_frame = 0;
        room.pending_inputs.clear();
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

        room.can_send_input(player_id)?;
        room.update_activity();
        room.logic.on_player_input(player_id, action, payload_json);
        let input_record = PlayerInputRecord {
            frame_id,
            player_id: player_id.to_string(),
            action: action.to_string(),
            payload_json: payload_json.to_string(),
            received_at: Instant::now(),
        };
        room.pending_inputs.push(input_record.clone());
        room.push_input_history(input_record);

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

            let (frame_bundle, pending_broadcasts) = {
                let mut rooms = self.rooms.lock().await;
                let Some(room) = rooms.get_mut(&room_id) else {
                    break;
                };

                room.update_activity();

                if room.phase != RoomPhase::InGame {
                    continue;
                }

                let policy = self.policies.resolve(&room.policy_id);
                let snapshot_interval = policy.snapshot_interval_frames;

                room.current_frame = room.current_frame.saturating_add(1);
                let frame_id = room.current_frame;
                let drained = std::mem::take(&mut room.pending_inputs);

                let (tick_inputs, future_inputs): (Vec<_>, Vec<_>) = drained
                    .into_iter()
                    .partition(|input| input.frame_id <= frame_id);

                room.pending_inputs = future_inputs;

                let inputs = tick_inputs
                    .iter()
                    .map(|input| FrameInput {
                        player_id: input.player_id.clone(),
                        action: input.action.clone(),
                        payload_json: input.payload_json.clone(),
                    })
                    .collect::<Vec<_>>();

                room.logic.on_tick(frame_id, fps, &tick_inputs);
                let pending_broadcasts = room.logic.take_pending_broadcasts();

                let snapshot = if frame_id % snapshot_interval == 0 {
                    room.last_snapshot_frame = frame_id;
                    info!(
                        room_id = %room_id,
                        frame_id = frame_id,
                        snapshot_interval = snapshot_interval,
                        ">>> SNAPSHOT GENERATED at frame {} <<<",
                        frame_id
                    );
                    Some(room.snapshot())
                } else {
                    None
                };

                if !inputs.is_empty() {
                    info!(
                        room_id = %room_id,
                        frame_id = frame_id,
                        fps = fps,
                        input_count = inputs.len(),
                        has_snapshot = snapshot.is_some(),
                        "FRAME Bundle: inputs={} frame={} fps={}",
                        inputs.len(),
                        frame_id,
                        fps
                    );
                    for input in &inputs {
                        info!(
                            room_id = %room_id,
                            frame_id = frame_id,
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
                        frame_id = frame_id,
                        fps = fps,
                        has_snapshot = snapshot.is_some(),
                        "FRAME Bundle: SILENT frame={} fps={}",
                        frame_id,
                        fps
                    );
                }

                (
                    FrameBundlePush {
                        room_id: room.room_id.clone(),
                        frame_id,
                        fps: u32::from(fps),
                        is_silent_frame: inputs.is_empty(),
                        inputs,
                        snapshot,
                    },
                    pending_broadcasts,
                )
            };

            for RoomLogicBroadcast { message_type, body } in pending_broadcasts {
                let _ = self.broadcast_to_room(&room_id, message_type, body).await;
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
}
