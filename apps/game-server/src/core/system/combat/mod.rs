use crate::core::system::GameplaySystem;

pub trait CombatSystem: GameplaySystem {
    fn tick_combat(&mut self, _frame_id: u32) {}
}
