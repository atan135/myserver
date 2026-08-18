//! Immutable server-side contract for the public grassland movement world.
//!
//! CSV and grid files remain the runtime source of truth. These constants give
//! tests and movement code one named contract while the world scale migrates.

pub const GRASSLAND_01_SCENE_ID: i32 = 1;
pub const GRASSLAND_01_CODE: &str = "grassland_01";
pub const GRASSLAND_01_DEFAULT_SPAWN_ID: i32 = 1001;

pub const GRASSLAND_01_GRID_WIDTH: i32 = 40;
pub const GRASSLAND_01_GRID_HEIGHT: i32 = 40;
pub const GRASSLAND_01_CELL_SIZE_METERS: f32 = 100.0;
// Valid server positions use exclusive upper bounds: 0 <= x,y < 4000.
pub const GRASSLAND_01_WORLD_SIZE_METERS: f32 = 4000.0;
pub const GRASSLAND_01_CELL_COUNT: usize = 1600;
// The 100m cells describe a fully walkable world boundary, not obstacle detail.

pub const GRASSLAND_01_DEFAULT_SPAWN_X: f32 = 2002.0;
pub const GRASSLAND_01_DEFAULT_SPAWN_Y: f32 = 2002.0;
// The external client maps this server position to its local (2, 0, 2) origin.

// Phase 4 applies these target values to the movement_demo room policy.
pub const MOVEMENT_DEMO_TARGET_FPS: u16 = 20;
pub const MOVEMENT_DEMO_TARGET_SPEED_METERS_PER_SECOND: f32 = 4.0;
pub const MOVEMENT_DEMO_TARGET_CORRECTION_INTERVAL_FRAMES: u32 = 3;
pub const MOVEMENT_DEMO_TARGET_CORRECTION_THRESHOLD_METERS: f32 = 0.05;
pub const MOVEMENT_DEMO_TARGET_AOI_ENABLED: bool = false;
