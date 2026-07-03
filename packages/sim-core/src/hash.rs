//! Deterministic simulation state hashing.

use crate::ids::FrameId;
use crate::state::{EntityKind, MovementMode, SimEntity, SimWorld};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimHash {
    pub frame: FrameId,
    pub value: u64,
}

impl SimHash {
    pub const fn placeholder(frame: FrameId) -> Self {
        Self { frame, value: 0 }
    }

    pub fn from_world(world: &SimWorld) -> Self {
        hash_world(world)
    }
}

pub fn hash_world(world: &SimWorld) -> SimHash {
    let mut hasher = StableHasher::new();

    hasher.write_bytes(b"sim-core-state-hash-v1");
    hasher.write_u16(world.schema_version);
    hasher.write_u32(world.frame.raw());
    hasher.write_u64(world.rng.seed);
    hasher.write_u64(world.rng.counter);

    let mut entities = world.entities.iter().collect::<Vec<_>>();
    entities.sort_by_key(|entity| entity.id);
    hasher.write_u64(entities.len() as u64);

    for entity in entities {
        hash_entity(&mut hasher, entity);
    }

    SimHash {
        frame: world.frame,
        value: hasher.finish(),
    }
}

fn hash_entity(hasher: &mut StableHasher, entity: &SimEntity) {
    hasher.write_u32(entity.id.raw());
    hasher.write_u8(entity_kind_code(entity.kind));
    hasher.write_u16(entity.team_id.raw());
    hasher.write_optional_str(entity.owner_character_id.as_deref());
    hasher.write_bool(entity.alive);

    hasher.write_i64(entity.transform.pos.x.raw());
    hasher.write_i64(entity.transform.pos.y.raw());
    hasher.write_i16(entity.transform.facing.x());
    hasher.write_i16(entity.transform.facing.y());
    hasher.write_i64(entity.transform.radius.raw());

    hasher.write_u8(movement_mode_code(entity.movement.mode));
    hasher.write_i16(entity.movement.move_dir.x());
    hasher.write_i16(entity.movement.move_dir.y());
    hasher.write_i64(entity.movement.speed_per_second.raw());

    hasher.write_i32(entity.combat.hp);
    hasher.write_i32(entity.combat.max_hp);
    hasher.write_i32(entity.combat.attack);
    hasher.write_i32(entity.combat.defense);
    hasher.write_i32(entity.combat.speed);
    hasher.write_u16(entity.combat.crit_rate_bps);
    hasher.write_u16(entity.combat.crit_damage_bps);

    hasher.write_u64(entity.combat.skill_slots.len() as u64);
    for slot in &entity.combat.skill_slots {
        hasher.write_u32(slot.skill_id.raw());
        hasher.write_u32(slot.cooldown_remaining);
    }

    hasher.write_u64(entity.combat.buffs.len() as u64);
    for buff in &entity.combat.buffs {
        hasher.write_u32(buff.buff_id.raw());
        hasher.write_u32(buff.duration_remaining);
        hasher.write_u32(buff.interval_remaining);
        hasher.write_u16(buff.stacks);
        hasher.write_u32(buff.source_entity.raw());
    }
}

fn entity_kind_code(kind: EntityKind) -> u8 {
    match kind {
        EntityKind::Player => 0,
        EntityKind::Npc => 1,
        EntityKind::Monster => 2,
        EntityKind::Projectile => 3,
        EntityKind::Summon => 4,
    }
}

fn movement_mode_code(mode: MovementMode) -> u8 {
    match mode {
        MovementMode::Idle => 0,
        MovementMode::Controlled => 1,
    }
}

struct StableHasher {
    value: u64,
}

impl StableHasher {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    const fn new() -> Self {
        Self {
            value: Self::FNV_OFFSET_BASIS,
        }
    }

    const fn finish(self) -> u64 {
        self.value
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.value ^= u64::from(*byte);
            self.value = self.value.wrapping_mul(Self::FNV_PRIME);
        }
    }

    fn write_u8(&mut self, value: u8) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_bool(&mut self, value: bool) {
        self.write_u8(u8::from(value));
    }

    fn write_i16(&mut self, value: i16) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_u16(&mut self, value: u16) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_i32(&mut self, value: i32) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_u32(&mut self, value: u32) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_i64(&mut self, value: i64) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_u64(&mut self, value: u64) {
        self.write_bytes(&value.to_le_bytes());
    }

    fn write_optional_str(&mut self, value: Option<&str>) {
        match value {
            Some(value) => {
                self.write_bool(true);
                self.write_u64(value.len() as u64);
                self.write_bytes(value.as_bytes());
            }
            None => self.write_bool(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{EntityId, TeamId};
    use crate::math::{Fp, QuantizedDir, Vec2Fp};
    use crate::state::{CombatState, MovementState, SimRngState, SimTransform};

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
                crit_rate_bps: 500,
                crit_damage_bps: 15_000,
                skill_slots: Vec::new(),
                buffs: Vec::new(),
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
    fn same_state_hashes_the_same() {
        let world = test_world();
        let cloned = world.clone();

        assert_eq!(hash_world(&world), hash_world(&cloned));
        assert_eq!(SimHash::from_world(&world), hash_world(&world));
    }

    #[test]
    fn entity_vec_order_does_not_change_hash() {
        let world = test_world();
        let mut reordered = world.clone();
        reordered.entities.reverse();

        assert_eq!(hash_world(&world), hash_world(&reordered));
    }

    #[test]
    fn position_change_changes_hash() {
        let world = test_world();
        let mut moved = world.clone();
        moved.entities[0].transform.pos.x = Fp::from_i32(9);

        assert_ne!(hash_world(&world), hash_world(&moved));
    }
}
