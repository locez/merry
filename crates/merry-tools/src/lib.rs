//! Built-in tools for Merry runtimes.
//!
//! This crate owns Merry-provided tool implementations and adapts filesystem
//! bounded text/image reads and opt-in constrained workspace edits into
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

pub mod browser;
mod config;
mod errors;
mod file;
mod patch;
mod path;
mod read;
mod registry;
mod schema;
pub mod ssrf;
mod state;
mod trace;
pub mod web_fetch;

pub use browser::{
    BROWSER_NETWORK_TOOL, BROWSER_TAB_TOOL, BrowserConfig, BrowserNetworkTool,
    BrowserSessionManager, BrowserTabTool,
};
pub use config::{WorkspaceToolConfigError, WorkspaceToolLimits, WorkspaceToolsConfig};
pub use patch::envelope::{
    WorkspacePatchOperationKind, WorkspacePatchSuccess, WorkspacePatchSuccessChange,
    WorkspacePatchSuccessLine, WorkspacePatchSuccessLineKind,
};
pub use read::{WorkspaceReadImageSuccess, WorkspaceReadPdfPagesSuccess};
pub use registry::WorkspaceTools;
pub use web_fetch::{WEB_FETCH_TOOL, WebFetchConfig, WebFetchTool};

/// Registered tool name for bounded read-only file reads (UTF-8 text, images, PDFs, Office docs, and notebooks).
pub const READ_FILE_TOOL: &str = "read_file";
/// Registered tool name for opt-in constrained file patches.
pub const APPLY_PATCH_TOOL: &str = "apply_patch";

#[cfg(test)]
mod tests;
