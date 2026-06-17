use super::transfer_codec::*;
use super::*;

use crate::core::runtime::room_policy::InputWaitStrategy;
use crate::pb::{FrameBundlePush, RoomFrameRatePush};
use crate::protocol::{MessageType, encode_body};
use tokio::time::{Instant as TokioInstant, sleep_until};

impl RoomManager {
    pub(super) async fn ensure_room_tick_running(&self, room_id: &str) {
        let runtime_entry = self.ensure_runtime_entry(room_id).await;
        let should_spawn = {
            let mut runtime = runtime_entry.lock().await;
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

        if let Some(runtime_entry) = self.get_runtime_entry(room_id).await {
            let mut runtime = runtime_entry.lock().await;
            if runtime.tick_running {
                runtime.tick_handle = Some(handle);
            } else {
                handle.abort();
            }
        } else {
            handle.abort();
        }
    }

    pub(super) async fn stop_room_tick(&self, room_id: &str) {
        if let Some(runtime_entry) = self.get_runtime_entry(room_id).await {
            let mut runtime = runtime_entry.lock().await;
            if let Some(handle) = runtime.tick_handle.take() {
                handle.abort();
            }
            runtime.tick_running = false;
        }
    }

    pub(super) async fn update_room_fps(&self, room_id: &str) {
        let target_fps = {
            let Some(room_entry) = self.get_room_entry(room_id).await else {
                return;
            };
            let room = room_entry.lock().await;
            if room.marked_for_destruction {
                return;
            }
            self.compute_room_fps(&room)
        };

        let changed = {
            let Some(runtime_entry) = self.get_runtime_entry(room_id).await else {
                return;
            };
            let mut runtime = runtime_entry.lock().await;
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

    pub(super) fn compute_room_fps(&self, room: &Room) -> u16 {
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

    pub(super) async fn process_room_tick(
        &self,
        room_id: &str,
        fps: u16,
    ) -> Option<(FrameBundlePush, Vec<RoomLogicBroadcast>)> {
        let room_entry = self.get_room_entry(room_id).await?;
        let mut room = room_entry.lock().await;

        if room_rejects_mutation(&room) {
            return None;
        }

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

        let (tick_inputs, newly_offline_players) =
            resolve_tick_inputs(&mut room, &participants, waiting_frame_id, &policy);
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

        drop(room);

        for player_id in newly_offline_players {
            self.set_player_index(&player_id, room_id, true).await;
        }

        Some((
            FrameBundlePush {
                room_id: room_id.to_string(),
                frame_id: waiting_frame_id,
                fps: u32::from(fps),
                is_silent_frame,
                inputs,
                snapshot,
            },
            pending_broadcasts,
        ))
    }

    pub(super) async fn run_room_tick_loop(self, room_id: String) {
        loop {
            let fps = {
                let Some(runtime_entry) = self.get_runtime_entry(&room_id).await else {
                    break;
                };
                let runtime = runtime_entry.lock().await;
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

        if let Some(runtime_entry) = self.get_runtime_entry(&room_id).await {
            let mut runtime = runtime_entry.lock().await;
            runtime.tick_running = false;
            runtime.tick_handle = None;
        }
        info!(room_id = room_id, "room tick stopped");
    }
}
