const PROTOC_BIN_VENDORED_VERSION: &str = "3.2.0";
const PROST_BUILD_VERSION: &str = "0.13.5";
const GENERATED_FILES: &[&str] = &["myserver.chat.rs"];

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
    let repo_root = manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .ok_or("failed to resolve repository root")?;
    let proto_dir = repo_root.join("packages").join("proto");
    let chat_proto = proto_dir.join("chat.proto");

    println!("cargo:rerun-if-changed={}", chat_proto.display());
    println!("cargo:rerun-if-env-changed=MYSERVER_PROTO_OUT_DIR");

    let out_dir = proto_out_dir(&manifest_dir);
    prepare_proto_out_dir(&out_dir)?;
    let protoc = protoc_bin_vendored::protoc_bin_path().map_err(|error| {
        std::io::Error::other(format!(
            "protocol generation requires protoc-bin-vendored {PROTOC_BIN_VENDORED_VERSION} for packages/proto/chat.proto; output target is {}; original error: {error}",
            out_dir.display()
        ))
    })?;
    let mut prost_build = prost_build::Config::new();
    prost_build.type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]");
    prost_build.out_dir(&out_dir).protoc_executable(protoc);
    prost_build
        .compile_protos(&[chat_proto], &[proto_dir])
        .map_err(|error| {
            std::io::Error::other(format!(
                "protocol generation failed with protoc-bin-vendored {PROTOC_BIN_VENDORED_VERSION} and prost-build {PROST_BUILD_VERSION}; input: packages/proto/chat.proto; output target: {}; original error: {error}",
                out_dir.display()
            ))
        })?;
    Ok(())
}
