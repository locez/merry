//! Applies one workspace-sandbox policy, then replaces this process.
//!
//! Tests and any host that is not the `merry` binary spawn this helper.
//! `merry` itself handles the same environment variables in its own `main`.

fn main() {
    merry_process::sandbox_helper_main();
}
