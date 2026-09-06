use std::{fs, path::Path, process::Command};

#[test]
fn tool_macro_supports_external_consumers_and_rejects_invalid_handlers() {
    let project = tempfile::tempdir().expect("temporary consumer project");
    let facade = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = format!(
        r#"[package]
name = "merry-macro-consumer"
version = "0.0.0"
edition = "2024"
[dependencies]
sdk = {{ package = "merry", path = {facade:?}, default-features = false }}
serde = {{ version = "1", features = ["derive"] }}
schemars = {{ version = "1", features = ["derive"] }}
"#
    );
    fs::write(project.path().join("Cargo.toml"), manifest).expect("write consumer manifest");
    fs::create_dir(project.path().join("src")).expect("create consumer sources");
    let source = project.path().join("src/main.rs");
    fs::write(&source, include_str!("fixtures/tool_macro_consumer.rs"))
        .expect("write valid consumer");
    let target = facade.join("../../target/tool-macro-consumer");
    let run = |operation: &str| {
        Command::new(env!("CARGO"))
            .arg(operation)
            .args(["--offline", "--quiet", "--manifest-path"])
            .arg(project.path().join("Cargo.toml"))
            .arg("--target-dir")
            .arg(&target)
            .output()
            .expect("run consumer Cargo command")
    };
    let output = run("run");
    assert!(
        output.status.success(),
        "consumer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    for invalid in [
        "#[sdk::tool(description = \"invalid\")] fn handler(input: String) {} fn main() {}",
        "#[derive(serde::Deserialize, schemars::JsonSchema)] struct Input {} #[sdk::tool(description = \"invalid\")] async fn handler(_input: Input) -> String { String::new() } fn main() {}",
    ] {
        fs::write(&source, invalid).expect("write invalid consumer");
        assert!(
            !run("check").status.success(),
            "invalid handler must not compile"
        );
    }
}
