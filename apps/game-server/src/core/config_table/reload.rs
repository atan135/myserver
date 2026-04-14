use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{error, info, warn};

use crate::core::config_table::ConfigTableRuntime;

#[derive(Clone, Debug, Eq, PartialEq)]
struct CsvFileState {
    exists: bool,
    len: u64,
    modified: Option<SystemTime>,
}

pub fn spawn_hot_reload_task(runtime: ConfigTableRuntime, interval: Duration) -> JoinHandle<()> {
    tokio::spawn(async move {
        let watched_files = runtime.watched_csv_files();
        if watched_files.is_empty() {
            warn!(csv_dir = %runtime.csv_dir().display(), "csv hot reload skipped because no tables are registered");
            return;
        }

        let mut known_states = snapshot_file_states(&watched_files);
        info!(
            csv_dir = %runtime.csv_dir().display(),
            interval_secs = interval.as_secs(),
            file_count = watched_files.len(),
            "csv hot reload task started"
        );

        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            ticker.tick().await;

            let next_states = snapshot_file_states(&watched_files);
            let changed_files = collect_changed_files(&watched_files, &known_states, &next_states);
            if changed_files.is_empty() {
                continue;
            }

            let changed_labels = changed_files
                .iter()
                .map(|path| {
                    path.file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("<unknown>")
                        .to_string()
                })
                .collect::<Vec<_>>();

            match runtime.reload_changed(&changed_files).await {
                Ok(tables) => {
                    let counts = tables.row_counts();
                    info!(
                        csv_dir = %runtime.csv_dir().display(),
                        changed_files = changed_labels.join(","),
                        testtable_100_rows = counts.testtable_100,
                        testtable_110_rows = counts.testtable_110,
                        "csv config hot reload succeeded"
                    );
                }
                Err(error) => {
                    error!(
                        csv_dir = %runtime.csv_dir().display(),
                        changed_files = changed_labels.join(","),
                        error = %error,
                        "csv config hot reload failed; keeping previous tables"
                    );
                }
            }

            known_states = next_states;
        }
    })
}

fn collect_changed_files(
    watched_files: &[PathBuf],
    previous: &HashMap<PathBuf, CsvFileState>,
    current: &HashMap<PathBuf, CsvFileState>,
) -> Vec<PathBuf> {
    watched_files
        .iter()
        .filter(|path| previous.get(*path) != current.get(*path))
        .cloned()
        .collect()
}

fn snapshot_file_states(paths: &[PathBuf]) -> HashMap<PathBuf, CsvFileState> {
    paths.iter()
        .cloned()
        .map(|path| {
            let state = match std::fs::metadata(&path) {
                Ok(metadata) => CsvFileState {
                    exists: true,
                    len: metadata.len(),
                    modified: metadata.modified().ok(),
                },
                Err(_) => CsvFileState {
                    exists: false,
                    len: 0,
                    modified: None,
                },
            };
            (path, state)
        })
        .collect()
}
