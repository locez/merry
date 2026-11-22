use crate::{
    CodingPermissionPolicy, CodingRuntimeBuilder, CodingRuntimeInput, CodingSubagentsConfig,
    WorktreeError, WorktreeIsolationManager, cleanup_orphaned_worktrees,
};
use merry_core::{SessionId, ToolName};
use merry_llm::{
    FinishReason, ModelEvent, ModelName, ModelOutput, ModelResponse, ModelToolCall,
    ModelToolCallId, ToolArguments, testing::FakeModelProvider,
};
use merry_process::{
    LocalProcessBackend, ProcessBackend, ProcessBackendMode, ProcessBackendOptions,
};
use merry_runtime::{
    AgentLoopConfig, ProcessActionIntent, ProcessEnvPolicy, ProcessRunnerContext, StepContext,
    StepInput, SubagentConfig, SubagentWorkspaceMode, SubagentWorktreeBase,
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

fn init_git_repo(dir: &Path) {
    let run = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git command should spawn");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["init", "-b", "main"]);
    run(&["config", "user.name", "Merry Test"]);
    run(&["config", "user.email", "merry-test@example.com"]);
    run(&["config", "commit.gpgsign", "false"]);
    run(&["config", "core.autocrlf", "false"]);
    fs::create_dir_all(dir.join("src")).expect("src dir");
    fs::write(dir.join("src/app.txt"), "v1 committed\n").expect("seed app.txt");
    run(&["add", "-A"]);
    run(&["commit", "-m", "initial commit"]);
}

fn completed_text(text: &str) -> ModelEvent {
    ModelEvent::Completed {
        response: ModelResponse::new(vec![ModelOutput::text(text)], FinishReason::Stop, None),
    }
}

fn tool_call_event(id: &str, name: &str, args: serde_json::Value) -> ModelEvent {
    let call = ModelToolCall::new(
        ModelToolCallId::new(id).expect("call id valid"),
        ToolName::new(name).expect("tool name valid"),
        ToolArguments::try_from(args).expect("args valid"),
    );
    ModelEvent::Completed {
        response: ModelResponse::new(
            vec![ModelOutput::tool_call(call)],
            FinishReason::ToolCalls,
            None,
        ),
    }
}

#[test]
fn non_git_workspace_is_rejected_without_silent_fallback() {
    let non_git = tempfile::tempdir().expect("tempdir");
    let cache = tempfile::tempdir().expect("cache dir");
    let manager = WorktreeIsolationManager::with_base_dir(cache.path());
    let session = SessionId::new("non-git-session").expect("valid session");

    let error = manager
        .prepare_or_reattach(
            non_git.path(),
            &session,
            "agent-1",
            SubagentWorktreeBase::WorkingTree,
            None,
        )
        .err()
        .expect("non-git workspace must fail");
    assert!(
        matches!(error, WorktreeError::NotAGitRepository { .. }),
        "unexpected error: {error:?}"
    );
}

#[test]
fn dirty_working_tree_snapshot_is_visible_and_net_diff_excludes_parent_edits() {
    let repo = tempfile::tempdir().expect("repo dir");
    let cache = tempfile::tempdir().expect("cache dir");
    init_git_repo(repo.path());

    // Parent has uncommitted tracked edit + untracked file.
    fs::write(repo.path().join("src/app.txt"), "v2 parent dirty\n").expect("dirty tracked");
    fs::write(repo.path().join("parent_notes.txt"), "parent untracked\n").expect("untracked");

    let manager = WorktreeIsolationManager::with_base_dir(cache.path());
    let session = SessionId::new("dirty-snapshot-session").expect("valid session");
    let prepared = manager
        .prepare_or_reattach(
            repo.path(),
            &session,
            "agent-1",
            SubagentWorktreeBase::WorkingTree,
            None,
        )
        .expect("worktree prepares cleanly");

    // 1. Child worktree sees both parent's uncommitted tracked edit and untracked file.
    assert_eq!(
        fs::read_to_string(prepared.child_workspace_root.join("src/app.txt"))
            .expect("child app.txt"),
        "v2 parent dirty\n"
    );
    assert_eq!(
        fs::read_to_string(prepared.child_workspace_root.join("parent_notes.txt"))
            .expect("child parent_notes.txt"),
        "parent untracked\n"
    );

    // 2. Child makes its own edits inside the worktree.
    fs::write(
        prepared.child_workspace_root.join("src/app.txt"),
        "v3 child edit\n",
    )
    .expect("child edits app.txt");
    fs::write(
        prepared.child_workspace_root.join("src/child_only.txt"),
        "created by child\n",
    )
    .expect("child creates file");

    // 3. Finalize: net diff contains ONLY child's edits, worktree is retained (fail-toward-keep).
    let finalization = prepared.lifecycle.finalize(true);
    assert_eq!(
        finalization.changed_paths,
        vec!["src/app.txt".to_owned(), "src/child_only.txt".to_owned()],
        "parent_notes.txt must NOT appear in child's net changed_paths"
    );
    let wt = finalization.worktree.expect("worktree view present");
    assert!(wt.retained, "dirty worktree must be retained");
    assert!(
        wt.diff_stat
            .as_deref()
            .is_some_and(|stat| stat.contains("src/app.txt") && stat.contains("src/child_only.txt")),
        "diff_stat should include tracked diff and untracked file: {:?}",
        wt.diff_stat
    );
    assert!(
        Path::new(&wt.path).is_dir(),
        "retained worktree exists on disk"
    );

    // 4. Main repository working tree was never mutated by the child.
    assert_eq!(
        fs::read_to_string(repo.path().join("src/app.txt")).expect("main app.txt"),
        "v2 parent dirty\n"
    );
    assert!(!repo.path().join("src/child_only.txt").exists());

    // 5. Reattaching on wake reuses the exact same worktree directory and baseline_oid.
    let reattached = manager
        .prepare_or_reattach(
            repo.path(),
            &session,
            "agent-1",
            SubagentWorktreeBase::WorkingTree,
            Some(&wt),
        )
        .expect("reattach succeeds");
    assert_eq!(reattached.child_workspace_root, PathBuf::from(&wt.path));
    assert_eq!(
        reattached.lifecycle.metadata().baseline_oid,
        wt.baseline_oid
    );

    // 6. Clean `head` base excludes parent's dirty changes and auto-cleans on zero net changes.
    let head_wt = manager
        .prepare_or_reattach(
            repo.path(),
            &session,
            "agent-2",
            SubagentWorktreeBase::Head,
            None,
        )
        .expect("head worktree prepares");
    assert_eq!(
        fs::read_to_string(head_wt.child_workspace_root.join("src/app.txt")).expect("head app.txt"),
        "v1 committed\n"
    );
    assert!(
        !head_wt
            .child_workspace_root
            .join("parent_notes.txt")
            .exists()
    );
    let head_path = head_wt.child_workspace_root.clone();
    let clean_finalization = head_wt.lifecycle.finalize(true);
    assert!(clean_finalization.changed_paths.is_empty());
    let clean_view = clean_finalization.worktree.expect("worktree view");
    assert!(
        !clean_view.retained,
        "zero-change worktree must be auto-removed"
    );
    assert!(
        !head_path.exists(),
        "removed worktree directory must no longer exist"
    );
}

#[test]
fn startup_gc_removes_dead_pid_worktrees_and_spares_live_pid() {
    let repo = tempfile::tempdir().expect("repo dir");
    let cache = tempfile::tempdir().expect("cache dir");
    init_git_repo(repo.path());

    // Create a live instance worktree owned by the current process PID.
    let live_manager = WorktreeIsolationManager::with_base_dir(cache.path());
    let session = SessionId::new("gc-session").expect("valid session");
    let live_wt = live_manager
        .prepare_or_reattach(
            repo.path(),
            &session,
            "agent-live",
            SubagentWorktreeBase::Head,
            None,
        )
        .expect("live worktree prepares");
    let live_path = live_wt.child_workspace_root.clone();

    // Create a simulated dead-PID instance with a real registered git worktree + `.repo` marker.
    let dead_instance = cache.path().join("pid-99999999-dead");
    let dead_session_dir = dead_instance.join("dead-session");
    fs::create_dir_all(&dead_session_dir).expect("dead session dir");
    // Use a PID that does not exist (u32::MAX / 2 is well above Linux pid_max=4194304).
    fs::write(dead_instance.join(".pid"), "99999999\n").expect("dead pid");
    let dead_wt_path = dead_session_dir.join("agent-dead");
    let canonical_repo = fs::canonicalize(repo.path()).expect("canonical repo");
    let status = Command::new("git")
        .arg("-C")
        .arg(&canonical_repo)
        .args(["worktree", "add", "--detach"])
        .arg(&dead_wt_path)
        .arg("HEAD")
        .status()
        .expect("git worktree add");
    assert!(status.success());
    fs::write(
        dead_session_dir.join("agent-dead.repo"),
        format!("{}\n", canonical_repo.display()),
    )
    .expect("repo marker");

    let report = cleanup_orphaned_worktrees(cache.path());
    assert_eq!(report.removed_instances, 1);
    assert_eq!(report.removed_worktrees, 1);
    assert_eq!(report.pruned_repos, 1);
    assert_eq!(report.skipped_live_instances, 1);
    assert!(
        !dead_instance.exists(),
        "dead instance directory must be removed"
    );
    assert!(
        live_path.is_dir(),
        "live instance worktree must remain intact"
    );

    // Clean up live worktree after assertion.
    let _ = live_wt.lifecycle.finalize(true);
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "current_thread")]
async fn sandboxed_worktree_blocks_writes_to_main_repo_and_git_hooks() {
    if merry_process::probe_workspace_sandbox().is_err() {
        return;
    }
    // Keep both the main repo and the worktree cache outside `/tmp` because
    // `/tmp` is always granted read-write in the baseline Landlock sandbox.
    let fixture_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp");
    fs::create_dir_all(&fixture_root).expect("fixture root dir");
    let fixture_root = fs::canonicalize(&fixture_root).expect("canonical fixture root");
    let repo = tempfile::tempdir_in(&fixture_root).expect("repo dir outside /tmp");
    let cache = tempfile::tempdir_in(&fixture_root).expect("cache dir outside /tmp");
    init_git_repo(repo.path());

    let manager = WorktreeIsolationManager::with_base_dir(cache.path());
    let session = SessionId::new("sandbox-wt-session").expect("valid session");
    let prepared = manager
        .prepare_or_reattach(
            repo.path(),
            &session,
            "agent-sandbox",
            SubagentWorktreeBase::WorkingTree,
            None,
        )
        .expect("worktree prepares");

    let backend = LocalProcessBackend::new(
        repo.path(),
        ProcessBackendMode::Isolated,
        ProcessBackendOptions::new(),
    )
    .expect("isolated backend builds");
    let proc_session = backend
        .new_session_with(
            merry_process::ProcessSessionRequest::new()
                .with_workspace_root(&prepared.child_workspace_root)
                .with_workspace_writable(true)
                .with_extra_path_rules(prepared.sandbox_path_rules(true)),
        )
        .expect("worktree process session builds");
    let runner = proc_session.runner();

    // 1. Inside the worktree: writing a file and running `git status` must succeed.
    let shell_intent = |script: String| {
        ProcessActionIntent::new(
            vec!["/bin/sh".to_owned(), "-c".to_owned(), script],
            None,
            ProcessEnvPolicy::default(),
            None,
            16_384,
            16_384,
        )
        .expect("valid intent")
    };
    let ok_intent = shell_intent("printf 'ok' > inside.txt && git status --short".to_owned());
    let ok_out = runner
        .run(ok_intent, ProcessRunnerContext::new(Default::default()))
        .await
        .expect("worktree write + git status should run");
    assert_eq!(
        ok_out.status().exit_code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(ok_out.stderr_data())
    );
    assert!(prepared.child_workspace_root.join("inside.txt").is_file());

    // 2. Attempting to write to the main repository working tree must fail under Landlock.
    let main_escape_target = fs::canonicalize(repo.path())
        .expect("canonical repo")
        .join("escaped.txt");
    let escape_intent = shell_intent(format!("printf 'pwn' > '{}'", main_escape_target.display()));
    let escape_out = runner
        .run(escape_intent, ProcessRunnerContext::new(Default::default()))
        .await
        .expect("sandboxed escape attempt runs and fails");
    assert_ne!(
        escape_out.status().exit_code(),
        Some(0),
        "sandbox must deny writes to main working tree"
    );
    assert!(!main_escape_target.exists());

    // 3. Attempting to write to `<main_repo>/.git/hooks/pre-commit` must fail under Landlock.
    let hook_target = prepared.git_common_dir.join("hooks").join("evil-hook");
    let hook_intent = shell_intent(format!("printf 'pwn' > '{}'", hook_target.display()));
    let hook_out = runner
        .run(hook_intent, ProcessRunnerContext::new(Default::default()))
        .await
        .expect("sandboxed hook write runs and fails");
    assert_ne!(
        hook_out.status().exit_code(),
        Some(0),
        "sandbox must deny writes to main .git/hooks"
    );
    assert!(!hook_target.exists());

    // Clean up worktree explicitly (it is dirty from inside.txt).
    let _ = fs::remove_file(prepared.child_workspace_root.join("inside.txt"));
    let finalization = prepared.lifecycle.finalize(true);
    assert!(!finalization.worktree.expect("wt").retained);
}

#[tokio::test(flavor = "current_thread")]
async fn parallel_worktree_subagents_collect_run_process_changes_and_enforce_cleanup_gate() {
    let repo = tempfile::tempdir().expect("repo dir");
    init_git_repo(repo.path());

    // Parent spawns two parallel-spec worktree children with overlapping write_scope (".").
    // `max_threads = 1` keeps FakeModelProvider turn consumption deterministic:
    // Child 1 edits `src/app.txt` via `run_process`; Child 2 makes zero changes.
    let combined_provider = Arc::new(FakeModelProvider::new_turns(vec![
        // Turn 1 (parent): spawn two worktree children with overlapping write_scope
        vec![Ok(tool_call_event(
            "spawn-wt-batch",
            "spawn_subagents",
            json!({
                "wait": "all",
                "tasks": [
                    {
                        "task": "Modify src/app.txt via run_process.",
                        "display_name": "wt-writer",
                        "role": "coder",
                        "workspace": "worktree",
                        "write_scope": ["."],
                        "allowed_tools": ["run_process"]
                    },
                    {
                        "task": "Inspect without editing.",
                        "display_name": "wt-clean",
                        "role": "coder",
                        "workspace": "worktree",
                        "write_scope": ["."],
                        "allowed_tools": ["read_text"]
                    }
                ]
            }),
        ))],
        // Child 1 turn 1: run_process edits src/app.txt
        vec![Ok(tool_call_event(
            "child-1-proc",
            "run_process",
            json!({ "command": "printf 'from-worktree-child\\n' > src/app.txt" }),
        ))],
        // Child 1 turn 2: finish
        vec![Ok(completed_text("Updated src/app.txt inside worktree."))],
        // Child 2 turn 1: finish with no edits
        vec![Ok(completed_text("No changes needed."))],
        // Parent turn 2: final response
        vec![Ok(completed_text("All worktree subagents settled."))],
    ]));

    let backend = Arc::new(
        LocalProcessBackend::new(
            repo.path(),
            ProcessBackendMode::Host,
            ProcessBackendOptions::new(),
        )
        .expect("host backend builds"),
    );
    let subagent_limits = SubagentConfig::new(1, 1)
        .expect("valid limits")
        .with_model_turn_bounds(1, 10)
        .expect("valid bounds");
    let input = CodingRuntimeInput::new(
        SessionId::new("parent-wt-e2e").expect("valid session"),
        repo.path(),
        combined_provider.clone(),
        ModelName::new("test-model").expect("valid model"),
        backend,
    )
    .with_subagents(CodingSubagentsConfig::enabled(subagent_limits));

    let coding = CodingRuntimeBuilder::new(input)
        .permission_policy(CodingPermissionPolicy::fully_trusted())
        .build()
        .expect("coding runtime builds");
    let runtime = coding.into_runtime();

    let result = runtime
        .run_agent_loop(
            StepInput::user_text("Run two worktree subagents.").expect("valid input"),
            StepContext::default(),
            AgentLoopConfig::new(6).expect("valid loop config"),
        )
        .await
        .expect("agent loop succeeds");
    assert_eq!(result.status(), &merry_runtime::AgentLoopStatus::Completed);

    let statuses = runtime
        .subagent_snapshot()
        .await
        .expect("subagent snapshot present");
    assert_eq!(statuses.len(), 2);

    let writer = statuses
        .iter()
        .find(|s| s.display_name.as_deref() == Some("wt-writer"))
        .expect("wt-writer status");
    assert_eq!(writer.changed_paths, vec!["src/app.txt".to_owned()]);
    let writer_wt = writer.worktree.as_ref().expect("writer worktree view");
    assert!(writer_wt.retained, "dirty worktree must be retained");
    assert!(Path::new(&writer_wt.path).is_dir());
    assert!(
        writer_wt
            .diff_stat
            .as_deref()
            .is_some_and(|s| s.contains("src/app.txt")),
        "diff_stat should mention src/app.txt: {:?}",
        writer_wt.diff_stat
    );
    assert_eq!(
        fs::read_to_string(Path::new(&writer_wt.path).join("src/app.txt")).expect("wt file"),
        "from-worktree-child\n"
    );
    // Main repo untouched!
    assert_eq!(
        fs::read_to_string(repo.path().join("src/app.txt")).expect("main file"),
        "v1 committed\n"
    );

    let clean = statuses
        .iter()
        .find(|s| s.display_name.as_deref() == Some("wt-clean"))
        .expect("wt-clean status");
    assert!(clean.changed_paths.is_empty());
    let clean_wt = clean.worktree.as_ref().expect("clean worktree view");
    assert!(!clean_wt.retained, "zero-change worktree must be removed");
    assert!(!Path::new(&clean_wt.path).exists());

    // Verify task mode enum round-trip.
    assert_eq!(SubagentWorkspaceMode::Worktree.as_str(), "worktree");

    // Clean up retained dirty test worktree from default cache so tests leave no clutter.
    let _ = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["worktree", "remove", "--force", &writer_wt.path])
        .status();
}
