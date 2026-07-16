use std::env;
use std::fs;
use std::path::PathBuf;

const PROTOC_BIN_VENDORED_VERSION: &str = "3.2.0";
const PROST_BUILD_VERSION: &str = "0.13.5";
const GENERATED_FILES: &[&str] = &["myserver.game.rs"];

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
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../packages/proto/game.proto");
    println!("cargo:rerun-if-env-changed=MYSERVER_PROTO_OUT_DIR");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let out_dir = proto_out_dir(&manifest_dir);
    prepare_proto_out_dir(&out_dir)?;
    let protoc = protoc_bin_vendored::protoc_bin_path().map_err(|error| {
        std::io::Error::other(format!(
            "protocol generation requires protoc-bin-vendored {PROTOC_BIN_VENDORED_VERSION} for packages/proto/game.proto; output target is {}; original error: {error}",
            out_dir.display()
        ))
    })?;
    let mut config = prost_build::Config::new();
    config.out_dir(&out_dir).protoc_executable(protoc);
    config
        .compile_protos(
            &["../../packages/proto/game.proto"],
            &["../../packages/proto"],
        )
        .map_err(|error| {
            std::io::Error::other(format!(
                "protocol generation failed with protoc-bin-vendored {PROTOC_BIN_VENDORED_VERSION} and prost-build {PROST_BUILD_VERSION}; input: packages/proto/game.proto; output target: {}; original error: {error}",
                out_dir.display()
            ))
        })?;
    Ok(())
}
