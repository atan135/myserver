//! Offline-safe pressure-test framework core.
//!
//! The stage-one runner deliberately has no production transport client.  It
//! validates plans, exercises deterministic fakes and writes reports only when
//! `--dry-run` is selected.  Real auth and KCP clients are added in later
//! stages behind the same safety gate and controller contracts.

pub mod abort;
pub mod accounts;
pub mod auth_budget;
pub mod auth_http;
pub mod calibration;
pub mod compatibility;
pub mod config;
pub mod contracts;
pub mod fake;
pub mod game_kcp;
pub mod gameplay;
pub mod lifecycle;
pub mod metrics;
pub mod preflight;
pub mod protection;
pub mod reconnect_burst;
pub mod report;
pub mod resource;
pub mod scheduler;
pub mod step;
pub mod virtual_player;

pub mod protocol_version_policy {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/proto/compatibility/version-policy.rs"
    ));
}

pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/myserver.game.rs"));
}

pub use config::{LoadTestConfig, RunAccess, load_config};

pub const SCHEMA_VERSION: u32 = 1;
