use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for proto in [
        "game.proto",
        "game_item.proto",
        "game_activity.proto",
        "chat.proto",
        "match.proto",
    ] {
        println!("cargo:rerun-if-changed=../../packages/proto/{proto}");
    }
    println!("cargo:rerun-if-changed=proto/loadtest_control.proto");
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    unsafe { env::set_var("PROTOC", &protoc) };
    tonic_build::configure()
        .build_server(false)
        .compile_protos(
            &[
                PathBuf::from("../../packages/proto/game.proto"),
                PathBuf::from("../../packages/proto/chat.proto"),
                PathBuf::from("../../packages/proto/match.proto"),
            ],
            &[PathBuf::from("../../packages/proto")],
        )?;
    tonic_build::configure().build_server(true).compile_protos(
        &[PathBuf::from("proto/loadtest_control.proto")],
        &[PathBuf::from("proto")],
    )?;
    emit_git_commit();
    let _ = env::var_os("OUT_DIR");
    Ok(())
}

fn emit_git_commit() {
    let root = PathBuf::from("../..");
    println!(
        "cargo:rerun-if-changed={}",
        root.join(".git/HEAD").display()
    );
    let metadata = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()));

    let value = metadata.unwrap_or_else(|| "build_metadata_missing:git_rev_parse_failed".into());
    println!("cargo:rustc-env=MYSERVER_GIT_COMMIT={value}");

    if let Ok(head) = fs::read_to_string(root.join(".git/HEAD")) {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            println!(
                "cargo:rerun-if-changed={}",
                root.join(".git").join(reference).display()
            );
        }
    }
}
