#![forbid(unsafe_code)]

pub mod protocol_version_policy {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/proto/compatibility/version-policy.rs"
    ));
}

pub mod offline;
pub mod online;
pub mod scenario;
