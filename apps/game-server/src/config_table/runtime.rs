use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::config_table::{ConfigTables, CsvLoadError};

#[derive(Clone)]
pub struct ConfigTableRuntime {
    csv_dir: PathBuf,
    tables: Arc<RwLock<Arc<ConfigTables>>>,
}

impl ConfigTableRuntime {
    pub fn load(csv_dir: &Path) -> Result<Self, CsvLoadError> {
        let tables = Arc::new(ConfigTables::load_from_dir(csv_dir)?);
        Ok(Self {
            csv_dir: csv_dir.to_path_buf(),
            tables: Arc::new(RwLock::new(tables)),
        })
    }

    pub fn csv_dir(&self) -> &Path {
        &self.csv_dir
    }

    pub fn watched_csv_files(&self) -> Vec<PathBuf> {
        ConfigTables::watched_csv_files(&self.csv_dir)
    }

    pub async fn snapshot(&self) -> Arc<ConfigTables> {
        self.tables.read().await.clone()
    }

    pub async fn reload_changed(
        &self,
        changed_files: &[PathBuf],
    ) -> Result<Arc<ConfigTables>, CsvLoadError> {
        let changed_file_names = changed_files
            .iter()
            .filter_map(|path| path.file_name().and_then(|value| value.to_str()))
            .map(|value| value.to_string())
            .collect::<HashSet<_>>();

        if changed_file_names.is_empty() {
            return Ok(self.snapshot().await);
        }

        let current = self.snapshot().await;
        let next = Arc::new(current.reload_changed(&self.csv_dir, &changed_file_names)?);

        let mut guard = self.tables.write().await;
        *guard = next.clone();
        Ok(next)
    }
}
