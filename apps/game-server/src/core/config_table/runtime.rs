use std::collections::HashSet;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;

use crate::core::config_table::CsvLoadError;
use crate::core::runtime::room_policy::{RoomPolicyRegistry, SharedRoomPolicyRegistry};
use crate::core::system::combat::{CsvCombatCatalog, SharedCombatCatalog};
use crate::core::system::scene::{SceneCatalog, SceneLoadError};
use crate::gameconfig::ConfigTables;

#[derive(Clone)]
pub struct ConfigTableRuntime {
    csv_dir: PathBuf,
    scene_dir: PathBuf,
    snapshot: Arc<RwLock<Arc<RuntimeGameConfig>>>,
    room_policies: SharedRoomPolicyRegistry,
}

#[derive(Clone)]
pub struct RuntimeGameConfig {
    pub version: u64,
    pub tables: Arc<ConfigTables>,
    pub scene_catalog: Arc<SceneCatalog>,
    pub combat_catalog: SharedCombatCatalog,
    pub room_policies: Arc<RoomPolicyRegistry>,
}

#[derive(Clone)]
pub struct ReloadedRuntimeGameConfig {
    pub snapshot: Arc<RuntimeGameConfig>,
    pub changed_file_names: Vec<String>,
}

#[derive(Debug)]
pub enum RuntimeConfigLoadError {
    Csv {
        stage: &'static str,
        source: CsvLoadError,
    },
    Scene {
        stage: &'static str,
        source: SceneLoadError,
    },
}

impl Display for RuntimeConfigLoadError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Csv { stage, source } => write!(f, "{stage}: {source}"),
            Self::Scene { stage, source } => write!(f, "{stage}: {source}"),
        }
    }
}

impl std::error::Error for RuntimeConfigLoadError {}

impl RuntimeGameConfig {
    fn build(
        version: u64,
        tables: Arc<ConfigTables>,
        scene_dir: &Path,
    ) -> Result<Self, RuntimeConfigLoadError> {
        let scene_catalog = Arc::new(
            SceneCatalog::load_from_dir(scene_dir, tables.as_ref()).map_err(|source| {
                RuntimeConfigLoadError::Scene {
                    stage: "build SceneCatalog",
                    source,
                }
            })?,
        );
        if scene_catalog.scenes.is_empty() {
            return Err(RuntimeConfigLoadError::Scene {
                stage: "build SceneCatalog",
                source: SceneLoadError::invalid("scene catalog is empty"),
            });
        }
        let combat_catalog: SharedCombatCatalog = Arc::new(
            CsvCombatCatalog::from_tables(tables.as_ref()).map_err(|source| {
                RuntimeConfigLoadError::Csv {
                    stage: "build CsvCombatCatalog",
                    source,
                }
            })?,
        );

        Ok(Self {
            version,
            tables,
            scene_catalog,
            combat_catalog,
            room_policies: Arc::new(RoomPolicyRegistry::default()),
        })
    }
}

impl ConfigTableRuntime {
    pub fn load(csv_dir: &Path) -> Result<Self, RuntimeConfigLoadError> {
        let scene_dir = default_scene_dir(csv_dir);
        Self::load_with_scene_dir(csv_dir, &scene_dir)
    }

    pub fn load_with_scene_dir(
        csv_dir: &Path,
        scene_dir: &Path,
    ) -> Result<Self, RuntimeConfigLoadError> {
        let tables = Arc::new(ConfigTables::load_from_dir(csv_dir).map_err(|source| {
            RuntimeConfigLoadError::Csv {
                stage: "load ConfigTables",
                source,
            }
        })?);
        let snapshot = Arc::new(RuntimeGameConfig::build(1, tables, scene_dir)?);
        let room_policies = SharedRoomPolicyRegistry::new(snapshot.room_policies.clone());
        Ok(Self {
            csv_dir: csv_dir.to_path_buf(),
            scene_dir: scene_dir.to_path_buf(),
            snapshot: Arc::new(RwLock::new(snapshot)),
            room_policies,
        })
    }

    pub fn csv_dir(&self) -> &Path {
        &self.csv_dir
    }

    pub fn scene_dir(&self) -> &Path {
        &self.scene_dir
    }

    pub fn watched_csv_files(&self) -> Vec<PathBuf> {
        ConfigTables::watched_csv_files(&self.csv_dir)
    }

    pub fn room_policy_registry(&self) -> SharedRoomPolicyRegistry {
        self.room_policies.clone()
    }

    pub fn current_snapshot(&self) -> Arc<RuntimeGameConfig> {
        self.snapshot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub async fn snapshot(&self) -> Arc<RuntimeGameConfig> {
        self.current_snapshot()
    }

    pub async fn tables_snapshot(&self) -> Arc<ConfigTables> {
        self.current_snapshot().tables.clone()
    }

    pub async fn reload_changed(
        &self,
        changed_files: &[PathBuf],
    ) -> Result<ReloadedRuntimeGameConfig, RuntimeConfigLoadError> {
        let mut changed_file_names = changed_files
            .iter()
            .filter_map(|path| path.file_name().and_then(|value| value.to_str()))
            .map(|value| value.to_string())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        changed_file_names.sort();

        if changed_file_names.is_empty() {
            return Ok(ReloadedRuntimeGameConfig {
                snapshot: self.current_snapshot(),
                changed_file_names,
            });
        }

        let current = self.current_snapshot();
        let changed_file_set = changed_file_names.iter().cloned().collect::<HashSet<_>>();
        let next_tables = Arc::new(
            current
                .tables
                .reload_changed(&self.csv_dir, &changed_file_set)
                .map_err(|source| RuntimeConfigLoadError::Csv {
                    stage: "reload ConfigTables",
                    source,
                })?,
        );
        let next = Arc::new(RuntimeGameConfig::build(
            current.version.saturating_add(1),
            next_tables,
            &self.scene_dir,
        )?);

        let mut guard = self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = next.clone();
        self.room_policies.replace(next.room_policies.clone());
        Ok(ReloadedRuntimeGameConfig {
            snapshot: next,
            changed_file_names,
        })
    }
}

fn default_scene_dir(csv_dir: &Path) -> PathBuf {
    csv_dir
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("scene")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::system::scene::SceneQuery;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempConfigDir {
        root: PathBuf,
        csv_dir: PathBuf,
        scene_dir: PathBuf,
    }

    impl TempConfigDir {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "game-server-runtime-config-test-{}-{unique}",
                std::process::id()
            ));
            let csv_dir = root.join("csv");
            let scene_dir = root.join("scene");
            fs::create_dir_all(&csv_dir).unwrap();
            fs::create_dir_all(&scene_dir).unwrap();

            let source_root = Path::new(env!("CARGO_MANIFEST_DIR"));
            copy_dir(&source_root.join("csv"), &csv_dir);
            copy_dir(&source_root.join("scene"), &scene_dir);

            Self {
                root,
                csv_dir,
                scene_dir,
            }
        }
    }

    impl Drop for TempConfigDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test]
    async fn reload_replaces_raw_tables_and_derived_catalogs() {
        let fixture = TempConfigDir::new();
        let runtime = ConfigTableRuntime::load_with_scene_dir(&fixture.csv_dir, &fixture.scene_dir)
            .expect("initial config should load");
        let initial = runtime.current_snapshot();
        assert_eq!(initial.version, 1);
        assert_eq!(
            initial
                .combat_catalog
                .skill_definition(2)
                .expect("skill exists")
                .range,
            300.0
        );
        assert_eq!(
            initial
                .scene_catalog
                .spawn_point(1001)
                .expect("spawn exists")
                .x,
            2.0
        );

        replace_in_file(
            &fixture.csv_dir.join("SkillBase.csv"),
            "2,fireball,火球术,90,0,300,Enemy,",
            "2,fireball,火球术,90,0,321.5,Enemy,",
        );
        replace_in_file(
            &fixture.csv_dir.join("SceneSpawnPoint.csv"),
            "1001,1,grassland_player_main,player,2.0,2.0,1.0,0.0,2.0,default|safe",
            "1001,1,grassland_player_main,player,3.5,2.0,1.0,0.0,2.0,default|safe",
        );

        let reloaded = runtime
            .reload_changed(&[
                fixture.csv_dir.join("SkillBase.csv"),
                fixture.csv_dir.join("SceneSpawnPoint.csv"),
            ])
            .await
            .expect("reload should succeed");

        assert_eq!(reloaded.snapshot.version, 2);
        assert_eq!(runtime.current_snapshot().version, 2);
        assert_eq!(
            reloaded
                .snapshot
                .combat_catalog
                .skill_definition(2)
                .expect("skill exists")
                .range,
            321.5
        );
        assert_eq!(
            reloaded
                .snapshot
                .scene_catalog
                .spawn_point(1001)
                .expect("spawn exists")
                .x,
            3.5
        );
    }

    #[tokio::test]
    async fn failed_derived_reload_keeps_previous_runtime_config() {
        let fixture = TempConfigDir::new();
        let runtime = ConfigTableRuntime::load_with_scene_dir(&fixture.csv_dir, &fixture.scene_dir)
            .expect("initial config should load");
        let initial = runtime.current_snapshot();

        replace_in_file(
            &fixture.csv_dir.join("SceneTable.csv"),
            "1,grassland_01,初心草原,grassland_01.grid.json,16,16,1.0,4,1001,pve|outdoor|safe_zone",
            "1,grassland_broken,初心草原,grassland_01.grid.json,16,16,1.0,4,1001,pve|outdoor|safe_zone",
        );

        let error = match runtime
            .reload_changed(&[fixture.csv_dir.join("SceneTable.csv")])
            .await
        {
            Ok(_) => panic!("scene catalog rebuild should fail"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("build SceneCatalog"),
            "error should include failed stage: {error}"
        );
        assert_eq!(runtime.current_snapshot().version, 1);
        assert!(Arc::ptr_eq(&initial, &runtime.current_snapshot()));
        assert_eq!(
            runtime
                .current_snapshot()
                .scene_catalog
                .scene_id_by_code("grassland_01"),
            Some(1)
        );
    }

    #[tokio::test]
    async fn empty_scene_catalog_reload_keeps_previous_runtime_config() {
        let fixture = TempConfigDir::new();
        let runtime = ConfigTableRuntime::load_with_scene_dir(&fixture.csv_dir, &fixture.scene_dir)
            .expect("initial config should load");
        let initial = runtime.current_snapshot();

        fs::write(
            fixture.csv_dir.join("SceneTable.csv"),
            "Id,Code,Name,GridFile,Width,Height,CellSize,AoiBlockSize,DefaultSpawnId,Tags\nint,string,string,string,int,int,float,int,int,Array<string>\n",
        )
        .unwrap();

        let error = match runtime
            .reload_changed(&[fixture.csv_dir.join("SceneTable.csv")])
            .await
        {
            Ok(_) => panic!("empty scene catalog should fail"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("scene catalog is empty"),
            "error should include empty catalog reason: {error}"
        );
        assert_eq!(runtime.current_snapshot().version, 1);
        assert!(Arc::ptr_eq(&initial, &runtime.current_snapshot()));
    }

    fn copy_dir(source: &Path, target: &Path) {
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let source_path = entry.path();
            let target_path = target.join(entry.file_name());
            if source_path.is_dir() {
                fs::create_dir_all(&target_path).unwrap();
                copy_dir(&source_path, &target_path);
            } else {
                fs::copy(&source_path, &target_path).unwrap();
            }
        }
    }

    fn replace_in_file(path: &Path, from: &str, to: &str) {
        let content = fs::read_to_string(path).unwrap();
        assert!(
            content.contains(from),
            "test fixture did not contain expected text in {}",
            path.display()
        );
        fs::write(path, content.replace(from, to)).unwrap();
    }
}
