use crate::core::config_table::ConfigTableRuntime;
use crate::core::logic::{RoomLogic, RoomLogicFactory};

use super::{
    CombatDemoLogic, DisposableMatchLogic, LockstepSimDemoLogic, MovementDemoLogic,
    PersistentWorldLogic, RobotSyncRoomLogic, SandboxLogic, UITouchRoomLogic,
};
use super::test_room::TestRoomLogic;

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
    fn create(&self, policy_id: &str) -> Result<Box<dyn RoomLogic>, &'static str> {
        match policy_id {
            "ui_touch_room" | "UITouchRoom" => Ok(Box::new(UITouchRoomLogic::default())),
            "robot_sync_room" => Ok(Box::new(RobotSyncRoomLogic::default())),
            "combat_demo" => Ok(Box::new(CombatDemoLogic::new(self.config_tables.clone()))),
            "lockstep_sim_demo" => Ok(Box::new(LockstepSimDemoLogic::new(
                self.config_tables.current_snapshot().version,
            ))),
            "movement_demo" => {
                let config_tables = self.config_tables.clone();
                let current = config_tables.current_snapshot();
                let movement_demo_scene_id = movement_demo_scene_id(
                    current.scene_catalog.scene_id_by_code("grassland_01"),
                )?;
                let policy = current
                    .room_policies
                    .resolve("movement_demo")
                    .ok_or("ROOM_POLICY_UNSUPPORTED")?;
                Ok(Box::new(MovementDemoLogic::new(
                    config_tables,
                    movement_demo_scene_id,
                    policy.movement_correction_interval_frames,
                    policy.movement_correction_threshold,
                    policy.movement_aoi_radius,
                    policy.movement_aoi_enabled,
                    policy.movement_control_stop_frames,
                )))
            }
            "persistent_world" => Ok(Box::new(PersistentWorldLogic { tick_count: 0 })),
            "disposable_match" => Ok(Box::new(DisposableMatchLogic { tick_count: 0 })),
            "sandbox" => Ok(Box::new(SandboxLogic { tick_count: 0 })),
            "default_match" => Ok(Box::new(TestRoomLogic { tick_count: 0 })),
            _ => Err("ROOM_POLICY_UNSUPPORTED"),
        }
    }
}

fn movement_demo_scene_id(scene_id: Option<i32>) -> Result<i32, &'static str> {
    scene_id.ok_or("ROOM_SCENE_UNAVAILABLE")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn config_tables() -> ConfigTableRuntime {
        ConfigTableRuntime::load_with_scene_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("csv"),
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("scene"),
        )
        .expect("game-server csv fixture should load")
    }

    #[test]
    fn movement_demo_requires_grassland_scene_instead_of_falling_back() {
        assert_eq!(movement_demo_scene_id(None), Err("ROOM_SCENE_UNAVAILABLE"));
    }

    #[test]
    fn unknown_room_logic_is_rejected() {
        let factory = GameRoomLogicFactory::new(config_tables());

        assert_eq!(
            factory.create("unknown_policy").err(),
            Some("ROOM_POLICY_UNSUPPORTED")
        );
    }
}
