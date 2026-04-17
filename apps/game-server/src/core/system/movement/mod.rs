pub mod correction;
pub mod input;
pub mod reconcile;
pub mod sim;
pub mod state;

use crate::core::system::GameplaySystem;

pub use correction::{
    correction_reason_label, decide_corrections, full_sync_broadcast, reject_broadcast,
    snapshot_broadcasts,
};
pub use input::player_input_from_move_req;
pub use reconcile::decide_snapshot;
pub use sim::tick_movement;
pub use state::RoomMovementState;

pub trait MovementSystem: GameplaySystem {
    fn movement_state(&self) -> &RoomMovementState;
}
