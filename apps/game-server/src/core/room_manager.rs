use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant as TokioInstant, sleep_until};
use tracing::info;

use crate::core::room::{OutboundMessage, PlayerInputRecord, Room, RoomMemberState, RoomPhase};
use crate::gameroom::RoomLogicFactory;
use crate::core::room_policy::RoomPolicyRegistry;
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

#[derive(Clone, Default)]
pub struct RoomManager {
    rooms: Arc<Mutex<HashMap<String, Room>>>,
    runtimes: Arc<Mutex<HashMap<String, RoomRuntime>>>,
    policies: RoomPolicyRegistry,
    logic_factory: RoomLogicFactory,
}

impl RoomManager {
    pub fn new() -> Self {
        let this = Self::default();
        this.spawn_cleanup_task();
        this
    }

    fn spawn_cleanup_task(&self) {
        let rooms = Arc::clone(&self.rooms);
        let runtimes = Arc::clone(&self.runtimes);
        let policies = self.policies.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(10));
            loop {
                interval.tick().await;

                let mut to_destroy = Vec::new();

                {
                    let mut rooms_guard = rooms.lock().await;

                    for (room_id, room) in rooms_guard.iter_mut() {
                        // Skip if already marked for destruction
                        if room.marked_for_destruction {
                            continue;
                        }

                        let policy = policies.resolve(&room.policy_id);

                        // Check if destruction is enabled
                        if !policy.destroy_enabled {
                            continue;
                        }

                        // Check if we should destroy when empty
                        if !policy.destroy_when_empty {
                            continue;
                        }

                        // Only consider destroying if no online members
                        if room.has_online_members() {
                            continue;
                        }

                        // At this point: no online members, destruction enabled, destroy when empty
                        if !policy.retain_state_when_empty {
                            // No retain - mark for immediate destruction
                            info!(
                                room_id = room_id,
                                policy_id = %policy.policy_id,
                                "room marked for destruction (no retain)"
                            );
                            room.mark_for_destruction();
                            to_destroy.push(room_id.clone());
                            continue;
                        }

                        // Retain state - check TTL
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

                // Execute destruction outside of rooms lock
                for room_id in to_destroy {
                    if let Some(runtime) = runtimes.lock().await.get(&room_id) {
                        if let Some(handle) = &runtime.tick_handle {
                            handle.abort();
                        }
                    }
                    rooms.lock().await.remove(&room_id);
                    info!(room_id = room_id, "room destroyed by cleanup task");
                }
            }
        });
    }

    pub async fn room_count(&self) -> usize {
        self.rooms.lock().await.len()
    }

    pub async fn join_room(
        &self,
        room_id: &str,
        player_id: &str,
        sender: mpsc::UnboundedSender<OutboundMessage>,
    ) -> Result<RoomSnapshot, &'static str> {
        let mut rooms = self.rooms.lock().await;
        let mut runtimes = self.runtimes.lock().await;
        let default_policy = self.policies.default_policy().clone();
        let room = rooms.entry(room_id.to_string()).or_insert_with(|| {
            let mut logic = self.logic_factory.create(&default_policy.policy_id);
            logic.on_room_created(room_id);
            info!(
                room_id = room_id,
                owner_player_id = player_id,
                policy_id = %default_policy.policy_id,
                "room created"
            );
            Room::new(
                room_id.to_string(),
                player_id.to_string(),
                default_policy.policy_id.clone(),
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

        Ok(room.snapshot())
    }

    pub async fn leave_room(&self, room_id: &str, player_id: &str) -> RoomLeaveResult {
        info!(
            room_id = room_id,
            player_id = player_id,
            "leave_room called"
        );

        let mut rooms = self.rooms.lock().await;
        let mut runtimes = self.runtimes.lock().await;
        let Some(room) = rooms.get_mut(room_id) else {
            info!(room_id = room_id, "leave_room: room not found");
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        };

        // Mark player as offline instead of removing, so they can reconnect later
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

        // Transfer ownership if owner goes offline
        if room.owner_player_id == player_id {
            if let Some(next_owner) = room.members.values().find(|m| !m.offline).map(|m| m.player_id.clone()) {
                room.owner_player_id = next_owner;
            }
        }

        // If all members are offline, mark room as empty
        if !room.has_online_members() {
            room.mark_empty();
        }

        // Note: destruction is now handled by the cleanup task in RoomManager
        // based on policy.destroy_enabled, destroy_when_empty, retain_state_when_empty, and empty_ttl_secs

        room.reset_to_waiting();

        RoomLeaveResult {
            snapshot: Some(room.snapshot()),
            room_removed: false,
        }
    }

    pub async fn reconnect_room(
        &self,
        room_id: &str,
        player_id: &str,
        sender: mpsc::UnboundedSender<OutboundMessage>,
    ) -> Result<RoomSnapshot, &'static str> {
        let mut rooms = self.rooms.lock().await;
        let room = rooms.get_mut(room_id).ok_or("ROOM_NOT_FOUND")?;

        // Check if player is offline in this room
        if let Some(member) = room.members.get_mut(player_id) {
            if !member.offline {
                return Err("PLAYER_ALREADY_ONLINE");
            }

            // Reconnect the player
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

            Ok(room.snapshot())
        } else {
            Err("PLAYER_NOT_IN_ROOM")
        }
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
        room.pending_inputs.push(PlayerInputRecord {
            frame_id,
            player_id: player_id.to_string(),
            action: action.to_string(),
            payload_json: payload_json.to_string(),
            received_at: Instant::now(),
        });

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
        drop(rooms);

        self.update_room_fps(room_id).await;

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

            let frame_bundle = {
                let mut rooms = self.rooms.lock().await;
                let Some(room) = rooms.get_mut(&room_id) else {
                    break;
                };

                room.update_activity();

                if room.phase != RoomPhase::InGame {
                    continue;
                }

                room.current_frame = room.current_frame.saturating_add(1);
                let frame_id = room.current_frame;
                let drained = std::mem::take(&mut room.pending_inputs);
                let tick_inputs: Vec<PlayerInputRecord> = drained
                    .into_iter()
                    .filter(|input| input.frame_id <= frame_id)
                    .collect();

                let inputs = tick_inputs
                    .iter()
                    .map(|input| FrameInput {
                        player_id: input.player_id.clone(),
                        action: input.action.clone(),
                        payload_json: input.payload_json.clone(),
                    })
                    .collect::<Vec<_>>();

                room.logic.on_tick(frame_id, &tick_inputs);

                FrameBundlePush {
                    room_id: room.room_id.clone(),
                    frame_id,
                    fps: u32::from(fps),
                    is_silent_frame: inputs.is_empty(),
                    inputs,
                }
            };

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

            // Only broadcast to online members
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
