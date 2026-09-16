//! Built-in tools for Merry runtimes.
//!
//! This crate owns Merry-provided tool implementations and adapts filesystem
//! bounded text reads and opt-in constrained workspace edits into
//! runtime-registered tools without making the runtime own real workspace
//! access policy. The public `merry::tool` declaration macro is re-exported by
//! the facade crate; this crate only contains the implementation dependency.
//!
//! A workspace has one root. A relative path is normalized to root-relative
//! components and dot segments are resolved lexically; an absolute path is used
//! as the caller named it, and an absolute path below the workspace root is
//! rewritten to its relative spelling so one file never has two identities. A
//! path outside the workspace root is not limited to a configured root, because
//! the process sandbox, accepted process profile, and trusted path rules already
//! decide which paths exist and which of them are writable. Every spelling is
//! accepted, including dot-prefixed components. A relative path is walked
//! without following symlink components, and on Unix file opens also use
//! `O_NOFOLLOW` to avoid following a symlink swapped into the leaf path between
//! validation and open. This is not an OS sandbox and does not claim complete
//! hardening against malicious concurrent filesystem mutation, including
//! replacement of intermediate directories during an operation.

use merry_core::ToolSpec;
use merry_runtime::{Tool, ToolBuildError};

mod config;
mod errors;
mod file;
mod patch;
mod path;
mod read;
mod registry;
mod schema;
mod state;
mod trace;

pub use config::{WorkspaceToolConfigError, WorkspaceToolLimits, WorkspaceToolsConfig};
pub use patch::envelope::{
    WorkspacePatchOperationKind, WorkspacePatchSuccess, WorkspacePatchSuccessChange,
    WorkspacePatchSuccessLine, WorkspacePatchSuccessLineKind,
};
pub use registry::WorkspaceTools;

/// Registered tool name for bounded read-only text ranges.
pub const READ_TEXT_TOOL: &str = "read_text";
/// Registered tool name for opt-in constrained file patches.
pub const APPLY_PATCH_TOOL: &str = "apply_patch";

#[cfg(test)]
mod tests;
