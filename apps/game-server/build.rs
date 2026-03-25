fn main() {
    prost_build::compile_protos(
        &["../../packages/proto/game.proto", "../../packages/proto/admin.proto"],
        &["../../packages/proto"],
    )
    .expect("failed to compile protobuf files");
}
