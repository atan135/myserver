fn main() -> Result<(), Box<dyn std::error::Error>> {
    // packages/proto 在项目根目录
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")?;
    let proto_dir = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("packages/proto");

    let proto_files = std::fs::read_dir(&proto_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "proto"))
        .filter(|e| e.file_name().to_string_lossy().starts_with("match"))
        .map(|e| e.path())
        .collect::<Vec<_>>();

    if !proto_files.is_empty() {
        tonic_build::configure()
            .build_server(true)
            .build_client(true)
            .out_dir("src/proto")
            .compile(&proto_files, &[&proto_dir])?;
    }
    Ok(())
}
