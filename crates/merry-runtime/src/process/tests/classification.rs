use crate::process::{
    ProcessActionIntent, ProcessEnvPolicy, ProcessIntentClass, ProcessPermissionProfileId,
    classify_process_intent, is_low_risk_process_action_intent, is_safe_cargo_package_token,
    required_process_permission_profile_id, requires_host_process_path_review,
};

#[test]
fn host_process_path_review_detects_external_paths_and_git_metadata_writes() {
    let read_only_workspace_git = ProcessActionIntent::new(
        vec!["git".to_owned(), "status".to_owned(), "--short".to_owned()],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("read-only workspace git intent is valid");
    assert!(!requires_host_process_path_review(&read_only_workspace_git));

    let workspace_git_write = ProcessActionIntent::new(
        vec![
            "git".to_owned(),
            "checkout".to_owned(),
            "--".to_owned(),
            "README.md".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("workspace git write intent is valid");
    assert!(requires_host_process_path_review(&workspace_git_write));

    let read_only_git_branch = ProcessActionIntent::new(
        vec![
            "git".to_owned(),
            "branch".to_owned(),
            "--show-current".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("read-only git branch intent is valid");
    assert!(!requires_host_process_path_review(&read_only_git_branch));

    let git_branch_write = ProcessActionIntent::new(
        vec![
            "git".to_owned(),
            "branch".to_owned(),
            "new-topic".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("git branch write intent is valid");
    assert!(requires_host_process_path_review(&git_branch_write));

    for argv in [
        vec!["cat", "/tmp/output"],
        vec!["cp", "a", "../outside"],
        vec!["git", "--git-dir=/pathA/.git", "status"],
        vec!["bash", "-lc", "cat /pathA/.git/HEAD"],
        vec!["/usr/bin/python", "-c", "print('ok')"],
    ] {
        let intent = ProcessActionIntent::new(
            argv.into_iter().map(str::to_owned).collect(),
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("external path intent is valid");
        assert!(requires_host_process_path_review(&intent));
    }

    for argv in [
        vec!["bash", "-lc", "cat $HOME/output"],
        vec!["bash", "-lc", "printf ok >~/output"],
        vec!["bash", "-lc", "python -c 'open(\"/tmp/output\", \"w\")'"],
        vec!["bash", "-lc", "printf '%s' \"$(pwd)\""],
    ] {
        let intent = ProcessActionIntent::new(
            argv.into_iter().map(str::to_owned).collect(),
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("shell path intent is valid");
        assert!(requires_host_process_path_review(&intent));
    }
}

#[test]
fn safe_cargo_package_token_allows_package_names_without_paths_or_flags() {
    for package in [
        "merry-runtime",
        "other-crate",
        "merry_coding_loop_task_status_text",
        "crate123",
    ] {
        assert!(is_safe_cargo_package_token(package));
    }

    for package in [
        "",
        "-package",
        "bad.package",
        "../other-crate",
        "crate/name",
    ] {
        assert!(!is_safe_cargo_package_token(package));
    }
}

#[test]
fn classifies_known_process_argv_shapes() {
    for argv in [
        vec!["rustc", "--version"],
        vec!["rg", "--version"],
        vec!["rg", "--files"],
        vec!["rg", "ProcessRunner"],
        vec!["cargo", "fmt", "--all", "--check"],
        vec!["sed", "-n", "1,80p", "crates/merry-runtime/src/process.rs"],
        vec!["git", "status", "--short"],
        vec!["git", "status", "--short", "--branch"],
        vec!["git", "status", "--branch", "--short"],
        vec!["git", "log", "--oneline", "-5"],
        vec!["git", "diff", "--", "crates/merry-runtime/src/process.rs"],
        vec!["git", "show", "--stat", "HEAD"],
        vec!["git", "branch", "--show-current"],
        vec!["bash", "-lc", "rg ProcessRunner | wc -l"],
        vec![
            "sh",
            "-c",
            "sed -n '1,5p' crates/merry-runtime/src/process.rs | wc -l",
        ],
        vec!["zsh", "-lc", "rg ProcessRunner && pwd"],
    ] {
        let intent = ProcessActionIntent::new(
            argv.into_iter().map(str::to_owned).collect(),
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("informational argv is a valid process intent");
        assert_eq!(
            classify_process_intent(&intent),
            ProcessIntentClass::Informational
        );
    }

    for argv in [
        vec!["cargo", "test", "-p", "merry-runtime"],
        vec!["cargo", "test", "--package", "merry-runtime"],
        vec!["cargo", "check", "-p", "merry-runtime"],
        vec!["cargo", "check", "--package", "merry-runtime"],
        vec!["cargo", "test", "-p", "other-crate"],
        vec!["cargo", "check", "-p", "merry_coding_loop_task_status_text"],
    ] {
        let intent = ProcessActionIntent::new(
            argv.into_iter().map(str::to_owned).collect(),
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("local workspace argv is a valid process intent");
        assert_eq!(
            classify_process_intent(&intent),
            ProcessIntentClass::LocalWorkspaceEffect
        );
    }

    for argv in [
        vec!["sh", "-c", "rm -rf target"],
        vec!["bash", "-lc", "rm -rf target"],
        vec!["zsh", "-c", "rm -rf target"],
        vec!["cmd", "/C", "echo unsafe"],
        vec!["powershell", "-Command", "Write-Host unsafe"],
        vec!["pwsh", "-Command", "Write-Host unsafe"],
        vec!["rm", "-rf", "target"],
        vec!["../bin/rm", "-rf", "target"],
        vec!["git", "clean", "-fd"],
        vec!["git", "reset", "--hard"],
        vec!["bash", "-lc", "rg ProcessRunner | rm -rf target"],
        vec!["bash", "-lc", "echo $(rm -rf target)"],
        vec!["/bin/bash", "-lc", "rm -rf target"],
    ] {
        let intent = ProcessActionIntent::new(
            argv.into_iter().map(str::to_owned).collect(),
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("forbidden argv is still a syntactically valid process intent");
        assert_eq!(
            classify_process_intent(&intent),
            ProcessIntentClass::Forbidden
        );
    }

    let git_checkout = ProcessActionIntent::new(
        vec![
            "git".to_owned(),
            "checkout".to_owned(),
            "--".to_owned(),
            "README.md".to_owned(),
        ],
        None,
        ProcessEnvPolicy::empty(),
        None,
        1024,
        1024,
    )
    .expect("git checkout is a syntactically valid process intent");
    assert_eq!(
        classify_process_intent(&git_checkout),
        ProcessIntentClass::Unknown
    );

    for argv in [
        vec!["curl", "https://example.invalid"],
        vec!["wget", "https://example.invalid"],
        vec!["ssh", "example.invalid"],
        vec!["scp", "a", "b"],
        vec!["rsync", "a", "b"],
        vec!["nc", "example.invalid", "443"],
        vec!["netcat", "example.invalid", "443"],
    ] {
        let intent = ProcessActionIntent::new(
            argv.into_iter().map(str::to_owned).collect(),
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("network argv is a syntactically valid process intent");
        assert_eq!(
            classify_process_intent(&intent),
            ProcessIntentClass::Unknown
        );
        assert_eq!(
            required_process_permission_profile_id(&intent),
            Some(ProcessPermissionProfileId::LOCAL_WORKSPACE)
        );
    }

    for argv in [
        vec!["fish", "-c", "echo unsafe"],
        vec!["python", "-c", "print('workspace effect')"],
        vec!["python3", "script.py"],
        vec!["perl", "-e", "print 'workspace effect'"],
        vec!["ruby", "-e", "puts 'workspace effect'"],
        vec!["node", "-e", "console.log('workspace effect')"],
        vec!["cargo", "test"],
        vec!["cargo", "test", "-p", "-package"],
        vec!["cargo", "test", "-p", "bad.package"],
        vec!["cargo", "test", "-p", "../other-crate"],
        vec!["/tmp/cargo", "test", "-p", "merry-runtime"],
        vec!["./cargo", "test", "-p", "merry-runtime"],
        vec!["../bin/cargo", "test", "-p", "merry-runtime"],
        vec!["/tmp/rustc", "--version"],
        vec!["./rg", "--version"],
        vec!["/tmp/rg", "--files"],
        vec!["./rg", "ProcessRunner"],
        vec!["rg", "-n", "ProcessRunner"],
        vec!["rg", "--glob", "*.rs"],
        vec!["rg", "-"],
        vec!["rg", "-pattern"],
        vec!["rg", "Process.*"],
        vec!["rg", "Process|Runner"],
        vec!["rg", "call()"],
        vec!["sed", "-e", "1,80p", "crates/merry-runtime/src/process.rs"],
        vec!["sed", "-n", "1,80d", "crates/merry-runtime/src/process.rs"],
        vec!["sed", "-n", "1,80p"],
        vec!["sed", "-n", "1,80p", "../outside.rs"],
        vec!["sed", "-n", "1,80p", "/tmp/outside.rs"],
        vec!["git", "status", "--porcelain=v2"],
        vec!["git", "diff", "--cached"],
        vec!["git", "show", "HEAD:README.md"],
        vec!["unknown-readonly-ish", "--version"],
        vec!["python3.12", "-c", "print('unknown')"],
        vec!["docker", "run", "image"],
        vec!["/tmp/sh", "-c", "echo unsafe"],
        vec!["./bash", "-lc", "echo unsafe"],
        vec!["bash", "-lc", "rg ProcessRunner > out.txt"],
        vec!["bash", "-lc", "echo $(pwd)"],
        vec!["bash", "-lc", "(pwd)"],
        vec!["bash", "-lc", "rg ProcessRunner | tee out.txt"],
        vec!["/bin/bash", "-lc", "rg ProcessRunner | wc -l"],
        vec![
            "bash",
            "-lc",
            "HOME=.merry/local/home cargo check --all-targets -p merry-runtime",
        ],
    ] {
        let intent = ProcessActionIntent::new(
            argv.into_iter().map(str::to_owned).collect(),
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("unknown argv is still a syntactically valid process intent");
        assert_eq!(
            classify_process_intent(&intent),
            ProcessIntentClass::Unknown
        );
    }
}

#[test]
fn sp3a_low_risk_process_admission_allows_narrow_read_only_argv() {
    for argv in [
        vec!["rustc", "--version"],
        vec!["rg", "--version"],
        vec!["rg", "--files"],
        vec!["rg", "ProcessRunner"],
        vec!["sed", "-n", "1,80p", "crates/merry-runtime/src/process.rs"],
        vec!["git", "status", "--short"],
        vec!["git", "log", "--oneline", "-5"],
        vec!["git", "diff", "--", "crates/merry-runtime/src/process.rs"],
        vec!["git", "show", "--stat", "HEAD"],
        vec!["git", "branch", "--show-current"],
    ] {
        let intent = ProcessActionIntent::new(
            argv.into_iter().map(str::to_owned).collect(),
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("informational argv is a valid process intent");
        assert!(is_low_risk_process_action_intent(&intent));
    }

    for argv in [
        vec!["cargo", "test", "-p", "merry-runtime"],
        vec!["cargo", "test", "--package", "merry-runtime"],
        vec!["/tmp/rustc", "--version"],
        vec!["./rg", "--version"],
        vec!["rg", "-n", "ProcessRunner"],
        vec!["rg", "--glob", "*.rs"],
        vec!["rg", "-"],
        vec!["rg", "Process.*"],
        vec!["rg", "Process|Runner"],
        vec!["sed", "-n", "1,80d", "crates/merry-runtime/src/process.rs"],
        vec!["git", "clean", "-fd"],
        vec!["unknown-readonly-ish", "--version"],
        vec!["sh", "-c", "rm -rf target"],
        vec!["bash", "-lc", "rg ProcessRunner | wc -l"],
    ] {
        let intent = ProcessActionIntent::new(
            argv.into_iter().map(str::to_owned).collect(),
            None,
            ProcessEnvPolicy::empty(),
            None,
            1024,
            1024,
        )
        .expect("non-informational argv is still a valid process intent");
        assert!(!is_low_risk_process_action_intent(&intent));
    }
}

#[test]
fn sp3a_low_risk_process_admission_rejects_stdin_or_env() {
    let stdin_intent = ProcessActionIntent::new(
        vec!["rg".to_owned(), "--files".to_owned()],
        None,
        ProcessEnvPolicy::empty(),
        Some("payload must not enter the auto-admitted lane".to_owned()),
        1024,
        1024,
    )
    .expect("stdin process intent is syntactically valid");
    assert_eq!(
        classify_process_intent(&stdin_intent),
        ProcessIntentClass::Informational
    );
    assert!(!is_low_risk_process_action_intent(&stdin_intent));

    let env_intent = ProcessActionIntent::new(
        vec!["rg".to_owned(), "--version".to_owned()],
        None,
        ProcessEnvPolicy::NonEmptyForTest,
        None,
        1024,
        1024,
    )
    .expect("non-empty env process intent is syntactically valid");
    assert_eq!(
        classify_process_intent(&env_intent),
        ProcessIntentClass::Informational
    );
    assert!(!is_low_risk_process_action_intent(&env_intent));
}
