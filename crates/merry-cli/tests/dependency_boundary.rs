use serde_json::Value;
use std::{collections::BTreeSet, path::PathBuf, process::Command};

#[test]
fn merry_cli_depends_only_on_facade_and_optional_web_surface() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|path| path.parent())
        .expect("crates/merry-cli should sit under the workspace root");

    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(workspace_root)
        .output()
        .expect("cargo metadata should run");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let metadata: Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata should be valid JSON");
    let packages = metadata["packages"]
        .as_array()
        .expect("metadata should include workspace packages");
    let workspace_crates: BTreeSet<&str> = packages
        .iter()
        .filter_map(|pkg| pkg["name"].as_str())
        .collect();
    let cli_pkg = packages
        .iter()
        .find(|pkg| pkg["name"].as_str() == Some("merry-cli"))
        .expect("merry-cli package should exist in workspace metadata");
    let direct_workspace_deps: BTreeSet<&str> = cli_pkg["dependencies"]
        .as_array()
        .expect("merry-cli should list dependencies")
        .iter()
        .filter_map(|dep| dep["name"].as_str())
        .filter(|name| workspace_crates.contains(name))
        .collect();

    let allowed = BTreeSet::from(["merry", "merry-web"]);
    let forbidden: Vec<&str> = direct_workspace_deps
        .difference(&allowed)
        .copied()
        .collect();
    assert!(
        forbidden.is_empty(),
        "merry-cli must consume lower layers through the `merry` facade rather than depending directly on workspace crates: {forbidden:?} (all direct workspace deps: {direct_workspace_deps:?})"
    );
    assert!(
        direct_workspace_deps.contains("merry"),
        "merry-cli must depend on the `merry` facade"
    );
}
