use crate::core::system::GameplaySystem;

pub trait MovementSystem: GameplaySystem {
    fn tick_movement(&mut self, _frame_id: u32) {}
}
