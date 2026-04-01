#[path = "tools/csv_codegen.rs"]
mod csv_codegen;

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("missing manifest dir"));
    let csv_dir = manifest_dir.join("csv");
    let csv_out_dir = manifest_dir.join("src").join("csv_code");
    let proto_out_dir = manifest_dir.join("src").join("proto");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=tools/csv_codegen.rs");
    println!("cargo:rerun-if-changed=csv");
    println!("cargo:rerun-if-changed=../../packages/proto/game.proto");
    println!("cargo:rerun-if-changed=../../packages/proto/admin.proto");

    csv_codegen::generate(&csv_dir, &csv_out_dir).expect("failed to generate csv table code");

    fs::create_dir_all(&proto_out_dir).expect("failed to create proto output dir");

    let mut config = prost_build::Config::new();
    config.out_dir(&proto_out_dir);
    config
        .compile_protos(
            &["../../packages/proto/game.proto", "../../packages/proto/admin.proto"],
            &["../../packages/proto"],
        )
        .expect("failed to compile protobuf files");
}
