#[path = "tools/csv_codegen.rs"]
mod csv_codegen;

use std::env;
use std::fs;
use std::path::PathBuf;

const PROTOC_BIN_VENDORED_VERSION: &str = "3.2.0";
const PROST_BUILD_VERSION: &str = "0.13.5";
const TONIC_BUILD_VERSION: &str = "0.12.3";
const GENERATED_FILES: &[&str] = &[
    "myserver.admin.rs",
    "myserver.game.rs",
    "myserver.matchservice.rs",
];

fn proto_out_dir(manifest_dir: &std::path::Path) -> PathBuf {
    env::var_os("MYSERVER_PROTO_OUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("src").join("proto"))
}

fn prepare_proto_out_dir(out_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(out_dir)?;
    for file_name in GENERATED_FILES {
        let generated_file = out_dir.join(file_name);
        if generated_file.exists() {
            fs::remove_file(generated_file)?;
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let csv_dir = manifest_dir.join("csv");
    let csv_out_dir = manifest_dir.join("src").join("csv_code");
    let proto_out_dir = proto_out_dir(&manifest_dir);

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=tools/csv_codegen.rs");
    println!("cargo:rerun-if-changed=csv");
    println!("cargo:rerun-if-changed=../../packages/proto/game.proto");
    println!("cargo:rerun-if-changed=../../packages/proto/admin.proto");
    println!("cargo:rerun-if-changed=../../packages/proto/match.proto");
    println!("cargo:rerun-if-env-changed=MYSERVER_PROTO_OUT_DIR");
    println!("cargo:rerun-if-env-changed=MYSERVER_PROTOCOL_ONLY");

    if env::var_os("MYSERVER_PROTOCOL_ONLY").is_none() {
        csv_codegen::generate(&csv_dir, &csv_out_dir).map_err(|error| {
            std::io::Error::other(format!("failed to generate csv table code: {error}"))
        })?;
    }

    prepare_proto_out_dir(&proto_out_dir)?;
    let protoc = protoc_bin_vendored::protoc_bin_path().map_err(|error| {
        std::io::Error::other(format!(
            "protocol generation requires protoc-bin-vendored {PROTOC_BIN_VENDORED_VERSION} for packages/proto/game.proto, packages/proto/admin.proto, and packages/proto/match.proto; output targets are {}; original error: {error}",
            proto_out_dir.display()
        ))
    })?;

    let mut config = prost_build::Config::new();
    config.out_dir(&proto_out_dir).protoc_executable(protoc);
    tonic_build::configure()
        .build_client(true)
        .out_dir(&proto_out_dir)
        .compile_protos_with_config(
            config,
            &[
                "../../packages/proto/game.proto",
                "../../packages/proto/admin.proto",
                "../../packages/proto/match.proto",
            ],
            &["../../packages/proto"],
        )
        .map_err(|error| {
            std::io::Error::other(format!(
                "protocol generation failed with protoc-bin-vendored {PROTOC_BIN_VENDORED_VERSION}, prost-build {PROST_BUILD_VERSION}, and tonic-build {TONIC_BUILD_VERSION}; inputs: packages/proto/game.proto, packages/proto/admin.proto, packages/proto/match.proto; output target: {}; original error: {error}",
                proto_out_dir.display()
            ))
        })?;

    Ok(())
}
