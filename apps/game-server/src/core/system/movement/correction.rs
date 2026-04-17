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
    all_player_ids: &[String],
    result: &SimulationTickResult,
) -> Vec<MovementCorrectionEnvelope> {
    let mut corrections = Vec::new();
    let mut targeted_players = std::collections::BTreeSet::new();

    for reject in &result.rejects {
        targeted_players.insert(reject.player_id.clone());
        corrections.push(state.strong_correction(
            frame_id,
            MovementCorrectionReason::try_from(reject.reason_code)
                .unwrap_or(MovementCorrectionReason::MovementRejected),
            vec![reject.player_id.clone()],
            state.targets_for_player(&reject.player_id),
        ));
    }

    for drift in &result.drifted_players {
        if targeted_players.contains(&drift.player_id) {
            continue;
        }
        targeted_players.insert(drift.player_id.clone());
        corrections.push(state.strong_correction(
            frame_id,
            MovementCorrectionReason::ClientDrift,
            vec![drift.player_id.clone()],
            state.targets_for_player(&drift.player_id),
        ));
    }

    if !result.changed_entities.is_empty() && state.should_periodic_sync(frame_id) {
        corrections.push(state.incremental_correction(
            frame_id,
            MovementCorrectionReason::Periodic,
            all_player_ids.to_vec(),
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

pub fn reject_broadcast(room_id: &str, frame_id: u32, reject: &MovementRejectRecord) -> RoomLogicBroadcast {
    let reason = MovementCorrectionReason::try_from(reject.reason_code)
        .unwrap_or(MovementCorrectionReason::MovementRejected);
    let message = MovementRejectPush {
        room_id: room_id.to_string(),
        frame_id,
        player_id: reject.player_id.clone(),
        error_code: reject.error_code.clone(),
        corrected: Some(reject.corrected.clone()),
        correction_kind: MovementCorrectionKind::Strong as i32,
        reason_code: reason as i32,
        reference_frame_id: reject.client_state.map(|state| state.frame_id).unwrap_or(frame_id),
        has_client_state: reject.client_state.is_some(),
        client_x: reject.client_state.map(|state| state.x).unwrap_or_default(),
        client_y: reject.client_state.map(|state| state.y).unwrap_or_default(),
        server_x: reject.server_x,
        server_y: reject.server_y,
    };

    RoomLogicBroadcast::broadcast_to_players(
        MessageType::MovementRejectPush,
        encode_body(&message),
        vec![reject.player_id.clone()],
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
            MovementCorrectionKind::FullSync | MovementCorrectionKind::Strong | MovementCorrectionKind::Recovery
        ),
        reason: correction_reason_label(reason).to_string(),
        correction_kind: correction.correction_kind,
        reason_code: correction.reason_code,
        target_player_ids: correction.target_player_ids.clone(),
        reference_frame_id: correction.reference_frame_id,
    };

    if correction.target_player_ids.is_empty() {
        RoomLogicBroadcast::broadcast_to_room(
            MessageType::MovementSnapshotPush,
            encode_body(&message),
        )
    } else {
        RoomLogicBroadcast::broadcast_to_players(
            MessageType::MovementSnapshotPush,
            encode_body(&message),
            correction.target_player_ids,
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
    }
}
