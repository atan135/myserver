use super::query::{SceneCatalog, SceneLoadError, SceneQuery};

pub fn validate_scene_catalog(catalog: &SceneCatalog) -> Result<(), SceneLoadError> {
    for scene in catalog.scenes.values() {
        let default_spawn = catalog
            .spawn_point(scene.default_spawn_id)
            .ok_or_else(|| {
                SceneLoadError::invalid(format!(
                    "scene {} missing default spawn {}",
                    scene.id, scene.default_spawn_id
                ))
            })?;

        if default_spawn.scene_id != scene.id {
            return Err(SceneLoadError::invalid(format!(
                "scene {} default spawn {} belongs to scene {}",
                scene.id, default_spawn.id, default_spawn.scene_id
            )));
        }

        if !catalog.is_walkable(scene.id, default_spawn.x, default_spawn.y) {
            return Err(SceneLoadError::invalid(format!(
                "scene {} default spawn {} is blocked",
                scene.id, default_spawn.id
            )));
        }
    }

    Ok(())
}
