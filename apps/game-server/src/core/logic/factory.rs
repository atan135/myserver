use std::sync::Arc;

use super::RoomLogic;

pub trait RoomLogicFactory: Send + Sync {
    fn create(&self, policy_id: &str) -> Result<Box<dyn RoomLogic>, &'static str>;
}

pub type SharedRoomLogicFactory = Arc<dyn RoomLogicFactory>;
