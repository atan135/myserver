use crate::core::logic::RoomLogicBroadcast;
use crate::core::system::movement::sim::{MovementRejectRecord, SimulationTickResult};
use crate::core::system::movement::state::{MovementCorrectionEnvelope, RoomMovementState};
use crate::pb::{
    MovementCorrectionKind, MovementCorrectionReason, MovementRejectPush, MovementSnapshotPush,
};
use crate::protocol::{MessageType, encode_body};

pub fn decide_corrections(
    state: &mut RoomMovementState,
    frame_id: u32,
    all_character_ids: &[String],
    result: &SimulationTickResult,
) -> Vec<MovementCorrectionEnvelope> {
    let mut corrections = Vec::new();
    let mut targeted_characters = std::collections::BTreeSet::new();

    for reject in &result.rejects {
        targeted_characters.insert(reject.character_id.clone());
        corrections.push(
            state.strong_correction(
                frame_id,
                MovementCorrectionReason::try_from(reject.reason_code)
                    .unwrap_or(MovementCorrectionReason::MovementRejected),
                vec![reject.character_id.clone()],
                state.targets_for_character(&reject.character_id),
            ),
        );
    }

    if !result.control_timeout_entities.is_empty() {
        corrections.push(state.incremental_correction(
            frame_id,
            MovementCorrectionReason::ControlTimeout,
            all_character_ids.to_vec(),
            result.control_timeout_entities.clone(),
        ));
    }

    for drift in &result.drifted_players {
        if targeted_characters.contains(&drift.character_id) {
            continue;
        }
        targeted_characters.insert(drift.character_id.clone());
        corrections.push(state.strong_correction(
            frame_id,
            MovementCorrectionReason::ClientDrift,
            vec![drift.character_id.clone()],
            state.targets_for_character(&drift.character_id),
        ));
    }

    if !result.changed_entities.is_empty() && state.should_periodic_sync(frame_id) {
        corrections.push(state.incremental_correction(
            frame_id,
            MovementCorrectionReason::Periodic,
            all_character_ids.to_vec(),
            result.changed_entities.clone(),
        ));
    }

    corrections
}

pub fn full_sync_broadcast(
    room_id: &str,
    state: &mut RoomMovementState,
    frame_id: u32,
    reason: MovementCorrectionReason,
) -> RoomLogicBroadcast {
    snapshot_broadcast_from_envelope(
        room_id,
        state.full_correction(frame_id, reason, Vec::new(), state.all_transforms()),
    )
}

pub fn snapshot_broadcasts(
    room_id: &str,
    corrections: Vec<MovementCorrectionEnvelope>,
) -> Vec<RoomLogicBroadcast> {
    corrections
        .into_iter()
        .map(|correction| snapshot_broadcast_from_envelope(room_id, correction))
        .collect()
}

pub fn reject_broadcast(
    room_id: &str,
    frame_id: u32,
    reject: &MovementRejectRecord,
) -> RoomLogicBroadcast {
    let reason = MovementCorrectionReason::try_from(reject.reason_code)
        .unwrap_or(MovementCorrectionReason::MovementRejected);
    let message = MovementRejectPush {
        room_id: room_id.to_string(),
        frame_id,
        character_id: reject.character_id.clone(),
        error_code: reject.error_code.clone(),
        corrected: Some(reject.corrected.clone()),
        correction_kind: MovementCorrectionKind::Strong as i32,
        reason_code: reason as i32,
        reference_frame_id: reject
            .client_state
            .map(|state| state.frame_id)
            .unwrap_or(frame_id),
        has_client_state: reject.client_state.is_some(),
        client_x: reject.client_state.map(|state| state.x).unwrap_or_default(),
        client_y: reject.client_state.map(|state| state.y).unwrap_or_default(),
        server_x: reject.server_x,
        server_y: reject.server_y,
    };

    RoomLogicBroadcast::broadcast_to_characters(
        MessageType::MovementRejectPush,
        encode_body(&message),
        vec![reject.character_id.clone()],
    )
}

fn snapshot_broadcast_from_envelope(
    room_id: &str,
    correction: MovementCorrectionEnvelope,
) -> RoomLogicBroadcast {
    let kind = MovementCorrectionKind::try_from(correction.correction_kind)
        .unwrap_or(MovementCorrectionKind::Incremental);
    let reason = MovementCorrectionReason::try_from(correction.reason_code)
        .unwrap_or(MovementCorrectionReason::Unknown);
    let message = MovementSnapshotPush {
        room_id: room_id.to_string(),
        frame_id: correction.frame_id,
        entities: correction.entities,
        full_sync: matches!(
            kind,
            MovementCorrectionKind::FullSync
                | MovementCorrectionKind::Strong
                | MovementCorrectionKind::Recovery
        ),
        reason: correction_reason_label(reason).to_string(),
        correction_kind: correction.correction_kind,
        reason_code: correction.reason_code,
        target_character_ids: correction.target_character_ids.clone(),
        reference_frame_id: correction.reference_frame_id,
    };

    if correction.target_character_ids.is_empty() {
        RoomLogicBroadcast::broadcast_to_room(
            MessageType::MovementSnapshotPush,
            encode_body(&message),
        )
    } else {
        RoomLogicBroadcast::broadcast_to_characters(
            MessageType::MovementSnapshotPush,
            encode_body(&message),
            correction.target_character_ids,
        )
    }
}

pub fn correction_reason_label(reason: MovementCorrectionReason) -> &'static str {
    match reason {
        MovementCorrectionReason::Unknown => "unknown",
        MovementCorrectionReason::Periodic => "periodic",
        MovementCorrectionReason::ClientDrift => "client_drift",
        MovementCorrectionReason::MovementRejected => "movement_rejected",
        MovementCorrectionReason::CollisionBlocked => "collision_blocked",
        MovementCorrectionReason::GameStarted => "game_started",
        MovementCorrectionReason::ReconnectRecovery => "reconnect_recovery",
        MovementCorrectionReason::ObserverRecovery => "observer_recovery",
        MovementCorrectionReason::PlayerOffline => "player_offline",
        MovementCorrectionReason::ControlTimeout => "control_timeout",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::system::movement::state::ClientStateSample;
    use crate::core::system::scene::query::SceneSpawnPointDefinition;
    use crate::pb::MovementCorrectionKind;
    use prost::Message;

    const ROOM_ID: &str = "main-world-public";
    const MAX_ROOM_MEMBERS: usize = 32;
    const ACTIVE_ROOM_FPS: usize = 20;

    fn spawn() -> SceneSpawnPointDefinition {
        SceneSpawnPointDefinition {
            id: 1001,
            scene_id: 1,
            code: "main_spawn".to_string(),
            spawn_type: "player".to_string(),
            x: 2002.0,
            y: 2002.0,
            dir_x: 1.0,
            dir_y: 0.0,
            radius: 0.0,
            tags: Vec::new(),
        }
    }

    fn full_public_room_state() -> (RoomMovementState, Vec<String>) {
        let mut state = RoomMovementState::new(1, 3);
        state.set_correction_config(3, 0.05, 16.0, false);
        let spawn = spawn();
        let recipients = (0..MAX_ROOM_MEMBERS)
            .map(|index| format!("character-{index:02}"))
            .collect::<Vec<_>>();
        for character_id in &recipients {
            state.spawn_character(character_id, &spawn, 4.0);
        }
        (state, recipients)
    }

    fn decode_snapshot(broadcast: &RoomLogicBroadcast) -> MovementSnapshotPush {
        MovementSnapshotPush::decode(broadcast.body.as_slice())
            .expect("movement snapshot body should decode")
    }

    #[test]
    fn aoi_disabled_full_sync_and_recovery_include_all_32_entities() {
        let (mut state, recipients) = full_public_room_state();

        let broadcast = full_sync_broadcast(
            ROOM_ID,
            &mut state,
            30,
            MovementCorrectionReason::GameStarted,
        );
        let snapshot = decode_snapshot(&broadcast);
        assert!(broadcast.target_character_ids.is_empty());
        assert!(snapshot.target_character_ids.is_empty());
        assert!(snapshot.full_sync);
        assert_eq!(
            snapshot.correction_kind,
            MovementCorrectionKind::FullSync as i32
        );
        assert_eq!(snapshot.entities.len(), MAX_ROOM_MEMBERS);

        let recovery = state.recovery_state_for_character(
            Some(&recipients[0]),
            31,
            MovementCorrectionReason::ReconnectRecovery,
        );
        assert!(!recovery.aoi_enabled);
        assert_eq!(recovery.entities.len(), MAX_ROOM_MEMBERS);
        assert_eq!(
            recovery.correction_kind,
            MovementCorrectionKind::Recovery as i32
        );

        let snapshot_bytes = snapshot.encoded_len();
        let recovery_bytes = recovery.encoded_len();
        let room_egress_bytes_per_second = snapshot_bytes * MAX_ROOM_MEMBERS * ACTIVE_ROOM_FPS;
        println!(
            "32-member movement sizes: snapshot={snapshot_bytes}B recovery={recovery_bytes}B \
             worst_case_room_egress_at_20hz={room_egress_bytes_per_second}B/s"
        );
        assert!(snapshot_bytes > 32 * 20);
        assert!(recovery_bytes > 32 * 20);
        assert!(room_egress_bytes_per_second < 2 * 1024 * 1024);
    }

    #[test]
    fn aoi_disabled_periodic_snapshot_broadcasts_changed_entities_to_whole_room() {
        let (mut state, recipients) = full_public_room_state();
        let changed = state.all_transforms()[..2].to_vec();
        let result = SimulationTickResult {
            changed_entities: changed.clone(),
            ..SimulationTickResult::default()
        };

        let first = decide_corrections(&mut state, 1, &recipients, &result);
        assert_eq!(first.len(), 1);
        let first_broadcast = snapshot_broadcasts(ROOM_ID, first).remove(0);
        let first_snapshot = decode_snapshot(&first_broadcast);
        assert_eq!(first_broadcast.target_character_ids, recipients);
        assert_eq!(first_snapshot.target_character_ids, recipients);
        assert_eq!(first_snapshot.entities, changed);
        assert!(!first_snapshot.full_sync);
        assert_eq!(
            first_snapshot.correction_kind,
            MovementCorrectionKind::Incremental as i32
        );

        assert!(decide_corrections(&mut state, 2, &recipients, &result).is_empty());
        assert!(decide_corrections(&mut state, 3, &recipients, &result).is_empty());
        assert_eq!(
            decide_corrections(&mut state, 4, &recipients, &result).len(),
            1
        );
    }

    #[test]
    fn aoi_disabled_reject_and_strong_correction_only_target_rejected_character() {
        let (mut state, recipients) = full_public_room_state();
        let rejected_character = recipients[0].clone();
        let corrected = state
            .entity(&rejected_character)
            .expect("rejected character should exist")
            .to_proto();
        let reject = MovementRejectRecord {
            character_id: rejected_character.clone(),
            error_code: "MOVEMENT_INVALID_INPUT".to_string(),
            corrected,
            reason_code: MovementCorrectionReason::MovementRejected as i32,
            client_state: Some(ClientStateSample {
                frame_id: 8,
                x: 2100.0,
                y: 2100.0,
            }),
            server_x: 2002.0,
            server_y: 2002.0,
        };
        let result = SimulationTickResult {
            rejects: vec![reject.clone()],
            ..SimulationTickResult::default()
        };

        let reject_message = reject_broadcast(ROOM_ID, 9, &reject);
        assert_eq!(
            reject_message.target_character_ids,
            vec![rejected_character.clone()]
        );
        let decoded_reject = MovementRejectPush::decode(reject_message.body.as_slice())
            .expect("movement reject body should decode");
        assert_eq!(decoded_reject.character_id, rejected_character);
        assert_eq!(decoded_reject.reference_frame_id, 8);
        assert!(decoded_reject.has_client_state);
        assert_eq!(
            decoded_reject.correction_kind,
            MovementCorrectionKind::Strong as i32
        );
        assert_eq!(
            decoded_reject.reason_code,
            MovementCorrectionReason::MovementRejected as i32
        );
        assert_eq!(decoded_reject.corrected.as_ref().unwrap().x, 2002.0);
        assert_eq!(decoded_reject.corrected.as_ref().unwrap().y, 2002.0);
        assert_eq!(decoded_reject.client_x, 2100.0);
        assert_eq!(decoded_reject.client_y, 2100.0);
        assert_eq!(decoded_reject.server_x, 2002.0);
        assert_eq!(decoded_reject.server_y, 2002.0);

        let correction = decide_corrections(&mut state, 9, &recipients, &result);
        assert_eq!(correction.len(), 1);
        let correction_message = snapshot_broadcasts(ROOM_ID, correction).remove(0);
        let snapshot = decode_snapshot(&correction_message);
        assert_eq!(
            correction_message.target_character_ids,
            vec![recipients[0].clone()]
        );
        assert_eq!(snapshot.target_character_ids, vec![recipients[0].clone()]);
        assert_eq!(snapshot.entities.len(), MAX_ROOM_MEMBERS);
        assert!(snapshot.full_sync);
        assert_eq!(
            snapshot.correction_kind,
            MovementCorrectionKind::Strong as i32
        );
    }
}
