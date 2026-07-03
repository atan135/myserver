//! Serializable simulation snapshots.
//!
//! A snapshot captures only deterministic simulation state needed to resume the
//! core lockstep world. It intentionally excludes rendering state, network
//! connection state, handles, and external resource paths.

use crate::SIM_CORE_SCHEMA_VERSION;
use crate::hash::{SimHash, hash_world};
use crate::ids::FrameId;
use crate::state::SimWorld;
use crate::tick::SimConfig;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimSnapshot {
    pub schema_version: u16,
    pub frame: FrameId,
    pub world: SimWorld,
    pub hash: SimHash,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    UnsupportedSchemaVersion {
        snapshot_version: u16,
        supported_version: u16,
    },
    WorldSchemaMismatch {
        snapshot_version: u16,
        world_version: u16,
    },
    FrameMismatch {
        snapshot_frame: FrameId,
        world_frame: FrameId,
    },
    HashMismatch {
        expected: SimHash,
        actual: SimHash,
    },
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSchemaVersion {
                snapshot_version,
                supported_version,
            } => write!(
                f,
                "unsupported simulation snapshot schema version: got {snapshot_version}, support {supported_version}"
            ),
            Self::WorldSchemaMismatch {
                snapshot_version,
                world_version,
            } => write!(
                f,
                "simulation snapshot world schema mismatch: snapshot {snapshot_version}, world {world_version}"
            ),
            Self::FrameMismatch {
                snapshot_frame,
                world_frame,
            } => write!(
                f,
                "simulation snapshot frame mismatch: snapshot {}, world {}",
                snapshot_frame.raw(),
                world_frame.raw()
            ),
            Self::HashMismatch { expected, actual } => write!(
                f,
                "simulation snapshot hash mismatch: expected frame {} value {}, got frame {} value {}",
                expected.frame.raw(),
                expected.value,
                actual.frame.raw(),
                actual.value
            ),
        }
    }
}

impl std::error::Error for SnapshotError {}

pub fn snapshot(world: &SimWorld, _config: &SimConfig) -> SimSnapshot {
    // P0 snapshots bind to a config context through the call site, but do not
    // serialize config fields until a config version/hash is introduced.
    SimSnapshot {
        schema_version: SIM_CORE_SCHEMA_VERSION,
        frame: world.frame,
        world: world.clone(),
        hash: hash_world(world),
    }
}

pub fn restore(snapshot: &SimSnapshot) -> Result<SimWorld, SnapshotError> {
    if snapshot.schema_version != SIM_CORE_SCHEMA_VERSION {
        return Err(SnapshotError::UnsupportedSchemaVersion {
            snapshot_version: snapshot.schema_version,
            supported_version: SIM_CORE_SCHEMA_VERSION,
        });
    }

    if snapshot.world.schema_version != snapshot.schema_version {
        return Err(SnapshotError::WorldSchemaMismatch {
            snapshot_version: snapshot.schema_version,
            world_version: snapshot.world.schema_version,
        });
    }

    if snapshot.frame != snapshot.world.frame {
        return Err(SnapshotError::FrameMismatch {
            snapshot_frame: snapshot.frame,
            world_frame: snapshot.world.frame,
        });
    }

    let expected = hash_world(&snapshot.world);
    if snapshot.hash != expected {
        return Err(SnapshotError::HashMismatch {
            expected,
            actual: snapshot.hash,
        });
    }

    Ok(snapshot.world.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{EntityId, TeamId};
    use crate::math::{Fp, QuantizedDir, Vec2Fp};
    use crate::state::{
        CombatState, EntityKind, MovementMode, MovementState, SimEntity, SimRngState, SimTransform,
    };
    use crate::tick::{MovementConfig, SceneBounds};

    fn test_config() -> SimConfig {
        SimConfig {
            movement: MovementConfig {
                tick_rate: 60,
                default_speed_per_second: Fp::from_i32(6),
                max_speed_per_second: Fp::from_i32(10),
                bounds: SceneBounds {
                    min: Vec2Fp::new(Fp::from_i32(-10), Fp::from_i32(-10)),
                    max: Vec2Fp::new(Fp::from_i32(10), Fp::from_i32(10)),
                },
                static_obstacles: Vec::new(),
            },
        }
    }

    fn test_entity(id: u32, pos: Vec2Fp) -> SimEntity {
        SimEntity {
            id: EntityId::new(id),
            kind: EntityKind::Player,
            owner_character_id: Some(format!("chr_{id}")),
            team_id: TeamId::new(1),
            transform: SimTransform {
                pos,
                facing: QuantizedDir::RIGHT,
                radius: Fp::from_milli(500),
            },
            movement: MovementState {
                mode: MovementMode::Controlled,
                move_dir: QuantizedDir::RIGHT,
                speed_per_second: Fp::from_i32(6),
            },
            combat: CombatState {
                hp: 100,
                max_hp: 100,
                attack: 10,
                defense: 3,
                speed: 6,
            },
            alive: true,
        }
    }

    fn test_world() -> SimWorld {
        SimWorld::with_rng(
            FrameId::new(7),
            SimRngState {
                seed: 11,
                counter: 22,
            },
            vec![
                test_entity(200, Vec2Fp::new(Fp::from_i32(2), Fp::ZERO)),
                test_entity(100, Vec2Fp::new(Fp::from_i32(1), Fp::ZERO)),
            ],
        )
        .unwrap()
    }

    #[test]
    fn snapshot_roundtrips_through_json_and_restores_world() {
        let world = test_world();
        let snapshot = snapshot(&world, &test_config());

        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: SimSnapshot = serde_json::from_str(&encoded).unwrap();
        let restored = restore(&decoded).unwrap();

        assert_eq!(decoded.schema_version, SIM_CORE_SCHEMA_VERSION);
        assert_eq!(decoded.frame, world.frame);
        assert_eq!(decoded.hash, hash_world(&world));
        assert_eq!(restored, world);
    }

    #[test]
    fn restore_rejects_unsupported_snapshot_schema_version() {
        let world = test_world();
        let mut snapshot = snapshot(&world, &test_config());
        snapshot.schema_version = SIM_CORE_SCHEMA_VERSION + 1;

        let error = restore(&snapshot).unwrap_err();

        assert_eq!(
            error,
            SnapshotError::UnsupportedSchemaVersion {
                snapshot_version: SIM_CORE_SCHEMA_VERSION + 1,
                supported_version: SIM_CORE_SCHEMA_VERSION,
            }
        );
    }

    #[test]
    fn restore_rejects_world_schema_mismatch() {
        let world = test_world();
        let mut snapshot = snapshot(&world, &test_config());
        snapshot.world.schema_version = SIM_CORE_SCHEMA_VERSION + 1;
        snapshot.hash = hash_world(&snapshot.world);

        let error = restore(&snapshot).unwrap_err();

        assert_eq!(
            error,
            SnapshotError::WorldSchemaMismatch {
                snapshot_version: SIM_CORE_SCHEMA_VERSION,
                world_version: SIM_CORE_SCHEMA_VERSION + 1,
            }
        );
    }

    #[test]
    fn restore_rejects_hash_mismatch() {
        let world = test_world();
        let mut snapshot = snapshot(&world, &test_config());
        snapshot.world.entities[0].combat.hp -= 1;

        let error = restore(&snapshot).unwrap_err();

        assert_eq!(
            error,
            SnapshotError::HashMismatch {
                expected: hash_world(&snapshot.world),
                actual: hash_world(&world),
            }
        );
    }
}
