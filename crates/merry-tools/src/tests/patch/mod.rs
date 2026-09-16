//! Behavior tests for the `apply_patch` tool.
//!
//! The suite is split by responsibility so a failure points at one area:
//! file operations, envelope grammar, diagnostics, and workspace scope.

use super::*;

mod add;
mod delete;
mod diagnostics;
mod grammar;
mod scope;
mod sections;
mod update;
