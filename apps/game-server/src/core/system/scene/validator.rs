use super::query::{SceneCatalog, SceneLoadError, SceneQuery};

pub fn validate_scene_catalog(catalog: &SceneCatalog) -> Result<(), SceneLoadError> {
    if catalog.scenes.is_empty() {
        return Ok(());
    }

    for scene in catalog.scenes.values() {
        let default_spawn = catalog.spawn_point(scene.default_spawn_id).ok_or_else(|| {
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

        if scene.aoi_block_size <= 0 {
            return Err(SceneLoadError::invalid(format!(
                "scene {} AoiBlockSize must be positive",
                scene.id
            )));
        }
    }

    for region in catalog.regions.values() {
        let scene = catalog.scenes.get(&region.scene_id).ok_or_else(|| {
            SceneLoadError::invalid(format!(
                "region {} references missing scene {}",
                region.id, region.scene_id
            ))
        })?;
        if region.min_x > region.max_x || region.min_y > region.max_y {
            return Err(SceneLoadError::invalid(format!(
                "region {} has invalid bounds",
                region.id
            )));
        }
        let max_scene_x = scene.width as f32 * scene.cell_size;
        let max_scene_y = scene.height as f32 * scene.cell_size;
        if region.min_x < 0.0
            || region.min_y < 0.0
            || region.max_x > max_scene_x
            || region.max_y > max_scene_y
        {
            return Err(SceneLoadError::invalid(format!(
                "region {} is outside scene {} bounds",
                region.id, scene.id
            )));
        }
    }

    for interaction in catalog.interactions.values() {
        validate_scene_reference(catalog, "interaction", interaction.id, interaction.scene_id)?;
        validate_region_reference(
            catalog,
            "interaction",
            interaction.id,
            interaction.scene_id,
            interaction.region_id,
            false,
        )?;
        if interaction.radius < 0.0 {
            return Err(SceneLoadError::invalid(format!(
                "interaction {} radius must be non-negative",
                interaction.id
            )));
        }
    }

    for branch in catalog.npc_branches.values() {
        validate_scene_reference(catalog, "npc branch", branch.id, branch.scene_id)?;
        validate_region_reference(
            catalog,
            "npc branch",
            branch.id,
            branch.scene_id,
            branch.region_id,
            false,
        )?;
        if branch.npc_id.trim().is_empty() {
            return Err(SceneLoadError::invalid(format!(
                "npc branch {} missing NpcId",
                branch.id
            )));
        }
    }

    for context in catalog.contexts.values() {
        validate_scene_reference(catalog, "context", context.id, context.scene_id)?;
        validate_region_reference(
            catalog,
            "context",
            context.id,
            context.scene_id,
            context.region_id,
            true,
        )?;
    }

    Ok(())
}

fn validate_scene_reference(
    catalog: &SceneCatalog,
    label: &str,
    id: i32,
    scene_id: i32,
) -> Result<(), SceneLoadError> {
    if catalog.scenes.contains_key(&scene_id) {
        Ok(())
    } else {
        Err(SceneLoadError::invalid(format!(
            "{label} {id} references missing scene {scene_id}"
        )))
    }
}

fn validate_region_reference(
    catalog: &SceneCatalog,
    label: &str,
    id: i32,
    scene_id: i32,
    region_id: i32,
    allow_zero: bool,
) -> Result<(), SceneLoadError> {
    if allow_zero && region_id == 0 {
        return Ok(());
    }
    let Some(region) = catalog.regions.get(&region_id) else {
        return Err(SceneLoadError::invalid(format!(
            "{label} {id} references missing region {region_id}"
        )));
    };
    if region.scene_id != scene_id {
        return Err(SceneLoadError::invalid(format!(
            "{label} {id} region {region_id} belongs to scene {}",
            region.scene_id
        )));
    }
    Ok(())
}
