use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::Path;

use crate::core::character_element::{CharacterElements, ElementValues};
use crate::gameconfig::ConfigTables;

use super::condition::{
    SceneCharacterState, SceneCondition, SceneConditionError, SceneConditionOutcome,
    SceneConditionStatus,
};
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

#[derive(Debug, Clone)]
pub struct SceneRegionDefinition {
    pub id: i32,
    pub scene_id: i32,
    pub code: String,
    pub region_type: String,
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
    pub tags: Vec<String>,
    pub entry_condition: SceneCondition,
    pub prompt_key: String,
}

#[derive(Debug, Clone)]
pub struct SceneInteractionDefinition {
    pub id: i32,
    pub scene_id: i32,
    pub code: String,
    pub interaction_type: String,
    pub region_id: i32,
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    pub condition: SceneCondition,
    pub context_effects: Vec<SceneContextElementEffect>,
    pub prompt_key: String,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct SceneNpcBranchDefinition {
    pub id: i32,
    pub scene_id: i32,
    pub npc_id: String,
    pub code: String,
    pub region_id: i32,
    pub branch_type: String,
    pub condition: SceneCondition,
    pub attitude: String,
    pub service_flags: Vec<String>,
    pub prompt_key: String,
    pub priority: i32,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct SceneContextDefinition {
    pub id: i32,
    pub scene_id: i32,
    pub code: String,
    pub context_type: String,
    pub region_id: i32,
    pub condition: SceneCondition,
    pub element_effects: Vec<SceneContextElementEffect>,
    pub priority: i32,
    pub enabled: bool,
    pub prompt_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneContextElementEffect {
    pub target: SceneElementTarget,
    pub element: SceneElement,
    pub delta: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneElementTarget {
    Affinity,
    Mastery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneElement {
    Earth,
    Fire,
    Water,
    Wind,
}

#[derive(Debug, Clone)]
pub struct SceneAccessEvaluation<'a> {
    pub definition_id: i32,
    pub scene_id: i32,
    pub code: &'a str,
    pub prompt_key: &'a str,
    pub outcome: SceneConditionOutcome,
}

#[derive(Debug, Clone)]
pub struct SceneNpcBranchEvaluation<'a> {
    pub branch: &'a SceneNpcBranchDefinition,
    pub outcome: SceneConditionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveSceneElements {
    pub elements: CharacterElements,
    pub applied_context_codes: Vec<String>,
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
    pub regions: HashMap<i32, SceneRegionDefinition>,
    pub interactions: HashMap<i32, SceneInteractionDefinition>,
    pub npc_branches: HashMap<i32, SceneNpcBranchDefinition>,
    pub contexts: HashMap<i32, SceneContextDefinition>,
    grids_by_scene_id: HashMap<i32, SceneGrid>,
    scene_code_to_id: HashMap<String, i32>,
    regions_by_scene_id: HashMap<i32, Vec<i32>>,
    interactions_by_scene_id: HashMap<i32, Vec<i32>>,
    npc_branches_by_scene_id: HashMap<i32, Vec<i32>>,
    contexts_by_scene_id: HashMap<i32, Vec<i32>>,
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

impl From<SceneConditionError> for SceneLoadError {
    fn from(value: SceneConditionError) -> Self {
        Self::invalid(value.to_string())
    }
}

impl SceneCatalog {
    pub fn load_from_dir(scene_dir: &Path, tables: &ConfigTables) -> Result<Self, SceneLoadError> {
        let mut scenes = HashMap::new();
        let mut spawns = HashMap::new();
        let mut regions = HashMap::new();
        let mut interactions = HashMap::new();
        let mut npc_branches = HashMap::new();
        let mut contexts = HashMap::new();
        let mut grids_by_scene_id = HashMap::new();
        let mut scene_code_to_id = HashMap::new();
        let mut regions_by_scene_id: HashMap<i32, Vec<i32>> = HashMap::new();
        let mut interactions_by_scene_id: HashMap<i32, Vec<i32>> = HashMap::new();
        let mut npc_branches_by_scene_id: HashMap<i32, Vec<i32>> = HashMap::new();
        let mut contexts_by_scene_id: HashMap<i32, Vec<i32>> = HashMap::new();

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

        for row in &tables.sceneregion.rows {
            let code = tables
                .sceneregion
                .resolve_string(row.code)
                .unwrap_or_default()
                .to_string();
            let region_type = tables
                .sceneregion
                .resolve_string(row.regiontype)
                .unwrap_or_default()
                .to_string();
            let tags = row
                .tags
                .iter()
                .filter_map(|tag| tables.sceneregion.resolve_string(*tag))
                .map(str::to_string)
                .collect::<Vec<_>>();
            let entry_conditions = tables
                .sceneregion
                .resolve_string(row.entryconditions)
                .unwrap_or_default();
            let prompt_key = tables
                .sceneregion
                .resolve_string(row.promptkey)
                .unwrap_or_default()
                .to_string();
            let entry_condition = SceneCondition::parse(entry_conditions).map_err(|error| {
                SceneLoadError::invalid(format!("SceneRegion {} EntryConditions: {error}", row.id))
            })?;

            regions_by_scene_id
                .entry(row.sceneid)
                .or_default()
                .push(row.id);
            regions.insert(
                row.id,
                SceneRegionDefinition {
                    id: row.id,
                    scene_id: row.sceneid,
                    code,
                    region_type,
                    min_x: row.minx,
                    min_y: row.miny,
                    max_x: row.maxx,
                    max_y: row.maxy,
                    tags,
                    entry_condition,
                    prompt_key,
                },
            );
        }

        for row in &tables.sceneinteraction.rows {
            let code = tables
                .sceneinteraction
                .resolve_string(row.code)
                .unwrap_or_default()
                .to_string();
            let interaction_type = tables
                .sceneinteraction
                .resolve_string(row.interactiontype)
                .unwrap_or_default()
                .to_string();
            let conditions = tables
                .sceneinteraction
                .resolve_string(row.conditions)
                .unwrap_or_default();
            let context_effects = tables
                .sceneinteraction
                .resolve_string(row.contexteffects)
                .unwrap_or_default();
            let prompt_key = tables
                .sceneinteraction
                .resolve_string(row.promptkey)
                .unwrap_or_default()
                .to_string();
            let condition = SceneCondition::parse(conditions).map_err(|error| {
                SceneLoadError::invalid(format!("SceneInteraction {} Conditions: {error}", row.id))
            })?;
            let context_effects =
                parse_context_element_effects(context_effects).map_err(|error| {
                    SceneLoadError::invalid(format!(
                        "SceneInteraction {} ContextEffects: {error}",
                        row.id
                    ))
                })?;

            interactions_by_scene_id
                .entry(row.sceneid)
                .or_default()
                .push(row.id);
            interactions.insert(
                row.id,
                SceneInteractionDefinition {
                    id: row.id,
                    scene_id: row.sceneid,
                    code,
                    interaction_type,
                    region_id: row.regionid,
                    x: row.x,
                    y: row.y,
                    radius: row.radius,
                    condition,
                    context_effects,
                    prompt_key,
                    enabled: row.enabled != 0,
                },
            );
        }

        for row in &tables.scenenpc.rows {
            let npc_id = tables
                .scenenpc
                .resolve_string(row.npcid)
                .unwrap_or_default()
                .to_string();
            let code = tables
                .scenenpc
                .resolve_string(row.code)
                .unwrap_or_default()
                .to_string();
            let branch_type = tables
                .scenenpc
                .resolve_string(row.branchtype)
                .unwrap_or_default()
                .to_string();
            let conditions = tables
                .scenenpc
                .resolve_string(row.conditions)
                .unwrap_or_default();
            let attitude = tables
                .scenenpc
                .resolve_string(row.attitude)
                .unwrap_or_default()
                .to_string();
            let service_flags = row
                .serviceflags
                .iter()
                .filter_map(|key| tables.scenenpc.resolve_string(*key))
                .map(str::to_string)
                .collect::<Vec<_>>();
            let prompt_key = tables
                .scenenpc
                .resolve_string(row.promptkey)
                .unwrap_or_default()
                .to_string();
            let condition = SceneCondition::parse(conditions).map_err(|error| {
                SceneLoadError::invalid(format!("SceneNpc {} Conditions: {error}", row.id))
            })?;

            npc_branches_by_scene_id
                .entry(row.sceneid)
                .or_default()
                .push(row.id);
            npc_branches.insert(
                row.id,
                SceneNpcBranchDefinition {
                    id: row.id,
                    scene_id: row.sceneid,
                    npc_id,
                    code,
                    region_id: row.regionid,
                    branch_type,
                    condition,
                    attitude,
                    service_flags,
                    prompt_key,
                    priority: row.priority,
                    enabled: row.enabled != 0,
                },
            );
        }

        for row in &tables.scenecontext.rows {
            let code = tables
                .scenecontext
                .resolve_string(row.code)
                .unwrap_or_default()
                .to_string();
            let context_type = tables
                .scenecontext
                .resolve_string(row.contexttype)
                .unwrap_or_default()
                .to_string();
            let conditions = tables
                .scenecontext
                .resolve_string(row.conditions)
                .unwrap_or_default();
            let element_effects = tables
                .scenecontext
                .resolve_string(row.elementeffects)
                .unwrap_or_default();
            let prompt_key = tables
                .scenecontext
                .resolve_string(row.promptkey)
                .unwrap_or_default()
                .to_string();
            let condition = SceneCondition::parse(conditions).map_err(|error| {
                SceneLoadError::invalid(format!("SceneContext {} Conditions: {error}", row.id))
            })?;
            let element_effects =
                parse_context_element_effects(element_effects).map_err(|error| {
                    SceneLoadError::invalid(format!(
                        "SceneContext {} ElementEffects: {error}",
                        row.id
                    ))
                })?;

            contexts_by_scene_id
                .entry(row.sceneid)
                .or_default()
                .push(row.id);
            contexts.insert(
                row.id,
                SceneContextDefinition {
                    id: row.id,
                    scene_id: row.sceneid,
                    code,
                    context_type,
                    region_id: row.regionid,
                    condition,
                    element_effects,
                    priority: row.priority,
                    enabled: row.enabled != 0,
                    prompt_key,
                },
            );
        }

        for ids in npc_branches_by_scene_id.values_mut() {
            ids.sort_by_key(|id| {
                npc_branches
                    .get(id)
                    .map(|branch| std::cmp::Reverse(branch.priority))
            });
        }
        for ids in contexts_by_scene_id.values_mut() {
            ids.sort_by_key(|id| contexts.get(id).map(|context| context.priority));
        }

        let catalog = Self {
            scenes,
            spawns,
            regions,
            interactions,
            npc_branches,
            contexts,
            grids_by_scene_id,
            scene_code_to_id,
            regions_by_scene_id,
            interactions_by_scene_id,
            npc_branches_by_scene_id,
            contexts_by_scene_id,
        };
        validate_scene_catalog(&catalog)?;
        Ok(catalog)
    }

    pub fn scene_id_by_code(&self, code: &str) -> Option<i32> {
        self.scene_code_to_id.get(code).copied()
    }

    pub fn region(&self, region_id: i32) -> Option<&SceneRegionDefinition> {
        self.regions.get(&region_id)
    }

    pub fn interaction(&self, interaction_id: i32) -> Option<&SceneInteractionDefinition> {
        self.interactions.get(&interaction_id)
    }

    pub fn context(&self, context_id: i32) -> Option<&SceneContextDefinition> {
        self.contexts.get(&context_id)
    }

    pub fn regions_in_scene(&self, scene_id: i32) -> Vec<&SceneRegionDefinition> {
        self.regions_by_scene_id
            .get(&scene_id)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.regions.get(id))
            .collect()
    }

    pub fn interactions_in_scene(&self, scene_id: i32) -> Vec<&SceneInteractionDefinition> {
        self.interactions_by_scene_id
            .get(&scene_id)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.interactions.get(id))
            .collect()
    }

    pub fn npc_branches_in_scene(&self, scene_id: i32) -> Vec<&SceneNpcBranchDefinition> {
        self.npc_branches_by_scene_id
            .get(&scene_id)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.npc_branches.get(id))
            .collect()
    }

    pub fn contexts_in_scene(&self, scene_id: i32) -> Vec<&SceneContextDefinition> {
        self.contexts_by_scene_id
            .get(&scene_id)
            .into_iter()
            .flat_map(|ids| ids.iter())
            .filter_map(|id| self.contexts.get(id))
            .collect()
    }

    pub fn regions_at_position(
        &self,
        scene_id: i32,
        world_x: f32,
        world_y: f32,
    ) -> Vec<&SceneRegionDefinition> {
        self.regions_in_scene(scene_id)
            .into_iter()
            .filter(|region| region.contains(world_x, world_y))
            .collect()
    }

    pub fn evaluate_region_entry<'a>(
        &'a self,
        region_id: i32,
        character: &SceneCharacterState,
    ) -> Option<SceneAccessEvaluation<'a>> {
        let region = self.region(region_id)?;
        Some(SceneAccessEvaluation {
            definition_id: region.id,
            scene_id: region.scene_id,
            code: &region.code,
            prompt_key: &region.prompt_key,
            outcome: region.entry_condition.evaluate(character),
        })
    }

    pub fn evaluate_interaction<'a>(
        &'a self,
        interaction_id: i32,
        character: &SceneCharacterState,
    ) -> Option<SceneAccessEvaluation<'a>> {
        let interaction = self.interaction(interaction_id)?;
        if !interaction.enabled {
            return Some(SceneAccessEvaluation {
                definition_id: interaction.id,
                scene_id: interaction.scene_id,
                code: &interaction.code,
                prompt_key: &interaction.prompt_key,
                outcome: SceneConditionOutcome::not_matched("interaction disabled"),
            });
        }
        Some(SceneAccessEvaluation {
            definition_id: interaction.id,
            scene_id: interaction.scene_id,
            code: &interaction.code,
            prompt_key: &interaction.prompt_key,
            outcome: interaction.condition.evaluate(character),
        })
    }

    pub fn resolve_npc_branch<'a>(
        &'a self,
        scene_id: i32,
        npc_id: &str,
        character: &SceneCharacterState,
    ) -> Option<SceneNpcBranchEvaluation<'a>> {
        let mut first_not_matched = None;
        for branch in self.npc_branches_in_scene(scene_id) {
            if !branch.enabled || !branch.npc_id.eq_ignore_ascii_case(npc_id) {
                continue;
            }
            let outcome = branch.condition.evaluate(character);
            match outcome.status {
                SceneConditionStatus::Matched => {
                    return Some(SceneNpcBranchEvaluation { branch, outcome });
                }
                SceneConditionStatus::NotMatched => {
                    if first_not_matched.is_none() {
                        first_not_matched = Some(SceneNpcBranchEvaluation { branch, outcome });
                    }
                }
                SceneConditionStatus::Unsupported => {
                    return Some(SceneNpcBranchEvaluation { branch, outcome });
                }
            }
        }
        first_not_matched
    }

    pub fn effective_elements_with_context(
        &self,
        scene_id: i32,
        region_id: Option<i32>,
        character: &SceneCharacterState,
        active_context_codes: &[String],
    ) -> EffectiveSceneElements {
        let mut effective = character.elements.clone();
        let mut applied_context_codes = Vec::new();
        let active_context_codes = active_context_codes
            .iter()
            .map(|code| code.as_str())
            .collect::<std::collections::HashSet<_>>();

        for context in self.contexts_in_scene(scene_id) {
            if !context.enabled {
                continue;
            }
            if !active_context_codes.contains(context.code.as_str()) {
                continue;
            }
            if context.region_id != 0 && Some(context.region_id) != region_id {
                continue;
            }
            let outcome = context.condition.evaluate(character);
            if !outcome.matched_bool() {
                continue;
            }
            apply_context_effects(&mut effective, &context.element_effects);
            applied_context_codes.push(context.code.clone());
        }

        EffectiveSceneElements {
            elements: effective,
            applied_context_codes,
        }
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

impl SceneRegionDefinition {
    pub fn contains(&self, world_x: f32, world_y: f32) -> bool {
        world_x >= self.min_x
            && world_x <= self.max_x
            && world_y >= self.min_y
            && world_y <= self.max_y
    }
}

fn parse_context_element_effects(
    raw: &str,
) -> Result<Vec<SceneContextElementEffect>, SceneLoadError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let value: serde_json::Value = serde_json::from_str(trimmed)?;
    let Some(values) = value.as_array() else {
        return Err(SceneLoadError::invalid(
            "element effects must be a JSON array",
        ));
    };

    let mut effects = Vec::with_capacity(values.len());
    for (index, item) in values.iter().enumerate() {
        let target = string_field(item, &["target"])
            .and_then(SceneElementTarget::parse)
            .ok_or_else(|| {
                SceneLoadError::invalid(format!(
                    "element effect {index} requires target affinity/mastery"
                ))
            })?;
        let element = string_field(item, &["element"])
            .and_then(SceneElement::parse)
            .ok_or_else(|| {
                SceneLoadError::invalid(format!(
                    "element effect {index} requires earth/fire/water/wind element"
                ))
            })?;
        let delta = number_field(item, &["delta", "value"]).ok_or_else(|| {
            SceneLoadError::invalid(format!("element effect {index} requires delta"))
        })?;
        effects.push(SceneContextElementEffect {
            target,
            element,
            delta,
        });
    }
    Ok(effects)
}

fn apply_context_effects(elements: &mut CharacterElements, effects: &[SceneContextElementEffect]) {
    for effect in effects {
        let target = match effect.target {
            SceneElementTarget::Affinity => &mut elements.affinity,
            SceneElementTarget::Mastery => &mut elements.mastery,
        };
        apply_element_delta(target, effect.element, effect.delta);
    }
}

fn apply_element_delta(values: &mut ElementValues, element: SceneElement, delta: i32) {
    match element {
        SceneElement::Earth => values.earth = values.earth.saturating_add(delta),
        SceneElement::Fire => values.fire = values.fire.saturating_add(delta),
        SceneElement::Water => values.water = values.water.saturating_add(delta),
        SceneElement::Wind => values.wind = values.wind.saturating_add(delta),
    }
}

impl SceneElementTarget {
    fn parse(value: String) -> Option<Self> {
        match value.as_str() {
            "affinity" | "element_affinity" => Some(Self::Affinity),
            "mastery" | "element_mastery" => Some(Self::Mastery),
            _ => None,
        }
    }
}

impl SceneElement {
    fn parse(value: String) -> Option<Self> {
        match value.as_str() {
            "earth" => Some(Self::Earth),
            "fire" => Some(Self::Fire),
            "water" => Some(Self::Water),
            "wind" => Some(Self::Wind),
            _ => None,
        }
    }
}

fn string_field(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(value) = map.get(*key).or_else(|| map.get(&to_camel_case(key))) {
            if let Some(text) = value.as_str() {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_ascii_lowercase());
                }
            }
        }
    }
    None
}

fn number_field(value: &serde_json::Value, keys: &[&str]) -> Option<i32> {
    let map = value.as_object()?;
    for key in keys {
        if let Some(value) = map.get(*key).or_else(|| map.get(&to_camel_case(key))) {
            if let Some(number) = value.as_i64().and_then(|value| i32::try_from(value).ok()) {
                return Some(number);
            }
            if let Some(number) = value.as_str().and_then(|value| value.trim().parse().ok()) {
                return Some(number);
            }
        }
    }
    None
}

fn to_camel_case(value: &str) -> String {
    let mut result = String::new();
    let mut uppercase_next = false;
    for ch in value.chars() {
        if ch == '_' {
            uppercase_next = true;
        } else if uppercase_next {
            result.push(ch.to_ascii_uppercase());
            uppercase_next = false;
        } else {
            result.push(ch);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use crate::core::character_discipline::CharacterDiscipline;
    use crate::core::character_element::{CharacterElements, ElementValues};
    use crate::core::character_title::CharacterTitle;
    use crate::core::inventory::PlayerData;
    use crate::core::inventory::item::{Item, ItemElementValues};
    use crate::gameconfig::ConfigTables;

    use super::*;

    fn catalog() -> SceneCatalog {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let tables = ConfigTables::load_from_dir(&root.join("csv")).unwrap();
        SceneCatalog::load_from_dir(&root.join("scene"), &tables).unwrap()
    }

    fn character_state() -> SceneCharacterState {
        let elements = CharacterElements {
            character_id: "chr_0000000000001".to_string(),
            affinity: ElementValues::new(2500, 2500, 2500, 2500),
            mastery: ElementValues::new(0, 20, 0, 0),
        };
        let mut state = SceneCharacterState::new(elements);
        state.disciplines.push(CharacterDiscipline {
            character_id: state.character_id.clone(),
            discipline_id: "forging".to_string(),
            points: 1,
            tier: "novice".to_string(),
            active: true,
            learned_at: "now".to_string(),
            updated_at: "now".to_string(),
        });
        state.titles.push(CharacterTitle {
            character_id: state.character_id.clone(),
            title_id: "2001".to_string(),
            source_type: "unit".to_string(),
            source_id: None,
            is_equipped: false,
            unlocked_at: "now".to_string(),
            expires_at: None,
            expired: false,
        });
        state
    }

    #[test]
    fn region_entry_checks_affinity_mastery_discipline_title_and_progress() {
        let catalog = catalog();
        let mut player_data = PlayerData::new("chr_0000000000001".to_string());
        player_data.progress.completed.insert(
            "quest_1".to_string(),
            crate::core::inventory::player_data::CharacterProgressRecord {
                progress_id: "quest_1".to_string(),
                source_type: "quest".to_string(),
                source_id: "quest_1".to_string(),
                completed_at: "now".to_string(),
            },
        );
        let state = character_state().with_player_progress(&player_data);

        let portal = catalog.evaluate_region_entry(3003, &state).unwrap();

        assert_eq!(portal.outcome.status, SceneConditionStatus::Matched);
        assert_eq!(portal.code, "grassland_to_dungeon");
        assert_eq!(portal.prompt_key, "scene.region.portal.locked");

        let missing_progress = catalog
            .evaluate_region_entry(3003, &character_state())
            .unwrap();
        assert_eq!(
            missing_progress.outcome.status,
            SceneConditionStatus::NotMatched
        );

        let boss_without_quest_source = catalog
            .evaluate_region_entry(4002, &character_state())
            .unwrap();
        assert_eq!(
            boss_without_quest_source.outcome.status,
            SceneConditionStatus::Unsupported
        );
    }

    #[test]
    fn scene_interactions_check_elements_discipline_item_growth_and_titles() {
        let catalog = catalog();
        let state = character_state();

        let fire_totem = catalog.evaluate_interaction(6001, &state).unwrap();

        assert_eq!(fire_totem.outcome.status, SceneConditionStatus::Matched);
        assert_eq!(fire_totem.code, "grassland_fire_totem");

        let mut player_data = PlayerData::new("chr_0000000000001".to_string());
        let mut item = Item::new(7, 1002, 1, false);
        item.growth_elements = ItemElementValues::new(0, 1, 0, 0);
        player_data.add_item(item).unwrap();
        let mut growth_state = character_state().with_player_progress(&player_data);
        growth_state.titles.push(CharacterTitle {
            character_id: growth_state.character_id.clone(),
            title_id: "3001".to_string(),
            source_type: "unit".to_string(),
            source_id: None,
            is_equipped: false,
            unlocked_at: "now".to_string(),
            expires_at: None,
            expired: false,
        });

        let growth_seal = catalog.evaluate_interaction(6002, &growth_state).unwrap();
        assert_eq!(growth_seal.outcome.status, SceneConditionStatus::Matched);

        let blocked = catalog
            .evaluate_interaction(6002, &character_state())
            .unwrap();
        assert_eq!(blocked.outcome.status, SceneConditionStatus::NotMatched);
    }

    #[test]
    fn npc_branches_use_priority_and_return_unsupported_for_missing_sources() {
        let catalog = catalog();

        let unsupported = catalog
            .resolve_npc_branch(1, "npc_blacksmith", &character_state())
            .unwrap();
        assert_eq!(unsupported.branch.code, "blacksmith_guild");
        assert_eq!(
            unsupported.outcome.status,
            SceneConditionStatus::Unsupported
        );

        let mut trusted = character_state();
        trusted
            .organization_states
            .insert("forging_guild".to_string(), "member".to_string());
        trusted
            .regional_reputation
            .insert("grassland".to_string(), 10);

        let branch = catalog
            .resolve_npc_branch(1, "npc_blacksmith", &trusted)
            .unwrap();
        assert_eq!(branch.branch.code, "blacksmith_guild");
        assert_eq!(branch.branch.attitude, "trusted");
        assert_eq!(branch.outcome.status, SceneConditionStatus::Matched);
    }

    #[test]
    fn temporary_scene_context_affects_effective_elements_only_while_active() {
        let catalog = catalog();
        let state = character_state();
        let before = state.elements.clone();

        let inactive = catalog.effective_elements_with_context(1, None, &state, &[]);
        assert_eq!(inactive.elements, before);
        assert!(inactive.applied_context_codes.is_empty());

        let active = catalog.effective_elements_with_context(
            1,
            Some(3002),
            &state,
            &[
                "grassland_rain".to_string(),
                "grassland_wolf_field_wind".to_string(),
            ],
        );

        assert_eq!(active.elements.affinity.water, before.affinity.water + 120);
        assert_eq!(active.elements.mastery.fire, before.mastery.fire - 2);
        assert_eq!(active.elements.mastery.wind, before.mastery.wind + 3);
        assert_eq!(
            active.applied_context_codes,
            vec![
                "grassland_rain".to_string(),
                "grassland_wolf_field_wind".to_string()
            ]
        );
        assert_eq!(state.elements, before);
    }

    #[test]
    fn world_event_context_requires_matching_runtime_event_state() {
        let catalog = catalog();
        let mut state = character_state();

        let missing = catalog.effective_elements_with_context(
            2,
            Some(4002),
            &state,
            &["relic_surge".to_string()],
        );
        assert_eq!(missing.elements.mastery.fire, 20);
        assert!(missing.applied_context_codes.is_empty());

        state
            .world_events
            .insert("relic_surge".to_string(), "active".to_string());
        let active = catalog.effective_elements_with_context(
            2,
            Some(4002),
            &state,
            &["relic_surge".to_string()],
        );
        assert_eq!(active.elements.mastery.fire, 28);
        assert_eq!(active.applied_context_codes, vec!["relic_surge"]);
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

        let walkable = self
            .layer_value(scene_id, "walkable", cell_x, cell_y)
            .unwrap_or(0);
        let blocked = self
            .layer_value(scene_id, "block", cell_x, cell_y)
            .unwrap_or(1);
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
