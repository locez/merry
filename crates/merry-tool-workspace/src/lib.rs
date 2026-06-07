//! Workspace tools for Merry runtimes.
//!
//! This crate is intentionally outside `merry-runtime`: it adapts filesystem
//! reads, read-only discovery, and opt-in constrained workspace edits into
//! runtime-registered tools without making the runtime own real workspace
//! access policy.
//!
//! Path safety is scoped to trusted, stable workspace roots. The MVP rejects
//! absolute paths, parent-directory traversal, ordinary dot components except
//! exact `.` where a tool addresses the root, hidden paths unless explicitly
//! enabled, and ordinary symlink components before reading, listing, searching,
//! or patching. On Unix, file opens also use `O_NOFOLLOW` to avoid following a
//! symlink swapped into the leaf path between validation and open. This is not
//! an OS sandbox and does not claim complete hardening against malicious
//! concurrent filesystem mutation, including replacement of intermediate
//! directories during an operation.

mod config;
mod errors;
mod list;
mod patch;
mod path;
mod profile;
mod read;
mod schema;
mod search;
mod state;
mod tools;
mod trace;

pub use config::{WorkspaceToolConfigError, WorkspaceToolLimits, WorkspaceToolsConfig};
pub use profile::{
    WorkspaceCodingLoopProfile, WorkspaceCodingLoopProfileError, WorkspaceRuntimeProfileBuilderExt,
};
pub use tools::ReadOnlyWorkspaceTools;

/// Registered tool name for read-only file reads.
pub const WORKSPACE_READ_FILE_TOOL: &str = "workspace_read_file";
/// Registered tool name for non-recursive read-only directory listing.
pub const WORKSPACE_LIST_DIR_TOOL: &str = "workspace_list_dir";
/// Registered tool name for bounded read-only literal text search.
pub const WORKSPACE_SEARCH_TEXT_TOOL: &str = "workspace_search_text";
/// Registered tool name for opt-in constrained workspace patches.
pub const WORKSPACE_PATCH_TOOL: &str = "workspace_patch";
/// Registered tool name for runtime-owned process execution in the coding-loop profile.
pub const CODING_LOOP_PROCESS_TOOL: &str = "run_process";

#[cfg(test)]
mod tests;
