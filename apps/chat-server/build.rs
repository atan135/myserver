fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let repo_root = manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .ok_or("failed to resolve repository root")?;
    let proto_dir = repo_root.join("packages").join("proto");
    let chat_proto = proto_dir.join("chat.proto");

    println!("cargo:rerun-if-changed={}", chat_proto.display());

    let mut prost_build = prost_build::Config::new();
    prost_build.type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]");
    std::fs::create_dir_all("src/proto")?;
    prost_build
        .out_dir("src/proto")
        .compile_protos(&[chat_proto], &[proto_dir])?;
    Ok(())
}
