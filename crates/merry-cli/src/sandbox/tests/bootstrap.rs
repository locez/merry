use crate::sandbox::{
    Bootstrap, Error, SANDBOX_CHILD_HANDOFF_ARG, SANDBOX_CHILD_HANDOFF_CLI_BWRAP,
    command::{args_without_sandbox_bootstrap_flags, find_bwrap_in_path},
    os, plan_bootstrap_with_file_exists,
    tests::{contains_sequence, plan_args, plan_sandbox, sandbox_host},
};
use std::path::{Path, PathBuf};

#[test]
fn planning_skips_when_disabled() {
    let host = sandbox_host();

    let bootstrap = plan_sandbox(false, &host).expect("disabled sandbox planning should succeed");

    assert_eq!(bootstrap, Bootstrap::Disabled);
}

#[test]
fn planning_skips_when_already_inside() {
    let mut host = sandbox_host();
    host.inside_sandbox = true;

    let bootstrap =
        plan_sandbox(true, &host).expect("already-inside sandbox planning should succeed");

    assert_eq!(bootstrap, Bootstrap::AlreadyInside);
}

#[test]
fn plan_reexecs_current_exe_with_hidden_handoff_and_sandbox_flag_removed() {
    let host = sandbox_host();
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    let exe_index = args
        .iter()
        .position(|arg| arg == "/workspace/merry/target/debug/merry")
        .expect("current executable should be present");
    assert_eq!(
        &args[exe_index + 1..],
        [
            SANDBOX_CHILD_HANDOFF_ARG,
            SANDBOX_CHILD_HANDOFF_CLI_BWRAP,
            "debug",
            "--session-id",
            "custom-session",
        ]
    );
}

#[test]
fn plan_strips_host_provided_hidden_handoff_before_injecting_its_own() {
    let mut host = sandbox_host();
    host.args = vec![
        os("--with-sandbox"),
        os(SANDBOX_CHILD_HANDOFF_ARG),
        os(SANDBOX_CHILD_HANDOFF_CLI_BWRAP),
        os("debug"),
        os("--session-id"),
        os("custom-session"),
    ];
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);
    let handoff_positions = args
        .iter()
        .enumerate()
        .filter_map(|(index, arg)| (arg == SANDBOX_CHILD_HANDOFF_ARG).then_some(index))
        .collect::<Vec<_>>();

    assert_eq!(handoff_positions.len(), 1);
    let handoff_index = handoff_positions[0];
    assert_eq!(args[handoff_index + 1], SANDBOX_CHILD_HANDOFF_CLI_BWRAP);
    assert!(contains_sequence(
        &args,
        &[
            "/workspace/merry/target/debug/merry",
            SANDBOX_CHILD_HANDOFF_ARG,
            SANDBOX_CHILD_HANDOFF_CLI_BWRAP,
            "debug",
            "--session-id",
            "custom-session",
        ],
    ));
}

#[test]
fn plan_strips_host_provided_hidden_handoff_assignment_before_injecting_its_own() {
    let mut host = sandbox_host();
    host.args = vec![
        os("--with-sandbox"),
        os("--merry-sandbox-child-handoff=cli-bwrap"),
        os("debug"),
        os("--session-id"),
        os("custom-session"),
    ];
    let Bootstrap::Reexec(plan) =
        plan_sandbox(true, &host).expect("sandbox planning should succeed")
    else {
        panic!("expected sandbox reexec plan");
    };
    let args = plan_args(&plan);

    assert_eq!(
        args.iter()
            .filter(|arg| arg.as_str() == SANDBOX_CHILD_HANDOFF_ARG)
            .count(),
        1
    );
    assert!(
        !args
            .iter()
            .any(|arg| arg == "--merry-sandbox-child-handoff=cli-bwrap")
    );
}

#[test]
fn find_bwrap_in_path_returns_first_existing_candidate() {
    let path = os("/missing/bin:/custom/bin:/later/bin");

    let found = find_bwrap_in_path(&path, |candidate| {
        candidate == Path::new("/custom/bin/bwrap") || candidate == Path::new("/later/bin/bwrap")
    });

    assert_eq!(found, Some(PathBuf::from("/custom/bin/bwrap")));
}

#[test]
fn planning_errors_when_bwrap_is_missing_from_path() {
    let host = sandbox_host();

    let error = plan_bootstrap_with_file_exists(true, &host, |_| false)
        .expect_err("missing bwrap should fail during planning");

    assert!(matches!(error, Error::MissingBubblewrap));
    assert_eq!(
        error.to_string(),
        "bubblewrap executable `bwrap` was not found in PATH; install bubblewrap to use TUI/run, or omit --with-sandbox for debug commands"
    );
}

#[test]
fn args_without_sandbox_bootstrap_flags_removes_only_first_sandbox_marker() {
    let args = vec![
        os("--with-sandbox"),
        os("debug"),
        os("--input"),
        os("--with-sandbox"),
    ];

    assert_eq!(
        args_without_sandbox_bootstrap_flags(&args),
        vec![os("debug"), os("--input"), os("--with-sandbox")]
    );
}

#[test]
fn args_without_sandbox_bootstrap_flags_preserves_shell_trailing_argv() {
    let args = vec![
        os("--with-sandbox"),
        os("shell"),
        os("--"),
        os("--with-sandbox"),
        os(SANDBOX_CHILD_HANDOFF_ARG),
        os(SANDBOX_CHILD_HANDOFF_CLI_BWRAP),
    ];

    assert_eq!(
        args_without_sandbox_bootstrap_flags(&args),
        vec![
            os("shell"),
            os("--"),
            os("--with-sandbox"),
            os(SANDBOX_CHILD_HANDOFF_ARG),
            os(SANDBOX_CHILD_HANDOFF_CLI_BWRAP),
        ]
    );
}
