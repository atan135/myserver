use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::Path;

use crate::gameconfig::ConfigTables;

use super::grid::SceneGrid;
use super::validator::validate_scene_catalog;

#[derive(Debug, Clone)]
pub struct SceneDefinition {
    pub id: i32,
    pub code: String,
    pub name: String,
    pub grid_file: String,
    pub width: i32,
    pub height: i32,
    pub cell_size: f32,
    pub aoi_block_size: i32,
    pub default_spawn_id: i32,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SceneSpawnPointDefinition {
    pub id: i32,
    pub scene_id: i32,
    pub code: String,
    pub spawn_type: String,
    pub x: f32,
    pub y: f32,
    pub dir_x: f32,
    pub dir_y: f32,
    pub radius: f32,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct ClampPositionResult {
    pub x: f32,
    pub y: f32,
    pub blocked: bool,
}

#[derive(Debug, Clone)]
pub struct SceneCatalog {
    pub scenes: HashMap<i32, SceneDefinition>,
    pub spawns: HashMap<i32, SceneSpawnPointDefinition>,
    grids_by_scene_id: HashMap<i32, SceneGrid>,
    scene_code_to_id: HashMap<String, i32>,
}

#[derive(Debug)]
pub struct SceneLoadError {
    message: String,
}

pub trait SceneQuery: Send + Sync {
    fn scene(&self, scene_id: i32) -> Option<&SceneDefinition>;
    fn spawn_point(&self, spawn_id: i32) -> Option<&SceneSpawnPointDefinition>;
    fn is_walkable(&self, scene_id: i32, world_x: f32, world_y: f32) -> bool;
    fn clamp_position(
        &self,
        scene_id: i32,
        from_x: f32,
        from_y: f32,
        to_x: f32,
        to_y: f32,
    ) -> ClampPositionResult;
}

impl SceneLoadError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for SceneLoadError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SceneLoadError {}

impl From<std::io::Error> for SceneLoadError {
    fn from(value: std::io::Error) -> Self {
        Self::invalid(value.to_string())
    }
}

impl From<serde_json::Error> for SceneLoadError {
    fn from(value: serde_json::Error) -> Self {
        Self::invalid(value.to_string())
    }
}

impl SceneCatalog {
    pub fn load_from_dir(scene_dir: &Path, tables: &ConfigTables) -> Result<Self, SceneLoadError> {
        let mut scenes = HashMap::new();
        let mut spawns = HashMap::new();
        let mut grids_by_scene_id = HashMap::new();
        let mut scene_code_to_id = HashMap::new();

        for row in &tables.scenetable.rows {
            let code = tables
                .scenetable
                .resolve_string(row.code)
                .unwrap_or_default()
                .to_string();
            let name = tables
                .scenetable
                .resolve_string(row.name)
                .unwrap_or_default()
                .to_string();
            let grid_file = tables
                .scenetable
                .resolve_string(row.gridfile)
                .unwrap_or_default()
                .to_string();
            let tags = row
                .tags
                .iter()
                .filter_map(|tag| tables.scenetable.resolve_string(*tag))
                .map(str::to_string)
                .collect::<Vec<_>>();
            let grid = SceneGrid::load_from_file(&scene_dir.join(&grid_file))?;

            if grid.scene_code != code {
                return Err(SceneLoadError::invalid(format!(
                    "scene {} code mismatch between csv `{}` and grid `{}`",
                    row.id, code, grid.scene_code
                )));
            }

            if grid.width != row.width || grid.height != row.height {
                return Err(SceneLoadError::invalid(format!(
                    "scene {} size mismatch between csv {}x{} and grid {}x{}",
                    row.id, row.width, row.height, grid.width, grid.height
                )));
            }

            if (grid.cell_size - row.cellsize).abs() > f32::EPSILON {
                return Err(SceneLoadError::invalid(format!(
                    "scene {} cell size mismatch between csv {} and grid {}",
                    row.id, row.cellsize, grid.cell_size
                )));
            }

            scenes.insert(
                row.id,
                SceneDefinition {
                    id: row.id,
                    code: code.clone(),
                    name,
                    grid_file,
                    width: row.width,
                    height: row.height,
                    cell_size: row.cellsize,
                    aoi_block_size: row.aoiblocksize,
                    default_spawn_id: row.defaultspawnid,
                    tags,
                },
            );
            grids_by_scene_id.insert(row.id, grid);
            scene_code_to_id.insert(code, row.id);
        }

        for row in &tables.scenespawnpoint.rows {
            let code = tables
                .scenespawnpoint
                .resolve_string(row.code)
                .unwrap_or_default()
                .to_string();
            let spawn_type = tables
                .scenespawnpoint
                .resolve_string(row.spawntype)
                .unwrap_or_default()
                .to_string();
            let tags = row
                .tags
                .iter()
                .filter_map(|tag| tables.scenespawnpoint.resolve_string(*tag))
                .map(str::to_string)
                .collect::<Vec<_>>();

            spawns.insert(
                row.id,
                SceneSpawnPointDefinition {
                    id: row.id,
                    scene_id: row.sceneid,
                    code,
                    spawn_type,
                    x: row.x,
                    y: row.y,
                    dir_x: row.dirx,
                    dir_y: row.diry,
                    radius: row.radius,
                    tags,
                },
            );
        }

        let catalog = Self {
            scenes,
            spawns,
            grids_by_scene_id,
            scene_code_to_id,
        };
        validate_scene_catalog(&catalog)?;
        Ok(catalog)
    }

    pub fn scene_id_by_code(&self, code: &str) -> Option<i32> {
        self.scene_code_to_id.get(code).copied()
    }

    fn grid(&self, scene_id: i32) -> Option<&SceneGrid> {
        self.grids_by_scene_id.get(&scene_id)
    }

    fn layer_value(&self, scene_id: i32, layer_name: &str, cell_x: i32, cell_y: i32) -> Option<u8> {
        let grid = self.grid(scene_id)?;
        let index = grid.cell_index(cell_x, cell_y)?;
        let layer = grid.layer(layer_name)?;
        layer.get(index).copied()
    }

    fn world_to_cell(&self, scene_id: i32, world_x: f32, world_y: f32) -> Option<(i32, i32)> {
        let scene = self.scene(scene_id)?;
        let cell_x = (world_x / scene.cell_size).floor() as i32;
        let cell_y = (world_y / scene.cell_size).floor() as i32;
        Some((cell_x, cell_y))
    }
}

impl SceneQuery for SceneCatalog {
    fn scene(&self, scene_id: i32) -> Option<&SceneDefinition> {
        self.scenes.get(&scene_id)
    }

    fn spawn_point(&self, spawn_id: i32) -> Option<&SceneSpawnPointDefinition> {
        self.spawns.get(&spawn_id)
    }

    fn is_walkable(&self, scene_id: i32, world_x: f32, world_y: f32) -> bool {
        let Some((cell_x, cell_y)) = self.world_to_cell(scene_id, world_x, world_y) else {
            return false;
        };

        let walkable = self.layer_value(scene_id, "walkable", cell_x, cell_y).unwrap_or(0);
        let blocked = self.layer_value(scene_id, "block", cell_x, cell_y).unwrap_or(1);
        walkable == 1 && blocked == 0
    }

    fn clamp_position(
        &self,
        scene_id: i32,
        from_x: f32,
        from_y: f32,
        to_x: f32,
        to_y: f32,
    ) -> ClampPositionResult {
        if self.is_walkable(scene_id, to_x, to_y) {
            return ClampPositionResult {
                x: to_x,
                y: to_y,
                blocked: false,
            };
        }

        if self.is_walkable(scene_id, to_x, from_y) {
            return ClampPositionResult {
                x: to_x,
                y: from_y,
                blocked: true,
            };
        }

        if self.is_walkable(scene_id, from_x, to_y) {
            return ClampPositionResult {
                x: from_x,
                y: to_y,
                blocked: true,
            };
        }

        ClampPositionResult {
            x: from_x,
            y: from_y,
            blocked: true,
        }
    }
}
