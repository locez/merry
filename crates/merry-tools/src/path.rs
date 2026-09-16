//! Tool path validation, resolution, and file opening.
//!
//! A workspace has one root. A relative path is normalized to root-relative
//! components and dot segments are resolved lexically; an absolute path is used
//! as the caller named it, and an absolute path below the workspace root is
//! rewritten to its relative spelling so one file never has two identities. A
//! path outside the workspace root is not limited to a configured root, because
//! the process sandbox, accepted process profile, and trusted path rules already
//! decide which paths exist and which of them are writable. Every spelling is
//! accepted, including dot-prefixed components.
//!
//! `validate` owns what a caller named and never touches the filesystem.
//! `open` owns resolution against the roots and every file open, which on Unix
//! passes `O_NOFOLLOW` so a symlink swapped into the leaf path between
//! validation and open cannot redirect the operation. This is not an OS sandbox
//! and does not claim complete hardening against malicious concurrent
//! filesystem mutation, including replacement of intermediate directories
//! during an operation.

mod open;
mod validate;

pub(crate) use open::{
    NewWorkspacePath, open_file_for_patch, open_file_for_patch_create_new, open_file_for_read,
    resolve_existing_path, resolve_new_file_path,
};
pub(crate) use validate::{ValidatedToolPath, validate_workspace_path_argument};
