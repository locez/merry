//! Unsupported-platform stub for the workspace sandbox boundary.
//!
//! Linux (Landlock + seccomp) and macOS (Seatbelt) use `confinement.rs`.
//! Targets without a native kernel workspace sandbox (such as Windows) still
//! expose the helper and probe entrypoints and fail closed if workspace
//! isolation is requested.

use std::env;

const HELPER_ENV: &str = "MERRY_SANDBOX_HELPER";
const POLICY_ENV: &str = "MERRY_SANDBOX_POLICY";
const SETUP_EXIT_CODE: i32 = 126;

/// Reports whether this process was spawned as the sandbox helper.
#[must_use]
pub fn sandbox_helper_requested() -> bool {
    env::var_os(HELPER_ENV).is_some() && env::var_os(POLICY_ENV).is_some()
}

/// Reports that the workspace sandbox helper is unavailable on this target and
/// exits with the sandbox setup failure code.
pub fn sandbox_helper_main() -> ! {
    eprintln!(
        "merry-sandbox: workspace sandbox is unavailable on {}",
        std::env::consts::OS
    );
    std::process::exit(SETUP_EXIT_CODE);
}

/// Reports that kernel workspace confinement is unavailable on this target.
pub fn probe_workspace_sandbox() -> Result<(), String> {
    Err(format!(
        "workspace sandbox is unavailable on {}",
        std::env::consts::OS
    ))
}

pub(crate) fn setup_failure(stderr: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    text.lines()
        .find(|line| line.starts_with("merry-sandbox:"))
        .map(|line| line.trim().to_owned())
}
