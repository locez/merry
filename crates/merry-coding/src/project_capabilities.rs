use std::path::Path;

pub(crate) fn project_capability_summary_for_root(root: &Path) -> Option<String> {
    let mut lines = Vec::new();
    let mut checks = Vec::new();

    if root.join("Cargo.toml").is_file() {
        lines.push(
            "Detected Rust project metadata: Cargo.toml is present at the workspace root."
                .to_owned(),
        );
        checks.push("cargo fmt --all --check");
        checks.push("cargo clippy --all-targets --all-features -- -D warnings");
        checks.push("cargo test --all");
    }

    if root.join("justfile").is_file() || root.join("Justfile").is_file() {
        lines.push(
            "Detected justfile; prefer project-provided just tasks when AGENTS.md or user instructions name them."
                .to_owned(),
        );
    }

    if root.join("package.json").is_file() {
        lines.push(
            "Detected JavaScript/TypeScript project metadata: package.json is present.".to_owned(),
        );
    }

    if root.join("pyproject.toml").is_file() {
        lines.push("Detected Python project metadata: pyproject.toml is present.".to_owned());
    }

    if !checks.is_empty() {
        lines.push(format!(
            "Default Rust verification candidates if not overridden by AGENTS.md or the user: {}.",
            checks.join("; ")
        ));
    }

    if lines.is_empty() {
        return None;
    }

    Some(lines.join("\n"))
}
