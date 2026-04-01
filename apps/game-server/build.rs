#[path = "tools/csv_codegen.rs"]
mod csv_codegen;

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("missing manifest dir"));
    let csv_dir = manifest_dir.join("csv");
    let out_dir = manifest_dir.join("src").join("csv_code");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=tools/csv_codegen.rs");
    println!("cargo:rerun-if-changed=csv");

    csv_codegen::generate(&csv_dir, &out_dir).expect("failed to generate csv table code");

    prost_build::compile_protos(
        &["../../packages/proto/game.proto", "../../packages/proto/admin.proto"],
        &["../../packages/proto"],
    )
    .expect("failed to compile protobuf files");
}


