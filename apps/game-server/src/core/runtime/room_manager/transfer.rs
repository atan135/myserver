use super::transfer_codec::*;
use super::*;

use serde_json::json;

use crate::pb::{RoomMigrationState, RoomTransferPayload};

impl RoomManager {
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
            let room_entry = match self.get_room_entry(room_id).await {
                Some(room_entry) => room_entry,
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
            let mut room = room_entry.lock().await;

            if room.marked_for_destruction {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_NOT_FOUND",
                    "room transfer freeze rejected because room is being destroyed"
                );
                return Err("ROOM_NOT_FOUND");
            }

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

        let room_entry = match self.get_room_entry(room_id).await {
            Some(room_entry) => room_entry,
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

        let mut room = room_entry.lock().await;
        if room.marked_for_destruction {
            warn!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "ROOM_NOT_FOUND",
                "room transfer export rejected because room is being destroyed"
            );
            return Err("ROOM_NOT_FOUND");
        }
        let was_exported = room.transfer_state.status == RoomTransferStatus::Exported;
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

        let room_version = if was_exported {
            room.transfer_state.room_version
        } else {
            room.transfer_state.room_version.saturating_add(1)
        };

        let mut payload = RoomTransferPayload {
            rollout_epoch: rollout_epoch.to_string(),
            room_id: room.room_id.clone(),
            room_version,
            policy_id: room.policy_id.clone(),
            owner_player_id: room.owner_player_id.clone(),
            room_phase: room_phase_name(room.phase).to_string(),
            current_frame_id,
            last_applied_frame_id,
            snapshot: Some(room.snapshot()),
            recent_inputs: room_frame_inputs_from_history(&room, current_frame_id),
            waiting_frame_id: room.current_waiting_frame_id(),
            waiting_inputs: room_frame_inputs_from_pending(&room, room.current_waiting_frame_id()),
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
            )?,
            match_id: room.match_id.clone().unwrap_or_default(),
            checksum: String::new(),
        };
        payload.checksum = room_transfer_checksum(&payload);

        if was_exported {
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
        } else {
            room.transfer_state.status = RoomTransferStatus::Exported;
            room.transfer_state.room_version = payload.room_version;
            room.transfer_state.last_transfer_checksum = Some(payload.checksum.clone());
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

        if self.room_exists(&room_id).await {
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

        let (_room_entry, room_count, inserted) = self.publish_room_entry(&room_id, room).await;
        if !inserted {
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
        if let Some(room_entry) = self.get_room_entry(&room_id).await {
            self.rebuild_room_indexes(&room_id, &room_entry).await;
        }
        METRICS.set_room_count(room_count as u64);

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

        let room_entry = match self.get_room_entry(room_id).await {
            Some(room_entry) => room_entry,
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
        let room = room_entry.lock().await;
        if room.marked_for_destruction {
            warn!(
                room_id = room_id,
                rollout_epoch = rollout_epoch,
                error_code = "ROOM_NOT_FOUND",
                checksum = checksum,
                room_version = room_version,
                "room ownership confirm rejected because room is being destroyed"
            );
            return Err("ROOM_NOT_FOUND");
        }

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
            let room_entry = match self.get_room_entry(room_id).await {
                Some(room_entry) => room_entry,
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
            let mut room = room_entry.lock().await;
            if room.marked_for_destruction {
                warn!(
                    room_id = room_id,
                    rollout_epoch = rollout_epoch,
                    error_code = "ROOM_NOT_FOUND",
                    checksum = checksum,
                    "room transfer retire rejected because room is being destroyed"
                );
                return Err("ROOM_NOT_FOUND");
            }

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

        self.remove_room_indexes(room_id).await;
        self.stop_room_tick(room_id).await;
        Ok(())
    }
}
