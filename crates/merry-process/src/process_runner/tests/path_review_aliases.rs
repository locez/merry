use super::{
    path_review::{execute, runners},
    permission_request,
};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, PermissionedProcessRunnerFactory,
};
use std::{fs, path::Path, process::Command};

const CHILD_ENV: &str = "MERRY_REVIEW_ALIAS_TEST_ROOT";
const TEST_NAME: &str =
    "process_runner::tests::path_review_aliases::inherited_bind_aliases_cannot_bypass_review";

#[test]
fn inherited_bind_aliases_cannot_bypass_review() {
    if let Some(root) = std::env::var_os(CHILD_ENV) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(assert_aliases(Path::new(&root)));
        return;
    }
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path();
    for directory in ["abc/d/nested", "workspace", "alias", "partial"] {
        fs::create_dir_all(root.join(directory)).unwrap();
    }
    fs::write(root.join("abc/d/token"), "protected evidence").unwrap();
    fs::write(root.join("abc/d/nested/token"), "nested evidence").unwrap();
    let output = Command::new("bwrap")
        .args([
            "--unshare-user",
            "--unshare-pid",
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
        ])
        .arg("--bind")
        .arg(root)
        .arg(root)
        .arg("--bind")
        .arg(root.join("abc/d"))
        .arg(root.join("alias"))
        .arg("--bind")
        .arg(root.join("abc/d/nested"))
        .arg(root.join("partial"))
        .args(["--proc", "/proc", "--dev", "/dev", "--"])
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_ENV, root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn assert_aliases(root: &Path) {
    let (runner, factory) = runners(root, PathAccess::ReadOnly, root.join("abc/d"));
    let hidden = execute(&runner, "test ! -f \"$HOME/abc/d/token\" && test ! -f \"$HOME/alias/token\" && test ! -f \"$HOME/partial/token\"").await;
    assert!(hidden.ok(), "{hidden:?}");
    let parent_request = permission_request(serde_json::json!({
        "requested": { "paths": [{ "path": root, "access": "ro" }] },
        "for_action": { "command": "test ! -f \"$HOME/alias/token\" && test ! -f \"$HOME/partial/token\"", "cwd": null }
    }));
    factory.validate_request(&parent_request).unwrap();
    let parent = execute(
        factory.runner_for(&parent_request).as_ref(),
        "test ! -f \"$HOME/alias/token\" && test ! -f \"$HOME/partial/token\"",
    )
    .await;
    assert!(
        parent.ok(),
        "parent approval exposed a reviewed bind alias: {parent:?}"
    );
    let script = "cat \"$HOME/abc/d/token\"; cat \"$HOME/alias/token\"; test ! -f \"$HOME/partial/token\"; if printf changed > \"$HOME/alias/token\"; then exit 1; fi";
    let request = permission_request(serde_json::json!({
        "requested": { "paths": [{ "path": root.join("alias/token"), "access": "rw" }] },
        "for_action": { "command": script, "cwd": null }
    }));
    assert!(
        !factory
            .request_capabilities_are_satisfied(&request)
            .unwrap()
    );
    factory.validate_request(&request).unwrap();
    let output = execute(factory.runner_for(&request).as_ref(), script).await;
    assert!(output.ok(), "{output:?}");
    assert_eq!(output.stdout_text(), "protected evidenceprotected evidence");
    let nested_factory = factory.backend().clone().with_path_rules([
        PathAccessRule::new(
            root.join("abc"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            root.join("abc/d"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        )
        .with_review_required(),
        PathAccessRule::new(
            root.join("abc/d/nested"),
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        )
        .with_review_required(),
    ]);
    let request = permission_request(serde_json::json!({
        "requested": { "paths": [{ "path": root.join("abc/d"), "access": "ro" }] },
        "for_action": { "command": "test -f \"$HOME/alias/token\" && test ! -f \"$HOME/partial/token\"", "cwd": null }
    }));
    let nested = execute(nested_factory.runner_for(&request).as_ref(), "test -f \"$HOME/alias/token\" && test ! -f \"$HOME/partial/token\" && test ! -f \"$HOME/alias/nested/token\"").await;
    assert!(
        nested.ok(),
        "nested review exposed through a partial bind alias: {nested:?}"
    );
}
