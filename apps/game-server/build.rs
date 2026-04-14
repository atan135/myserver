#[path = "tools/csv_codegen.rs"]
mod csv_codegen;

use std::env;
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
    println!("cargo:rerun-if-changed=../../packages/proto/match.proto");

    csv_codegen::generate(&csv_dir, &csv_out_dir).expect("failed to generate csv table code");

    // Use tonic_build for generating gRPC client code
    tonic_build::configure()
        .build_client(true)
        .out_dir(&proto_out_dir)
        .compile(
            &[
                "../../packages/proto/game.proto",
                "../../packages/proto/admin.proto",
                "../../packages/proto/match.proto",
            ],
            &["../../packages/proto"],
        )
        .expect("failed to compile protobuf files");
}
