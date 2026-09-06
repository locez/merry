//! Built-in tools for Merry runtimes.
//!
//! This crate owns Merry-provided tool implementations and adapts filesystem
//! bounded text reads and opt-in constrained workspace edits into
//! runtime-registered tools without making the runtime own real workspace
//! access policy. The public `merry::tool` declaration macro is re-exported by
//! the facade crate; this crate only contains the implementation dependency.
//!
//! Path safety is scoped to trusted, stable workspace roots. The MVP rejects
//! absolute paths, parent-directory traversal, ordinary dot components except
//! exact `.` where a tool addresses the root, hidden paths unless explicitly
//! enabled, and ordinary symlink components before reading or patching. On
//! Unix, file opens also use `O_NOFOLLOW` to avoid following a
//! symlink swapped into the leaf path between validation and open. This is not
//! an OS sandbox and does not claim complete hardening against malicious
//! concurrent filesystem mutation, including replacement of intermediate
//! directories during an operation.

use merry_core::ToolSpec;
use merry_runtime::{Tool, ToolBuildError};

mod config;
mod errors;
mod patch;
mod path;
mod read;
mod registry;
mod schema;
mod state;
mod trace;

pub use config::{WorkspaceToolConfigError, WorkspaceToolLimits, WorkspaceToolsConfig};
pub use registry::WorkspaceTools;

/// Registered tool name for bounded read-only text ranges.
pub const READ_TEXT_TOOL: &str = "read_text";
/// Registered tool name for opt-in constrained file patches.
pub const APPLY_PATCH_TOOL: &str = "apply_patch";

#[cfg(test)]
mod tests;
