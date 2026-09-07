use super::{permission_request, request_process_intent};
use crate::{
    BwrapPermissionedProcessRunnerFactory, BwrapProcessEnvironment, BwrapProcessRunner,
    BwrapSessionPermissions,
};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, PermissionedProcessRunnerFactory,
    ProcessActionIntent, ProcessEnvPolicy, ProcessRunner, ProcessRunnerContext,
    ProcessRunnerOutput,
};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

struct Fixture {
    directory: tempfile::TempDir,
    runner: BwrapProcessRunner,
    factory: merry_runtime::SessionPermissionedProcessRunnerFactory<
        BwrapPermissionedProcessRunnerFactory,
    >,
}

impl Fixture {
    fn new(access: PathAccess) -> Self {
        let directory = tempfile::tempdir().expect("fixture");
        let root = directory.path();
        fs::create_dir_all(root.join("abc/d")).unwrap();
        fs::create_dir(root.join("workspace")).unwrap();
        fs::write(root.join("abc/e"), "public evidence").unwrap();
        fs::write(root.join("abc/d/token"), "protected evidence").unwrap();
        fs::write(root.join("abc/d/other"), "protected neighbor").unwrap();
        fs::write(root.join("workspace/read.sh"), "cat \"$HOME/abc/d/token\"").unwrap();
        let (runner, factory) = runners(root, access, root.join("abc/d"));
        Self {
            directory,
            runner,
            factory,
        }
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }
}

pub(super) fn runners(
    root: &Path,
    access: PathAccess,
    reviewed: PathBuf,
) -> (
    BwrapProcessRunner,
    merry_runtime::SessionPermissionedProcessRunnerFactory<BwrapPermissionedProcessRunnerFactory>,
) {
    let environment = BwrapProcessEnvironment::new("/usr/bin:/bin", root, "/tmp").unwrap();
    let permissions = BwrapSessionPermissions::new();
    let rules = vec![
        PathAccessRule::new(
            root.join("abc"),
            access,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(reviewed, access, PathAccessRuleSource::TrustedGlobalConfig)
            .with_review_required(),
    ];
    let runner = BwrapProcessRunner::new_at_workspace_root(root.join("workspace"))
        .with_environment(environment.clone())
        .with_path_rules(rules.clone())
        .with_session_permissions(permissions.clone());
    let factory =
        BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(root.join("workspace"))
            .with_environment(environment)
            .with_path_rules(rules)
            .with_session_permissions(permissions);
    (runner, factory)
}

pub(super) async fn execute(runner: &dyn ProcessRunner, script: &str) -> ProcessRunnerOutput {
    let intent = ProcessActionIntent::new(
        vec!["/bin/sh".into(), "-c".into(), script.into()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        4096,
        4096,
    )
    .unwrap();
    runner
        .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
        .await
        .expect("sandbox execution")
}

#[tokio::test]
async fn readonly_ceiling_cannot_be_reopened_by_configured_writable_descendants() {
    let fixture = Fixture::new(PathAccess::ReadOnly);
    let root = fixture.root();
    let runner = BwrapProcessRunner::new_at_workspace_root(root.join("workspace"))
        .with_environment(BwrapProcessEnvironment::new("/usr/bin:/bin", root, "/tmp").unwrap())
        .with_path_rules([
            PathAccessRule::new(
                root.join("abc"),
                PathAccess::ReadOnly,
                PathAccessRuleSource::TrustedGlobalConfig,
            ),
            PathAccessRule::new(
                root.join("abc/d"),
                PathAccess::ReadWrite,
                PathAccessRuleSource::TrustedGlobalConfig,
            ),
        ]);
    let output = execute(
        &runner,
        "cat \"$HOME/abc/d/token\"; if printf changed > \"$HOME/abc/d/token\"; then exit 1; fi",
    )
    .await;
    assert!(output.ok(), "{output:?}");
    assert_eq!(output.stdout_text(), "protected evidence");
}

#[tokio::test]
async fn remapped_tmp_does_not_expose_a_second_unreviewed_path() {
    let fixture = Fixture::new(PathAccess::ReadOnly);
    let root = fixture.root();
    let environment = BwrapProcessEnvironment::new("/usr/bin:/bin", root, root).unwrap();
    let runner = fixture.runner.clone().with_environment(environment.clone());
    let output = execute(
        &runner,
        "test ! -f /tmp/abc/d/token && test ! -f \"$HOME/abc/d/token\"",
    )
    .await;
    assert!(output.ok(), "{output:?}");
    let factory = fixture
        .factory
        .backend()
        .clone()
        .with_environment(environment);
    let request = permission_request(serde_json::json!({
        "requested": { "paths": [{ "path": root.join("abc/d/token"), "access": "ro" }] },
        "for_action": { "command": "cat /tmp/abc/d/token", "cwd": null }
    }));
    let output = execute(
        factory.runner_for(&request).as_ref(),
        "cat /tmp/abc/d/token; test ! -f /tmp/abc/d/other",
    )
    .await;
    assert!(output.ok(), "{output:?}");
    assert_eq!(output.stdout_text(), "protected evidence");
}

#[tokio::test]
async fn a_missing_review_target_reports_a_setup_error_instead_of_starting_the_command() {
    let fixture = Fixture::new(PathAccess::ReadOnly);
    let (runner, _) = runners(
        fixture.root(),
        PathAccess::ReadOnly,
        fixture.root().join("abc/missing"),
    );
    let intent = ProcessActionIntent::new(
        vec!["/usr/bin/touch".into(), "executed".into()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .unwrap();
    let error = runner
        .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cannot protect sandbox path"));
    assert!(!fixture.root().join("workspace/executed").exists());
}

#[tokio::test]
async fn review_masks_one_subtree_but_preserves_readonly_siblings_and_script_enforcement() {
    let fixture = Fixture::new(PathAccess::ReadOnly);
    let output = execute(&fixture.runner, "cat \"$HOME/abc/e\"; test ! -f \"$HOME/abc/d/token\"; if /bin/sh read.sh; then exit 1; fi; if printf changed > \"$HOME/abc/e\"; then exit 2; fi").await;
    assert!(output.ok(), "{output:?}");
    assert_eq!(output.stdout_text(), "public evidence");
    assert_eq!(
        fs::read_to_string(fixture.root().join("abc/e")).unwrap(),
        "public evidence"
    );
}

#[tokio::test]
async fn reviewed_file_grant_is_exact_readonly_and_never_reused() {
    let fixture = Fixture::new(PathAccess::ReadOnly);
    let request = permission_request(serde_json::json!({
        "requested": { "paths": [{ "path": fixture.root().join("abc/d/token"), "access": "ro" }] },
        "for_action": { "command": "cat \"$HOME/abc/d/token\"; test ! -f \"$HOME/abc/d/other\"; if printf changed > \"$HOME/abc/d/token\"; then exit 1; fi", "cwd": null }
    }));
    assert!(
        !fixture
            .factory
            .request_capabilities_are_satisfied(&request)
            .unwrap()
    );
    fixture.factory.validate_request(&request).unwrap();
    let runner = fixture.factory.runner_for(&request);
    let output = runner
        .run(
            request_process_intent(&request).clone(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    assert!(output.ok(), "{output:?}");
    assert_eq!(output.stdout_text(), "protected evidence");
    assert!(
        !fixture
            .factory
            .request_capabilities_are_satisfied(&request)
            .unwrap()
    );
    assert!(
        !execute(&fixture.runner, "cat \"$HOME/abc/d/token\"")
            .await
            .ok()
    );
}

#[tokio::test]
async fn configured_readwrite_is_preauthorized_but_review_grants_remain_action_scoped() {
    let fixture = Fixture::new(PathAccess::ReadWrite);
    assert!(
        execute(&fixture.runner, "printf changed > \"$HOME/abc/e\"")
            .await
            .ok()
    );
    assert!(
        !execute(&fixture.runner, "printf denied > \"$HOME/abc/d/token\"")
            .await
            .ok()
    );
    let request = permission_request(serde_json::json!({
        "requested": { "paths": [{ "path": fixture.root().join("abc/d"), "access": "rw" }] },
        "for_action": { "command": "printf approved > \"$HOME/abc/d/token\"", "cwd": null }
    }));
    let runner = fixture.factory.runner_for(&request);
    let output = runner
        .run(
            request_process_intent(&request).clone(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    assert!(output.ok(), "{output:?}");
    assert_eq!(
        fs::read_to_string(fixture.root().join("abc/d/token")).unwrap(),
        "approved"
    );
    assert!(
        !execute(&fixture.runner, "cat \"$HOME/abc/d/token\"")
            .await
            .ok()
    );
}

#[tokio::test]
async fn broader_approvals_do_not_unlock_reviewed_descendants() {
    let fixture = Fixture::new(PathAccess::ReadOnly);
    let request = permission_request(serde_json::json!({
        "requested": { "paths": [{ "path": fixture.root(), "access": "ro" }] },
        "for_action": { "command": "cat \"$HOME/abc/e\"; test ! -f \"$HOME/abc/d/token\"", "cwd": null }
    }));
    fixture.factory.validate_request(&request).unwrap();
    let runner = fixture.factory.runner_for(&request);
    for runner in [runner.as_ref(), &fixture.runner as &dyn ProcessRunner] {
        let output = execute(
            runner,
            "cat \"$HOME/abc/e\"; test ! -f \"$HOME/abc/d/token\"",
        )
        .await;
        assert!(output.ok(), "{output:?}");
        assert_eq!(output.stdout_text(), "public evidence");
    }
}

#[tokio::test]
async fn nested_review_and_private_denials_survive_parent_approval() {
    let fixture = Fixture::new(PathAccess::ReadWrite);
    let root = fixture.root();
    fs::create_dir(root.join("abc/d/nested")).unwrap();
    fs::create_dir(root.join("abc/d/private")).unwrap();
    fs::write(root.join("abc/d/nested/token"), "nested evidence").unwrap();
    fs::write(root.join("abc/d/private/token"), "private evidence").unwrap();
    let rules = vec![
        PathAccessRule::new(
            root.join("abc"),
            PathAccess::ReadWrite,
            PathAccessRuleSource::TrustedGlobalConfig,
        ),
        PathAccessRule::new(
            root.join("abc/d"),
            PathAccess::ReadWrite,
            PathAccessRuleSource::TrustedGlobalConfig,
        )
        .with_review_required(),
        PathAccessRule::new(
            root.join("abc/d/nested"),
            PathAccess::ReadWrite,
            PathAccessRuleSource::TrustedGlobalConfig,
        )
        .with_review_required(),
        PathAccessRule::new(
            root.join("abc/d/private"),
            PathAccess::Deny,
            PathAccessRuleSource::ProductPrivate,
        ),
    ];
    let factory =
        BwrapPermissionedProcessRunnerFactory::new_at_workspace_root(root.join("workspace"))
            .with_environment(BwrapProcessEnvironment::new("/usr/bin:/bin", root, "/tmp").unwrap())
            .with_path_rules(rules);
    for (path, script) in [
        (
            root.join("abc/d"),
            "cat \"$HOME/abc/d/token\"; test ! -f \"$HOME/abc/d/nested/token\"; test ! -f \"$HOME/abc/d/private/token\"",
        ),
        (
            root.join("abc/d/nested"),
            "printf approved > \"$HOME/abc/d/nested/token\"; test ! -f \"$HOME/abc/d/token\"; test ! -f \"$HOME/abc/d/private/token\"",
        ),
    ] {
        let request = permission_request(serde_json::json!({
            "requested": { "paths": [{ "path": path, "access": "rw" }] },
            "for_action": { "command": script, "cwd": null }
        }));
        factory.validate_request(&request).unwrap();
        let output = execute(factory.runner_for(&request).as_ref(), script).await;
        assert!(output.ok(), "{output:?}");
    }
    assert_eq!(
        fs::read_to_string(root.join("abc/d/nested/token")).unwrap(),
        "approved"
    );
    let denied = permission_request(serde_json::json!({
        "requested": { "paths": [{ "path": root.join("abc/d/private/token"), "access": "ro" }] },
        "for_action": { "command": "true", "cwd": null }
    }));
    assert!(factory.validate_request(&denied).is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_aliases_cannot_bypass_review_or_promote_the_access_ceiling() {
    let fixture = Fixture::new(PathAccess::ReadOnly);
    let alias = fixture.root().join("alias");
    std::os::unix::fs::symlink(fixture.root().join("abc/d"), &alias).unwrap();
    assert!(
        !execute(&fixture.runner, "cat \"$HOME/alias/token\"")
            .await
            .ok()
    );
    let request = permission_request(serde_json::json!({
        "requested": { "paths": [{ "path": alias, "access": "rw" }] },
        "for_action": { "command": "cat \"$HOME/alias/token\"; if printf changed > \"$HOME/alias/token\"; then exit 1; fi", "cwd": null }
    }));
    assert!(
        !fixture
            .factory
            .request_capabilities_are_satisfied(&request)
            .unwrap()
    );
    let output = fixture
        .factory
        .runner_for(&request)
        .run(
            request_process_intent(&request).clone(),
            ProcessRunnerContext::new(CancellationToken::new()),
        )
        .await
        .unwrap();
    assert!(output.ok(), "{output:?}");
    assert_eq!(output.stdout_text(), "protected evidence");
}

#[tokio::test]
async fn review_can_protect_a_file_without_masking_its_parent() {
    let fixture = Fixture::new(PathAccess::ReadOnly);
    let (runner, _) = runners(
        fixture.root(),
        PathAccess::ReadOnly,
        fixture.root().join("abc/d/token"),
    );
    let output = execute(
        &runner,
        "cat \"$HOME/abc/d/token\"; cat \"$HOME/abc/d/other\"",
    )
    .await;
    assert!(output.ok(), "{output:?}");
    assert_eq!(output.stdout_text(), "protected neighbor");
}
