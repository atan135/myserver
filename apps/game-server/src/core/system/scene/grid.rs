use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use super::query::SceneLoadError;

#[derive(Debug, Clone)]
pub struct SceneGrid {
    pub version: u32,
    pub scene_code: String,
    pub width: i32,
    pub height: i32,
    pub cell_size: f32,
    pub layers: HashMap<String, Vec<u8>>,
    pub aoi_block_size: i32,
}

#[derive(Debug, Deserialize)]
struct RawSceneGrid {
    version: u32,
    scene_code: String,
    width: i32,
    height: i32,
    cell_size: f32,
    layers: HashMap<String, serde_json::Value>,
    aoi: RawSceneGridAoi,
}

#[derive(Debug, Deserialize)]
struct RawSceneGridAoi {
    block_size: i32,
}

impl SceneGrid {
    pub fn load_from_file(path: &Path) -> Result<Self, SceneLoadError> {
        let contents = std::fs::read_to_string(path)?;
        let raw: RawSceneGrid = serde_json::from_str(&contents)?;
        let expected_cell_count = expected_cell_count(raw.width, raw.height)?;
        let mut layers = HashMap::new();

        for (name, value) in raw.layers {
            let Some(array) = value.as_array() else {
                return Err(SceneLoadError::invalid(format!(
                    "scene grid layer `{name}` must use array encoding in first version"
                )));
            };

            let mut encoded = Vec::with_capacity(array.len());
            for (index, item) in array.iter().enumerate() {
                let Some(number) = item.as_u64() else {
                    return Err(SceneLoadError::invalid(format!(
                        "scene grid layer `{name}` item {index} is not an unsigned integer"
                    )));
                };
                let Ok(byte) = u8::try_from(number) else {
                    return Err(SceneLoadError::invalid(format!(
                        "scene grid layer `{name}` item {index} is out of byte range"
                    )));
                };
                encoded.push(byte);
            }

            if encoded.len() != expected_cell_count {
                return Err(SceneLoadError::invalid(format!(
                    "scene grid layer `{name}` has {} cells, expected {expected_cell_count} for {}x{}",
                    encoded.len(),
                    raw.width,
                    raw.height
                )));
            }

            layers.insert(name, encoded);
        }

        Ok(Self {
            version: raw.version,
            scene_code: raw.scene_code,
            width: raw.width,
            height: raw.height,
            cell_size: raw.cell_size,
            layers,
            aoi_block_size: raw.aoi.block_size,
        })
    }

    pub fn layer(&self, name: &str) -> Option<&[u8]> {
        self.layers.get(name).map(|layer| layer.as_slice())
    }

    pub fn cell_index(&self, cell_x: i32, cell_y: i32) -> Option<usize> {
        if cell_x < 0 || cell_y < 0 || cell_x >= self.width || cell_y >= self.height {
            return None;
        }

        let width = usize::try_from(self.width).ok()?;
        let x = usize::try_from(cell_x).ok()?;
        let y = usize::try_from(cell_y).ok()?;
        Some(y * width + x)
    }
}

fn expected_cell_count(width: i32, height: i32) -> Result<usize, SceneLoadError> {
    let width = usize::try_from(width)
        .ok()
        .filter(|width| *width > 0)
        .ok_or_else(|| SceneLoadError::invalid("scene grid width must be positive"))?;
    let height = usize::try_from(height)
        .ok()
        .filter(|height| *height > 0)
        .ok_or_else(|| SceneLoadError::invalid("scene grid height must be positive"))?;
    width
        .checked_mul(height)
        .ok_or_else(|| SceneLoadError::invalid("scene grid cell count overflows usize"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn loader_rejects_layer_length_that_does_not_match_grid_dimensions() {
        let unique = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "game-server-scene-grid-test-{}-{unique}.json",
            std::process::id()
        ));
        fs::write(
            &path,
            r#"{
                "version": 1,
                "scene_code": "test_scene",
                "width": 2,
                "height": 2,
                "cell_size": 1.0,
                "layers": { "walkable": [1, 1, 1] },
                "aoi": { "block_size": 1 }
            }"#,
        )
        .expect("grid fixture should be written");

        let error = SceneGrid::load_from_file(&path).expect_err("short layer must be rejected");
        let _ = fs::remove_file(path);

        assert!(error.to_string().contains("has 3 cells, expected 4"));
    }
}
