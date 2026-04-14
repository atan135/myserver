use crate::core::system::GameplaySystem;

pub trait SceneSystem: GameplaySystem {
    fn validate_spawn(&self, _scene_id: &str, _spawn_id: &str) -> bool {
        true
    }
}
