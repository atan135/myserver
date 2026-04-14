pub mod combat;
pub mod movement;
pub mod scene;

pub trait GameplaySystem: Send + Sync {
    fn system_name(&self) -> &'static str;
}
