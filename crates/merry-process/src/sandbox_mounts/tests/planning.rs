//! Deterministic filesystem fixtures; no bubblewrap executable or user namespace required.

use crate::{SandboxMountError, SandboxMountPlan};
use merry_runtime::PathAccess;
use std::{fs, os::unix::fs::symlink, path::Path};

#[test]
fn registration_rejects_relative_mount_coordinates() {
    for (source, destination) in [("relative", "/input"), ("/source", "relative")] {
        let mut inputs = SandboxMountPlan::new();
        assert!(matches!(
            inputs.bind(
                Path::new(source),
                Path::new(destination),
                PathAccess::ReadOnly,
                false
            ),
            Err(SandboxMountError::RelativePath { .. })
        ));
        assert!(inputs.resolved_destinations().unwrap().is_empty());
    }
}

#[test]
fn preflight_preserves_input_order_and_matches_prepared_coordinates() {
    let fixture = tempfile::tempdir().unwrap();
    let config = fixture.path().join("config");
    let target = fixture.path().join("target");
    fs::create_dir(&config).unwrap();
    fs::write(&target, "settings").unwrap();
    symlink("/run/settings", config.join("current")).unwrap();
    let mut inputs = SandboxMountPlan::new();
    inputs
        .bind(
            &target,
            Path::new("/etc/current"),
            PathAccess::ReadOnly,
            false,
        )
        .unwrap();
    inputs
        .bind(&config, Path::new("/etc"), PathAccess::ReadOnly, false)
        .unwrap();
    let destinations = inputs.resolved_destinations().unwrap();
    assert_eq!(
        destinations,
        [Path::new("/run/settings"), Path::new("/etc")]
    );
    let prepared = inputs.complete(&[]).unwrap();
    for (logical, expected) in ["/etc/current", "/etc"].into_iter().zip(destinations) {
        assert_eq!(prepared.destination(Path::new(logical)).unwrap(), expected);
    }
    assert_eq!(
        prepared
            .resolve(Path::new("/etc/current"))
            .unwrap()
            .unwrap()
            .source(),
        target
    );
}

#[test]
fn preflight_does_not_resolve_unimported_host_directory_links() {
    let fixture = tempfile::tempdir().unwrap();
    let directory = fixture.path().join("directory");
    let alias = fixture.path().join("alias");
    let target = fixture.path().join("target");
    fs::create_dir(&directory).unwrap();
    fs::write(&target, "settings").unwrap();
    symlink(&directory, &alias).unwrap();
    let logical = alias.join("file");
    let mut inputs = SandboxMountPlan::new();
    inputs
        .bind(&target, &logical, PathAccess::ReadOnly, false)
        .unwrap();
    assert_eq!(
        inputs.resolved_destinations().unwrap(),
        std::slice::from_ref(&logical)
    );
    let prepared = inputs.complete(&[]).unwrap();
    assert_eq!(prepared.destination(&logical).unwrap(), logical);
    assert_eq!(
        prepared.resolve(&logical).unwrap().unwrap().source(),
        target
    );
}

#[test]
fn opaque_runtime_targets_remain_symbolic_during_planning() {
    let fixture = tempfile::tempdir().unwrap();
    symlink("/proc/self/mounts", fixture.path().join("link")).unwrap();
    let mut inputs = SandboxMountPlan::new();
    inputs
        .bind(
            fixture.path(),
            Path::new("/input"),
            PathAccess::ReadOnly,
            false,
        )
        .unwrap();
    inputs.opaque(Path::new("/proc")).unwrap();
    assert_eq!(
        inputs.resolved_destinations().unwrap(),
        [Path::new("/input")]
    );
    let prepared = inputs.complete(&["/input".into()]).unwrap();
    assert!(prepared.issues().is_empty());
    assert_eq!(
        prepared.destination(Path::new("/input/link")).unwrap(),
        Path::new("/proc/self/mounts")
    );
    assert!(
        prepared
            .resolve(Path::new("/input/link"))
            .unwrap()
            .is_none()
    );
}

#[test]
fn changed_source_links_fail_before_emitting_mounts() {
    let fixture = tempfile::tempdir().unwrap();
    let target = fixture.path().join("target");
    let link = fixture.path().join("link");
    fs::write(&target, "admitted").unwrap();
    symlink("target", &link).unwrap();
    let mut plan = SandboxMountPlan::new();
    plan.bind(&link, &link, PathAccess::ReadOnly, false)
        .unwrap();
    let plan = plan.complete(&[]).unwrap();
    fs::remove_file(&link).unwrap();
    symlink("changed", &link).unwrap();
    let mut args = Vec::new();
    assert!(matches!(
        plan.append_args(&mut args, |_| false),
        Err(SandboxMountError::ChangedLink { .. })
    ));
    assert!(args.is_empty());
}

#[test]
fn explicit_directory_scan_roots_still_obey_path_depth_limits() {
    let fixture = tempfile::tempdir().unwrap();
    let mut destination = Path::new("/input").to_path_buf();
    for _ in 0..128 {
        destination.push("child");
    }
    let mut plan = SandboxMountPlan::new();
    plan.bind(fixture.path(), &destination, PathAccess::ReadOnly, false)
        .unwrap();
    assert!(matches!(
        plan.complete(&[destination]),
        Err(SandboxMountError::Limit {
            kind: "path depth",
            ..
        })
    ));
}
