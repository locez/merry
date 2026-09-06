use super::intent;
use crate::process::{
    AcceptedLocalWorkspaceProcessAdmission, LocalWorkspaceProcessSandboxProfile,
    MAX_PROCESS_OUTPUT_LIMIT_BYTES, ProcessActionError, ProcessActionIntent, ProcessEnvPolicy,
    ProcessExecutionEvidence, ProcessExitStatus, ProcessPermissionProfileId,
    required_process_permission_profile_id,
};

#[test]
fn process_action_intent_validates_argv_cwd_and_limits() {
    let valid = intent();
    assert_eq!(valid.argv(), ["cargo", "test"]);
    assert_eq!(valid.cwd(), Some("crates/merry-runtime"));
    assert_eq!(valid.env_policy(), ProcessEnvPolicy::Empty);
    assert_eq!(valid.stdin_text(), Some("stdin text"));
    assert_eq!(valid.stdout_limit_bytes(), 1024);
    assert_eq!(valid.stderr_limit_bytes(), 2048);
    assert!(valid.summary().contains("argv[0]=cargo"));

    let empty_argv = ProcessActionIntent::new(
        Vec::new(),
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect_err("empty argv is rejected");
    assert!(matches!(empty_argv, ProcessActionError::InvalidArgv { .. }));

    let empty_arg = ProcessActionIntent::new(
        vec!["cargo".to_owned(), String::new()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect_err("empty argv item is rejected");
    assert!(matches!(
        empty_arg,
        ProcessActionError::InvalidArgument { index: 1, .. }
    ));

    let multiline_shell = ProcessActionIntent::new(
        vec![
            "bash".to_owned(),
            "-lc".to_owned(),
            "cargo check -p merry-runtime\ncargo test -p merry-runtime".to_owned(),
        ],
        Some(".".to_owned()),
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("shell argv may contain newline scripts");
    assert_eq!(
        multiline_shell.argv()[2],
        "cargo check -p merry-runtime\ncargo test -p merry-runtime"
    );

    for bad_arg in ["bad\u{0}arg", "bad\rarg", "bad\u{7f}arg"] {
        let error = ProcessActionIntent::new(
            vec!["bash".to_owned(), "-lc".to_owned(), bad_arg.to_owned()],
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect_err("unsafe argv controls are rejected");
        assert!(matches!(
            error,
            ProcessActionError::InvalidArgument { index: 2, .. }
        ));
    }

    for cwd in [
        Some("/tmp".to_owned()),
        Some("../outside".to_owned()),
        Some("dir/../outside".to_owned()),
        Some("bad\ncwd".to_owned()),
    ] {
        let error = ProcessActionIntent::new(
            vec!["cargo".to_owned()],
            cwd,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect_err("bad cwd is rejected");
        assert!(matches!(error, ProcessActionError::InvalidCwd { .. }));
    }

    let zero_limit = ProcessActionIntent::new(
        vec!["cargo".to_owned()],
        Some(".".to_owned()),
        ProcessEnvPolicy::empty(),
        None,
        0,
        1024,
    )
    .expect_err("zero output limit is rejected");
    assert!(matches!(
        zero_limit,
        ProcessActionError::InvalidOutputLimit {
            field: "stdout_limit_bytes",
            ..
        }
    ));

    let oversized_limit = ProcessActionIntent::new(
        vec!["cargo".to_owned()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        MAX_PROCESS_OUTPUT_LIMIT_BYTES + 1,
    )
    .expect_err("oversized output limit is rejected");
    assert!(matches!(
        oversized_limit,
        ProcessActionError::InvalidOutputLimit {
            field: "stderr_limit_bytes",
            ..
        }
    ));
}

#[test]
fn process_execution_evidence_records_intent_identity_and_output_metadata() {
    let intent = intent();
    let evidence = ProcessExecutionEvidence::new(
        &intent,
        ProcessPermissionProfileId::READ_ONLY,
        ProcessExitStatus::Exited(0),
        128,
        false,
        256,
        true,
    )
    .expect("valid process execution evidence");

    assert_eq!(evidence.intent_summary(), intent.summary());
    assert_eq!(evidence.argv(), intent.argv());
    assert_eq!(evidence.cwd(), intent.cwd());
    assert_eq!(
        evidence.permission_profile_id(),
        ProcessPermissionProfileId::READ_ONLY
    );
    assert_eq!(evidence.status(), ProcessExitStatus::Exited(0));
    assert_eq!(evidence.exit_code(), Some(0));
    assert_eq!(evidence.stdout_bytes(), 128);
    assert!(!evidence.stdout_truncated());
    assert_eq!(evidence.stderr_bytes(), 256);
    assert!(evidence.stderr_truncated());
    assert!(evidence.matches_intent(&intent));

    let too_many_bytes = ProcessExecutionEvidence::new(
        &intent,
        ProcessPermissionProfileId::READ_ONLY,
        ProcessExitStatus::Exited(1),
        1025,
        true,
        0,
        false,
    )
    .expect_err("captured bytes must stay within intent limits");
    assert!(matches!(
        too_many_bytes,
        ProcessActionError::InvalidExecutionEvidence {
            field: "stdout_bytes",
            ..
        }
    ));
}

#[test]
fn process_permission_profile_id_is_derived_from_admitted_intent_shape() {
    let informational = ProcessActionIntent::new(
        vec!["rg".to_owned(), "--files".to_owned()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("informational intent is valid");
    assert_eq!(
        required_process_permission_profile_id(&informational),
        Some(ProcessPermissionProfileId::READ_ONLY)
    );

    let local_workspace_effect = ProcessActionIntent::new(
        vec![
            "cargo".to_owned(),
            "test".to_owned(),
            "-p".to_owned(),
            "merry-runtime".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("local workspace effect intent is valid");
    assert_eq!(
        required_process_permission_profile_id(&local_workspace_effect),
        Some(ProcessPermissionProfileId::LOCAL_WORKSPACE)
    );

    let unknown = ProcessActionIntent::new(
        vec!["unknown-readonly-ish".to_owned(), "--version".to_owned()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("unknown intent is syntactically valid");
    assert_eq!(
        required_process_permission_profile_id(&unknown),
        Some(ProcessPermissionProfileId::LOCAL_WORKSPACE)
    );

    let with_stdin = ProcessActionIntent::new(
        vec!["rg".to_owned(), "--files".to_owned()],
        None,
        ProcessEnvPolicy::empty(),
        Some("stdin is outside the read-only profile".to_owned()),
        1024,
        1024,
    )
    .expect("stdin intent is syntactically valid");
    assert_eq!(
        required_process_permission_profile_id(&with_stdin),
        Some(ProcessPermissionProfileId::LOCAL_WORKSPACE)
    );

    let shell_read_only = ProcessActionIntent::new(
        vec![
            "bash".to_owned(),
            "-lc".to_owned(),
            "rg ProcessRunner | wc -l".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("read-only shell intent is valid");
    assert_eq!(
        required_process_permission_profile_id(&shell_read_only),
        Some(ProcessPermissionProfileId::SHELL_READ_ONLY)
    );

    let shell_workspace_effect = ProcessActionIntent::new(
        vec![
            "bash".to_owned(),
            "-lc".to_owned(),
            "HOME=.merry/local/home cargo check --all-targets -p merry-runtime".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("shell workspace effect intent is valid");
    assert_eq!(
        required_process_permission_profile_id(&shell_workspace_effect),
        Some(ProcessPermissionProfileId::LOCAL_WORKSPACE)
    );
}

#[test]
fn local_workspace_process_admission_matches_only_its_permission_profile() {
    let local_workspace_effect = ProcessActionIntent::new(
        vec![
            "cargo".to_owned(),
            "test".to_owned(),
            "-p".to_owned(),
            "merry-runtime".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("local workspace effect intent is valid");
    let informational = ProcessActionIntent::new(
        vec!["rg".to_owned(), "--files".to_owned()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("informational intent is valid");

    let admission = AcceptedLocalWorkspaceProcessAdmission::accept_local_workspace();
    assert_eq!(
        admission.permission_profile_id(),
        ProcessPermissionProfileId::LOCAL_WORKSPACE
    );
    assert!(admission.matches_intent(&local_workspace_effect));
    assert!(!admission.matches_intent(&informational));

    let mismatched_admission =
        AcceptedLocalWorkspaceProcessAdmission::for_test_permission_profile_id(
            ProcessPermissionProfileId::READ_ONLY,
        );
    assert!(!mismatched_admission.matches_intent(&local_workspace_effect));

    assert_eq!(
        serde_json::from_str::<ProcessPermissionProfileId>("\"process.local_workspace\"")
            .expect("process profile id should remain readable"),
        ProcessPermissionProfileId::LOCAL_WORKSPACE
    );

    let host_admission = AcceptedLocalWorkspaceProcessAdmission::accept_host();
    assert_eq!(
        host_admission.sandbox_profile(),
        LocalWorkspaceProcessSandboxProfile::Host
    );
    assert_eq!(
        host_admission.permission_profile_id(),
        ProcessPermissionProfileId::LOCAL_WORKSPACE_HOST
    );
    assert!(host_admission.matches_intent(&local_workspace_effect));
    assert!(host_admission.matches_intent(&informational));
}
