//! Typed tool construction and declaration contracts for Rust applications.
//!
//! Use [`crate::tool`] on an async typed handler to generate a factory that
//! derives its input schema through the runtime's typed tool API.

pub use merry_core::ToolSpec;
pub use merry_runtime::{Tool, ToolBuildError};
pub use merry_tools::tool;
