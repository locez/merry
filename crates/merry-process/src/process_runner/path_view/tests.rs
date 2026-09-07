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
    let source = view.source(&imported).unwrap().unwrap();
    assert_eq!(fs::read_to_string(source).unwrap(), "explicit import");
    let source = view.source(Path::new("/tmp/plain.conf")).unwrap().unwrap();
    assert_eq!(fs::read_to_string(source).unwrap(), "mapped temporary file");
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
    let source = view.source(Path::new("/tmp/linked.conf")).unwrap().unwrap();
    assert_eq!(source, temporary.join("target.conf"));
    assert_eq!(fs::read_to_string(source).unwrap(), "planned namespace");
}
