use crate::core::logic::{RoomLogic, RoomLogicFactory};

use super::{
    DisposableMatchLogic, PersistentWorldLogic, SandboxLogic, TestRoomLogic,
};

#[derive(Clone, Default)]
pub struct GameRoomLogicFactory;

impl RoomLogicFactory for GameRoomLogicFactory {
    fn create(&self, policy_id: &str) -> Box<dyn RoomLogic> {
        match policy_id {
            "persistent_world" => Box::new(PersistentWorldLogic { tick_count: 0 }),
            "disposable_match" => Box::new(DisposableMatchLogic { tick_count: 0 }),
            "sandbox" => Box::new(SandboxLogic { tick_count: 0 }),
            _ => Box::new(TestRoomLogic { tick_count: 0 }),
        }
    }
}
