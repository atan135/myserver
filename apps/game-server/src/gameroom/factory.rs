use crate::core::config_table::ConfigTableRuntime;
use crate::core::logic::{RoomLogic, RoomLogicFactory};

use super::{
    CombatDemoLogic, DisposableMatchLogic, MovementDemoLogic, PersistentWorldLogic,
    RobotSyncRoomLogic, SandboxLogic, TestRoomLogic, UITouchRoomLogic,
};

#[derive(Clone)]
pub struct GameRoomLogicFactory {
    config_tables: ConfigTableRuntime,
}

impl GameRoomLogicFactory {
    pub fn new(config_tables: ConfigTableRuntime) -> Self {
        Self { config_tables }
    }
}

impl RoomLogicFactory for GameRoomLogicFactory {
    fn create(&self, policy_id: &str) -> Box<dyn RoomLogic> {
        match policy_id {
            "ui_touch_room" | "UITouchRoom" => Box::new(UITouchRoomLogic::default()),
            "robot_sync_room" => Box::new(RobotSyncRoomLogic::default()),
            "combat_demo" => Box::new(CombatDemoLogic::new(self.config_tables.clone())),
            "movement_demo" => {
                let config_tables = self.config_tables.clone();
                let current = config_tables.current_snapshot();
                let movement_demo_scene_id = current
                    .scene_catalog
                    .scene_id_by_code("grassland_01")
                    .or_else(|| current.scene_catalog.scenes.keys().min().copied())
                    .unwrap_or_default();
                let policy = current.room_policies.resolve("movement_demo");
                Box::new(MovementDemoLogic::new(
                    config_tables,
                    movement_demo_scene_id,
                    policy.movement_correction_interval_frames,
                    policy.movement_correction_threshold,
                    policy.movement_aoi_radius,
                    policy.movement_aoi_enabled,
                    policy.movement_control_stop_frames,
                ))
            }
            "persistent_world" => Box::new(PersistentWorldLogic { tick_count: 0 }),
            "disposable_match" => Box::new(DisposableMatchLogic { tick_count: 0 }),
            "sandbox" => Box::new(SandboxLogic { tick_count: 0 }),
            _ => Box::new(TestRoomLogic { tick_count: 0 }),
        }
    }
}
