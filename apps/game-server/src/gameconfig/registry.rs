use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::config_table::{CsvLoadError, CsvTableLoader};
use crate::csv_code::bufferbase::BufferBase;
use crate::csv_code::itemtable::ItemTable;
use crate::csv_code::scenemonsterspawn::SceneMonsterSpawn;
use crate::csv_code::sceneportal::ScenePortal;
use crate::csv_code::sceneregion::SceneRegion;
use crate::csv_code::scenespawnpoint::SceneSpawnPoint;
use crate::csv_code::scenetable::SceneTable;
use crate::csv_code::skillbase::SkillBase;
use crate::csv_code::testtable_100::TestTable100;
use crate::csv_code::testtable_110::TestTable110;

const SCENETABLE_FILE: &str = "SceneTable.csv";
const SCENESPAWNPOINT_FILE: &str = "SceneSpawnPoint.csv";
const SCENEPORTAL_FILE: &str = "ScenePortal.csv";
const SCENEREGION_FILE: &str = "SceneRegion.csv";
const SCENEMONSTERSPAWN_FILE: &str = "SceneMonsterSpawn.csv";
const TESTTABLE_100_FILE: &str = "TestTable_100.csv";
const TESTTABLE_110_FILE: &str = "TestTable_110.csv";
const ITEMTABLE_FILE: &str = "ItemTable.csv";
const SKILLBASE_FILE: &str = "SkillBase.csv";
const BUFFERBASE_FILE: &str = "BufferBase.csv";

#[derive(Clone)]
pub struct ConfigTables {
    pub scenetable: Arc<SceneTable>,
    pub scenespawnpoint: Arc<SceneSpawnPoint>,
    pub sceneportal: Arc<ScenePortal>,
    pub sceneregion: Arc<SceneRegion>,
    pub scenemonsterspawn: Arc<SceneMonsterSpawn>,
    pub testtable_100: Arc<TestTable100>,
    pub testtable_110: Arc<TestTable110>,
    pub item_table: Arc<ItemTable>,
    pub skillbase: Arc<SkillBase>,
    pub bufferbase: Arc<BufferBase>,
}

#[derive(Clone, Copy, Debug)]
pub struct ConfigTableRowCounts {
    pub scenetable: usize,
    pub scenespawnpoint: usize,
    pub sceneportal: usize,
    pub sceneregion: usize,
    pub scenemonsterspawn: usize,
    pub testtable_100: usize,
    pub testtable_110: usize,
    pub itemtable: usize,
    pub skillbase: usize,
    pub bufferbase: usize,
}

impl ConfigTables {
    pub fn load_from_dir(csv_dir: &Path) -> Result<Self, CsvLoadError> {
        let scenetable = SceneTable::load_from_csv(&csv_dir.join(SCENETABLE_FILE))?;
        let scenespawnpoint =
            SceneSpawnPoint::load_from_csv(&csv_dir.join(SCENESPAWNPOINT_FILE))?;
        let sceneportal = ScenePortal::load_from_csv(&csv_dir.join(SCENEPORTAL_FILE))?;
        let sceneregion = SceneRegion::load_from_csv(&csv_dir.join(SCENEREGION_FILE))?;
        let scenemonsterspawn =
            SceneMonsterSpawn::load_from_csv(&csv_dir.join(SCENEMONSTERSPAWN_FILE))?;
        let testtable_100 = TestTable100::load_from_csv(&csv_dir.join(TESTTABLE_100_FILE))?;
        let testtable_110 = TestTable110::load_from_csv(&csv_dir.join(TESTTABLE_110_FILE))?;
        let itemtable = ItemTable::load_from_csv(&csv_dir.join(ITEMTABLE_FILE))?;
        let skillbase = SkillBase::load_from_csv(&csv_dir.join(SKILLBASE_FILE))?;
        let bufferbase = BufferBase::load_from_csv(&csv_dir.join(BUFFERBASE_FILE))?;

        Ok(Self {
            scenetable: Arc::new(scenetable),
            scenespawnpoint: Arc::new(scenespawnpoint),
            sceneportal: Arc::new(sceneportal),
            sceneregion: Arc::new(sceneregion),
            scenemonsterspawn: Arc::new(scenemonsterspawn),
            testtable_100: Arc::new(testtable_100),
            testtable_110: Arc::new(testtable_110),
            item_table: Arc::new(itemtable),
            skillbase: Arc::new(skillbase),
            bufferbase: Arc::new(bufferbase),
        })
    }

    pub fn reload_changed(
        &self,
        csv_dir: &Path,
        changed_files: &HashSet<String>,
    ) -> Result<Self, CsvLoadError> {
        let scenetable = if changed_files.contains(SCENETABLE_FILE) {
            Arc::new(SceneTable::load_from_csv(&csv_dir.join(SCENETABLE_FILE))?)
        } else {
            self.scenetable.clone()
        };

        let scenespawnpoint = if changed_files.contains(SCENESPAWNPOINT_FILE) {
            Arc::new(SceneSpawnPoint::load_from_csv(
                &csv_dir.join(SCENESPAWNPOINT_FILE),
            )?)
        } else {
            self.scenespawnpoint.clone()
        };

        let sceneportal = if changed_files.contains(SCENEPORTAL_FILE) {
            Arc::new(ScenePortal::load_from_csv(&csv_dir.join(SCENEPORTAL_FILE))?)
        } else {
            self.sceneportal.clone()
        };

        let sceneregion = if changed_files.contains(SCENEREGION_FILE) {
            Arc::new(SceneRegion::load_from_csv(&csv_dir.join(SCENEREGION_FILE))?)
        } else {
            self.sceneregion.clone()
        };

        let scenemonsterspawn = if changed_files.contains(SCENEMONSTERSPAWN_FILE) {
            Arc::new(SceneMonsterSpawn::load_from_csv(
                &csv_dir.join(SCENEMONSTERSPAWN_FILE),
            )?)
        } else {
            self.scenemonsterspawn.clone()
        };

        let testtable_100 = if changed_files.contains(TESTTABLE_100_FILE) {
            Arc::new(TestTable100::load_from_csv(&csv_dir.join(TESTTABLE_100_FILE))?)
        } else {
            self.testtable_100.clone()
        };

        let testtable_110 = if changed_files.contains(TESTTABLE_110_FILE) {
            Arc::new(TestTable110::load_from_csv(&csv_dir.join(TESTTABLE_110_FILE))?)
        } else {
            self.testtable_110.clone()
        };

        let itemtable = if changed_files.contains(ITEMTABLE_FILE) {
            Arc::new(ItemTable::load_from_csv(&csv_dir.join(ITEMTABLE_FILE))?)
        } else {
            self.item_table.clone()
        };

        let skillbase = if changed_files.contains(SKILLBASE_FILE) {
            Arc::new(SkillBase::load_from_csv(&csv_dir.join(SKILLBASE_FILE))?)
        } else {
            self.skillbase.clone()
        };

        let bufferbase = if changed_files.contains(BUFFERBASE_FILE) {
            Arc::new(BufferBase::load_from_csv(&csv_dir.join(BUFFERBASE_FILE))?)
        } else {
            self.bufferbase.clone()
        };

        Ok(Self {
            scenetable,
            scenespawnpoint,
            sceneportal,
            sceneregion,
            scenemonsterspawn,
            testtable_100,
            testtable_110,
            item_table: itemtable,
            skillbase,
            bufferbase,
        })
    }

    pub fn watched_csv_files(csv_dir: &Path) -> Vec<PathBuf> {
        vec![
            csv_dir.join(SCENETABLE_FILE),
            csv_dir.join(SCENESPAWNPOINT_FILE),
            csv_dir.join(SCENEPORTAL_FILE),
            csv_dir.join(SCENEREGION_FILE),
            csv_dir.join(SCENEMONSTERSPAWN_FILE),
            csv_dir.join(TESTTABLE_100_FILE),
            csv_dir.join(TESTTABLE_110_FILE),
            csv_dir.join(ITEMTABLE_FILE),
            csv_dir.join(SKILLBASE_FILE),
            csv_dir.join(BUFFERBASE_FILE),
        ]
    }

    pub fn row_counts(&self) -> ConfigTableRowCounts {
        ConfigTableRowCounts {
            scenetable: self.scenetable.rows.len(),
            scenespawnpoint: self.scenespawnpoint.rows.len(),
            sceneportal: self.sceneportal.rows.len(),
            sceneregion: self.sceneregion.rows.len(),
            scenemonsterspawn: self.scenemonsterspawn.rows.len(),
            testtable_100: self.testtable_100.rows.len(),
            testtable_110: self.testtable_110.rows.len(),
            itemtable: self.item_table.rows.len(),
            skillbase: self.skillbase.rows.len(),
            bufferbase: self.bufferbase.rows.len(),
        }
    }
}
