//! Simulation world state.

use crate::SIM_CORE_SCHEMA_VERSION;
use crate::combat::{BuffId, SkillId};
use crate::ids::{EntityId, FrameId, TeamId};
use crate::math::{Fp, QuantizedDir, Vec2Fp};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntityKind {
    Player,
    Npc,
    Monster,
    Projectile,
    Summon,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimTransform {
    pub pos: Vec2Fp,
    pub facing: QuantizedDir,
    pub radius: Fp,
}

impl Default for SimTransform {
    fn default() -> Self {
        Self {
            pos: Vec2Fp::zero(),
            facing: QuantizedDir::ZERO,
            radius: Fp::ZERO,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MovementMode {
    #[default]
    Idle,
    Controlled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovementState {
    pub mode: MovementMode,
    pub move_dir: QuantizedDir,
    /// Simulation units per second represented as `Fp` raw milli-units.
    pub speed_per_second: Fp,
}

impl Default for MovementState {
    fn default() -> Self {
        Self {
            mode: MovementMode::Idle,
            move_dir: QuantizedDir::ZERO,
            speed_per_second: Fp::ZERO,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombatState {
    pub hp: i32,
    pub max_hp: i32,
    pub attack: i32,
    pub defense: i32,
    pub speed: i32,
    pub crit_rate_bps: u16,
    pub crit_damage_bps: u16,
    pub skill_slots: Vec<SkillSlot>,
    pub buffs: Vec<BuffSlot>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SkillSlot {
    pub skill_id: SkillId,
    pub cooldown_remaining: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BuffSlot {
    pub buff_id: BuffId,
    pub duration_remaining: u32,
    pub interval_remaining: u32,
    pub stacks: u16,
    pub source_entity: EntityId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimEntity {
    pub id: EntityId,
    pub kind: EntityKind,
    pub owner_character_id: Option<String>,
    pub team_id: TeamId,
    pub transform: SimTransform,
    pub movement: MovementState,
    pub combat: CombatState,
    pub alive: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SimRngState {
    pub seed: u64,
    pub counter: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// Deterministic simulation world advanced by sequential frames.
///
/// The world stores only simulation state: schema, frame, RNG state, and
/// entities. Entity storage is kept sorted by `EntityId` for stable lookup and
/// hashing.
pub struct SimWorld {
    pub schema_version: u16,
    pub frame: FrameId,
    pub rng: SimRngState,
    pub entities: Vec<SimEntity>,
}

impl SimWorld {
    pub fn new(frame: FrameId, mut entities: Vec<SimEntity>) -> Result<Self, SimWorldError> {
        sort_entities_by_id(&mut entities);
        reject_duplicate_entity_ids(&entities)?;

        Ok(Self {
            schema_version: SIM_CORE_SCHEMA_VERSION,
            frame,
            rng: SimRngState::default(),
            entities,
        })
    }

    pub fn with_rng(
        frame: FrameId,
        rng: SimRngState,
        mut entities: Vec<SimEntity>,
    ) -> Result<Self, SimWorldError> {
        sort_entities_by_id(&mut entities);
        reject_duplicate_entity_ids(&entities)?;

        Ok(Self {
            schema_version: SIM_CORE_SCHEMA_VERSION,
            frame,
            rng,
            entities,
        })
    }

    pub fn sort_entities_by_id(&mut self) {
        sort_entities_by_id(&mut self.entities);
    }

    pub fn entities_sorted_by_id(&self) -> &[SimEntity] {
        &self.entities
    }

    pub fn entity(&self, id: EntityId) -> Option<&SimEntity> {
        self.entities
            .binary_search_by_key(&id, |entity| entity.id)
            .ok()
            .map(|index| &self.entities[index])
    }

    pub fn entity_mut(&mut self, id: EntityId) -> Option<&mut SimEntity> {
        self.entities
            .binary_search_by_key(&id, |entity| entity.id)
            .ok()
            .map(|index| &mut self.entities[index])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimWorldError {
    DuplicateEntityId(EntityId),
}

impl fmt::Display for SimWorldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateEntityId(id) => {
                write!(f, "duplicate simulation entity id: {}", id.raw())
            }
        }
    }
}

impl std::error::Error for SimWorldError {}

pub fn sort_entities_by_id(entities: &mut [SimEntity]) {
    entities.sort_by_key(|entity| entity.id);
}

fn reject_duplicate_entity_ids(entities: &[SimEntity]) -> Result<(), SimWorldError> {
    for pair in entities.windows(2) {
        if pair[0].id == pair[1].id {
            return Err(SimWorldError::DuplicateEntityId(pair[0].id));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entity(id: u32, kind: EntityKind) -> SimEntity {
        SimEntity {
            id: EntityId::new(id),
            kind,
            owner_character_id: Some(format!("chr_{id}")),
            team_id: TeamId::new(1),
            transform: SimTransform {
                pos: Vec2Fp::new(Fp::from_i32(id as i32), Fp::ZERO),
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

    #[test]
    fn world_new_sets_schema_frame_rng_and_sorts_entities() {
        let world = SimWorld::new(
            FrameId::new(7),
            vec![
                test_entity(300, EntityKind::Monster),
                test_entity(100, EntityKind::Player),
                test_entity(200, EntityKind::Npc),
            ],
        )
        .unwrap();

        assert_eq!(world.schema_version, SIM_CORE_SCHEMA_VERSION);
        assert_eq!(world.frame, FrameId::new(7));
        assert_eq!(world.rng, SimRngState::default());
        assert_eq!(
            world
                .entities_sorted_by_id()
                .iter()
                .map(|entity| entity.id.raw())
                .collect::<Vec<_>>(),
            vec![100, 200, 300]
        );
    }

    #[test]
    fn world_rejects_duplicate_entity_ids_after_sorting() {
        let error = SimWorld::new(
            FrameId::default(),
            vec![
                test_entity(2, EntityKind::Monster),
                test_entity(1, EntityKind::Player),
                test_entity(2, EntityKind::Summon),
            ],
        )
        .unwrap_err();

        assert_eq!(error, SimWorldError::DuplicateEntityId(EntityId::new(2)));
    }

    #[test]
    fn world_finds_entities_by_id() {
        let mut world = SimWorld::new(
            FrameId::new(1),
            vec![
                test_entity(20, EntityKind::Projectile),
                test_entity(10, EntityKind::Player),
            ],
        )
        .unwrap();

        assert_eq!(
            world.entity(EntityId::new(10)).map(|entity| entity.kind),
            Some(EntityKind::Player)
        );
        assert!(world.entity(EntityId::new(99)).is_none());

        let entity = world.entity_mut(EntityId::new(20)).unwrap();
        entity.alive = false;

        assert_eq!(
            world.entity(EntityId::new(20)).map(|entity| entity.alive),
            Some(false)
        );
    }

    #[test]
    fn all_entity_kinds_are_representable() {
        let kinds = [
            EntityKind::Player,
            EntityKind::Npc,
            EntityKind::Monster,
            EntityKind::Projectile,
            EntityKind::Summon,
        ];

        assert_eq!(kinds.len(), 5);
    }

    #[test]
    fn combat_state_stores_skill_slots_and_buff_slots() {
        let mut entity = test_entity(100, EntityKind::Player);
        entity.combat.skill_slots.push(SkillSlot {
            skill_id: SkillId::new(10),
            cooldown_remaining: 30,
        });
        entity.combat.buffs.push(BuffSlot {
            buff_id: BuffId::new(20),
            duration_remaining: 120,
            interval_remaining: 15,
            stacks: 2,
            source_entity: EntityId::new(200),
        });

        assert_eq!(entity.combat.skill_slots[0].skill_id, SkillId::new(10));
        assert_eq!(entity.combat.skill_slots[0].cooldown_remaining, 30);
        assert_eq!(entity.combat.buffs[0].buff_id, BuffId::new(20));
        assert_eq!(entity.combat.buffs[0].duration_remaining, 120);
        assert_eq!(entity.combat.buffs[0].interval_remaining, 15);
        assert_eq!(entity.combat.buffs[0].stacks, 2);
        assert_eq!(entity.combat.buffs[0].source_entity, EntityId::new(200));
    }
}
