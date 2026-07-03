//! Deterministic simulation core shared by server and client code.
//!
//! `sim` is short for `simulation`, and `sim-core` is the deterministic model
//! boundary for lockstep gameplay rules. The current P0 scope supports
//! fixed-point values, quantized input, world state, a minimal movement tick,
//! stable state hashes, and serializable snapshots.
//!
//! P0 intentionally does not implement full combat resolution, entity or map
//! collision, server room policy integration, or Bevy scene/client integration.

#![forbid(unsafe_code)]

pub mod hash;
pub mod ids;
pub mod input;
pub mod math;
pub mod snapshot;
pub mod state;
pub mod tick;

pub use hash::{SimHash, hash_world};
pub use ids::{EntityId, FrameId, TeamId};
pub use input::{FaceCommand, MoveCommand, SimCommand, SimInput, SimInputSource};
pub use math::{FP_SCALE, Fp, QuantizedDir, QuantizedDirError, Vec2Fp};
pub use snapshot::{SimSnapshot, SnapshotError, restore, snapshot};
pub use state::{
    CombatState, EntityKind, MovementMode, MovementState, SimEntity, SimRngState, SimTransform,
    SimWorld,
};
pub use tick::{
    MovementConfig, SceneBounds, SimConfig, SimStepResult, StaticObstacle, StaticObstacleShape,
    StepError, step,
};

pub const SIM_CORE_SCHEMA_VERSION: u16 = 1;
