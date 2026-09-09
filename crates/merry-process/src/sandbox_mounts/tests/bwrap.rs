//! Execute prepared plans in real bubblewrap processes.

use crate::{PreparedSandboxMountPlan, SandboxLinkIssue, SandboxMountPlan};
use merry_runtime::PathAccess;
use std::{ffi::OsString, fs, os::unix::fs::symlink, path::Path, process::Command};

mod snapshots;

mod dependencies;

fn system_plan() -> SandboxMountPlan {
    let mut plan = SandboxMountPlan::new();
    for path in ["/usr", "/bin", "/lib", "/lib64"] {
        if Path::new(path).exists() {
            plan.bind(
                Path::new(path),
                Path::new(path),
                PathAccess::ReadOnly,
                false,
            )
            .unwrap();
        }
    }
    plan.opaque(Path::new("/proc")).unwrap();
    plan.opaque(Path::new("/dev")).unwrap();
    plan
}

fn execute(plan: &PreparedSandboxMountPlan, script: &str, paths: &[&Path]) -> String {
    let mut args = [
        "--unshare-user",
        "--unshare-pid",
        "--die-with-parent",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--chdir",
        "/",
    ]
    .into_iter()
    .map(OsString::from)
    .collect::<Vec<_>>();
    plan.append_args(&mut args, |_| false).unwrap();
    args.extend(
        ["--", "/bin/sh", "-eu", "-c", script, "mount-test"]
            .into_iter()
            .map(OsString::from),
    );
    args.extend(paths.iter().map(|path| path.as_os_str().to_owned()));
    let output = Command::new("bwrap").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn standalone_file_links_keep_their_original_representation() {
    for relative in [false, true] {
        let fixture = tempfile::tempdir().unwrap();
        let target = fixture.path().join("target");
        let link = fixture.path().join("link");
        fs::write(&target, "public content").unwrap();
        let text = if relative {
            Path::new("target")
        } else {
            target.as_path()
        };
        symlink(text, &link).unwrap();
        let mut plan = system_plan();
        plan.bind(&link, &link, PathAccess::ReadOnly, false)
            .unwrap();
        let plan = plan.complete(&[]).unwrap();
        assert_eq!(plan.resolve(&link).unwrap().unwrap().destination(), target);
        assert_eq!(
            execute(
                &plan,
                "test -L \"$1\"; test \"$(readlink \"$1\")\" = \"$2\"; test ! -w \"$1\"; cat \"$1\"",
                &[&link, text]
            ),
            "public content"
        );
    }
}

#[test]
fn directory_coverage_avoids_redundant_file_binds() {
    let fixture = tempfile::tempdir().unwrap();
    let input = fixture.path().join("input");
    let target = fixture.path().join("target");
    fs::create_dir(&input).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(target.join("file"), "covered").unwrap();
    symlink("/target/file", input.join("link")).unwrap();
    let mut plan = system_plan();
    plan.bind(&input, Path::new("/input"), PathAccess::ReadOnly, false)
        .unwrap();
    plan.bind(&target, Path::new("/target"), PathAccess::ReadOnly, false)
        .unwrap();
    let plan = plan.complete(&["/input".into()]).unwrap();
    assert!(plan.issues().is_empty());
    let mut args = Vec::new();
    plan.append_args(&mut args, |_| false).unwrap();
    assert!(!args.iter().any(|argument| argument == "/target/file"));
    assert_eq!(
        execute(&plan, "test -L /input/link; cat /input/link", &[]),
        "covered"
    );
}

#[test]
fn directory_links_reuse_grants_at_other_locations_without_exposing_neighbours() {
    let fixture = tempfile::tempdir().unwrap();
    let input = fixture.path().join("input");
    let target = fixture.path().join("target");
    let neighbour = fixture.path().join("private");
    fs::create_dir(&input).unwrap();
    fs::write(&target, "admitted").unwrap();
    fs::write(&neighbour, "not admitted").unwrap();
    symlink(&target, input.join("first")).unwrap();
    symlink(&target, input.join("second")).unwrap();
    let mut plan = system_plan();
    plan.bind(&input, Path::new("/input"), PathAccess::ReadOnly, false)
        .unwrap();
    plan.bind(&target, Path::new("/approved"), PathAccess::ReadOnly, false)
        .unwrap();
    let plan = plan.complete(&["/input".into()]).unwrap();
    assert!(plan.issues().is_empty(), "{:?}", plan.issues());
    let mut args = Vec::new();
    plan.append_args(&mut args, |_| false).unwrap();
    assert_eq!(
        args.windows(3)
            .filter(|args| args[0] == "--ro-bind" && args[2] == target.as_os_str())
            .count(),
        1
    );
    assert_eq!(
        execute(
            &plan,
            "test ! -e \"$1\"; test -L /input/first; test -L /input/second; cat /input/first",
            &[&neighbour]
        ),
        "admitted"
    );
}

#[test]
fn missing_intermediate_aliases_are_preserved_with_the_final_target() {
    let fixture = tempfile::tempdir().unwrap();
    let input = fixture.path().join("input");
    let intermediate = fixture.path().join("intermediate");
    let target = fixture.path().join("target");
    fs::create_dir(&input).unwrap();
    fs::write(&target, "multi-hop").unwrap();
    symlink(&target, &intermediate).unwrap();
    symlink(&intermediate, input.join("link")).unwrap();
    let mut plan = system_plan();
    plan.bind(&input, Path::new("/input"), PathAccess::ReadOnly, false)
        .unwrap();
    plan.bind(&target, Path::new("/approved"), PathAccess::ReadOnly, false)
        .unwrap();
    let plan = plan.complete(&["/input".into()]).unwrap();
    assert!(plan.issues().is_empty(), "{:?}", plan.issues());
    assert_eq!(
        execute(
            &plan,
            "test -L /input/link; test -L \"$1\"; cat /input/link",
            &[&intermediate]
        ),
        "multi-hop"
    );
}

#[test]
fn external_targets_are_not_authorized_by_an_untrusted_link() {
    let fixture = tempfile::tempdir().unwrap();
    let input = fixture.path().join("input");
    let private = fixture.path().join("private");
    fs::create_dir(&input).unwrap();
    fs::write(&private, "not granted").unwrap();
    symlink(&private, input.join("link")).unwrap();
    let mut plan = system_plan();
    plan.bind(&input, Path::new("/input"), PathAccess::ReadOnly, false)
        .unwrap();
    let plan = plan.complete(&["/input".into()]).unwrap();
    assert!(matches!(
        plan.issues(),
        [SandboxLinkIssue::UnexposedTarget { .. }]
    ));
    assert_eq!(
        execute(
            &plan,
            "test -L /input/link; test ! -e /input/link; test ! -e \"$1\"",
            &[&private]
        ),
        ""
    );
}

#[test]
fn explicit_denies_remain_effective_through_links() {
    let fixture = tempfile::tempdir().unwrap();
    let input = fixture.path().join("input");
    fs::create_dir(&input).unwrap();
    fs::write(input.join("private"), "not granted").unwrap();
    symlink("/input/private", input.join("link")).unwrap();
    let mut plan = system_plan();
    plan.bind(&input, Path::new("/input"), PathAccess::ReadOnly, false)
        .unwrap();
    plan.bind(
        &input.join("private"),
        Path::new("/input/private"),
        PathAccess::Deny,
        false,
    )
    .unwrap();
    let plan = plan.complete(&["/input".into()]).unwrap();
    assert!(plan.resolve(Path::new("/input/link")).unwrap().is_none());
    assert_eq!(
        execute(&plan, "test -L /input/link; test ! -s /input/link", &[]),
        ""
    );
}

#[test]
fn procfs_links_are_not_resolved_using_the_planning_process_pid() {
    let fixture = tempfile::tempdir().unwrap();
    symlink("/proc/self/mounts", fixture.path().join("mtab")).unwrap();
    let mut plan = system_plan();
    plan.bind(
        fixture.path(),
        Path::new("/input"),
        PathAccess::ReadOnly,
        false,
    )
    .unwrap();
    let plan = plan.complete(&["/input".into()]).unwrap();
    assert!(plan.issues().is_empty());
    assert_eq!(
        plan.destination(Path::new("/input/mtab")).unwrap(),
        Path::new("/proc/self/mounts")
    );
    assert!(plan.resolve(Path::new("/input/mtab")).unwrap().is_none());
    assert_eq!(
        execute(&plan, "test -L /input/mtab; test -r /input/mtab", &[]),
        ""
    );
}

#[test]
fn dangling_and_cyclic_directory_entries_do_not_block_unrelated_files() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("ordinary"), "still readable").unwrap();
    symlink("missing", fixture.path().join("dangling")).unwrap();
    symlink("second", fixture.path().join("first")).unwrap();
    symlink("first", fixture.path().join("second")).unwrap();
    let mut plan = system_plan();
    plan.bind(
        fixture.path(),
        Path::new("/input"),
        PathAccess::ReadOnly,
        false,
    )
    .unwrap();
    let plan = plan.complete(&["/input".into()]).unwrap();
    assert!(
        plan.issues()
            .iter()
            .any(|issue| matches!(issue, SandboxLinkIssue::Cycle { .. }))
    );
    assert_eq!(
        execute(
            &plan,
            "test -L /input/dangling; test -L /input/first; cat /input/ordinary",
            &[]
        ),
        "still readable"
    );
}

#[test]
fn standalone_multihop_links_preserve_every_alias() {
    let fixture = tempfile::tempdir().unwrap();
    let first = fixture.path().join("first");
    let second = fixture.path().join("second");
    let target = fixture.path().join("target");
    fs::write(&target, "chain").unwrap();
    symlink("target", &second).unwrap();
    symlink("second", &first).unwrap();
    let mut plan = system_plan();
    plan.bind(&first, &first, PathAccess::ReadOnly, false)
        .unwrap();
    let plan = plan.complete(&[]).unwrap();
    assert_eq!(
        execute(
            &plan,
            "test -L \"$1\"; test -L \"$2\"; test ! -L \"$3\"; cat \"$1\"",
            &[&first, &second, &target]
        ),
        "chain"
    );
}

#[test]
fn covered_directory_targets_are_scanned_only_when_explicitly_requested() {
    let fixture = tempfile::tempdir().unwrap();
    let input = fixture.path().join("input");
    let target = fixture.path().join("target");
    let final_file = fixture.path().join("final");
    fs::create_dir(&input).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(&final_file, "nested").unwrap();
    symlink("/target", input.join("directory")).unwrap();
    symlink(&final_file, target.join("link")).unwrap();
    for scan_target in [false, true] {
        let mut plan = system_plan();
        plan.bind(&input, Path::new("/input"), PathAccess::ReadOnly, false)
            .unwrap();
        plan.bind(&target, Path::new("/target"), PathAccess::ReadOnly, false)
            .unwrap();
        plan.bind(
            &final_file,
            Path::new("/approved"),
            PathAccess::ReadOnly,
            false,
        )
        .unwrap();
        let mut roots = vec!["/input".into()];
        if scan_target {
            roots.push("/target".into());
        }
        let plan = plan.complete(&roots).unwrap();
        assert!(plan.issues().is_empty(), "{:?}", plan.issues());
        assert_eq!(plan.resolve(&final_file).unwrap().is_some(), scan_target);
        let script = if scan_target {
            "test -L /input/directory; test -L /input/directory/link; cat /input/directory/link"
        } else {
            "test -L /input/directory; test -L /input/directory/link; test ! -e /input/directory/link; cat /approved"
        };
        assert_eq!(execute(&plan, script, &[]), "nested");
    }
}

#[test]
fn directory_dependencies_retain_descendant_denials_and_readonly_limits() {
    let fixture = tempfile::tempdir().unwrap();
    let input = fixture.path().join("input");
    let target = fixture.path().join("target");
    fs::create_dir(&input).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(target.join("private"), "denied").unwrap();
    fs::write(target.join("readonly"), "readable").unwrap();
    fs::write(target.join("writable"), "writable").unwrap();
    symlink(&target, input.join("directory")).unwrap();
    let mut plan = system_plan();
    plan.bind(&input, Path::new("/input"), PathAccess::ReadWrite, false)
        .unwrap();
    plan.bind(
        &target,
        Path::new("/approved"),
        PathAccess::ReadWrite,
        false,
    )
    .unwrap();
    plan.bind(
        &target.join("private"),
        Path::new("/approved/private"),
        PathAccess::Deny,
        false,
    )
    .unwrap();
    plan.bind(
        &target.join("readonly"),
        Path::new("/approved/readonly"),
        PathAccess::ReadOnly,
        false,
    )
    .unwrap();
    let plan = plan.complete(&["/input".into()]).unwrap();
    assert!(
        plan.resolve(Path::new("/input/directory/private"))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        execute(
            &plan,
            "test ! -s /input/directory/private; test ! -w /input/directory/readonly; test -w /input/directory/writable; cat /input/directory/readonly",
            &[]
        ),
        "readable"
    );
    assert_eq!(
        fs::read_to_string(target.join("private")).unwrap(),
        "denied"
    );
}

#[test]
fn explicit_denials_cover_existing_source_aliases() {
    let fixture = tempfile::tempdir().unwrap();
    fs::write(fixture.path().join("private"), "denied").unwrap();
    let mut plan = system_plan();
    plan.bind(
        fixture.path(),
        Path::new("/first"),
        PathAccess::ReadOnly,
        false,
    )
    .unwrap();
    plan.bind(
        fixture.path(),
        Path::new("/second"),
        PathAccess::ReadOnly,
        false,
    )
    .unwrap();
    plan.bind(
        &fixture.path().join("private"),
        Path::new("/first/private"),
        PathAccess::Deny,
        false,
    )
    .unwrap();
    let plan = plan.complete(&[]).unwrap();
    assert_eq!(
        execute(
            &plan,
            "test ! -s /first/private; test ! -s /second/private",
            &[]
        ),
        ""
    );
}

#[test]
fn equal_bindings_are_emitted_only_once() {
    let fixture = tempfile::tempdir().unwrap();
    let file = fixture.path().join("file");
    fs::write(&file, "once").unwrap();
    let mut plan = system_plan();
    for _ in 0..2 {
        plan.bind(&file, Path::new("/file"), PathAccess::ReadOnly, false)
            .unwrap();
    }
    let plan = plan.complete(&[]).unwrap();
    let mut args = Vec::new();
    plan.append_args(&mut args, |_| false).unwrap();
    assert_eq!(
        args.windows(3)
            .filter(|args| args[0] == "--ro-bind" && args[2] == "/file")
            .count(),
        1
    );
    assert_eq!(execute(&plan, "cat /file", &[]), "once");
}

#[test]
fn parent_components_are_applied_after_resolving_directory_links() {
    let fixture = tempfile::tempdir().unwrap();
    let first = fixture.path().join("first");
    let directory = fixture.path().join("directory");
    let target = fixture.path().join("target");
    fs::create_dir_all(target.join("child")).unwrap();
    fs::write(target.join("file"), "correct target").unwrap();
    fs::write(fixture.path().join("file"), "wrong lexical target").unwrap();
    fs::write(target.join("child/private"), "not granted").unwrap();
    symlink("target/child", &directory).unwrap();
    symlink("directory/../file", &first).unwrap();
    let mut plan = system_plan();
    plan.bind(&first, &first, PathAccess::ReadOnly, false)
        .unwrap();
    let plan = plan.complete(&[]).unwrap();
    assert_eq!(plan.destination(&first).unwrap(), target.join("file"));
    assert_eq!(
        execute(
            &plan,
            "test -L \"$1\"; test -L \"$2\"; test ! -e \"$2/private\"; cat \"$1\"",
            &[&first, &directory]
        ),
        "correct target"
    );
}

#[test]
fn standalone_procfs_aliases_do_not_import_the_host_pid_namespace() {
    let fixture = tempfile::tempdir().unwrap();
    let link = fixture.path().join("mtab");
    symlink("/proc/self/mounts", &link).unwrap();
    let mut plan = system_plan();
    plan.bind(&link, &link, PathAccess::ReadOnly, false)
        .unwrap();
    let plan = plan.complete(&[]).unwrap();
    assert_eq!(
        plan.destination(&link).unwrap(),
        Path::new("/proc/self/mounts")
    );
    assert!(plan.resolve(&link).unwrap().is_none());
    let mut args = Vec::new();
    plan.append_args(&mut args, |_| false).unwrap();
    assert!(
        !args
            .windows(3)
            .any(|args| args[0] == "--ro-bind" && Path::new(&args[1]).starts_with("/proc"))
    );
    assert_eq!(
        execute(
            &plan,
            "test -L \"$1\"; cmp \"$1\" /proc/self/mounts",
            &[&link]
        ),
        ""
    );
}

#[test]
fn ordinary_directory_subtrees_are_imported_without_recursive_scanning() {
    let fixture = tempfile::tempdir().unwrap();
    let mut nested = fixture.path().to_path_buf();
    for _ in 0..130 {
        nested.push("child");
    }
    fs::create_dir_all(&nested).unwrap();
    fs::write(nested.join("file"), "deeply nested").unwrap();
    let mut plan = system_plan();
    plan.bind(
        fixture.path(),
        Path::new("/input"),
        PathAccess::ReadOnly,
        false,
    )
    .unwrap();
    let plan = plan.complete(&["/input".into()]).unwrap();
    assert!(plan.issues().is_empty());
    let visible_file = Path::new("/input")
        .join(nested.strip_prefix(fixture.path()).unwrap())
        .join("file");
    assert_eq!(
        execute(&plan, "test ! -w \"$1\"; cat \"$1\"", &[&visible_file]),
        "deeply nested"
    );
}
