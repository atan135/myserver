use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../packages/proto/game.proto");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("missing OUT_DIR"));
    prost_build::Config::new()
        .out_dir(&out_dir)
        .compile_protos(&["../../packages/proto/game.proto"], &["../../packages/proto"])
        .expect("failed to compile game proto");
}
