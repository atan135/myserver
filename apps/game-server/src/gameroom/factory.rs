use std::sync::Arc;

use crate::core::logic::{RoomLogic, RoomLogicFactory};
use crate::core::system::scene::SceneCatalog;

use super::{
    DisposableMatchLogic, MovementDemoLogic, PersistentWorldLogic, SandboxLogic, TestRoomLogic,
};

#[derive(Clone)]
pub struct GameRoomLogicFactory {
    scene_catalog: Arc<SceneCatalog>,
    movement_demo_scene_id: i32,
}

impl GameRoomLogicFactory {
    pub fn new(scene_catalog: Arc<SceneCatalog>, movement_demo_scene_id: i32) -> Self {
        Self {
            scene_catalog,
            movement_demo_scene_id,
        }
    }
}

impl RoomLogicFactory for GameRoomLogicFactory {
    fn create(&self, policy_id: &str) -> Box<dyn RoomLogic> {
        match policy_id {
            "movement_demo" => Box::new(MovementDemoLogic::new(
                self.scene_catalog.clone(),
                self.movement_demo_scene_id,
            )),
            "persistent_world" => Box::new(PersistentWorldLogic { tick_count: 0 }),
            "disposable_match" => Box::new(DisposableMatchLogic { tick_count: 0 }),
            "sandbox" => Box::new(SandboxLogic { tick_count: 0 }),
            _ => Box::new(TestRoomLogic { tick_count: 0 }),
        }
    }
}
