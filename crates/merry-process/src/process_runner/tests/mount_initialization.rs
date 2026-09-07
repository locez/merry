use crate::{BwrapProcessEnvironment, BwrapProcessRunner};
use merry_runtime::{
    PathAccess, PathAccessRule, PathAccessRuleSource, ProcessActionIntent, ProcessEnvPolicy,
    ProcessRunner, ProcessRunnerContext,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn missing_optional_development_paths_do_not_require_writable_home() {
    let fixture = tempfile::tempdir().expect("fixture");
    let home = fixture.path().join("home");
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir(&home).expect("home");
    std::fs::create_dir(&workspace).expect("workspace");
    let environment =
        BwrapProcessEnvironment::new("/usr/bin:/bin", &home, "/tmp").expect("environment");
    let mut rules = vec![PathAccessRule::new(
        fixture.path(),
        PathAccess::ReadOnly,
        PathAccessRuleSource::TrustedGlobalConfig,
    )];
    for suffix in [".rustup/toolchains", ".cargo/bin", ".local/bin", ".cache"] {
        rules.push(PathAccessRule::new(
            home.join(suffix),
            PathAccess::ReadOnly,
            PathAccessRuleSource::DefaultDevelopmentBaseline,
        ));
    }
    let runner = BwrapProcessRunner::new_at_workspace_root(&workspace)
        .with_environment(environment)
        .with_path_rules(rules);
    let intent = ProcessActionIntent::new(
        vec!["/usr/bin/printf".to_owned(), "command started".to_owned()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        4096,
    )
    .expect("intent");
    let output = runner
        .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
        .await
        .expect("runner");
    assert!(output.ok(), "sandbox failed: {output:?}");
    assert_eq!(output.stdout_text(), "command started");
    assert_eq!(std::fs::read_dir(&home).expect("home entries").count(), 0);
}

#[tokio::test]
async fn missing_deny_target_is_rejected_before_execution_without_creating_host_paths() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    let target = home.join("absent");
    let runner = BwrapProcessRunner::new_at_workspace_root(&workspace)
        .with_environment(BwrapProcessEnvironment::new("/usr/bin:/bin", &home, "/tmp").unwrap())
        .with_path_rules([
            PathAccessRule::new(
                &home,
                PathAccess::ReadOnly,
                PathAccessRuleSource::TrustedGlobalConfig,
            ),
            PathAccessRule::new(
                &target,
                PathAccess::Deny,
                PathAccessRuleSource::TrustedGlobalConfig,
            ),
        ]);
    let intent = ProcessActionIntent::new(
        vec!["/usr/bin/touch".into(), "action-ran".into()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        4096,
    )
    .unwrap();
    let error = runner
        .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("create the configured target first"),
        "{error}"
    );
    assert!(!target.exists());
    assert!(!workspace.join("action-ran").exists());
}

#[tokio::test]
async fn denied_file_is_empty_but_its_sibling_remains_readable() {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("home");
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir(&home).unwrap();
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(home.join("denied"), "synthetic protected content").unwrap();
    std::fs::write(home.join("sibling"), "public sibling").unwrap();
    let runner = BwrapProcessRunner::new_at_workspace_root(&workspace)
        .with_environment(BwrapProcessEnvironment::new("/usr/bin:/bin", &home, "/tmp").unwrap())
        .with_path_rules([
            PathAccessRule::new(
                &home,
                PathAccess::ReadOnly,
                PathAccessRuleSource::TrustedGlobalConfig,
            ),
            PathAccessRule::new(
                home.join("denied"),
                PathAccess::Deny,
                PathAccessRuleSource::TrustedGlobalConfig,
            ),
        ]);
    let intent = ProcessActionIntent::new(
        vec![
            "/bin/sh".into(),
            "-eu".into(),
            "-c".into(),
            "test ! -s \"$HOME/denied\"; cat \"$HOME/sibling\"".into(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        4096,
    )
    .unwrap();
    let output = runner
        .run(intent, ProcessRunnerContext::new(CancellationToken::new()))
        .await
        .unwrap();
    assert!(output.ok(), "{output:?}");
    assert_eq!(output.stdout_text(), "public sibling");
    assert_eq!(
        std::fs::read_to_string(home.join("denied")).unwrap(),
        "synthetic protected content"
    );
}
