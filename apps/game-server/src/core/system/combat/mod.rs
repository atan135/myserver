pub mod buffs;
pub mod catalog;
pub mod components;
pub mod ecs;
pub mod input;
pub mod skills;

use crate::core::system::GameplaySystem;

#[allow(unused_imports)]
pub use buffs::{BuffDefinition, BuffEffect, BuffEffectType, BuffType};
#[allow(unused_imports)]
pub use catalog::{
    BuiltinCombatCatalog, CombatCatalog, CsvCombatCatalog, SharedCombatCatalog,
};
#[allow(unused_imports)]
pub use components::{
    BuffSlot, DamageFormula, EntityMeta, EntityType, Health, MoveState, MoveStateType, Position,
    SkillSlot, Stats,
};
#[allow(unused_imports)]
pub use ecs::{
    CombatCommand, CombatCommandResult, CombatEntityBlueprint, CombatEntitySnapshot, CombatEvent,
    CombatEventKind, CombatHooks, CombatSnapshot, DamageContext, EntityId, MAX_BUFFS_PER_ENTITY,
    MAX_ENTITIES, MAX_SKILLS_PER_ENTITY, NoopCombatHooks, RoomCombatEcs, SkillCastRequest,
};
#[allow(unused_imports)]
pub use input::{
    ACTION_COMBAT_APPLY_BUFF, ACTION_COMBAT_CAST_SKILL, ApplyBuffInputPayload,
    CastSkillInputPayload, CombatInputError, parse_player_input,
};
#[allow(unused_imports)]
pub use skills::{SkillDefinition, SkillEffect, SkillEffectType, SkillTargetType};

pub trait CombatSystem: GameplaySystem {
    fn tick_combat(&mut self, _frame_id: u32, _fps: u16) {}
}
