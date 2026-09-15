//! Shared scaffolding for the sandbox tests that re-enter this test binary.
//!
//! These tests have two halves. The parent half prepares a sandbox plan or
//! command, replaces the re-executed command with [`sandboxed_reentry_arguments`]
//! or [`reentry_arguments`], runs it through [`run_plan`], and then checks the
//! result with [`assert_child_passed`]. The child half recognizes itself with
//! [`is_child`] and does the work that only makes sense inside the sandbox.
//!
//! The parent must not settle for the child's exit status. The test harness
//! exits successfully when `--exact` matches no test, so a renamed test or a
//! stale filter would pass as an empty run; [`assert_child_passed`] requires the
//! harness to report the named test as one passing test.

use crate::sandbox::{Plan, os};
use std::{
    ffi::{OsStr, OsString},
    path::Path,
    process::{Command, Output},
};

/// Returns true when the current process is the sandboxed child test.
pub(super) fn is_child(marker: &str) -> bool {
    std::env::var_os(marker).as_deref() == Some(OsStr::new("1"))
}

/// Runner arguments that select exactly one test in the test binary at
/// `executable`, with no sandbox prefix. Use
/// [`sandboxed_reentry_arguments`] when bubblewrap has to install the marker.
pub(super) fn reentry_arguments(executable: &Path, test_path: &str) -> Vec<OsString> {
    vec![
        executable.into(),
        os("--exact"),
        os(test_path),
        os("--nocapture"),
    ]
}

/// Removes the command a prepared plan would have run, keeping its sandbox
/// arguments, so the caller can supply the real command.
pub(super) fn truncate_before_command(plan: &mut Plan, executable: &Path) {
    let command_index = plan
        .args
        .iter()
        .rposition(|argument| argument == executable.as_os_str())
        .expect("outer sandbox plan re-executes the test binary");
    plan.args.truncate(command_index);
}

/// Bubblewrap arguments that re-enter the test binary at `executable`, running
/// exactly `test_path` with `marker` set.
///
/// The marker reaches the child through bubblewrap's `--setenv`, so the child
/// half can recognize itself without inheriting parent state.
pub(super) fn sandboxed_reentry_arguments(
    marker: &str,
    executable: &Path,
    test_path: &str,
) -> Vec<OsString> {
    let mut command = vec![os("--setenv"), os(marker), os("1")];
    command.extend(reentry_arguments(executable, test_path));
    command
}

/// Runs a prepared plan with the environment and descriptors the sandbox
/// command expects.
pub(super) fn run_plan(plan: &Plan) -> Output {
    let mut command = Command::new(&plan.program);
    command
        .args(&plan.args)
        .env_clear()
        .envs(plan.env.iter().cloned());
    plan.ssh_config
        .configure_command(&mut command)
        .expect("SSH configuration snapshot");
    command.output().expect("bubblewrap test dependency")
}

/// Asserts the sandboxed child ran exactly `test_path` and reported success.
///
/// Fails closed on the empty selection produced by a stale test path, and
/// surfaces the child's own harness output when the test itself failed.
pub(super) fn assert_child_passed(output: &Output, test_path: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains(&format!("test {test_path} ... ok")),
        "the sandboxed child did not run {test_path}; check that the test name still exists: {stdout}\n{stderr}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("test result: ok. 1 passed; 0 failed;")),
        "the sandboxed child did not report exactly one passing test: {stdout}\n{stderr}"
    );
}

#[cfg(test)]
mod assert_child_passed_tests {
    use super::assert_child_passed;
    use std::{
        os::unix::process::ExitStatusExt,
        process::{ExitStatus, Output},
    };

    const TEST_PATH: &str = "sandbox::tests::group::expected_child";

    fn harness_output(status: i32, stdout: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(status),
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    /// `cargo test --exact <stale path>` exits successfully after running zero
    /// tests, so a renamed child must not look like a passing one.
    #[test]
    #[should_panic(expected = "did not run")]
    fn empty_selection_is_not_a_pass() {
        assert_child_passed(
            &harness_output(
                0,
                "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 630 filtered out; finished in 0.00s\n",
            ),
            TEST_PATH,
        );
    }

    /// A substring check would accept any count ending in `1`, so the count is
    /// matched from the start of the harness line.
    #[test]
    #[should_panic(expected = "did not report exactly one passing test")]
    fn more_than_one_passing_test_is_not_a_single_child() {
        assert_child_passed(
            &harness_output(
                0,
                "test sandbox::tests::group::expected_child ... ok\n\ntest result: ok. 31 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n",
            ),
            TEST_PATH,
        );
    }
}
