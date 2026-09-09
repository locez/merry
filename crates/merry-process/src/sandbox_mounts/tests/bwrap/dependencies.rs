use super::*;
use std::{io, os::unix::fs::PermissionsExt, path::PathBuf};

fn linked_roots(fixture: &Path, target: &Path) -> SandboxMountPlan {
    let mut plan = system_plan();
    for (name, access) in [
        ("readonly", PathAccess::ReadOnly),
        ("writable", PathAccess::ReadWrite),
    ] {
        let directory = fixture.join(name);
        fs::create_dir(&directory).unwrap();
        symlink(target, directory.join("link")).unwrap();
        plan.bind(&directory, &Path::new("/").join(name), access, false)
            .unwrap();
    }
    plan
}

fn root_orders() -> [[PathBuf; 2]; 2] {
    [
        ["/readonly".into(), "/writable".into()],
        ["/writable".into(), "/readonly".into()],
    ]
}

#[test]
fn dependency_access_merges_independently_of_scan_order_within_the_grant() {
    for directory in [false, true] {
        for granted in [PathAccess::ReadOnly, PathAccess::ReadWrite] {
            let fixture = tempfile::tempdir().unwrap();
            let target = fixture.path().join("target");
            let file = if directory {
                fs::create_dir(&target).unwrap();
                target.join("file")
            } else {
                target.clone()
            };
            fs::write(&file, "content").unwrap();
            let mut base = linked_roots(fixture.path(), &target);
            base.bind(&target, Path::new("/approved"), granted, false)
                .unwrap();
            for roots in root_orders() {
                let plan = base.clone().complete(&roots).unwrap();
                assert!(plan.issues().is_empty(), "{:?}", plan.issues());
                let mut args = Vec::new();
                plan.append_args(&mut args, |_| false).unwrap();
                let flag = if granted == PathAccess::ReadWrite {
                    "--bind"
                } else {
                    "--ro-bind"
                };
                let mounts = args
                    .windows(3)
                    .filter(|args| {
                        (args[0] == "--bind" || args[0] == "--ro-bind")
                            && args[2] == target.as_os_str()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(mounts.len(), 1, "{roots:?}");
                assert_eq!(mounts[0][0], flag, "{roots:?}");
                let link = if directory {
                    Path::new("/writable/link/file")
                } else {
                    Path::new("/writable/link")
                };
                let script = if granted == PathAccess::ReadWrite {
                    "test -L /readonly/link; test -L /writable/link; printf changed > \"$1\"; cat \"$1\""
                } else {
                    "test -L /readonly/link; test -L /writable/link; test ! -w \"$1\"; cat \"$1\""
                };
                assert_eq!(
                    execute(&plan, script, &[link]),
                    if granted == PathAccess::ReadWrite {
                        "changed"
                    } else {
                        "content"
                    }
                );
            }
        }
    }
}

#[test]
fn readonly_dependency_does_not_use_unrequested_write_access() {
    let fixture = tempfile::tempdir().unwrap();
    let target = fixture.path().join("target");
    fs::write(&target, "readonly").unwrap();
    let mut plan = linked_roots(fixture.path(), &target);
    plan.bind(
        &target,
        Path::new("/approved"),
        PathAccess::ReadWrite,
        false,
    )
    .unwrap();
    let plan = plan.complete(&["/readonly".into()]).unwrap();
    assert_eq!(
        execute(&plan, "test ! -w /readonly/link; cat /readonly/link", &[]),
        "readonly"
    );
}

#[test]
fn explicit_readonly_destination_is_not_upgraded_by_a_dependency() {
    let fixture = tempfile::tempdir().unwrap();
    let target = fixture.path().join("target");
    fs::write(&target, "readonly").unwrap();
    let mut base = linked_roots(fixture.path(), &target);
    base.bind(&target, &target, PathAccess::ReadOnly, false)
        .unwrap();
    base.bind(
        &target,
        Path::new("/approved"),
        PathAccess::ReadWrite,
        false,
    )
    .unwrap();
    for roots in root_orders() {
        let plan = base.clone().complete(&roots).unwrap();
        assert_eq!(
            execute(&plan, "test ! -w /writable/link; cat /writable/link", &[]),
            "readonly"
        );
    }
}

#[test]
fn writable_file_dependency_does_not_upgrade_its_readonly_parent() {
    let fixture = tempfile::tempdir().unwrap();
    let target = fixture.path().join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("file"), "writable").unwrap();
    fs::write(target.join("other"), "readonly").unwrap();
    let mut base = linked_roots(fixture.path(), &target);
    fs::remove_file(fixture.path().join("writable/link")).unwrap();
    symlink(target.join("file"), fixture.path().join("writable/link")).unwrap();
    base.bind(
        &target,
        Path::new("/approved"),
        PathAccess::ReadWrite,
        false,
    )
    .unwrap();
    for roots in root_orders() {
        let plan = base.clone().complete(&roots).unwrap();
        assert_eq!(
            execute(
                &plan,
                "test ! -w /readonly/link/other; test -w /writable/link; printf changed > /writable/link; cat /readonly/link/file",
                &[]
            ),
            "changed"
        );
    }
}

#[test]
fn upgraded_directory_propagates_access_to_discovered_dependencies() {
    let fixture = tempfile::tempdir().unwrap();
    let target = fixture.path().join("target");
    let bridge = fixture.path().join("bridge");
    let file = fixture.path().join("file");
    fs::create_dir(&target).unwrap();
    fs::create_dir(&bridge).unwrap();
    fs::write(&file, "writable").unwrap();
    symlink(&file, target.join("link")).unwrap();
    symlink(&target, bridge.join("link")).unwrap();
    let mut base = linked_roots(fixture.path(), &target);
    fs::remove_file(fixture.path().join("writable/link")).unwrap();
    symlink(&bridge, fixture.path().join("writable/link")).unwrap();
    for source in [&target, &bridge, &file] {
        base.bind(
            source,
            &Path::new("/approved").join(source.file_name().unwrap()),
            PathAccess::ReadWrite,
            false,
        )
        .unwrap();
    }
    for roots in root_orders() {
        let plan = base.clone().complete(&roots).unwrap();
        assert!(plan.issues().is_empty(), "{:?}", plan.issues());
        assert_eq!(
            execute(
                &plan,
                "test -L /writable/link/link/link; printf changed > /writable/link/link/link; cat /readonly/link/link",
                &[]
            ),
            "changed"
        );
    }
}

#[test]
fn upgraded_directory_retains_explicit_descendant_restrictions() {
    let fixture = tempfile::tempdir().unwrap();
    let target = fixture.path().join("target");
    fs::create_dir(&target).unwrap();
    for name in ["readonly", "writable", "private"] {
        fs::write(target.join(name), name).unwrap();
    }
    let mut base = linked_roots(fixture.path(), &target);
    base.bind(
        &target,
        Path::new("/approved"),
        PathAccess::ReadWrite,
        false,
    )
    .unwrap();
    for (name, access) in [
        ("readonly", PathAccess::ReadOnly),
        ("private", PathAccess::Deny),
    ] {
        base.bind(
            &target.join(name),
            &Path::new("/approved").join(name),
            access,
            false,
        )
        .unwrap();
    }
    for roots in root_orders() {
        let plan = base.clone().complete(&roots).unwrap();
        assert_eq!(
            execute(
                &plan,
                "test ! -w /writable/link/readonly; test ! -s /writable/link/private; printf changed > /writable/link/writable; cat /readonly/link/writable",
                &[]
            ),
            "changed"
        );
    }
    assert_eq!(
        fs::read_to_string(target.join("private")).unwrap(),
        "private"
    );
}

struct LockedDirectory(PathBuf);

impl Drop for LockedDirectory {
    fn drop(&mut self) {
        fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn inaccessible_dependency_fails_on_access_not_during_preparation() {
    let fixture = tempfile::tempdir().unwrap();
    let locked = fixture.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("file"), "inaccessible").unwrap();
    fs::write(fixture.path().join("ordinary"), "readable").unwrap();
    symlink("locked/file", fixture.path().join("link")).unwrap();
    let _restore = LockedDirectory(locked.clone());
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(
        fs::symlink_metadata(locked.join("file"))
            .unwrap_err()
            .kind(),
        io::ErrorKind::PermissionDenied,
        "this fixture requires an unprivileged test process"
    );
    let mut plan = system_plan();
    plan.bind(
        fixture.path(),
        Path::new("/input"),
        PathAccess::ReadOnly,
        false,
    )
    .unwrap();
    let plan = plan
        .complete(&["/input".into(), "/input/locked/subdir".into()])
        .unwrap();
    assert!(plan.issues().contains(&SandboxLinkIssue::Unavailable {
        path: "/input/link".into(),
    }));
    assert!(plan.issues().contains(&SandboxLinkIssue::Unavailable {
        path: "/input/locked/subdir".into(),
    }));
    assert!(matches!(
        plan.resolve(Path::new("/input/link")),
        Err(crate::SandboxMountError::Path(crate::SandboxPathError::Io { source, .. }))
            if source.kind() == io::ErrorKind::PermissionDenied
    ));
    assert_eq!(
        execute(
            &plan,
            "test -L /input/link; if cat /input/link 2>/dev/null; then exit 1; fi; cat /input/ordinary",
            &[]
        ),
        "readable"
    );
}
