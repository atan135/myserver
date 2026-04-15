use crate::core::system::movement::state::RoomMovementState;

#[derive(Debug, Clone)]
pub struct ReconcileDecision {
    pub should_emit_snapshot: bool,
    pub full_sync: bool,
    pub reason: &'static str,
}

pub fn decide_snapshot(
    state: &mut RoomMovementState,
    frame_id: u32,
    changed_count: usize,
    reject_count: usize,
) -> ReconcileDecision {
    if reject_count > 0 {
        state.last_snapshot_frame = frame_id;
        return ReconcileDecision {
            should_emit_snapshot: true,
            full_sync: true,
            reason: "movement_rejected",
        };
    }

    if changed_count == 0 {
        return ReconcileDecision {
            should_emit_snapshot: false,
            full_sync: false,
            reason: "no_change",
        };
    }

    if state.last_snapshot_frame == 0
        || frame_id.saturating_sub(state.last_snapshot_frame) >= state.snapshot_interval_frames
    {
        state.last_snapshot_frame = frame_id;
        return ReconcileDecision {
            should_emit_snapshot: true,
            full_sync: false,
            reason: "movement_changed",
        };
    }

    ReconcileDecision {
        should_emit_snapshot: false,
        full_sync: false,
        reason: "snapshot_throttled",
    }
}
