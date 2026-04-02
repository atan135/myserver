use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant as TokioInstant, sleep_until};
use tracing::info;

use crate::core::room::{OutboundMessage, PlayerInputRecord, Room, RoomMemberState, RoomPhase};
use crate::core::room_logic::RoomLogicFactory;
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
        Self::default()
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
            },
        );

        if is_new_member {
            room.logic.on_player_join(player_id);
        }

        runtimes
            .entry(room_id.to_string())
            .or_insert_with(RoomRuntime::default);

        Ok(room.snapshot())
    }

    pub async fn leave_room(&self, room_id: &str, player_id: &str) -> RoomLeaveResult {
        let mut rooms = self.rooms.lock().await;
        let mut runtimes = self.runtimes.lock().await;
        let Some(room) = rooms.get_mut(room_id) else {
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        };

        if room.members.remove(player_id).is_none() {
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        }

        room.logic.on_player_leave(player_id);
        let policy = self.policies.resolve(&room.policy_id);

        if room.members.is_empty() && policy.destroy_when_empty {
            info!(
                room_id = room_id,
                policy_id = %room.policy_id,
                "room removed because it became empty"
            );
            rooms.remove(room_id);
            if let Some(runtime) = runtimes.remove(room_id) {
                if let Some(handle) = runtime.tick_handle {
                    handle.abort();
                }
            }
            return RoomLeaveResult {
                snapshot: None,
                room_removed: true,
            };
        }

        if room.owner_player_id == player_id {
            if let Some(next_owner) = room.members.keys().next() {
                room.owner_player_id = next_owner.clone();
            }
        }

        room.reset_to_waiting();

        RoomLeaveResult {
            snapshot: Some(room.snapshot()),
            room_removed: false,
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
        room.logic.on_game_started();
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
        room.logic.on_game_ended();
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
        let member_count = room.members.len();

        if member_count == 0 {
            return policy.silent_room_fps.max(1);
        }

        match room.phase {
            RoomPhase::Waiting => policy.idle_room_fps.max(1),
            RoomPhase::InGame => {
                if member_count >= policy.busy_room_player_threshold {
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

                if room.phase != RoomPhase::InGame {
                    continue;
                }

                room.current_frame = room.current_frame.saturating_add(1);
                let frame_id = room.current_frame;
                let drained = std::mem::take(&mut room.pending_inputs);
                let inputs = drained
                    .into_iter()
                    .filter(|input| input.frame_id <= frame_id)
                    .map(|input| FrameInput {
                        player_id: input.player_id,
                        action: input.action,
                        payload_json: input.payload_json,
                    })
                    .collect::<Vec<_>>();

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
                return Ok(());
            };

            room.members
                .values()
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
