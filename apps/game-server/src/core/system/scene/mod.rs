pub mod grid;
pub mod query;
pub mod validator;

use crate::core::system::GameplaySystem;

pub use query::{SceneCatalog, SceneQuery};

pub trait SceneSystem: GameplaySystem {
    fn scene_catalog(&self) -> &SceneCatalog;
}
