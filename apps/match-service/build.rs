const PROTOC_BIN_VENDORED_VERSION: &str = "3.2.0";
const PROST_BUILD_VERSION: &str = "0.13.5";
const TONIC_BUILD_VERSION: &str = "0.12.3";
const GENERATED_FILES: &[&str] = &[
    "myserver.admin.rs",
    "myserver.game.rs",
    "myserver.matchservice.rs",
];

fn proto_out_dir(manifest_dir: &std::path::Path) -> std::path::PathBuf {
    std::env::var_os("MYSERVER_PROTO_OUT_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("src").join("proto"))
}

fn prepare_proto_out_dir(out_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    std::fs::create_dir_all(out_dir)?;
    for file_name in GENERATED_FILES {
        let generated_file = out_dir.join(file_name);
        if generated_file.exists() {
            std::fs::remove_file(generated_file)?;
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let proto_dir = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("packages/proto");

    println!("cargo:rerun-if-changed=build.rs");
    println!(
        "cargo:rerun-if-changed={}",
        proto_dir.join("game.proto").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        proto_dir.join("match.proto").display()
    );
    println!("cargo:rerun-if-env-changed=MYSERVER_PROTO_OUT_DIR");

    let out_dir = proto_out_dir(&manifest_dir);
    prepare_proto_out_dir(&out_dir)?;
    let protoc = protoc_bin_vendored::protoc_bin_path().map_err(|error| {
        std::io::Error::other(format!(
            "protocol generation requires protoc-bin-vendored {PROTOC_BIN_VENDORED_VERSION} for packages/proto/game.proto and packages/proto/match.proto; output target is {}; original error: {error}",
            out_dir.display()
        ))
    })?;
    let mut config = prost_build::Config::new();
    config.out_dir(&out_dir).protoc_executable(protoc);
    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .out_dir(&out_dir)
        .compile_protos_with_config(
            config,
            &[proto_dir.join("game.proto"), proto_dir.join("match.proto")],
            &[&proto_dir],
        )
        .map_err(|error| {
            std::io::Error::other(format!(
                "protocol generation failed with protoc-bin-vendored {PROTOC_BIN_VENDORED_VERSION}, prost-build {PROST_BUILD_VERSION}, and tonic-build {TONIC_BUILD_VERSION}; inputs: packages/proto/game.proto, packages/proto/match.proto; output target: {}; original error: {error}",
                out_dir.display()
            ))
        })?;
    Ok(())
}
