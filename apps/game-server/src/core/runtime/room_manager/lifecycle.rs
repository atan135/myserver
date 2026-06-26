use super::*;

use super::transfer_codec::{room_frame_inputs_from_history, room_frame_inputs_from_pending};

impl RoomManager {
    pub async fn create_matched_room(
        &self,
        match_id: &str,
        room_id: &str,
        character_ids: &[String],
        mode: &str,
    ) -> Result<RoomSnapshot, &'static str> {
        let default_policy = self.policies.default_policy();
        let (snapshot, room_count) = if let Some(room_entry) = self.get_room_entry(room_id).await {
            let room = room_entry.lock().await;
            if room_rejects_mutation(&room) {
                return Err(room_mutation_error_code(&room));
            }

            (room.snapshot(), self.room_count().await)
        } else {
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
                character_ids.first().cloned().unwrap_or_default(),
                default_policy.policy_id.clone(),
                logic,
            );
            room.set_match_id(match_id.to_string());
            let (room_entry, room_count, inserted) = self.publish_room_entry(room_id, room).await;
            let room = room_entry.lock().await;
            if !inserted && room_rejects_mutation(&room) {
                return Err(room_mutation_error_code(&room));
            }
            (room.snapshot(), room_count)
        };

        if self.get_runtime_entry(room_id).await.is_none() {
            self.ensure_runtime_entry(room_id).await;
        }
        METRICS.set_room_count(room_count as u64);

        self.notify_room_created(match_id, room_id, character_ids, mode)
            .await;

        Ok(snapshot)
    }

    pub(super) fn join_existing_room_locked(
        &self,
        room: &mut Room,
        character_id: &str,
        outbound: OutboundChannel,
        role: MemberRole,
    ) -> Result<(RoomSnapshot, Option<String>), &'static str> {
        if room_rejects_mutation(room) {
            return Err(room_mutation_error_code(room));
        }

        let policy = self.policies.resolve(&room.policy_id);
        if room.phase == RoomPhase::InGame
            && !policy.allow_join_in_game
            && !room.members.contains_key(character_id)
        {
            return Err("ROOM_ALREADY_IN_GAME");
        }

        if room.members.len() >= policy.max_members && !room.members.contains_key(character_id) {
            return Err("ROOM_FULL");
        }

        let is_new_member = !room.members.contains_key(character_id);
        let sync_before_broadcast =
            is_new_member && room.phase == RoomPhase::InGame && policy.allow_join_in_game;
        room.members.insert(
            character_id.to_string(),
            RoomMemberState {
                character_id: character_id.to_string(),
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
            room.logic.on_character_join(character_id);
        }

        Ok((room.snapshot(), room.match_id.clone()))
    }

    pub(super) fn join_observer_locked(
        &self,
        room: &mut Room,
        character_id: &str,
        outbound: OutboundChannel,
    ) -> Result<RoomRecoveryState, &'static str> {
        if room_rejects_mutation(room) {
            return Err(room_mutation_error_code(room));
        }

        let policy = self.policies.resolve(&room.policy_id);
        if room.members.len() >= policy.max_members && !room.members.contains_key(character_id) {
            return Err("ROOM_FULL");
        }

        let is_new_member = !room.members.contains_key(character_id);
        room.members.insert(
            character_id.to_string(),
            RoomMemberState {
                character_id: character_id.to_string(),
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
            room.logic.on_character_join(character_id);
        }

        let snapshot = room.snapshot();
        let current_frame_id = room.current_frame;
        let recent_inputs = room_frame_inputs_from_history(room, current_frame_id);
        let waiting_frame_id = room.current_waiting_frame_id();
        let waiting_inputs = room_frame_inputs_from_pending(room, waiting_frame_id);
        let input_delay_frames = self.policies.resolve(&room.policy_id).input_delay_frames;
        let movement_recovery = room
            .logic
            .movement_recovery_state(None, MovementCorrectionReason::ObserverRecovery);

        Ok(RoomRecoveryState {
            snapshot,
            current_frame_id,
            recent_inputs,
            waiting_frame_id,
            waiting_inputs,
            input_delay_frames,
            movement_recovery,
        })
    }

    pub async fn join_room(
        &self,
        room_id: &str,
        character_id: &str,
        outbound: impl Into<OutboundChannel>,
        role: MemberRole,
        requested_policy_id: Option<&str>,
    ) -> Result<RoomSnapshot, &'static str> {
        let outbound = outbound.into();
        let requested_policy_id = requested_policy_id
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.policies.default_policy().policy_id);
        let selected_policy = self.policies.resolve(&requested_policy_id);
        let mut outbound = Some(outbound);
        let (snapshot, match_id, room_count) = if let Some(room_entry) =
            self.get_room_entry(room_id).await
        {
            let mut room = room_entry.lock().await;
            let (snapshot, match_id) = self.join_existing_room_locked(
                &mut room,
                character_id,
                outbound.take().expect("outbound should be available"),
                role,
            )?;
            (snapshot, match_id, self.room_count().await)
        } else {
            let mut logic = self.logic_factory.create(&selected_policy.policy_id);
            logic.on_room_created(room_id);
            info!(
                room_id = room_id,
                owner_character_id = character_id,
                policy_id = %selected_policy.policy_id,
                "room created"
            );
            let mut room = Room::new(
                room_id.to_string(),
                character_id.to_string(),
                selected_policy.policy_id.clone(),
                logic,
            );
            let (new_snapshot, new_match_id) = self.join_existing_room_locked(
                &mut room,
                character_id,
                outbound
                    .as_ref()
                    .expect("outbound should be available")
                    .clone(),
                role,
            )?;
            let (room_entry, room_count, inserted) = self.publish_room_entry(room_id, room).await;
            if inserted {
                info!(
                    room_id = room_id,
                    character_id = character_id,
                    "room initialized and published"
                );
                (new_snapshot, new_match_id, room_count)
            } else {
                let mut room = room_entry.lock().await;
                let (snapshot, match_id) = self.join_existing_room_locked(
                    &mut room,
                    character_id,
                    outbound.take().expect("outbound should be available"),
                    role,
                )?;
                (snapshot, match_id, room_count)
            }
        };
        if self.get_runtime_entry(room_id).await.is_none() {
            self.ensure_runtime_entry(room_id).await;
        }
        self.set_character_index(character_id, room_id, false).await;
        METRICS.set_room_count(room_count as u64);

        if let Some(ref mid) = match_id {
            self.notify_player_joined(mid, character_id, room_id).await;
        }
        self.update_room_fps(room_id).await;

        Ok(snapshot)
    }

    pub async fn finish_member_sync(&self, room_id: &str, character_id: &str) {
        let sync_completed = {
            let Some(room_entry) = self.get_room_entry(room_id).await else {
                return;
            };
            let mut room = room_entry.lock().await;
            room.finish_member_sync(character_id)
        };

        if sync_completed {
            info!(
                room_id = room_id,
                character_id = character_id,
                "room member sync completed"
            );
            self.update_room_fps(room_id).await;
        }
    }

    pub async fn is_member_syncing(&self, room_id: &str, character_id: &str) -> bool {
        let Some(room_entry) = self.get_room_entry(room_id).await else {
            return false;
        };
        let room = room_entry.lock().await;
        room.members
            .get(character_id)
            .map(|member| member.syncing)
            .unwrap_or(false)
    }

    pub async fn leave_room(&self, room_id: &str, character_id: &str) -> RoomLeaveResult {
        info!(
            room_id = room_id,
            character_id = character_id,
            "leave_room called"
        );

        let Some(room_entry) = self.get_room_entry(room_id).await else {
            info!(room_id = room_id, "leave_room: room not found");
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        };
        let mut room = room_entry.lock().await;
        if room.marked_for_destruction {
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        }
        let previous_online_member_count = room
            .members
            .values()
            .filter(|member| !member.offline)
            .count();

        if let Some(member) = room.members.get_mut(character_id) {
            member.offline = true;
            member.offline_since = Some(Instant::now());
            detach_member_outbound(member);
            room.logic.on_character_offline(room_id, character_id);
            info!(
                room_id = room_id,
                character_id = character_id,
                "player marked offline, members count: {}",
                room.members.len()
            );
        } else {
            info!(
                room_id = room_id,
                character_id = character_id,
                "leave_room: player not found in room members, current members: {:?}",
                room.members.keys().collect::<Vec<_>>()
            );
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        }

        let policy = self.policies.resolve(&room.policy_id);

        if room.owner_character_id == character_id {
            if let Some(next_owner) = room
                .members
                .values()
                .find(|m| !m.offline)
                .map(|m| m.character_id.clone())
            {
                room.owner_character_id = next_owner;
            }
        }

        if !room.has_online_members() {
            room.mark_empty();
            if previous_online_member_count > 0 {
                log_room_entered_transferable_empty_candidate(&room, character_id, "leave_room");
            }
        }

        let _ = policy;
        room.reset_to_waiting();

        let pending_broadcasts = room.logic.take_pending_broadcasts();
        let snapshot = room.snapshot();
        let match_id = room.match_id.clone();
        drop(room);

        self.set_character_index(character_id, room_id, true).await;

        self.broadcast_logic_broadcasts(room_id, pending_broadcasts)
            .await;
        self.update_room_fps(room_id).await;

        if let Some(ref mid) = match_id {
            let should_abort = self.notify_player_left(mid, character_id, "normal").await;
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

    pub async fn disconnect_room_member(
        &self,
        room_id: &str,
        character_id: &str,
    ) -> RoomLeaveResult {
        info!(
            room_id = room_id,
            character_id = character_id,
            "disconnect_room_member called"
        );

        let Some(room_entry) = self.get_room_entry(room_id).await else {
            info!(room_id = room_id, "disconnect_room_member: room not found");
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        };
        let mut room = room_entry.lock().await;
        if room.marked_for_destruction {
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        }
        let previous_online_member_count = room
            .members
            .values()
            .filter(|member| !member.offline)
            .count();

        if let Some(member) = room.members.get_mut(character_id) {
            member.offline = true;
            member.offline_since = Some(Instant::now());
            detach_member_outbound(member);
            room.logic.on_character_offline(room_id, character_id);
            info!(
                room_id = room_id,
                character_id = character_id,
                phase = ?room.phase,
                "player marked offline without resetting runtime state"
            );
        } else {
            info!(
                room_id = room_id,
                character_id = character_id,
                "disconnect_room_member: player not found in room members, current members: {:?}",
                room.members.keys().collect::<Vec<_>>()
            );
            return RoomLeaveResult {
                snapshot: None,
                room_removed: false,
            };
        }

        if room.owner_character_id == character_id {
            if let Some(next_owner) = room
                .members
                .values()
                .find(|m| !m.offline)
                .map(|m| m.character_id.clone())
            {
                room.owner_character_id = next_owner;
            }
        }

        if !room.has_online_members() {
            room.mark_empty();
            room.wait_started_at = None;
            if previous_online_member_count > 0 {
                log_room_entered_transferable_empty_candidate(
                    &room,
                    character_id,
                    "disconnect_room_member",
                );
            }
        }

        let pending_broadcasts = room.logic.take_pending_broadcasts();
        let snapshot = room.snapshot();
        let match_id = room.match_id.clone();
        drop(room);

        self.set_character_index(character_id, room_id, true).await;

        self.broadcast_logic_broadcasts(room_id, pending_broadcasts)
            .await;
        self.update_room_fps(room_id).await;

        if let Some(ref mid) = match_id {
            let should_abort = self
                .notify_player_left(mid, character_id, "disconnect")
                .await;
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
        character_id: &str,
        outbound: impl Into<OutboundChannel>,
    ) -> Result<RoomRecoveryState, &'static str> {
        let outbound = outbound.into();
        let room_entry = self.get_room_entry(room_id).await.ok_or("ROOM_NOT_FOUND")?;
        let mut room = room_entry.lock().await;

        if room_rejects_mutation(&room) {
            return Err(room_mutation_error_code(&room));
        }

        if let Some(member) = room.members.get_mut(character_id) {
            if !member.offline {
                return Err("PLAYER_ALREADY_ONLINE");
            }

            member.offline = false;
            member.offline_since = None;
            member.sender = outbound.sender;
            member.close_state = outbound.close_state;
            member.syncing = false;
            room.logic.on_character_online(room_id, character_id);
            room.clear_empty();
            room.update_activity();

            info!(
                room_id = room_id,
                character_id = character_id,
                "player reconnected"
            );

            let snapshot = room.snapshot();
            let current_frame_id = room.current_frame;
            let recent_inputs = room_frame_inputs_from_history(&room, current_frame_id);
            let waiting_frame_id = room.current_waiting_frame_id();
            let waiting_inputs = room_frame_inputs_from_pending(&room, waiting_frame_id);
            let input_delay_frames = self.policies.resolve(&room.policy_id).input_delay_frames;
            let movement_recovery = room.logic.movement_recovery_state(
                Some(character_id),
                MovementCorrectionReason::ReconnectRecovery,
            );
            let match_id = room.match_id.clone();
            drop(room);

            self.remove_offline_character_index(character_id, room_id)
                .await;
            self.set_character_index(character_id, room_id, false).await;

            if let Some(ref mid) = match_id {
                self.notify_player_joined(mid, character_id, room_id).await;
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
        character_id: &str,
        outbound: impl Into<OutboundChannel>,
    ) -> Result<RoomRecoveryState, &'static str> {
        let outbound = outbound.into();
        let default_policy = self.policies.default_policy().clone();
        let mut outbound = Some(outbound);
        let (recovery, room_count) = if let Some(room_entry) = self.get_room_entry(room_id).await {
            let mut room = room_entry.lock().await;
            let recovery = self.join_observer_locked(
                &mut room,
                character_id,
                outbound.take().expect("outbound should be available"),
            )?;
            (recovery, self.room_count().await)
        } else {
            let mut logic = self.logic_factory.create(&default_policy.policy_id);
            logic.on_room_created(room_id);
            info!(
                room_id = room_id,
                owner_character_id = character_id,
                policy_id = %default_policy.policy_id,
                "room created for observer"
            );
            let mut room = Room::new(
                room_id.to_string(),
                character_id.to_string(),
                default_policy.policy_id.clone(),
                logic,
            );
            let new_recovery = self.join_observer_locked(
                &mut room,
                character_id,
                outbound
                    .as_ref()
                    .expect("outbound should be available")
                    .clone(),
            )?;
            let (room_entry, room_count, inserted) = self.publish_room_entry(room_id, room).await;
            if inserted {
                (new_recovery, room_count)
            } else {
                let mut room = room_entry.lock().await;
                let recovery = self.join_observer_locked(
                    &mut room,
                    character_id,
                    outbound.take().expect("outbound should be available"),
                )?;
                (recovery, room_count)
            }
        };
        if self.get_runtime_entry(room_id).await.is_none() {
            self.ensure_runtime_entry(room_id).await;
        }
        self.set_character_index(character_id, room_id, false).await;
        METRICS.set_room_count(room_count as u64);

        info!(
            room_id = room_id,
            character_id = character_id,
            current_frame_id = recovery.current_frame_id,
            "observer joined"
        );

        self.update_room_fps(room_id).await;

        Ok(recovery)
    }

    pub async fn cleanup_expired_offline_characters(&self) {
        for (room_id, room_entry) in self.room_entries_snapshot().await {
            let mut room = room_entry.lock().await;
            if room_rejects_mutation(&room) {
                continue;
            }

            let policy = self.policies.resolve(&room.policy_id);
            let expired = room.collect_expired_offline_characters(policy.offline_ttl_secs);

            if !expired.is_empty() {
                info!(
                    room_id = room_id,
                    expired_players = ?expired,
                    ttl_secs = policy.offline_ttl_secs,
                    "removing expired offline characters"
                );

                for character_id in &expired {
                    room.logic.on_character_leave(character_id);
                }

                room.remove_members(&expired);
                for character_id in &expired {
                    self.remove_character_indexes_for_room(character_id, &room_id)
                        .await;
                }

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
        character_id: &str,
        ready: bool,
    ) -> Result<RoomSnapshot, &'static str> {
        let room_entry = self.get_room_entry(room_id).await.ok_or("ROOM_NOT_FOUND")?;
        let mut room = room_entry.lock().await;

        if room_rejects_mutation(&room) {
            return Err(room_mutation_error_code(&room));
        }
        if room.phase == RoomPhase::InGame {
            return Err("ROOM_ALREADY_IN_GAME");
        }

        let member = room
            .members
            .get_mut(character_id)
            .ok_or("ROOM_MEMBER_NOT_FOUND")?;
        member.ready = ready;
        Ok(room.snapshot())
    }

    pub async fn start_game(
        &self,
        room_id: &str,
        character_id: &str,
    ) -> Result<RoomSnapshot, &'static str> {
        {
            let room_entry = self.get_room_entry(room_id).await.ok_or("ROOM_NOT_FOUND")?;
            let mut room = room_entry.lock().await;
            let policy = self.policies.resolve(&room.policy_id);

            if room_rejects_mutation(&room) {
                return Err(room_mutation_error_code(&room));
            }
            room.can_start_game(character_id, policy.min_start_players)?;
            room.phase = RoomPhase::InGame;
            room.clear_runtime_inputs();
            room.logic.on_game_started(room_id);
            info!(
                room_id = room_id,
                owner_character_id = character_id,
                member_count = room.members.len(),
                "room entered in_game phase"
            );
        }

        self.ensure_room_tick_running(room_id).await;
        self.update_room_fps(room_id).await;

        let room_entry = self.get_room_entry(room_id).await.ok_or("ROOM_NOT_FOUND")?;
        let room = room_entry.lock().await;
        Ok(room.snapshot())
    }

    pub async fn accept_player_input(
        &self,
        room_id: &str,
        character_id: &str,
        frame_id: u32,
        action: &str,
        payload_json: &str,
    ) -> Result<(), &'static str> {
        let room_entry = self.get_room_entry(room_id).await.ok_or("ROOM_NOT_FOUND")?;
        let mut room = room_entry.lock().await;
        let policy = self.policies.resolve(&room.policy_id);

        if room_rejects_mutation(&room) {
            return Err(room_mutation_error_code(&room));
        }
        room.can_send_input(character_id)?;
        room.logic
            .validate_character_input(character_id, action, payload_json)?;
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
            character_id: character_id.to_string(),
            action: action.to_string(),
            payload_json: payload_json.to_string(),
            received_at: Instant::now(),
            is_synthetic: false,
        };
        let outcome = room.upsert_pending_input(input_record);
        room.update_activity();
        room.logic
            .on_character_input(character_id, action, payload_json);
        if matches!(outcome, PendingInputUpsert::Replaced) {
            info!(
                room_id = room_id,
                character_id = character_id,
                frame_id = frame_id,
                "pending input replaced for same frame"
            );
        }

        Ok(())
    }

    pub async fn end_game(
        &self,
        room_id: &str,
        character_id: &str,
    ) -> Result<RoomSnapshot, &'static str> {
        let room_entry = self.get_room_entry(room_id).await.ok_or("ROOM_NOT_FOUND")?;
        let mut room = room_entry.lock().await;

        if room_rejects_mutation(&room) {
            return Err(room_mutation_error_code(&room));
        }
        room.can_end_game(character_id)?;
        room.logic.on_game_ended(room_id);
        room.reset_to_waiting();
        info!(
            room_id = room_id,
            owner_character_id = character_id,
            member_count = room.members.len(),
            "room returned to waiting phase"
        );

        let match_id = room.match_id.clone();
        drop(room);

        self.update_room_fps(room_id).await;

        if let Some(ref mid) = match_id {
            self.notify_match_end(mid, room_id, "game_over").await;
        }

        let room_entry = self.get_room_entry(room_id).await.ok_or("ROOM_NOT_FOUND")?;
        let room = room_entry.lock().await;
        Ok(room.snapshot())
    }
}
