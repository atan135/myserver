use std::sync::Arc;

use super::RoomLogic;

pub trait RoomLogicFactory: Send + Sync {
    fn create(&self, policy_id: &str) -> Box<dyn RoomLogic>;
}

pub type SharedRoomLogicFactory = Arc<dyn RoomLogicFactory>;
