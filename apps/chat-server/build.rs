fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_files = std::fs::read_dir("src/proto")?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "proto"))
        .map(|e| e.path())
        .collect::<Vec<_>>();

    if !proto_files.is_empty() {
        let mut prost_build = prost_build::Config::new();
        prost_build.type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]");
        // 生成到 src/proto 目录
        std::fs::create_dir_all("src/proto").unwrap();
        prost_build.out_dir("src/proto").compile_protos(&proto_files, &["src/proto"])?;
    }
    Ok(())
}
