use super::*;
use std::fs;

#[test]
fn explicit_file_imports_take_precedence_over_the_tmp_mapping() {
    let fixture = tempfile::tempdir().unwrap();
    let temporary = fixture.path().join("temporary");
    let workspace = fixture.path().join("workspace");
    let imported = fixture.path().join("imported.conf");
    fs::create_dir(&temporary).unwrap();
    fs::create_dir(&workspace).unwrap();
    fs::write(&imported, "explicit import").unwrap();
    fs::write(temporary.join("plain.conf"), "mapped temporary file").unwrap();
    let view = ActionPathView::prepare(
        &[PathAccessRule::new(
            &imported,
            PathAccess::ReadOnly,
            PathAccessRuleSource::TrustedGlobalConfig,
        )],
        &temporary,
        &workspace,
    )
    .unwrap();
    let source = view.resolve(&imported).unwrap().unwrap();
    assert_eq!(
        fs::read_to_string(source.source()).unwrap(),
        "explicit import"
    );
    let source = view.resolve(Path::new("/tmp/plain.conf")).unwrap().unwrap();
    assert_eq!(
        fs::read_to_string(source.source()).unwrap(),
        "mapped temporary file"
    );
}

#[cfg(unix)]
#[test]
fn absolute_symlinks_are_followed_in_the_action_namespace() {
    let fixture = tempfile::tempdir().unwrap();
    let temporary = fixture.path().join("temporary");
    let workspace = fixture.path().join("workspace");
    fs::create_dir(&temporary).unwrap();
    fs::create_dir(&workspace).unwrap();
    fs::write(temporary.join("target.conf"), "planned namespace").unwrap();
    std::os::unix::fs::symlink("/tmp/target.conf", temporary.join("linked.conf")).unwrap();
    let view = ActionPathView::prepare(&[], &temporary, &workspace).unwrap();
    let source = view
        .resolve(Path::new("/tmp/linked.conf"))
        .unwrap()
        .unwrap();
    assert_eq!(source.source(), temporary.join("target.conf"));
    assert_eq!(source.destination(), Path::new("/tmp/target.conf"));
    assert_eq!(
        fs::read_to_string(source.source()).unwrap(),
        "planned namespace"
    );
}

#[cfg(unix)]
#[test]
fn reviewed_links_are_not_followed_before_admission() {
    let fixture = tempfile::tempdir().unwrap();
    let temporary = fixture.path().join("temporary");
    let workspace = fixture.path().join("workspace");
    fs::create_dir(&temporary).unwrap();
    fs::create_dir(&workspace).unwrap();
    let link = workspace.join("reviewed");
    std::os::unix::fs::symlink("/etc/ssh/ssh_config", &link).unwrap();
    let rules = [PathAccessRule::new(
        &link,
        PathAccess::ReadOnly,
        PathAccessRuleSource::TrustedGlobalConfig,
    )
    .with_review_required()];
    let view = ActionPathView::prepare(&rules, &temporary, &workspace).unwrap();
    assert!(view.resolve(&link).unwrap().is_none());
    assert!(
        view.resolve(Path::new("/etc/ssh/ssh_config"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn runtime_procfs_paths_have_no_host_source_mapping() {
    let fixture = tempfile::tempdir().unwrap();
    let view = ActionPathView::prepare(&[], fixture.path(), fixture.path()).unwrap();
    assert!(
        view.resolve(Path::new("/proc/self/mounts"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        view.destination(Path::new("/proc/self/mounts")).unwrap(),
        Path::new("/proc/self/mounts")
    );
}
