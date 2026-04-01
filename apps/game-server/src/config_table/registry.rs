use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::config_table::traits::{CsvLoadError, CsvTableLoader};
use crate::csv_code::testtable_100::TestTable100;
use crate::csv_code::testtable_110::TestTable110;

const TESTTABLE_100_FILE: &str = "TestTable_100.csv";
const TESTTABLE_110_FILE: &str = "TestTable_110.csv";

#[derive(Clone)]
pub struct ConfigTables {
    pub testtable_100: Arc<TestTable100>,
    pub testtable_110: Arc<TestTable110>,
}

#[derive(Clone, Copy, Debug)]
pub struct ConfigTableRowCounts {
    pub testtable_100: usize,
    pub testtable_110: usize,
}

impl ConfigTables {
    pub fn load_from_dir(csv_dir: &Path) -> Result<Self, CsvLoadError> {
        let testtable_100 = TestTable100::load_from_csv(&csv_dir.join(TESTTABLE_100_FILE))?;
        let testtable_110 = TestTable110::load_from_csv(&csv_dir.join(TESTTABLE_110_FILE))?;

        Ok(Self {
            testtable_100: Arc::new(testtable_100),
            testtable_110: Arc::new(testtable_110),
        })
    }

    pub fn reload_changed(
        &self,
        csv_dir: &Path,
        changed_files: &HashSet<String>,
    ) -> Result<Self, CsvLoadError> {
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

        Ok(Self {
            testtable_100,
            testtable_110,
        })
    }

    pub fn watched_csv_files(csv_dir: &Path) -> Vec<PathBuf> {
        vec![
            csv_dir.join(TESTTABLE_100_FILE),
            csv_dir.join(TESTTABLE_110_FILE),
        ]
    }

    pub fn row_counts(&self) -> ConfigTableRowCounts {
        ConfigTableRowCounts {
            testtable_100: self.testtable_100.rows.len(),
            testtable_110: self.testtable_110.rows.len(),
        }
    }
}
